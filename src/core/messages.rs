//! 邮件行的写入：信封入库、正文落盘（.eml + 解析结果 + 全文索引）、删除。
//!
//! 「移动占位」：async-imap 的 MOVE 拿不到 COPYUID，为了移动后不丢已缓存的正文、不让阅读中的信消失，
//! 本地先把行挂到目标文件夹、uid 设成负数（-id）占位；目标文件夹下次同步拉到新 UID 时按 Message-ID 认领。

use super::db::{now_ms, Store};
use super::imap::{Flags, HeaderItem};
use super::model::Addr;
use super::parse::{self, Parsed};
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

pub struct FolderRef {
    pub id: i64,
    pub account_id: i64,
}

/// 写入信封；返回真正新增的封数（认领占位不算新增）
pub fn insert_envelopes(store: &Store, f: &FolderRef, items: &[HeaderItem]) -> Result<usize> {
    Ok(insert_envelopes_ids(store, f, items)?.len())
}

/// 同 [`insert_envelopes`]，返回新增行的 id（规则引擎要用）
pub fn insert_envelopes_ids(store: &Store, f: &FolderRef, items: &[HeaderItem]) -> Result<Vec<i64>> {
    if items.is_empty() {
        return Ok(vec![]);
    }
    store.tx(|tx| {
        let mut ids = Vec::new();
        for m in items {
            let e = &m.env;
            if claim_placeholder(tx, f.id, m.uid, &e.message_id, &m.flags)? {
                continue;
            }
            let date = e.date.or(m.internal_date).unwrap_or_else(now_ms);
            let changed = tx.execute(
                "INSERT INTO messages (account_id, folder_id, uid, message_id, in_reply_to, refs, subject, from_name, from_addr, to_json, cc_json, reply_to,
                   date, size, seen, flagged, answered, draft, has_att, thread, unsub, unsub_post, auth_res)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(folder_id, uid) DO NOTHING",
                params![
                    f.account_id,
                    f.id,
                    m.uid,
                    e.message_id,
                    e.in_reply_to,
                    e.refs,
                    e.subject,
                    e.from.name,
                    e.from.address,
                    serde_json::to_string(&e.to)?,
                    serde_json::to_string(&e.cc)?,
                    e.reply_to,
                    date,
                    m.size,
                    m.flags.seen as i64,
                    m.flags.flagged as i64,
                    m.flags.answered as i64,
                    m.flags.draft as i64,
                    e.maybe_att as i64,
                    e.thread_key(),
                    e.unsub,
                    e.unsub_post as i64,
                    e.auth_res
                ],
            )?;
            if changed > 0 {
                let id = tx.last_insert_rowid();
                fix_thread(tx, id)?;
                ids.push(id);
                Store::fts_upsert(tx, id, &e.subject, &parse::people_text(&e.from, &e.to, &e.cc), "")?;
            }
        }
        Ok(ids)
    })
}

/// 没有任何 Message-ID 的信自成一个会话
pub fn fix_thread(c: &Connection, id: i64) -> Result<()> {
    c.execute("UPDATE messages SET thread = 'id:' || id WHERE id = ? AND thread = ''", [id])?;
    Ok(())
}

