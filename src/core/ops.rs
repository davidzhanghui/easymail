//! 用户操作：标记 / 移动 / 删除 / 垃圾邮件 / 归档 / 本地文件夹管理。
//!
//! 远端操作走持久化队列（见 `pending.rs`）：本地改动与待办在同一个事务里落库（界面立刻有反馈），
//! 再请账号 worker 把待办送到服务器并等结果：
//! - 成功：完成；
//! - 连接类错误：本地改动保留，待办留在队列里，下次同步 / 联网后自动重试（提示一句，不算失败）；
//! - 服务器明确拒绝：worker 已按 undo 回滚本地，这里返回错误。
//!
//! 本地归档的邮件只改本地库。

use super::db::Store;
use super::engine::{Engine, Evt};
use super::messages;
use super::pending::{self, Outcome, Task, Undo};
use anyhow::{bail, Result};
use rusqlite::{params, params_from_iter, OptionalExtension};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
struct Row {
    id: i64,
    uid: i64,
    seen: bool,
    flagged: bool,
    message_id: String,
}

#[derive(Clone, Debug)]
struct Group {
    account_id: i64,
    folder_id: i64,
    path: String,
    role: String,
    local: bool,
    rows: Vec<Row>,
}

impl Group {
    /// 服务器上真实存在的行（占位的 uid 是负数，还没有服务器 UID）
    fn remote_rows(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter().filter(|r| r.uid > 0)
    }
    fn uids(&self) -> Vec<u32> {
        self.remote_rows().map(|r| r.uid as u32).collect()
    }
    fn ids(&self) -> Vec<i64> {
        self.rows.iter().map(|r| r.id).collect()
    }
    fn id_accounts(&self) -> Vec<(i64, i64)> {
        self.rows.iter().map(|r| (r.id, self.account_id)).collect()
    }
    fn needs_remote(&self) -> bool {
        !self.local && self.rows.iter().any(|r| r.uid > 0)
    }
}

/// 按「账号 + 文件夹」分组，一组一条待办
fn groups(store: &Store, ids: &[i64]) -> Result<Vec<Group>> {
    if ids.is_empty() {
        bail!("没有选中任何邮件");
    }
    let c = store.r();
    let sql = format!(
        "SELECT m.id, m.uid, m.account_id, m.folder_id, m.seen, m.flagged, m.message_id, f.path, f.role, a.provider FROM messages m
         JOIN folders f ON f.id = m.folder_id JOIN accounts a ON a.id = m.account_id WHERE m.id IN ({})",
        vec!["?"; ids.len()].join(",")
    );
    let mut st = c.prepare(&sql)?;
    let mut map: BTreeMap<i64, Group> = BTreeMap::new();
    let rows = st.query_map(params_from_iter(ids.iter()), |r| {
        Ok((
            Row { id: r.get(0)?, uid: r.get(1)?, seen: r.get::<_, i64>(4)? != 0, flagged: r.get::<_, i64>(5)? != 0, message_id: r.get(6)? },
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, String>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, String>(9)?,
        ))
    })?;
    for r in rows {
        let (row, acc, fid, path, role, provider) = r?;
        map.entry(fid)
            .or_insert_with(|| Group { account_id: acc, folder_id: fid, path, role, local: provider == super::providers::LOCAL_PROVIDER, rows: vec![] })
            .rows
            .push(row);
    }
    if map.is_empty() {
        bail!("找不到这些邮件，可能已被删除或移动，请刷新列表");
    }
    Ok(map.into_values().collect())
}

fn folder_changed(eng: &Engine, g: &Group) {
    eng.emit(Evt::Folder { account_id: g.account_id, folder_id: g.folder_id, added: 0 });
}

