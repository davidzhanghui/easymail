//! 发件箱：点「发送」后信先落库，再由引擎在到点时发出。
//!
//! - 撤回发送：默认 5 秒的窗口（设置里可改，0 = 立即发送），窗口内可以撤回，信回到草稿
//! - 定时发送：send_at 设成将来的时间
//! - 离线发送：连接类错误不算失败，按 1 / 2 / 5 / 15 / 30 分钟退避重试，联网后自动发出
//! - 服务器明确拒绝（认证失败、收件人被拒等）：标为 failed，提示用户，信留在发件箱里可以重试或撤回编辑
//!
//! 状态：queued（等待）→ sending（发送中）→ accepted（服务器已收下，正在存「已发送」等收尾）→ 删除；
//! 失败为 failed。
//!
//! 防重发（SMTP 没有幂等性）：
//! - 每封信入队时就固定 Message-ID，重试沿用同一个
//! - 服务器一回 250 就把状态记成 accepted；之后进程被杀，重启时直接当作已发出，不会再发
//! - 进程在 sending 时退出、或正文写完后等回应时断线（`smtp::MaybeDelivered`）：记 uncertain，
//!   下次发之前先去服务器「已发送」里按 Message-ID 查，查到了就当作已发出
//! - 仍然查不到（服务商不自动存「已发送」、而上次又恰好死在 250 之前的一瞬间）才会重发，比丢信好

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
    let mut o = o.clone();
    if o.message_id.trim().is_empty() {
        let email: String = store.r().query_row("SELECT email FROM accounts WHERE id = ?", [o.account_id], |r| r.get(0)).optional()?.unwrap_or_default();
        o.message_id = mailer::new_message_id(&email);
    }
    let o = &o;
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
        Some((st, _)) if st == "accepted" => bail!("这封信已经发出"),
        Some((_, draft)) => {
            c.execute("DELETE FROM outbox WHERE id = ?", [id])?;
            Ok(draft)
        }
    }
}

/// 失败的信重新排队（立即发送）
pub fn retry(store: &Store, id: i64) -> Result<()> {
    store
        .w()
        .execute("UPDATE outbox SET state = 'queued', send_at = ?, last_error = '' WHERE id = ? AND state IN ('queued', 'failed')", params![now_ms(), id])?;
    Ok(())
}

/// 启动时整理上次没走完的信，返回「其实已经发出」的封数：
/// - accepted：服务器已经收下，只是收尾没做完 → 直接删掉，不再发
/// - sending：不知道服务器收没收到 → 回到 queued 并记 uncertain，发之前先核对「已发送」
pub fn recover(store: &Store) -> usize {
    let c = store.w();
    let done = c.execute("DELETE FROM outbox WHERE state = 'accepted'", []).unwrap_or(0);
    if done > 0 {
        log::info!("[outbox] 上次退出时有 {done} 封已被服务器收下、收尾未完成，按已发出处理");
    }
    let _ = c.execute("UPDATE outbox SET state = 'queued', uncertain = 1 WHERE state = 'sending'", []);
    done
}

/// 取一封到点的信并标记为 sending（原子操作，避免并发重复发送）
struct Claimed {
    id: i64,
    o: Outgoing,
    attempts: i64,
    uncertain: bool,
}