fn claim_placeholder(c: &Connection, folder_id: i64, uid: u32, message_id: &str, flags: &Flags) -> Result<bool> {
    if message_id.is_empty() {
        return Ok(false);
    }
    let id: Option<i64> = c
        .query_row("SELECT id FROM messages WHERE folder_id = ? AND uid < 0 AND message_id = ? ORDER BY id LIMIT 1", params![folder_id, message_id], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(id) = id else { return Ok(false) };
    c.execute(
        "UPDATE OR IGNORE messages SET uid = ?, seen = ?, flagged = ?, answered = ? WHERE id = ?",
        params![uid, flags.seen as i64, flags.flagged as i64, flags.answered as i64, id],
    )?;
    Ok(true)
}

/// 目标文件夹同步完新邮件后，仍未认领的占位说明服务器上没有对应邮件 → 清掉。
/// `keep`：移动还在待办队列里（还没送达服务器）的行，不能清（否则远端失败或尚未执行时信就丢了）。
pub fn drop_stale_placeholders(store: &Store, folder_id: i64, keep: &std::collections::HashSet<i64>) -> Result<usize> {
    let mut rows = ids_with_account(store, "SELECT id, account_id FROM messages WHERE folder_id = ? AND uid < 0", [folder_id])?;
    rows.retain(|(id, _)| !keep.contains(id));
    let n = rows.len();
    remove(store, &rows)?;
    Ok(n)
}

pub fn ids_with_account(store: &Store, sql: &str, p: impl rusqlite::Params) -> Result<Vec<(i64, i64)>> {
    let c = store.r();
    let mut st = c.prepare(sql)?;
    let v = st.query_map(p, |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// 删除行 + 对应的 .eml
pub fn remove(store: &Store, rows: &[(i64, i64)]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    store.tx(|tx| delete_rows_in(tx, rows))?;
    remove_files(store, rows);
    Ok(())
}

/// 在调用方的事务里删行（文件等事务提交后再用 [`remove_files`] 删）
pub fn delete_rows_in(tx: &Connection, rows: &[(i64, i64)]) -> Result<()> {
    for (id, _) in rows {
        tx.execute("DELETE FROM messages WHERE id = ?", [id])?;
    }
    Ok(())
}

pub fn remove_files(store: &Store, rows: &[(i64, i64)]) {
    for (id, acc) in rows {
        let _ = std::fs::remove_file(store.eml_abs(*acc, *id));
    }
}

/// 清扫孤儿 .eml：库里已经没有对应消息行的原文文件（老版本删文件夹时靠外键级联留下的）。返回删了几个。
pub fn sweep_orphan_eml(store: &Store) -> Result<usize> {
    let root = store.dir().join("eml");
    let Ok(accs) = std::fs::read_dir(&root) else { return Ok(0) };
    let mut n = 0;
    for acc in accs.flatten() {
        let Some(acc_id) = acc.file_name().to_str().and_then(|s| s.parse::<i64>().ok()) else { continue };
        let known: std::collections::HashSet<i64> = {
            let c = store.r();
            let mut st = c.prepare("SELECT id FROM messages WHERE account_id = ?")?;
            let v = st.query_map([acc_id], |r| r.get(0))?.collect::<Result<_, _>>()?;
            v
        };
        let Ok(files) = std::fs::read_dir(acc.path()) else { continue };
        for f in files.flatten() {
            let name = f.file_name();
            let Some(id) = name.to_str().and_then(|s| s.strip_suffix(".eml")).and_then(|s| s.parse::<i64>().ok()) else { continue };
            if !known.contains(&id) && std::fs::remove_file(f.path()).is_ok() {
                n += 1;
            }
        }
    }
    Ok(n)
}

/// 原文落盘 + 解析结果入库 + 全文索引。
///
/// HTML 不再写进库（显示时从 .eml 现解析，见 `queries::detail`）：同一封正文以前在磁盘上存了 .eml、text、html、
/// 两份索引共四五份，html 往往是其中最大的一份。text 仍然保留：全文索引回填、摘要、回复引用都要用。
pub fn store_body(store: &Store, msg_id: i64, raw: &[u8]) -> Result<Parsed> {
    let (acc_id, subject, from_name, from_addr): (i64, String, String, String) =
        store.r().query_row("SELECT account_id, subject, from_name, from_addr FROM messages WHERE id = ?", [msg_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
    let parsed = match parse::parse_full(raw) {
        Some(p) => p,
        None => {
            store.w().execute("UPDATE messages SET body_state = 2 WHERE id = ?", [msg_id])?;
            anyhow::bail!("邮件原文无法解析");
        }
    };
    let file = store.eml_abs(acc_id, msg_id);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&file, raw)?;
    let subject = if subject.is_empty() { parsed.env.subject.clone() } else { subject };
    let from = Addr { name: from_name, address: from_addr };
    let (hosts, pixels) = parse::remote_trackers(&parsed.html);
    let trackers = if hosts.is_empty() { String::new() } else { format!("{pixels}|{}", hosts.join(",")) };
    let c = store.w();
    c.execute(
        "UPDATE messages SET body_state = 1, text = ?, html = '', att_json = ?, snippet = ?, eml_path = ?, has_att = ?, to_json = ?, cc_json = ?, trackers = ?,
           unsub = CASE WHEN unsub = '' THEN ? ELSE unsub END, unsub_post = MAX(unsub_post, ?),
           auth_res = CASE WHEN auth_res = '' THEN ? ELSE auth_res END WHERE id = ?",
        params![
            parsed.text,
            serde_json::to_string(&parsed.atts)?,
            parsed.snippet,
            Store::eml_rel(acc_id, msg_id),
            (parsed.files > 0) as i64,
            serde_json::to_string(&parsed.env.to)?,
            serde_json::to_string(&parsed.env.cc)?,
            trackers,
            parsed.env.unsub,
            parsed.env.unsub_post as i64,
            parsed.env.auth_res,
            msg_id
        ],
    )?;
    Store::fts_upsert(&c, msg_id, &subject, &parse::people_text(&from, &parsed.env.to, &parsed.env.cc), &parsed.text)?;
    Ok(parsed)
}

/// 本地已有原文就直接读
pub fn read_local_source(store: &Store, msg_id: i64) -> Option<Vec<u8>> {
    let p: String = store.r().query_row("SELECT eml_path FROM messages WHERE id = ?", [msg_id], |r| r.get(0)).ok()?;
    let path = store.resolve_eml(&p)?;
    std::fs::read(path).ok()
}

/// 正文是否已缓存（body_state=1 且文件还在）
pub fn has_body(store: &Store, msg_id: i64) -> bool {
    let r: Option<(i64, String)> = store.r().query_row("SELECT body_state, eml_path FROM messages WHERE id = ?", [msg_id], |r| Ok((r.get(0)?, r.get(1)?))).ok();
    matches!(r, Some((1, p)) if store.resolve_eml(&p).is_some_and(|p| p.exists()))
}

/// 把一批行挂到目标文件夹（占位）。返回 (行数, 其中未读数)
pub fn move_local(store: &Store, ids: &[i64], src_folder: i64, dest_folder: i64, placeholder: bool) -> Result<(usize, usize)> {
    store.tx(|tx| move_local_in(tx, ids, src_folder, dest_folder, placeholder))
}

/// 同 [`move_local`]，在调用方的事务里执行
pub fn move_local_in(tx: &Connection, ids: &[i64], src_folder: i64, dest_folder: i64, placeholder: bool) -> Result<(usize, usize)> {
    {
        let mut moved = 0;
        let mut unseen = 0;
        let mut next_uid: i64 = tx.query_row("SELECT COALESCE(MAX(uid), 0) FROM messages WHERE folder_id = ?", [dest_folder], |r| r.get(0))?;
        for id in ids {
            let seen: Option<i64> =
                tx.query_row("SELECT seen FROM messages WHERE id = ? AND folder_id = ?", params![id, src_folder], |r| r.get(0)).optional()?;
            let Some(seen) = seen else { continue };
            let uid = if placeholder {
                -id
            } else {
                next_uid += 1;
                next_uid
            };
            tx.execute("UPDATE OR IGNORE messages SET folder_id = ?, uid = ? WHERE id = ?", params![dest_folder, uid, id])?;
            moved += 1;
            if seen == 0 {
                unseen += 1;
            }
        }
        tx.execute("UPDATE folders SET total = MAX(0, total - ?), unseen = MAX(0, unseen - ?) WHERE id = ?", params![moved as i64, unseen as i64, src_folder])?;
        tx.execute("UPDATE folders SET total = total + ?, unseen = unseen + ? WHERE id = ?", params![moved as i64, unseen as i64, dest_folder])?;
        Ok((moved, unseen))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::parse::Envelope;

    fn setup() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        {
            let c = s.w();
            c.execute("INSERT INTO accounts(id,email,provider,imap_host,smtp_host,secret,created_at) VALUES(1,'a@b.c','custom','h','h','x',0)", []).unwrap();
            c.execute("INSERT INTO folders(id,account_id,path,name,role) VALUES(1,1,'INBOX','INBOX','inbox'),(2,1,'Trash','Trash','trash')", []).unwrap();
        }
        (dir, s)
    }

    fn item(uid: u32, mid: &str) -> HeaderItem {
        HeaderItem {
            uid,
            flags: Flags::default(),
            size: 10,
            internal_date: Some(1000 + uid as i64),
            env: Envelope { subject: format!("主题{uid}"), message_id: mid.into(), ..Default::default() },
        }
    }

    #[test]
    fn insert_store_move_claim() {
        let (_d, s) = setup();
        let f = FolderRef { id: 1, account_id: 1 };
        assert_eq!(insert_envelopes(&s, &f, &[item(1, "<a@x>"), item(2, "<b@x>")]).unwrap(), 2);
        assert_eq!(insert_envelopes(&s, &f, &[item(2, "<b@x>")]).unwrap(), 0, "重复不新增");
        let id: i64 = s.r().query_row("SELECT id FROM messages WHERE uid = 1", [], |r| r.get(0)).unwrap();
        let raw = b"From: x@y.z\r\nSubject: hi\r\nMessage-ID: <a@x>\r\n\r\n\xe5\x8f\x91\xe7\xa5\xa8\xe6\x9d\xa5\xe4\xba\x86 hello\r\n";
        store_body(&s, id, raw).unwrap();
        assert!(has_body(&s, id));
        assert_eq!(read_local_source(&s, id).unwrap(), raw.to_vec());
        let hits: i64 = s.r().query_row("SELECT count(*) FROM messages_fts WHERE messages_fts MATCH '\"发票来\"'", [], |r| r.get(0)).unwrap();
        assert_eq!(hits, 1);
        // 移到垃圾箱（占位）
        let (n, unseen) = move_local(&s, &[id], 1, 2, true).unwrap();
        assert_eq!((n, unseen), (1, 1));
        let uid: i64 = s.r().query_row("SELECT uid FROM messages WHERE id = ?", [id], |r| r.get(0)).unwrap();
        assert_eq!(uid, -id);
        // 目标夹同步到新 UID 77 → 认领占位，正文保留
        let t = FolderRef { id: 2, account_id: 1 };
        assert_eq!(insert_envelopes(&s, &t, &[item(77, "<a@x>")]).unwrap(), 0);
        let (uid, state): (i64, i64) = s.r().query_row("SELECT uid, body_state FROM messages WHERE id = ?", [id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((uid, state), (77, 1));
        // 没被认领的占位会被清掉
        let id2: i64 = s.r().query_row("SELECT id FROM messages WHERE uid = 2", [], |r| r.get(0)).unwrap();
        move_local(&s, &[id2], 1, 2, true).unwrap();
        // 移动还在待办队列里时不清
        assert_eq!(drop_stale_placeholders(&s, 2, &std::collections::HashSet::from([id2])).unwrap(), 0);
        assert_eq!(drop_stale_placeholders(&s, 2, &Default::default()).unwrap(), 1);
        let left: i64 = s.r().query_row("SELECT count(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 1);
        remove(&s, &[(id, 1)]).unwrap();
        assert!(!s.eml_abs(1, id).exists());
        let fts: i64 = s.r().query_row("SELECT count(*) FROM messages_fts WHERE messages_fts MATCH '\"发票来\"'", [], |r| r.get(0)).unwrap();
        assert_eq!(fts, 0, "触发器同步删索引");
    }

    #[test]
    fn sweeps_orphan_eml() {
        let (_d, s) = setup();
        insert_envelopes(&s, &FolderRef { id: 1, account_id: 1 }, &[item(1, "<a@x>")]).unwrap();
        let id: i64 = s.r().query_row("SELECT id FROM messages", [], |r| r.get(0)).unwrap();
        store_body(&s, id, b"From: x@y.z\r\nSubject: hi\r\n\r\nbody\r\n").unwrap();
        std::fs::write(s.eml_abs(1, 999), b"orphan").unwrap();
        assert_eq!(sweep_orphan_eml(&s).unwrap(), 1);
        assert!(s.eml_abs(1, id).exists() && !s.eml_abs(1, 999).exists());
    }
}
