//! 归档工具：完整备份（附校验报告）、安全瘦身、账号间搬家、定时增量导出。
//!
//! 定位是「邮件档案馆」：数据在用户自己手里，能完整地带走、能证明带走的东西是完整的。

use super::db::{now_ms, Store};
use super::engine::{Engine, Op};
use super::queries::ListParams;
use super::transfer::{self, ExportKind};
use anyhow::{bail, Context, Result};
use rusqlite::{params, OptionalExtension};
use std::path::{Path, PathBuf};

// ---------------- 完整备份 ----------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BackupReport {
    pub dir: PathBuf,
    pub messages: i64,
    pub with_body: i64,
    pub eml_copied: usize,
    pub eml_missing: usize,
    pub bytes: u64,
    /// 备份库与原库的邮件数是否一致、所有正文文件是否齐全
    pub verified: bool,
    pub problems: Vec<String>,
}

pub(crate) fn copy_dir(src: &Path, dst: &Path, bytes: &mut u64, n: &mut usize) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let p = e.path();
        let t = dst.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &t, bytes, n)?;
        } else {
            *bytes += std::fs::copy(&p, &t)?;
            *n += 1;
        }
    }
    Ok(())
}

/// 备份到 `parent` 下新建的带时间戳的目录：数据库（`VACUUM INTO`，一致性快照）+ 全部 .eml + 上传文件 + 主密钥，
/// 然后打开备份库逐条核对正文文件，生成 `校验报告.txt`。
pub fn backup(store: &Store, parent: &Path) -> Result<BackupReport> {
    let dir = parent.join(format!("EasyMail-备份-{}", transfer::stamp(now_ms())));
    std::fs::create_dir_all(&dir).with_context(|| format!("无法创建 {}", dir.display()))?;
    let db = dir.join(super::db::DB_NAME);
    store.w().execute("VACUUM INTO ?", [db.to_string_lossy().to_string()]).context("数据库快照失败")?;
    let mut rep = BackupReport { dir: dir.clone(), ..Default::default() };
    rep.bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
    let mut n = 0;
    copy_dir(&store.dir().join("eml"), &dir.join("eml"), &mut rep.bytes, &mut n)?;
    rep.eml_copied = n;
    let mut _u = 0;
    copy_dir(&store.uploads_dir(), &dir.join("uploads"), &mut rep.bytes, &mut _u)?;
    let key = store.dir().join("master.key");
    if key.exists() {
        rep.bytes += std::fs::copy(&key, dir.join("master.key"))?;
    }
    // 校验：用备份出来的库核对
    let c = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    rep.messages = c.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
    rep.with_body = c.query_row("SELECT COUNT(*) FROM messages WHERE body_state = 1", [], |r| r.get(0))?;
    let src_n: i64 = store.r().query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
    if src_n != rep.messages {
        rep.problems.push(format!("邮件数不一致：原库 {src_n}，备份 {}", rep.messages));
    }
    {
        let mut st = c.prepare("SELECT id, eml_path FROM messages WHERE body_state = 1")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, path) = row?;
            let p = Path::new(&path);
            let rel = if p.is_absolute() { p.strip_prefix(store.dir()).map(Path::to_path_buf).unwrap_or_default() } else { p.to_path_buf() };
            let ok = !rel.as_os_str().is_empty() && std::fs::metadata(dir.join(&rel)).is_ok_and(|m| m.len() > 0);
            if !ok {
                rep.eml_missing += 1;
                if rep.problems.len() < 20 {
                    rep.problems.push(format!("邮件 #{id} 的正文文件缺失：{path}"));
                }
            }
        }
    }
    rep.verified = rep.problems.is_empty();
    let report = format!(
        "EasyMail · 迷你邮件 备份校验报告\n时间：{}\n版本：{}\n\n邮件总数：{}\n已缓存正文：{}\n复制的正文文件：{}\n缺失的正文文件：{}\n总大小：{:.1} MB\n结论：{}\n{}\n\n说明：\n- easymail.db 是数据库快照，eml/ 是每封邮件的完整原文（RFC 822，可单独用任何邮件客户端打开）。\n- master.key 是凭据加密密钥：备份里含有加密后的账号密码 / 令牌，请像保管密码一样保管这个目录。\n- 恢复：退出 EasyMail，把整个目录里的文件复制到 ~/Library/Application Support/EasyMail/data/ 覆盖即可。\n- 未缓存正文的邮件只备份了信封（主题、发件人等），正文仍在服务器上。\n",
        chrono::Local::now().format("%Y/%m/%d %H:%M:%S"),
        super::config::VERSION,
        rep.messages,
        rep.with_body,
        rep.eml_copied,
        rep.eml_missing,
        rep.bytes as f64 / 1048576.0,
        if rep.verified { "✓ 校验通过：数据库与正文文件完整" } else { "✗ 发现问题（见下）" },
        rep.problems.join("\n")
    );
    std::fs::write(dir.join("校验报告.txt"), report)?;
    Ok(rep)
}

