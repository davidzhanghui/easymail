//! 发件箱：点「发送」后信先落库，再由引擎在到点时发出。
//!
//! - 撤回发送：默认 5 秒的窗口（设置里可改，0 = 立即发送），窗口内可以撤回，信回到草稿
//! - 定时发送：send_at 设成将来的时间
//! - 离线发送：连接类错误不算失败，按 1 / 2 / 5 / 15 / 30 分钟退避重试，联网后自动发出
//! - 服务器明确拒绝（认证失败、收件人被拒等）：标为 failed，提示用户，信留在发件箱里可以重试或撤回编辑
//!
//! 状态：queued（等待）→ sending（发送中）→ 成功即删除；失败为 failed。进程在 sending 时退出，
//! 下次启动按 queued 处理（SMTP 没有幂等性，极端情况下可能重复发送一次，比丢信好）。

use super::db::{now_ms, Store};
use super::engine::Engine;
use super::error::{kind_of, ErrorKind};
use super::mailer::{self, Outgoing};
use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension};

const RETRY_MIN: [i64; 5] = [1, 2, 5, 15, 30];

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: i64,
    pub account_id: i64,
    pub draft_id: Option<i64>,
    pub subject: String,
    pub rcpt: String,
    pub send_at: i64,
    pub state: String,
    pub attempts: i64,
    pub last_error: String,
}

/// 撤回窗口（秒），设置项 `undo_send_secs`
pub fn undo_secs(store: &Store) -> i64 {
    store.get_kv("undo_send_secs").and_then(|v| v.parse().ok()).unwrap_or(5).clamp(0, 60)
}

/// 放进发件箱，返回 id。`send_at` 为 None 时 = 现在 + 撤回窗口
pub fn enqueue(store: &Store, o: &Outgoing, send_at: Option<i64>) -> Result<i64> {
    if o.to.trim().is_empty() && o.cc.trim().is_empty() && o.bcc.trim().is_empty() {
        bail!("请填写收件人");
    }
    let at = send_at.unwrap_or_else(|| now_ms() + undo_secs(store) * 1000);
    let rcpt = [o.to.as_str(), o.cc.as_str(), o.bcc.as_str()].iter().filter(|s| !s.trim().is_empty()).cloned().collect::<Vec<_>>().join(", ");
    let c = store.w();
    c.execute(
        "INSERT INTO outbox (account_id, draft_id, payload, subject, rcpt, send_at, state, created_at) VALUES (?,?,?,?,?,?,'queued',?)",
        params![o.account_id, o.draft_id, serde_json::to_string(o)?, o.subject, rcpt, at, now_ms()],
    )?;
    Ok(c.last_insert_rowid())
}

