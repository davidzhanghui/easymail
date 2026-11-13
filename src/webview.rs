//! 邮件正文视图：在 GPUI 窗口上叠加一个真实的 WKWebView（方案 A）。
//!
//! - 正文区域固定位置，WebView 自己滚动；GPUI 每帧把正文区域的窗口坐标同步过来（`show`）。
//! - 内容禁用 JavaScript（`allowsContentJavaScript = false`），远程资源由文档里的 CSP 拦截，
//!   效果等同「无脚本沙箱 iframe + CSP」。
//! - 点链接：导航代理拦下来交给应用（浏览器打开 / mailto 写信），WebView 本身永远不跳转。
//! - 原生视图压在 GPUI 画布之上，GPUI 的弹层 / 模态会被它盖住：有弹层时先截一张快照，
//!   隐藏 WebView，由 GPUI 在原位置画快照（`freeze` / `thaw`），用户看不出差别。
//!
//! 所有 WebKit 调用都在主线程（GPUI 的前台执行器就是主线程）。

use block2::RcBlock;
use gpui::{AsyncApp, Bounds, Pixels, Window};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::DefinedClass;
use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage, NSView};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSData, NSDictionary, NSError, NSNumber, NSObject, NSObjectNSKeyValueCoding, NSObjectProtocol, NSString};
use objc2_web_kit::{
    WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationType, WKSnapshotConfiguration, WKWebView, WKWebViewConfiguration,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 用户在正文里点了的链接（导航代理写入，界面每帧取走）。
static CLICKED: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());

/// 主窗口的窗口编号；其他窗口（单独打开的阅读窗口）用 GPUI 的窗口 id
pub const PRIMARY: u64 = 0;

pub fn take_clicked() -> Vec<String> {
    take_clicked_in(PRIMARY)
}

pub fn take_clicked_in(wid: u64) -> Vec<String> {
    let mut all = CLICKED.lock().unwrap();
    let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut *all).into_iter().partition(|(w, _)| *w == wid);
    *all = rest;
    mine.into_iter().map(|(_, u)| u).collect()
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = u64]
    struct NavDelegate;

    unsafe impl NSObjectProtocol for NavDelegate {}

    unsafe impl WKNavigationDelegate for NavDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(&self, _web: &WKWebView, action: &WKNavigationAction, handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>) {
            let ty = unsafe { action.navigationType() };
            let url = unsafe { action.request().URL() }.and_then(|u| u.absoluteString()).map(|s| s.to_string()).unwrap_or_default();
            // 首次 loadHTMLString 是 about:blank / Other，放行。
            // 只有用户真的点了链接才交给应用（浏览器打开 / 写信）；<meta refresh>、表单提交等
            // 不是用户点击引起的外部导航一律静默取消，否则一封信打开时就能自己拉起浏览器（确认已读 / 钓鱼）。
            let internal = url.is_empty() || url.starts_with("about:") || url.starts_with("data:");
            if ty == WKNavigationType::LinkActivated {
                if !url.is_empty() && !url.starts_with("about:") {
                    CLICKED.lock().unwrap().push((*self.ivars(), url));
                    crate::ui::macos::notify_ui();
                }
                handler.call((WKNavigationActionPolicy::Cancel,));
            } else if internal && ty == WKNavigationType::Other {
                handler.call((WKNavigationActionPolicy::Allow,));
            } else {
                handler.call((WKNavigationActionPolicy::Cancel,));
            }
        }
    }
);

impl NavDelegate {
    fn new(mtm: MainThreadMarker, wid: u64) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(wid);
        unsafe { msg_send![super(this), init] }
    }
}

struct Live {
    key: u64,
    view: Retained<WKWebView>,
    _delegate: Retained<NavDelegate>,
    parent: Retained<NSView>,
    frozen: bool,
    frame: CGRect,
}

thread_local! {
    static LIVE: RefCell<std::collections::HashMap<u64, Live>> = RefCell::new(std::collections::HashMap::new());
}

pub(crate) fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("WebKit 只能在主线程使用")
}

