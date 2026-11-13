//! 写信的富文本正文编辑器：在 GPUI 窗口上叠加一个 WKWebView，里面是 contenteditable（`editor.html`）。
//!
//! 和 Gmail 一样走 HTML：加粗 / 斜体 / 下划线 / 字体字号 / 颜色 / 对齐 / 列表 / 引用 / 链接 / 表情 / 图片，
//! 工具栏、链接对话框、表情面板都画在页面里（原生视图盖在 GPUI 之上，页内弹层不会被遮挡）。
//!
//! 与 `webview.rs`（阅读邮件，禁用脚本）分开：这里的 WebView 开启脚本，但只加载应用自己生成的文档——
//! CSP 只允许带随机 nonce 的那一段脚本，用户内容（含粘贴进来的 HTML）都经过白名单清洗后才进入 DOM。
//! 页面通过 `window.webkit.messageHandlers.em` 把事件（内容变化 / 发送 / 粘贴文件…）以 JSON 回传，
//! 这里解析后放进队列，由写信窗口轮询取走。
//!
//! 所有 WebKit 调用都在主线程（GPUI 的前台执行器就是主线程）。

use crate::webview::{eval_of, mtm, parent_view, snapshot_of};
use base64::Engine as _;
use gpui::{AsyncApp, Bounds, Hsla, Pixels, Window};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSView;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSNumber, NSObject, NSObjectNSKeyValueCoding, NSObjectProtocol, NSString};
use objc2_web_kit::{
    WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationType, WKScriptMessage, WKScriptMessageHandler, WKUserContentController,
    WKWebView, WKWebViewConfiguration,
};
use std::cell::RefCell;
use std::sync::Mutex;

/// 页面里粘贴 / 拖入的文件
#[derive(Debug, Clone)]
pub struct FileItem {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// 页面脚本就绪，可以发 `run_js`
    Ready,
    /// 正文变了（已序列化的 HTML，内嵌图片只带 `data-token`）
    Changed(String),
    Send,
    Cancel,
    Tab,
    BackTab,
    /// 编辑器拿到了键盘焦点（收件人等输入框要失焦）
    Focus,
    /// 点了工具栏的「插入图片」
    PickImage,
    Files(Vec<FileItem>),
    Toast(String),
}

static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());

pub fn take_events() -> Vec<Event> {
    std::mem::take(&mut *EVENTS.lock().unwrap())
}

fn parse_event(s: &str) -> Option<Event> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let str_of = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(match v.get("t")?.as_str()? {
        "ready" => Event::Ready,
        "changed" => Event::Changed(str_of("html")),
        "send" => Event::Send,
        "cancel" => Event::Cancel,
        "tab" => Event::Tab,
        "backtab" => Event::BackTab,
        "focus" => Event::Focus,
        "pickImage" => Event::PickImage,
        "toast" => Event::Toast(str_of("msg")),
        "files" => {
            let items = v.get("items")?.as_array()?;
            let files = items
                .iter()
                .filter_map(|it| {
                    let data = base64::engine::general_purpose::STANDARD.decode(it.get("data")?.as_str()?).ok()?;
                    Some(FileItem {
                        name: it.get("name")?.as_str()?.to_string(),
                        mime: it.get("mime").and_then(|m| m.as_str()).unwrap_or("").to_string(),
                        data,
                    })
                })
                .collect();
            Event::Files(files)
        }
        _ => return None,
    })
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct MsgHandler;

    unsafe impl NSObjectProtocol for MsgHandler {}

    unsafe impl WKScriptMessageHandler for MsgHandler {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn did_receive(&self, _ucc: &WKUserContentController, message: &WKScriptMessage) {
            let body = unsafe { message.body() };
            let Some(s) = body.downcast_ref::<NSString>() else { return };
            if let Some(ev) = parse_event(&s.to_string()) {
                EVENTS.lock().unwrap().push(ev);
            }
        }
    }
);

impl MsgHandler {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct NavDelegate;

    unsafe impl NSObjectProtocol for NavDelegate {}

