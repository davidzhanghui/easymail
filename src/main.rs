//! EasyMail · 迷你邮件 —— 本地优先的多账号邮件客户端（Rust + GPUI，macOS）。

use easymail::core::config::{default_data_dir, APP_NAME, BUNDLE_ID};
use easymail::core::db::Store;
use easymail::core::engine::Engine;
use easymail::core::{accounts, mailer};
use easymail::ui::{self, app::MailApp, macos};
use gpui::*;
use std::path::Path;

/// 把 panic 信息落到数据目录的 panic.log（双击打开的 .app 没有终端接住 stderr）。
fn install_panic_log(dir: &Path) {
    let path = dir.join("panic.log");
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let thread = std::thread::current();
        let loc = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(非字符串 panic)".into());
        let bt = std::backtrace::Backtrace::force_capture();
        let body = format!("[{at}] thread={}\n位置: {loc}\n消息: {msg}\n\n{bt}\n{}\n", thread.name().unwrap_or("unnamed"), "-".repeat(70));
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = f.write_all(body.as_bytes());
        }
        default_hook(info);
    }));
}

/// 启动早期（GPUI 还没起来）的原生提示框，返回用户点的按钮。双击打开的 .app 没有终端，eprintln 用户看不到。
fn alert(title: &str, msg: &str, buttons: &[&str]) -> Option<String> {
    if std::env::var_os("EASYMAIL_SCRIPT").is_some() {
        return None; // 自动化测试里不弹框
    }
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let list = buttons.iter().map(|b| format!("\"{}\"", esc(b))).collect::<Vec<_>>().join(", ");
    let script = format!(
        "button returned of (display alert \"{}\" message \"{}\" as critical buttons {{{list}}} default button \"{}\")",
        esc(title),
        esc(msg),
        esc(buttons.last().copied().unwrap_or("好"))
    );
    let out = std::process::Command::new("osascript").args(["-e", &script]).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let dir = default_data_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("无法创建数据目录 {}: {e}", dir.display());
        std::process::exit(1);
    }
    install_panic_log(&dir);
    // 上次预览附件留下的临时文件
    let _ = std::fs::remove_dir_all(easymail::ui::app::preview_dir());
    easymail::core::logging::init(&dir);
    log::info!("启动 {} {}，数据目录 {}", APP_NAME, easymail::core::config::VERSION, dir.display());
    let background = std::env::args().any(|a| a == "--background");
    let script = std::env::var("EASYMAIL_SCRIPT").ok();
    if script.is_none() && !macos::single_instance(&dir) {
        eprintln!("{APP_NAME} 已在运行，已切换到已有窗口");
        return;
    }
    let store = match Store::open(&dir) {
        Ok(s) => s,
        Err(e) => {
            log::error!("无法打开数据库: {e:#}");
            eprintln!("无法打开数据库: {e:#}");
            let msg = format!("{e:#}\n\n数据目录：{}\n日志：easymail.log", dir.display());
            if alert(&format!("{APP_NAME} 无法打开数据库"), &msg, &["打开数据目录", "退出"]).as_deref() == Some("打开数据目录") {
                let _ = std::process::Command::new("open").arg(&dir).status();
            }
            std::process::exit(1);
        }
    };
    let _ = accounts::ensure_local(&store);
    // 首次启动探测本机代理（v2ray / Clash / Surge 常见端口），探测到就写进设置
    if std::env::var_os("EASYMAIL_SCRIPT").is_none() {
        easymail::core::tools::init_proxy(&store);
    }
    {
        let s = store.clone();
        std::thread::spawn(move || {
            mailer::cleanup_uploads(&s);
            // 升级前的老邮件补建中文二元组索引（每批 500 封，补完前两字词搜索走 LIKE）
            while let Ok(n) = s.backfill_bigrams(500) {
                if n == 0 {
                    break;
                }
            }
            match easymail::core::messages::sweep_orphan_eml(&s) {
                Ok(n) if n > 0 => log::info!("清理了 {n} 个孤儿 .eml"),
                Ok(_) => {}
                Err(e) => log::warn!("清理孤儿 .eml 失败：{e:#}"),
            }
        });
    }
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let eng = Engine::new(store, tx).expect("engine");
    if std::env::var_os("EASYMAIL_NO_SYNC").is_none() {
        eng.start();
    }

    let app = Application::new().with_assets(ui::Assets);
    app.on_reopen(|cx| {
        cx.activate(true);
        if let Some(w) = cx.windows().first() {
            w.update(cx, |_, window, _| window.activate_window()).ok();
        }
    });
    app.run(move |cx: &mut App| {
        ui::bind_keys(cx);
        ui::composer::bind_keys(cx);
        macos::init_notifications();
        cx.set_menus(ui::menus());
        cx.on_action(|_: &ui::Quit, cx: &mut App| cx.quit());
        cx.on_action(|_: &ui::HideWindow, cx: &mut App| cx.hide());
        cx.on_action(|_: &ui::About, cx: &mut App| {
            cx.open_url("https://github.com/davidzhanghui/easymail");
        });
        let testing = script.is_some();
        let bounds = Bounds::centered(None, size(px(1400.), px(880.)), cx);
        let rx = std::cell::RefCell::new(Some(rx));
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some(APP_NAME.into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(18.), px(24.))),
                    }),
                    window_min_size: Some(size(px(1000.), px(600.))),
                    app_id: Some(BUNDLE_ID.into()),
                    kind: if testing { WindowKind::Floating } else { WindowKind::Normal },
                    ..Default::default()
                },
                |window, cx| {
                    let rx = rx.borrow_mut().take().expect("rx");
                    cx.new(|cx| MailApp::new(eng.clone(), rx, window, cx))
                },
            )
            .expect("open window");
        window
            .update(cx, |app, _, cx| {
                if !testing {
                    app.startup_checks(cx);
                }
            })
            .ok();
        window
            .update(cx, |_, window, cx| {
                // 关窗口 = 隐藏到菜单栏，后台继续收信；退出走菜单 / 托盘 / ⌘Q
                window.on_window_should_close(cx, move |_, cx| {
                    if testing {
                        return true;
                    }
                    cx.hide();
                    false
                });
            })
            .ok();
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if !testing {
            macos::install_tray();
        }
        if background {
            cx.hide();
        } else {
            cx.activate(true);
        }
        #[cfg(feature = "dev")]
        if let Some(path) = script {
            ui::autotest::run(path, window, cx);
        }
    });
}
