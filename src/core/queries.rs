//! 只读查询：列表 / 搜索 / 详情 / 侧栏计数 / 联系人补全。界面直接在读连接上调用。
//!
//! 搜索语法：普通词（≥ 3 个字符走 trigram 全文索引；两个汉字走二元组索引；更短的只匹配主题与联系人），另支持
//!   from:张三   to:xx@163.com   subject:发票   is:unread   is:starred   has:attachment
//!   before:2024-01-01   after:2023-06-01   larger:5M   smaller:100K   filename:pdf   in:sent   -排除词

use super::db::{now_ms, Store};
use super::model::*;
use super::providers::folder_label;
use anyhow::Result;
use rusqlite::types::Value;
use rusqlite::{params_from_iter, OptionalExtension, Row};

const HIDDEN_ROLES: &str = "('trash','junk')";
/// 稍后处理中、还没到点的邮件不出现在普通列表里
const NOT_SNOOZED: &str = "m.id NOT IN (SELECT message_id FROM snoozed)";
/// 列表总数最多数到这里，再多就显示「10000+」（大邮箱里精确计数要扫全部命中行）
pub const COUNT_CAP: i64 = 10_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum View {
    #[default]
    Inbox,
    Unread,
    Starred,
    Sent,
    Attachments,
    Drafts,
    All,
    /// 稍后处理
    Snoozed,
    /// 发票（附件是 PDF / OFD 且主题或附件名像发票）
    Invoices,
}

