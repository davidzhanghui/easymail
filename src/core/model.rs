//! 行模型：数据库行 ↔ Rust 结构。

use rusqlite::Row;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Account {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub provider: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_secure: bool,
    pub use_proxy: bool,
    pub secret: String,
    pub color: String,
    pub signature: String,
    pub sync_enabled: bool,
    pub sort: i64,
    pub created_at: i64,
    pub last_sync_at: Option<i64>,
    pub last_error: String,
    /// 登录方式：password / oauth2
    pub auth: String,
}

impl Account {
    pub const COLS: &'static str = "id, email, name, provider, imap_host, imap_port, smtp_host, smtp_port, smtp_secure, use_proxy, secret, color, signature, sync_enabled, sort, created_at, last_sync_at, last_error, auth";

    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Account {
            id: r.get(0)?,
            email: r.get(1)?,
            name: r.get(2)?,
            provider: r.get(3)?,
            imap_host: r.get(4)?,
            imap_port: r.get::<_, i64>(5)? as u16,
            smtp_host: r.get(6)?,
            smtp_port: r.get::<_, i64>(7)? as u16,
            smtp_secure: r.get::<_, i64>(8)? != 0,
            use_proxy: r.get::<_, i64>(9)? != 0,
            secret: r.get(10)?,
            color: r.get(11)?,
            signature: r.get(12)?,
            sync_enabled: r.get::<_, i64>(13)? != 0,
            sort: r.get(14)?,
            created_at: r.get(15)?,
            last_sync_at: r.get(16)?,
            last_error: r.get(17)?,
            auth: r.get(18)?,
        })
    }

    pub fn is_local(&self) -> bool {
        self.provider == super::providers::LOCAL_PROVIDER
    }

    pub fn is_oauth(&self) -> bool {
        self.auth == "oauth2"
    }

    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.email
        } else {
            &self.name
        }
    }

    pub fn provider_name(&self) -> &'static str {
        if self.is_local() {
            "本地归档"
        } else {
            super::providers::provider(&self.provider).name
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Folder {
    pub id: i64,
    pub account_id: i64,
    pub path: String,
    pub name: String,
    pub delimiter: String,
    pub role: String,
    pub sync_enabled: bool,
    pub uidvalidity: String,
    pub modseq: String,
    pub last_uid: u32,
    pub oldest_uid: u32,
    pub total: i64,
    pub unseen: i64,
    pub backfill_done: bool,
    pub synced_at: Option<i64>,
    /// 本地已存封数（列表查询时计算）
    pub local: i64,
}

impl Folder {
    pub const COLS: &'static str =
        "id, account_id, path, name, delimiter, role, sync_enabled, uidvalidity, modseq, last_uid, oldest_uid, total, unseen, backfill_done, synced_at";

    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Folder {
            id: r.get(0)?,
            account_id: r.get(1)?,
            path: r.get(2)?,
            name: r.get(3)?,
            delimiter: r.get(4)?,
            role: r.get(5)?,
            sync_enabled: r.get::<_, i64>(6)? != 0,
            uidvalidity: r.get(7)?,
            modseq: r.get(8)?,
            last_uid: r.get::<_, i64>(9)?.clamp(0, u32::MAX as i64) as u32,
            oldest_uid: r.get::<_, i64>(10)?.clamp(0, u32::MAX as i64) as u32,
            total: r.get(11)?,
            unseen: r.get(12)?,
            backfill_done: r.get::<_, i64>(13)? != 0,
            synced_at: r.get(14)?,
            local: 0,
        })
    }

    pub fn label(&self) -> String {
        super::providers::folder_label(&self.role, &self.name)
    }

    /// 侧栏显示的叶子名
    pub fn leaf(&self) -> String {
        let l = self.label();
        if self.delimiter.is_empty() {
            return l;
        }
        l.rsplit(self.delimiter.as_str()).next().unwrap_or(&l).to_string()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Addr {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub address: String,
}

impl Addr {
    pub fn text(&self) -> String {
        if self.name.is_empty() {
            self.address.clone()
        } else {
            format!("{} <{}>", self.name, self.address)
        }
    }
    pub fn short(&self) -> &str {
        if self.name.is_empty() {
            &self.address
        } else {
            &self.name
        }
    }
}

pub fn addrs_text(list: &[Addr]) -> String {
    list.iter().map(Addr::text).collect::<Vec<_>>().join(", ")
}

pub fn parse_addrs_json(s: &str) -> Vec<Addr> {
    serde_json::from_str(s).unwrap_or_default()
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttMeta {
    pub index: usize,
    pub filename: String,
    pub content_type: String,
    pub size: u64,
    #[serde(default)]
    pub inline: bool,
}

/// 列表里的一行
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListItem {
    pub id: i64,
    pub account_id: i64,
    pub folder_id: i64,
    pub subject: String,
    pub from: Addr,
    pub to: Vec<Addr>,
    pub date: i64,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub has_att: bool,
    pub snippet: String,
    pub size: i64,
    pub folder_name: String,
    pub folder_role: String,
    pub account_email: String,
    pub account_name: String,
    pub account_color: String,
    /// 会话视图：这个会话里（命中筛选条件的）邮件数；平铺视图恒为 1
    pub thread_count: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadItem {
    pub id: i64,
    pub subject: String,
    pub from: Addr,
    pub date: i64,
    pub folder: String,
    /// 所在文件夹的角色（inbox / sent / trash …），会话级操作用它决定哪些信可以动
    pub role: String,
    pub seen: bool,
    pub snippet: String,
}

/// 读信详情
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detail {
    pub id: i64,
    pub account_id: i64,
    pub folder_id: i64,
    pub subject: String,
    pub message_id: String,
    pub from: Addr,
    pub reply_to: String,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub date: i64,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub text: String,
    pub html: String,
    pub attachments: Vec<AttMeta>,
    pub account_email: String,
    pub account_name: String,
    pub account_color: String,
    pub account_local: bool,
    pub folder_role: String,
    pub folder_label: String,
    pub thread: Vec<ThreadItem>,
    /// List-Unsubscribe 原文
    pub unsub: String,
    pub unsub_post: bool,
    /// Authentication-Results 原文
    pub auth_res: String,
    /// "像素数|主机1,主机2"
    pub trackers: String,
    pub snoozed_until: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftAtt {
    pub token: String,
    pub filename: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draft {
    pub id: Option<i64>,
    pub account_id: Option<i64>,
    pub mode: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    pub attachments: Vec<DraftAtt>,
    pub source_id: Option<i64>,
    pub include_original: bool,
    pub updated_at: i64,
    /// 回复 / 转发时要引用的原邮件 HTML（只在打开写信窗口时用一次，不入库；载入编辑器清洗后变成正文的一部分）
    pub quote_html: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub inbox: i64,
    pub unread: i64,
    pub starred: i64,
    pub drafts: i64,
    pub archived: i64,
    pub with_body: i64,
    pub snoozed: i64,
    pub outbox: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contact {
    pub name: String,
    pub address: String,
}
