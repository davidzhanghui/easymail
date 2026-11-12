//! 本地存档：SQLite（WAL）。
//!
//! 连接：一条 `write` 给同步引擎与所有写操作，`read` 是一个小连接池给各处的读查询（WAL 下读写、读读互不阻塞）。
//! messages_fts 用 trigram 分词，中文 / 英文子串都能搜（≥ 3 个字符走 FTS，更短的回退 LIKE）。

use super::config::Settings;
use super::crypto::Crypto;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// 数据库文件名
pub const DB_NAME: &str = "easymail.db";

pub const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS accounts (
  id            INTEGER PRIMARY KEY,
  email         TEXT NOT NULL UNIQUE,
  name          TEXT NOT NULL DEFAULT '',
  provider      TEXT NOT NULL,
  imap_host     TEXT NOT NULL,
  imap_port     INTEGER NOT NULL DEFAULT 993,
  smtp_host     TEXT NOT NULL,
  smtp_port     INTEGER NOT NULL DEFAULT 465,
  smtp_secure   INTEGER NOT NULL DEFAULT 1,
  use_proxy     INTEGER NOT NULL DEFAULT 0,
  secret        TEXT NOT NULL,
  color         TEXT NOT NULL DEFAULT '',
  signature     TEXT NOT NULL DEFAULT '',
  sync_enabled  INTEGER NOT NULL DEFAULT 1,
  sort          INTEGER NOT NULL DEFAULT 0,
  created_at    INTEGER NOT NULL,
  last_sync_at  INTEGER,
  last_error    TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS folders (
  id            INTEGER PRIMARY KEY,
  account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  path          TEXT NOT NULL,
  name          TEXT NOT NULL,
  delimiter     TEXT NOT NULL DEFAULT '/',
  role          TEXT NOT NULL DEFAULT '',
  sync_enabled  INTEGER NOT NULL DEFAULT 1,
  uidvalidity   TEXT NOT NULL DEFAULT '',
  modseq        TEXT NOT NULL DEFAULT '',
  last_uid      INTEGER NOT NULL DEFAULT 0,
  oldest_uid    INTEGER NOT NULL DEFAULT 0,
  total         INTEGER NOT NULL DEFAULT 0,
  unseen        INTEGER NOT NULL DEFAULT 0,
  backfill_done INTEGER NOT NULL DEFAULT 0,
  synced_at     INTEGER,
  UNIQUE (account_id, path)
);

CREATE TABLE IF NOT EXISTS messages (
  id           INTEGER PRIMARY KEY,
  account_id   INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  folder_id    INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
  uid          INTEGER NOT NULL,
  message_id   TEXT NOT NULL DEFAULT '',
  in_reply_to  TEXT NOT NULL DEFAULT '',
  refs         TEXT NOT NULL DEFAULT '',
  subject      TEXT NOT NULL DEFAULT '',
  from_name    TEXT NOT NULL DEFAULT '',
  from_addr    TEXT NOT NULL DEFAULT '',
  to_json      TEXT NOT NULL DEFAULT '[]',
  cc_json      TEXT NOT NULL DEFAULT '[]',
  reply_to     TEXT NOT NULL DEFAULT '',
  date         INTEGER NOT NULL,
  size         INTEGER NOT NULL DEFAULT 0,
  seen         INTEGER NOT NULL DEFAULT 0,
  flagged      INTEGER NOT NULL DEFAULT 0,
  answered     INTEGER NOT NULL DEFAULT 0,
  draft        INTEGER NOT NULL DEFAULT 0,
  has_att      INTEGER NOT NULL DEFAULT 0,
  snippet      TEXT NOT NULL DEFAULT '',
  body_state   INTEGER NOT NULL DEFAULT 0,
  text         TEXT,
  html         TEXT,
  att_json     TEXT NOT NULL DEFAULT '[]',
  eml_path     TEXT NOT NULL DEFAULT '',
  UNIQUE (folder_id, uid)
);
CREATE INDEX IF NOT EXISTS idx_msg_folder_date ON messages(folder_id, date DESC);
CREATE INDEX IF NOT EXISTS idx_msg_account_date ON messages(account_id, date DESC);
CREATE INDEX IF NOT EXISTS idx_msg_date ON messages(date DESC);
CREATE INDEX IF NOT EXISTS idx_msg_mid ON messages(message_id);
CREATE INDEX IF NOT EXISTS idx_msg_body ON messages(body_state, folder_id);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
  subject, people, body,
  content = '', contentless_delete = 1, tokenize = 'trigram'
);
CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
  DELETE FROM messages_fts WHERE rowid = old.id;
