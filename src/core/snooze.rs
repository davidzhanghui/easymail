//! 稍后处理（Snooze）：选一个时间，邮件先从列表里消失，到点后标为未读重新出现在原来的位置。
//!
//! 纯本地功能：不改服务器上的文件夹（各家对 Snooze 没有统一的 IMAP 表达），所以在别的客户端里这封信照常可见。
//! 到点时把本地行标为未读，并送一条「去掉 \Seen」的待办，让服务器上也变回未读、手机上也能看到提醒。

use super::db::{now_ms, Store};
use super::pending::{self, Task, Undo};
use anyhow::Result;
use chrono::{Datelike, Duration, Local, TimeZone, Timelike, Weekday};
use rusqlite::params;

pub fn snooze(store: &Store, ids: &[i64], until: i64) -> Result<usize> {
    store.tx(|tx| {
        let mut n = 0;
        for id in ids {
            n += tx.execute("INSERT OR REPLACE INTO snoozed (message_id, until) VALUES (?, ?)", params![id, until])?;
        }
        Ok(n)
    })
}

pub fn unsnooze(store: &Store, ids: &[i64]) -> Result<()> {
    store.tx(|tx| {
        for id in ids {
            tx.execute("DELETE FROM snoozed WHERE message_id = ?", [id])?;
        }
        Ok(())
    })
}

/// 到点的：移出稍后处理、标为未读（服务器上也去掉 \Seen）。返回唤醒的封数
pub fn wake_due(store: &Store) -> Result<usize> {
    let due: Vec<(i64, i64, i64, i64, String, bool, bool)> = {
        let c = store.r();
        let mut st = c.prepare(
            "SELECT m.id, m.account_id, m.folder_id, m.uid, f.path, m.seen, a.provider = 'local' FROM snoozed s
             JOIN messages m ON m.id = s.message_id JOIN folders f ON f.id = m.folder_id JOIN accounts a ON a.id = m.account_id WHERE s.until <= ?",
        )?;
        let v = st
            .query_map([now_ms()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get::<_, i64>(5)? != 0, r.get::<_, i64>(6)? != 0)))?
            .collect::<Result<Vec<_>, _>>()?;
        v
    };
    if due.is_empty() {
        // 行已被删掉的残留（外键级联一般会清，这里兜底）
        store.w().execute("DELETE FROM snoozed WHERE until <= ? AND message_id NOT IN (SELECT id FROM messages)", [now_ms()])?;
        return Ok(0);
    }
    store.tx(|tx| {
        for (id, acc, fid, uid, path, seen, local) in &due {
            tx.execute("DELETE FROM snoozed WHERE message_id = ?", [id])?;
            if *seen {
                tx.execute("UPDATE messages SET seen = 0 WHERE id = ?", [id])?;
                tx.execute("UPDATE folders SET unseen = unseen + 1 WHERE id = ?", [fid])?;
                if !local && *uid > 0 {
                    let undo = Undo { column: "seen".into(), flags: vec![(*id, true)], ..Default::default() };
                    pending::enqueue(tx, *acc, &Task::Flag { path: path.clone(), uids: vec![*uid as u32], flag: "\\Seen".into(), add: false }, &undo)?;
                }
            }
        }
        Ok(())
    })?;
    Ok(due.len())
}

/// 常用的稍后处理时间：(标签, 时间戳)
pub fn presets() -> Vec<(&'static str, i64)> {
    let now = Local::now();
    let at =
        |d: chrono::DateTime<Local>, h: u32| Local.with_ymd_and_hms(d.year(), d.month(), d.day(), h, 0, 0).single().map(|t| t.timestamp_millis()).unwrap_or(0);
    let mut v = vec![("3 小时后", (now + Duration::hours(3)).timestamp_millis())];
    if now.hour() < 17 {
        v.push(("今晚 20:00", at(now, 20)));
    }
    v.push(("明天 08:00", at(now + Duration::days(1), 8)));
    let days_to_sat = (Weekday::Sat.num_days_from_monday() as i64 - now.weekday().num_days_from_monday() as i64).rem_euclid(7);
    if days_to_sat > 1 {
        v.push(("周六 09:00", at(now + Duration::days(days_to_sat), 9)));
    }
    let days_to_mon = (7 - now.weekday().num_days_from_monday() as i64) % 7;
    v.push(("下周一 08:00", at(now + Duration::days(if days_to_mon == 0 { 7 } else { days_to_mon }), 8)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snooze_and_wake() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        crate::core::queries::tests::seed(&s);
        snooze(&s, &[2], now_ms() - 1).unwrap();
        snooze(&s, &[1], now_ms() + 3_600_000).unwrap();
        assert_eq!(wake_due(&s).unwrap(), 1);
        let (seen, n): (i64, i64) =
            s.r().query_row("SELECT seen, (SELECT COUNT(*) FROM snoozed) FROM messages WHERE id = 2", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((seen, n), (0, 1), "到点的标为未读并移出");
        assert_eq!(pending::count(&s, 1), 1, "服务器上也去掉 \\\\Seen");
        unsnooze(&s, &[1]).unwrap();
        assert!(presets().iter().all(|(_, t)| *t > now_ms()));
    }
}
