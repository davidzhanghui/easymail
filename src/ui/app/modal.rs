//! 模态窗口：写信、账号设置、导入、输入框、附件预览。

use super::*;

impl MailApp {
    pub(super) fn render_modal(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        // 注意：写信有自己的窗口，编辑器由那个窗口管理，这里不能 hide（会误关正在写的信）
        let Some(modal) = self.modal.as_ref() else {
            webview::hide_quicklook();
            return None;
        };
        if !matches!(modal, Modal::Preview { .. }) {
            webview::hide_quicklook();
        }
        let (inner, overlay_close): (AnyElement, bool) = match modal {
            Modal::Preview { msg_id, index, filename, size, path } => {
                let vp = window.viewport_size();
                let w = (f32::from(vp.width) * 0.8).clamp(480., 1100.);
                let body_h = (f32::from(vp.height) * 0.9 - 130.).max(240.);
                let (mid, ix, name, p) = (*msg_id, *index, filename.clone(), path.clone());
                (
                    modal_shell(
                        t,
                        w,
                        one_line(filename, 60),
                        fmt_size(*size),
                        button("preview-x", t, Kind::Ghost, Some("close"), "", true).on_click(cx.listener(|a, _, w, cx| {
                            a.modal = None;
                            webview::hide_quicklook();
                            w.focus(&a.focus);
                            cx.notify();
                        })),
                    )
                    .child(
                        div().h(px(body_h)).w_full().bg(t.panel_2).child(
                            canvas(
                                |b, _, _| b,
                                move |b, _, window, _| {
                                    // 原生视图不受 GPUI 裁剪：自己与弹窗的可见区域（max_w / max_h / overflow_hidden 之后）取交集
                                    let vis = b.intersect(&window.content_mask().bounds);
                                    webview::show_quicklook(window, vis, &p)
                                },
                            )
                            .size_full(),
                        ),
                    )
                    .child(
                        modal_footer(t).child(div().flex_1()).child(
                            button("preview-save", t, Kind::Primary, Some("download"), "另存为…", false)
                                .on_click(cx.listener(move |a, _, _, cx| a.save_attachment(mid, ix, name.clone(), cx))),
                        ),
                    )
                    .into_any_element(),
                    true,
                )
            }
            Modal::Panel(_) => (self.render_panel(t, window, cx), true),
            Modal::Accounts(m) => (m.clone().into_any_element(), true),
            Modal::Import(m) => (m.clone().into_any_element(), !m.read(cx).busy()),
            Modal::Prompt { title, input, .. } => {
                let focused = input.read(cx).is_focused(window);
                (
                    modal_shell(
                        t,
                        420.,
                        title.clone(),
                        "",
                        button("prompt-x", t, Kind::Ghost, Some("close"), "", true).on_click(cx.listener(|a, _, w, cx| {
                            a.modal = None;
                            w.focus(&a.focus);
                            cx.notify();
                        })),
                    )
                    .child(div().p(px(22.)).child(field_box(t, focused).child(div().flex_1().child(input.clone()))))
                    .child(
                        modal_footer(t)
                            .child(div().flex_1())
                            .child(button("prompt-cancel", t, Kind::Ghost, None, "取消", false).on_click(cx.listener(|a, _, w, cx| {
                                a.modal = None;
                                w.focus(&a.focus);
                                cx.notify();
                            })))
                            .child(button("prompt-ok", t, Kind::Primary, None, "确定", false).on_click(cx.listener(|a, _, w, cx| a.submit_prompt(w, cx)))),
                    )
                    .into_any_element(),
                    true,
                )
            }
        };
        Some(
            div()
                .id("modal-overlay")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(t.overlay)
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |a, _, w, cx| {
                        if overlay_close && a.pop.is_none() {
                            if matches!(a.modal, Some(Modal::Accounts(_)) | Some(Modal::Import(_))) {
                                a.load_meta(cx);
                            }
                            a.modal = None;
                            w.focus(&a.focus);
                            cx.notify();
                        }
                    }),
                )
                .child(div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(inner))
                .into_any_element(),
        )
    }
}