fn make_view(wid: u64) -> (Retained<WKWebView>, Retained<NavDelegate>) {
    let mtm = mtm();
    unsafe {
        let config = WKWebViewConfiguration::new(mtm);
        // 非持久化数据仓：放行远程图片后服务器种下的 cookie / 缓存只活在内存里，不会跨邮件、跨重启把你认出来
        config.setWebsiteDataStore(&objc2_web_kit::WKWebsiteDataStore::nonPersistentDataStore(mtm));
        config.defaultWebpagePreferences().setAllowsContentJavaScript(false);
        let prefs = config.preferences();
        prefs.setJavaScriptCanOpenWindowsAutomatically(false);
        let frame = CGRect::new(CGPoint::new(0., 0.), CGSize::new(400., 300.));
        let view = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config);
        let delegate = NavDelegate::new(mtm, wid);
        view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // 背景交给文档自己画（HTML 邮件白底，纯文本跟随主题）
        view.setValue_forKey(Some(&NSNumber::new_bool(false)), &NSString::from_str("drawsBackground"));
        (view, delegate)
    }
}

pub(crate) fn parent_view(window: &Window) -> Option<&NSView> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
    Some(unsafe { &*(h.ns_view.as_ptr() as *const NSView) })
}

/// 显示（或更新位置）。`key` 变化表示换了一封信 / 换了显示模式，需要重新加载文档。
pub fn show(window: &Window, bounds: Bounds<Pixels>, key: u64, document: &str) {
    show_in(PRIMARY, window, bounds, key, document)
}

pub fn show_in(wid: u64, window: &Window, bounds: Bounds<Pixels>, key: u64, document: &str) {
    let Some(parent) = parent_view(window) else { return };
    let h = parent.bounds().size.height;
    let frame = CGRect::new(
        CGPoint::new(f64::from(bounds.origin.x), h - f64::from(bounds.origin.y) - f64::from(bounds.size.height)),
        CGSize::new(f64::from(bounds.size.width).max(1.), f64::from(bounds.size.height).max(1.)),
    );
    LIVE.with(|l| {
        let mut l = l.borrow_mut();
        let live = l.entry(wid).or_insert_with(|| {
            let (view, delegate) = make_view(wid);
            parent.addSubview(&view);
            let parent = unsafe { Retained::retain(parent as *const NSView as *mut NSView) }.expect("ns_view");
            view.setFrame(frame);
            Live { key: !key, view, _delegate: delegate, parent, frozen: false, frame }
        });
        if live.key != key {
            live.key = key;
            unsafe { live.view.loadHTMLString_baseURL(&NSString::from_str(document), None) };
        }
        if live.frame != frame {
            live.frame = frame;
            live.view.setFrame(frame);
        }
        live.view.setHidden(live.frozen);
    });
}

/// 收起（没有选中邮件 / 切到别的界面）。
pub fn hide() {
    hide_in(PRIMARY)
}

pub fn hide_in(wid: u64) {
    LIVE.with(|l| {
        if let Some(live) = l.borrow_mut().remove(&wid) {
            live.view.removeFromSuperview();
            // WebView 可能拿着键盘焦点：交还给 GPUI 的内容视图，快捷键才继续有效
            if let Some(win) = live.parent.window() {
                let _ = win.makeFirstResponder(Some(&live.parent));
            }
        }
    });
}

pub fn is_live() -> bool {
    LIVE.with(|l| l.borrow().contains_key(&PRIMARY))
}

pub fn is_live_in(wid: u64) -> bool {
    LIVE.with(|l| l.borrow().contains_key(&wid))
}

pub fn is_frozen() -> bool {
    LIVE.with(|l| l.borrow().get(&PRIMARY).is_some_and(|x| x.frozen))
}

pub fn current_key() -> Option<u64> {
    LIVE.with(|l| l.borrow().get(&PRIMARY).map(|x| x.key))
}

