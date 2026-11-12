//! 侧栏：视图、账号与文件夹树。

use super::*;

impl MailApp {
    pub(super) fn render_sidebar(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div()
            .flex()
            .flex_col()
            .min_h_full()
            .px(px(12.))
            .pt(px(16.))
            .pb(px(18.))
            .child(div().text_size(px(11.)).text_color(t.muted).px(px(10.)).mb(px(8.)).child("邮  件"));
        for v in VIEWS {
            let active = self.nav == Nav::View(v) && self.q.is_empty();
            let n = match v {
                View::Inbox => self.stats.inbox,
                View::Unread => self.stats.unread,
                View::Starred => self.stats.starred,
                View::Drafts => self.stats.drafts,
                _ => 0,
            };
            let strong = matches!(v, View::Inbox | View::Unread);
            col = col.child(
                div()
                    .id(SharedString::from(format!("nav-{}", v.key())))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(H_ROW))
                    .px(px(10.))
                    .rounded(px(7.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(if active { t.accent_deep } else { t.ink })
                    .when(active, |d| d.bg(t.accent_soft).font_weight(FontWeight::SEMIBOLD))
                    .hover(|s| s.bg(if active { t.accent_soft } else { t.row_hover }))
                    .child(icon(view_icon(v), 14., if active { t.accent } else { t.muted }))
                    .child(div().flex_1().child(view_label(v)))
                    .when(n > 0, |d| {
                        d.child(
                            div()
                                .text_size(px(11.))
                                .text_color(if strong { t.accent_deep } else { t.muted })
                                .when(strong, |d| d.font_weight(FontWeight::SEMIBOLD))
                                .child(fmt_num(n)),
                        )
                    })
                    .on_click(cx.listener(move |a, _, window, cx| {
                        if !a.q.is_empty() {
                            a.q.clear();
                            a.search.update(cx, |s, cx| s.set_text("", cx));
                        }
                        a.set_nav(Nav::View(v), cx);
                        window.focus(&a.focus);
                    })),
            );
        }
        let saved = panels::load_saved(&self.store);
        if !saved.is_empty() {
            col = col.child(div().text_size(px(11.)).text_color(t.muted).px(px(10.)).mt(px(20.)).mb(px(8.)).child("保存的搜索"));
            for (i, q) in saved.into_iter().enumerate() {
                let active = self.q == q;
                let q2 = q.clone();
                col = col.child(
                    div()
                        .id(("saved", i))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .h(px(H_ROW))
                        .px(px(10.))
                        .rounded(px(7.))
                        .cursor_pointer()
                        .text_size(px(13.))
                        .text_color(if active { t.accent_deep } else { t.ink })
                        .when(active, |d| d.bg(t.accent_soft))
                        .hover(|s| s.bg(t.row_hover))
                        .child(icon("search", 14., t.muted))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(q))
                        .on_click(cx.listener(move |a, _, window, cx| {
                            a.search.update(cx, |s, cx| s.set_text(&q2, cx));
                            a.set_query(q2.clone(), cx);
                            window.focus(&a.focus);
                        })),
                );
            }
        }
        col = col.child(div().text_size(px(11.)).text_color(t.muted).px(px(10.)).mt(px(20.)).mb(px(8.)).child("账  号"));
        let accounts = self.accounts.clone();
        for a in accounts.iter() {
            let list: Vec<&Folder> = self.folders.iter().filter(|f| f.account_id == a.id).collect();
            if a.is_local() && list.is_empty() {
                continue;
            }
            let closed = self.collapsed.contains(&a.id);
            let st = self.sync.get(&a.id).cloned().unwrap_or_default();
            let aid = a.id;
            let local = a.is_local();
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .child(
                        div()
                            .id(SharedString::from(format!("acct-{aid}")))
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .h(px(H_ROW))
                            .px(px(8.))
                            .rounded(px(7.))
                            .cursor_pointer()
                            .hover(|s| s.bg(t.row_hover))
                            .text_size(px(11.5))
                            .text_color(t.ink_2)
                            .child(
                                div().mr(px(4.)).child(
                                    icon("caret", 12., t.muted)
                                        .when(closed, |s| s.with_transformation(Transformation::rotate(radians(-std::f32::consts::FRAC_PI_2)))),
                                ),
                            )
                            .child(dot(t.named(&a.color), 7.).mr(px(7.)))
                            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(if local {
                                "本地归档".to_string()
                            } else {
                                a.email.clone()
                            }))
                            .on_click(cx.listener(move |app, _, _, cx| {
                                if !app.collapsed.remove(&aid) {
                                    app.collapsed.insert(aid);
                                }
                                let v: Vec<i64> = app.collapsed.iter().copied().collect();
                                app.store.set_kv("collapsed", &serde_json::to_string(&v).unwrap_or_default());
                                cx.notify();
                            })),
                    )
                    .when(!local, |d| {
                        d.child(
                            button(SharedString::from(format!("acct-sync-{aid}")), t, Kind::Ghost, Some("refresh"), "", true)
                                .text_color(if st.syncing { t.accent } else { t.muted })
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    app.eng.request_sync(aid, None, true);
                                    app.toast("已开始同步", false, cx);
                                })),
                        )
                    })
                    .child(
                        button(SharedString::from(format!("acct-more-{aid}")), t, Kind::Ghost, Some("more"), "", true)
                            .on_click(cx.listener(move |app, ev: &ClickEvent, _, cx| app.toggle_pop(Pop::Account(aid), ev.position(), cx))),
                    ),
            );
            if closed {
                continue;
            }
            if !a.last_error.is_empty() && !st.syncing {
                col = col.child(div().px(px(10.)).pl(px(18.)).py(px(6.)).text_size(px(11.5)).text_color(t.danger).child(one_line(&a.last_error, 120)));
            }
            if list.is_empty() {
                col = col.child(div().h(px(H_ROW)).flex().items_center().pl(px(18.)).text_size(px(12.)).text_color(t.muted).child(if st.syncing {
                    "正在读取文件夹…"
                } else {
                    "尚未同步"
                }));
            }
            for f in list.iter() {
                let depth =
                    list.iter().filter(|g| g.id != f.id && !g.delimiter.is_empty() && f.path.starts_with(&format!("{}{}", g.path, g.delimiter))).count();
                let active = self.nav == Nav::Folder(f.id);
                let fid = f.id;
                let group = SharedString::from(format!("fg-{fid}"));
                col = col.child(
                    div()
                        .id(SharedString::from(format!("folder-{fid}")))
                        .group(group.clone())
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .h(px(H_ROW))
                        .pl(px(18. + depth as f32 * 14.))
                        .pr(px(4.))
                        .rounded(px(7.))
                        .cursor_pointer()
                        .text_size(px(12.5))
                        .text_color(if active {
                            t.accent_deep
                        } else if f.sync_enabled || local || self.virtual_folder(f.id).is_some() {
                            t.ink
                        } else {
                            t.faint
                        })
                        .when(active, |d| d.bg(t.accent_soft).font_weight(FontWeight::SEMIBOLD))
                        .hover(|s| s.bg(if active { t.accent_soft } else { t.row_hover }))
                        .child(icon(role_icon(&f.role), 13., if active { t.accent } else { t.muted }))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(f.leaf()))
                        // 未同步的文件夹（如 Gmail 的「所有邮件 / 重要」）本地没有邮件，不显示服务器未读数，免得点进去对不上
                        .when(f.unseen > 0 && (f.sync_enabled || local), |d| {
                            d.child(div().text_size(px(11.)).text_color(t.accent_deep).font_weight(FontWeight::SEMIBOLD).child(fmt_num(f.unseen)))
                        })
                        .child(
                            div()
                                .id(SharedString::from(format!("folder-more-{fid}")))
                                .size(px(22.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(5.))
                                .opacity(0.)
                                .group_hover(group, |s| s.opacity(1.))
                                .hover(|s| s.bg(t.border_soft))
                                .child(icon("more", 13., t.muted))
                                .on_click(cx.listener(move |app, ev: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    app.toggle_pop(Pop::Folder(fid), ev.position(), cx)
                                })),
                        )
                        .on_click(cx.listener(move |app, _, window, cx| {
                            app.set_nav(Nav::Folder(fid), cx);
                            window.focus(&app.focus);
                        })),
                );
            }
        }
        let col = col
            .child(div().flex_1().min_h(px(18.)))
            .child(
                button("side-import", t, Kind::Ghost, Some("download"), "导入邮件", true)
                    .justify_start()
                    .w_full()
                    .on_click(cx.listener(|a, _, window, cx| a.open_import(window, cx))),
            )
            .child(
                button("side-accounts", t, Kind::Ghost, Some("gear"), "账号与设置", true)
                    .justify_start()
                    .w_full()
                    .mt(px(4.))
                    .on_click(cx.listener(|a, _, window, cx| a.open_accounts(Tab::Accounts, false, window, cx))),
            )
            .child(div().px(px(10.)).pt(px(6.)).text_size(px(10.5)).text_color(t.faint).child(format!(
                "本地存档 {} 封 · 已缓存正文 {} 封",
                fmt_num(self.stats.archived),
                fmt_num(self.stats.with_body)
            )));
        // 外层只负责滚动；内层 flex 列的子项一律不收缩（否则内容超高时条目会被压扁）
        div().id("sidebar").flex_none().w(px(self.sidebar_w)).h_full().bg(t.panel_2).overflow_y_scroll().child(col)
    }
}
