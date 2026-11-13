//! 发票归集：从邮件里的 PDF 附件提取文字，用规则解析金额 / 开票方 / 日期 / 发票号。
//! 全部在本地完成。扫描件（没有文字层的 PDF）识别不出来，会被标记为已扫描、非发票。

use regex::Regex;
use std::sync::LazyLock;

/// 超过这个大小的 PDF 不解析（发票通常几十 KB）
pub const MAX_PDF: usize = 8 << 20;

/// 解析出的发票信息。`is_invoice = false` 表示这个 PDF 看起来不是发票（只用来记「已扫描过」）
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InvoiceInfo {
    pub is_invoice: bool,
    /// 价税合计，单位：分
    pub amount_cents: Option<i64>,
    pub seller: String,
    pub buyer: String,
    /// yyyy-mm-dd
    pub date: String,
    pub number: String,
    /// 增值税电子发票 / 铁路电子客票 / 其他
    pub kind: String,
}

/// 提取 PDF 文字。pdf-extract 遇到怪 PDF 可能 panic，这里兜住，返回 None
pub fn pdf_text(bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_PDF || !bytes.starts_with(b"%PDF") {
        return None;
    }
    let b = bytes.to_vec();
    std::panic::catch_unwind(move || pdf_extract::extract_text_from_mem(&b).ok()).ok().flatten()
}

/// 全角 / 兼容字符归一，去掉多余空白，方便用统一的正则匹配
fn normalize(text: &str) -> String {
    // 电子发票 PDF 的字体映射常把汉字输出成「康熙部首」（⼦ ⽇ ⽅ ⼀ …），NFKC 把它们变回普通汉字，同时处理全角字符
    use unicode_normalization::UnicodeNormalization;
    text.nfkc()
        .collect::<String>()
        .chars()
        .map(|c| match c {
            '：' => ':',
            '（' => '(',
            '）' => ')',
            '￥' | '¥' => '¥',
            '\u{3000}' | '\u{a0}' => ' ',
            _ => c,
        })
        .collect()
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("regex")
}

/// 「1,234.50」→ 123450
fn cents(s: &str) -> Option<i64> {
    let t: String = s.chars().filter(|c| *c != ',' && *c != ' ').collect();
    let (int, frac) = match t.split_once('.') {
        Some((i, f)) => (i, f),
        None => (t.as_str(), ""),
    };
    let int: i64 = int.parse().ok()?;
    let mut f = frac.chars().take(2).collect::<String>();
    while f.len() < 2 {
        f.push('0');
    }
    Some(int * 100 + f.parse::<i64>().ok()?)
}