END;

CREATE TABLE IF NOT EXISTS drafts (
  id           INTEGER PRIMARY KEY,
  account_id   INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
  mode         TEXT NOT NULL DEFAULT 'new',
  to_addr      TEXT NOT NULL DEFAULT '',
  cc_addr      TEXT NOT NULL DEFAULT '',
  bcc_addr     TEXT NOT NULL DEFAULT '',
  subject      TEXT NOT NULL DEFAULT '',
  body         TEXT NOT NULL DEFAULT '',
  attachments  TEXT NOT NULL DEFAULT '[]',
  source_id    INTEGER,
  include_orig INTEGER NOT NULL DEFAULT 0,
  updated_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS uploads (
  token        TEXT PRIMARY KEY,
  filename     TEXT NOT NULL,
  content_type TEXT NOT NULL,
  size         INTEGER NOT NULL,
  created_at   INTEGER NOT NULL
);
"#;

/// 顺序迁移：`PRAGMA user_version` 记录已执行到第几条。上面的 `SCHEMA` 视为版本 0，
/// 以后的表结构改动只往这里**追加**，不要改已有的条目，已有的数据目录照常可用。
pub const MIGRATIONS: &[&str] = &[
    // 1：持久化的远端操作队列。本地改动与待办在同一个事务里写入，由账号 worker 负责送达服务器，
    //    断网 / 超时就留着下次重放，服务器明确拒绝才按 undo 回滚本地。
    "CREATE TABLE IF NOT EXISTS pending_ops (
       id          INTEGER PRIMARY KEY,
       account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
       kind        TEXT NOT NULL,
       payload     TEXT NOT NULL,
       undo        TEXT NOT NULL DEFAULT '{}',
       attempts    INTEGER NOT NULL DEFAULT 0,
       last_error  TEXT NOT NULL DEFAULT '',
       created_at  INTEGER NOT NULL
     );
     CREATE INDEX IF NOT EXISTS idx_pending_account ON pending_ops(account_id, id);",
    // 2：侧栏「未读 / 星标」计数只扫对应的少量行，不再全表扫
    "CREATE INDEX IF NOT EXISTS idx_msg_unseen ON messages(folder_id) WHERE seen = 0;
     CREATE INDEX IF NOT EXISTS idx_msg_flagged ON messages(folder_id) WHERE flagged = 1;",
    // 3：每个文件夹上次同步时服务器的 EXISTS / UIDNEXT 与上次全量核对的时间。
    "CREATE TABLE IF NOT EXISTS folder_sync (
       folder_id   INTEGER PRIMARY KEY REFERENCES folders(id) ON DELETE CASCADE,
       exists_n    INTEGER NOT NULL DEFAULT -1,
       uidnext     INTEGER NOT NULL DEFAULT 0,
       full_at     INTEGER NOT NULL DEFAULT 0
     );",
    // 4：中日韩二元组索引。trigram 要求词至少 3 个字，而中文最常搜的是两个字的词（发票、合同、报销）；
    //    写入时把 CJK 文字切成重叠的二元组（「增值税」→「增值 值税」），两字词走这张表，不再全表 LIKE。
    //    老数据由启动时的后台任务补建（见 `Store::backfill_bigrams`），补完之前查询仍走 LIKE。
    "CREATE VIRTUAL TABLE IF NOT EXISTS messages_bigram USING fts5(
       grams, content = '', contentless_delete = 1, tokenize = 'unicode61'
     );
     CREATE TRIGGER IF NOT EXISTS messages_ad_bigram AFTER DELETE ON messages BEGIN
       DELETE FROM messages_bigram WHERE rowid = old.id;
     END;",
    // 5：待办的「最早执行时间」。移动 / 删除后有几秒的撤销窗口，窗口内 worker 不送达服务器
    "ALTER TABLE pending_ops ADD COLUMN not_before INTEGER NOT NULL DEFAULT 0;",
    // 6：会话键。取 References 的第一个（会话的根），没有就取 In-Reply-To，再没有就是自己的 Message-ID。
    //    打开邮件时的「同一会话」与列表的会话视图都按 (account_id, thread) 走索引，不再全表扫 refs。
    "ALTER TABLE messages ADD COLUMN thread TEXT NOT NULL DEFAULT '';
     UPDATE messages SET thread = CASE
         WHEN trim(refs) != '' THEN substr(trim(refs), 1, instr(trim(refs) || ' ', ' ') - 1)
         WHEN in_reply_to != '' THEN in_reply_to
         WHEN message_id != '' THEN message_id
         ELSE 'id:' || id END;
     CREATE INDEX IF NOT EXISTS idx_msg_thread ON messages(account_id, thread);",
    // 7：退订头、服务器写的认证结果（SPF / DKIM / DMARC）、正文里的远程资源域名（追踪像素统计）
    "ALTER TABLE messages ADD COLUMN unsub TEXT NOT NULL DEFAULT '';
     ALTER TABLE messages ADD COLUMN unsub_post INTEGER NOT NULL DEFAULT 0;
     ALTER TABLE messages ADD COLUMN auth_res TEXT NOT NULL DEFAULT '';
     ALTER TABLE messages ADD COLUMN trackers TEXT NOT NULL DEFAULT '';
     CREATE INDEX IF NOT EXISTS idx_msg_from ON messages(from_addr);",
    // 8：发件箱：点「发送」先落库，撤回窗口 / 定时时间到了再由引擎发出；离线时一直排队，联网后自动发
    "CREATE TABLE IF NOT EXISTS outbox (
       id          INTEGER PRIMARY KEY,
       account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
       draft_id    INTEGER,
       payload     TEXT NOT NULL,
       subject     TEXT NOT NULL DEFAULT '',
       rcpt        TEXT NOT NULL DEFAULT '',
       send_at     INTEGER NOT NULL,
       state       TEXT NOT NULL DEFAULT 'queued',
       attempts    INTEGER NOT NULL DEFAULT 0,
       last_error  TEXT NOT NULL DEFAULT '',
       created_at  INTEGER NOT NULL
     );
     CREATE INDEX IF NOT EXISTS idx_outbox_due ON outbox(state, send_at);",
    // 9：本地规则：新信入库后按发件人 / 收件人 / 主题匹配，执行移动 / 标记 / 星标 / 归档 / 删除
    "CREATE TABLE IF NOT EXISTS rules (
       id          INTEGER PRIMARY KEY,
       account_id  INTEGER REFERENCES accounts(id) ON DELETE CASCADE,
       field       TEXT NOT NULL,
       value       TEXT NOT NULL,
       action      TEXT NOT NULL,
       target      INTEGER,
       enabled     INTEGER NOT NULL DEFAULT 1,
       hits        INTEGER NOT NULL DEFAULT 0,
       created_at  INTEGER NOT NULL
     );",
    // 10：稍后处理：到点前不出现在列表里，到点后标为未读重新出现
    "CREATE TABLE IF NOT EXISTS snoozed (
       message_id  INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
       until       INTEGER NOT NULL
     );
     CREATE INDEX IF NOT EXISTS idx_snooze_until ON snoozed(until);",
    // 11：始终显示远程图片的发件人
    "CREATE TABLE IF NOT EXISTS remote_allow (
       address     TEXT PRIMARY KEY,
       created_at  INTEGER NOT NULL
     );",
    // 12：账号间搬家的进度（断点续传）：已经复制过的源邮件
    "CREATE TABLE IF NOT EXISTS copy_done (
       job         TEXT NOT NULL,
       message_id  INTEGER NOT NULL,
       PRIMARY KEY (job, message_id)
     );",
    // 13：登录方式。password = 密码 / 授权码；oauth2 = XOAUTH2（secret 里存加密后的令牌 JSON）
    "ALTER TABLE accounts ADD COLUMN auth TEXT NOT NULL DEFAULT 'password';",
    // 14：草稿同步到服务器「草稿箱」后的 Message-ID（下次保存时替换掉旧副本；发出 / 丢弃时删掉）
    "ALTER TABLE drafts ADD COLUMN remote_mid TEXT NOT NULL DEFAULT '';
     ALTER TABLE drafts ADD COLUMN remote_account INTEGER;",
    // 15：发件箱 id 改为 AUTOINCREMENT：撤回会删行，复用 rowid 会让旧提示条上的「撤回」误撤另一封
    "CREATE TABLE outbox_v2 (
       id          INTEGER PRIMARY KEY AUTOINCREMENT,
       account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
       draft_id    INTEGER,
       payload     TEXT NOT NULL,
       subject     TEXT NOT NULL DEFAULT '',
       rcpt        TEXT NOT NULL DEFAULT '',
       send_at     INTEGER NOT NULL,
       state       TEXT NOT NULL DEFAULT 'queued',
       attempts    INTEGER NOT NULL DEFAULT 0,
       last_error  TEXT NOT NULL DEFAULT '',
       created_at  INTEGER NOT NULL
     );
     INSERT INTO outbox_v2 SELECT id, account_id, draft_id, payload, subject, rcpt, send_at, state, attempts, last_error, created_at FROM outbox;
     DROP TABLE outbox;
     ALTER TABLE outbox_v2 RENAME TO outbox;
     CREATE INDEX IF NOT EXISTS idx_outbox_due ON outbox(state, send_at);",
];

