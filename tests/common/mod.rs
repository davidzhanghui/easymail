//! 端到端测试共用：拉起 / 重置本机 GreenMail，构造测试账号。
//!
//! GreenMail 没在跑时用 tools/greenmail-standalone.jar 拉起（jar 用 scripts/fetch-greenmail.sh 下载）。
//! 缺 java / jar 时默认跳过并打印原因；设置 `EASYMAIL_REQUIRE_GREENMAIL=1`（scripts/check.sh、CI）时直接失败，
//! 不让「环境不全」伪装成「测试通过」。

#![allow(dead_code)]

use easymail::core::model::Account;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

pub const IMAP: u16 = 3143;
pub const SMTP: u16 = 3025;
const API: u16 = 8080;

/// 同一个测试进程里的多个用例共用一台 GreenMail（`reset` 会清空所有邮箱），必须串行
pub fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn port_open(p: u16) -> bool {
    TcpStream::connect_timeout(&format!("127.0.0.1:{p}").parse().unwrap(), Duration::from_millis(300)).is_ok()
}

fn unavailable(why: &str) -> bool {
    if std::env::var_os("EASYMAIL_REQUIRE_GREENMAIL").is_some() {
        panic!("GreenMail 不可用：{why}（设置了 EASYMAIL_REQUIRE_GREENMAIL，不允许跳过）");
    }
    eprintln!("跳过：GreenMail 不可用：{why}");
    false
}

/// GreenMail 可用返回 true；不可用时按 `EASYMAIL_REQUIRE_GREENMAIL` 决定跳过还是失败。
pub fn ensure_greenmail() -> bool {
    if port_open(IMAP) && port_open(API) {
        return true;
    }
    let jar = concat!(env!("CARGO_MANIFEST_DIR"), "/tools/greenmail-standalone.jar");
    if !std::path::Path::new(jar).exists() {
        return unavailable(&format!("没有 {jar}，先执行 scripts/fetch-greenmail.sh"));
    }
    let spawned = std::process::Command::new("java")
        .args(["-Dgreenmail.setup.test.all", "-Dgreenmail.users=me:pw@local.test,bob:pw@local.test", "-Dgreenmail.users.login=email", "-jar", jar])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Err(e) = spawned {
        return unavailable(&format!("无法启动 java：{e}"));
    }
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(30) {
        if port_open(IMAP) && port_open(SMTP) && port_open(API) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    unavailable("30 秒内没有启动完成")
}

/// 清空 GreenMail 上的所有邮件与文件夹
pub fn reset() {
    let mut s = TcpStream::connect(("127.0.0.1", API)).unwrap();
    s.write_all(b"POST /api/service/reset HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    assert!(out.contains("200"), "reset: {out}");
}

pub fn account(email: &str) -> Account {
    Account {
        email: email.into(),
        name: email.split('@').next().unwrap().into(),
        provider: "custom".into(),
        imap_host: "127.0.0.1".into(),
        imap_port: IMAP,
        smtp_host: "127.0.0.1".into(),
        smtp_port: SMTP,
        smtp_secure: false,
        sync_enabled: true,
        ..Default::default()
    }
}