/// 提取文字后识别发票。不是发票返回 `is_invoice = false`
pub fn parse_text(raw: &str) -> InvoiceInfo {
    static MARK: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(电子发票|增值税|发票号码|发票代码|电子客票|invoice|tax invoice)"));
    static AMOUNT_TOTAL: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)价税合计.{0,60}?[(（]小写[)）]\s*[¥]?\s*([0-9][0-9,]*(?:\.[0-9]{1,2})?)"));
    static AMOUNT_SMALL: LazyLock<Regex> = LazyLock::new(|| re(r"[(（]小写[)）]\s*[¥]?\s*([0-9][0-9,]*(?:\.[0-9]{1,2})?)"));
    static AMOUNT_RAIL: LazyLock<Regex> = LazyLock::new(|| re(r"票价:?\s*[¥]?\s*([0-9][0-9,]*(?:\.[0-9]{1,2})?)"));
    static AMOUNT_EN: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(?:total|amount due|grand total)[^0-9¥$]{0,20}[¥$]?\s*([0-9][0-9,]*\.[0-9]{2})"));
    static AMOUNT_YEN: LazyLock<Regex> = LazyLock::new(|| re(r"¥\s*([0-9][0-9,]*\.[0-9]{2})"));
    static NUMBER: LazyLock<Regex> = LazyLock::new(|| re(r"发票号码:?\s*([0-9]{8,20})"));
    static NUMBER_ANY: LazyLock<Regex> = LazyLock::new(|| re(r"\b([0-9]{20})\b"));
    static DATE: LazyLock<Regex> = LazyLock::new(|| re(r"(?:开票日期|日期)[: ]*([0-9]{4})\s*[年/.-]\s*([0-9]{1,2})\s*[月/.-]\s*([0-9]{1,2})"));
    static DATE_ANY: LazyLock<Regex> = LazyLock::new(|| re(r"([0-9]{4})\s*年\s*([0-9]{1,2})\s*月\s*([0-9]{1,2})\s*日"));
    static SELLER: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)销售方.{0,30}?名称:?\s*([^\s:]{4,60})"));
    static SELLER2: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)名称:?\s*[^\s:]{4,60}\s+[^\n]{0,40}?名称:?\s*([^\s:]{4,60})"));
    static BUYER: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)购买方.{0,30}?名称:?\s*([^\s:]{4,60})"));
    static BUYER2: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)名称:?\s*([^\s:]{4,60})\s+[^\n]{0,40}?名称:?\s*[^\s:]{4,60}"));

    // 「购 买 方」「价 税 合 计」这类被空格拆开的标签字合回去；只处理已知标签，免得把相邻两栏的内容粘在一起
    static LABELS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
        ["购买方", "销售方", "名称", "价税合计", "开票日期", "发票号码", "小写", "电子发票", "电子客票"]
            .iter()
            .map(|l| (re(&l.chars().map(|c| c.to_string()).collect::<Vec<_>>().join(" *")), *l))
            .collect()
    });
    let mut text = normalize(raw);
    for (r, l) in LABELS.iter() {
        text = r.replace_all(&text, *l).into_owned();
    }
    let mut info = InvoiceInfo::default();
    if !MARK.is_match(&text) {
        return info;
    }
    info.is_invoice = true;
    info.kind = if text.contains("电子客票") || text.contains("铁路") {
        "铁路电子客票"
    } else if text.contains("增值税") || text.contains("发票号码") || text.contains("电子发票") {
        "电子发票"
    } else {
        "发票 / 收据"
    }
    .into();
    let grab = |r: &Regex| r.captures(&text).map(|c| c[1].to_string());
    info.amount_cents = grab(&AMOUNT_TOTAL)
        .or_else(|| grab(&AMOUNT_SMALL))
        .or_else(|| grab(&AMOUNT_RAIL))
        .or_else(|| grab(&AMOUNT_EN))
        .or_else(|| {
            // 兜底：整页里最大的「¥ 金额」
            AMOUNT_YEN.captures_iter(&text).filter_map(|c| cents(&c[1])).max().map(|c| format!("{}.{:02}", c / 100, c % 100))
        })
        .and_then(|s| cents(&s));
    info.number = grab(&NUMBER).or_else(|| grab(&NUMBER_ANY)).unwrap_or_default();
    let date = DATE.captures(&text).or_else(|| DATE_ANY.captures(&text));
    if let Some(c) = date {
        let (y, m, d): (i32, u32, u32) = (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0), c[3].parse().unwrap_or(0));
        if (1990..=2100).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d) {
            info.date = format!("{y:04}-{m:02}-{d:02}");
        }
    }
    info.seller = grab(&SELLER).or_else(|| grab(&SELLER2)).unwrap_or_default();
    info.buyer = grab(&BUYER).or_else(|| grab(&BUYER2)).unwrap_or_default();
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vat_invoice_layout() {
        let t = "电子发票（普通发票）\n发票号码：25322000000123456789\n开票日期：2025年03月08日\n购买方信息 名称：张三科技有限公司 销售方信息 名称：南京某某餐饮管理有限公司\n价税合计（大写） 壹佰贰拾叁圆伍角 （小写）¥123.50\n";
        let i = parse_text(t);
        assert!(i.is_invoice);
        assert_eq!(i.amount_cents, Some(12350));
        assert_eq!(i.number, "25322000000123456789");
        assert_eq!(i.date, "2025-03-08");
        assert_eq!(i.kind, "电子发票");
        assert_eq!(i.buyer, "张三科技有限公司");
        assert_eq!(i.seller, "南京某某餐饮管理有限公司");
    }

    #[test]
    fn parses_separate_labels_and_thousands() {
        let t = "发票号码: 12345678\n开票日期: 2024-12-01\n销售方 名称: 北京示例软件有限公司\n购买方 名称: 个人\n价税合计 (小写) ¥ 1,234.5";
        let i = parse_text(t);
        assert_eq!(i.amount_cents, Some(123450));
        assert_eq!(i.seller, "北京示例软件有限公司");
        assert_eq!(i.date, "2024-12-01");
    }

    #[test]
    fn rail_and_english_and_non_invoice() {
        let r = parse_text("电子发票(铁路电子客票)\n票价:￥553.00\n开票日期:2025年01月02日");
        assert_eq!((r.kind.as_str(), r.amount_cents), ("铁路电子客票", Some(55300)));
        let e = parse_text("Invoice #42\nTotal: $1,200.00\n");
        assert_eq!(e.amount_cents, Some(120000));
        assert!(!parse_text("本月对账单 请查收").is_invoice);
        assert_eq!(cents("7"), Some(700));
        assert_eq!(cents("0.5"), Some(50));
    }

    /// 真实结构的 PDF（Chrome 生成，内嵌中文字体 + ToUnicode，和电子发票一致）：提取文字并解析
    #[test]
    fn extracts_and_parses_a_real_pdf() {
        let bytes = std::fs::read("tests/fixtures/invoice_sample.pdf").unwrap();
        let text = pdf_text(&bytes).expect("提取文字");
        let i = parse_text(&text);
        assert!(i.is_invoice, "{text}");
        assert_eq!(i.amount_cents, Some(12350), "{text}");
        assert_eq!(i.number, "25322000000123456789");
        assert_eq!(i.date, "2025-03-08");
        assert_eq!(i.seller, "南京示例餐饮管理有限公司", "{text}");
        assert_eq!(i.buyer, "测试科技有限公司", "{text}");
        assert!(pdf_text(b"not a pdf").is_none());
    }

    /// 本机样本探测（需要 target/tmp/pdfs 里放 PDF；没有就跳过）：只统计，不打印内容
    #[test]
    fn local_samples_probe() {
        let Ok(dir) = std::fs::read_dir("target/tmp/pdfs") else { return };
        let (mut n, mut text_ok, mut inv, mut amt) = (0, 0, 0, 0);
        for e in dir.flatten() {
            n += 1;
            let Ok(b) = std::fs::read(e.path()) else { continue };
            if let Some(t) = pdf_text(&b) {
                if !t.trim().is_empty() {
                    text_ok += 1;
                }
                let i = parse_text(&t);
                inv += i.is_invoice as i32;
                amt += i.amount_cents.is_some() as i32;
            }
        }
        eprintln!("PROBE pdfs={n} with_text={text_ok} invoice={inv} with_amount={amt}");
    }
}

