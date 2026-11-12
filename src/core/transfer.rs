//! 导入 / 导出。
//!
//! 导入：.eml 单封、.mbox 归档 → 只进本地存档（挂在 provider='local' 的「本地归档」账号下，不写回任何服务器）。
//! 导出：mbox（按时间正序，mboxrd 转义）/ csv（带 BOM，Excel 直接打开）/ 标准 maildir 目录（cur/ new/ tmp/ + index.csv）。

use super::accounts;
use super::db::{now_ms, Store};
use super::engine::Engine;
use super::messages;
use super::parse;
use super::queries::{self, ExportRow, ListParams};
use anyhow::{bail, Result};
use chrono::{Local, TimeZone};
use rusqlite::{params, OptionalExtension};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

pub const DEFAULT_FOLDER: &str = "导入";
pub const MAX_IMPORT: u64 = 512 * 1024 * 1024;
/// 单次导出的上限；超出时结果里带 `truncated`，界面明确提示
pub const EXPORT_CAP: usize = 20_000;

// ---------------- 导入 ----------------

/// 返回 (本地账号 id, 文件夹 id)
pub fn ensure_local_folder(store: &Store, name: &str) -> Result<(i64, i64)> {
    let acc = accounts::ensure_local(store)?;
    let clean = if name.trim().is_empty() { DEFAULT_FOLDER } else { name.trim() };
    let f: Option<i64> = store.r().query_row("SELECT id FROM folders WHERE account_id = ? AND path = ?", params![acc.id, clean], |r| r.get(0)).optional()?;
    if let Some(f) = f {
        return Ok((acc.id, f));
    }
    let c = store.w();
    c.execute("INSERT INTO folders (account_id, path, name, delimiter, role, sync_enabled) VALUES (?,?,?,'/','',1)", params![acc.id, clean, clean])?;
    Ok((acc.id, c.last_insert_rowid()))
}

pub fn refresh_local_counts(store: &Store, folder_id: i64) -> Result<()> {
    store.w().execute(
        "UPDATE folders SET total = (SELECT COUNT(*) FROM messages WHERE folder_id = ?1),
           unseen = (SELECT COUNT(*) FROM messages WHERE folder_id = ?1 AND seen = 0) WHERE id = ?1",
        [folder_id],
    )?;
    Ok(())
}

/// mbox 拆成单封：遇到 From_ 行开新的一封，并还原 mboxrd 的 ">From" 转义。字节原样，不按 UTF-8 解码。
///
/// 只有「文件开头或前一行是空行」的 From_ 行才算分隔，正文里碰巧以 `From ` 开头的行不会把一封信劈成两半。
/// 读文件出错时停止迭代并把错误留在 `error` 里，调用方必须检查，不能当成正常结束。
pub struct MboxReader<R: BufRead> {
    r: R,
    cur: Option<Vec<u8>>,
    pub read: u64,
    done: bool,
    prev_blank: bool,
    pub error: Option<std::io::Error>,
}

impl<R: BufRead> MboxReader<R> {
    pub fn new(r: R) -> Self {
        MboxReader { r, cur: None, read: 0, done: false, prev_blank: true, error: None }
    }
}

fn is_from_line(l: &[u8]) -> bool {
    l.starts_with(b"From ") && l.len() > 5 && !l[5].is_ascii_whitespace()
}