impl View {
    pub fn key(self) -> &'static str {
        match self {
            View::Inbox => "inbox",
            View::Unread => "unread",
            View::Starred => "starred",
            View::Sent => "sent",
            View::Attachments => "attachments",
            View::Drafts => "drafts",
            View::All => "all",
            View::Snoozed => "snoozed",
            View::Invoices => "invoices",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Unread,
    Starred,
    Attachments,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListParams {
    pub view: View,
    pub folder_id: Option<i64>,
    pub account_id: Option<i64>,
    pub filter: Filter,
    pub q: String,
    pub ids: Vec<i64>,
    /// 会话视图：同一会话只出一行（最新的一封），附带会话内封数
    pub threads: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct ParsedQuery {
    pub terms: Vec<String>,
    pub exclude: Vec<String>,
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub subject: Vec<String>,
    pub filename: Vec<String>,
    pub unread: bool,
    pub starred: bool,
    pub att: bool,
    pub before: Option<i64>,
    pub after: Option<i64>,
    pub larger: Option<i64>,
    pub smaller: Option<i64>,
    pub folder_role: Option<String>,
}

/// "2024-01-02" / "2024/1/2" / "2024-01" → 当天（当月）0 点的毫秒时间戳（本地时区）
fn parse_date(v: &str) -> Option<i64> {
    use chrono::{Local, NaiveDate, TimeZone};
    let v = v.replace('/', "-");
    let d = NaiveDate::parse_from_str(&v, "%Y-%m-%d").ok().or_else(|| NaiveDate::parse_from_str(&format!("{v}-01"), "%Y-%m-%d").ok())?;
    Local.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).single().map(|t| t.timestamp_millis())
}

/// "5M" / "300k" / "1.5mb" / "2048" → 字节
fn parse_size(v: &str) -> Option<i64> {
    let l = v.trim().to_lowercase();
    let l = l.trim_end_matches('b');
    let (num, mul) = match l.chars().last()? {
        'k' => (&l[..l.len() - 1], 1024.0),
        'm' => (&l[..l.len() - 1], 1024.0 * 1024.0),
        'g' => (&l[..l.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (l, 1.0),
    };
    num.trim().parse::<f64>().ok().map(|n| (n * mul) as i64)
}

pub fn parse_query(q: &str) -> ParsedQuery {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r#"(-?)(\w+):("([^"]*)"|\S+)|(-?)"([^"]+)"|(\S+)"#).expect("regex"));
    let re = &*RE;
    let mut out = ParsedQuery::default();
    for m in re.captures_iter(q) {
        if let Some(k) = m.get(2) {
            let key = k.as_str().to_lowercase();
            let val = m.get(4).or(m.get(3)).map(|v| v.as_str().to_string()).unwrap_or_default();
            let lv = val.to_lowercase();
            match key.as_str() {
                "from" => out.from.push(val),
                "to" => out.to.push(val),
                "subject" => out.subject.push(val),
                "filename" | "file" => out.filename.push(val),
                "is" if lv.contains("unread") || val.contains("未读") => out.unread = true,
                "is" if lv.contains("star") || lv.contains("flag") || val.contains("星标") => out.starred = true,
                "has" if lv.contains("att") || val.contains("附件") => out.att = true,
                "before" if parse_date(&val).is_some() => out.before = parse_date(&val),
                "after" | "since" if parse_date(&val).is_some() => out.after = parse_date(&val),
                "larger" | "size" if parse_size(&val).is_some() => out.larger = parse_size(&val),
                "smaller" if parse_size(&val).is_some() => out.smaller = parse_size(&val),
                "in" => {
                    out.folder_role = Some(
                        match lv.as_str() {
                            "收件箱" => "inbox",
                            "已发送" => "sent",
                            "草稿" | "草稿箱" => "drafts",
                            "垃圾箱" | "已删除" | "bin" => "trash",
                            "垃圾邮件" | "spam" => "junk",
                            "归档" => "archive",
                            other => other,
                        }
                        .to_string(),
                    )
                }
                _ => {
                    let whole = m.get(0).map(|x| x.as_str().to_string()).unwrap_or_default();
                    match whole.strip_prefix('-') {
                        Some(rest) if !rest.is_empty() => out.exclude.push(rest.to_string()),
                        _ => out.terms.push(whole),
                    }
                }
            }
        } else if let Some(t) = m.get(6) {
            if m.get(5).is_some_and(|x| x.as_str() == "-") {
                out.exclude.push(t.as_str().to_string());
            } else {
                out.terms.push(t.as_str().to_string());
            }
        } else if let Some(t) = m.get(7) {
            let t = t.as_str();
            match t.strip_prefix('-') {
                Some(rest) if !rest.is_empty() => out.exclude.push(rest.to_string()),
                _ => out.terms.push(t.to_string()),
            }
        }
    }
    out
}

fn like(s: &str) -> String {
    let mut o = String::from("%");
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            o.push('\\');
        }
        o.push(c);
    }
    o.push('%');
    o
}

/// 发票的识别条件：附件里有 PDF / OFD，且主题、附件名里出现「发票 / invoice / 收据」之类的字眼
const INVOICE_COND: &str = "m.has_att = 1 AND (m.att_json LIKE '%.pdf%' OR m.att_json LIKE '%.ofd%' OR m.att_json LIKE '%application/pdf%' OR m.body_state = 0)
   AND (m.subject LIKE '%发票%' OR m.subject LIKE '%invoice%' OR m.subject LIKE '%收据%' OR m.subject LIKE '%receipt%' OR m.att_json LIKE '%发票%' OR m.att_json LIKE '%invoice%')";

/// 一个普通搜索词编译成的条件
fn term_cond(t: &str, w: &mut Vec<String>, a: &mut Vec<Value>, negate: bool) {
    let n = t.chars().count();
    let not = if negate { "NOT " } else { "" };
    if n >= 3 {
        w.push(format!("m.id {not}IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)"));
        a.push(format!("\"{}\"", t.replace('"', "\"\"")).into());
    } else if n == 2 && t.chars().all(super::db::is_cjk) && super::db::bigram_ready() {
        // 两个字的中文词（发票、合同）走二元组索引
        w.push(format!("m.id {not}IN (SELECT rowid FROM messages_bigram WHERE messages_bigram MATCH ?)"));
        a.push(format!("\"{t}\"").into());
    } else if n == 2 && t.chars().all(super::db::is_cjk) {
        // 二元组索引还在后台补建：暂时扫正文，补完后自动走索引
        w.push(format!(
            "{not}(m.subject LIKE ? ESCAPE '\\' OR m.from_name LIKE ? ESCAPE '\\' OR m.from_addr LIKE ? ESCAPE '\\' OR m.to_json LIKE ? ESCAPE '\\' OR m.text LIKE ? ESCAPE '\\')"
        ));
        for _ in 0..5 {
            a.push(like(t).into());
        }
    } else {
        // 一两个字符：只匹配主题和联系人（这几列短），不扫正文
        w.push(format!(
            "{not}(m.subject LIKE ? ESCAPE '\\' OR m.from_name LIKE ? ESCAPE '\\' OR m.from_addr LIKE ? ESCAPE '\\' OR m.to_json LIKE ? ESCAPE '\\')"
        ));
        for _ in 0..4 {
            a.push(like(t).into());
        }
    }
}

/// 把 view / folder / account / 筛选 / 搜索词编译成 WHERE 片段
pub fn build_filter(p: &ListParams) -> (Vec<String>, Vec<Value>) {
    let mut w: Vec<String> = Vec::new();
    let mut a: Vec<Value> = Vec::new();
    let has_q = !p.q.trim().is_empty();
    let q = if has_q { parse_query(p.q.trim()) } else { ParsedQuery::default() };
    if let Some(fid) = p.folder_id {
        w.push("m.folder_id = ?".into());
        a.push(fid.into());
    } else {
        w.push(match p.view {
            View::Inbox => "f.role = 'inbox'".into(),
            View::Unread => "m.seen = 0 AND f.role NOT IN ('trash','junk','sent','drafts')".into(),
            View::Starred => format!("m.flagged = 1 AND f.role NOT IN {HIDDEN_ROLES}"),
            View::Sent => "f.role = 'sent'".into(),
            View::Attachments => format!("m.has_att = 1 AND f.role NOT IN {HIDDEN_ROLES}"),
            View::Drafts => "f.role = 'drafts'".into(),
            View::Snoozed => "m.id IN (SELECT message_id FROM snoozed)".into(),
            View::Invoices => format!("{INVOICE_COND} AND f.role NOT IN {HIDDEN_ROLES}"),
            View::All => {
                if has_q && q.folder_role.is_none() {
                    "1 = 1".into()
                } else {
                    format!("f.role NOT IN {HIDDEN_ROLES}")
                }
            }
        });
        if let Some(acc) = p.account_id {
            w.push("m.account_id = ?".into());
            a.push(acc.into());
        }
    }
    if p.view != View::Snoozed && p.ids.is_empty() && !has_q {
        w.push(NOT_SNOOZED.into());
    }
    if p.ids.is_empty() && (p.view == View::Drafts || p.folder_id.is_some()) {
        // 本机草稿同步到服务器的副本：列表里已经有本机草稿那一行了，不再重复显示
        w.push("m.message_id NOT IN (SELECT '<' || remote_mid || '>' FROM drafts WHERE remote_mid != '')".into());
    }
    match p.filter {
        Filter::Unread => w.push("m.seen = 0".into()),
        Filter::Starred => w.push("m.flagged = 1".into()),
        Filter::Attachments => w.push("m.has_att = 1".into()),
        Filter::All => {}
    }
    if has_q {
        // 多个 ≥3 字的词合成一次 FTS 查询（AND），其余逐个加条件
        let (long, short): (Vec<&String>, Vec<&String>) = q.terms.iter().partition(|t| t.chars().count() >= 3);
        if !long.is_empty() {
            w.push("m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)".into());
            a.push(long.iter().map(|t| format!("\"{}\"", t.replace('"', "\"\""))).collect::<Vec<_>>().join(" AND ").into());
        }
        for t in short {
            term_cond(t, &mut w, &mut a, false);
        }
        for t in &q.exclude {
            term_cond(t, &mut w, &mut a, true);
        }
        for v in &q.from {
            w.push("(m.from_name LIKE ? ESCAPE '\\' OR m.from_addr LIKE ? ESCAPE '\\')".into());
            a.push(like(v).into());
            a.push(like(v).into());
        }
        for v in &q.to {
            w.push("(m.to_json LIKE ? ESCAPE '\\' OR m.cc_json LIKE ? ESCAPE '\\')".into());
            a.push(like(v).into());
            a.push(like(v).into());
        }
        for v in &q.subject {
            w.push("m.subject LIKE ? ESCAPE '\\'".into());
            a.push(like(v).into());
        }
        for v in &q.filename {
            w.push("m.att_json LIKE ? ESCAPE '\\'".into());
            a.push(like(v).into());
        }
        if q.unread {
            w.push("m.seen = 0".into());
        }
        if q.starred {
            w.push("m.flagged = 1".into());
        }
        if q.att {
            w.push("m.has_att = 1".into());
        }
        if let Some(t) = q.before {
            w.push("m.date < ?".into());
            a.push(t.into());
        }
        if let Some(t) = q.after {
            w.push("m.date >= ?".into());
            a.push(t.into());
        }
        if let Some(n) = q.larger {
            w.push("m.size >= ?".into());
            a.push(n.into());
        }
        if let Some(n) = q.smaller {
            w.push("m.size < ?".into());
            a.push(n.into());
        }
        if let Some(r) = &q.folder_role {
            w.push("(f.role = ? OR lower(f.name) = ?)".into());
            a.push(r.clone().into());
            a.push(r.to_lowercase().into());
        }
    }
    if !p.ids.is_empty() {
        w.push(format!("m.id IN ({})", vec!["?"; p.ids.len()].join(",")));
        a.extend(p.ids.iter().map(|i| Value::from(*i)));
    }
    (w, a)
}

const FROM_JOIN: &str = "FROM messages m JOIN folders f ON f.id = m.folder_id JOIN accounts a ON a.id = m.account_id";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub total: i64,
    pub items: Vec<ListItem>,
    /// (date, id) 游标
    pub next: Option<(i64, i64)>,
}

const LIST_COLS: &str = "m.id, m.account_id, m.folder_id, m.subject, m.from_name, m.from_addr, m.to_json, m.date,
           m.seen, m.flagged, m.answered, m.has_att, m.snippet, m.size, f.name, f.role, a.email, a.name, a.color";

fn list_item(r: &Row) -> rusqlite::Result<ListItem> {
    Ok(ListItem {
        id: r.get(0)?,
        account_id: r.get(1)?,
        folder_id: r.get(2)?,
        subject: r.get(3)?,
        from: Addr { name: r.get(4)?, address: r.get(5)? },
        to: parse_addrs_json(&r.get::<_, String>(6)?),
        date: r.get(7)?,
        seen: r.get::<_, i64>(8)? != 0,
        flagged: r.get::<_, i64>(9)? != 0,
        answered: r.get::<_, i64>(10)? != 0,
        has_att: r.get::<_, i64>(11)? != 0,
        snippet: r.get(12)?,
        size: r.get(13)?,
        folder_name: folder_label(&r.get::<_, String>(15)?, &r.get::<_, String>(14)?),
        folder_role: r.get(15)?,
        account_email: r.get(16)?,
        account_name: r.get(17)?,
        account_color: r.get(18)?,
        thread_count: r.get::<_, Option<i64>>(19).ok().flatten().unwrap_or(1),
    })
}

/// 取一页。`count`：是否重新计数（翻下一页时不必，`total` 返回 -1 表示沿用上次的值）。
/// 计数最多数到 [`COUNT_CAP`]，超过时返回 `COUNT_CAP + 1`，界面显示「10000+」。
pub fn list_page(store: &Store, p: &ListParams, cursor: Option<(i64, i64)>, limit: usize, count: bool) -> Result<Page> {
    let (w, args) = build_filter(p);
    let limit = limit.clamp(1, 500);
    let cond = if w.is_empty() { "1 = 1".into() } else { w.join(" AND ") };
    let base = format!("{FROM_JOIN} WHERE {cond}");
    let c = store.r();
    let mut page = args.clone();
    let mut cur = String::new();
    if let Some((d, id)) = cursor {
        cur = " AND (sd < ? OR (sd = ? AND sid < ?))".into();
        page.extend([Value::from(d), Value::from(d), Value::from(id)]);
    }
    page.push(Value::from(limit as i64 + 1));
    let (sql, count_sql) = if p.threads {
        // 会话视图：每个会话取最新的一封，带上会话内（命中条件的）封数
        (
            format!(
                "SELECT * FROM (SELECT {LIST_COLS}, COUNT(*) OVER w AS n, ROW_NUMBER() OVER (w ORDER BY m.date DESC, m.id DESC) AS rn, m.date AS sd, m.id AS sid
                 {base} WINDOW w AS (PARTITION BY m.account_id, m.thread)) WHERE rn = 1{cur} ORDER BY sd DESC, sid DESC LIMIT ?"
            ),
            format!("SELECT COUNT(*) FROM (SELECT DISTINCT m.account_id, m.thread {base} LIMIT {})", COUNT_CAP + 1),
        )
    } else {
        (
            format!("SELECT * FROM (SELECT {LIST_COLS}, 1 AS n, m.date AS sd, m.id AS sid {base}) WHERE 1 = 1{cur} ORDER BY sd DESC, sid DESC LIMIT ?"),
            format!("SELECT COUNT(*) FROM (SELECT 1 {base} LIMIT {})", COUNT_CAP + 1),
        )
    };
    let total: i64 = if count { c.query_row(&count_sql, params_from_iter(args.iter()), |r| r.get(0))? } else { -1 };
    let mut st = c.prepare(&sql)?;
    let mut items = st.query_map(params_from_iter(page.iter()), list_item)?.collect::<Result<Vec<_>, _>>()?;
    let more = items.len() > limit;
    items.truncate(limit);
    let next = if more { items.last().map(|l| (l.date, l.id)) } else { None };
    Ok(Page { total, items, next })
}

