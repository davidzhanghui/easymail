//! 持久化的远端操作队列（`pending_ops` 表）。
//!
//! 以前的模型是「本地先改 → 远端执行 → 失败就指望重新同步来纠正」，但增量同步只拉 `UID > last_uid` 的新信，
//! 纠正不了删除和移动：远端一失败，本地这封信就永久消失了。现在改成：
//!
//! ```text
//!   用户操作 ──一个事务──▶ ① 改本地行（界面立刻生效）  ② 写入 pending_ops（含回滚信息 undo）
//!   账号 worker 按 id 顺序重放：
//!     成功              → 删掉这条待办
//!     连接类错误        → 留着，下次同步 / 联网后再试（离线期间一直留着）
//!     服务器明确拒绝    → 按 undo 回滚本地改动，删掉待办，提示用户
//! ```
//!
//! 同步时要避开仍有待办的行（[`busy_ids`]）：不拿服务器上的旧标记覆盖用户刚做的改动，也不把还没送达的移动占位当成孤儿清掉。

use super::db::{now_ms, Store};
use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// 要送到服务器的操作
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Task {
    Flag {
        path: String,
        uids: Vec<u32>,
        flag: String,
        add: bool,
    },
    AllSeen {
        path: String,
    },
    /// `items`：(源 UID, Message-ID)。Message-ID 用于没有 MOVE 能力时判断「上次是不是已经 COPY 过去了」
    Move {
        path: String,
        items: Vec<(u32, String)>,
        dest: String,
    },
    Delete {
        path: String,
        uids: Vec<u32>,
    },
}

impl Task {
    pub fn kind(&self) -> &'static str {
        match self {
            Task::Flag { .. } => "flag",
            Task::AllSeen { .. } => "all_seen",
            Task::Move { .. } => "move",
            Task::Delete { .. } => "delete",
        }
    }

    /// 给用户看的名字
    pub fn label(&self) -> &'static str {
        match self {
            Task::Flag { flag, .. } if flag == "\\Seen" => "已读状态",
            Task::Flag { flag, .. } if flag == "\\Flagged" => "星标",
            Task::Flag { .. } => "标记",
            Task::AllSeen { .. } => "全部标为已读",
            Task::Move { .. } => "移动",
            Task::Delete { .. } => "删除",
        }
    }
}

/// 服务器拒绝时回滚本地改动所需的信息
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Undo {
    /// 标记：改的是哪一列（seen / flagged / answered）
    #[serde(default)]
    pub column: String,
    /// 标记：(消息 id, 改之前的值)
    #[serde(default)]
    pub flags: Vec<(i64, bool)>,
    /// 移动：(消息 id, 原文件夹 id, 原 UID)
    #[serde(default)]
    pub moved: Vec<(i64, i64, i64)>,
    /// 本地行已经删掉了：回滚时从服务器重新拉这个文件夹里这些 UID 的信封
    #[serde(default)]
    pub refetch_folder: i64,
    #[serde(default)]
    pub refetch_uids: Vec<u32>,
}

impl Undo {
    /// 涉及的本地消息 id
    pub fn message_ids(&self) -> impl Iterator<Item = i64> + '_ {
        self.flags.iter().map(|f| f.0).chain(self.moved.iter().map(|m| m.0))
    }
}

#[derive(Clone, Debug)]
pub struct Pending {
    pub id: i64,
    pub account_id: i64,
    pub task: Task,
    pub undo: Undo,
    pub attempts: i64,
    pub created_at: i64,
}

/// 一条待办的执行结果
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Done,
    /// 连接问题，留在队列里稍后重试
    Deferred(String),
    /// 服务器明确拒绝，本地已回滚
    Rejected(String),
}

/// 写入一条待办；必须和对应的本地改动在**同一个事务**里调用
pub fn enqueue(c: &Connection, account_id: i64, task: &Task, undo: &Undo) -> Result<i64> {
    c.execute(
        "INSERT INTO pending_ops (account_id, kind, payload, undo, created_at) VALUES (?,?,?,?,?)",
        params![account_id, task.kind(), serde_json::to_string(task)?, serde_json::to_string(undo)?, now_ms()],
    )?;
    Ok(c.last_insert_rowid())
}