impl<R: BufRead> Iterator for MboxReader<R> {
    type Item = Vec<u8>;
    fn next(&mut self) -> Option<Vec<u8>> {
        if self.done {
            return None;
        }
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = match self.r.read_until(b'\n', &mut line) {
                Ok(n) => n,
                Err(e) => {
                    self.done = true;
                    self.error = Some(e);
                    return None;
                }
            };
            if n == 0 {
                self.done = true;
                return self.cur.take().filter(|c| !c.is_empty());
            }
            self.read += n as u64;
            let mut l: &[u8] = &line;
            if l.ends_with(b"\n") {
                l = &l[..l.len() - 1];
            }
            if l.ends_with(b"\r") {
                l = &l[..l.len() - 1];
            }
            let separator = self.prev_blank && is_from_line(l);
            self.prev_blank = l.is_empty();
            if separator {
                let prev = self.cur.replace(Vec::new());
                if let Some(p) = prev.filter(|p| !p.is_empty()) {
                    return Some(p);
                }
                continue;
            }
            let Some(cur) = self.cur.as_mut() else { continue };
            let gt = l.iter().take_while(|b| **b == b'>').count();
            if gt > 0 && l[gt..].starts_with(b"From ") {
                cur.extend_from_slice(&l[1..]);
            } else {
                cur.extend_from_slice(l);
            }
            cur.push(b'\n');
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ImportStat {
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
    pub folder_id: i64,
    pub account_id: i64,
    pub errors: Vec<String>,
}

fn import_one(store: &Store, acc_id: i64, folder_id: i64, raw: &[u8], st: &mut ImportStat) {
    match import_raw(store, acc_id, folder_id, raw) {
        Ok(true) => st.imported += 1,
        Ok(false) => st.skipped += 1,
        Err(e) => {
            st.failed += 1;
            if st.errors.len() < 5 {
                st.errors.push(format!("{e:#}"));
            }
        }
    }
}

/// 导入一封原文到本地文件夹；返回 false 表示按 Message-ID 判定已存在而跳过
pub fn import_raw(store: &Store, acc_id: i64, folder_id: i64, raw: &[u8]) -> Result<bool> {
    let r = (|| -> Result<bool> {
        let Some(p) = parse::parse_full(raw) else { bail!("无法解析邮件") };
        let e = &p.env;
        if !e.message_id.is_empty() {
            let dup: Option<i64> = store
                .r()
                .query_row("SELECT id FROM messages WHERE account_id = ? AND message_id = ?", params![acc_id, e.message_id], |r| r.get(0))
                .optional()?;
            if dup.is_some() {
                return Ok(false);
            }
        }
        let seen = p.status.is_empty() || p.status.to_lowercase().contains('r');
        let flagged = p.x_status.to_lowercase().contains('f');
        let id = {
            let c = store.w();
            let uid: i64 = c.query_row("SELECT COALESCE(MAX(uid), 0) + 1 FROM messages WHERE folder_id = ?", [folder_id], |r| r.get(0))?;
            c.execute(
                "INSERT INTO messages (account_id, folder_id, uid, message_id, in_reply_to, refs, subject, from_name, from_addr, to_json, cc_json, reply_to, date, size, seen, flagged, thread)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    acc_id,
                    folder_id,
                    uid,
                    e.message_id,
                    e.in_reply_to,
                    e.refs,
                    e.subject,
                    e.from.name,
                    e.from.address,
                    serde_json::to_string(&e.to)?,
                    serde_json::to_string(&e.cc)?,
                    e.reply_to,
                    e.date.unwrap_or_else(now_ms),
                    raw.len() as i64,
                    seen as i64,
                    flagged as i64,
                    e.thread_key()
                ],
            )?;
            let id = c.last_insert_rowid();
            messages::fix_thread(&c, id)?;
            id
        };
        messages::store_body(store, id, raw)?;
        Ok(true)
    })();
    r
}

/// 导入一个文件；progress(已处理封数, 百分比)
pub fn import_file(store: &Store, file: &Path, folder: &str, mut progress: impl FnMut(&ImportStat, usize, u8)) -> Result<ImportStat> {
    let size = std::fs::metadata(file)?.len();
    if size > MAX_IMPORT {
        bail!("单个导入文件不能超过 512 MB，请拆分成多个 .mbox");
    }
    let ext = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let (acc, fid) = ensure_local_folder(store, folder)?;
    let mut st = ImportStat { folder_id: fid, account_id: acc, ..Default::default() };
    let is_mbox = matches!(ext.as_str(), "mbox" | "mbx" | "txt") || (ext.is_empty() && starts_with_from(file));
    if is_mbox {
        let mut rd = MboxReader::new(BufReader::with_capacity(256 * 1024, std::fs::File::open(file)?));
        let mut done = 0;
        while let Some(raw) = rd.next() {
            import_one(store, acc, fid, &raw, &mut st);
            done += 1;
            if done % 5 == 0 {
                progress(&st, done, ((rd.read * 100) / size.max(1)) as u8);
            }
        }
        progress(&st, done, 100);
        if let Some(e) = rd.error.take() {
            refresh_local_counts(store, fid)?;
            bail!("读取 mbox 时出错，导入在第 {done} 封后中断（已导入 {} 封）：{e}", st.imported);
        }
    } else {
        let raw = std::fs::read(file)?;
        import_one(store, acc, fid, &raw, &mut st);
        progress(&st, 1, 100);
    }
    refresh_local_counts(store, fid)?;
    Ok(st)
}

/// 无扩展名的文件：只读前 5 个字节判断是不是 mbox（不把可能几百 MB 的文件整个读进内存）
fn starts_with_from(file: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 5];
    std::fs::File::open(file).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == b"From "
}

