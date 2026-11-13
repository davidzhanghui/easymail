//! 单独的阅读窗口：把一封邮件放进自己的窗口（可并排对照多封邮件）。
//!
//! 每个阅读窗口有自己的 WKWebView（按 GPUI 窗口 id 区分，见 `webview::show_in`），不参与主窗口的同步事件，
//! 邮件内容在打开时读出，之后不随服务器变化；关窗口就是真的关掉（主窗口关闭才是隐藏到菜单栏）。

use super::theme::{hex_css, theme};
use super::widgets::*;
use super::{fmt_full, fmt_size, SANS, SERIF};
use crate::core::db::Store;
use crate::core::model::{addrs_text, Detail};
use crate::core::queries;
use crate::webview;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::Arc;
use std::time::Duration;

pub struct ReaderWindow {
    d: Detail,
    store: Store,
    wid: u64,
    text_mode: bool,
    doc: Option<(u64, Arc<str>)>,
}

/// 在新窗口打开一封邮件（邮件正文要已在本地，没缓存的先在主窗口打开一次）
pub fn open(store: &Store, id: i64, cx: &mut App) -> anyhow::Result<()> {
    let d = queries::detail(store, id)?.ok_or_else(|| anyhow::anyhow!("邮件不存在"))?;
    if d.html.is_empty() && d.text.is_empty() {
        anyhow::bail!("正文还没下载到本地，先在主窗口打开一次再试");
    }
    let title = if d.subject.is_empty() { "(无主题)".to_string() } else { d.subject.clone() };
    let size = gpui::size(px(860.), px(720.));
    let store = store.clone();
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
            titlebar: Some(TitlebarOptions { title: Some(title.into()), appears_transparent: false, traffic_light_position: None }),
            window_min_size: Some(gpui::size(px(520.), px(360.))),
            ..Default::default()
        },
        move |window, cx| {
            let wid = window.window_handle().window_id().as_u64();
            let view = cx.new(|_| ReaderWindow { d, store, wid, text_mode: false, doc: None });
            // 点正文里的链接：导航代理记下来，这里轮询取走
            let weak = view.downgrade();
            window
                .spawn(cx, async move |cx| loop {
                    cx.background_executor().timer(Duration::from_millis(150)).await;
                    let Some(v) = weak.upgrade() else { break };
                    let links = webview::take_clicked_in(wid);
                    if links.is_empty() {
                        continue;
                    }
                    let _ = v.update(cx, |a, cx| {
                        for l in links {
                            a.open_link(&l, cx);
                        }
                    });
                })
                .detach();
            // 窗口关闭时释放它的 WKWebView
            window.on_window_should_close(cx, move |_, _| {
                webview::hide_in(wid);
                true
            });
            view
        },
    )?;
    Ok(())
}

impl ReaderWindow {
    fn open_link(&mut self, url: &str, cx: &mut Context<Self>) {
        if url.starts_with("http://") || url.starts_with("https://") {
            let url = if self.store.get_kv("strip_tracking").as_deref() == Some("0") { url.to_string() } else { crate::core::extract::strip_tracking(url) };
            cx.open_url(&url);
        } else if url.starts_with("mailto:") {
            cx.open_url(url);
        }
    }
}

impl Render for ReaderWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let d = &self.d;
        let show_html = !d.html.is_empty() && !self.text_mode;
        let remote = crate::core::queries::remote_allowed(&self.store, &d.from.address);
        let dark_mail = t.dark && show_html && self.store.get_kv("dark_mail").as_deref() != Some("0");
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (d.id, show_html, remote, t.id, dark_mail).hash(&mut h);
            h.finish()
        };
        if self.doc.as_ref().map(|x| x.0) != Some(key) {
            let doc = if show_html {
                webview::mail_document(&d.html, remote, dark_mail)
            } else {
                webview::text_document(&d.text, &hex_css(t.hex_ink), &hex_css(t.hex_paper), &hex_css(t.hex_muted), &hex_css(t.hex_accent))
            };
            self.doc = Some((key, doc.into()));
        }
        let doc = self.doc.as_ref().map(|x| x.1.clone()).unwrap_or_else(|| Arc::from(""));
        let wid = self.wid;
        let meta = |label: &'static str, v: String| {
            div()
                .flex()
                .gap(px(10.))
                .text_size(px(12.5))
                .child(div().w(px(48.)).flex_none().text_color(t.muted).child(label))
                .child(div().flex_1().min_w_0().text_color(t.ink_2).child(v))
        };
        let files: Vec<String> = d.attachments.iter().filter(|a| !a.inline).map(|a| format!("{}（{}）", a.filename, fmt_size(a.size))).collect();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.ink)
            .font_family(SANS)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .flex_none()
                    .px(px(24.))
                    .pt(px(16.))
                    .pb(px(10.))
                    .child(div().font_family(SERIF).text_size(px(20.)).font_weight(FontWeight::BOLD).child(if d.subject.is_empty() {
                        "(无主题)".to_string()
                    } else {
                        d.subject.clone()
                    }))
                    .child(meta("发件人", d.from.text()))
                    .child(meta("收件人", addrs_text(&d.to)))
                    .when(!d.cc.is_empty(), |x| x.child(meta("抄送", addrs_text(&d.cc))))
                    .child(meta("时间", fmt_full(d.date)))
                    .when(!files.is_empty(), |x| x.child(meta("附件", files.join("；")))),
            )
            .child(div().flex().flex_none().items_center().justify_end().px(px(24.)).h(px(30.)).when(!d.html.is_empty(), |x| {
                x.child(button("rw-mode", &t, Kind::Ghost, None, if self.text_mode { "原始排版" } else { "纯文本" }, true).on_click(cx.listener(
                    |a, _, _, cx| {
                        a.text_mode = !a.text_mode;
                        cx.notify();
                    },
                )))
            }))
            .child(
                div()
                    .id("rw-body")
                    .flex_1()
                    .min_h(px(120.))
                    .mx(px(16.))
                    .mb(px(16.))
                    .relative()
                    .rounded(px(6.))
                    .overflow_hidden()
                    .child(canvas(|b, _, _| b, move |b, _, window, _| webview::show_in(wid, window, b, key, &doc)).size_full()),
            )
    }
}