/// 二元组索引是否已覆盖全部老数据（没补完时两字词搜索退回 LIKE，结果不缺）
static BIGRAM_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn bigram_ready() -> bool {
    BIGRAM_READY.load(std::sync::atomic::Ordering::Relaxed)
}

const BIGRAM_CURSOR_KEY: &str = "bigram_cursor";
/// 补建完成的标记值
const BIGRAM_DONE: i64 = i64::MAX;

/// 中日韩文字（汉字、假名、谚文）
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xAC00..=0xD7AF | 0x20000..=0x2FFFF)
}

/// 把文本里的 CJK 连续片段切成重叠二元组，用空格连接（unicode61 分词后每个二元组就是一个词）
pub fn cjk_bigrams(text: &str) -> String {
    let mut out = String::new();
    let mut prev: Option<char> = None;
    for c in text.chars() {
        if is_cjk(c) {
            if let Some(p) = prev {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push(p);
                out.push(c);
            }
            prev = Some(c);
        } else {
            prev = None;
        }
    }
    out
}

/// 执行还没跑过的迁移（每条一个事务，跑完立刻写回版本号）。返回执行了几条。
pub fn run_migrations(c: &mut Connection) -> Result<usize> {
    let cur: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let mut n = 0;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(cur.max(0) as usize) {
        let tx = c.transaction()?;
        tx.execute_batch(sql).with_context(|| format!("数据库迁移 {} 失败", i + 1))?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
        n += 1;
    }
    Ok(n)
}

