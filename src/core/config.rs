//! 数据目录与可在「设置」里修改的选项。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const APP_NAME: &str = "迷你邮件";
pub const APP_NAME_EN: &str = "EasyMail";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUNDLE_ID: &str = "app.easymail";

/// 数据目录：`EASYMAIL_DATA_DIR` 优先，否则 `~/Library/Application Support/EasyMail/data`。
pub fn default_data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("EASYMAIL_DATA_DIR") {
        return PathBuf::from(d);
    }
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join(APP_NAME_EN).join("data")
}

/// settings 表里按 JSON 存。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub proxy_url: String,
    pub sync_interval_min: u32,
    /// 首次同步每个文件夹拉最近多少封的信封，其余后台逐批回填
    pub initial_sync_per_folder: u32,
    /// 是否在空闲时继续回填更早的邮件
    pub backfill: bool,
    /// 每个文件夹后台预取正文的封数（用于全文搜索与离线阅读）
    pub prefetch_bodies: u32,
    pub max_prefetch_size: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            // 默认不写死代理地址：首次启动时探测本机常见代理端口（见 `tools::init_proxy`）
            proxy_url: std::env::var("PROXY_URL").unwrap_or_default(),
            sync_interval_min: 5,
            initial_sync_per_folder: 500,
            backfill: true,
            prefetch_bodies: 300,
            max_prefetch_size: 15 * 1024 * 1024,
        }
    }
}

impl Settings {
    pub const KEYS: [&'static str; 6] = ["proxyUrl", "syncIntervalMin", "initialSyncPerFolder", "backfill", "prefetchBodies", "maxPrefetchSize"];
}