pub fn list(store: &Store) -> Result<Vec<Item>> {
    let c = store.r();
    let mut st = c.prepare("SELECT id, account_id, draft_id, subject, rcpt, send_at, state, attempts, last_error FROM outbox ORDER BY send_at")?;
    let v = st
        .query_map([], |r| {
            Ok(Item {
                id: r.get(0)?,
                account_id: r.get(1)?,
                draft_id: r.get(2)?,
                subject: r.get(3)?,
                rcpt: r.get(4)?,
                send_at: r.get(5)?,
                state: r.get(6)?,
                attempts: r.get(7)?,
                last_error: r.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// 撤回：还没开始发送的才能撤回；返回原来的草稿 id（信回到草稿箱）
pub fn cancel(store: &Store, id: i64) -> Result<Option<i64>> {
    let c = store.w();
    let row: Option<(String, Option<i64>)> = c.query_row("SELECT state, draft_id FROM outbox WHERE id = ?", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    match row {
        None => bail!("这封信已经发出"),
        Some((st, _)) if st == "sending" => bail!("正在发送，来不及撤回了"),
        Some((_, draft)) => {
            c.execute("DELETE FROM outbox WHERE id = ?", [id])?;
            Ok(draft)
        }
    }
}

/// 失败的信重新排队（立即发送）
pub fn retry(store: &Store, id: i64) -> Result<()> {
    store.w().execute("UPDATE outbox SET state = 'queued', send_at = ?, last_error = '' WHERE id = ? AND state != 'sending'", params![now_ms(), id])?;
    Ok(())
}

/// 进程上次在 sending 时退出：恢复成 queued
pub fn recover(store: &Store) {
    let _ = store.w().execute("UPDATE outbox SET state = 'queued' WHERE state = 'sending'", []);
}

/// 取一封到点的信并标记为 sending（原子操作，避免并发重复发送）
fn claim(store: &Store) -> Result<Option<(i64, Outgoing, i64)>> {
    let c = store.w();
    let row: Option<(i64, String, i64)> = c
        .query_row("SELECT id, payload, attempts FROM outbox WHERE state = 'queued' AND send_at <= ? ORDER BY send_at LIMIT 1", [now_ms()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    let Some((id, payload, attempts)) = row else { return Ok(None) };
    c.execute("UPDATE outbox SET state = 'sending', attempts = attempts + 1 WHERE id = ?", [id])?;
    match serde_json::from_str::<Outgoing>(&payload) {
        Ok(o) => Ok(Some((id, o, attempts))),
        Err(e) => {
            c.execute("UPDATE outbox SET state = 'failed', last_error = ? WHERE id = ?", params![format!("内容损坏：{e}"), id])?;
            Ok(None)
        }
    }
}

/// 引擎定时调用：把到点的信都发出去。返回发出的封数
pub async fn tick(eng: &Engine) -> usize {
    let store = eng.store().clone();
    let mut sent = 0;
    loop {
        let (id, o, attempts) = match claim(&store) {
            Ok(Some(x)) => x,
            Ok(None) => break,
            Err(e) => {
                log::warn!("[outbox] 读取发件箱失败：{e:#}");
                break;
            }
        };
        let subject = if o.subject.trim().is_empty() { "（无主题）".to_string() } else { o.subject.clone() };
        match mailer::send(eng.clone(), o).await {
            Ok(r) => {
                let _ = store.w().execute("DELETE FROM outbox WHERE id = ?", [id]);
                sent += 1;
                eng.toast(if r.saved_to_sent { format!("已发送「{subject}」· 已存入「已发送」") } else { format!("已发送「{subject}」") }, false);
            }
            Err(e) if kind_of(&e) == ErrorKind::Connection => {
                let wait = RETRY_MIN[(attempts as usize).min(RETRY_MIN.len() - 1)] * 60_000;
                let _ = store
                    .w()
                    .execute("UPDATE outbox SET state = 'queued', send_at = ?, last_error = ? WHERE id = ?", params![now_ms() + wait, format!("{e:#}"), id]);
                if attempts == 0 {
                    eng.toast(format!("「{subject}」暂时发不出去，已留在发件箱，联网后自动重试（{e:#}）"), true);
                }
                // 连不上时后面的大概率也连不上，这一轮先停
                break;
            }
            Err(e) => {
                let _ = store.w().execute("UPDATE outbox SET state = 'failed', last_error = ? WHERE id = ?", params![format!("{e:#}"), id]);
                eng.toast(format!("「{subject}」发送失败，已留在发件箱：{e:#}"), true);
            }
        }
        eng.emit(super::engine::Evt::Accounts);
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_cancel_claim() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.w().execute("INSERT INTO accounts(id,email,provider,imap_host,smtp_host,secret,created_at) VALUES(1,'a@b.c','custom','h','h','x',0)", []).unwrap();
        let o = Outgoing { account_id: 1, to: "x@y.z".into(), subject: "hi".into(), draft_id: Some(9), ..Default::default() };
        assert!(enqueue(&s, &Outgoing { account_id: 1, ..Default::default() }, None).is_err(), "没有收件人");
        let id = enqueue(&s, &o, None).unwrap();
        assert!(claim(&s).unwrap().is_none(), "撤回窗口内不发");
        assert_eq!(cancel(&s, id).unwrap(), Some(9));
        assert!(list(&s).unwrap().is_empty());
        let id = enqueue(&s, &o, Some(0)).unwrap();
        let (cid, back, attempts) = claim(&s).unwrap().unwrap();
        assert_eq!((cid, back.subject.as_str(), attempts), (id, "hi", 0));
        assert!(cancel(&s, id).is_err(), "发送中不能撤回");
        recover(&s);
        assert_eq!(list(&s).unwrap()[0].state, "queued");
        assert_eq!(list(&s).unwrap()[0].attempts, 1);
    }
}