/// 存储：两条连接 + 数据目录 + 凭据加密器。可在线程间共享。
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

/// 读连接池大小：界面、引擎 worker、后台任务的读查询不再挤在一把锁上（WAL 下读读、读写都可并行）
const READERS: usize = 3;

struct Inner {
    write: Mutex<Connection>,
    read: Vec<Mutex<Connection>>,
    dir: PathBuf,
    crypto: Crypto,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store> {
        for d in ["", "eml", "uploads"] {
            std::fs::create_dir_all(dir.join(d)).with_context(|| format!("无法创建数据目录 {}", dir.display()))?;
        }
        let path = dir.join(DB_NAME);
        let mut write = Connection::open(&path).with_context(|| format!("无法打开数据库 {}", path.display()))?;
        write.busy_timeout(std::time::Duration::from_secs(10))?;
        write.execute_batch(SCHEMA)?;
        let fresh_bigram = {
            let v: i64 = write.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            v < 4
        };
        run_migrations(&mut write)?;
        if fresh_bigram {
            // 刚建的二元组表：没有老邮件时直接可用，有就交给后台补建（游标从 0 开始）
            let n: i64 = write.query_row("SELECT count(*) FROM messages", [], |r| r.get(0))?;
            write.execute("DELETE FROM settings WHERE key = ?", [BIGRAM_CURSOR_KEY])?;
            if n == 0 {
                write.execute("INSERT INTO settings(key, value) VALUES(?, ?)", params![BIGRAM_CURSOR_KEY, BIGRAM_DONE.to_string()])?;
            }
        }
        let mut read = Vec::with_capacity(READERS);
        for _ in 0..READERS {
            let c = Connection::open(&path)?;
            c.busy_timeout(std::time::Duration::from_secs(10))?;
            c.execute_batch("PRAGMA foreign_keys = ON;")?;
            read.push(Mutex::new(c));
        }
        let crypto = Crypto::load_or_create(&dir.join("master.key"))?;
        let s = Store { inner: Arc::new(Inner { write: Mutex::new(write), read, dir: dir.to_path_buf(), crypto }) };
        s.migrate_eml_paths()?;
        // 新库或已补完：二元组索引可直接用（新写入的邮件都会同步建索引）
        BIGRAM_READY.store(s.bigram_cursor() == BIGRAM_DONE, std::sync::atomic::Ordering::Relaxed);
        Ok(s)
    }

    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    pub fn crypto(&self) -> &Crypto {
        &self.inner.crypto
    }

