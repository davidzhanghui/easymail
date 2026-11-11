//! 验证：GPUI 窗口叠加 WKWebView 显示 HTML 邮件；打开弹层时冻结成快照，弹层不被遮挡；点链接被拦截。
use easymail::{testkit, webview};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::Arc;
use std::time::Duration;

struct Demo {
    popover: bool,
    snap: Option<Arc<Image>>,
}

const HTML: &str = r#"<h1 style="color:#c00">Hello 邮件</h1><p>正文第一段，<a href="https://example.com/x">点我打开</a>。</p>
<img src="https://example.com/track.gif" width=1 height=1><div style="background:#0a0;height:600px">绿色块</div>"#;

impl Render for Demo {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snap = self.snap.clone();
        let frozen = self.popover;
        div()
            .size_full()
            .flex()
            .bg(rgb(0xf3f1e9))
            .child(div().w(px(300.)).h_full().p_4().child("左侧栏"))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .relative()
                    .child(
                        canvas(
                            |b, _, _| b,
                            move |b, _, window, _| {
                                let doc = webview::mail_document(HTML, false);
                                webview::show(window, b, 1, &doc);
                                webview::set_frozen(frozen);
                            },
                        )
                        .size_full(),
                    )
                    .when_some(snap.filter(|_| frozen), |d, s| d.child(img(s).absolute().top_0().left_0().size_full()))
                    .when(self.popover, |d| d.child(div().absolute().top(px(40.)).left(px(-120.)).w(px(260.)).h(px(200.)).bg(rgb(0x2244ff)).child("弹层"))),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, _| {}))
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1000.), px(700.)), cx);
        let win = cx
            .open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), kind: WindowKind::Floating, ..Default::default() }, |_, cx| {
                cx.new(|_| Demo { popover: false, snap: None })
            })
            .unwrap();
        cx.activate(true);
        let dir = std::path::PathBuf::from(std::env::var("SHOT_DIR").unwrap_or("/tmp".into()));
        cx.spawn(async move |cx: &mut AsyncApp| {
            let ex = cx.background_executor().clone();
            let t = move |ms| ex.timer(Duration::from_millis(ms));
            t(2500).await;
            let any: AnyWindowHandle = win.into();
            let title = webview::eval(cx, "document.querySelector('h1').textContent + '|' + document.images[0].naturalWidth + '|' + (typeof alert)").await;
            println!("EVAL {:?}", title);
            let mut ok = true;
            let shot_a = cx.update_window(any, |_, w, _| (testkit::self_shot(w, &dir.join("wv-live.png")), w.scale_factor())).unwrap();
            if let (Some(img), s) = shot_a {
                let p = testkit::pixel(&img, s, 600., 400.);
                println!("live 绿色块像素 {:?}", p);
                ok &= is_green(p);
            }
            // 打开弹层：先截快照，再冻结
            let png = webview::snapshot(cx).await;
            println!("快照 {} 字节", png.as_ref().map(|p| p.len()).unwrap_or(0));
            win.update(cx, |d, _, cx| {
                d.snap = png.map(|p| Arc::new(Image::from_bytes(ImageFormat::Png, p)));
                d.popover = true;
                cx.notify();
            })
            .ok();
            t(600).await;
            let shot_b = cx.update_window(any, |_, w, _| (testkit::self_shot(w, &dir.join("wv-popover.png")), w.scale_factor())).unwrap();
            if let (Some(img), s) = shot_b {
                let pop = testkit::pixel(&img, s, 400., 120.);
                let green = testkit::pixel(&img, s, 700., 400.);
                println!("弹层像素 {:?}；快照区绿色像素 {:?}；frozen={}", pop, green, webview::is_frozen());
                ok &= testkit::near(pop, [0x22, 0x44, 0xff], 30) && is_green(green);
            }
            // 关弹层恢复 live，并点链接
            win.update(cx, |d, _, cx| {
                d.popover = false;
                cx.notify();
            })
            .ok();
            t(500).await;
            let link = webview::eval(cx, "JSON.stringify(document.querySelector('a').getBoundingClientRect())").await.unwrap_or_default();
            println!("链接位置 {link}");
            let r: serde_json::Value = serde_json::from_str(&link).unwrap_or_default();
            let (lx, ly) = (r["x"].as_f64().unwrap_or(0.) as f32 + 300. + 10., r["y"].as_f64().unwrap_or(0.) as f32 + 8.);
            let title_h = cx
                .update_window(any, |_, w, _| {
                    let h = w.viewport_size().height;
                    let _ = h;
                    testkit::click(w, lx, ly);
                })
                .ok();
            let _ = title_h;
            t(800).await;
            let clicked = webview::take_clicked();
            println!("拦截到的链接 {:?}；webview 仍在原页 {:?}", clicked, webview::eval(cx, "location.href").await);
            ok &= clicked.iter().any(|u| u.starts_with("https://example.com/x"));
            println!("RESULT {}", if ok { "PASS" } else { "FAIL" });
            cx.update(|cx| cx.quit()).ok();
        })
        .detach();
    });
}

/// 显示器色彩配置会改变具体数值，只判断绿色通道占主导
fn is_green(p: [u8; 4]) -> bool {
    p[1] > 120 && p[1] as i32 > p[0] as i32 + 60 && p[1] as i32 > p[2] as i32 + 60
}
