//! 各类弹层、模态、主题与提示条。

use super::*;

impl MailApp {
    pub(crate) fn on_evt(&mut self, e: Evt, cx: &mut Context<Self>) {
        match e {
            Evt::Sync { account_id, syncing, folder, error } => {
                self.sync.insert(account_id, SyncState { syncing, folder, error: error.clone() });
                if let Some(a) = self.accounts.iter_mut().find(|a| a.id == account_id) {
                    if !syncing {
                        a.last_error = error;
                        if a.last_error.is_empty() {
                            a.last_sync_at = Some(crate::core::db::now_ms());
                        }
                    } else if !error.is_empty() {
                        a.last_error = error;
                    }
                }
                cx.notify();
            }
            Evt::Folder { added, .. } => {
                if added > 0 {
                    self.soft_refresh(cx);
                }
            }
            Evt::Synced { .. } | Evt::Accounts => self.soft_refresh(cx),
            Evt::NewMail { email, count, from, subject, .. } => {
                let msg = format!("{email} 收到 {count} 封新邮件");
                let body = if count == 1 && !subject.is_empty() { format!("{from}：{subject}") } else { msg.clone() };
                // 主题里就带验证码的（多数短信式通知邮件如此）：通知标题直接显示，提示条给「复制」
                let code = if count == 1 { crate::core::extract::verification_code(&subject, "") } else { None };
                match &code {
                    Some(c) => {
                        self.toast_with(format!("验证码 {c}（{from}）"), false, Some(ToastAction::Copy(c.clone())), cx);
                        macos::notify(&format!("验证码 {c}"), &body);
                    }
                    None => {
                        self.toast(msg.clone(), false, cx);
                        macos::notify("新邮件", &body);
                    }
                }
                self.last_newmail = Some(msg);
                self.soft_refresh(cx);
            }
            Evt::Watch { .. } => cx.notify(),
            Evt::Toast { msg, error } => self.toast(msg, error, cx),
            Evt::Unsendable { msg, outbox_id } => self.toast_with(msg, false, Some(ToastAction::Unsend(outbox_id)), cx),
        }
    }

    pub(crate) fn toast(&mut self, msg: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        self.toast_with(msg, error, None, cx);
    }

    /// 带按钮的提示条：撤销 / 撤回类的提示停留到撤销窗口结束
    pub(crate) fn toast_with(&mut self, msg: impl Into<String>, error: bool, action: Option<ToastAction>, cx: &mut Context<Self>) {
        self.toast_seq += 1;
        let id = self.toast_seq;
        let ms = match &action {
            Some(ToastAction::Undo) => crate::core::pending::undo_window().max(3200) as u64,
            Some(ToastAction::Unsend(_)) => (crate::core::outbox::undo_secs(&self.store) * 1000).max(3200) as u64,
            Some(ToastAction::Open(_)) => 12000,
            Some(ToastAction::Copy(_)) => 15000,
            Some(ToastAction::Update(_)) => 20000,
            None if error => 6000,
            None => 3200,
        };
        self.toasts.push(Toast { id, msg: msg.into(), error, action });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(Duration::from_millis(ms)).await;
            this.update(cx, |a, cx| {
                a.toasts.retain(|t| t.id != id);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// 在引擎的运行时上执行异步操作，结束后回到界面线程
    pub(crate) fn run<T: Send + 'static>(
        &mut self,
        fut: impl Future<Output = anyhow::Result<T>> + Send + 'static,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, anyhow::Result<T>, &mut Context<Self>) + 'static,
    ) {
        let f = self.eng.call(fut);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let r = f.await;
            this.update(cx, |a, cx| done(a, r, cx)).ok();
        })
        .detach();
    }