// ---------------- 导出 ----------------

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// maildir 文件名：`<秒>.M<id>.easymail:2,<标记>`，标记按 ASCII 排序（F 星标、S 已读）
fn maildir_name(r: &ExportRow) -> String {
    let mut flags = String::new();
    if r.flagged {
        flags.push('F');
    }
    if r.seen {
        flags.push('S');
    }
    format!("{}.M{}.easymail:2,{flags}", r.date.max(0) / 1000, r.id)
}

/// mbox 的 From_ 行用 ctime 风格时间（本地时区）
pub fn ctime(ts: i64) -> String {
    use chrono::{Datelike, Timelike};
    let d = Local.timestamp_millis_opt(ts).single().unwrap_or_else(Local::now);
    format!(
        "{} {} {:>2} {:02}:{:02}:{:02} {}",
        DAYS[d.weekday().num_days_from_sunday() as usize],
        MONTHS[d.month0() as usize],
        d.day(),
        d.hour(),
        d.minute(),
        d.second(),
        d.year()
    )
}

pub fn stamp(ts: i64) -> String {
    Local.timestamp_millis_opt(ts).single().unwrap_or_else(Local::now).format("%Y%m%d-%H%M").to_string()
}

/// mboxrd 转义：行首 "From "（以及 ">From "）多加一个 ">"，行尾统一成 LF
pub fn escape_mbox(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() + 64);
    for raw in src.split(|b| *b == b'\n') {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        let gt = line.iter().take_while(|b| **b == b'>').count();
        if line[gt..].starts_with(b"From ") {
            out.push(b'>');
        }
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    while out.ends_with(b"\n\n") {
        out.pop();
    }
    out.push(b'\n');
    out
}