    unsafe impl WKNavigationDelegate for NavDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(&self, _web: &WKWebView, action: &WKNavigationAction, handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>) {
            // 只放行 loadHTMLString 的首次加载；任何别的导航（点链接、拖入文件、脚本跳转）一律取消
            let ty = unsafe { action.navigationType() };
            let url = unsafe { action.request().URL() }.and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
            let internal = url.is_empty() || url.starts_with("about:");
            if internal && ty == WKNavigationType::Other {
                handler.call((WKNavigationActionPolicy::Allow,));
            } else {
                handler.call((WKNavigationActionPolicy::Cancel,));
            }
        }
    }
);

impl NavDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

struct Live {
    key: u64,
    view: Retained<WKWebView>,
    _nav: Retained<NavDelegate>,
    _msg: Retained<MsgHandler>,
    parent: Retained<NSView>,
    frozen: bool,
    frame: CGRect,
}

thread_local! {
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
}

fn make_view() -> (Retained<WKWebView>, Retained<NavDelegate>, Retained<MsgHandler>) {
    let mtm = mtm();
    unsafe {
        let config = WKWebViewConfiguration::new(mtm);
        config.setWebsiteDataStore(&objc2_web_kit::WKWebsiteDataStore::nonPersistentDataStore(mtm));
        let handler = MsgHandler::new(mtm);
        config.userContentController().addScriptMessageHandler_name(ProtocolObject::from_ref(&*handler), &NSString::from_str("em"));
        let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(400., 300.));
        let view = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config);
        let nav = NavDelegate::new(mtm);
        view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*nav)));
        // 背景交给页面自己画（跟随主题）
        view.setValue_forKey(Some(&NSNumber::new_bool(false)), &NSString::from_str("drawsBackground"));
        (view, nav, handler)
    }
}

/// 显示（或更新位置）。`key` 变化表示换了一封信，需要重新加载文档。
pub fn show(window: &Window, bounds: Bounds<Pixels>, key: u64, document: &str) {
    let Some(parent) = parent_view(window) else { return };
    let h = parent.bounds().size.height;
    let frame = CGRect::new(
        CGPoint::new(f64::from(bounds.origin.x), h - f64::from(bounds.origin.y) - f64::from(bounds.size.height)),
        CGSize::new(f64::from(bounds.size.width).max(1.), f64::from(bounds.size.height).max(1.)),
    );
    LIVE.with(|l| {
        let mut l = l.borrow_mut();
        if l.is_none() {
            let (view, nav, msg) = make_view();
            parent.addSubview(&view);
            let parent = unsafe { Retained::retain(parent as *const NSView as *mut NSView) }.expect("ns_view");
            view.setFrame(frame);
            *l = Some(Live { key: !key, view, _nav: nav, _msg: msg, parent, frozen: false, frame });
        }
        let live = l.as_mut().unwrap();
        // 写信挪到独立窗口（或从一个窗口换到另一个）时，把原生编辑器搬过去
        if !std::ptr::eq(&*live.parent, parent) {
            live.view.removeFromSuperview();
            parent.addSubview(&live.view);
            live.parent = unsafe { Retained::retain(parent as *const NSView as *mut NSView) }.expect("ns_view");
            live.frame = CGRect::new(CGPoint::new(-1., -1.), CGSize::new(0., 0.));
            // 强制下面重设位置
        }
        if live.key != key {
            live.key = key;
            EVENTS.lock().unwrap().clear();
            unsafe { live.view.loadHTMLString_baseURL(&NSString::from_str(document), None) };
        }
        if live.frame != frame {
            live.frame = frame;
            live.view.setFrame(frame);
        }
        live.view.setHidden(live.frozen);
    });
}

/// 关闭编辑器（写信窗口关了）。没有编辑器时什么也不做，可每帧调用。
pub fn hide() {
    LIVE.with(|l| {
        if let Some(live) = l.borrow_mut().take() {
            unsafe {
                live.view.configuration().userContentController().removeScriptMessageHandlerForName(&NSString::from_str("em"));
            }
            live.view.removeFromSuperview();
            if let Some(win) = live.parent.window() {
                let _ = win.makeFirstResponder(Some(&live.parent));
            }
            EVENTS.lock().unwrap().clear();
        }
    });
}

/// 只关掉属于 `key` 的编辑器（写信窗口释放时调用，避免误关新开的那个）
pub fn hide_key(key: u64) {
    if LIVE.with(|l| l.borrow().as_ref().is_some_and(|x| x.key == key)) {
        hide();
    }
}

