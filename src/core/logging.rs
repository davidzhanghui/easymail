//! 日志后端：写到数据目录的 `easymail.log`，超过上限就把旧文件改名为 `easymail.log.1` 再重新开始。
//!
//! 双击打开的 .app 没有终端接 stderr，没有后端的话 `log::warn!` 会全部被丢弃。
//! 级别默认 info（本 crate）/ warn（依赖），可用环境变量 `EASYMAIL_LOG=debug` 调高。

use log::{Level, LevelFilter, Log, Metadata, Record};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 单个日志文件的上限；轮转后最多占用两倍这么大
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;

struct FileLogger {
    path: PathBuf,
    max: u64,
    level: Level,
    state: Mutex<Option<(File, u64)>>,
    stderr: bool,
}

impl FileLogger {
    fn open(path: &Path) -> Option<(File, u64)> {
        let f = OpenOptions::new().create(true).append(true).open(path).ok()?;
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        Some((f, len))
    }

    fn write_line(&self, line: &str) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.is_none() {
            *st = Self::open(&self.path);
        }
        if st.as_ref().is_some_and(|(_, len)| *len + line.len() as u64 > self.max) {
            *st = None;
            let _ = std::fs::rename(&self.path, rotated(&self.path));
            *st = Self::open(&self.path);
        }
        if let Some((f, len)) = st.as_mut() {
            if f.write_all(line.as_bytes()).is_ok() {
                *len += line.len() as u64;
            }
        }
    }
}

fn rotated(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".1");
    PathBuf::from(s)
}

impl Log for FileLogger {
    fn enabled(&self, m: &Metadata) -> bool {
        let ours = m.target().starts_with("easymail");
        m.level() <= if ours { self.level } else { self.level.min(Level::Warn) }
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let line = format!("[{at}] {:<5} {}: {}\n", r.level(), r.target(), r.args());
        if self.stderr {
            eprint!("{line}");
        }
        self.write_line(&line);
    }

    fn flush(&self) {
        if let Some((f, _)) = self.state.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = f.flush();
        }
    }
}

fn level_from_env() -> Level {
    match std::env::var("EASYMAIL_LOG").unwrap_or_default().to_ascii_lowercase().as_str() {
        "error" => Level::Error,
        "warn" => Level::Warn,
        "debug" => Level::Debug,
        "trace" => Level::Trace,
        _ => Level::Info,
    }
}

/// 安装全局日志后端（重复调用无害）。返回日志文件路径。
pub fn init(dir: &Path) -> PathBuf {
    let path = dir.join("easymail.log");
    let level = level_from_env();
    let logger = FileLogger { path: path.clone(), max: MAX_BYTES, level, state: Mutex::new(None), stderr: cfg!(debug_assertions) };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(level.to_level_filter().max(LevelFilter::Warn));
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_when_full() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        let l = FileLogger { path: path.clone(), max: 100, level: Level::Info, state: Mutex::new(None), stderr: false };
        for i in 0..15 {
            l.write_line(&format!("line {i:02} ..........\n"));
        }
        l.flush();
        let cur = std::fs::read_to_string(&path).unwrap();
        let old = std::fs::read_to_string(rotated(&path)).unwrap();
        assert!(cur.len() <= 100 && old.len() <= 100);
        assert!(cur.contains("line 14") && old.contains("line 09"));
        assert!(!cur.contains("line 00") && !old.contains("line 00"), "更早的内容随轮转丢弃");
    }
}
