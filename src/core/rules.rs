//! 本地规则：新信入库后按条件自动处理。
//!
//! 条件：发件人 / 收件人 / 主题 包含某段文字（不区分大小写）；动作：移动到文件夹 / 标为已读 / 加星标 / 归档 / 删除。
//! 只对同步进来的**新信**生效（不回溯历史），只在本机执行；远端改动和手动操作一样走 `pending_ops` 队列。

use super::db::{now_ms, Store};
use super::engine::Engine;
use super::ops::{self, FlagChange};
use anyhow::{bail, Result};
use rusqlite::params;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Rule {
    pub id: i64,
    /// None = 所有账号
    pub account_id: Option<i64>,
    /// from / to / subject
    pub field: String,
    pub value: String,
    /// move / read / star / archive / delete
    pub action: String,
    /// move 的目标文件夹
    pub target: Option<i64>,
    pub enabled: bool,
    pub hits: i64,
}

pub const FIELDS: [(&str, &str); 3] = [("from", "发件人"), ("to", "收件人"), ("subject", "主题")];
pub const ACTIONS: [(&str, &str); 5] = [("move", "移动到"), ("read", "标为已读"), ("star", "加星标"), ("archive", "归档"), ("delete", "删除")];

pub fn list(store: &Store) -> Result<Vec<Rule>> {
    let c = store.r();
    let mut st = c.prepare("SELECT id, account_id, field, value, action, target, enabled, hits FROM rules ORDER BY id")?;
    let v = st
        .query_map([], |r| {
            Ok(Rule {
                id: r.get(0)?,
                account_id: r.get(1)?,
                field: r.get(2)?,
                value: r.get(3)?,
                action: r.get(4)?,
                target: r.get(5)?,
                enabled: r.get::<_, i64>(6)? != 0,
                hits: r.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

pub fn add(store: &Store, r: &Rule) -> Result<i64> {
    if r.value.trim().is_empty() {
        bail!("请填写要匹配的文字");
    }
    if !FIELDS.iter().any(|f| f.0 == r.field) || !ACTIONS.iter().any(|a| a.0 == r.action) {
        bail!("规则的条件或动作不正确");
    }
    if r.action == "move" && r.target.is_none() {
        bail!("请选择目标文件夹");
    }
    let c = store.w();
    c.execute(
        "INSERT INTO rules (account_id, field, value, action, target, enabled, created_at) VALUES (?,?,?,?,?,1,?)",
        params![r.account_id, r.field, r.value.trim(), r.action, r.target, now_ms()],
    )?;
    Ok(c.last_insert_rowid())
}

pub fn remove(store: &Store, id: i64) -> Result<()> {
    store.w().execute("DELETE FROM rules WHERE id = ?", [id])?;
    Ok(())
}

pub fn set_enabled(store: &Store, id: i64, on: bool) -> Result<()> {
    store.w().execute("UPDATE rules SET enabled = ? WHERE id = ?", params![on as i64, id])?;
    Ok(())
}

/// 规则命中的消息：(规则, 消息 id 列表)。纯查询，便于测试
pub fn matches(store: &Store, msg_ids: &[i64]) -> Result<Vec<(Rule, Vec<i64>)>> {
    let rules: Vec<Rule> = list(store)?.into_iter().filter(|r| r.enabled).collect();
    if rules.is_empty() || msg_ids.is_empty() {
        return Ok(vec![]);
    }
    let c = store.r();
    let mut st = c.prepare("SELECT account_id, from_name || ' ' || from_addr, to_json || ' ' || cc_json, subject FROM messages WHERE id = ?")?;
    let mut msgs = Vec::new();
    for id in msg_ids {
        if let Ok(m) = st.query_row([id], |r| Ok((*id, r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))) {
            msgs.push(m);
        }
    }
    let mut out: Vec<(Rule, Vec<i64>)> = Vec::new();
    let mut taken = std::collections::HashSet::new();
    for r in rules {
        let needle = r.value.to_lowercase();
        let hit: Vec<i64> = msgs
            .iter()
            .filter(|(id, acc, from, to, subj)| {
                // 移动类动作每封信只执行第一条命中的规则
                let movable = matches!(r.action.as_str(), "move" | "archive" | "delete");
                if movable && taken.contains(id) {
                    return false;
                }
                r.account_id.is_none_or(|a| a == *acc)
                    && match r.field.as_str() {
                        "from" => from.to_lowercase().contains(&needle),
                        "to" => to.to_lowercase().contains(&needle),
                        _ => subj.to_lowercase().contains(&needle),
                    }
            })
            .map(|m| m.0)
            .collect();
        if hit.is_empty() {
            continue;
        }
        if matches!(r.action.as_str(), "move" | "archive" | "delete") {
            taken.extend(hit.iter().copied());
        }
        out.push((r, hit));
    }
    Ok(out)
}

/// 对新入库的信执行规则；返回被处理的封数
pub async fn apply(eng: &Engine, msg_ids: &[i64]) -> usize {
    let store = eng.store().clone();
    let Ok(hits) = matches(&store, msg_ids) else { return 0 };
    let mut n = 0;
    for (r, ids) in hits {
        let res = match r.action.as_str() {
            "read" => ops::set_flags(eng.clone(), ids.clone(), FlagChange { seen: Some(true), flagged: None }).await,
            "star" => ops::set_flags(eng.clone(), ids.clone(), FlagChange { seen: None, flagged: Some(true) }).await,
            "archive" => ops::move_to_role(eng.clone(), ids.clone(), "archive").await.map(|_| ()),
            "delete" => ops::delete_messages(eng.clone(), ids.clone()).await.map(|_| ()),
            "move" => match r.target {
                Some(t) => ops::move_messages(eng.clone(), ids.clone(), t).await.map(|_| ()),
                None => Ok(()),
            },
            _ => Ok(()),
        };
        match res {
            Ok(()) => {
                n += ids.len();
                let _ = store.w().execute("UPDATE rules SET hits = hits + ? WHERE id = ?", params![ids.len() as i64, r.id]);
            }
            Err(e) => log::warn!("[rules] 规则 {} 执行失败：{e:#}", r.id),
        }
    }
    // 规则产生的移动不进「撤销」，免得 ⌘Z 撤掉的不是用户刚做的操作
    if n > 0 {
        eng.set_last_undo(None);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        crate::core::queries::tests::seed(&s);
        assert!(add(&s, &Rule { field: "from".into(), value: " ".into(), action: "read".into(), ..Default::default() }).is_err());
        assert!(add(&s, &Rule { field: "from".into(), value: "x".into(), action: "move".into(), ..Default::default() }).is_err(), "移动必须有目标");
        add(&s, &Rule { field: "from".into(), value: "BOB@".into(), action: "archive".into(), ..Default::default() }).unwrap();
        add(&s, &Rule { field: "subject".into(), value: "weekly".into(), action: "delete".into(), ..Default::default() }).unwrap();
        add(&s, &Rule { field: "subject".into(), value: "发票".into(), action: "star".into(), ..Default::default() }).unwrap();
        let m = matches(&s, &[1, 2, 5]).unwrap();
        let got: Vec<(String, Vec<i64>)> = m.into_iter().map(|(r, ids)| (r.action, ids)).collect();
        assert_eq!(got, vec![("archive".into(), vec![2]), ("star".into(), vec![1])], "移动类每封只执行第一条");
        let id = list(&s).unwrap()[0].id;
        set_enabled(&s, id, false).unwrap();
        assert_eq!(matches(&s, &[2]).unwrap()[0].0.action, "delete");
    }
}
