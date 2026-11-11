//! 界面端到端测试：GreenMail 提供真实 IMAP / SMTP，启动真正的 GPUI 窗口，
//! 由应用内的 `EASYMAIL_SCRIPT=e2e` 场景驱动按键与操作（见 src/ui/autotest.rs）。
//!
//! 需要图形会话（本机桌面）。GreenMail 的拉起 / 重置 / 测试账号见 tests/common/mod.rs。

use easymail::core::db::Store;
use easymail::core::imap::Imap;
use easymail::core::{accounts, smtp};
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

mod common;
use common::{account, ensure_greenmail, reset};

fn deliver(rt: &tokio::runtime::Runtime, subject: &str, body: &str, day: u32) {
    let raw = format!(
        "From: Bob <bob@local.test>\r\nTo: me@local.test\r\nSubject: {subject}\r\nMessage-ID: <{}@local.test>\r\nDate: Thu, {day:02} Nov 2025 10:00:00 +0800\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n",
        uuid::Uuid::new_v4()
    );
    rt.block_on(smtp::send(&account("bob@local.test"), "pw", None, &["me@local.test".to_string()], raw.as_bytes())).unwrap();
}

#[test]
fn ui_end_to_end() {
    if !ensure_greenmail() {
        return;
    }
    reset();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let mut c = Imap::connect(&account("me@local.test"), "pw", None).await.unwrap();
        for f in ["Trash", "Sent", "Junk"] {
            c.create_folder(f).await.unwrap();
        }
        c.logout().await;
    });
    deliver(&rt, "要删除的邮件", "这封会被删除。正文。", 3);
    deliver(&rt, "发票 2025-11", "本月发票见附件。正文。", 4);
    deliver(&rt, "周五的项目会议", "大家好：\n周五下午三点开会。\n这是正文，带一个链接 https://example.com/agenda", 6);

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    {
        let store = Store::open(&data).unwrap();
        accounts::create(&store, &account("me@local.test"), "pw", "我", "-- 测试签名").unwrap();
        accounts::ensure_local(&store).unwrap();
    }
    let shots = std::path::PathBuf::from(std::env::var("SHOT_DIR").unwrap_or_else(|_| concat!(env!("CARGO_TARGET_TMPDIR"), "/ui-shots").into()));
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_easymail"))
        .env("EASYMAIL_DATA_DIR", &data)
        .env("EASYMAIL_SCRIPT", "e2e")
        .env("EASYMAIL_FORCE_DRAW", "1")
        .env("EASYMAIL_NO_NOTIFY", "1")
        .env("EASYMAIL_IMPORT_FROM", dir.path().join("none"))
        .env("SHOT_DIR", &shots)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("启动 easymail");
    let out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for l in BufReader::new(out).lines().map_while(Result::ok) {
            println!("{l}");
            let _ = tx.send(l);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut result = None;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(l) if l == "READY_FOR_NEWMAIL" => {
                std::thread::sleep(Duration::from_millis(500));
                deliver(&rt, "实时到达的新邮件", "IDLE 推送过来的。正文。", 7);
            }
            Ok(l) if l.starts_with("RESULT") => {
                result = Some(l);
                break;
            }
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(Some(st)) = child.try_wait() {
                    panic!("easymail 提前退出：{st}");
                }
            }
            Err(_) => break,
        }
    }
    let _ = child.wait_timeout_kill();
    let r = result.expect("超时：没有拿到 RESULT");
    println!("截图目录：{}", shots.display());
    assert_eq!(r, "RESULT PASS", "{r}");
}

trait WaitKill {
    fn wait_timeout_kill(&mut self) -> std::io::Result<()>;
}

impl WaitKill for std::process::Child {
    fn wait_timeout_kill(&mut self) -> std::io::Result<()> {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) {
            if self.try_wait()?.is_some() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.kill()
    }
}
