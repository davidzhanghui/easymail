//! 弹出菜单：主题、导出、移动、文件夹、账号。

use super::*;

impl MailApp {
    pub(super) fn render_pop(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (pop, at) = self.pop.clone()?;
        let mut p = popover(t, 230.).id("popover").occlude().max_h(px(f32::from(window.viewport_size().height) * 0.75)).overflow_y_scroll();
        match pop {
            Pop::Snooze(ids) => {
                p = p.child(menu_title(t, "稍后处理，到时重新出现在收件箱"));
                for (i, (label, at)) in crate::core::snooze::presets().into_iter().enumerate() {
                    let ids = ids.clone();
                    p = p.child(menu_row(SharedString::from(format!("sz-{i}")), t, Some("clock"), format!("{label} · {}", fmt_full(at)), false).on_click(
                        cx.listener(move |a, _, _, cx| {
                            a.pop = None;
                            match crate::core::snooze::snooze(&a.store, &ids, at) {
                                Ok(n) => a.toast(format!("{n} 封邮件将在 {} 回到收件箱", fmt_full(at)), false, cx),
                                Err(e) => a.toast(format!("{e:#}"), true, cx),
                            }
                            a.load_list(true, false, cx);
                            a.load_meta(cx);
                        }),
                    ));
                }
            }
            Pop::Row(id) => {
                let ids = self.targets(id);
                let n = ids.len();
                let rows: Vec<&ListItem> = self.items.iter().filter(|m| ids.contains(&m.id)).collect();
                let any_unread = rows.iter().any(|m| !m.seen);
                let any_unstarred = rows.iter().any(|m| !m.flagged);
                let local = rows.first().is_some_and(|m| self.accounts.iter().any(|a| a.id == m.account_id && a.is_local()));
                let can_archive = !local && rows.first().is_some_and(|m| m.folder_role != "archive" && self.has_role(m.account_id, "archive"));
                let subject = rows.first().map(|m| m.subject.clone()).unwrap_or_default();
                p = p.w(px(220.)).child(menu_title(t, if n > 1 { format!("{n} 封邮件") } else { one_line(&subject, 22) }));
                let (i1, i2, i3, i4, i5, i6, i7) = (ids.clone(), ids.clone(), ids.clone(), ids.clone(), ids.clone(), ids.clone(), ids.clone());
                if n == 1 {
                    p = p.child(menu_row("rc-open", t, Some("envelope"), "打开", false).on_click(cx.listener(move |a, _, _, cx| {
                        a.pop = None;
                        a.reader_collapsed = false;
                        a.open_message(id, cx);
                    })));
                }
                p = p
                    .child(
                        menu_row("rc-seen", t, Some(if any_unread { "read" } else { "unread" }), if any_unread { "标为已读" } else { "标为未读" }, false)
                            .on_click(cx.listener(move |a, _, _, cx| {
                                a.pop = None;
                                a.set_seen(i1.clone(), any_unread, cx)
                            })),
                    )
                    .child(
                        menu_row(
                            "rc-star",
                            t,
                            Some(if any_unstarred { "star" } else { "star-fill" }),
                            if any_unstarred { "加星标" } else { "取消星标" },
                            false,
                        )
                        .on_click(cx.listener(move |a, _, _, cx| {
                            a.pop = None;
                            a.set_flagged(i2.clone(), any_unstarred, false, cx)
                        })),
                    )
                    .child(menu_row("rc-move", t, Some("move"), "移动到…", false).on_click(cx.listener(move |a, ev: &ClickEvent, _, cx| {
                        a.pop = Some((Pop::Move(i3.clone()), ev.position()));
                        cx.notify();
                    })))
                    .child(menu_row("rc-snooze", t, Some("clock"), "稍后处理…", false).on_click(cx.listener(move |a, ev: &ClickEvent, _, cx| {
                        a.pop = Some((Pop::Snooze(i4.clone()), ev.position()));
                        cx.notify();
                    })));
                if can_archive {
                    p = p.child(menu_row("rc-archive", t, Some("archive"), "归档", false).on_click(cx.listener(move |a, _, _, cx| {
                        a.pop = None;
                        a.to_role(i5.clone(), "archive", "已归档", cx)
                    })));
                }
                if n == 1 {
                    let s = subject.clone();
                    p = p.child(menu_row("rc-copy", t, Some("draft"), "复制主题", false).on_click(cx.listener(move |a, _, _, cx| {
                        a.pop = None;
                        cx.write_to_clipboard(ClipboardItem::new_string(s.clone()));
                        a.toast("已复制主题", false, cx);
                    })));
                }
                p = p.child(menu_row("rc-del", t, Some("trash"), "删除", false).text_color(t.danger).on_click(cx.listener(move |a, _, w, cx| {
                    a.pop = None;
                    a.delete(i6.clone(), w, cx)
                })));
                let _ = i7;
            }
            Pop::Tools => {
                p = p.w(px(270.)).child(menu_title(t, "工具 · ⌘K 打开命令面板"));
                use super::panels::Cmd;
                for (i, (c, ic, label)) in [
                    (Cmd::Senders, "users", "发件人视图 / 批量清理"),
                    (Cmd::Privacy, "eyeOff", "隐私报告"),
                    (Cmd::Outbox, "outbox", "发件箱"),
                    (Cmd::Rules, "rule", "本地规则"),
                    (Cmd::Templates, "draft", "回复模板"),
                    (Cmd::Migrate, "archive", "账号间搬家"),
                    (Cmd::Diagnose, "shield", "连接诊断"),
                    (Cmd::Contacts, "users", "联系人"),
                    (Cmd::Invoices, "receipt", "发票汇总 / 按月导出"),
                    (Cmd::Backup, "download", "完整备份（可加密）"),
                    (Cmd::Restore, "refresh", "从备份恢复…"),
                    (Cmd::Slim, "trash", "安全瘦身"),
                    (Cmd::ToggleThreads, "list", if self.threads { "改为逐封显示" } else { "按会话合并显示" }),
                    (Cmd::Prefs, "gear", "设置"),
                    (Cmd::CheckUpdate, "refresh", "检查更新"),
                ]
                .into_iter()
                .enumerate()
                {
                    p = p.child(menu_row(("tool", i), t, Some(ic), label, false).on_click(cx.listener(move |a, _, w, cx| {
                        a.pop = None;
                        a.run_cmd(c, w, cx);
                    })));
                }
            }
            Pop::Theme => {
                let cur = theme(cx).id;
                let auto = self.pref.pref == "auto";
                p = p.w(px(250.)).child(menu_title(t, format!("配色主题 · {} 套", palettes::PALETTES.len())));
                p = p.child(menu_row("th-auto", t, None, "跟随系统 · Auto", auto).on_click(cx.listener(|a, _, w, cx| {
                    let mut p = a.pref.clone();
                    p.pref = "auto".into();
                    a.set_theme_pref(p, w, cx);
                })));
                if auto {
                    for (dark, label) in [(false, "浅色时"), (true, "深色时")] {
                        let mut row = div()
                            .flex()
                            .flex_wrap()
                            .gap(px(4.))
                            .px(px(9.))
                            .py(px(4.))
                            .child(div().w(px(42.)).text_size(px(11.)).text_color(t.muted).child(label));
                        for pal in palettes::PALETTES.iter().filter(|p| p.dark == dark) {
                            let sel = if dark { self.pref.dark == pal.id } else { self.pref.light == pal.id };
                            let id = pal.id;
                            row = row.child(
                                div()
                                    .id(SharedString::from(format!("slot-{id}")))
                                    .size(px(20.))
                                    .rounded(px(5.))
                                    .bg(rgb(pal.paper))
                                    .border_2()
                                    .border_color(if sel { t.accent } else { t.border })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .child(div().size(px(9.)).rounded(px(2.5)).bg(rgb(pal.accent)))
                                    .on_click(cx.listener(move |a, _, w, cx| {
                                        let mut p = a.pref.clone();
                                        if dark {
                                            p.dark = id.into();
                                        } else {
                                            p.light = id.into();
                                        }
                                        a.set_theme_pref(p, w, cx);
                                    })),
                            );
                        }
                        p = p.child(row);
                    }
                }
                for (dark, sec) in [(false, "浅色"), (true, "深色")] {
                    p = p.child(menu_title(t, sec));
                    for pal in palettes::PALETTES.iter().filter(|p| p.dark == dark) {
                        let id = pal.id;
                        let on = !auto && cur == id;
                        p = p.child(
                            menu_row(SharedString::from(format!("th-{id}")), t, None, pal.name, on)
                                .child(
                                    div()
                                        .size(px(17.))
                                        .rounded(px(5.))
                                        .bg(rgb(pal.paper))
                                        .border_1()
                                        .border_color(t.border)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(div().size(px(9.)).rounded(px(3.)).bg(rgb(pal.accent))),
                                )
                                .on_click(cx.listener(move |a, _, w, cx| {
                                    let mut p = a.pref.clone();
                                    p.pref = id.into();
                                    a.set_theme_pref(p, w, cx);
                                    a.pop = None;
                                })),
                        );
                    }
                }
            }
            Pop::Export => {
                let n = self.checked.len();
                p = p.w(px(250.)).child(menu_title(t, if n > 0 { format!("导出选中的 {n} 封") } else { format!("导出当前列表 {} 封", fmt_total(self.total)) }));
                p = p
                    .child(
                        menu_row("ex-mbox", t, Some("archive"), "mbox（Thunderbird / Apple Mail）", false)
                            .on_click(cx.listener(|a, _, _, cx| a.export(transfer::ExportKind::Mbox, cx))),
                    )
                    .child(
                        menu_row("ex-csv", t, Some("draft"), "CSV 表格（Excel 可直接打开）", false)
                            .on_click(cx.listener(|a, _, _, cx| a.export(transfer::ExportKind::Csv, cx))),
                    )
                    .child(
                        menu_row("ex-maildir", t, Some("folder"), "Maildir / .eml 文件夹", false)
                            .on_click(cx.listener(|a, _, _, cx| a.export(transfer::ExportKind::Maildir, cx))),
                    );
            }
            Pop::Move(ids) => {
                let acc_ids: HashSet<i64> = self
                    .items
                    .iter()
                    .filter(|m| ids.contains(&m.id))
                    .map(|m| m.account_id)
                    .chain(self.detail.iter().filter(|d| ids.contains(&d.id)).map(|d| d.account_id))
                    .collect();
                p = p.child(menu_title(t, "移动到…"));
                if acc_ids.len() > 1 {
                    p = p.child(div().px(px(9.)).py(px(6.)).text_size(px(12.)).text_color(t.muted).child("选中的邮件来自多个账号，不能一起移动。"));
                } else if let Some(acc) = acc_ids.iter().next().copied() {
                    let cur: HashSet<i64> = self
                        .items
                        .iter()
                        .filter(|m| ids.contains(&m.id))
                        .map(|m| m.folder_id)
                        .chain(self.detail.iter().filter(|d| ids.contains(&d.id)).map(|d| d.folder_id))
                        .collect();
                    for f in self.folders.iter().filter(|f| f.account_id == acc && !(cur.len() == 1 && cur.contains(&f.id))) {
                        let fid = f.id;
                        let ids = ids.clone();
                        p = p.child(
                            menu_row(SharedString::from(format!("mv-{fid}")), t, Some(role_icon(&f.role)), f.label(), false)
                                .on_click(cx.listener(move |a, _, _, cx| a.move_to(ids.clone(), fid, cx))),
                        );
                    }
                }
            }
            Pop::Folder(fid) => {
                let f = self.folders.iter().find(|f| f.id == fid)?.clone();
                let local = self.accounts.iter().any(|a| a.id == f.account_id && a.is_local());
                p = p.child(menu_title(t, f.label()));
                let item = |id: &'static str, ic: &'static str, label: String, action: &'static str, cx: &mut Context<Self>| {
                    menu_row(id, t, Some(ic), label, false).on_click(cx.listener(move |a, _, w, cx| a.folder_action(action, fid, w, cx)))
                };
                p = p.child(item("fm-read", "read", "全部标为已读".into(), "read-all", cx));
                if !local {
                    p = p.child(item("fm-sync", "refresh", "立即同步此文件夹".into(), "sync", cx)).child(item(
                        "fm-toggle",
                        "eye",
                        if f.sync_enabled { "暂停同步此文件夹".into() } else { "恢复同步此文件夹".into() },
                        "toggle-sync",
                        cx,
                    ));
                }
                if f.role.is_empty() || local {
                    p = p.child(menu_sep(t)).child(item("fm-new", "plus", "新建子文件夹…".into(), "new-sub", cx));
                    if f.role.is_empty() && !f.path.eq_ignore_ascii_case("INBOX") {
                        p = p
                            .child(item("fm-rename", "edit", "重命名…".into(), "rename", cx))
                            .child(item("fm-delete", "trash", "删除文件夹…".into(), "delete", cx).text_color(t.danger));
                    }
                }
            }
            Pop::Account(aid) => {
                let a = self.accounts.iter().find(|a| a.id == aid)?.clone();
                let local = a.is_local();
                p = p.child(menu_title(t, if local { "本地归档".to_string() } else { a.email.clone() }));
                if !local {
                    p = p
                        .child(menu_row("am-sync", t, Some("refresh"), "立即同步", false).on_click(cx.listener(move |app, _, _, cx| {
                            app.pop = None;
                            app.eng.request_sync(aid, None, true);
                            app.toast("已开始同步", false, cx);
                        })))
                        .child(
                            menu_row("am-edit", t, Some("gear"), "账号设置…", false)
                                .on_click(cx.listener(|app, _, w, cx| app.open_accounts(Tab::Accounts, false, w, cx))),
                        )
                        .child(menu_row("am-new-remote", t, Some("plus"), "新建文件夹…", false).on_click(cx.listener(move |app, _, w, cx| {
                            app.open_prompt("在邮箱服务器上新建文件夹".into(), "", PromptAction::NewFolder { account_id: aid, parent: None }, w, cx)
                        })));
                } else {
                    p = p
                        .child(menu_row("am-new", t, Some("plus"), "新建文件夹…", false).on_click(cx.listener(move |app, _, w, cx| {
                            app.open_prompt("在本地归档新建文件夹".into(), "", PromptAction::NewFolder { account_id: aid, parent: None }, w, cx)
                        })))
                        .child(menu_row("am-import", t, Some("download"), "导入邮件…", false).on_click(cx.listener(|app, _, w, cx| app.open_import(w, cx))));
                }
            }
        }
        let p = p.on_mouse_down_out(cx.listener(|a, _, _, cx| {
            a.pop = None;
            a.pop_closed_at = Instant::now();
            cx.notify();
        }));
        Some(deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(p)).with_priority(1).into_any_element())
    }
}