// ---------------- 安全瘦身 ----------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlimReport {
    pub messages: usize,
    pub bytes: u64,
}

/// 预估：早于 `days` 天、服务器上还有副本（非本地归档）的已缓存正文能释放多少空间
pub fn slim_preview(store: &Store, days: i64) -> Result<SlimReport> {
    let cutoff = now_ms() - days * 86_400_000;
    let (n, bytes): (i64, i64) = store.r().query_row(
        "SELECT COUNT(*), COALESCE(SUM(m.size), 0) FROM messages m JOIN accounts a ON a.id = m.account_id
         WHERE m.body_state = 1 AND m.date < ? AND a.provider != 'local' AND m.uid > 0",
        [cutoff],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(SlimReport { messages: n as usize, bytes: bytes as u64 })
}

/// 瘦身：删除早于 `days` 天的本地正文缓存（.eml），**只针对服务器上仍有副本的邮件**；
/// 本地归档（导入的邮件）是唯一副本，永远不动。正文文字保留（全文搜索照常可用），打开时再从服务器取回。
pub fn slim(store: &Store, days: i64) -> Result<SlimReport> {
    if days < 30 {
        bail!("为安全起见，只清理 30 天以前的邮件");
    }
    let cutoff = now_ms() - days * 86_400_000;
    let rows: Vec<(i64, String)> = {
        let c = store.r();
        let mut st = c.prepare(
            "SELECT m.id, m.eml_path FROM messages m JOIN accounts a ON a.id = m.account_id
             WHERE m.body_state = 1 AND m.date < ? AND a.provider != 'local' AND m.uid > 0",
        )?;
        let v = st.query_map([cutoff], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        v
    };
    let mut rep = SlimReport::default();
    for chunk in rows.chunks(500) {
        store.tx(|tx| {
            for (id, _) in chunk {
                tx.execute("UPDATE messages SET body_state = 0, eml_path = '', html = '' WHERE id = ?", [id])?;
            }
            Ok(())
        })?;
        for (_, path) in chunk {
            if let Some(p) = store.resolve_eml(path) {
                rep.bytes += std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                let _ = std::fs::remove_file(&p);
            }
            rep.messages += 1;
        }
    }
    Ok(rep)
}

// ---------------- 账号间搬家 ----------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CopyReport {
    pub copied: usize,
    pub skipped: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

/// 把一个文件夹里的邮件复制到另一个账号的文件夹（IMAP APPEND，保留已读 / 星标）。
/// 断点续传：每复制一封记一笔（copy_done），中断后重跑只复制剩下的。原邮件不删除。
pub async fn copy_folder(eng: Engine, src_folder: i64, dest_folder: i64, mut progress: impl FnMut(usize, usize) + Send) -> Result<CopyReport> {
    let store = eng.store().clone();
    let (dest_acc, dest_path, dest_provider): (i64, String, String) = store
        .r()
        .query_row("SELECT f.account_id, f.path, a.provider FROM folders f JOIN accounts a ON a.id = f.account_id WHERE f.id = ?", [dest_folder], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("目标文件夹不存在"))?;
    if src_folder == dest_folder {
        bail!("源文件夹和目标文件夹相同");
    }
    let job = format!("{src_folder}->{dest_folder}");
    let ids: Vec<(i64, bool, bool)> = {
        let c = store.r();
        let mut st = c.prepare(
            "SELECT id, seen, flagged FROM messages WHERE folder_id = ? AND uid != 0 AND id NOT IN (SELECT message_id FROM copy_done WHERE job = ?) ORDER BY date",
        )?;
        let v =
            st.query_map(params![src_folder, job], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0, r.get::<_, i64>(2)? != 0)))?.collect::<Result<Vec<_>, _>>()?;
        v
    };
    let done_before: i64 = store.r().query_row("SELECT COUNT(*) FROM copy_done WHERE job = ?", [&job], |r| r.get(0))?;
    let mut rep = CopyReport { skipped: done_before as usize, ..Default::default() };
    let total = ids.len();
    for (i, (id, seen, flagged)) in ids.into_iter().enumerate() {
        progress(i, total);
        let r = async {
            let raw = eng.source(id).await?;
            if dest_provider == super::providers::LOCAL_PROVIDER {
                transfer::import_raw(&store, dest_acc, dest_folder, &raw)?;
            } else {
                let mut flags = Vec::new();
                if seen {
                    flags.push("\\Seen");
                }
                if flagged {
                    flags.push("\\Flagged");
                }
                eng.remote(dest_acc, Op::Append { path: dest_path.clone(), flags: format!("({})", flags.join(" ")), raw }).await?;
            }
            anyhow::Ok(())
        }
        .await;
        match r {
            Ok(()) => {
                store.w().execute("INSERT OR IGNORE INTO copy_done (job, message_id) VALUES (?, ?)", params![job, id])?;
                rep.copied += 1;
            }
            Err(e) => {
                rep.failed += 1;
                if rep.errors.len() < 5 {
                    rep.errors.push(format!("#{id}：{e:#}"));
                }
                // 连接问题：后面的大概率也失败，停下来，下次续传
                if super::error::kind_of(&e) == super::error::ErrorKind::Connection && rep.failed >= 3 {
                    break;
                }
            }
        }
    }
    progress(total, total);
    if dest_provider == super::providers::LOCAL_PROVIDER {
        transfer::refresh_local_counts(&store, dest_folder)?;
    } else {
        eng.request_sync(dest_acc, Some(dest_folder), false);
    }
    Ok(rep)
}

