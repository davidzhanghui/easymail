//! 独立的写信窗口：写信 / 回复 / 转发都在自己的窗口里，主窗口可以继续翻邮件查资料。
//!
//! 正文仍然是原生 WKWebView 富文本编辑器（`crate::editor`），换窗口时会把原生视图搬到新窗口的
//! 内容视图上（见 `editor::show` 里的重挂载逻辑）。同一时间只有一个写信窗口，重复打开会先聚焦已有的。

use super::composer::{Composer, ComposerEvent};
use super::theme::theme;
use super::SANS;
use crate::core::engine::{Engine, Evt};
use crate::core::model::{Account, Draft};
use gpui::*;
use std::time::Duration;

/// 当前写信窗口（同一时间只有一个：原生编辑器只有一个）
static LAST: std::sync::Mutex<Option<WindowHandle<ComposerWindow>>> = std::sync::Mutex::new(None);

/// 当前写信窗口（测试用；窗口已关闭时返回 None）
pub fn current(cx: &mut App) -> Option<WindowHandle<ComposerWindow>> {
    let h = (*LAST.lock().unwrap())?;
    h.update(cx, |_, _, _| ()).ok().map(|_| h)
}

/// 当前写信组件（测试用）
pub fn composer_of(cx: &mut App) -> Option<Entity<Composer>> {
    current(cx)?.update(cx, |w, _, _| w.composer.clone()).ok()
}

pub fn has_open(cx: &mut App) -> bool {
    current(cx).is_some()
}

pub struct ComposerWindow {
    pub composer: Entity<Composer>,
    eng: Engine,
    focus: FocusHandle,
    /// 写信结果（发送 / 存草稿 / 进发件箱）：第一次渲染时订阅
    sub: Option<Subscription>,
    /// 窗口内的提示（草稿存失败等）。发送结果的提示交给主窗口（那里有「撤回」按钮）
    toasts: Vec<(u64, String, bool)>,
    seq: u64,
}

/// 打开写信窗口；已有就聚焦它。
pub fn open(eng: Engine, accounts: Vec<Account>, draft: Draft, cx: &mut App) {
    // 已有窗口：聚焦，不重复打开
    let prev = LAST.lock().unwrap().take();
    if let Some(h) = prev {
        // 还在就聚焦；已经被关掉（Err）就继续开新的
        if h.update(cx, |_, window, _| window.activate_window()).is_ok() {
            LAST.lock().unwrap().replace(h);
            return;
        }
    }
    let title = match draft.mode.as_str() {
        "reply" => "回复",
        "replyAll" => "全部回复",
        "forward" => "转发",
        _ => "写一封新信",
    };
    let size = gpui::size(px(820.), px(660.));
    let notify = eng.clone();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
            titlebar: Some(TitlebarOptions { title: Some(title.into()), appears_transparent: false, traffic_light_position: None }),
            window_min_size: Some(gpui::size(px(560.), px(420.))),
            ..Default::default()
        },
        move |window, cx| {
            let c = cx.new(|cx| {
                let mut c = Composer::new(eng.clone(), accounts, draft, window, cx);
                c.window_mode = true;
                c
            });
            let view =
                cx.new(|cx| ComposerWindow { composer: c.clone(), eng: notify.clone(), focus: cx.focus_handle(), sub: None, toasts: Vec::new(), seq: 0 });
            let focus = view.read(cx).focus.clone();
            window.focus(&focus);
            // 窗口关闭时释放原生编辑器（去掉「编辑器不再属于任何窗口」的悬空视图）
            window.on_window_should_close(cx, move |window, _| {
                crate::editor::hide_key_for_window(window);
                true
            });
            view
        },
    );
    if let Ok(h) = opened {
        LAST.lock().unwrap().replace(h);
    }
}

impl ComposerWindow {
    /// 写信结束（发送 / 存草稿 / 进发件箱）：给主窗口发提示，然后关掉这个窗口
    fn on_composer_event(&mut self, ev: &ComposerEvent, window: &mut Window, cx: &mut Context<Self>) {
        let toast = |msg: String, error: bool| self.eng.emit(Evt::Toast { msg, error });
        // 草稿 / 已发送 / 发件箱都变了：让主窗口刷新列表
        let refresh = || self.eng.emit(Evt::Accounts);
        match ev {
            ComposerEvent::Closed { saved } => {
                if *saved {
                    toast("已保存到「草稿箱」".into(), false);
                }
                refresh();
                window.remove_window();
            }
            ComposerEvent::Sent(r) => {
                toast(format!("已发送{}", if r.saved_to_sent { " · 已存入「已发送」" } else { "" }), false);
                refresh();
                window.remove_window();
            }
            ComposerEvent::Queued { outbox_id, send_at, scheduled } => {
                let secs = (*send_at - crate::core::db::now_ms()).max(0) / 1000;
                let msg = if *scheduled {
                    format!("已定时：{} 发出（在「发件箱」里可撤回）", super::fmt_full(*send_at))
                } else if secs > 0 {
                    format!("{secs} 秒后发送")
                } else {
                    "正在发送…".into()
                };
                self.eng.emit(Evt::Unsendable { msg, outbox_id: *outbox_id });
                refresh();
                window.remove_window();
            }
            ComposerEvent::Toast(m, err) => self.toast(m.clone(), *err, cx),
        }
    }

    fn toast(&mut self, msg: String, error: bool, cx: &mut Context<Self>) {
        self.seq += 1;
        let id = self.seq;
        self.toasts.push((id, msg, error));
        let weak = cx.weak_entity();
        cx.spawn(async move |_: WeakEntity<Self>, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = weak.update(cx, |a, cx| {
                a.toasts.retain(|t| t.0 != id);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for ComposerWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.sub.is_none() {
            self.sub =
                Some(cx.subscribe_in(&self.composer, window, |this: &mut Self, _, ev: &ComposerEvent, window, cx| this.on_composer_event(ev, window, cx)));
        }
        let t = theme(cx).clone();
        let mut root = div()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.ink)
            .font_family(SANS)
            // Esc：和以前一样，有内容就存草稿再关
            .on_key_down(cx.listener(|a, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    a.composer.update(cx, |c, cx| c.close(window, cx));
                    cx.notify();
                }
            }))
            .child(div().flex_1().min_h_0().child(self.composer.clone()));
        if !self.toasts.is_empty() {
            let mut col = div().absolute().bottom(px(16.)).left(px(0.)).right(px(0.)).flex().flex_col().items_center().gap(px(6.));
            for (_, msg, error) in self.toasts.iter() {
                col = col.child(
                    div()
                        .px(px(14.))
                        .py(px(8.))
                        .rounded(px(8.))
                        .bg(if *error { t.danger } else { t.ink })
                        .text_color(t.paper)
                        .text_size(px(12.5))
                        .shadow(vec![BoxShadow { color: t.shadow, offset: point(px(0.), px(6.)), blur_radius: px(24.), spread_radius: px(0.) }])
                        .child(msg.clone()),
                );
            }
            root = root.child(col);
        }
        root
    }
}