/// 请 worker 把这些待办送到服务器并等结果（见模块说明）
async fn submit(eng: &Engine, account_id: i64, op_ids: Vec<i64>, what: &str) -> Result<()> {
    if op_ids.is_empty() {
        return Ok(());
    }
    let out = match eng.flush(account_id, op_ids.clone()).await {
        Ok(o) => o,
        Err(e) => {
            // 队列本身不可用（应用正在退出等）：待办仍在库里，下次启动重放
            log::warn!("[ops] 提交{what}失败：{e:#}");
            Default::default()
        }
    };
    let mut deferred = None;
    for id in &op_ids {
        match out.get(id) {
            Some(Outcome::Rejected(m)) => bail!("服务器拒绝了{what}，本地已恢复原状：{m}"),
            Some(Outcome::Deferred(m)) => {
                deferred.get_or_insert_with(|| m.clone());
            }
            Some(Outcome::Done) | None => {}
        }
    }
    if let Some(m) = deferred {
        eng.toast(format!("{what}暂时没能同步到服务器，已保存在本地，联网后会自动重试（{m}）"), true);
    }
    Ok(())
}

async fn submit_all(eng: &Engine, by_acc: BTreeMap<i64, Vec<i64>>, what: &str) -> Result<()> {
    let mut first_err = None;
    for (acc, ids) in by_acc {
        if let Err(e) = submit(eng, acc, ids, what).await {
            first_err.get_or_insert(e);
        }
    }
    first_err.map_or(Ok(()), Err)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FlagChange {
    pub seen: Option<bool>,
    pub flagged: Option<bool>,
}

pub async fn set_flags(eng: Engine, ids: Vec<i64>, ch: FlagChange) -> Result<()> {
    let store = eng.store().clone();
    let gs = groups(&store, &ids)?;
    let mut by_acc: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for g in &gs {
        let op_ids = store.tx(|tx| {
            for r in &g.rows {
                if let Some(s) = ch.seen {
                    tx.execute("UPDATE messages SET seen = ? WHERE id = ?", params![s as i64, r.id])?;
                }
                if let Some(f) = ch.flagged {
                    tx.execute("UPDATE messages SET flagged = ? WHERE id = ?", params![f as i64, r.id])?;
                }
            }
            if let Some(s) = ch.seen {
                let delta = g.rows.iter().filter(|r| r.seen != s).count() as i64 * if s { -1 } else { 1 };
                tx.execute("UPDATE folders SET unseen = MAX(0, unseen + ?) WHERE id = ?", params![delta, g.folder_id])?;
            }
            let mut ops = Vec::new();
            if g.needs_remote() {
                if let Some(s) = ch.seen {
                    let undo = Undo { column: "seen".into(), flags: g.remote_rows().map(|r| (r.id, r.seen)).collect(), ..Default::default() };
                    ops.push(pending::enqueue(tx, g.account_id, &Task::Flag { path: g.path.clone(), uids: g.uids(), flag: "\\Seen".into(), add: s }, &undo)?);
                }
                if let Some(f) = ch.flagged {
                    let undo = Undo { column: "flagged".into(), flags: g.remote_rows().map(|r| (r.id, r.flagged)).collect(), ..Default::default() };
                    ops.push(pending::enqueue(
                        tx,
                        g.account_id,
                        &Task::Flag { path: g.path.clone(), uids: g.uids(), flag: "\\Flagged".into(), add: f },
                        &undo,
                    )?);
                }
            }
            Ok(ops)
        })?;
        folder_changed(&eng, g);
        by_acc.entry(g.account_id).or_default().extend(op_ids);
    }
    submit_all(&eng, by_acc, if ch.seen.is_some() { "已读状态" } else { "星标" }).await
}

/// 回复成功后给原信加 \\Answered
pub async fn set_answered(eng: Engine, id: i64) -> Result<()> {
    let store = eng.store().clone();
    let gs = groups(&store, &[id])?;
    let mut by_acc: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for g in &gs {
        let op = store.tx(|tx| {
            tx.execute("UPDATE messages SET answered = 1 WHERE id = ?", [id])?;
            if !g.needs_remote() {
                return Ok(None);
            }
            let task = Task::Flag { path: g.path.clone(), uids: g.uids(), flag: "\\Answered".into(), add: true };
            let undo = Undo { column: "answered".into(), flags: g.remote_rows().map(|r| (r.id, false)).collect(), ..Default::default() };
            Ok(Some(pending::enqueue(tx, g.account_id, &task, &undo)?))
        })?;
        folder_changed(&eng, g);
        by_acc.entry(g.account_id).or_default().extend(op);
    }
    submit_all(&eng, by_acc, "回复标记").await
}

pub async fn mark_folder_read(eng: Engine, folder_id: i64) -> Result<()> {
    let store = eng.store().clone();
    let (acc, path, provider): (i64, String, String) = store
        .r()
        .query_row("SELECT f.account_id, f.path, a.provider FROM folders f JOIN accounts a ON a.id = f.account_id WHERE f.id = ?", [folder_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("文件夹不存在"))?;
    let local = provider == super::providers::LOCAL_PROVIDER;
    let op = store.tx(|tx| {
        tx.execute("UPDATE messages SET seen = 1 WHERE folder_id = ?", [folder_id])?;
        tx.execute("UPDATE folders SET unseen = 0 WHERE id = ?", [folder_id])?;
        // 整夹的回滚信息可能很大且价值不高：被拒时只提示，下次同步按服务器纠正
        Ok(if local { None } else { Some(pending::enqueue(tx, acc, &Task::AllSeen { path: path.clone() }, &Undo::default())?) })
    })?;
    eng.emit(Evt::Folder { account_id: acc, folder_id, added: 0 });
    submit(&eng, acc, op.into_iter().collect(), "全部标为已读").await
}

struct Dest {
    id: i64,
    account_id: i64,
    path: String,
    sync_enabled: bool,
}

fn dest(store: &Store, id: i64) -> Result<Dest> {
    store
        .r()
        .query_row("SELECT id, account_id, path, sync_enabled FROM folders WHERE id = ?", [id], |r| {
            Ok(Dest { id: r.get(0)?, account_id: r.get(1)?, path: r.get(2)?, sync_enabled: r.get::<_, i64>(3)? != 0 })
        })
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("目标文件夹不存在"))
}

/// 返回移动的封数
pub async fn move_messages(eng: Engine, ids: Vec<i64>, dest_id: i64) -> Result<usize> {
    let store = eng.store().clone();
    let d = dest(&store, dest_id)?;
    let gs = groups(&store, &ids)?;
    if gs.iter().any(|g| g.account_id != d.account_id) {
        bail!("不能跨账号移动邮件（IMAP 不支持），请用「转发」");
    }
    let mut moved = 0;
    let mut op_ids = Vec::new();
    for g in gs.iter().filter(|g| g.folder_id != d.id) {
        // 目标夹同步时会按 Message-ID 认领占位；目标夹不同步就直接删本地行，免得留下孤儿
        let drop_rows = !g.local && !d.sync_enabled;
        let (n, op) = store.tx(|tx| {
            let mut undo = Undo::default();
            let n = if drop_rows {
                messages::delete_rows_in(tx, &g.id_accounts())?;
                undo.refetch_folder = g.folder_id;
                undo.refetch_uids = g.uids();
                g.rows.len()
            } else {
                undo.moved = g.remote_rows().map(|r| (r.id, g.folder_id, r.uid)).collect();
                messages::move_local_in(tx, &g.ids(), g.folder_id, d.id, !g.local)?.0
            };
            let op = if g.needs_remote() {
                let items = g.remote_rows().map(|r| (r.uid as u32, r.message_id.clone())).collect();
                Some(pending::enqueue(tx, g.account_id, &Task::Move { path: g.path.clone(), items, dest: d.path.clone() }, &undo)?)
            } else {
                None
            };
            Ok((n, op))
        })?;
        if drop_rows {
            messages::remove_files(&store, &g.id_accounts());
        }
        moved += n;
        op_ids.extend(op);
        folder_changed(&eng, g);
    }
    eng.emit(Evt::Folder { account_id: d.account_id, folder_id: d.id, added: 0 });
    let any_remote = !op_ids.is_empty();
    submit(&eng, d.account_id, op_ids, "移动").await?;
    if any_remote && d.sync_enabled {
        eng.request_sync(d.account_id, Some(d.id), false);
    }
    Ok(moved)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DeleteResult {
    pub trashed: usize,
    pub purged: usize,
}

/// 删除：不在垃圾箱 → 移入垃圾箱；已在垃圾箱 / 垃圾邮件（或服务商没有垃圾箱）→ 彻底删除
pub async fn delete_messages(eng: Engine, ids: Vec<i64>) -> Result<DeleteResult> {
    let store = eng.store().clone();
    let mut res = DeleteResult::default();
    for g in groups(&store, &ids)? {
        let trash: Option<i64> =
            store.r().query_row("SELECT id FROM folders WHERE account_id = ? AND role = 'trash'", [g.account_id], |r| r.get(0)).optional()?;
        if let Some(t) = trash.filter(|_| g.role != "trash" && g.role != "junk") {
            res.trashed += move_messages(eng.clone(), g.ids(), t).await?;
            continue;
        }
        let rows = g.id_accounts();
        let unseen = g.rows.iter().filter(|r| !r.seen).count();
        let op = store.tx(|tx| {
            messages::delete_rows_in(tx, &rows)?;
            tx.execute("UPDATE folders SET total = MAX(0, total - ?), unseen = MAX(0, unseen - ?) WHERE id = ?", params![rows.len(), unseen, g.folder_id])?;
            if !g.needs_remote() {
                return Ok(None);
            }
            // 本地行已经删掉；服务器拒绝时从服务器把这些信封重新拉回来
            let undo = Undo { refetch_folder: g.folder_id, refetch_uids: g.uids(), ..Default::default() };
            Ok(Some(pending::enqueue(tx, g.account_id, &Task::Delete { path: g.path.clone(), uids: g.uids() }, &undo)?))
        })?;
        messages::remove_files(&store, &rows);
        res.purged += rows.len();
        folder_changed(&eng, &g);
        submit(&eng, g.account_id, op.into_iter().collect(), "删除").await?;
    }
    Ok(res)
}

/// 「标为垃圾邮件」/「这不是垃圾邮件」/「归档」都是移动的特例
pub async fn move_to_role(eng: Engine, ids: Vec<i64>, role: &str) -> Result<usize> {
    let store = eng.store().clone();
    let gs = groups(&store, &ids)?;
    let g = &gs[0];
    if g.local {
        bail!("本地归档没有「垃圾邮件 / 归档」这类系统文件夹，请用「移动到…」");
    }
    let d: Option<i64> =
        store.r().query_row("SELECT id FROM folders WHERE account_id = ? AND role = ?", params![g.account_id, role], |r| r.get(0)).optional()?;
    let Some(d) = d else { bail!("这个账号没有对应的文件夹") };
    move_messages(eng, ids, d).await
}

// ---------------- 文件夹管理（只对本地归档开放；邮箱文件夹在服务器上管理） ----------------

fn is_local_account(store: &Store, acc_id: i64) -> Result<bool> {
    let p: Option<String> = store.r().query_row("SELECT provider FROM accounts WHERE id = ?", [acc_id], |r| r.get(0)).optional()?;
    match p {
        Some(p) => Ok(p == super::providers::LOCAL_PROVIDER),
        None => bail!("账号不存在"),
    }
}

pub fn create_folder(store: &Store, acc_id: i64, name: &str, parent: Option<i64>) -> Result<i64> {
    let clean = name.trim();
    if clean.is_empty() {
        bail!("文件夹名称不能为空");
    }
    if clean.contains('/') {
        bail!("文件夹名称不能包含 /");
    }
    if !is_local_account(store, acc_id)? {
        bail!("邮箱文件夹只读：请在邮箱客户端或网页端新建文件夹");
    }
    let full = match parent {
        Some(pid) => {
            let (p, d): (String, String) = store.r().query_row("SELECT path, delimiter FROM folders WHERE id = ?", [pid], |r| Ok((r.get(0)?, r.get(1)?)))?;
            format!("{p}{d}{clean}")
        }
        None => clean.to_string(),
    };
    let exists: Option<i64> =
        store.r().query_row("SELECT id FROM folders WHERE account_id = ? AND path = ?", params![acc_id, full], |r| r.get(0)).optional()?;
    if exists.is_some() {
        bail!("同名文件夹已存在");
    }
    let c = store.w();
    c.execute("INSERT INTO folders (account_id, path, name, delimiter, role, sync_enabled) VALUES (?,?,?,'/','',1)", params![acc_id, full, clean])?;
    Ok(c.last_insert_rowid())
}

pub fn rename_folder(store: &Store, folder_id: i64, name: &str) -> Result<()> {
    let clean = name.trim();
    if clean.is_empty() || clean.contains('/') {
        bail!("文件夹名称不能为空，也不能包含 /");
    }
    let (acc, path, delim, role): (i64, String, String, String) = store
        .r()
        .query_row("SELECT account_id, path, delimiter, role FROM folders WHERE id = ?", [folder_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("文件夹不存在"))?;
    if !role.is_empty() {
        bail!("系统文件夹不能改名");
    }
    if !is_local_account(store, acc)? {
        bail!("邮箱文件夹只读：请在邮箱客户端或网页端改名");
    }
    let mut parts: Vec<&str> = path.split(delim.as_str()).collect();
    if let Some(l) = parts.last_mut() {
        *l = clean;
    }
    let next = parts.join(&delim);
    store.tx(|tx| {
        tx.execute("UPDATE folders SET path = ?, name = ? WHERE id = ?", params![next, clean, folder_id])?;
        // 子文件夹的 path 前缀一起改，否则层级会断
        let prefix = format!("{path}{delim}");
        let children: Vec<(i64, String)> = {
            let mut st = tx.prepare("SELECT id, path FROM folders WHERE account_id = ? AND substr(path, 1, ?) = ?")?;
            let v = st.query_map(params![acc, prefix.chars().count() as i64, prefix], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
            v
        };
        for (cid, cp) in children {
            tx.execute("UPDATE folders SET path = ? WHERE id = ?", params![format!("{next}{}", &cp[path.len()..]), cid])?;
        }
        Ok(())
    })
}

pub fn delete_folder(store: &Store, folder_id: i64) -> Result<()> {
    let (acc, role, path, delim): (i64, String, String, String) = store
        .r()
        .query_row("SELECT account_id, role, path, delimiter FROM folders WHERE id = ?", [folder_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("文件夹不存在"))?;
    if !role.is_empty() {
        bail!("系统文件夹不能删除");
    }
    if !is_local_account(store, acc)? {
        bail!("邮箱文件夹只读：请在邮箱客户端或网页端删除");
    }
    let prefix = format!("{path}{delim}");
    let targets: Vec<i64> = {
        let c = store.r();
        let mut st = c.prepare("SELECT id FROM folders WHERE account_id = ? AND (id = ? OR substr(path, 1, ?) = ?)")?;
        let v = st.query_map(params![acc, folder_id, prefix.chars().count() as i64, prefix], |r| r.get(0))?.collect::<Result<Vec<i64>, _>>()?;
        v
    };
    for fid in targets {
        let rows = messages::ids_with_account(store, "SELECT id, account_id FROM messages WHERE folder_id = ?", [fid])?;
        messages::remove(store, &rows)?;
        store.w().execute("DELETE FROM folders WHERE id = ?", [fid])?;
    }
    Ok(())
}

pub fn set_folder_sync(store: &Store, folder_id: i64, enabled: bool) -> Result<()> {
    store.w().execute("UPDATE folders SET sync_enabled = ? WHERE id = ?", params![enabled as i64, folder_id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::accounts;

    #[test]
    fn local_folder_management() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let local = accounts::ensure_local(&s).unwrap();
        let a = create_folder(&s, local.id, "工作", None).unwrap();
        let b = create_folder(&s, local.id, "2024", Some(a)).unwrap();
        assert!(create_folder(&s, local.id, "工作", None).is_err());
        rename_folder(&s, a, "项目").unwrap();
        let p: String = s.r().query_row("SELECT path FROM folders WHERE id = ?", [b], |r| r.get(0)).unwrap();
        assert_eq!(p, "项目/2024");
        s.w().execute("INSERT INTO messages(account_id,folder_id,uid,date) VALUES(?,?,1,0)", params![local.id, b]).unwrap();
        delete_folder(&s, a).unwrap();
        let n: i64 = s.r().query_row("SELECT count(*) FROM folders", [], |r| r.get(0)).unwrap();
        let m: i64 = s.r().query_row("SELECT count(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!((n, m), (0, 0), "连同子文件夹与邮件一起删");
        // 真实账号的文件夹只读
        s.w().execute("INSERT INTO accounts(id,email,provider,imap_host,smtp_host,secret,created_at) VALUES(9,'x@y.z','custom','h','h','x',0)", []).unwrap();
        assert!(create_folder(&s, 9, "x", None).is_err());
    }
}
