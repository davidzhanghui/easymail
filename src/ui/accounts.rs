//! 账号与设置：账号列表（同步 / 暂停 / 编辑 / 移除）、
//! 添加 / 编辑表单（服务商预设、帮助步骤、连接测试）、同步与网络设置。

use super::fmt_ago;
use super::line_input::{InputEvent, LineInput};
use super::text_area::TextArea;
use super::theme::{theme, Theme};
use super::widgets::*;
use crate::core::accounts::{self, AccountInput, AccountPatch};
use crate::core::engine::Engine;
use crate::core::model::Account;
use crate::core::providers::{self, PROVIDERS};
use crate::core::{imap, smtp};
use gpui::prelude::FluentBuilder;
use gpui::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tab {
    Accounts,
    Settings,
}

pub enum AccountsEvent {
    Close,
    Changed,
    Toast(String, bool),
}

impl EventEmitter<AccountsEvent> for AccountsModal {}

#[derive(Clone, PartialEq, Debug)]
enum Editing {
    None,
    New,
    Edit(i64),
}

#[derive(Clone, Debug)]
pub struct TestResult {
    pub imap: Result<usize, String>,
    pub smtp: Result<(), String>,
    pub error: Option<String>,
}

struct Form {
    provider: String,
    email: Entity<LineInput>,
    password: Entity<LineInput>,
    name: Entity<LineInput>,
    imap_host: Entity<LineInput>,
    imap_port: Entity<LineInput>,
    smtp_host: Entity<LineInput>,
    smtp_port: Entity<LineInput>,
    signature: Entity<TextArea>,
    use_proxy: bool,
    show_pw: bool,
}

struct SettingsForm {
    proxy: Entity<LineInput>,
    interval: Entity<LineInput>,
    initial: Entity<LineInput>,
    prefetch: Entity<LineInput>,
    backfill: bool,
    // 以下存在 kv 里（键名与读取处保持一致）
    undo_secs: Entity<LineInput>,
    check_updates: bool,
    strip_tracking: bool,
    export_dir: Entity<LineInput>,
    export_days: Entity<LineInput>,
    ms_client_id: Entity<LineInput>,
    google_client_id: Entity<LineInput>,
    google_client_secret: Entity<LineInput>,
}

pub struct AccountsModal {
    eng: Engine,
    pub tab: Tab,
    pub accounts: Vec<Account>,
    editing: Editing,
    form: Option<Form>,
    settings: SettingsForm,
    pub busy: Option<&'static str>,
    pub result: Option<TestResult>,
    _subs: Vec<Subscription>,
}

fn input(cx: &mut Context<AccountsModal>, text: &str, ph: &str) -> Entity<LineInput> {
    let (text, ph) = (text.to_string(), ph.to_string());
    cx.new(move |cx| {
        let mut i = LineInput::new(&ph, cx);
        i.set_text(&text, cx);
        i.collapse_to_end(cx);
        i
    })
}

impl AccountsModal {
    pub fn new(eng: Engine, tab: Tab, adding: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let accounts: Vec<Account> = accounts::list(eng.store()).unwrap_or_default().into_iter().filter(|a| !a.is_local()).collect();
        let s = eng.store().settings();
        let kv = |k: &str, d: &str| eng.store().get_kv(k).unwrap_or_else(|| d.to_string());
        let settings = SettingsForm {
            proxy: input(cx, &s.proxy_url, "socks5://127.0.0.1:1080 或 http://127.0.0.1:1087"),
            interval: input(cx, &s.sync_interval_min.to_string(), ""),
            initial: input(cx, &s.initial_sync_per_folder.to_string(), ""),
            prefetch: input(cx, &s.prefetch_bodies.to_string(), ""),
            backfill: s.backfill,
            undo_secs: input(cx, &kv("undo_send_secs", "5"), "5"),
            check_updates: kv("check_updates", "1") != "0",
            strip_tracking: kv("strip_tracking", "1") != "0",
            export_dir: input(cx, &kv("auto_export_dir", ""), "留空 = 关闭"),
            export_days: input(cx, &kv("auto_export_days", "7"), "7"),
            ms_client_id: input(cx, &kv("oauth.ms_client_id", ""), "留空使用内置"),
            google_client_id: input(cx, &kv("oauth.google_client_id", ""), "Gmail OAuth 登录需要"),
            google_client_secret: {
                let e = input(cx, &kv("oauth.google_client_secret", ""), "");
                e.update(cx, |i, cx| i.set_masked(true, cx));
                e
            },
        };
        let mut m = AccountsModal { eng, tab, accounts, editing: Editing::None, form: None, settings, busy: None, result: None, _subs: vec![] };
        if adding || m.accounts.is_empty() {
            m.start_edit(Editing::New, window, cx);
        }
        m
    }