    /// 写连接（同步引擎 / 写操作）。
    pub fn w(&self) -> MutexGuard<'_, Connection> {
        self.inner.write.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 读连接：从池里拿一条空闲的；都在忙就按线程分散到某一条上等。
    pub fn r(&self) -> MutexGuard<'_, Connection> {
        for m in &self.inner.read {
            match m.try_lock() {
                Ok(g) => return g,
                Err(std::sync::TryLockError::Poisoned(e)) => return e.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {}
            }
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::thread::current().id().hash(&mut h);
        let i = (h.finish() as usize) % self.inner.read.len();
        self.inner.read[i].lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 事务：出错回滚。
    pub fn tx<T>(&self, f: impl FnOnce(&rusqlite::Transaction) -> Result<T>) -> Result<T> {
        let mut c = self.w();
        let tx = c.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    // ---------- 设置 ----------

    pub fn settings(&self) -> Settings {
        let mut map = serde_json::to_value(Settings::default()).unwrap_or_default();
        if let Ok(rows) = (|| -> Result<Vec<(String, String)>> {
            let c = self.r();
            let mut st = c.prepare("SELECT key, value FROM settings")?;
            let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })() {
            for (k, v) in rows {
                if let (Some(obj), Ok(val)) = (map.as_object_mut(), serde_json::from_str::<serde_json::Value>(&v)) {
                    if Settings::KEYS.contains(&k.as_str()) {
                        obj.insert(k, val);
                    }
                }
            }
        }
        serde_json::from_value(map).unwrap_or_default()
    }

    pub fn save_settings(&self, s: &Settings) -> Result<Settings> {
        let v = serde_json::to_value(s)?;
        let c = self.w();
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                c.execute("INSERT INTO settings(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![k, val.to_string()])?;
            }
        }
        drop(c);
        Ok(self.settings())
    }

    /// 应用私有的键值（主题、窗口状态等），存在同一张 settings 表里，用 `ui.` 前缀与同步设置区分。
    pub fn get_kv(&self, key: &str) -> Option<String> {
        self.r()
            .query_row("SELECT value FROM settings WHERE key = ?", [format!("ui.{key}")], |r| r.get::<_, String>(0))
            .optional()
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str::<String>(&v).ok())
    }

    pub fn set_kv(&self, key: &str, value: &str) {
        let _ = self.w().execute(
            "INSERT INTO settings(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![format!("ui.{key}"), serde_json::to_string(value).unwrap_or_default()],
        );
    }

    // ---------- 迁移 ----------

    /// 老版本把 .eml 的绝对路径写进了库，换机器就失效；统一改写成相对路径（幂等）。
    fn migrate_eml_paths(&self) -> Result<usize> {
        let prefix = format!("{}/", self.dir().display());
        let c = self.w();
        let n =
            c.execute("UPDATE messages SET eml_path = substr(eml_path, ?1) WHERE eml_path LIKE ?2", params![prefix.len() as i64 + 1, format!("{prefix}%")])?;
        Ok(n)
    }

    // ---------- 全文索引 ----------