/// 取一页并计数
pub fn list(store: &Store, p: &ListParams, cursor: Option<(i64, i64)>, limit: usize) -> Result<Page> {
    list_page(store, p, cursor, limit, true)
}

/// 导出用的行：全部命中、按时间正序
#[derive(Clone, Debug, Default)]
pub struct ExportRow {
    pub id: i64,
    pub date: i64,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub to: Vec<Addr>,
    pub seen: bool,
    pub flagged: bool,
    pub size: i64,
    pub message_id: String,
    pub atts: usize,
    pub account_email: String,
    pub folder_label: String,
}

pub fn export_rows(store: &Store, p: &ListParams, cap: usize) -> Result<Vec<ExportRow>> {
    let (w, mut args) = build_filter(p);
    args.push(Value::from(cap as i64));
    let sql = format!(
        "SELECT m.id, m.date, m.subject, m.from_name, m.from_addr, m.to_json, m.seen, m.flagged, m.size, m.message_id, m.att_json, a.email, f.role, f.name
         {FROM_JOIN} WHERE {} ORDER BY m.date, m.id LIMIT ?",
        if w.is_empty() { "1 = 1".into() } else { w.join(" AND ") }
    );
    let c = store.r();
    let mut st = c.prepare(&sql)?;
    let v = st
        .query_map(params_from_iter(args.iter()), |r| {
            let atts: Vec<AttMeta> = serde_json::from_str(&r.get::<_, String>(10)?).unwrap_or_default();
            Ok(ExportRow {
                id: r.get(0)?,
                date: r.get(1)?,
                subject: r.get(2)?,
                from_name: r.get(3)?,
                from_addr: r.get(4)?,
                to: parse_addrs_json(&r.get::<_, String>(5)?),
                seen: r.get::<_, i64>(6)? != 0,
                flagged: r.get::<_, i64>(7)? != 0,
                size: r.get(8)?,
                message_id: r.get(9)?,
                atts: atts.iter().filter(|a| !a.inline).count(),
                account_email: r.get(11)?,
                folder_label: folder_label(&r.get::<_, String>(12)?, &r.get::<_, String>(13)?),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// (总数, 正文未缓存数)
pub fn count_export(store: &Store, p: &ListParams) -> Result<(i64, i64)> {
    let (w, args) = build_filter(p);
    let sql = format!(
        "SELECT COUNT(*), COALESCE(SUM(CASE WHEN m.body_state != 1 OR m.eml_path = '' THEN 1 ELSE 0 END), 0) {FROM_JOIN} WHERE {}",
        if w.is_empty() { "1 = 1".into() } else { w.join(" AND ") }
    );
    Ok(store.r().query_row(&sql, params_from_iter(args.iter()), |r| Ok((r.get(0)?, r.get(1)?)))?)
}

pub fn detail(store: &Store, id: i64) -> Result<Option<Detail>> {
    let c = store.r();
    let row = c
        .query_row(
            "SELECT m.id, m.account_id, m.folder_id, m.subject, m.message_id, m.from_name, m.from_addr, m.reply_to, m.to_json, m.cc_json, m.date,
               m.seen, m.flagged, m.answered, COALESCE(m.text, ''), COALESCE(m.html, ''), m.att_json, a.email, a.name, a.color, a.provider, f.role, f.name,
               m.thread, m.eml_path, m.body_state, m.unsub, m.unsub_post, m.auth_res, m.trackers, (SELECT until FROM snoozed WHERE message_id = m.id)
             FROM messages m JOIN folders f ON f.id = m.folder_id JOIN accounts a ON a.id = m.account_id WHERE m.id = ?",
            [id],
            |r| {
                Ok((
                    Detail {
                        id: r.get(0)?,
                        account_id: r.get(1)?,
                        folder_id: r.get(2)?,
                        subject: r.get(3)?,
                        message_id: r.get(4)?,
                        from: Addr { name: r.get(5)?, address: r.get(6)? },
                        reply_to: r.get(7)?,
                        to: parse_addrs_json(&r.get::<_, String>(8)?),
                        cc: parse_addrs_json(&r.get::<_, String>(9)?),
                        date: r.get(10)?,
                        seen: r.get::<_, i64>(11)? != 0,
                        flagged: r.get::<_, i64>(12)? != 0,
                        answered: r.get::<_, i64>(13)? != 0,
                        text: r.get(14)?,
                        html: r.get(15)?,
                        attachments: serde_json::from_str(&r.get::<_, String>(16)?).unwrap_or_default(),
                        account_email: r.get(17)?,
                        account_name: r.get(18)?,
                        account_color: r.get(19)?,
                        account_local: r.get::<_, String>(20)? == super::providers::LOCAL_PROVIDER,
                        folder_role: r.get(21)?,
                        folder_label: folder_label(&r.get::<_, String>(21)?, &r.get::<_, String>(22)?),
                        thread: vec![],
                        unsub: r.get(26)?,
                        unsub_post: r.get::<_, i64>(27)? != 0,
                        auth_res: r.get(28)?,
                        trackers: r.get(29)?,
                        snoozed_until: r.get(30)?,
                    },
                    r.get::<_, String>(23)?,
                    r.get::<_, String>(24)?,
                    r.get::<_, i64>(25)?,
                ))
            },
        )
        .optional()?;
    let Some((mut d, thread, eml_path, body_state)) = row else { return Ok(None) };
    // HTML 不进库：正文已缓存时从 .eml 现解析（毫秒级）；老数据库里存过的 html 照常用
    let raw =
        if body_state == 1 && (d.html.is_empty() || d.html.contains("cid:")) { store.resolve_eml(&eml_path).and_then(|p| std::fs::read(p).ok()) } else { None };
    if let Some(raw) = &raw {
        if d.html.is_empty() {
            if let Some(p) = super::parse::parse_full(raw) {
                d.html = p.html;
            }
        }
        if d.html.contains("cid:") {
            // 内嵌图片从 .eml 现取（库里不存 base64 展开后的 HTML）
            d.html = super::parse::inline_cids_from_raw(&d.html, raw);
        }
    }
    if !thread.is_empty() {
        // 整个会话（含当前这封），按时间从旧到新：阅读区的会话时间线用
        let mut st = c.prepare(
            "SELECT m.id, m.subject, m.from_name, m.from_addr, m.date, f.role, f.name, m.seen, m.snippet FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE m.account_id = ? AND m.thread = ? ORDER BY m.date, m.id LIMIT 100",
        )?;
        d.thread = st
            .query_map(rusqlite::params![d.account_id, thread], |r| {
                let role: String = r.get(5)?;
                Ok(ThreadItem {
                    id: r.get(0)?,
                    subject: r.get(1)?,
                    from: Addr { name: r.get(2)?, address: r.get(3)? },
                    date: r.get(4)?,
                    folder: folder_label(&role, &r.get::<_, String>(6)?),
                    role,
                    seen: r.get::<_, i64>(7)? != 0,
                    snippet: r.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
    }
    Ok(Some(d))
}

/// 发件人视图的一行
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sender {
    pub address: String,
    pub name: String,
    pub count: i64,
    pub unread: i64,
    pub bytes: i64,
    pub last: i64,
    /// 有退订链接
    pub unsub: bool,
}

/// 按发件人聚合（只看收件箱与自定义文件夹，不含已发送 / 草稿 / 垃圾箱），按封数倒序
pub fn senders(store: &Store, account_id: Option<i64>, limit: usize) -> Result<Vec<Sender>> {
    let c = store.r();
    let mut st = c.prepare(
        "SELECT m.from_addr, MAX(m.from_name), COUNT(*), SUM(m.seen = 0), SUM(m.size), MAX(m.date), MAX(m.unsub != '')
         FROM messages m JOIN folders f ON f.id = m.folder_id
         WHERE m.from_addr != '' AND f.role NOT IN ('sent','drafts','trash','junk') AND (?1 IS NULL OR m.account_id = ?1)
         GROUP BY m.from_addr ORDER BY COUNT(*) DESC LIMIT ?2",
    )?;
    let v = st
        .query_map(rusqlite::params![account_id, limit as i64], |r| {
            Ok(Sender {
                address: r.get(0)?,
                name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                count: r.get(2)?,
                unread: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                bytes: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                last: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                unsub: r.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// 某个发件人的全部邮件 id（不含已发送 / 草稿 / 垃圾箱）
pub fn sender_message_ids(store: &Store, address: &str, account_id: Option<i64>) -> Result<Vec<i64>> {
    let c = store.r();
    let mut st = c.prepare(
        "SELECT m.id FROM messages m JOIN folders f ON f.id = m.folder_id
         WHERE m.from_addr = ?1 AND f.role NOT IN ('sent','drafts','trash','junk') AND (?2 IS NULL OR m.account_id = ?2)",
    )?;
    let v = st.query_map(rusqlite::params![address, account_id], |r| r.get(0))?.collect::<Result<Vec<i64>, _>>()?;
    Ok(v)
}

/// 隐私报告：过去 `days` 天里带追踪像素 / 远程图片的发件人，(发件人, 封数, 像素数, 主机)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackerRow {
    pub sender: String,
    pub mails: i64,
    pub pixels: i64,
    pub hosts: Vec<String>,
}

pub fn tracker_report(store: &Store, days: i64) -> Result<Vec<TrackerRow>> {
    let c = store.r();
    let mut st = c.prepare("SELECT from_addr, trackers FROM messages WHERE trackers != '' AND date >= ?")?;
    let since = now_ms() - days * 86_400_000;
    let mut map: std::collections::BTreeMap<String, TrackerRow> = Default::default();
    for row in st.query_map([since], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (sender, t) = row?;
        let (px, hosts) = t.split_once('|').unwrap_or(("0", ""));
        let e = map.entry(sender.clone()).or_insert_with(|| TrackerRow { sender, ..Default::default() });
        e.mails += 1;
        e.pixels += px.parse::<i64>().unwrap_or(0);
        for h in hosts.split(',').filter(|h| !h.is_empty()) {
            if !e.hosts.iter().any(|x| x == h) {
                e.hosts.push(h.to_string());
            }
        }
    }
    let mut v: Vec<TrackerRow> = map.into_values().filter(|r| r.pixels > 0).collect();
    v.sort_by(|a, b| b.pixels.cmp(&a.pixels).then(b.mails.cmp(&a.mails)));
    Ok(v)
}

/// 发件人是否在「始终显示远程图片」名单里
pub fn remote_allowed(store: &Store, address: &str) -> bool {
    store.r().query_row("SELECT 1 FROM remote_allow WHERE address = ?", [address.to_lowercase()], |_| Ok(())).is_ok()
}

pub fn stats(store: &Store) -> Result<Stats> {
    Ok(store.r().query_row(
        &format!(
            "SELECT
              (SELECT COALESCE(SUM(unseen), 0) FROM folders WHERE role = 'inbox'),
              (SELECT COUNT(*) FROM messages m JOIN folders f ON f.id = m.folder_id WHERE m.seen = 0 AND f.role NOT IN ('trash','junk','sent','drafts')),
              (SELECT COUNT(*) FROM messages m JOIN folders f ON f.id = m.folder_id WHERE m.flagged = 1 AND f.role NOT IN {HIDDEN_ROLES}),
              (SELECT COUNT(*) FROM drafts),
              (SELECT COUNT(*) FROM messages),
              (SELECT COUNT(*) FROM messages WHERE body_state = 1),
              (SELECT COUNT(*) FROM snoozed),
              (SELECT COUNT(*) FROM outbox WHERE state != 'sent')"
        ),
        [],
        |r| {
            Ok(Stats {
                inbox: r.get(0)?,
                unread: r.get(1)?,
                starred: r.get(2)?,
                drafts: r.get(3)?,
                archived: r.get(4)?,
                with_body: r.get(5)?,
                snoozed: r.get(6)?,
                outbox: r.get(7)?,
            })
        },
    )?)
}

/// 未读收件箱数（Dock 角标 / 托盘）
pub fn inbox_unread(store: &Store) -> i64 {
    store.r().query_row("SELECT COALESCE(SUM(unseen), 0) FROM folders WHERE role = 'inbox'", [], |r| r.get(0)).unwrap_or(0)
}

pub fn folders(store: &Store) -> Result<Vec<Folder>> {
    let c = store.r();
    let cols: String = Folder::COLS.split(", ").map(|x| format!("f.{x}")).collect::<Vec<_>>().join(", ");
    let mut st =
        c.prepare(&format!("SELECT {cols}, (SELECT COUNT(*) FROM messages m WHERE m.folder_id = f.id) FROM folders f ORDER BY f.account_id, f.path"))?;
    let v = st
        .query_map([], |r| {
            let mut f = Folder::from_row(r)?;
            f.local = r.get(15)?;
            Ok(f)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    // 侧栏顺序：同账号内先系统文件夹（按阅读习惯），再自定义文件夹按路径
    let mut v = v;
    v.sort_by(|a, b| a.account_id.cmp(&b.account_id).then(display_rank(&a.role).cmp(&display_rank(&b.role))).then(a.path.cmp(&b.path)));
    Ok(v)
}

/// 侧栏展示顺序：收件箱 → 草稿 → 已发送 → 星标 / 重要 / 所有邮件 → 归档 → 垃圾 → 已删除 → 自定义
fn display_rank(role: &str) -> usize {
    const ORDER: [&str; 9] = ["inbox", "drafts", "sent", "flagged", "important", "all", "archive", "junk", "trash"];
    ORDER.iter().position(|r| *r == role).unwrap_or(99)
}

pub fn contacts(store: &Store, q: &str) -> Result<Vec<Contact>> {
    let k = like(q);
    let c = store.r();
    let mut st = c.prepare(
        "SELECT name, address, SUM(n) n FROM (
           SELECT from_name name, from_addr address, COUNT(*) n FROM messages WHERE from_addr != '' AND (from_addr LIKE ?1 ESCAPE '\\' OR from_name LIKE ?1 ESCAPE '\\') GROUP BY from_addr
           UNION ALL
           SELECT json_extract(j.value, '$.name'), lower(json_extract(j.value, '$.address')), COUNT(*) * 3
             FROM messages m JOIN folders f ON f.id = m.folder_id, json_each(m.to_json) j
             WHERE f.role = 'sent' AND (json_extract(j.value, '$.address') LIKE ?1 ESCAPE '\\' OR json_extract(j.value, '$.name') LIKE ?1 ESCAPE '\\')
             GROUP BY lower(json_extract(j.value, '$.address'))
         ) WHERE address IS NOT NULL GROUP BY address ORDER BY n DESC LIMIT 8",
    )?;
    let v = st
        .query_map([k], |r| {
            Ok(Contact { name: r.get::<_, Option<String>>(0)?.unwrap_or_default(), address: r.get::<_, Option<String>>(1)?.unwrap_or_default() })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::core::db::Store;

    #[test]
    fn query_syntax() {
        let q = parse_query(r#"from:张三 to:"a b" 发票 is:unread has:附件 subject:周报 "exact phrase" xx:yy"#);
        assert_eq!(q.from, vec!["张三"]);
        assert_eq!(q.to, vec!["a b"]);
        assert_eq!(q.subject, vec!["周报"]);
        assert!(q.unread && q.att && !q.starred);
        assert_eq!(q.terms, vec!["发票", "exact phrase", "xx:yy"]);
    }

    pub fn seed(s: &Store) {
        let c = s.w();
        c.execute_batch(
            "INSERT INTO accounts(id,email,name,provider,imap_host,smtp_host,secret,created_at,color) VALUES(1,'me@x.com','我','custom','h','h','x',0,'accent');
             INSERT INTO folders(id,account_id,path,name,role,unseen) VALUES(1,1,'INBOX','INBOX','inbox',2),(2,1,'Sent','Sent','sent',0),(3,1,'Trash','Trash','trash',0);",
        )
        .unwrap();
        let rows = [
            (1, 1, "增值税发票已开具", "张三", "zhang@corp.com", r#"[{"name":"我","address":"me@x.com"}]"#, 1000, 0, 0, 1, "<a@x>", "", ""),
            (2, 1, "Weekly report", "Bob", "bob@ex.com", "[]", 2000, 1, 1, 0, "<b@x>", "", ""),
            (3, 2, "Re: Weekly report", "我", "me@x.com", r#"[{"name":"Bob","address":"bob@ex.com"}]"#, 3000, 1, 0, 0, "<c@x>", "<b@x>", "<b@x>"),
            (4, 3, "垃圾", "spam", "s@s.com", "[]", 4000, 0, 0, 0, "", "", ""),
            (5, 1, "短", "李四", "li@ex.com", "[]", 1000, 0, 0, 0, "", "", ""),
        ];
        for (id, fid, subj, fname, faddr, to, date, seen, flagged, att, mid, irt, refs) in rows {
            c.execute(
                "INSERT INTO messages(id,account_id,folder_id,uid,subject,from_name,from_addr,to_json,date,seen,flagged,has_att,message_id,in_reply_to,refs,text,thread) VALUES(?,1,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                rusqlite::params![id, fid, id, subj, fname, faddr, to, date, seen, flagged, att, mid, irt, refs, format!("正文{id}"), {
                    let t = crate::core::parse::thread_key(mid, irt, refs);
                    if t.is_empty() { format!("id:{id}") } else { t }
                }],
            )
            .unwrap();
            Store::fts_upsert(&c, id, subj, &format!("{fname} {faddr}"), &format!("正文{id}")).unwrap();
        }
    }

    #[test]
    fn extended_syntax() {
        let q = parse_query("before:2024-02-01 after:2023/12 larger:5M smaller:100k filename:pdf in:已发送 -广告 -\"每周 简报\" 合同");
        assert!(q.before.is_some() && q.after.is_some() && q.after < q.before);
        assert_eq!((q.larger, q.smaller), (Some(5 * 1024 * 1024), Some(100 * 1024)));
        assert_eq!(q.filename, vec!["pdf"]);
        assert_eq!(q.folder_role.as_deref(), Some("sent"));
        assert_eq!(q.exclude, vec!["广告", "每周 简报"]);
        assert_eq!(q.terms, vec!["合同"]);
    }

    #[test]
    fn threads_snooze_senders_cap() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        seed(&s);
        let ids = |p: ListParams| list(&s, &p, None, 50).unwrap().items.iter().map(|i| (i.id, i.thread_count)).collect::<Vec<_>>();
        // 会话视图：2 和 3 是同一会话，只出最新的 3，计数 2
        assert_eq!(ids(ListParams { view: View::All, threads: true, ..Default::default() }), vec![(3, 2), (5, 1), (1, 1)]);
        let p = list(&s, &ListParams { view: View::All, threads: true, ..Default::default() }, None, 1).unwrap();
        assert_eq!(p.total, 3);
        let p2 = list(&s, &ListParams { view: View::All, threads: true, ..Default::default() }, p.next, 1).unwrap();
        assert_eq!(p2.items[0].id, 5);
        // 不计数翻页
        assert_eq!(list_page(&s, &ListParams::default(), None, 1, false).unwrap().total, -1);
        // 稍后处理：从收件箱消失，出现在「稍后处理」
        s.w().execute("INSERT INTO snoozed(message_id, until) VALUES (2, 99999999999999)", []).unwrap();
        assert_eq!(ids(ListParams::default()).iter().map(|x| x.0).collect::<Vec<_>>(), vec![5, 1]);
        assert_eq!(ids(ListParams { view: View::Snoozed, ..Default::default() }).iter().map(|x| x.0).collect::<Vec<_>>(), vec![2]);
        // 排除词、时间
        assert_eq!(
            ids(ListParams { view: View::All, q: "-weekly".into(), ..Default::default() }).iter().map(|x| x.0).collect::<Vec<_>>(),
            vec![4, 5, 1],
            "搜索含垃圾箱"
        );
        assert_eq!(ids(ListParams { view: View::All, q: "in:sent".into(), ..Default::default() }).iter().map(|x| x.0).collect::<Vec<_>>(), vec![3]);
        // 发件人聚合
        let ss = senders(&s, None, 10).unwrap();
        assert!(ss.iter().any(|x| x.address == "bob@ex.com" && x.count == 1));
        assert_eq!(sender_message_ids(&s, "zhang@corp.com", None).unwrap(), vec![1]);
        // 发票视图
        s.w().execute("UPDATE messages SET att_json = '[{\"filename\":\"fp.pdf\"}]', body_state = 1 WHERE id = 1", []).unwrap();
        assert_eq!(ids(ListParams { view: View::Invoices, ..Default::default() }).iter().map(|x| x.0).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn listing_and_search() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        seed(&s);
        let ids = |p: ListParams| list(&s, &p, None, 50).unwrap().items.iter().map(|i| i.id).collect::<Vec<_>>();
        assert_eq!(ids(ListParams::default()), vec![2, 5, 1], "收件箱按时间倒序，同时间按 id 倒序");
        assert_eq!(ids(ListParams { view: View::Unread, ..Default::default() }), vec![5, 1]);
        assert_eq!(ids(ListParams { view: View::Starred, ..Default::default() }), vec![2]);
        assert_eq!(ids(ListParams { view: View::Sent, ..Default::default() }), vec![3]);
        assert_eq!(ids(ListParams { view: View::All, ..Default::default() }), vec![3, 2, 5, 1], "全部不含垃圾箱");
        assert_eq!(ids(ListParams { view: View::All, q: "垃圾".into(), ..Default::default() }), vec![4], "搜索包括垃圾箱");
        assert_eq!(ids(ListParams { view: View::All, q: "增值税".into(), ..Default::default() }), vec![1], "≥3 字走 FTS");
        assert_eq!(ids(ListParams { view: View::All, q: "发票".into(), ..Default::default() }), vec![1], "2 字中文走二元组索引");
        assert_eq!(ids(ListParams { view: View::All, q: "正文".into(), ..Default::default() }).len(), 5, "2 字词能搜到正文");
        assert_eq!(ids(ListParams { view: View::All, q: "李".into(), ..Default::default() }), vec![5], "单字走 LIKE");
        assert_eq!(ids(ListParams { view: View::All, q: "from:bob".into(), ..Default::default() }), vec![2]);
        assert_eq!(ids(ListParams { view: View::All, q: "to:bob is:starred".into(), ..Default::default() }), Vec::<i64>::new());
        assert_eq!(ids(ListParams { view: View::All, q: "has:attachment".into(), ..Default::default() }), vec![1]);
        assert_eq!(ids(ListParams { folder_id: Some(3), ..Default::default() }), vec![4]);
        assert_eq!(ids(ListParams { filter: Filter::Unread, ..Default::default() }), vec![5, 1]);
        // 分页游标
        let p1 = list(&s, &ListParams::default(), None, 2).unwrap();
        assert_eq!(p1.total, 3);
        assert_eq!(p1.items.len(), 2);
        let p2 = list(&s, &ListParams::default(), p1.next, 2).unwrap();
        assert_eq!(p2.items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![1]);
        assert!(p2.next.is_none());
        // 详情 + 会话
        let d = detail(&s, 3).unwrap().unwrap();
        // 会话含当前这封，按时间从旧到新
        assert_eq!(d.thread.iter().map(|t| t.id).collect::<Vec<_>>(), vec![2, 3]);
        let d2 = detail(&s, 2).unwrap().unwrap();
        assert_eq!(d2.thread.iter().map(|t| t.id).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(d2.thread.iter().map(|t| (t.role.as_str(), t.seen)).collect::<Vec<_>>().len(), 2);
        // 没有 References / Message-ID 关联的信，会话里只有它自己
        let solo = detail(&s, 4).unwrap().unwrap();
        assert_eq!(solo.thread.iter().map(|t| t.id).collect::<Vec<_>>(), vec![4]);
        assert_eq!(d2.folder_label, "收件箱");
        // 统计
        let st = stats(&s).unwrap();
        assert_eq!((st.inbox, st.unread, st.starred, st.archived), (2, 2, 1, 5));
        assert_eq!(inbox_unread(&s), 2);
        // 联系人：已发送里的收件人权重更高
        let cs = contacts(&s, "bo").unwrap();
        assert_eq!(cs[0].address, "bob@ex.com");
        assert_eq!(folders(&s).unwrap().iter().find(|f| f.id == 1).unwrap().local, 3);
        let (n, uncached) = count_export(&s, &ListParams { view: View::All, ..Default::default() }).unwrap();
        assert_eq!((n, uncached), (4, 4));
        assert_eq!(export_rows(&s, &ListParams::default(), 100).unwrap().len(), 3);
    }
}