    fn reload(&mut self) {
        self.accounts = accounts::list(self.eng.store()).unwrap_or_default().into_iter().filter(|a| !a.is_local()).collect();
    }

    fn start_edit(&mut self, e: Editing, window: &mut Window, cx: &mut Context<Self>) {
        let acc = match &e {
            Editing::Edit(id) => self.accounts.iter().find(|a| a.id == *id).cloned(),
            _ => None,
        };
        let provider = acc.as_ref().map(|a| a.provider.clone()).unwrap_or_else(|| "netease163".into());
        let p = providers::provider(&provider);
        let email = input(cx, acc.as_ref().map(|a| a.email.as_str()).unwrap_or(""), p.placeholder);
        let password = input(cx, "", if acc.is_some() { "留空表示不修改" } else { p.password_label });
        password.update(cx, |i, cx| i.set_masked(true, cx));
        let name = input(cx, acc.as_ref().map(|a| a.name.as_str()).unwrap_or(""), "发信时对方看到的名字");
        let sig = acc.as_ref().map(|a| a.signature.clone()).unwrap_or_default();
        let signature = cx.new(|cx| TextArea::new(&sig, "写信 / 回复时自动附在正文后", cx));
        let form = Form {
            provider,
            email: email.clone(),
            password: password.clone(),
            name,
            imap_host: input(cx, "", "imap.example.com"),
            imap_port: input(cx, "993", ""),
            smtp_host: input(cx, "", "smtp.example.com"),
            smtp_port: input(cx, "465", ""),
            signature,
            use_proxy: acc.as_ref().map(|a| a.use_proxy).unwrap_or(p.needs_proxy),
            show_pw: false,
        };
        // 输入邮箱时自动识别服务商
        self._subs.push(cx.subscribe(&email, |m: &mut Self, _, ev: &InputEvent, cx| {
            if let InputEvent::Changed = ev {
                m.result = None;
                if m.editing == Editing::New {
                    if let Some(f) = m.form.as_mut() {
                        let v = f.email.read(cx).text();
                        let id = providers::detect_provider(v.trim());
                        if id != "custom" && id != f.provider {
                            f.provider = id.to_string();
                            f.use_proxy = providers::provider(id).needs_proxy;
                            let p = providers::provider(id);
                            f.password.update(cx, |i, cx| i.set_placeholder(p.password_label, cx));
                        }
                    }
                }
                cx.notify();
            }
        }));
        let focus = if acc.is_some() { password.focus_handle(cx) } else { email.focus_handle(cx) };
        window.focus(&focus);
        self.form = Some(form);
        self.editing = e;
        self.result = None;
        cx.notify();
    }

    fn pick_provider(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(f) = self.form.as_mut() {
            f.provider = id.into();
            let p = providers::provider(id);
            f.use_proxy = p.needs_proxy;
            f.email.update(cx, |i, cx| i.set_placeholder(p.placeholder, cx));
            f.password.update(cx, |i, cx| i.set_placeholder(p.password_label, cx));
        }
        self.result = None;
        cx.notify();
    }

    fn input_of(&self, cx: &App) -> Option<AccountInput> {
        let f = self.form.as_ref()?;
        let custom = f.provider == "custom" && self.editing == Editing::New;
        Some(AccountInput {
            email: f.email.read(cx).text(),
            password: f.password.read(cx).text(),
            name: f.name.read(cx).text(),
            provider: f.provider.clone(),
            use_proxy: Some(f.use_proxy),
            signature: f.signature.read(cx).text().to_string(),
            imap_host: if custom { f.imap_host.read(cx).text().trim().to_string() } else { String::new() },
            imap_port: if custom { f.imap_port.read(cx).text().trim().parse().unwrap_or(993) } else { 0 },
            smtp_host: if custom { f.smtp_host.read(cx).text().trim().to_string() } else { String::new() },
            smtp_port: if custom { f.smtp_port.read(cx).text().trim().parse().unwrap_or(465) } else { 0 },
        })
    }

