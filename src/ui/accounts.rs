//! 账号与设置。对应 just-mail 的 AccountsModal.jsx：账号列表（同步 / 暂停 / 编辑 / 移除）、
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
        let settings = SettingsForm {
            proxy: input(cx, &s.proxy_url, "socks5://127.0.0.1:1080 或 http://127.0.0.1:1087"),
            interval: input(cx, &s.sync_interval_min.to_string(), ""),
            initial: input(cx, &s.initial_sync_per_folder.to_string(), ""),
            prefetch: input(cx, &s.prefetch_bodies.to_string(), ""),
            backfill: s.backfill,
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
            accounts::clean_password(&inp.password)
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
        match store.save_settings(&s) {
            Ok(_) => cx.emit(AccountsEvent::Toast("设置已保存".into(), false)),
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
        if f.provider == "custom" && editing.is_none() {
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
        let hint = |x: &'static str| div().text_size(px(11.5)).text_color(t.muted).child(x);
        div()
            .child(section_title(t, "同步与网络"))
            .child(form_row(t, "代理地址", false).child(self.field(t, &s.proxy, window, cx)))
            .child(
                form_row(t, "同步间隔", false)
                    .child(self.field(t, &s.interval, window, cx).flex_none().w(px(90.)))
                    .child(hint("分钟（收件箱另有 IDLE 实时推送）")),
            )
            .child(form_row(t, "首次同步", false).child(self.field(t, &s.initial, window, cx).flex_none().w(px(90.))).child(hint("每个文件夹先拉最近多少封")))
            .child(
                form_row(t, "正文预取", false)
                    .child(self.field(t, &s.prefetch, window, cx).flex_none().w(px(90.)))
                    .child(hint("每个文件夹缓存最新多少封的正文（全文搜索、离线阅读）")),
            )
            .child(form_row(t, "历史回填", false).child(check_row("backfill", t, s.backfill, "空闲时按每批 500 封继续回填更早的邮件（完整归档）").on_click(
                cx.listener(|m, _, _, cx| {
                    m.settings.backfill = !m.settings.backfill;
                    cx.notify();
                }),
            )))
            .child(form_row(t, "开机启动", false).child(
                check_row("autostart", t, super::macos::autostart_enabled(), "登录 macOS 时自动在后台启动（只显示菜单栏图标）").on_click(cx.listener(
                    |_, _, _, cx| {
                        let on = !super::macos::autostart_enabled();
                        if let Err(e) = super::macos::set_autostart(on) {
                            cx.emit(AccountsEvent::Toast(e.to_string(), true));
                        }
                        cx.notify();
                    },
                )),
            ))
            .child(
                div()
                    .ml(px(96.))
                    .child(button("save-settings", t, Kind::Primary, None, "保存设置", false).on_click(cx.listener(|m, _, _, cx| m.save_settings(cx)))),
            )
            .child(div().ml(px(96.)).mt(px(14.)).text_size(px(11.)).text_color(t.faint).child(format!("数据目录：{}", self.eng.store().dir().display())))
    }
}

impl Render for AccountsModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let close = button("acc-x", &t, Kind::Ghost, Some("close"), "", true).on_click(cx.listener(|_, _, _, cx| cx.emit(AccountsEvent::Close)));
        let tabs = div()
            .flex()
            .gap(px(6.))
            .mb(px(14.))
            .child(chip("tab-accounts", &t, "邮箱账号", self.tab == Tab::Accounts).on_click(cx.listener(|m, _, _, cx| {
                m.tab = Tab::Accounts;
                cx.notify();
            })))
            .child(chip("tab-settings", &t, "同步设置", self.tab == Tab::Settings).on_click(cx.listener(|m, _, _, cx| {
                m.tab = Tab::Settings;
                cx.notify();
            })));
        let mut content = div().id("acc-scroll").flex_1().min_h_0().overflow_y_scroll().px(px(22.)).py(px(16.)).child(tabs);
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
        modal_shell(&t, 720., "账号与设置", "凭据 AES-256-GCM 加密保存在本机", close).h(px(640.)).child(content)
    }
}