/// 关了写信窗口：编辑器若正挂在这个窗口上就释放掉（避免视图悬空在已销毁的窗口里）
pub fn hide_key_for_window(window: &Window) {
    let Some(parent) = crate::webview::parent_view(window) else { return };
    let same = LIVE.with(|l| l.borrow().as_ref().is_some_and(|x| std::ptr::eq(&*x.parent, parent)));
    if same {
        hide();
    }
}

pub fn is_live() -> bool {
    LIVE.with(|l| l.borrow().is_some())
}

pub fn is_frozen() -> bool {
    LIVE.with(|l| l.borrow().as_ref().is_some_and(|x| x.frozen))
}

/// 冻结：隐藏原生视图（由调用方在原位置画快照），让 GPUI 的下拉 / 弹层能显示出来。
pub fn set_frozen(frozen: bool) {
    LIVE.with(|l| {
        if let Some(live) = l.borrow_mut().as_mut() {
            if live.frozen != frozen {
                live.frozen = frozen;
                live.view.setHidden(frozen);
                if frozen {
                    if let Some(win) = live.parent.window() {
                        let _ = win.makeFirstResponder(Some(&live.parent));
                    }
                }
            }
        }
    });
}

fn view() -> Option<Retained<WKWebView>> {
    LIVE.with(|l| l.borrow().as_ref().map(|x| x.view.clone()))
}

pub async fn snapshot(cx: &AsyncApp) -> Option<Vec<u8>> {
    snapshot_of(view()?, cx).await
}

/// 把键盘焦点交给编辑器（原生第一响应者）。
pub fn focus_native() {
    LIVE.with(|l| {
        if let Some(live) = l.borrow().as_ref() {
            if let Some(win) = live.view.window() {
                let _ = win.makeFirstResponder(Some(&live.view));
            }
        }
    });
}

/// 把键盘焦点还给 GPUI 的内容视图（点收件人 / 主题等输入框、弹窗时）。
pub fn resign() {
    LIVE.with(|l| {
        if let Some(live) = l.borrow().as_ref() {
            if let Some(win) = live.parent.window() {
                let _ = win.makeFirstResponder(Some(&live.parent));
            }
        }
    });
}

/// 执行一段页面脚本，不等结果。
pub fn run_js(js: &str) {
    if let Some(v) = view() {
        unsafe { v.evaluateJavaScript_completionHandler(&NSString::from_str(js), None) };
    }
}

/// 执行脚本并取回 `String(结果)`。测试 / 需要读页面状态时用。
pub async fn eval(cx: &AsyncApp, js: &str) -> Option<String> {
    eval_of(view()?, cx, js).await
}

/// 测试用：向窗口投递一个原生按键事件（走真实的 AppKit 响应者链）。
#[cfg(feature = "dev")]
pub fn test_key(code: u16, chars: &str) {
    use objc2::runtime::AnyObject;
    let Some(win) = LIVE.with(|l| l.borrow().as_ref().and_then(|x| x.view.window())) else { return };
    let ch = NSString::from_str(chars);
    unsafe {
        let cls = objc2::class!(NSEvent);
        let ev: Option<Retained<AnyObject>> = msg_send![cls,
            keyEventWithType: 10usize, location: CGPoint::new(0., 0.), modifierFlags: if chars.starts_with('\u{F700}') || chars.starts_with('\u{F701}') || chars.starts_with('\u{F702}') || chars.starts_with('\u{F703}') { 0xA00000usize } else { 0usize }, timestamp: 0f64,
            windowNumber: win.windowNumber(), context: std::ptr::null::<AnyObject>(),
            characters: &*ch, charactersIgnoringModifiers: &*ch, isARepeat: false, keyCode: code];
        if let Some(ev) = ev {
            let _: () = msg_send![&*win, sendEvent: &*ev];
        }
    }
}

/// 测试用：在编辑器视图内 (x, y)（相对视图左上角，点）处投递一次真实的鼠标点击。
#[cfg(feature = "dev")]
pub fn test_click(x: f64, y: f64) {
    use objc2::runtime::AnyObject;
    let Some((win, frame)) = LIVE.with(|l| l.borrow().as_ref().and_then(|v| v.view.window().map(|w| (w, v.view.frame())))) else { return };
    let (wx, wy) = (frame.origin.x + x, frame.origin.y + frame.size.height - y);
    for ty in [1usize, 2] {
        unsafe {
            let cls = objc2::class!(NSEvent);
            let ev: Option<Retained<AnyObject>> = msg_send![cls,
                mouseEventWithType: ty, location: CGPoint::new(wx, wy), modifierFlags: 0usize, timestamp: 0f64,
                windowNumber: win.windowNumber(), context: std::ptr::null::<AnyObject>(),
                eventNumber: 1isize, clickCount: 1isize, pressure: 1f32];
            if let Some(ev) = ev {
                let _: () = msg_send![&*win, sendEvent: &*ev];
            }
        }
    }
}