    /// OAuth2 登录（Outlook / Microsoft 365；Gmail 需在「设置」里填自己的 Google Client ID）：
    /// 打开浏览器授权，回调到本机临时端口，拿到 refresh token 后直接保存账号。
    fn oauth_login(&mut self, cx: &mut Context<Self>) {
        let Some(inp) = self.input_of(cx) else { return };
        let Some(kind) = crate::core::oauth::Kind::for_provider(&inp.provider) else { return };
        let store = self.eng.store().clone();
        let (client_id, secret) = match crate::core::oauth::client_for(&store, kind) {
            Ok(c) => c,
            Err(e) => {
                self.result = Some(TestResult { imap: Err(String::new()), smtp: Err(String::new()), error: Some(e.to_string()) });
                cx.notify();
                return;
            }
        };
        self.busy = Some("oauth");
        self.result = None;
        cx.notify();
        let proxy = if inp.use_proxy.unwrap_or(false) { self.eng.proxy() } else { None };
        let hint = inp.email.trim().to_string();
        let fut = self.eng.call(async move {
            crate::core::oauth::login(kind, &client_id, &secret, &hint, proxy.as_ref(), |url| {
                let _ = std::process::Command::new("open").arg(url).status();
            })
            .await
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let r = fut.await;
            this.update(cx, |m, cx| {
                m.busy = None;
                let res = r.and_then(|tok| {
                    let mut inp = inp.clone();
                    if inp.email.trim().is_empty() {
                        inp.email = tok.email.clone();
                    }
                    let d = accounts::draft(&inp, None)?;
                    let acc = accounts::save_oauth(&store, &d, &tok)?;
                    if !inp.name.trim().is_empty() || !inp.signature.is_empty() {
                        let patch = AccountPatch {
                            name: Some(inp.name.trim().to_string()),
                            signature: Some(inp.signature.clone()),
                            use_proxy: inp.use_proxy,
                            sync_enabled: None,
                            password: None,
                        };
                        let _ = accounts::update(&store, acc.id, &patch);
                    }
                    Ok(acc)
                });
                match res {
                    Ok(acc) => {
                        m.eng.restart_account(acc.id);
                        m.eng.request_sync(acc.id, None, true);
                        cx.emit(AccountsEvent::Toast(format!("已通过 OAuth 登录 {}，正在后台同步…", acc.email), false));
                        cx.emit(AccountsEvent::Changed);
                        m.editing = Editing::None;
                        m.form = None;
                        m.reload();
                    }
                    Err(e) => {
                        m.result = Some(TestResult { imap: Err(String::new()), smtp: Err(String::new()), error: Some(format!("OAuth 登录失败：{e:#}")) })
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 自定义服务商：按邮箱域名查自动配置（域名自己的 autoconfig → Thunderbird 公共库）
    fn autoconfig(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.form.as_ref() else { return };
        let email = f.email.read(cx).text();
        let proxy = if f.use_proxy { self.eng.proxy() } else { None };
        self.busy = Some("auto");
        self.result = None;
        cx.notify();
        let fut = self.eng.call(async move { providers::autoconfig(&email, proxy.as_ref()).await });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let r = fut.await;
            this.update(cx, |m, cx| {
                m.busy = None;
                match r {
                    Ok(ac) => {
                        if let Some(f) = m.form.as_ref() {
                            let sv = ac.server.clone();
                            f.imap_host.update(cx, |i, cx| i.set_text(&sv.imap_host, cx));
                            f.imap_port.update(cx, |i, cx| i.set_text(&sv.imap_port.to_string(), cx));
                            f.smtp_host.update(cx, |i, cx| i.set_text(&sv.smtp_host, cx));
                            f.smtp_port.update(cx, |i, cx| i.set_text(&sv.smtp_port.to_string(), cx));
                        }
                        cx.emit(AccountsEvent::Toast(format!("已从{}填好服务器地址", ac.source), false));
                    }
                    Err(e) => {
                        m.result =
                            Some(TestResult {
                                imap: Err(String::new()), smtp: Err(String::new()), error: Some(format!("没找到自动配置：{e:#}，请手动填写"))
                            })
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn existing(&self) -> Option<Account> {
        match self.editing {
            Editing::Edit(id) => self.accounts.iter().find(|a| a.id == id).cloned(),
            _ => None,
        }
    }

    /// 测试连接（save=true 时测试通过后直接保存 / 添加）
    fn test(&mut self, save: bool, cx: &mut Context<Self>) {
        let Some(inp) = self.input_of(cx) else { return };
        let existing = self.existing();
        let acc = match accounts::draft(&inp, existing.as_ref()) {
            Ok(a) => a,
            Err(e) => {
                self.result = Some(TestResult { imap: Err(String::new()), smtp: Err(String::new()), error: Some(e.to_string()) });
                cx.notify();
                return;
            }
        };
        let store = self.eng.store().clone();
        let pw = if inp.password.trim().is_empty() {
            match existing.as_ref().map(|e| accounts::password(&store, e)) {
                Some(Ok(p)) => p,
                _ => {
                    self.result = Some(TestResult { imap: Err(String::new()), smtp: Err(String::new()), error: Some("请填写授权码 / 密码".into()) });
                    cx.notify();
                    return;
                }
            }
        } else {
            accounts::clean_password(&acc.provider, &inp.password)
        };
        // 编辑时只改了名称 / 签名：不必重新测试
        if save && existing.is_some() && inp.password.trim().is_empty() && existing.as_ref().is_some_and(|e| e.use_proxy == acc.use_proxy) {
            self.save(acc, inp, cx);
            return;
        }
        self.busy = Some(if save { "save" } else { "test" });
        self.result = None;
        cx.notify();
        let proxy = self.eng.proxy();
        let a2 = acc.clone();
        let fut = self.eng.call(async move {
            let (i, s) = futures::join!(imap::test_login(&a2, &pw, proxy.as_ref()), smtp::test_login(&a2, &pw, proxy.as_ref()));
            Ok(TestResult {
                imap: i.map_err(|e| imap::explain_login_error(&a2, &e.to_string())),
                smtp: s.map_err(|e| imap::explain_login_error(&a2, &e.to_string())),
                error: None,
            })
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let r: anyhow::Result<TestResult> = fut.await;
            this.update(cx, |m, cx| {
                m.busy = None;
                let r = r.unwrap_or_else(|e| TestResult { imap: Err(e.to_string()), smtp: Err(String::new()), error: None });
                let ok = r.imap.is_ok();
                // 收信通过就保存；发信没通过要明确告诉用户，别让人以为账号完全可用
                let smtp_err = r.smtp.as_ref().err().filter(|e| !e.is_empty()).cloned();
                m.result = Some(r);
                if save && ok {
                    m.save(acc, inp, cx);
                    if let Some(e) = smtp_err {
                        cx.emit(AccountsEvent::Toast(format!("账号已保存，可以收信；但发信（SMTP）测试没通过，暂时发不出邮件：{e}"), true));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save(&mut self, acc: Account, inp: AccountInput, cx: &mut Context<Self>) {
        let store = self.eng.store().clone();
        let r = match self.existing() {
            Some(e) => {
                let patch = AccountPatch {
                    name: Some(inp.name.trim().to_string()),
                    signature: Some(inp.signature.clone()),
                    use_proxy: inp.use_proxy,
                    sync_enabled: None,
                    password: (!inp.password.trim().is_empty()).then(|| inp.password.clone()),
                };
                accounts::update(&store, e.id, &patch).map(|(a, changed)| {
                    if changed {
                        self.eng.restart_account(a.id);
                    }
                    "账号已更新"
                })
            }
            None => accounts::create(&store, &acc, &inp.password, &inp.name, &inp.signature).map(|a| {
                self.eng.restart_account(a.id);
                self.eng.request_sync(a.id, None, true);
                "已添加，正在后台同步…"
            }),
        };
        match r {
            Ok(msg) => {
                cx.emit(AccountsEvent::Toast(msg.into(), false));
                cx.emit(AccountsEvent::Changed);
                self.editing = Editing::None;
                self.form = None;
                self.result = None;
                self.reload();
            }
            Err(e) => self.result = Some(TestResult { imap: Err(String::new()), smtp: Err(String::new()), error: Some(e.to_string()) }),
        }
        cx.notify();
    }

    fn remove(&mut self, a: Account, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("移除账号 {}？", a.email),
            Some("只删除本机的存档与凭据，服务器上的邮件不受影响。"),
            &["移除", "取消"],
            cx,
        );
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if answer.await.ok() == Some(0) {
                this.update(cx, |m, cx| {
                    m.eng.stop_account(a.id);
                    match accounts::delete(m.eng.store(), a.id) {
                        Ok(()) => cx.emit(AccountsEvent::Toast(format!("已移除 {}", a.email), false)),
                        Err(e) => cx.emit(AccountsEvent::Toast(e.to_string(), true)),
                    }
                    if m.editing == Editing::Edit(a.id) {
                        m.editing = Editing::None;
                        m.form = None;
                    }
                    m.reload();
                    cx.emit(AccountsEvent::Changed);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn toggle_sync(&mut self, a: &Account, cx: &mut Context<Self>) {
        let on = !a.sync_enabled;
        match accounts::update(self.eng.store(), a.id, &AccountPatch { sync_enabled: Some(on), ..Default::default() }) {
            Ok(_) => {
                if on {
                    self.eng.restart_account(a.id);
                } else {
                    self.eng.stop_account(a.id);
                }
                self.reload();
                cx.emit(AccountsEvent::Changed);
            }
            Err(e) => cx.emit(AccountsEvent::Toast(e.to_string(), true)),
        }
        cx.notify();
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        let store = self.eng.store();
        let mut s = store.settings();
        let num = |e: &Entity<LineInput>, cx: &App| e.read(cx).text().trim().parse::<u32>().ok();
        s.proxy_url = self.settings.proxy.read(cx).text().trim().to_string();
        if let Some(v) = num(&self.settings.interval, cx) {
            s.sync_interval_min = v.max(1);
        }
        if let Some(v) = num(&self.settings.initial, cx) {
            s.initial_sync_per_folder = v.max(50);
        }
        if let Some(v) = num(&self.settings.prefetch, cx) {
            s.prefetch_bodies = v;
        }
        s.backfill = self.settings.backfill;
        if !s.proxy_url.is_empty() && crate::core::net::Proxy::parse(&s.proxy_url).is_err() {
            cx.emit(AccountsEvent::Toast("代理地址格式不对：例如 socks5://127.0.0.1:1080".into(), true));
            return;
        }
        let text = |e: &Entity<LineInput>, cx: &App| e.read(cx).text().trim().to_string();
        let f = &self.settings;
        let flag = |b: bool| if b { "1" } else { "0" };
        let undo = num(&f.undo_secs, cx).unwrap_or(5).min(60);
        let days = num(&f.export_days, cx).unwrap_or(7).max(1);
        match store.save_settings(&s) {
            Ok(_) => {
                store.set_kv("undo_send_secs", &undo.to_string());
                store.set_kv("check_updates", flag(f.check_updates));
                store.set_kv("strip_tracking", flag(f.strip_tracking));
                store.set_kv("auto_export_dir", &text(&f.export_dir, cx));
                store.set_kv("auto_export_days", &days.to_string());
                store.set_kv("oauth.ms_client_id", &text(&f.ms_client_id, cx));
                store.set_kv("oauth.google_client_id", &text(&f.google_client_id, cx));
                store.set_kv("oauth.google_client_secret", &text(&f.google_client_secret, cx));
                cx.emit(AccountsEvent::Toast("设置已保存".into(), false));
            }
            Err(e) => cx.emit(AccountsEvent::Toast(e.to_string(), true)),
        }
    }

    fn field(&self, t: &Theme, e: &Entity<LineInput>, window: &Window, cx: &App) -> Div {
        field_box(t, e.read(cx).is_focused(window)).flex_1().child(div().flex_1().min_w_0().child(e.clone()))
    }

    fn render_form(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let f = self.form.as_ref()?;
        let editing = self.existing();
        let p = providers::provider(&f.provider);
        let mut d = div().mt(px(18.)).pt(px(16.)).border_t_1().border_color(t.border_soft).child(section_title(
            t,
            match &editing {
                Some(a) => format!("编辑 · {}", a.email),
                None => "添加邮箱".into(),
            },
        ));
        if editing.is_none() {
            let mut row = div().flex().flex_wrap().gap(px(6.));
            for pr in PROVIDERS.iter() {
                let id = pr.id;
                row = row.child(
                    chip(SharedString::from(format!("pv-{id}")), t, pr.name, f.provider == id)
                        .on_click(cx.listener(move |m, _, _, cx| m.pick_provider(id, cx))),
                );
            }
            d = d.child(form_row(t, "服务商", false).child(row));
            let mut help = div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .mb(px(12.))
                .ml(px(96.))
                .p(px(10.))
                .rounded(px(8.))
                .bg(t.panel_2)
                .text_size(px(12.))
                .text_color(t.ink_2)
                .line_height(px(20.));
            for (i, h) in p.help.iter().enumerate() {
                help = help.child(format!("{}. {h}", i + 1));
            }
            d = d.child(help);
            d = d.child(form_row(t, "邮箱地址", false).child(self.field(t, &f.email, window, cx)));
        }
        let show_pw = f.show_pw;
        d = d.child(form_row(t, if p.password_label.is_empty() { "授权码" } else { p.password_label }, false).child(
            self.field(t, &f.password, window, cx).child(button("pw-eye", t, Kind::Ghost, Some(if show_pw { "eyeOff" } else { "eye" }), "", true).on_click(
                cx.listener(|m, _, _, cx| {
                    if let Some(f) = m.form.as_mut() {
                        f.show_pw = !f.show_pw;
                        let masked = !f.show_pw;
                        f.password.update(cx, |i, cx| i.set_masked(masked, cx));
                    }
                    cx.notify();
                }),
            )),
        ));
        d = d.child(form_row(t, "显示名称", false).child(self.field(t, &f.name, window, cx)));
        let oauth_kind = crate::core::oauth::Kind::for_provider(&f.provider);
        if let (Some(kind), None) = (oauth_kind, &editing) {
            let busy = self.busy == Some("oauth");
            let label = match kind {
                crate::core::oauth::Kind::Microsoft => "用 Microsoft 账号登录（推荐）",
                crate::core::oauth::Kind::Google => "用 Google 账号登录（OAuth）",
            };
            d = d.child(
                form_row(t, "OAuth", false)
                    .child(
                        disabled(
                            button("acc-oauth", t, Kind::Primary, Some("shield"), if busy { "等待浏览器授权…" } else { label }, false),
                            self.busy.is_some(),
                        )
                        .on_click(cx.listener(|m, _, _, cx| {
                            if m.busy.is_none() {
                                m.oauth_login(cx)
                            }
                        })),
                    )
                    .child(div().text_size(px(11.5)).text_color(t.muted).child("在浏览器里授权，不需要授权码；下面的密码可以不填")),
            );
        }
        if f.provider == "custom" && editing.is_none() {
            d = d.child(
                form_row(t, "自动配置", false).child(
                    disabled(
                        button(
                            "acc-auto",
                            t,
                            Kind::Secondary,
                            Some("search"),
                            if self.busy == Some("auto") { "查找中…" } else { "按邮箱自动查找服务器" },
                            true,
                        ),
                        self.busy.is_some(),
                    )
                    .on_click(cx.listener(|m, _, _, cx| {
                        if m.busy.is_none() {
                            m.autoconfig(cx)
                        }
                    })),
                ),
            );
            d = d
                .child(
                    form_row(t, "IMAP 服务器", false)
                        .child(self.field(t, &f.imap_host, window, cx))
                        .child(self.field(t, &f.imap_port, window, cx).flex_none().w(px(80.))),
                )
                .child(
                    form_row(t, "SMTP 服务器", false)
                        .child(self.field(t, &f.smtp_host, window, cx))
                        .child(self.field(t, &f.smtp_port, window, cx).flex_none().w(px(80.))),
                );
        }
        d = d.child(form_row(t, "签名", true).child(
            div().flex_1().h(px(76.)).p(px(8.)).rounded(px(8.)).border_1().border_color(t.border).bg(t.paper).text_size(px(13.)).child(f.signature.clone()),
        ));
        d = d.child(form_row(t, "网络", false).child(
            check_row("use-proxy", t, f.use_proxy, "通过代理连接（Gmail 在国内必须勾选；代理地址在「同步设置」里改）").on_click(cx.listener(|m, _, _, cx| {
                if let Some(f) = m.form.as_mut() {
                    f.use_proxy = !f.use_proxy;
                }
                m.result = None;
                cx.notify();
            })),
        ));
        if let Some(r) = &self.result {
            let mut box_ =
                div().ml(px(96.)).mb(px(12.)).p(px(10.)).rounded(px(8.)).bg(t.panel_2).text_size(px(12.)).line_height(px(20.)).flex().flex_col().gap(px(2.));
            if let Some(e) = &r.error {
                box_ = box_.child(div().text_color(t.danger).child(e.clone()));
            } else {
                box_ = box_
                    .child(match &r.imap {
                        Ok(n) => div().text_color(t.live).child(format!("IMAP 收信：连接成功，{n} 个文件夹")),
                        Err(e) => div().text_color(t.danger).child(format!("IMAP 收信：{e}")),
                    })
                    .child(match &r.smtp {
                        Ok(()) => div().text_color(t.live).child("SMTP 发信：认证成功"),
                        Err(e) => div().text_color(t.danger).child(format!("SMTP 发信：{e}")),
                    });
                if r.imap.is_err() || r.smtp.is_err() {
                    box_ = box_.child(div().text_color(t.muted).child(p.auth_hint));
                }
            }
            d = d.child(box_);
        }
        let busy = self.busy;
        d = d.child(
            div()
                .flex()
                .gap(px(8.))
                .ml(px(96.))
                .child(
                    disabled(button("acc-test", t, Kind::Secondary, None, if busy == Some("test") { "测试中…" } else { "测试连接" }, false), busy.is_some())
                        .on_click(cx.listener(|m, _, _, cx| {
                            if m.busy.is_none() {
                                m.test(false, cx)
                            }
                        })),
                )
                .child(
                    disabled(
                        button(
                            "acc-save",
                            t,
                            Kind::Primary,
                            None,
                            match (busy, editing.is_some()) {
                                (Some("save"), true) => "保存中…",
                                (Some("save"), false) => "验证并添加…",
                                (_, true) => "保存",
                                (_, false) => "添加账号",
                            },
                            false,
                        ),
                        busy.is_some(),
                    )
                    .on_click(cx.listener(|m, _, _, cx| {
                        if m.busy.is_none() {
                            m.test(true, cx)
                        }
                    })),
                )
                .child(button("acc-cancel", t, Kind::Ghost, None, "取消", false).on_click(cx.listener(|m, _, _, cx| {
                    m.editing = Editing::None;
                    m.form = None;
                    m.result = None;
                    cx.notify();
                }))),
        );
        Some(d)
    }

    fn render_settings(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let s = &self.settings;
        let num = |e: &Entity<LineInput>| self.field(t, e, window, cx).flex_none().w(px(110.)).into_any_element();
        let wide = |e: &Entity<LineInput>| self.field(t, e, window, cx).flex_none().w(px(320.)).into_any_element();
        let autostart = super::macos::autostart_enabled();
        div()
            .child(group(t, "同步与网络"))
            .child(card(t, vec![
                setting_row(t, "代理地址", "收发邮件时使用，留空则直连", wide(&s.proxy)),
                setting_row(t, "同步间隔", "单位：分钟。收件箱另有 IDLE 实时推送", num(&s.interval)),
                setting_row(t, "首次同步", "每个文件夹先拉最近多少封", num(&s.initial)),
                setting_row(t, "正文预取", "每个文件夹缓存最新多少封的正文，用于全文搜索和离线阅读", num(&s.prefetch)),
                toggle_row("backfill", t, "历史回填", "空闲时按每批 500 封继续回填更早的邮件（完整归档）", s.backfill)
                    .on_click(cx.listener(|m, _, _, cx| {
                        m.settings.backfill = !m.settings.backfill;
                        cx.notify();
                    }))
                    .into_any_element(),
                toggle_row("autostart", t, "开机启动", "登录 macOS 时自动在后台启动，只显示菜单栏图标", autostart)
                    .on_click(cx.listener(|_, _, _, cx| {
                        let on = !super::macos::autostart_enabled();
                        if let Err(e) = super::macos::set_autostart(on) {
                            cx.emit(AccountsEvent::Toast(e.to_string(), true));
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            ]))
            .child(group(t, "写信与隐私"))
            .child(card(t, vec![
                setting_row(t, "撤回发送窗口", "单位：秒，发送后这段时间内可撤回。0 = 立即发送", num(&s.undo_secs)),
                toggle_row("strip-tracking", t, "去除跟踪参数", "打开链接时去掉 utm 等跟踪参数", s.strip_tracking)
                    .on_click(cx.listener(|m, _, _, cx| {
                        m.settings.strip_tracking = !m.settings.strip_tracking;
                        cx.notify();
                    }))
                    .into_any_element(),
            ]))
            .child(group(t, "更新与备份"))
            .child(card(t, vec![
                toggle_row("check-updates", t, "自动检查更新", "每天最多一次请求 GitHub Releases，不带设备标识", s.check_updates)
                    .on_click(cx.listener(|m, _, _, cx| {
                        m.settings.check_updates = !m.settings.check_updates;
                        cx.notify();
                    }))
                    .into_any_element(),
                setting_row(t, "定时增量导出目录", "留空则不导出", wide(&s.export_dir)),
                setting_row(t, "导出间隔", "单位：天", num(&s.export_days)),
            ]))
            .child(group(t, "OAuth 登录（高级）"))
            .child(card(t, vec![
                setting_row(t, "Microsoft Client ID", "留空使用内置应用；组织要求自有注册时才填", wide(&s.ms_client_id)),
                setting_row(t, "Google Client ID", "Gmail 走 OAuth 登录时需要", wide(&s.google_client_id)),
                setting_row(t, "Google Client Secret", "与上面的 Client ID 配套", wide(&s.google_client_secret)),
            ]))
            .child(div().mt(px(8.)).px(px(4.)).text_size(px(11.)).line_height(px(17.)).text_color(t.muted).child(
                "Google 不允许开源客户端内置密钥，需要填你自己在 Google Cloud 创建的「桌面应用」凭据；不填则继续使用应用专用密码。Microsoft 内置应用在 Azure 中注册为「移动和桌面应用」，重定向 http://localhost。",
            ))
    }
}

impl Render for AccountsModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let close = button("acc-x", &t, Kind::Ghost, Some("close"), "", true).on_click(cx.listener(|_, _, _, cx| cx.emit(AccountsEvent::Close)));
        let tabs = div()
            .flex()
            .flex_none()
            .gap(px(6.))
            .px(px(22.))
            .pt(px(14.))
            .pb(px(4.))
            .child(chip("tab-accounts", &t, "邮箱账号", self.tab == Tab::Accounts).on_click(cx.listener(|m, _, _, cx| {
                m.tab = Tab::Accounts;
                cx.notify();
            })))
            .child(chip("tab-settings", &t, "设置", self.tab == Tab::Settings).on_click(cx.listener(|m, _, _, cx| {
                m.tab = Tab::Settings;
                cx.notify();
            })));
        let mut content = div().id("acc-scroll").flex_1().min_h_0().overflow_y_scroll().px(px(22.)).pt(px(10.)).pb(px(18.));
        if self.tab == Tab::Settings {
            content = content.child(self.render_settings(&t, window, cx));
        } else {
            content = content.child(section_title(&t, format!("已接入 · {}", self.accounts.len())));
            if self.accounts.is_empty() {
                content = content.child(
                    div()
                        .p(px(12.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(t.border_soft)
                        .text_size(px(12.))
                        .text_color(t.muted)
                        .child("还没有账号。先添加一个 Gmail、163 或 QQ 邮箱。"),
                );
            }
            for a in self.accounts.clone() {
                let st = self.eng.sync_state(a.id);
                let status = if st.syncing {
                    format!("正在同步：{}", if st.folder.is_empty() { "…" } else { &st.folder })
                } else if a.sync_enabled {
                    format!("上次同步 {}", fmt_ago(a.last_sync_at))
                } else {
                    "已暂停同步".into()
                };
                let (a1, a2, a3, a4) = (a.clone(), a.clone(), a.clone(), a.id);
                content = content.child(
                    div()
                        .flex()
                        .gap(px(12.))
                        .p(px(12.))
                        .mb(px(8.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(t.border_soft)
                        .child(dot(t.named(&a.color), 9.).mt(px(5.)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(12.))
                                .line_height(px(20.))
                                .child(div().text_size(px(13.)).text_color(t.ink).child(format!("{} · {}", a.name, a.email)))
                                .child(div().text_color(t.muted).child(format!(
                                    "{} · IMAP {}:{} · SMTP {}:{}{}",
                                    a.provider_name(),
                                    a.imap_host,
                                    a.imap_port,
                                    a.smtp_host,
                                    a.smtp_port,
                                    if a.use_proxy { " · 经代理" } else { "" }
                                )))
                                .child(div().text_color(t.muted).child(status))
                                .when(!a.last_error.is_empty(), |d| d.child(div().text_color(t.danger).child(a.last_error.clone()))),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .gap(px(4.))
                                .items_start()
                                .child(button(SharedString::from(format!("as-{}", a.id)), &t, Kind::Secondary, Some("refresh"), "同步", true).on_click(
                                    cx.listener(move |m, _, _, cx| {
                                        m.eng.request_sync(a4, None, true);
                                        cx.emit(AccountsEvent::Toast(format!("已开始同步 {}", a1.email), false));
                                    }),
                                ))
                                .child(
                                    button(
                                        SharedString::from(format!("ap-{}", a.id)),
                                        &t,
                                        Kind::Secondary,
                                        None,
                                        if a.sync_enabled { "暂停" } else { "恢复" },
                                        true,
                                    )
                                    .on_click(cx.listener(move |m, _, _, cx| m.toggle_sync(&a2, cx))),
                                )
                                .child(
                                    button(SharedString::from(format!("ae-{}", a.id)), &t, Kind::Secondary, Some("edit"), "编辑", true)
                                        .on_click(cx.listener(move |m, _, w, cx| m.start_edit(Editing::Edit(a4), w, cx))),
                                )
                                .child(
                                    button(SharedString::from(format!("ar-{}", a.id)), &t, Kind::Danger, Some("trash"), "移除", true)
                                        .on_click(cx.listener(move |m, _, w, cx| m.remove(a3.clone(), w, cx))),
                                ),
                        ),
                );
            }
            if self.editing == Editing::None {
                content = content.child(
                    button("add-account", &t, Kind::Primary, Some("plus"), "添加邮箱", true)
                        .on_click(cx.listener(|m, _, w, cx| m.start_edit(Editing::New, w, cx))),
                );
            }
            if let Some(form) = self.render_form(&t, window, cx) {
                content = content.child(form);
            }
        }
        let mut shell = modal_shell(&t, 720., "账号与设置", "凭据 AES-256-GCM 加密保存在本机", close).h(px(640.)).child(tabs).child(content);
        if self.tab == Tab::Settings {
            shell = shell.child(
                modal_footer(&t)
                    .child(div().flex_1().min_w_0().text_size(px(11.)).text_color(t.faint).child(format!("数据目录：{}", self.eng.store().dir().display())))
                    .child(button("save-settings", &t, Kind::Primary, None, "保存设置", false).on_click(cx.listener(|m, _, _, cx| m.save_settings(cx)))),
            );
        }
        shell
    }
}

/// 设置分组标题
fn group(t: &Theme, title: &'static str) -> Div {
    div().mt(px(18.)).mb(px(8.)).px(px(4.)).text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(t.ink_2).child(title)
}

/// 分组卡片：行与行之间用细线分隔
fn card(t: &Theme, rows: Vec<AnyElement>) -> Div {
    let mut d = div().flex().flex_col().rounded(px(10.)).border_1().border_color(t.border_soft).overflow_hidden();
    for (i, r) in rows.into_iter().enumerate() {
        d = d.child(div().when(i > 0, |d| d.border_t_1().border_color(t.border_soft)).child(r));
    }
    d
}

/// 设置行：左侧「标题 + 说明」，右侧控件
fn setting_row(t: &Theme, title: &'static str, desc: &'static str, control: AnyElement) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .px(px(14.))
        .py(px(10.))
        .min_h(px(54.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(div().text_size(px(13.)).text_color(t.ink).child(title))
                .child(div().mt(px(1.)).text_size(px(11.5)).line_height(px(16.)).text_color(t.muted).child(desc)),
        )
        .child(div().flex_none().child(control))
        .into_any_element()
}

/// 开关行：整行可点
fn toggle_row(id: &'static str, t: &Theme, title: &'static str, desc: &'static str, on: bool) -> Stateful<Div> {
    let ctl = checkbox(SharedString::from(format!("{id}-box")), t, on).into_any_element();
    let row = setting_row(t, title, desc, ctl);
    div().id(id).cursor_pointer().hover(|s| s.bg(t.row_hover)).child(row)
}
