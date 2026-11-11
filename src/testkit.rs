//! 端到端测试工具（dev 功能）：进程内截取本窗口、投递真实 NSEvent 鼠标事件、读像素。
//! 截图走 CGWindowListCreateImage(本窗口)，不需要「屏幕录制」权限，叠加的 WKWebView 也会被截进去。

use gpui::Window;

fn ns_view(window: &Window) -> Option<&objc2_app_kit::NSView> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
    Some(unsafe { &*(h.ns_view.as_ptr() as *const objc2_app_kit::NSView) })
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCreateImage(rect: objc2_core_foundation::CGRect, list: u32, window: u32, image: u32) -> *mut std::ffi::c_void;
    fn CGImageRelease(img: *mut std::ffi::c_void);
}

/// 截取本窗口存为 PNG，返回 (宽, 高, RGBA 像素)，逻辑像素 × 缩放。
pub fn self_shot(window: &Window, path: &std::path::Path) -> Option<image::RgbaImage> {
    use objc2::AnyThread;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
    let view = ns_view(window)?;
    let win = view.window()?;
    let num = win.windowNumber() as u32;
    // CGRectNull = {inf, inf, 0, 0}
    let null =
        objc2_core_foundation::CGRect::new(objc2_core_foundation::CGPoint::new(f64::INFINITY, f64::INFINITY), objc2_core_foundation::CGSize::new(0., 0.));
    // kCGWindowListOptionIncludingWindow = 1 << 3, kCGWindowImageBoundsIgnoreFraming = 1 << 0
    let img = unsafe { CGWindowListCreateImage(null, 1 << 3, num, 1) };
    if img.is_null() {
        eprintln!("SHOT 失败：无法截取窗口 {num}");
        return None;
    }
    let cg: &objc2_core_graphics::CGImage = unsafe { &*(img as *const objc2_core_graphics::CGImage) };
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), cg);
    unsafe { CGImageRelease(img) };
    let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &objc2_foundation::NSDictionary::new()) }?;
    let bytes = data.to_vec();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(path, &bytes).ok();
    println!("SHOT {}", path.display());
    image::load_from_memory(&bytes).ok().map(|i| i.to_rgba8())
}

/// 窗口缩放（截图像素 / 逻辑像素）。
pub fn scale(window: &Window) -> f32 {
    window.scale_factor()
}

#[derive(Clone, Copy)]
pub enum MouseKind {
    LeftDown,
    LeftUp,
    Moved,
}

/// 在窗口坐标 (x, y)（左上原点、逻辑像素）投递真实 NSEvent 鼠标事件：
/// 会走到 AppKit 命中测试，叠加的 WKWebView 也能收到。
pub fn post_mouse(window: &Window, x: f32, y: f32, kinds: &[MouseKind]) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
    use objc2_foundation::NSPoint;
    let Some(view) = ns_view(window) else { return };
    let Some(win) = view.window() else { return };
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    let loc = NSPoint::new(x as f64, view.bounds().size.height - y as f64);
    let now = objc2_foundation::NSProcessInfo::processInfo().systemUptime();
    for kind in kinds {
        let ty = match kind {
            MouseKind::LeftDown => NSEventType::LeftMouseDown,
            MouseKind::LeftUp => NSEventType::LeftMouseUp,
            MouseKind::Moved => NSEventType::MouseMoved,
        };
        let ev = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            ty,
            loc,
            NSEventModifierFlags::empty(),
            now,
            win.windowNumber(),
            None,
            0,
            1,
            1.0,
        );
        if let Some(ev) = ev {
            app.postEvent_atStart(&ev, false);
        }
    }
}

pub fn click(window: &Window, x: f32, y: f32) {
    post_mouse(window, x, y, &[MouseKind::LeftDown, MouseKind::LeftUp]);
}

/// 取截图里逻辑坐标 (x, y) 处的像素。
pub fn pixel(img: &image::RgbaImage, scale: f32, x: f32, y: f32) -> [u8; 4] {
    let px = ((x * scale) as u32).min(img.width().saturating_sub(1));
    let py = ((y * scale) as u32).min(img.height().saturating_sub(1));
    img.get_pixel(px, py).0
}

pub fn near(a: [u8; 4], b: [u8; 3], tol: i32) -> bool {
    (0..3).all(|i| (a[i] as i32 - b[i] as i32).abs() <= tol)
}