/// 冻结：隐藏原生视图（由调用方在原位置画快照）。
pub fn set_frozen(frozen: bool) {
    LIVE.with(|l| {
        if let Some(live) = l.borrow_mut().get_mut(&PRIMARY) {
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
    LIVE.with(|l| l.borrow().get(&PRIMARY).map(|x| x.view.clone()))
}

pub fn is_loading() -> bool {
    view().is_some_and(|v| unsafe { v.isLoading() })
}

/// 截取当前可见区域（2 倍分辨率 PNG）。
pub async fn snapshot(cx: &AsyncApp) -> Option<Vec<u8>> {
    snapshot_of(view()?, cx).await
}

pub(crate) async fn snapshot_of(v: Retained<WKWebView>, cx: &AsyncApp) -> Option<Vec<u8>> {
    let out: Rc<RefCell<Option<Option<Vec<u8>>>>> = Rc::new(RefCell::new(None));
    let out2 = out.clone();
    let block = RcBlock::new(move |img: *mut NSImage, _err: *mut NSError| {
        let png = unsafe { img.as_ref() }.and_then(|img| unsafe {
            let cg = img.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None)?;
            let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
            let data: Retained<NSData> = rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())?;
            Some(data.to_vec())
        });
        *out2.borrow_mut() = Some(png);
    });
    unsafe {
        let config = WKSnapshotConfiguration::new(mtm());
        config.setAfterScreenUpdates(false);
        // 按屏幕缩放截取，冻结时画上去和原生视图一样清晰
        let w = v.frame().size.width;
        let scale = v.window().map(|w| w.backingScaleFactor()).unwrap_or(2.);
        config.setSnapshotWidth(Some(&NSNumber::new_f64(w * scale)));
        v.takeSnapshotWithConfiguration_completionHandler(Some(&config), &block);
    }
    let start = Instant::now();
    loop {
        if let Some(v) = out.borrow_mut().take() {
            return v;
        }
        if start.elapsed() > Duration::from_secs(3) {
            return None;
        }
        cx.background_executor().timer(Duration::from_millis(8)).await;
    }
}

/// 执行一段应用侧脚本（内容脚本被禁用，但应用注入的脚本仍可运行），返回 String(结果)。测试用。
pub async fn eval(cx: &AsyncApp, js: &str) -> Option<String> {
    eval_of(view()?, cx, js).await
}

pub(crate) async fn eval_of(v: Retained<WKWebView>, cx: &AsyncApp, js: &str) -> Option<String> {
    let out: Rc<RefCell<Option<Option<String>>>> = Rc::new(RefCell::new(None));
    let out2 = out.clone();
    let block = RcBlock::new(move |res: *mut AnyObject, _err: *mut NSError| {
        let s = unsafe { res.as_ref() }.and_then(|o| o.downcast_ref::<NSString>()).map(|s| s.to_string());
        *out2.borrow_mut() = Some(s);
    });
    let code = NSString::from_str(&format!("(function(){{ try {{ return String({js}); }} catch(e) {{ return 'ERR:' + e; }} }})()"));
    unsafe { v.evaluateJavaScript_completionHandler(&code, Some(&block)) };
    let start = Instant::now();
    loop {
        if let Some(v) = out.borrow_mut().take() {
            return v;
        }
        if start.elapsed() > Duration::from_secs(5) {
            return None;
        }
        cx.background_executor().timer(Duration::from_millis(10)).await;
    }
}

// ───────────────────────────── 文档 ─────────────────────────────

/// HTML 邮件文档：CSP 默认拦截远程资源（追踪像素），用户点「显示图片」后放行。
pub fn mail_document(html: &str, allow_remote: bool, dark: bool) -> String {
    let csp = if allow_remote {
        // 「显示图片」只放行图片：外链样式表、字体、音视频仍然拦截（它们同样能用来追踪，且对阅读没有必要）
        "default-src 'none'; img-src * data: blob:; style-src 'unsafe-inline'; font-src data:; form-action 'none'; base-uri 'none'"
    } else {
        "default-src 'none'; img-src data: blob:; style-src 'unsafe-inline'; font-src data:; form-action 'none'; base-uri 'none'"
    };
    let cleaned = strip_dangerous(html);
    // 深色：整页反色（白底变深灰、黑字变浅），再把图片 / 视频等反回来；hue-rotate 让反色后颜色不至于完全失真
    let dark_css = if dark {
        "html { background: #fff; filter: invert(.92) hue-rotate(180deg); }\
img, video, picture, canvas, svg, [style*=\"background-image\"] { filter: invert(1) hue-rotate(180deg); }"
    } else {
        ""
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\
<style>\
html, body {{ margin: 0; padding: 0; background: #fff; color: #222; }}\
body {{ padding: 20px 22px; font: 14px/1.65 -apple-system, 'PingFang SC', 'Microsoft YaHei', sans-serif; word-wrap: break-word; overflow-wrap: anywhere; }}\
img {{ max-width: 100%; height: auto; }}\
table {{ max-width: 100%; }}\
pre {{ white-space: pre-wrap; }}\
blockquote {{ margin: 0 0 0 .8ex; border-left: 2px solid #ccc; padding-left: 1ex; color: #555; }}\
{dark_css}\
</style></head><body>{cleaned}</body></html>"
    )
}

/// 纯文本正文：保留换行、`>` 引用行变灰、URL 与邮箱变成可点链接；配色跟随主题。
pub fn text_document(text: &str, fg: &str, bg: &str, quote: &str, link: &str) -> String {
    let text = if text.trim().is_empty() { "（这封信没有可显示的正文）".to_string() } else { crate::core::parse::tidy_text(text) };
    let mut body = String::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        let q = line.trim_start().starts_with('>');
        if q {
            body.push_str("<span class=\"q\">");
        }
        body.push_str(&linkify_html(line));
        if q {
            body.push_str("</span>");
        }
        body.push('\n');
    }
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; form-action 'none'; base-uri 'none'\">\
<style>\
html, body {{ margin: 0; padding: 0; background: {bg}; color: {fg}; }}\
body {{ padding: 6px 4px 24px; font: 15px/1.7 'Songti SC', 'PingFang SC', serif; white-space: pre-wrap; word-wrap: break-word; overflow-wrap: anywhere; }}\
.q {{ color: {quote}; }}\
a {{ color: {link}; }}\
</style></head><body>{body}</body></html>"
    )
}

pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// URL 与邮箱 → `<a>`，其余转义。
pub fn linkify_html(line: &str) -> String {
    use std::sync::LazyLock;
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"(https?://[^\s<>"'）)]+)|([\w.+-]+@[\w-]+\.[\w.-]+)"#).expect("regex"));
    let mut out = String::new();
    let mut last = 0;
    for m in RE.captures_iter(line) {
        let whole = m.get(0).unwrap();
        out.push_str(&escape_html(&line[last..whole.start()]));
        let label = whole.as_str();
        let href = if m.get(1).is_some() { label.to_string() } else { format!("mailto:{label}") };
        out.push_str(&format!("<a href=\"{}\">{}</a>", escape_html(&href), escape_html(label)));
        last = whole.end();
    }
    out.push_str(&escape_html(&line[last..]));
    out
}

/// 去掉 `<script>` 与 `<meta http-equiv>`（后者可能覆盖我们的 CSP 或自动跳转）。
/// 属性值按引号整体匹配：`<meta content="a>b" http-equiv=refresh>` 这种值里带 `>` 的写法也能拦住。
fn strip_dangerous(html: &str) -> String {
    use std::sync::LazyLock;
    static SCRIPT: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?is)<script[\s\S]*?</script\s*>").expect("regex"));
    static META: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?i)<meta\b(?:[^>"']|"[^"]*"|'[^']*')*?http-equiv(?:[^>"']|"[^"]*"|'[^']*')*>"#).expect("regex"));
    let s = SCRIPT.replace_all(html, "");
    META.replace_all(&s, "").into_owned()
}

/// 是否引用了远程资源（决定是否显示「显示图片」提示条）。
pub fn has_remote_content(html: &str) -> bool {
    use std::sync::LazyLock;
    static RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?i)<img[^>]+src=["']?https?:|url\(\s*['"]?https?:|<link[^>]+href=["']?https?:"#).expect("regex"));
    RE.is_match(html)
}

// ───────────────────────────── 附件预览（系统 Quick Look） ─────────────────────────────
// 和访达里按空格看到的一样：图片 / PDF / Office / iWork / 文本 / 音视频等都能预览，不认识的类型只显示图标。
// 做法：在 GPUI 窗口上叠加原生 QLPreviewView，预览期间邮件正文 WebView 已被冻结成快照。

#[link(name = "Quartz", kind = "framework")]
extern "C" {}

thread_local! {
    /// (文件, QLPreviewView, 父视图)
    static QL: RefCell<Option<(std::path::PathBuf, Retained<NSView>, Retained<NSView>)>> = const { RefCell::new(None) };
}

/// 显示（或更新位置）附件预览；同一个文件只创建一次。
pub fn show_quicklook(window: &Window, bounds: Bounds<Pixels>, path: &std::path::Path) {
    let Some(parent) = parent_view(window) else { return };
    let h = parent.bounds().size.height;
    let frame = CGRect::new(
        CGPoint::new(f64::from(bounds.origin.x), h - f64::from(bounds.origin.y) - f64::from(bounds.size.height)),
        CGSize::new(f64::from(bounds.size.width).max(1.), f64::from(bounds.size.height).max(1.)),
    );
    QL.with(|q| {
        let mut q = q.borrow_mut();
        if !q.as_ref().is_some_and(|(p, ..)| p == path) {
            if let Some((_, old, _)) = q.take() {
                close_ql(&old);
            }
            let Some(cls) = objc2::runtime::AnyClass::get(c"QLPreviewView") else { return };
            let v: Retained<NSView> = unsafe {
                let obj: objc2::rc::Allocated<NSView> = msg_send![cls, alloc];
                // QLPreviewViewStyleNormal = 0
                msg_send![obj, initWithFrame: frame, style: 0usize]
            };
            let url = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            unsafe {
                let _: () = msg_send![&*v, setPreviewItem: &*url];
            }
            parent.addSubview(&v);
            let parent = unsafe { Retained::retain(parent as *const NSView as *mut NSView) }.expect("ns_view");
            *q = Some((path.to_path_buf(), v, parent));
        }
        if let Some((_, v, _)) = q.as_ref() {
            v.setFrame(frame);
        }
    });
}

fn close_ql(v: &NSView) {
    unsafe {
        let _: () = msg_send![v, close];
    }
    v.removeFromSuperview();
}

/// 关闭附件预览（没有预览时什么也不做，可每帧调用）。
pub fn hide_quicklook() {
    QL.with(|q| {
        if let Some((_, v, parent)) = q.borrow_mut().take() {
            close_ql(&v);
            if let Some(win) = parent.window() {
                let _ = win.makeFirstResponder(Some(&parent));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_blocks_remote_by_default() {
        let d = mail_document("<p>hi</p><script>alert(1)</script><meta http-equiv=\"refresh\" content=\"0;url=x\">", false, false);
        assert!(d.contains("img-src data: blob:;"));
        assert!(!d.contains("<script>alert"));
        assert!(!d.contains("refresh"));
        assert!(d.contains("form-action 'none'; base-uri 'none'"));
        // 属性值里带 `>` 的绕过写法
        for evil in [
            "<meta content=\"0;url=https://evil.example/a>b\" http-equiv=\"refresh\">",
            "<META CONTENT='x>y' HTTP-EQUIV=refresh>",
            "<meta\nhttp-equiv=\"refresh\"\ncontent=\"0;url=https://evil.example\">",
        ] {
            let d = mail_document(&format!("<p>hi</p>{evil}<p>after</p>"), false, false);
            assert!(!d.to_lowercase().contains("http-equiv=\"refresh") && !d.to_lowercase().contains("http-equiv=refresh"), "没拦住：{evil}");
            assert!(d.contains("<p>after</p>"));
        }
        let d2 = mail_document("<p>hi</p>", true, false);
        assert!(mail_document("<p>hi</p>", false, true).contains("invert("), "深色模式要带反色样式");
        assert!(!d2.contains("invert("));
        assert!(d2.contains("img-src * data: blob:"));
    }

    #[test]
    fn remote_detection() {
        assert!(has_remote_content("<img src=\"https://t.co/p.gif\">"));
        assert!(has_remote_content("<div style=\"background:url( 'http://x/y.png')\">"));
        assert!(!has_remote_content("<img src=\"data:image/png;base64,AAAA\">"));
    }

    #[test]
    fn linkify() {
        let s = linkify_html("看 https://a.com/x?y=1 或写信给 bob@ex.com <b>");
        assert!(s.contains("<a href=\"https://a.com/x?y=1\">https://a.com/x?y=1</a>"));
        assert!(s.contains("<a href=\"mailto:bob@ex.com\">bob@ex.com</a>"));
        assert!(s.ends_with("&lt;b&gt;"));
        let doc = text_document("hi\n> quoted", "#000", "#fff", "#999", "#00f");
        assert!(doc.contains("<span class=\"q\">&gt; quoted</span>"));
    }
}