fn claim(store: &Store) -> Result<Option<Claimed>> {
    let c = store.w();
    let row: Option<(i64, String, i64, bool)> = c
        .query_row("SELECT id, payload, attempts, uncertain FROM outbox WHERE state = 'queued' AND send_at <= ? ORDER BY send_at LIMIT 1", [now_ms()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    let Some((id, payload, attempts, uncertain)) = row else { return Ok(None) };
    c.execute("UPDATE outbox SET state = 'sending', attempts = attempts + 1 WHERE id = ?", [id])?;
    match serde_json::from_str::<Outgoing>(&payload) {
        Ok(mut o) => {
            // 旧版本入队的信没有固定 Message-ID：补一个并写回，之后的重试都沿用
            if o.message_id.trim().is_empty() {
                let email: String = c.query_row("SELECT email FROM accounts WHERE id = ?", [o.account_id], |r| r.get(0)).optional()?.unwrap_or_default();
                o.message_id = mailer::new_message_id(&email);
                c.execute("UPDATE outbox SET payload = ? WHERE id = ?", params![serde_json::to_string(&o)?, id])?;
            }
            Ok(Some(Claimed { id, o, attempts, uncertain }))
        }
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
        let Claimed { id, o, attempts, uncertain } = match claim(&store) {
            Ok(Some(x)) => x,
            Ok(None) => break,
            Err(e) => {
                log::warn!("[outbox] 读取发件箱失败：{e:#}");
                break;
            }
        };
        let subject = if o.subject.trim().is_empty() { "（无主题）".to_string() } else { o.subject.clone() };
        if uncertain {
            match already_sent(eng, &o).await {
                Ok(true) => {
                    log::info!("[outbox] 「{subject}」在「已发送」里找到了 {}，不再重发", o.message_id);
                    let _ = store.w().execute("DELETE FROM outbox WHERE id = ?", [id]);
                    if let Some(d) = o.draft_id {
                        let _ = mailer::delete_draft_everywhere(eng, d, true);
                    }
                    eng.toast(format!("「{subject}」上次其实已经发出，没有重复发送"), false);
                    eng.emit(super::engine::Evt::Accounts);
                    continue;
                }
                Ok(false) => {}
                Err(e) => {
                    // 核对不了（连不上）就先别发，免得重复；按连接错误退避
                    let wait = RETRY_MIN[(attempts as usize).min(RETRY_MIN.len() - 1)] * 60_000;
                    let _ = store.w().execute(
                        "UPDATE outbox SET state = 'queued', send_at = ?, last_error = ? WHERE id = ?",
                        params![now_ms() + wait, format!("核对是否已发出时失败：{e:#}"), id],
                    );
                    break;
                }
            }
        }
        let st = store.clone();
        let accepted = move || {
            let _ = st.w().execute("UPDATE outbox SET state = 'accepted' WHERE id = ?", [id]);
        };
        match mailer::send_with(eng.clone(), o, accepted).await {
            Ok(r) => {
                let _ = store.w().execute("DELETE FROM outbox WHERE id = ?", [id]);
                sent += 1;
                eng.toast(if r.saved_to_sent { format!("已发送「{subject}」· 已存入「已发送」") } else { format!("已发送「{subject}」") }, false);
            }
            Err(e) if kind_of(&e) == ErrorKind::Connection => {
                let wait = RETRY_MIN[(attempts as usize).min(RETRY_MIN.len() - 1)] * 60_000;
                let maybe = super::smtp::maybe_delivered(&e);
                let _ = store.w().execute(
                    "UPDATE outbox SET state = 'queued', send_at = ?, last_error = ?, uncertain = MAX(uncertain, ?) WHERE id = ?",
                    params![now_ms() + wait, format!("{e:#}"), maybe, id],
                );
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

/// 「已发送」里有没有这封信（按固定的 Message-ID）。账号没有「已发送」文件夹时无从核对，返回 false
async fn already_sent(eng: &Engine, o: &Outgoing) -> Result<bool> {
    let mid = o.message_id.trim();
    if mid.is_empty() {
        return Ok(false);
    }
    let path: Option<String> =
        eng.store().r().query_row("SELECT path FROM folders WHERE account_id = ? AND role = 'sent'", [o.account_id], |r| r.get(0)).optional()?;
    let Some(path) = path else { return Ok(false) };
    let mid = if mid.starts_with('<') { mid.to_string() } else { format!("<{mid}>") };
    eng.has_message(o.account_id, path, mid).await
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
        let c = claim(&s).unwrap().unwrap();
        assert_eq!((c.id, c.o.subject.as_str(), c.attempts, c.uncertain), (id, "hi", 0, false));
        assert!(cancel(&s, id).is_err(), "发送中不能撤回");
        recover(&s);
        let it = &list(&s).unwrap()[0];
        assert_eq!((it.state.as_str(), it.attempts), ("queued", 1));
        let unc: bool = s.r().query_row("SELECT uncertain FROM outbox WHERE id = ?", [id], |r| r.get(0)).unwrap();
        assert!(unc, "死在「发送中」的重发前要先核对是否已发出");
    }

    #[test]
    fn enqueue_fixes_message_id() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.w().execute("INSERT INTO accounts(id,email,provider,imap_host,smtp_host,secret,created_at) VALUES(1,'a@b.c','custom','h','h','x',0)", []).unwrap();
        let id = enqueue(&s, &Outgoing { account_id: 1, to: "x@y.z".into(), subject: "hi".into(), ..Default::default() }, Some(0)).unwrap();
        let c = claim(&s).unwrap().unwrap();
        assert_eq!(c.id, id);
        assert!(c.o.message_id.ends_with("@b.c"), "入队时按发件域名生成固定的 Message-ID：{}", c.o.message_id);
        // 重试沿用同一个
        recover(&s);
        let c2 = claim(&s).unwrap().unwrap();
        assert_eq!(c2.o.message_id, c.o.message_id);
    }

    #[test]
    fn accepted_is_not_resent() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.w().execute("INSERT INTO accounts(id,email,provider,imap_host,smtp_host,secret,created_at) VALUES(1,'a@b.c','custom','h','h','x',0)", []).unwrap();
        let id = enqueue(&s, &Outgoing { account_id: 1, to: "x@y.z".into(), subject: "hi".into(), ..Default::default() }, Some(0)).unwrap();
        let _ = claim(&s).unwrap().unwrap();
        // 服务器回了 250、状态记成 accepted，随后进程被杀
        s.w().execute("UPDATE outbox SET state = 'accepted' WHERE id = ?", [id]).unwrap();
        assert_eq!(recover(&s), 1);
        assert!(list(&s).unwrap().is_empty(), "已收下的信重启后不能重发");
        assert!(claim(&s).unwrap().is_none());
    }
}