// ───────────────────────────── 入库 / 查询 / 导出 ─────────────────────────────

use super::db::Store;
use super::{messages, parse, transfer};
use anyhow::{Context, Result};
use rusqlite::params;
use std::path::{Path, PathBuf};

/// 解析一封邮件里的全部 PDF 附件并入库（已有的行会被覆盖）。返回识别出几张发票
pub fn scan_message(store: &Store, msg_id: i64, raw: &[u8]) -> Result<usize> {
    let mut found = 0;
    for (idx, name, bytes) in parse::pdf_attachments(raw) {
        let info = pdf_text(&bytes).map(|t| parse_text(&t)).unwrap_or_default();
        found += info.is_invoice as usize;
        store.w().execute(
            "INSERT OR REPLACE INTO invoices(message_id, att_index, filename, is_invoice, amount_cents, seller, buyer, inv_date, number, kind)
             VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![msg_id, idx as i64, name, info.is_invoice as i64, info.amount_cents, info.seller, info.buyer, info.date, info.number, info.kind],
        )?;
    }
    Ok(found)
}

/// 补扫：已缓存正文、带 PDF 附件、还没扫过的邮件，每次最多 `limit` 封。返回本批处理了几封
pub fn scan_backlog(store: &Store, limit: usize) -> Result<usize> {
    let ids: Vec<i64> = {
        let c = store.r();
        let mut st = c.prepare(
            "SELECT m.id FROM messages m WHERE m.body_state = 1 AND m.has_att = 1 AND (m.att_json LIKE '%.pdf%' OR m.att_json LIKE '%application/pdf%')
               AND NOT EXISTS (SELECT 1 FROM invoices i WHERE i.message_id = m.id) ORDER BY m.id DESC LIMIT ?",
        )?;
        let rows = st.query_map([limit as i64], |r| r.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let n = ids.len();
    for id in ids {
        match messages::read_local_source(store, id) {
            Some(raw) => {
                let _ = scan_message(store, id, &raw);
            }
            // 原文文件丢了：记一笔，免得每次启动都重试
            None => {
                let _ = store.w().execute("INSERT OR IGNORE INTO invoices(message_id, att_index, is_invoice) VALUES(?, -1, 0)", [id]);
            }
        }
    }
    Ok(n)
}

/// 待下载正文的疑似发票邮件（主题像发票、正文还没缓存），最新的在前
pub fn remote_candidates(store: &Store, limit: usize) -> Result<Vec<i64>> {
    let c = store.r();
    let mut st = c.prepare(
        "SELECT m.id FROM messages m JOIN accounts a ON a.id = m.account_id WHERE m.body_state = 0 AND m.uid > 0 AND a.provider != 'local'
           AND (m.subject LIKE '%发票%' OR m.subject LIKE '%invoice%' OR m.subject LIKE '%收据%' OR m.subject LIKE '%receipt%' OR m.subject LIKE '%电子客票%')
         ORDER BY m.date DESC LIMIT ?",
    )?;
    let rows = st.query_map([limit as i64], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InvoiceRow {
    pub message_id: i64,
    pub att_index: i64,
    pub filename: String,
    pub amount_cents: Option<i64>,
    pub seller: String,
    pub buyer: String,
    /// yyyy-mm-dd；解析不出开票日期时用邮件日期
    pub date: String,
    pub number: String,
    pub kind: String,
    pub subject: String,
}

/// 月份表达式：开票日期优先，没有就用邮件日期
const MONTH_EXPR: &str = "COALESCE(NULLIF(substr(i.inv_date, 1, 7), ''), strftime('%Y-%m', m.date / 1000, 'unixepoch', 'localtime'))";
const DATE_EXPR: &str = "COALESCE(NULLIF(i.inv_date, ''), strftime('%Y-%m-%d', m.date / 1000, 'unixepoch', 'localtime'))";

/// 发票列表；`month` = "2025-03" 时只看该月。日期新的在前
pub fn list(store: &Store, month: Option<&str>) -> Result<Vec<InvoiceRow>> {
    let c = store.r();
    let sql = format!(
        "SELECT i.message_id, i.att_index, i.filename, i.amount_cents, i.seller, i.buyer, {DATE_EXPR}, i.number, i.kind, m.subject
           FROM invoices i JOIN messages m ON m.id = i.message_id
          WHERE i.is_invoice = 1 AND (?1 IS NULL OR {MONTH_EXPR} = ?1) ORDER BY 7 DESC, i.id DESC"
    );
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map([month], |r| {
        Ok(InvoiceRow {
            message_id: r.get(0)?,
            att_index: r.get(1)?,
            filename: r.get(2)?,
            amount_cents: r.get(3)?,
            seller: r.get(4)?,
            buyer: r.get(5)?,
            date: r.get(6)?,
            number: r.get(7)?,
            kind: r.get(8)?,
            subject: r.get(9)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 每月的 (月份, 张数, 合计分)，新的在前
pub fn months(store: &Store) -> Result<Vec<(String, i64, i64)>> {
    let c = store.r();
    let sql = format!(
        "SELECT {MONTH_EXPR} AS mo, COUNT(*), COALESCE(SUM(i.amount_cents), 0) FROM invoices i JOIN messages m ON m.id = i.message_id
          WHERE i.is_invoice = 1 GROUP BY mo ORDER BY mo DESC"
    );
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn fmt_amount(cents: i64) -> String {
    format!("¥{}.{:02}", cents / 100, cents % 100)
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// 文件名里不能出现的字符换成下划线，并限制长度
fn safe_name(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
    t.trim().chars().take(40).collect()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportReport {
    pub zip: PathBuf,
    pub count: usize,
    pub total_cents: i64,
}

/// 把某个月的发票打成 ZIP：每张发票的 PDF + `汇总.csv`（带 BOM，Excel 直接打开不乱码）
pub fn export_month(store: &Store, month: &str, parent: &Path) -> Result<ExportReport> {
    let rows = list(store, Some(month))?;
    if rows.is_empty() {
        anyhow::bail!("{month} 没有发票");
    }
    let name = format!("发票-{month}");
    let work = store.dir().with_file_name("invoice-export-tmp");
    let _ = std::fs::remove_dir_all(&work);
    let dir = work.join(&name);
    std::fs::create_dir_all(&dir)?;
    let res = (|| -> Result<ExportReport> {
        let mut csv = String::from("\u{feff}开票日期,开票方,购买方,价税合计(元),发票号码,类型,邮件主题,文件\n");
        let mut total = 0;
        for (n, r) in rows.iter().enumerate() {
            let raw = messages::read_local_source(store, r.message_id).with_context(|| format!("邮件 #{} 的原文不在本地", r.message_id))?;
            let (_, _, bytes) = parse::attachment(&raw, r.att_index as usize).with_context(|| format!("邮件 #{} 的附件取不出", r.message_id))?;
            let amount = r.amount_cents.map(|c| format!("{}.{:02}", c / 100, c % 100)).unwrap_or_default();
            let file = format!(
                "{:02}_{}_{}_{}.pdf",
                n + 1,
                r.date,
                safe_name(if r.seller.is_empty() { "未识别" } else { &r.seller }),
                if amount.is_empty() { "未识别".into() } else { amount.clone() }
            );
            std::fs::write(dir.join(&file), bytes)?;
            total += r.amount_cents.unwrap_or(0);
            csv.push_str(
                &[r.date.as_str(), &r.seller, &r.buyer, &amount, &r.number, &r.kind, &r.subject, &file]
                    .iter()
                    .map(|s| csv_cell(s))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            csv.push('\n');
        }
        csv.push_str(&format!(",,,{}.{:02},,,合计 {} 张,\n", total / 100, total % 100, rows.len()));
        std::fs::write(dir.join("汇总.csv"), csv)?;
        std::fs::create_dir_all(parent)?;
        let zip = parent.join(format!("{name}-{}.zip", transfer::stamp(super::db::now_ms())));
        let _ = std::fs::remove_file(&zip);
        // macOS 自带 ditto：不增加依赖，中文文件名正常
        let st = std::process::Command::new("/usr/bin/ditto").args(["-c", "-k", "--keepParent"]).arg(&dir).arg(&zip).status().context("无法运行 ditto")?;
        if !st.success() {
            anyhow::bail!("打包失败（ditto 退出码 {:?}）", st.code());
        }
        Ok(ExportReport { zip, count: rows.len(), total_cents: total })
    })();
    let _ = std::fs::remove_dir_all(&work);
    res
}

#[cfg(test)]
mod store_tests {
    use super::*;

    fn eml_with_pdf(pdf: &[u8]) -> Vec<u8> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(pdf);
        let wrapped: Vec<String> = b64.as_bytes().chunks(76).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
        format!(
            "From: a@b.c\r\nTo: me@local.test\r\nSubject: 您的电子发票\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=XX\r\n\r\n--XX\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n请查收\r\n--XX\r\nContent-Type: application/pdf; name=\"inv.pdf\"\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"inv.pdf\"\r\n\r\n{}\r\n--XX--\r\n",
            wrapped.join("\r\n")
        )
        .into_bytes()
    }

    #[test]
    fn scan_list_months_and_export() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("data")).unwrap();
        crate::core::queries::tests::seed(&s);
        let pdf = std::fs::read("tests/fixtures/invoice_sample.pdf").unwrap();
        let raw = eml_with_pdf(&pdf);
        // store_body 会顺手识别
        crate::core::messages::store_body(&s, 1, &raw).unwrap();
        let rows = list(&s, None).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!((rows[0].amount_cents, rows[0].date.as_str()), (Some(12350), "2025-03-08"));
        assert_eq!(months(&s).unwrap(), vec![("2025-03".to_string(), 1, 12350)]);
        assert!(list(&s, Some("2024-01")).unwrap().is_empty());
        // 补扫不会重复处理已扫过的
        assert_eq!(scan_backlog(&s, 10).unwrap(), 0);
        // 非发票 PDF：记为已扫描但不进列表
        crate::core::messages::store_body(&s, 2, &eml_with_pdf(b"%PDF-1.4\n%%EOF")).unwrap();
        assert_eq!(list(&s, None).unwrap().len(), 1);
        // 导出
        let rep = export_month(&s, "2025-03", &dir.path().join("out")).unwrap();
        assert_eq!((rep.count, rep.total_cents), (1, 12350));
        assert!(rep.zip.exists() && std::fs::metadata(&rep.zip).unwrap().len() > 1000);
        assert!(export_month(&s, "2020-01", &dir.path().join("out")).is_err());
        assert_eq!(fmt_amount(12350), "¥123.50");
    }
}