// ---------------- 定时增量导出 ----------------

/// 设置：kv `auto_export_dir`（为空 = 关闭）、`auto_export_days`（间隔天数，默认 7）
pub fn auto_export_due(store: &Store) -> Option<PathBuf> {
    let dir = store.get_kv("auto_export_dir").filter(|d| !d.trim().is_empty())?;
    let days: i64 = store.get_kv("auto_export_days").and_then(|v| v.parse().ok()).unwrap_or(7).max(1);
    let last: i64 = store.get_kv("auto_export_at").and_then(|v| v.parse().ok()).unwrap_or(0);
    (now_ms() - last >= days * 86_400_000).then(|| PathBuf::from(dir))
}

/// 导出上次导出之后**新到**的邮件（mbox，一次一个文件），返回导出的封数
pub async fn run_auto_export(eng: Engine, dir: PathBuf) -> Result<usize> {
    let store = eng.store().clone();
    // 行号单调递增：上次导出到的最大 id 之后的就是新到的
    let since: i64 = store.get_kv("auto_export_since").and_then(|v| v.parse().ok()).unwrap_or(0);
    let now = now_ms();
    let ids: Vec<i64> = {
        let c = store.r();
        let mut st = c.prepare("SELECT m.id FROM messages m JOIN folders f ON f.id = m.folder_id WHERE m.id > ? AND m.uid != 0 AND f.role NOT IN ('trash','junk') ORDER BY m.id LIMIT ?")?;
        let v = st.query_map(params![since, transfer::EXPORT_CAP as i64], |r| r.get(0))?.collect::<Result<Vec<i64>, _>>()?;
        v
    };
    store.set_kv("auto_export_at", &now.to_string());
    if ids.is_empty() {
        return Ok(0);
    }
    std::fs::create_dir_all(&dir)?;
    let max_id = ids.iter().copied().max().unwrap_or(since);
    let dest = dir.join(format!("EasyMail-增量-{}.mbox", transfer::stamp(now)));
    let p = ListParams { view: super::queries::View::All, ids, ..Default::default() };
    let st = transfer::export(eng, p, ExportKind::Mbox, dest, String::new()).await?;
    store.set_kv("auto_export_since", &max_id.to_string());
    Ok(st.written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_verifies_and_slim_is_safe() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("data")).unwrap();
        crate::core::queries::tests::seed(&s);
        // 给 1 号写一个正文文件
        let raw = b"From: a@b.c\r\nSubject: x\r\n\r\nhello\r\n";
        crate::core::messages::store_body(&s, 1, raw).unwrap();
        let out = dir.path().join("bk");
        let rep = backup(&s, &out).unwrap();
        assert!(rep.verified, "{:?}", rep.problems);
        assert_eq!((rep.messages, rep.with_body, rep.eml_missing), (5, 1, 0));
        assert!(rep.dir.join("校验报告.txt").exists());
        assert!(rep.dir.join("master.key").exists());
        // 瘦身：太近的不清；把日期改老后清理，正文文字保留
        assert!(slim(&s, 7).is_err());
        assert_eq!(slim_preview(&s, 365).unwrap().messages, 1, "seed 的日期是 1970 年");
        let r = slim(&s, 365).unwrap();
        assert_eq!(r.messages, 1);
        let (state, text): (i64, String) = s.r().query_row("SELECT body_state, text FROM messages WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(state, 0);
        assert!(text.contains("hello"));
        assert!(!s.eml_abs(1, 1).exists());
    }
}
