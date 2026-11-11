//! macOS 集成：菜单栏图标（托盘）、Dock 未读角标、新邮件通知、开机自启（LaunchAgent）、单实例。
//!
//! 托盘菜单的点击、第二个实例发来的「显示窗口」都写进命令队列，界面每 120ms 取走执行（主线程）。

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Show,
    Compose,
    SyncAll,
    RevealData,
    Quit,
}

static COMMANDS: Mutex<Vec<Command>> = Mutex::new(Vec::new());
static UNREAD: AtomicI64 = AtomicI64::new(-1);

pub fn push(c: Command) {
    COMMANDS.lock().unwrap().push(c);
    notify_ui();
}

type Wake = (futures::channel::mpsc::UnboundedSender<()>, Mutex<Option<futures::channel::mpsc::UnboundedReceiver<()>>>);

fn wake() -> &'static Wake {
    static WAKE: std::sync::OnceLock<Wake> = std::sync::OnceLock::new();
    WAKE.get_or_init(|| {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        (tx, Mutex::new(Some(rx)))
    })
}

/// 原生回调（托盘菜单、WebView 点链接）写入队列后唤醒界面，界面不用定时轮询
pub fn notify_ui() {
    let _ = wake().0.unbounded_send(());
}

/// 界面取走唤醒信号的接收端（只能取一次）
pub fn take_wake_receiver() -> Option<futures::channel::mpsc::UnboundedReceiver<()>> {
    wake().1.lock().unwrap().take()
}

pub fn take_commands() -> Vec<Command> {
    std::mem::take(&mut *COMMANDS.lock().unwrap())
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct TrayTarget;

    unsafe impl NSObjectProtocol for TrayTarget {}

    impl TrayTarget {
        #[unsafe(method(show:))]
        fn show(&self, _s: Option<&AnyObject>) {
            push(Command::Show);
        }
        #[unsafe(method(compose:))]
        fn compose(&self, _s: Option<&AnyObject>) {
            push(Command::Compose);
        }
        #[unsafe(method(sync:))]
        fn sync(&self, _s: Option<&AnyObject>) {
            push(Command::SyncAll);
        }
        #[unsafe(method(reveal:))]
        fn reveal(&self, _s: Option<&AnyObject>) {
            push(Command::RevealData);
        }
        #[unsafe(method(autostart:))]
        fn autostart(&self, _s: Option<&AnyObject>) {
            let on = !autostart_enabled();
            let _ = set_autostart(on);
            refresh_tray();
        }
        #[unsafe(method(quit:))]
        fn quit(&self, _s: Option<&AnyObject>) {
            push(Command::Quit);
        }
    }
);

impl TrayTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

struct Tray {
    item: Retained<NSStatusItem>,
    _target: Retained<TrayTarget>,
    unread_item: Retained<NSMenuItem>,
    autostart_item: Retained<NSMenuItem>,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

fn menu_item(mtm: MainThreadMarker, title: &str, action: Option<objc2::runtime::Sel>, target: &TrayTarget) -> Retained<NSMenuItem> {
    let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), action, &NSString::from_str("")) };
    if action.is_some() {
        unsafe { item.setTarget(Some(target)) };
    }
    item
}

/// 创建菜单栏图标（主线程）
pub fn install_tray() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    if TRAY.with(|t| t.borrow().is_some()) {
        return;
    }
    let target = TrayTarget::new(mtm);
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    let name = crate::core::config::APP_NAME;
    menu.addItem(&menu_item(mtm, &format!("打开{name}"), Some(sel!(show:)), &target));
    menu.addItem(&menu_item(mtm, "写新邮件", Some(sel!(compose:)), &target));
    menu.addItem(&menu_item(mtm, "立即同步全部账号", Some(sel!(sync:)), &target));
    let unread_item = menu_item(mtm, "未读邮件：0", None, &target);
    unread_item.setEnabled(false);
    menu.addItem(&unread_item);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&menu_item(mtm, "在访达中显示数据目录", Some(sel!(reveal:)), &target));
    let autostart_item = menu_item(mtm, "开机自启", Some(sel!(autostart:)), &target);
    menu.addItem(&autostart_item);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&menu_item(mtm, &format!("退出{name}"), Some(sel!(quit:)), &target));

    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        let img = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str("envelope"), Some(&NSString::from_str(name)));
        if let Some(img) = img {
            img.setTemplate(true);
            button.setImage(Some(&img));
        } else {
            button.setTitle(&NSString::from_str("✉"));
        }
    }
    item.setMenu(Some(&menu));
    TRAY.with(|t| *t.borrow_mut() = Some(Tray { item, _target: target, unread_item, autostart_item }));
    refresh_tray();
}

fn refresh_tray() {
    let n = UNREAD.load(Ordering::Relaxed).max(0);
    let auto = autostart_enabled();
    TRAY.with(|t| {
        if let Some(t) = t.borrow().as_ref() {
            t.unread_item.setTitle(&NSString::from_str(&format!("未读邮件：{n}")));
            t.autostart_item.setState(if auto { NSControlStateValueOn } else { NSControlStateValueOff });
            if let Some(mtm) = MainThreadMarker::new() {
                if let Some(b) = t.item.button(mtm) {
                    let title = if n > 0 { format!(" {n}") } else { String::new() };
                    b.setTitle(&NSString::from_str(&title));
                }
            }
        }
    });
}