/// 某个账号的全部待办，按写入顺序。解析不了的条目（理论上不会有）记日志后丢弃，不卡住后面的。
pub fn list(store: &Store, account_id: i64) -> Result<Vec<Pending>> {
    let rows: Vec<(i64, String, String, i64, i64)> = {
        let c = store.w();
        let mut st = c.prepare("SELECT id, payload, undo, attempts, created_at FROM pending_ops WHERE account_id = ? ORDER BY id")?;
        let v = st.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<Vec<_>, _>>()?;
        v
    };
    let mut out = Vec::with_capacity(rows.len());
    for (id, payload, undo, attempts, created_at) in rows {
        match serde_json::from_str::<Task>(&payload) {
            Ok(task) => out.push(Pending { id, account_id, task, undo: serde_json::from_str(&undo).unwrap_or_default(), attempts, created_at }),
            Err(e) => {
                log::warn!("[pending] 丢弃无法解析的待办 {id}：{e}");
                done(store, id)?;
            }
        }
    }
    Ok(out)
}

pub fn count(store: &Store, account_id: i64) -> i64 {
    store.r().query_row("SELECT count(*) FROM pending_ops WHERE account_id = ?", [account_id], |r| r.get(0)).unwrap_or(0)
}

/// 开始执行前先把尝试次数加一：执行到一半进程退出，下次重放就知道「上次可能已经做了一部分」
pub fn begin(store: &Store, id: i64) -> Result<()> {
    store.w().execute("UPDATE pending_ops SET attempts = attempts + 1 WHERE id = ?", [id])?;
    Ok(())
}

pub fn done(store: &Store, id: i64) -> Result<()> {
    store.w().execute("DELETE FROM pending_ops WHERE id = ?", [id])?;
    Ok(())
}

pub fn deferred(store: &Store, id: i64, error: &str) -> Result<()> {
    store.w().execute("UPDATE pending_ops SET last_error = ? WHERE id = ?", params![error, id])?;
    Ok(())
}

/// 仍有待办的本地消息 id：同步时不要覆盖它们的本地改动，也不要清掉它们的移动占位
pub fn busy_ids(c: &Connection, account_id: i64) -> Result<HashSet<i64>> {
    let mut st = c.prepare("SELECT undo FROM pending_ops WHERE account_id = ?")?;
    let mut out = HashSet::new();
    for u in st.query_map([account_id], |r| r.get::<_, String>(0))? {
        if let Ok(undo) = serde_json::from_str::<Undo>(&u?) {
            out.extend(undo.message_ids());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_busy_ids() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.w().execute("INSERT INTO accounts(id,email,provider,imap_host,smtp_host,secret,created_at) VALUES(1,'a@b.c','custom','h','h','x',0)", []).unwrap();
        let mv = Task::Move { path: "INBOX".into(), items: vec![(3, "<m@x>".into())], dest: "Trash".into() };
        let id = s.tx(|tx| enqueue(tx, 1, &mv, &Undo { moved: vec![(42, 1, 3)], ..Default::default() })).unwrap();
        s.tx(|tx| {
            enqueue(
                tx,
                1,
                &Task::Flag { path: "INBOX".into(), uids: vec![5], flag: "\\Seen".into(), add: true },
                &Undo { column: "seen".into(), flags: vec![(7, false)], ..Default::default() },
            )
        })
        .unwrap();
        let l = list(&s, 1).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].task, mv);
        assert_eq!(l[0].task.label(), "移动");
        assert_eq!(l[1].task.label(), "已读状态");
        assert_eq!(busy_ids(&s.w(), 1).unwrap(), HashSet::from([42, 7]));
        begin(&s, id).unwrap();
        assert_eq!(list(&s, 1).unwrap()[0].attempts, 1);
        done(&s, id).unwrap();
        assert_eq!(count(&s, 1), 1);
        // 删除账号时待办跟着清掉
        s.w().execute("DELETE FROM accounts WHERE id = 1", []).unwrap();
        assert_eq!(count(&s, 1), 0);
    }
}
