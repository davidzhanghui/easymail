//! 只读查询：列表 / 搜索 / 详情 / 侧栏计数 / 联系人补全。界面直接在读连接上调用。
//!
//! 搜索语法：普通词（≥ 3 个字符走 trigram 全文索引，更短走 LIKE），另支持
//!   from:张三   to:xx@163.com   subject:发票   is:unread   is:starred   has:attachment

use super::db::Store;
use super::model::*;
use super::providers::folder_label;
use anyhow::Result;
use rusqlite::types::Value;
use rusqlite::{params_from_iter, OptionalExtension, Row};

const HIDDEN_ROLES: &str = "('trash','junk')";

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
}

#[derive(Debug, Default, PartialEq)]
pub struct ParsedQuery {
    pub terms: Vec<String>,
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub subject: Vec<String>,
    pub unread: bool,
    pub starred: bool,
    pub att: bool,
}

pub fn parse_query(q: &str) -> ParsedQuery {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r#"(\w+):("([^"]*)"|\S+)|"([^"]+)"|(\S+)"#).expect("regex"));
    let re = &*RE;
    let mut out = ParsedQuery::default();
    for m in re.captures_iter(q) {
        if let Some(k) = m.get(1) {
            let key = k.as_str().to_lowercase();
            let val = m.get(3).or(m.get(2)).map(|v| v.as_str().to_string()).unwrap_or_default();
            let lv = val.to_lowercase();
            match key.as_str() {
                "from" => out.from.push(val),
                "to" => out.to.push(val),
                "subject" => out.subject.push(val),
                "is" if lv.contains("unread") || val.contains("未读") => out.unread = true,
                "is" if lv.contains("star") || lv.contains("flag") || val.contains("星标") => out.starred = true,
                "has" if lv.contains("att") || val.contains("附件") => out.att = true,
                _ => out.terms.push(m.get(0).map(|x| x.as_str().to_string()).unwrap_or_default()),
            }
        } else if let Some(t) = m.get(4).or(m.get(5)) {
            out.terms.push(t.as_str().to_string());
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

/// 把 view / folder / account / 筛选 / 搜索词编译成 WHERE 片段
pub fn build_filter(p: &ListParams) -> (Vec<String>, Vec<Value>) {
    let mut w: Vec<String> = Vec::new();
    let mut a: Vec<Value> = Vec::new();
    let has_q = !p.q.trim().is_empty();
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
            View::All => {
                if has_q {
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
    match p.filter {
        Filter::Unread => w.push("m.seen = 0".into()),
        Filter::Starred => w.push("m.flagged = 1".into()),
        Filter::Attachments => w.push("m.has_att = 1".into()),
        Filter::All => {}
    }
    if has_q {
        let q = parse_query(p.q.trim());
        let mut fts = Vec::new();
        let mut grams = Vec::new();
        for t in &q.terms {
            let n = t.chars().count();
            if n >= 3 {
                fts.push(format!("\"{}\"", t.replace('"', "\"\"")));
            } else if n == 2 && t.chars().all(super::db::is_cjk) && super::db::bigram_ready() {
                // 两个字的中文词（发票、合同）走二元组索引，不再对正文全表 LIKE
                grams.push(format!("\"{t}\""));
            } else {
                w.push(
                    "(m.subject LIKE ? ESCAPE '\\' OR m.from_name LIKE ? ESCAPE '\\' OR m.from_addr LIKE ? ESCAPE '\\' OR m.to_json LIKE ? ESCAPE '\\' OR m.text LIKE ? ESCAPE '\\')"
                        .into(),
                );
                for _ in 0..5 {
                    a.push(like(t).into());
                }
            }
        }
        if !fts.is_empty() {
            w.push("m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)".into());
            a.push(fts.join(" AND ").into());
        }
        if !grams.is_empty() {
            w.push("m.id IN (SELECT rowid FROM messages_bigram WHERE messages_bigram MATCH ?)".into());
            a.push(grams.join(" AND ").into());
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
        if q.unread {
            w.push("m.seen = 0".into());
        }
        if q.starred {
            w.push("m.flagged = 1".into());
        }
        if q.att {
            w.push("m.has_att = 1".into());
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
    })
}

pub fn list(store: &Store, p: &ListParams, cursor: Option<(i64, i64)>, limit: usize) -> Result<Page> {
    let (w, args) = build_filter(p);
    let limit = limit.clamp(1, 500);
    let base = format!("{FROM_JOIN} WHERE {}", if w.is_empty() { "1 = 1".into() } else { w.join(" AND ") });
    let c = store.r();
    let total: i64 = c.query_row(&format!("SELECT COUNT(*) {base}"), params_from_iter(args.iter()), |r| r.get(0))?;
    let mut page = args.clone();
    let mut cur = String::new();
    if let Some((d, id)) = cursor {
        cur = " AND (m.date < ? OR (m.date = ? AND m.id < ?))".into();
        page.extend([Value::from(d), Value::from(d), Value::from(id)]);
    }
    page.push(Value::from(limit as i64 + 1));
    let sql = format!(
        "SELECT m.id, m.account_id, m.folder_id, m.subject, m.from_name, m.from_addr, m.to_json, m.date,
           m.seen, m.flagged, m.answered, m.has_att, m.snippet, m.size, f.name, f.role, a.email, a.name, a.color
         {base}{cur} ORDER BY m.date DESC, m.id DESC LIMIT ?"
    );
    let mut st = c.prepare(&sql)?;
    let mut items = st.query_map(params_from_iter(page.iter()), list_item)?.collect::<Result<Vec<_>, _>>()?;
    let more = items.len() > limit;
    items.truncate(limit);
    let next = if more { items.last().map(|l| (l.date, l.id)) } else { None };
    Ok(Page { total, items, next })
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
               m.in_reply_to, m.refs
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
                    },
                    r.get::<_, String>(23)?,
                    r.get::<_, String>(24)?,
                ))
            },
        )
        .optional()?;
    let Some((mut d, in_reply_to, refs)) = row else { return Ok(None) };
    if d.html.contains("cid:") {
        // 内嵌图片从 .eml 现取（库里不再存 base64 展开后的 HTML）
        let eml: Option<String> = c.query_row("SELECT eml_path FROM messages WHERE id = ?", [id], |r| r.get(0)).ok();
        if let Some(raw) = eml.and_then(|p| store.resolve_eml(&p)).and_then(|p| std::fs::read(p).ok()) {
            d.html = super::parse::inline_cids_from_raw(&d.html, &raw);
        }
    }
    let refs: Vec<String> = std::iter::once(in_reply_to).chain(refs.split_whitespace().map(String::from)).filter(|s| !s.is_empty()).collect();
    if !d.message_id.is_empty() || !refs.is_empty() {
        let placeholders = if refs.is_empty() { "''".to_string() } else { vec!["?"; refs.len()].join(",") };
        let sql = format!(
            "SELECT m.id, m.subject, m.from_name, m.from_addr, m.date, f.role, f.name FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE m.id != ? AND m.account_id = ? AND ((m.message_id != '' AND m.message_id IN ({placeholders}))
               OR (? != '' AND (m.in_reply_to = ? OR instr(m.refs, ?) > 0)))
             ORDER BY m.date LIMIT 30"
        );
        let mut args: Vec<Value> = vec![d.id.into(), d.account_id.into()];
        args.extend(refs.iter().map(|r| Value::from(r.clone())));
        for _ in 0..3 {
            args.push(d.message_id.clone().into());
        }
        let mut st = c.prepare(&sql)?;
        d.thread = st
            .query_map(params_from_iter(args.iter()), |r| {
                Ok(ThreadItem {
                    id: r.get(0)?,
                    subject: r.get(1)?,
                    from: Addr { name: r.get(2)?, address: r.get(3)? },
                    date: r.get(4)?,
                    folder: folder_label(&r.get::<_, String>(5)?, &r.get::<_, String>(6)?),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
    }
    Ok(Some(d))
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
              (SELECT COUNT(*) FROM messages WHERE body_state = 1)"
        ),
        [],
        |r| Ok(Stats { inbox: r.get(0)?, unread: r.get(1)?, starred: r.get(2)?, drafts: r.get(3)?, archived: r.get(4)?, with_body: r.get(5)? }),
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
mod tests {
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
                "INSERT INTO messages(id,account_id,folder_id,uid,subject,from_name,from_addr,to_json,date,seen,flagged,has_att,message_id,in_reply_to,refs,text) VALUES(?,1,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                rusqlite::params![id, fid, id, subj, fname, faddr, to, date, seen, flagged, att, mid, irt, refs, format!("正文{id}")],
            )
            .unwrap();
            Store::fts_upsert(&c, id, subj, &format!("{fname} {faddr}"), &format!("正文{id}")).unwrap();
        }
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
        assert_eq!(d.thread.iter().map(|t| t.id).collect::<Vec<_>>(), vec![2]);
        let d2 = detail(&s, 2).unwrap().unwrap();
        assert_eq!(d2.thread.iter().map(|t| t.id).collect::<Vec<_>>(), vec![3]);
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