    /// 通用：成功提示 + 刷新计数；失败提示 + 重读列表
    pub(crate) fn run_op<T: Send + 'static>(
        &mut self,
        fut: impl Future<Output = anyhow::Result<T>> + Send + 'static,
        ok: impl FnOnce(&T) -> String + 'static,
        cx: &mut Context<Self>,
    ) {
        self.run(fut, cx, move |a, r, cx| match r {
            Ok(v) => {
                let m = ok(&v);
                if !m.is_empty() {
                    a.toast(m, false, cx);
                }
                a.load_meta(cx);
            }
            Err(e) => {
                a.toast(e.to_string(), true, cx);
                a.load_list(true, false, cx);
                a.load_meta(cx);
            }
        });
    }

    /// 同 run_op，成功后的提示带「撤销」按钮（撤销窗口内有效）
    pub(crate) fn run_op_undo<T: Send + 'static>(
        &mut self,
        fut: impl Future<Output = anyhow::Result<T>> + Send + 'static,
        ok: impl FnOnce(&T) -> String + 'static,
        cx: &mut Context<Self>,
    ) {
        self.run(fut, cx, move |a, r, cx| match r {
            Ok(v) => {
                let undoable = a.eng.peek_last_undo().is_some_and(|u| crate::core::db::now_ms() - u.at < 2_000);
                a.toast_with(ok(&v), false, undoable.then_some(ToastAction::Undo), cx);
                a.load_meta(cx);
            }
            Err(e) => {
                a.toast(e.to_string(), true, cx);
                a.load_list(true, false, cx);
                a.load_meta(cx);
            }
        });
    }

    pub(crate) fn toast_click(&mut self, action: ToastAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            ToastAction::Undo => self.undo(cx),
            ToastAction::Unsend(id) => self.unsend(id, window, cx),
            ToastAction::Open(url) => cx.open_url(&url),
            ToastAction::Update(rel) => self.open_update(rel, cx),
            ToastAction::Copy(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.toast("已复制", false, cx);
            }
        }
    }

    pub(crate) fn compose_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sendable: Vec<&Account> = self.accounts.iter().filter(|a| !a.is_local()).collect();
        if sendable.is_empty() {
            self.open_accounts(Tab::Accounts, true, window, cx);
            return;
        }
        let in_folder = match self.nav {
            Nav::Folder(fid) => self.folders.iter().find(|f| f.id == fid).and_then(|f| sendable.iter().find(|a| a.id == f.account_id)).copied(),
            _ => None,
        };
        let acc = in_folder.unwrap_or(sendable[0]);
        let d = Draft {
            mode: "new".into(),
            account_id: Some(acc.id),
            body: if acc.signature.is_empty() { String::new() } else { format!("\n\n-- \n{}", acc.signature) },
            ..Default::default()
        };
        self.open_composer(d, window, cx);
    }

    pub(crate) fn compose_to(&mut self, to: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(acc) = self.accounts.iter().find(|a| !a.is_local()).cloned() else {
            self.open_accounts(Tab::Accounts, true, window, cx);
            return;
        };
        let d = Draft {
            account_id: Some(acc.id),
            to: to.to_string(),
            body: if acc.signature.is_empty() { String::new() } else { format!("\n\n-- \n{}", acc.signature) },
            ..Default::default()
        };
        self.open_composer(d, window, cx);
    }

    pub(crate) fn reply(&mut self, mode: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.as_ref().filter(|_| self.detail_state == DetailState::Ready) else { return };
        if !self.accounts.iter().any(|a| !a.is_local()) {
            self.open_accounts(Tab::Accounts, true, window, cx);
            return;
        }
        let draft = build_reply(d, mode, &self.accounts);
        self.open_composer(draft, window, cx);
    }

    pub(crate) fn open_composer(&mut self, d: Draft, _window: &mut Window, cx: &mut Context<Self>) {
        self.pop = None;
        crate::ui::composer_window::open(self.eng.clone(), self.accounts.clone(), d, cx);
        cx.notify();
    }

    pub(crate) fn open_accounts(&mut self, tab: Tab, adding: bool, window: &mut Window, cx: &mut Context<Self>) {
        let eng = self.eng.clone();
        let m = cx.new(|cx| AccountsModal::new(eng, tab, adding, window, cx));
        let sub = cx.subscribe_in(&m, window, |this: &mut Self, _, ev: &AccountsEvent, window, cx| match ev {
            AccountsEvent::Close => {
                this.modal = None;
                window.focus(&this.focus);
                this.load_meta(cx);
                this.load_list(true, false, cx);
            }
            AccountsEvent::Changed => {
                this.load_meta(cx);
                this.load_list(true, false, cx);
            }
            AccountsEvent::Toast(m, e) => this.toast(m.clone(), *e, cx),
        });
        self.modal_sub = Some(sub);
        self.pop = None;
        self.modal = Some(Modal::Accounts(m));
        cx.notify();
    }

    pub(crate) fn open_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let eng = self.eng.clone();
        let m = cx.new(|cx| ImportModal::new(eng, window, cx));
        let sub = cx.subscribe_in(&m, window, |this: &mut Self, _, ev: &ImportEvent, window, cx| match ev {
            ImportEvent::Close => {
                this.modal = None;
                window.focus(&this.focus);
                this.load_meta(cx);
            }
            ImportEvent::Done { folder_id } => {
                this.load_meta(cx);
                this.set_nav(Nav::Folder(*folder_id), cx);
            }
            ImportEvent::Toast(m, e) => this.toast(m.clone(), *e, cx),
        });
        self.modal_sub = Some(sub);
        self.pop = None;
        self.modal = Some(Modal::Import(m));
        cx.notify();
    }

    pub(crate) fn open_prompt(&mut self, title: String, initial: &str, action: PromptAction, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            let mut i = LineInput::new("文件夹名称", cx);
            i.set_text(initial, cx);
            i
        });
        let sub = cx.subscribe_in(&input, window, |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::Submit { .. } => this.submit_prompt(window, cx),
            InputEvent::Cancel => {
                this.modal = None;
                window.focus(&this.focus);
                cx.notify();
            }
            _ => {}
        });
        self.modal_sub = Some(sub);
        window.focus(&input.focus_handle(cx));
        self.pop = None;
        self.modal = Some(Modal::Prompt { title, input, action });
        cx.notify();
    }

    pub(crate) fn submit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Prompt { input, action, .. }) = &self.modal else { return };
        let name = input.read(cx).text().trim().to_string();
        let action = action.clone();
        if name.is_empty() {
            return;
        }
        let eng = self.eng.clone();
        let fut: std::pin::Pin<Box<dyn Future<Output = anyhow::Result<&'static str>> + Send>> = match action {
            PromptAction::NewFolder { account_id, parent } => {
                Box::pin(async move { ops::create_folder_any(eng, account_id, name, parent).await.map(|_| "文件夹已创建") })
            }
            PromptAction::Rename { folder_id } => Box::pin(async move { ops::rename_folder_any(eng, folder_id, name).await.map(|_| "已重命名") }),
        };
        self.run(fut, cx, |a, r, cx| match r {
            Ok(m) => {
                if matches!(a.modal, Some(Modal::Prompt { .. })) {
                    a.modal = None;
                }
                a.toast(m, false, cx);
                a.load_meta(cx);
                a.load_list(true, false, cx);
            }
            Err(e) => a.toast(format!("{e:#}"), true, cx),
        });
        window.focus(&self.focus);
    }

    pub(crate) fn set_theme_pref(&mut self, pref: ThemePref, window: &mut Window, cx: &mut Context<Self>) {
        self.pref = pref;
        self.store.set_kv("theme", &serde_json::to_string(&(&self.pref.pref, &self.pref.light, &self.pref.dark)).unwrap_or_default());
        self.apply_theme(window.appearance(), cx);
    }

    pub(crate) fn apply_theme(&mut self, appearance: WindowAppearance, cx: &mut Context<Self>) {
        cx.set_global(Theme::by_id(self.pref.resolve(appearance)));
        self.doc = None;
        cx.notify();
    }

    pub(crate) fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cur = theme(cx).id;
        let ix = palettes::PALETTES.iter().position(|p| p.id == cur).unwrap_or(0);
        let next = palettes::PALETTES[(ix + 1) % palettes::PALETTES.len()].id;
        let mut p = self.pref.clone();
        p.pref = next.into();
        self.set_theme_pref(p, window, cx);
        self.toast(format!("配色：{}", palette(next).name), false, cx);
    }

    pub(crate) fn toggle_pop(&mut self, pop: Pop, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.pop.as_ref().is_some_and(|p| p.0 == pop) || (self.pop.is_none() && self.pop_closed_at.elapsed() < Duration::from_millis(250)) {
            self.pop = None;
        } else {
            self.pop = Some((pop, at));
        }
        cx.notify();
    }

    /// 测试用：在固定位置打开弹层
    #[cfg(feature = "dev")]
    pub(crate) fn toggle_pop_for_test(&mut self, pop: Pop, cx: &mut Context<Self>) {
        self.pop = Some((pop, point(px(900.), px(60.))));
        cx.notify();
    }

    pub(crate) fn overlay_open(&self) -> bool {
        self.pop.is_some() || self.modal.is_some()
    }

    /// 弹层 / 模态打开时把 WebView 冻结成快照（原生视图会盖住 GPUI 画的弹层）
    pub(crate) fn sync_freeze(&mut self, cx: &mut Context<Self>) {
        let want = self.overlay_open();
        if let super::freeze::Step::Capture = self.freeze.sync(want) {
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let png = super::freeze::Freeze::capture(cx).await;
                this.update(cx, |a, cx| {
                    let still = a.overlay_open();
                    a.freeze.finish(png, still);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    pub(crate) fn render_header(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let syncing: Vec<(&Account, &SyncState)> = self.accounts.iter().filter_map(|a| self.sync.get(&a.id).filter(|s| s.syncing).map(|s| (a, s))).collect();
        let last = self.accounts.iter().filter(|a| !a.is_local()).filter_map(|a| a.last_sync_at).max();
        let real = self.accounts.iter().any(|a| !a.is_local());
        let pill = if let Some((a, s)) = syncing.first() {
            format!("同步中 · {}", if s.folder.is_empty() { a.email.clone() } else { s.folder.clone() })
        } else if real {
            format!("已同步 {}", fmt_ago(last))
        } else {
            "未接入邮箱".into()
        };
        let q_empty = self.search.read(cx).text().is_empty();
        let focused = self.search.read(cx).is_focused(window);
        let cur = palette(theme(cx).id);
        let auto = self.pref.pref == "auto";
        div()
            .id("header")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(14.))
            .h(px(64.))
            .pl(px(86.))
            .pr(px(22.))
            .bg(t.panel)
            .border_b_1()
            .border_color(t.border)
            .on_mouse_down(MouseButton::Left, |ev, window, _| title_drag(ev, window))
            .child(
                div().flex().items_center().gap(px(12.)).min_w(px(170.)).child(logo(34.)).child(
                    div()
                        .child(div().font_family(SERIF).text_size(px(17.)).font_weight(FontWeight::BOLD).text_color(t.ink).child(crate::core::config::APP_NAME))
                        .child(div().font_family(SERIF).text_size(px(9.)).text_color(t.muted).child("E A S Y M A I L")),
                ),
            )
            .child(
                field_box(t, focused)
                    .id("searchbar")
                    .flex_1()
                    .max_w(px(560.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(icon("search", 15., t.muted))
                    .child(div().flex_1().min_w_0().child(self.search.clone()))
                    .map(|d| {
                        if q_empty {
                            d.child(
                                div()
                                    .px(px(6.))
                                    .h(px(18.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(t.border)
                                    .text_size(px(11.))
                                    .text_color(t.muted)
                                    .child("/"),
                            )
                        } else {
                            d.child(button("clear-q", t, Kind::Ghost, Some("close"), "", true).on_click(cx.listener(|a, _, window, cx| {
                                a.search.update(cx, |s, cx| s.set_text("", cx));
                                a.set_query(String::new(), cx);
                                window.focus(&a.focus);
                            })))
                        }
                    }),
            )
            .child(div().flex_1())
            .child(
                button("sync-pill", t, Kind::Ghost, Some("refresh"), pill, false)
                    .text_color(if syncing.is_empty() { t.ink_2 } else { t.accent_deep })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|a, _, _, cx| a.sync_now(cx))),
            )
            .child(
                button("theme-btn", t, Kind::Secondary, Some("palette"), if auto { "跟随系统".to_string() } else { cur.name.to_string() }, false)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|a, ev: &ClickEvent, _, cx| {
                        let p = ev.position();
                        a.toggle_pop(Pop::Theme, point(p.x - px(150.), px(56.)), cx)
                    })),
            )
            .child(
                button("compose", t, Kind::Primary, Some("pen"), "写信", false)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|a, _, window, cx| a.compose_new(window, cx))),
            )
    }
}
