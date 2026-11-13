//! 邮件列表：搜索结果、草稿行、邮件行。

use super::*;

impl MailApp {
    pub(super) fn render_list(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let folder = match self.nav {
            Nav::Folder(id) => self.folders.iter().find(|f| f.id == id).cloned(),
            _ => None,
        };
        let drafts = self.is_drafts();
        let n_checked = self.checked.len();
        let any_unread = self.items.iter().any(|m| self.checked.contains(&m.id) && !m.seen);
        let any_unstarred = self.items.iter().any(|m| self.checked.contains(&m.id) && !m.flagged);
        let all_checked = !self.items.is_empty() && self.items.iter().all(|m| self.checked.contains(&m.id));
        let unified = (!matches!(self.nav, Nav::Folder(_)) && self.accounts.len() > 1) || !self.q.is_empty();
        let syncing = self.sync.values().any(|s| s.syncing);
        let head = div()
            .flex_none()
            .px(px(18.))
            .pt(px(16.))
            .pb(px(10.))
            .border_b_1()
            .border_color(t.border_soft)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .min_h(px(H_SM))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_family(SERIF)
                            .text_size(px(21.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(t.ink)
                            .child(self.title()),
                    )
                    .when(folder.as_ref().is_some_and(|f| f.unseen > 0 && f.sync_enabled), |d| {
                        let fid = folder.as_ref().map(|f| f.id).unwrap_or(0);
                        d.child(
                            button("mark-all", t, Kind::Ghost, Some("read"), "", true)
                                .on_click(cx.listener(move |a, _, window, cx| a.folder_action("read-all", fid, window, cx))),
                        )
                    })
                    .child(
                        button("list-refresh", t, Kind::Ghost, Some("refresh"), "", true)
                            .text_color(if syncing { t.accent } else { t.muted })
                            .on_click(cx.listener(|a, _, _, cx| a.sync_now(cx))),
                    )
                    .when(self.reader_collapsed, |d| {
                        d.child(button("expand-reader", t, Kind::Ghost, Some("panel-right"), "", true).on_click(cx.listener(|a, _, _, cx| {
                            a.reader_collapsed = false;
                            a.save_layout();
                            cx.notify();
                        })))
                    })
                    .when(!self.q.is_empty(), |d| {
                        let saved = panels::load_saved(&self.store).contains(&self.q);
                        d.child(
                            button("save-search", t, Kind::Ghost, Some(if saved { "star-fill" } else { "star" }), "", true)
                                .on_click(cx.listener(|a, _, _, cx| a.toggle_saved_search(cx))),
                        )
                    })
                    .when(!drafts, |d| {
                        d.child(
                            button("list-export", t, Kind::Ghost, Some("download"), "", true)
                                .on_click(cx.listener(|a, ev: &ClickEvent, _, cx| a.toggle_pop(Pop::Export, ev.position(), cx))),
                        )
                    })
                    .child(
                        button("list-tools", t, Kind::Ghost, Some("more"), "", true)
                            .on_click(cx.listener(|a, ev: &ClickEvent, _, cx| a.toggle_pop(Pop::Tools, ev.position(), cx))),
                    ),
            )
            .child(div().text_size(px(11.5)).text_color(t.muted).mt(px(3.)).child(self.subtitle()))
            .when(self.q.is_empty() && !drafts && n_checked == 0, |d| {
                let mut row = div().flex().gap(px(6.)).mt(px(10.));
                for (f, label) in [(Filter::All, "全部"), (Filter::Unread, "未读"), (Filter::Starred, "星标"), (Filter::Attachments, "附件")] {
                    row = row.child(
                        chip(SharedString::from(format!("filter-{label}")), t, label, self.filter == f)
                            .on_click(cx.listener(move |a, _, _, cx| a.set_filter(f, cx))),
                    );
                }
                row = row.child(div().flex_1()).child(chip("threads", t, "会话", self.threads).on_click(cx.listener(|a, _, _, cx| a.toggle_threads(cx))));
                d.child(row)
            })
            .when(n_checked > 0, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(4.))
                        .mt(px(10.))
                        .child(checkbox("check-all", t, all_checked).mr(px(4.)).on_click(cx.listener(move |a, _, _, cx| {
                            if all_checked {
                                a.checked.clear();
                            } else {
                                a.checked = a.items.iter().map(|m| m.id).collect();
                            }
                            cx.notify();
                        })))
                        .child(div().text_size(px(12.)).text_color(t.accent_deep).mr(px(4.)).child(format!("已选 {n_checked}")))
                        .child(
                            button("b-seen", t, Kind::Ghost, Some(if any_unread { "read" } else { "unread" }), if any_unread { "已读" } else { "未读" }, true)
                                .on_click(cx.listener(move |a, _, _, cx| {
                                    let ids: Vec<i64> = a.checked.iter().copied().collect();
                                    a.set_seen(ids, any_unread, cx)
                                })),
                        )
                        .child(
                            button(
                                "b-star",
                                t,
                                Kind::Ghost,
                                Some(if any_unstarred { "star" } else { "star-fill" }),
                                if any_unstarred { "星标" } else { "取消星标" },
                                true,
                            )
                            .on_click(cx.listener(move |a, _, _, cx| {
                                let ids: Vec<i64> = a.checked.iter().copied().collect();
                                a.set_flagged(ids, any_unstarred, false, cx)
                            })),
                        )
                        .child(button("b-move", t, Kind::Ghost, Some("move"), "移动", true).on_click(cx.listener(|a, ev: &ClickEvent, _, cx| {
                            let ids: Vec<i64> = a.checked.iter().copied().collect();
                            a.toggle_pop(Pop::Move(ids), ev.position(), cx)
                        })))
                        .child(button("b-del", t, Kind::Ghost, Some("trash"), "删除", true).text_color(t.danger).on_click(cx.listener(|a, _, window, cx| {
                            let ids: Vec<i64> = a.checked.iter().copied().collect();
                            a.delete(ids, window, cx)
                        })))
                        .child(button("b-cancel", t, Kind::Ghost, None, "取消", true).on_click(cx.listener(|a, _, _, cx| {
                            a.checked.clear();
                            cx.notify();
                        }))),
                )
            });

        let body = if drafts {
            let mut col = div().id("draft-list").flex_1().overflow_y_scroll();
            if self.drafts.is_empty() {
                col = col.child(empty_list(t, "还没有草稿。"));
            }
            let emails: HashMap<i64, String> = self.accounts.iter().map(|a| (a.id, a.email.clone())).collect();
            for d in self.drafts.clone() {
                col = col.child(self.draft_row(t, d, &emails, cx));
            }
            col.into_any_element()
        } else if self.items.is_empty() && self.drafts.is_empty() {
            let unsynced = match self.nav {
                Nav::Folder(id) if self.q.is_empty() && self.virtual_folder(id).is_none() => {
                    self.folders.iter().any(|f| f.id == id && !f.sync_enabled && !self.accounts.iter().any(|a| a.id == f.account_id && a.is_local()))
                }
                _ => false,
            };
            let msg = if self.loading {
                "正在读取…"
            } else if unsynced {
                "这个文件夹没有同步到本地。\nGmail 的「重要」等标签文件夹与收件箱内容重复，默认不同步；\n需要的话可在文件夹的「…」菜单里选「恢复同步此文件夹」。"
            } else {
                "这里空空如也，\n一封符合条件的邮件都没有。"
            };
            div().flex_1().child(empty_list(t, msg)).into_any_element()
        } else {
            let count = self.items.len() + usize::from(self.next.is_some());
            let list = uniform_list(
                "mail-list",
                count,
                cx.processor(move |a: &mut MailApp, range: std::ops::Range<usize>, _window, cx| {
                    let t = theme(cx).clone();
                    range.map(|ix| a.render_row(&t, ix, unified, cx).into_any_element()).collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.list_scroll.clone())
            .flex_1();
            if self.drafts.is_empty() {
                list.into_any_element()
            } else {
                // 账号「草稿箱」：本机草稿排在服务器上的邮件之前
                let emails: HashMap<i64, String> = self.accounts.iter().map(|a| (a.id, a.email.clone())).collect();
                let mut col = div().id("folder-drafts").flex().flex_col().flex_1().min_h_0();
                let mut dl = div().id("folder-draft-rows").flex_none().max_h(px(280.)).overflow_y_scroll();
                for d in self.drafts.clone() {
                    dl = dl.child(self.draft_row(t, d, &emails, cx));
                }
                col = col.child(dl);
                if !self.items.is_empty() {
                    col = col.child(list);
                }
                col.into_any_element()
            }
        };

        div()
            .flex()
            .flex_col()
            .h_full()
            .min_h_0()
            .bg(t.panel)
            .map(|d| if self.reader_collapsed { d.flex_1().min_w_0() } else { d.flex_none().w(px(self.list_w)) })
            .child(head)
            .child(body)
    }

    /// 本机草稿行（草稿视图与账号「草稿箱」文件夹共用）
    pub(super) fn draft_row(&self, t: &Theme, d: Draft, emails: &HashMap<i64, String>, cx: &mut Context<Self>) -> Stateful<Div> {
        let did = d.id.unwrap_or(0);
        let acc = d.account_id.and_then(|i| emails.get(&i).cloned()).unwrap_or_else(|| "未选择账号".into());
        div()
            .id(SharedString::from(format!("draft-{did}")))
            .px(px(18.))
            .pl(px(34.))
            .py(px(11.))
            .border_b_1()
            .border_color(t.border_soft)
            .cursor_pointer()
            .hover(|s| s.bg(t.row_hover))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(t.ink_2)
                            .child(if d.to.trim().is_empty() { "（未填收件人）".to_string() } else { format!("致 {}", d.to) }),
                    )
                    .child(div().text_size(px(11.)).text_color(t.muted).child(fmt_short(d.updated_at))),
            )
            .child(
                div()
                    .mt(px(3.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_family(SERIF)
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.ink)
                    .child(if d.subject.is_empty() { "(无主题)".to_string() } else { d.subject.clone() }),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .mt(px(4.))
                    .items_center()
                    .child(badge(t, format!("草稿 · {acc}"), false))
                    .when(!d.attachments.is_empty(), |x| x.child(badge(t, "附件", true)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(12.))
                            .text_color(t.muted)
                            .child(one_line(&crate::core::mailer::body_snippet(&d.body), 120)),
                    ),
            )
            .on_click(cx.listener(move |a, _, window, cx| a.open_composer(d.clone(), window, cx)))
    }

    pub(super) fn render_row(&mut self, t: &Theme, ix: usize, unified: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        if ix >= self.items.len() {
            return div().id("more").h(px(ROW_H)).flex().items_center().justify_center().child(
                button("load-more", t, Kind::Secondary, None, if self.loading { "正在读取…" } else { "加载更早的邮件" }, true).on_click(cx.listener(
                    |a, _, _, cx| {
                        if !a.loading {
                            a.load_list(false, true, cx)
                        }
                    },
                )),
            );
        }
        let m = &self.items[ix];
        let id = m.id;
        let drag_label = {
            let n = self.targets(id).len();
            if n > 1 {
                format!("{n} 封邮件")
            } else if m.subject.is_empty() {
                "(无主题)".to_string()
            } else {
                one_line(&m.subject, 24)
            }
        };
        let selected = self.selected == Some(id);
        let checked = self.checked.contains(&id);
        let flagged = m.flagged;
        let from = if m.folder_role == "sent" {
            format!("致 {}", m.to.iter().map(|a| a.short().to_string()).collect::<Vec<_>>().join(", "))
        } else {
            m.from.short().to_string()
        };
        div()
            .id(("row", id as u64))
            .h(px(ROW_H))
            .relative()
            .pl(px(34.))
            .pr(px(18.))
            .pt(px(12.))
            .border_b_1()
            .border_color(t.border_soft)
            .cursor_pointer()
            .map(|d| {
                if selected {
                    d.bg(t.accent_soft).child(div().absolute().left_0().top_0().bottom_0().w(px(3.)).bg(t.accent))
                } else if checked {
                    d.bg(t.accent_soft.opacity(0.5))
                } else {
                    d.bg(if m.seen { t.panel_2 } else { t.panel }).hover(|s| s.bg(t.row_hover))
                }
            })
            .when(!m.seen, |d| d.child(div().absolute().left(px(13.)).top(px(41.)).size(px(9.)).rounded_full().bg(t.accent)))
            .child(div().absolute().left(px(10.)).top(px(13.)).child(checkbox(("chk", id as u64), t, checked).on_click(cx.listener(
                move |a, ev: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    if ev.modifiers().shift {
                        a.select_range(id);
                    } else if !a.checked.remove(&id) {
                        a.checked.insert(id);
                        a.anchor = Some(id);
                    }
                    cx.notify();
                },
            ))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(if m.seen { t.muted } else { t.ink })
                            .font_weight(if m.seen { FontWeight::NORMAL } else { FontWeight::BOLD })
                            .child(from),
                    )
                    .child(div().flex_none().text_size(px(11.)).text_color(t.muted).child(fmt_short(m.date))),
            )
            .child(
                div()
                    .mt(px(3.))
                    .pr(px(26.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_family(SERIF)
                    .text_size(px(15.))
                    .font_weight(if m.seen { FontWeight::NORMAL } else { FontWeight::BOLD })
                    .text_color(if !m.seen {
                        t.ink
                    } else if selected {
                        t.ink_2
                    } else {
                        t.muted
                    })
                    .child(if m.subject.is_empty() { "(无主题)".to_string() } else { m.subject.clone() }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .mt(px(4.))
                    .when(unified, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap(px(4.))
                                .text_size(px(10.5))
                                .text_color(t.muted)
                                .child(dot(t.named(&m.account_color), 6.))
                                .child(div().max_w(px(110.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(m.folder_name.clone())),
                        )
                    })
                    .when(m.thread_count > 1, |d| d.child(badge(t, format!("{} 封", m.thread_count), false)))
                    .when(m.answered, |d| d.child(badge(t, "已回复", false)))
                    .when(m.has_att, |d| d.child(badge(t, "附件", true)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(12.))
                            .text_color(t.muted)
                            .child(one_line(&m.snippet, 140)),
                    ),
            )
            .child(
                div()
                    .id(("star", id as u64))
                    .absolute()
                    .right(px(12.))
                    .top(px(34.))
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .hover(|s| s.bg(t.border_soft))
                    .child(icon(if flagged { "star-fill" } else { "star" }, 14., if flagged { t.gold } else { t.faint }))
                    .on_click(cx.listener(move |a, _, _, cx| {
                        cx.stop_propagation();
                        a.set_flagged(vec![id], !flagged, true, cx)
                    })),
            )
            // 拖到侧栏文件夹上移动；已勾选的邮件整批一起拖
            .on_drag(DragMails { ids: self.targets(id), label: drag_label }, |d, _, _, cx| cx.new(|_| DragGhost(d.label.clone())))
            // 右键：上下文菜单
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |a, ev: &MouseDownEvent, _, cx| {
                    a.pop = None;
                    a.toggle_pop(Pop::Row(id), ev.position, cx)
                }),
            )
            .on_click(cx.listener(move |a, ev: &ClickEvent, window, cx| {
                window.focus(&a.focus);
                let m = ev.modifiers();
                // Shift：范围选择；⌘：单个勾选 / 取消；都不按：打开邮件
                if m.shift {
                    a.select_range(id);
                    cx.notify();
                    return;
                }
                if m.platform {
                    if !a.checked.remove(&id) {
                        a.checked.insert(id);
                    }
                    a.anchor = Some(id);
                    cx.notify();
                    return;
                }
                a.anchor = Some(id);
                a.reader_collapsed = false;
                a.save_layout();
                a.open_message(id, cx)
            }))
    }
}
