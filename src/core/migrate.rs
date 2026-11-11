//! 首次启动：从 just-mail（Tauri 版或开发目录）的数据目录迁移存档。
//!
//! 数据库用 `VACUUM INTO` 拷贝（源库正在被 just-mail 使用也能得到一致快照），
//! 正文 eml 与上传目录优先硬链接（同一磁盘卷瞬间完成、不占双份空间），失败再复制。

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const DB_NAME: &str = "just-mail.db";

/// 候选的旧数据目录（按优先级）
pub fn candidates() -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(d) = std::env::var_os("EASYMAIL_IMPORT_FROM") {
        v.push(PathBuf::from(d));
    }
    if let Some(d) = dirs::data_dir() {
        v.push(d.join("Just Mail").join("data"));
    }
    v
}

/// 目标目录还没有数据库时，从第一个存在数据库的候选目录迁移。返回迁移来源。
pub fn migrate_if_needed(dest: &Path, from: &[PathBuf]) -> Result<Option<PathBuf>> {
    if dest.join(DB_NAME).exists() {
        return Ok(None);
    }
    let Some(src) = from.iter().find(|d| d.join(DB_NAME).is_file() && d.join("master.key").is_file() && *d != dest) else { return Ok(None) };
    std::fs::create_dir_all(dest)?;
    let tmp = dest.join(format!("{DB_NAME}.migrating"));
    let _ = std::fs::remove_file(&tmp);
    {
        let c = rusqlite::Connection::open_with_flags(src.join(DB_NAME), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).context("打开旧数据库")?;
        c.busy_timeout(std::time::Duration::from_secs(10))?;
        c.execute("VACUUM INTO ?", [tmp.to_string_lossy().as_ref()]).context("复制旧数据库")?;
    }
    std::fs::copy(src.join("master.key"), dest.join("master.key"))?;
    for sub in ["eml", "uploads"] {
        let s = src.join(sub);
        if s.is_dir() {
            link_tree(&s, &dest.join(sub))?;
        }
    }
    std::fs::rename(&tmp, dest.join(DB_NAME))?;
    Ok(Some(src.clone()))
}

fn link_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let (s, d) = (e.path(), dst.join(e.file_name()));
        if e.file_type()?.is_dir() {
            link_tree(&s, &d)?;
        } else if !d.exists() && std::fs::hard_link(&s, &d).is_err() {
            std::fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::Store;

    #[test]
    fn migrates_once() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let new = root.path().join("new");
        {
            let s = Store::open(&old).unwrap();
            s.set_kv("marker", "42");
            std::fs::create_dir_all(old.join("eml/1")).unwrap();
            std::fs::write(old.join("eml/1/7.eml"), b"Subject: hi\r\n\r\nbody").unwrap();
        }
        assert_eq!(migrate_if_needed(&new, std::slice::from_ref(&old)).unwrap(), Some(old.clone()));
        assert_eq!(std::fs::read(new.join("eml/1/7.eml")).unwrap(), b"Subject: hi\r\n\r\nbody");
        assert_eq!(std::fs::read(new.join("master.key")).unwrap(), std::fs::read(old.join("master.key")).unwrap());
        let s = Store::open(&new).unwrap();
        assert_eq!(s.get_kv("marker").as_deref(), Some("42"));
        drop(s);
        // 已有数据库：不再迁移
        assert_eq!(migrate_if_needed(&new, &[old]).unwrap(), None);
        assert_eq!(migrate_if_needed(&root.path().join("x"), &[root.path().join("none")]).unwrap(), None);
    }
}