    pub fn fts_upsert(c: &Connection, id: i64, subject: &str, people: &str, body: &str) -> Result<()> {
        c.execute("DELETE FROM messages_fts WHERE rowid = ?", [id])?;
        let body: String = body.chars().take(200_000).collect();
        c.execute("INSERT INTO messages_fts(rowid, subject, people, body) VALUES(?, ?, ?, ?)", params![id, subject, people, body])?;
        Self::bigram_upsert(c, id, &format!("{subject}\n{people}\n{body}"))
    }

    fn bigram_upsert(c: &Connection, id: i64, text: &str) -> Result<()> {
        c.execute("DELETE FROM messages_bigram WHERE rowid = ?", [id])?;
        let grams = cjk_bigrams(text);
        if !grams.is_empty() {
            c.execute("INSERT INTO messages_bigram(rowid, grams) VALUES(?, ?)", params![id, grams])?;
        }
        Ok(())
    }

    fn bigram_cursor(&self) -> i64 {
        self.r()
            .query_row("SELECT value FROM settings WHERE key = ?", [BIGRAM_CURSOR_KEY], |r| r.get::<_, String>(0))
            .ok()
            .and_then(|v| serde_json::from_str::<serde_json::Value>(&v).ok().and_then(|j| j.as_i64()).or_else(|| v.parse().ok()))
            .unwrap_or(0)
    }