/// JS 字符串字面量（带引号、已转义），拼进脚本用。
pub fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

// ───────────────────────────── 文档 ─────────────────────────────

fn css(c: Hsla) -> String {
    let c = c.to_rgb();
    format!("rgba({},{},{},{:.3})", (c.r * 255.).round() as u8, (c.g * 255.).round() as u8, (c.b * 255.).round() as u8, c.a)
}

/// 页面配色（取自当前主题）
pub struct Colors {
    pub fg: Hsla,
    pub bg: Hsla,
    pub muted: Hsla,
    pub border: Hsla,
    pub accent: Hsla,
    pub accent_soft: Hsla,
    pub panel: Hsla,
    pub panel2: Hsla,
    pub hover: Hsla,
    /// 暗色主题：引用的原邮件改用主题色显示（只影响编辑器里的显示，不改发出去的 HTML）
    pub dark: bool,
}

/// 生成编辑器文档。`body_html` 必须是已经可信的 HTML（草稿正文，或纯文本经转义转换而来）。
pub fn document(body_html: &str, quote: Option<(&str, bool)>, placeholder: &str, c: &Colors) -> String {
    let vars = format!(
        "--fg:{};--bg:{};--muted:{};--border:{};--accent:{};--accent-soft:{};--panel:{};--panel2:{};--hover:{};",
        css(c.fg),
        css(c.bg),
        css(c.muted),
        css(c.border),
        css(c.accent),
        css(c.accent_soft),
        css(c.panel),
        css(c.panel2),
        css(c.hover)
    );
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    // 原邮件 HTML：放进不可执行的 JSON 数据块，由页面脚本在惰性文档里解析、白名单清洗后再接到正文末尾
    let quote_block = quote
        .map(|(html, bq)| {
            let j = serde_json::json!({ "html": html, "bq": bq }).to_string().replace('<', "\\u003c").replace('>', "\\u003e").replace('&', "\\u0026");
            format!("<script type=\"application/json\" id=\"quote\">{j}</script>")
        })
        .unwrap_or_default();
    include_str!("editor.html")
        .replace("__NONCE__", &nonce)
        .replace("__VARS__", &vars)
        .replace("__THEME__", if c.dark { "dark" } else { "light" })
        .replace("__PLACEHOLDER__", &crate::webview::escape_html(placeholder))
        .replace("__QUOTE__", &quote_block)
        // 正文放最后替换：它可能含有上面的占位串
        .replace("__BODY__", body_html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_parse() {
        assert!(matches!(parse_event(r#"{"t":"changed","html":"<div>x</div>"}"#), Some(Event::Changed(h)) if h == "<div>x</div>"));
        assert!(matches!(parse_event(r#"{"t":"send"}"#), Some(Event::Send)));
        assert!(parse_event(r#"{"t":"nope"}"#).is_none());
        // "aGk=" = hi
        match parse_event(r#"{"t":"files","items":[{"name":"a.png","mime":"image/png","data":"aGk="}]}"#) {
            Some(Event::Files(f)) => assert_eq!((f[0].name.as_str(), f[0].data.as_slice()), ("a.png", b"hi".as_slice())),
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn document_marks_theme() {
        let c = |dark| Colors {
            fg: gpui::black(),
            bg: gpui::white(),
            muted: gpui::black(),
            border: gpui::black(),
            accent: gpui::black(),
            accent_soft: gpui::black(),
            panel: gpui::white(),
            panel2: gpui::white(),
            hover: gpui::white(),
            dark,
        };
        assert!(document("", None, "", &c(true)).contains(r#"<html data-theme="dark">"#));
        assert!(document("", None, "", &c(false)).contains(r#"<html data-theme="light">"#));
    }

    #[test]
    fn js_string_escapes() {
        assert_eq!(js_str("a\"b\n</script>"), "\"a\\\"b\\n</script>\"");
    }
}