fn csv_cell(s: &str) -> String {
    // 以 = + - @ 开头的单元格会被表格软件当成公式执行（CSV 注入），前面垫一个单引号
    let guarded;
    let s = if s.starts_with(['=', '+', '-', '@']) {
        guarded = format!("'{s}");
        guarded.as_str()
    } else {
        s
    };
    if s.contains(['"', ',', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn csv_of(rows: &[ExportRow]) -> String {
    let mut lines = vec!["日期,发件人,发件人邮箱,收件人,主题,账号,文件夹,已读,星标,附件,大小,Message-ID".to_string()];
    for r in rows {
        let date = Local.timestamp_millis_opt(r.date).single().map(|d| d.format("%Y/%m/%d %H:%M:%S").to_string()).unwrap_or_default();
        let to = r.to.iter().map(|a| a.address.as_str()).filter(|a| !a.is_empty()).collect::<Vec<_>>().join(" ");
        let cells = [
            date,
            r.from_name.clone(),
            r.from_addr.clone(),
            to,
            r.subject.clone(),
            r.account_email.clone(),
            r.folder_label.clone(),
            if r.seen { "是" } else { "否" }.into(),
            if r.flagged { "是" } else { "否" }.into(),
            r.atts.to_string(),
            r.size.to_string(),
            r.message_id.clone(),
        ];
        lines.push(cells.iter().map(|c| csv_cell(c)).collect::<Vec<_>>().join(","));
    }
    format!("\u{feff}{}\r\n", lines.join("\r\n"))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExportKind {
    Mbox,
    Csv,
    Maildir,
}

#[derive(Clone, Debug, Default)]
pub struct ExportStat {
    pub rows: usize,
    /// 命中数超过上限、只导出了前 `EXPORT_CAP` 封
    pub truncated: bool,
    pub written: usize,
    pub failed: usize,
    pub path: PathBuf,
}

/// 导出到 `dest`（mbox/csv 为文件路径，maildir 为父目录）；正文没缓存的先从服务器取回
pub async fn export(eng: Engine, p: ListParams, kind: ExportKind, dest: PathBuf, label: String) -> Result<ExportStat> {
    let store = eng.store().clone();
    let mut rows = queries::export_rows(&store, &p, EXPORT_CAP + 1)?;
    let truncated = rows.len() > EXPORT_CAP;
    rows.truncate(EXPORT_CAP);
    let mut st = ExportStat { rows: rows.len(), truncated, ..Default::default() };
    match kind {
        ExportKind::Csv => {
            std::fs::write(&dest, csv_of(&rows))?;
            st.written = rows.len();
            st.path = dest;
        }
        ExportKind::Mbox => {
            let mut f = std::io::BufWriter::new(std::fs::File::create(&dest)?);
            for r in &rows {
                match eng.source(r.id).await {
                    Ok(src) => {
                        let from = if r.from_addr.is_empty() { "MAILER-DAEMON" } else { r.from_addr.as_str() };
                        writeln!(f, "From {} {}", from, ctime(r.date))?;
                        f.write_all(&escape_mbox(&src))?;
                        f.write_all(b"\n")?;
                        st.written += 1;
                    }
                    Err(_) => st.failed += 1,
                }
            }
            f.flush()?;
            st.path = dest;
        }
        ExportKind::Maildir => {
            let safe: String = label.chars().map(|c| if "\\/:*?\"<>| ".contains(c) { '-' } else { c }).take(40).collect();
            let dir = dest.join(format!("{}-{}", if safe.is_empty() { "export".into() } else { safe }, stamp(now_ms())));
            // 标准 maildir：cur/ new/ tmp/ 三个目录齐全，文件名带 :2, 标记信息，其它客户端可直接当 maildir 打开
            for d in ["cur", "new", "tmp"] {
                std::fs::create_dir_all(dir.join(d))?;
            }
            for r in &rows {
                match eng.source(r.id).await {
                    Ok(src) => {
                        std::fs::write(dir.join("cur").join(maildir_name(r)), src)?;
                        st.written += 1;
                    }
                    Err(_) => st.failed += 1,
                }
            }
            std::fs::write(dir.join("index.csv"), csv_of(&rows))?;
            let skipped = if st.failed > 0 { format!("（{} 封正文无法取回，已跳过）", st.failed) } else { String::new() };
            std::fs::write(
                dir.join("README.txt"),
                format!(
                    "EasyMail · 迷你邮件 导出\n时间：{}\n邮件数：{}{skipped}\n\n整个目录是标准 maildir（cur/ new/ tmp/），可直接用支持 maildir 的客户端打开；\ncur/ 里每个文件都是一封完整的 RFC 822 邮件（可改扩展名为 .eml 单独打开）。index.csv 是元数据索引。\n",
                    Local::now().format("%Y/%m/%d %H:%M:%S"),
                    st.written
                ),
            )?;
            st.path = dir;
        }
    }
    Ok(st)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MBOX: &[u8] = b"From a@b.c Mon Jan  1 00:00:00 2024\nFrom: a@b.c\nSubject: one\nMessage-ID: <1@x>\nStatus: RO\n\nhello\n>From the start\n\nFrom b@b.c Tue Jan  2 00:00:00 2024\nFrom: b@b.c\nSubject: =?UTF-8?B?5Lit5paH?=\nMessage-ID: <2@x>\nX-Status: F\nStatus: O\n\nbody2\n";

    #[test]
    fn mbox_split_and_escape_roundtrip() {
        let msgs: Vec<Vec<u8>> = MboxReader::new(std::io::Cursor::new(MBOX)).collect();
        assert_eq!(msgs.len(), 2);
        assert!(String::from_utf8_lossy(&msgs[0]).contains("\nFrom the start\n"), "还原 >From");
        let esc = escape_mbox(&msgs[0]);
        assert!(String::from_utf8_lossy(&esc).contains("\n>From the start\n"));
        let mut again = Vec::new();
        for m in &msgs {
            again.extend_from_slice(format!("From x@y {}\n", ctime(0)).as_bytes());
            again.extend_from_slice(&escape_mbox(m));
            again.push(b'\n');
        }
        let back: Vec<Vec<u8>> = MboxReader::new(std::io::Cursor::new(again)).collect();
        assert_eq!(back.len(), 2);
        assert_eq!(parse::parse_full(&back[1]).unwrap().env.subject, "中文");
    }

    #[test]
    fn import_dedup_and_flags() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let f = dir.path().join("a.mbox");
        std::fs::write(&f, MBOX).unwrap();
        let mut last = 0u8;
        let st = import_file(&s, &f, "", |_, _, pct| last = pct).unwrap();
        assert_eq!((st.imported, st.skipped, st.failed, last), (2, 0, 0, 100));
        let st2 = import_file(&s, &f, "", |_, _, _| {}).unwrap();
        assert_eq!((st2.imported, st2.skipped), (0, 2), "按 Message-ID 去重");
        let rows: Vec<(String, i64, i64, i64)> = {
            let c = s.r();
            let mut q = c.prepare("SELECT subject, seen, flagged, body_state FROM messages ORDER BY uid").unwrap();
            let v = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(|x| x.unwrap()).collect();
            v
        };
        assert_eq!(rows, vec![("one".into(), 1, 0, 1), ("中文".into(), 0, 1, 1)]);
        let (total, unseen): (i64, i64) =
            s.r().query_row("SELECT total, unseen FROM folders WHERE id = ?", [st.folder_id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((total, unseen), (2, 1));
        let eml = dir.path().join("x.eml");
        std::fs::write(&eml, b"From: c@d.e\r\nSubject: single\r\n\r\nhi\r\n").unwrap();
        assert_eq!(import_file(&s, &eml, "单封", |_, _, _| {}).unwrap().imported, 1);
    }

    #[test]
    fn maildir_names() {
        let r = ExportRow { id: 7, date: 1_700_000_000_123, seen: true, flagged: true, ..Default::default() };
        assert_eq!(maildir_name(&r), "1700000000.M7.easymail:2,FS");
        assert_eq!(maildir_name(&ExportRow { id: 8, ..Default::default() }), "0.M8.easymail:2,");
    }

    #[test]
    fn csv_format() {
        let rows = vec![ExportRow { subject: "a,\"b\"".into(), date: 0, seen: true, ..Default::default() }];
        let c = csv_of(&rows);
        assert!(c.starts_with('\u{feff}'));
        assert!(c.contains("\"a,\"\"b\"\"\""));
        assert!(c.ends_with("\r\n"));
        let evil = vec![ExportRow { subject: "=HYPERLINK(\"http://x\")".into(), from_name: "@cmd".into(), ..Default::default() }];
        let c = csv_of(&evil);
        assert!(c.contains("'@cmd") && c.contains("\"'=HYPERLINK(\"\"http://x\"\")\""), "{c}");
    }

    #[test]
    fn mbox_body_from_line_is_not_a_separator() {
        let raw =
            b"From a@b.c Mon Jan  1 00:00:00 2024\nSubject: one\n\nline1\nFrom here on it is body\n\nFrom b@b.c Tue Jan  2 00:00:00 2024\nSubject: two\n\nx\n";
        let msgs: Vec<Vec<u8>> = MboxReader::new(std::io::Cursor::new(&raw[..])).collect();
        assert_eq!(msgs.len(), 2);
        assert!(String::from_utf8_lossy(&msgs[0]).contains("line1\nFrom here on it is body\n"));
    }

    #[test]
    fn mbox_read_error_is_reported() {
        struct Broken(usize);
        impl std::io::Read for Broken {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let data = b"From a@b.c Mon Jan  1 00:00:00 2024\nSubject: one\n\nhi\n";
                if self.0 >= data.len() {
                    return Err(std::io::Error::other("磁盘读错误"));
                }
                let n = buf.len().min(data.len() - self.0);
                buf[..n].copy_from_slice(&data[self.0..self.0 + n]);
                self.0 += n;
                Ok(n)
            }
        }
        let mut rd = MboxReader::new(BufReader::new(Broken(0)));
        assert!(rd.next().is_none());
        assert!(rd.error.is_some(), "IO 错误不能被当成文件结束");
    }
}