    /// 给升级前就有的邮件补建二元组索引：按 id 递增分批处理，进度记在 settings 表里，中途退出下次接着来。
    /// 每批一个短事务，不长时间占着写锁。返回这次处理的封数；全部补完后 [`bigram_ready`] 变为 true。
    pub fn backfill_bigrams(&self, batch: usize) -> Result<usize> {
        let cursor = self.bigram_cursor();
        if cursor == BIGRAM_DONE {
            BIGRAM_READY.store(true, std::sync::atomic::Ordering::Relaxed);
            return Ok(0);
        }
        let rows: Vec<(i64, String, String, String, String, String)> = {
            let c = self.r();
            let mut st = c.prepare(
                "SELECT id, subject, from_name || ' ' || from_addr, to_json, cc_json, COALESCE(text, '') FROM messages WHERE id > ? ORDER BY id LIMIT ?",
            )?;
            let v = st
                .query_map(params![cursor, batch as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            v
        };
        let Some(last) = rows.last().map(|r| r.0) else {
            self.w().execute(
                "INSERT INTO settings(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![BIGRAM_CURSOR_KEY, BIGRAM_DONE.to_string()],
            )?;
            BIGRAM_READY.store(true, std::sync::atomic::Ordering::Relaxed);
            return Ok(0);
        };
        self.tx(|tx| {
            for (id, subject, from, to, cc, text) in &rows {
                let body: String = text.chars().take(200_000).collect();
                Self::bigram_upsert(tx, *id, &format!("{subject}\n{from}\n{to}\n{cc}\n{body}"))?;
            }
            tx.execute(
                "INSERT INTO settings(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![BIGRAM_CURSOR_KEY, last.to_string()],
            )?;
            Ok(())
        })?;
        Ok(rows.len())
    }

    // ---------- .eml 路径 ----------

    pub fn eml_abs(&self, acc_id: i64, msg_id: i64) -> PathBuf {
        self.dir().join("eml").join(acc_id.to_string()).join(format!("{msg_id}.eml"))
    }

    pub fn eml_rel(acc_id: i64, msg_id: i64) -> String {
        format!("eml/{acc_id}/{msg_id}.eml")
    }

    /// 解析数据库里的 eml_path：兼容老的绝对路径与新的相对路径。
    pub fn resolve_eml(&self, stored: &str) -> Option<PathBuf> {
        if stored.is_empty() {
            return None;
        }
        let p = Path::new(stored);
        Some(if p.is_absolute() { p.to_path_buf() } else { self.dir().join(p) })
    }

    pub fn uploads_dir(&self) -> PathBuf {
        self.dir().join("uploads")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_schema_and_settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        assert!(dir.path().join("master.key").exists());
        let mut st = s.settings();
        assert_eq!(st.sync_interval_min, 5);
        st.sync_interval_min = 9;
        st.proxy_url = "http://127.0.0.1:1087".into();
        let back = s.save_settings(&st).unwrap();
        assert_eq!(back.sync_interval_min, 9);
        assert_eq!(back.proxy_url, "http://127.0.0.1:1087");
        // 布尔值按 JSON 存
        s.w().execute("INSERT OR REPLACE INTO settings(key,value) VALUES('backfill','false')", []).unwrap();
        assert!(!s.settings().backfill);
        s.set_kv("theme", "midnight");
        assert_eq!(s.get_kv("theme").as_deref(), Some("midnight"));
        // ui.* 不会混进同步设置
        assert_eq!(s.settings().sync_interval_min, 9);
    }

    #[test]
    fn migrations_run_once_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let v: i64 = s.r().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
        let n: i64 = s.r().query_row("SELECT count(*) FROM sqlite_master WHERE name = 'pending_ops'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        drop(s);
        // 再次打开不会重复执行
        let s = Store::open(dir.path()).unwrap();
        assert_eq!(run_migrations(&mut s.w()).unwrap(), 0);
    }

    #[test]
    fn bigrams() {
        assert_eq!(cjk_bigrams("增值税 invoice 发票a合"), "增值 值税 发票");
        assert_eq!(cjk_bigrams("单"), "");
        // 新库直接可用；老库补建后可用
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        assert_eq!(s.bigram_cursor(), BIGRAM_DONE);
        s.w().execute("INSERT INTO accounts(id,email,provider,imap_host,smtp_host,secret,created_at) VALUES(1,'a@b.c','custom','h','h','x',0)", []).unwrap();
        s.w().execute("INSERT INTO folders(id,account_id,path,name) VALUES(1,1,'INBOX','INBOX')", []).unwrap();
        s.w().execute("INSERT INTO messages(id,account_id,folder_id,uid,date,subject,text) VALUES(5,1,1,1,0,'报销','请查收合同')", []).unwrap();
        s.w().execute("DELETE FROM settings WHERE key = ?", [BIGRAM_CURSOR_KEY]).unwrap();
        assert_eq!(s.backfill_bigrams(100).unwrap(), 1);
        assert_eq!(s.backfill_bigrams(100).unwrap(), 0);
        assert_eq!(s.bigram_cursor(), BIGRAM_DONE);
        let n: i64 = s.r().query_row("SELECT count(*) FROM messages_bigram WHERE messages_bigram MATCH '\"合同\"'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        s.w().execute("DELETE FROM messages WHERE id = 5", []).unwrap();
        let n: i64 = s.r().query_row("SELECT count(*) FROM messages_bigram WHERE messages_bigram MATCH '\"合同\"'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "删除触发器同步清索引");
    }

    #[test]
    fn trigram_fts_cjk() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let c = s.w();
        Store::fts_upsert(&c, 1, "发票已开具", "张三 zhang@ex.com", "您的增值税发票请查收").unwrap();
        Store::fts_upsert(&c, 2, "Weekly report", "bob", "numbers inside").unwrap();
        let n: i64 = c.query_row("SELECT count(*) FROM messages_fts WHERE messages_fts MATCH '\"增值税\"'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let n: i64 = c.query_row("SELECT count(*) FROM messages_fts WHERE messages_fts MATCH '\"eekly\"'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn migrates_absolute_eml_paths() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = Store::open(dir.path()).unwrap();
            let c = s.w();
            c.execute("INSERT INTO accounts(id,email,provider,imap_host,smtp_host,secret,created_at) VALUES(1,'a@b.c','custom','h','h','x',0)", []).unwrap();
            c.execute("INSERT INTO folders(id,account_id,path,name) VALUES(1,1,'INBOX','INBOX')", []).unwrap();
            let abs = format!("{}/eml/1/9.eml", dir.path().display());
            c.execute("INSERT INTO messages(id,account_id,folder_id,uid,date,eml_path) VALUES(9,1,1,1,0,?)", [abs]).unwrap();
        }
        let s = Store::open(dir.path()).unwrap();
        let p: String = s.r().query_row("SELECT eml_path FROM messages WHERE id=9", [], |r| r.get(0)).unwrap();
        assert_eq!(p, "eml/1/9.eml");
        assert_eq!(s.resolve_eml(&p).unwrap(), dir.path().join("eml/1/9.eml"));
    }
}