/// 收件箱未读数 → Dock 角标 + 托盘
pub fn set_unread(n: i64) {
    if UNREAD.swap(n, Ordering::Relaxed) == n {
        return;
    }
    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        let label = if n > 0 { Some(NSString::from_str(&if n > 999 { "999+".to_string() } else { n.to_string() })) } else { None };
        app.dockTile().setBadgeLabel(label.as_deref());
        refresh_tray();
    }
}

pub fn unread() -> i64 {
    UNREAD.load(Ordering::Relaxed)
}

/// 系统通知。打包成 .app 时走 NSUserNotificationCenter，直接 cargo run 时退回 osascript。
pub fn notify(title: &str, body: &str) {
    if std::env::var_os("EASYMAIL_NO_NOTIFY").is_some() {
        return;
    }
    let bundled = objc2_foundation::NSBundle::mainBundle().bundleIdentifier().is_some();
    if bundled {
        #[allow(deprecated)]
        {
            use objc2_foundation::{NSUserNotification, NSUserNotificationCenter};
            let n = NSUserNotification::new();
            n.setTitle(Some(&NSString::from_str(title)));
            n.setInformativeText(Some(&NSString::from_str(body)));
            NSUserNotificationCenter::defaultUserNotificationCenter().deliverNotification(&n);
        }
    } else {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("display notification \"{}\" with title \"{}\"", esc(body), esc(title));
        let _ = std::process::Command::new("osascript").arg("-e").arg(script).spawn();
    }
}

// ───────────────────────────── 开机自启 ─────────────────────────────

pub fn launch_agent_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("Library/LaunchAgents").join(format!("{}.plist", crate::core::config::BUNDLE_ID))
}

pub fn autostart_enabled() -> bool {
    launch_agent_path().exists()
}

pub fn launch_agent_plist(exe: &Path) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{}</string>
  <key>ProgramArguments</key>
  <array><string>{}</string><string>--background</string></array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#,
        crate::core::config::BUNDLE_ID,
        esc(&exe.to_string_lossy())
    )
}

pub fn set_autostart(on: bool) -> anyhow::Result<()> {
    let p = launch_agent_path();
    if on {
        let exe = std::env::current_exe()?;
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, launch_agent_plist(&exe))?;
    } else if p.exists() {
        std::fs::remove_file(&p)?;
    }
    Ok(())
}

// ───────────────────────────── 单实例 ─────────────────────────────

/// 已有实例在运行：通知它显示窗口并返回 false；否则开始监听并返回 true。
pub fn single_instance(dir: &Path) -> bool {
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    let sock = dir.join("easymail.sock");
    if let Ok(mut s) = UnixStream::connect(&sock) {
        let _ = s.write_all(b"show\n");
        return false;
    }
    let _ = std::fs::remove_file(&sock);
    match UnixListener::bind(&sock) {
        Ok(l) => {
            std::thread::Builder::new()
                .name("single-instance".into())
                .spawn(move || {
                    for mut s in l.incoming().flatten() {
                        let mut buf = [0u8; 16];
                        let _ = s.read(&mut buf);
                        push(Command::Show);
                    }
                })
                .ok();
            true
        }
        Err(_) => true,
    }
}

/// 剪贴板里的文件（访达里 ⌘C 复制的文件）：GPUI 只会读到文件名文本，这里直接读 `public.file-url`。
pub fn clipboard_file_paths() -> Vec<PathBuf> {
    unsafe {
        let Some(cls) = objc2::runtime::AnyClass::get(c"NSPasteboard") else { return vec![] };
        let pb: *mut AnyObject = msg_send![cls, generalPasteboard];
        if pb.is_null() {
            return vec![];
        }
        let items: *mut AnyObject = msg_send![pb, pasteboardItems];
        if items.is_null() {
            return vec![];
        }
        let n: usize = msg_send![items, count];
        let ty = NSString::from_str("public.file-url");
        let mut out = Vec::new();
        for i in 0..n {
            let it: *mut AnyObject = msg_send![items, objectAtIndex: i];
            let s: Option<Retained<NSString>> = msg_send![it, stringForType: &*ty];
            // 访达给的常是文件引用 URL（file:///.file/id=…），必须经 NSURL 解析成真实路径
            let Some(s) = s else { continue };
            let u: Option<Retained<objc2_foundation::NSURL>> = objc2_foundation::NSURL::URLWithString(&s);
            let path: Option<Retained<NSString>> = u.and_then(|u| u.filePathURL()).and_then(|u| u.path());
            if let Some(p) = path {
                out.push(PathBuf::from(p.to_string()));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_and_queue() {
        let p = launch_agent_plist(Path::new("/Applications/EasyMail.app/Contents/MacOS/easymail"));
        assert!(p.contains("<string>app.easymail</string>"));
        assert!(p.contains("<string>--background</string>"));
        assert!(p.contains("/Applications/EasyMail.app/Contents/MacOS/easymail"));
        push(Command::SyncAll);
        assert!(take_commands().contains(&Command::SyncAll));
    }

    #[test]
    fn single_instance_handoff() {
        let dir = tempfile::tempdir().unwrap();
        assert!(single_instance(dir.path()));
        take_commands();
        assert!(!single_instance(dir.path()), "第二个实例应当让位");
        for _ in 0..50 {
            if take_commands().contains(&Command::Show) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("第一个实例没有收到 show");
    }
}
