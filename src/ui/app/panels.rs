//! 工具面板：发件人视图、隐私报告、发件箱、本地规则、账号间搬家、连接诊断、回复模板、命令面板。
//!
//! 都是「打开时读一次数据、操作后重读」的简单面板，用一个 [`Panel`] 枚举承载状态，在模态层里渲染。

use super::*;
use crate::core::{archive, backup_pack, contacts, invoice, outbox, queries::Sender, queries::TrackerRow, rules, tools, updater};
use crate::ui::text_area::{TextArea, TextAreaEvent};
use std::path::PathBuf;

pub enum Panel {
    Senders(Vec<Sender>),
    Privacy(Vec<TrackerRow>),
    Outbox(Vec<outbox::Item>),
    Rules {
        list: Vec<rules::Rule>,
        field: usize,
        action: usize,
        target: Option<i64>,
        value: Entity<LineInput>,
    },
    Migrate {
        src: Option<i64>,
        dest: Option<i64>,
        progress: Option<(usize, usize)>,
        result: Option<String>,
    },
    Diagnose {
        account: Option<i64>,
        steps: Option<Vec<tools::Step>>,
        running: bool,
    },
    Templates {
        list: Vec<Template>,
        name: Entity<LineInput>,
        body: Entity<TextArea>,
    },
    Palette {
        input: Entity<LineInput>,
        sel: usize,
    },
    Update {
        rel: tools::Release,
        state: UpdateState,
    },
    /// 快捷键一览（按 ? 打开）
    Shortcuts,
    /// 联系人列表（q = 搜索框）
    Contacts {
        rows: Vec<contacts::ContactRow>,
        q: Entity<LineInput>,
    },
    /// 编辑 / 新建联系人。`fields` 依次为：地址、姓名、公司、电话、备注
    ContactEdit {
        is_new: bool,
        starred: bool,
        fields: Vec<Entity<LineInput>>,
    },
    /// 发票汇总：列表 / 月份筛选 / 扫描 / 按月导出
    Invoices {
        rows: Vec<invoice::InvoiceRow>,
        months: Vec<(String, i64, i64, i64)>,
        month: Option<String>,
        scanning: bool,
        cov: invoice::Coverage,
    },
    /// 完整备份：可选密码（留空 = 不加密）
    Backup {
        pw: Entity<LineInput>,
        pw2: Entity<LineInput>,
        error: Option<String>,
    },
    /// 从加密备份恢复：输入密码
    Restore {
        source: PathBuf,
        pw: Entity<LineInput>,
        busy: bool,
        error: Option<String>,
    },
}

/// 「发现新版本」面板的进度
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateState {
    Idle,
    Working,
    Failed(String),
}

/// 回复模板（存在 kv `templates`）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Template {
    pub name: String,
    pub body: String,
}

pub fn load_templates(store: &Store) -> Vec<Template> {
    store.get_kv("templates").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_templates(store: &Store, v: &[Template]) {
    store.set_kv("templates", &serde_json::to_string(v).unwrap_or_default());
}

/// 保存的搜索（kv `saved_searches`）
pub fn load_saved(store: &Store) -> Vec<String> {
    store.get_kv("saved_searches").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_saved(store: &Store, v: &[String]) {
    store.set_kv("saved_searches", &serde_json::to_string(v).unwrap_or_default());
}

/// 命令面板里的一条命令
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    Compose,
    Sync,
    Settings,
    Import,
    Nav(View),
    Senders,
    Privacy,
    Outbox,
    Rules,
    Migrate,
    Diagnose,
    Templates,
    Prefs,
    Backup,
    Restore,
    Invoices,
    Shortcuts,
    Contacts,
    Slim,
    CheckUpdate,
    Undo,
    ToggleThreads,
}

pub fn commands() -> Vec<(Cmd, &'static str, &'static str)> {
    let mut v = vec![
        (Cmd::Compose, "写新邮件", "compose new xiexin"),
        (Cmd::Sync, "立即同步全部账号", "sync tongbu"),
        (Cmd::Undo, "撤销上一步移动 / 删除", "undo chexiao"),
        (Cmd::ToggleThreads, "切换：按会话合并 / 平铺", "thread huihua"),
        (Cmd::Settings, "账号与设置", "settings account shezhi"),
        (Cmd::Prefs, "设置（同步、撤回发送、更新、OAuth、定时导出）", "prefs more gengduo shezhi"),
        (Cmd::Import, "导入 .eml / .mbox", "import daoru"),
        (Cmd::Senders, "发件人视图：按发件人整理 / 批量清理", "senders fajianren cleanup"),
        (Cmd::Privacy, "隐私报告：谁在追踪你", "privacy tracker yinsi"),
        (Cmd::Outbox, "发件箱（待发送 / 定时 / 失败）", "outbox fajianxiang"),
        (Cmd::Rules, "本地规则", "rules filter guize"),
        (Cmd::Templates, "回复模板", "templates moban"),
        (Cmd::Migrate, "账号间搬家（复制文件夹）", "migrate copy banjia"),
        (Cmd::Diagnose, "连接诊断", "diagnose zhenduan"),
        (Cmd::Backup, "完整备份（附校验报告，可加密）", "backup beifen"),
        (Cmd::Contacts, "联系人：往来统计 / 备注 / vCard 导入导出", "contacts lianxiren tongxunlu"),
        (Cmd::Shortcuts, "快捷键一览（也可以按 ?）", "shortcuts keys kuaijiejian help"),
        (Cmd::Invoices, "发票汇总：金额 / 开票方 / 按月导出", "invoice fapiao fp"),
        (Cmd::Restore, "从备份恢复…（目录或 .embak 加密备份）", "restore huifu beifen"),
        (Cmd::Slim, "安全瘦身（清理旧的本地正文缓存）", "slim shoushen"),
        (Cmd::CheckUpdate, "检查更新", "update gengxin"),
    ];
    for vw in VIEWS {
        v.push((Cmd::Nav(vw), view_label(vw), vw.key()));
    }
    v
}

fn filter_cmds(q: &str) -> Vec<(Cmd, &'static str)> {
    let q = q.trim().to_lowercase();
    commands().into_iter().filter(|(_, label, kw)| q.is_empty() || label.to_lowercase().contains(&q) || kw.contains(&q)).map(|(c, l, _)| (c, l)).collect()
}

impl MailApp {
    fn open_panel(&mut self, p: Panel, cx: &mut Context<Self>) {
        self.pop = None;
        self.modal_sub = None;
        self.modal = Some(Modal::Panel(p));
        cx.notify();
    }

    fn line(&self, placeholder: &str, value: &str, cx: &mut Context<Self>) -> Entity<LineInput> {
        cx.new(|cx| {
            let mut i = LineInput::new(placeholder, cx);
            i.set_text(value, cx);
            i
        })
    }

    pub fn run_cmd(&mut self, c: Cmd, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        match c {
            Cmd::Compose => self.compose_new(window, cx),
            Cmd::Sync => self.sync_now(cx),
            Cmd::Settings => self.open_accounts(Tab::Accounts, false, window, cx),
            Cmd::Import => self.open_import(window, cx),
            Cmd::Nav(v) => self.set_nav(Nav::View(v), cx),
            Cmd::Senders => self.open_senders(cx),
            Cmd::Privacy => self.open_privacy(cx),
            Cmd::Outbox => self.open_outbox(cx),
            Cmd::Rules => self.open_rules(cx),
            Cmd::Migrate => self.open_panel(Panel::Migrate { src: None, dest: None, progress: None, result: None }, cx),
            Cmd::Diagnose => {
                let first = self.accounts.iter().find(|a| !a.is_local()).map(|a| a.id);
                self.open_panel(Panel::Diagnose { account: first, steps: None, running: false }, cx)
            }
            Cmd::Templates => self.open_templates(window, cx),
            Cmd::Prefs => self.open_accounts(Tab::Settings, false, window, cx),
            Cmd::Backup => self.open_backup(window, cx),
            Cmd::Restore => self.restore(window, cx),
            Cmd::Invoices => self.open_invoices(None, cx),
            Cmd::Shortcuts => self.open_panel_shortcuts(cx),
            Cmd::Contacts => self.open_contacts("", window, cx),
            Cmd::Slim => self.slim(window, cx),
            Cmd::CheckUpdate => self.check_update(true, cx),
            Cmd::Undo => self.undo(cx),
            Cmd::ToggleThreads => self.toggle_threads(cx),
        }
        if !matches!(self.modal, Some(Modal::Panel(_)) | Some(Modal::Accounts(_)) | Some(Modal::Import(_))) {
            window.focus(&self.focus);
        }
    }

    pub fn toggle_threads(&mut self, cx: &mut Context<Self>) {
        self.threads = !self.threads;
        self.store.set_kv("threads", if self.threads { "1" } else { "0" });
        self.toast(if self.threads { "已按会话合并显示" } else { "已改为逐封显示" }, false, cx);
        self.load_list(false, false, cx);
    }

    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.line("输入命令或视图名称…", "", cx);
        let sub = cx.subscribe_in(&input, window, |this: &mut Self, _, ev: &InputEvent, window, cx| {
            let Some(Modal::Panel(Panel::Palette { input, sel })) = &mut this.modal else { return };
            let n = filter_cmds(&input.read(cx).text()).len();
            match ev {
                InputEvent::Changed => {
                    *sel = 0;
                    this.palette_scroll.scroll_to_item(0);
                }
                InputEvent::Down => {
                    *sel = (*sel + 1).min(n.saturating_sub(1));
                    this.palette_scroll.scroll_to_item(*sel);
                }
                InputEvent::Up => {
                    *sel = sel.saturating_sub(1);
                    this.palette_scroll.scroll_to_item(*sel);
                }
                InputEvent::Submit { .. } => {
                    let list = filter_cmds(&input.read(cx).text());
                    if let Some((c, _)) = list.get(*sel).copied() {
                        this.run_cmd(c, window, cx);
                    }
                    return;
                }
                InputEvent::Cancel => {
                    this.modal = None;
                    window.focus(&this.focus);
                }
                _ => {}
            }
            cx.notify();
        });
        window.focus(&input.focus_handle(cx));
        self.pop = None;
        self.modal = Some(Modal::Panel(Panel::Palette { input, sel: 0 }));
        self.modal_sub = Some(sub);
        cx.notify();
    }

    pub fn open_senders(&mut self, cx: &mut Context<Self>) {
        let rows = queries::senders(&self.store, None, 200).unwrap_or_default();
        self.open_panel(Panel::Senders(rows), cx);
    }

    pub fn open_privacy(&mut self, cx: &mut Context<Self>) {
        let rows = queries::tracker_report(&self.store, 30).unwrap_or_default();
        self.open_panel(Panel::Privacy(rows), cx);
    }

    pub fn open_outbox(&mut self, cx: &mut Context<Self>) {
        let items = outbox::list(&self.store).unwrap_or_default();
        self.open_panel(Panel::Outbox(items), cx);
    }

    pub fn open_rules(&mut self, cx: &mut Context<Self>) {
        let value = self.line("要匹配的文字，例如 newsletter@ 或 发票", "", cx);
        let list = rules::list(&self.store).unwrap_or_default();
        self.open_panel(Panel::Rules { list, field: 0, action: 0, target: None, value }, cx);
    }

    /// 从发件人视图一键建规则：此发件人以后的新信自动归档
    fn rule_for_sender(&mut self, address: &str, cx: &mut Context<Self>) {
        let r = rules::Rule { field: "from".into(), value: address.into(), action: "archive".into(), ..Default::default() };
        match rules::add(&self.store, &r) {
            Ok(_) => self.toast(format!("已建规则：{address} 的新信自动归档"), false, cx),
            Err(e) => self.toast(e.to_string(), true, cx),
        }
    }

    pub fn open_templates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.line("模板名称", "", cx);
        // 正文可以多行：和账号设置里的签名一样用多行文本框
        let body = cx.new(|cx| TextArea::new("", "模板正文（插入到写信窗口的光标处）。⌘↵ 添加", cx));
        // Tab / Shift-Tab 在「名称 ↔ 正文」之间切换；名称里按回车跳到正文；正文里 ⌘↵ 直接添加
        let (name_h, body_h) = (name.focus_handle(cx), body.focus_handle(cx));
        let sub_name = cx.subscribe_in(&name, window, {
            let body_h = body_h.clone();
            move |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Tab { .. } | InputEvent::Submit { .. } => window.focus(&body_h),
                InputEvent::Cancel => {
                    this.modal = None;
                    window.focus(&this.focus);
                    cx.notify();
                }
                _ => {}
            }
        });
        let sub_body = cx.subscribe_in(&body, window, move |this: &mut Self, _, ev: &TextAreaEvent, window, cx| match ev {
            TextAreaEvent::Tab | TextAreaEvent::BackTab => window.focus(&name_h),
            TextAreaEvent::Commit => this.add_template(window, cx),
            TextAreaEvent::Cancel => {
                this.modal = None;
                window.focus(&this.focus);
                cx.notify();
            }
            _ => {}
        });
        self.open_panel(Panel::Templates { list: load_templates(&self.store), name: name.clone(), body }, cx);
        self.modal_sub = Some(Subscription::join(sub_name, sub_body));
        window.focus(&name.focus_handle(cx));
    }

    /// 把输入框里的名称 / 正文存成一个新模板，然后重新打开面板（清空输入、列表刷新）
    fn add_template(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Panel(Panel::Templates { name, body, .. })) = &self.modal else { return };
        let (n, bd) = (name.read(cx).text(), body.read(cx).text().to_string());
        if n.trim().is_empty() || bd.trim().is_empty() {
            self.toast("请填写模板名称和正文", true, cx);
            return;
        }
        let mut v = load_templates(&self.store);
        v.push(Template { name: n.trim().into(), body: bd });
        save_templates(&self.store, &v);
        self.open_templates(window, cx);
    }

    /// 联系人列表。`filter` 是搜索框的初始内容
    pub fn open_contacts(&mut self, filter: &str, window: &mut Window, cx: &mut Context<Self>) {
        let q = self.line("搜索姓名 / 邮箱 / 公司", filter, cx);
        let sub = cx.subscribe_in(&q, window, |this: &mut Self, input, ev: &InputEvent, window, cx| match ev {
            InputEvent::Changed => {
                let text = input.read(cx).text().to_string();
                if let Some(Modal::Panel(Panel::Contacts { rows, .. })) = &mut this.modal {
                    *rows = contacts::list(&this.store, &text, 200).unwrap_or_default();
                    cx.notify();
                }
            }
            InputEvent::Cancel => this.close_modal(window, cx),
            _ => {}
        });
        let rows = contacts::list(&self.store, filter, 200).unwrap_or_default();
        let h = q.focus_handle(cx);
        self.open_panel(Panel::Contacts { rows, q }, cx);
        self.modal_sub = Some(sub);
        window.focus(&h);
    }

    /// 编辑（`row = Some`）或新建联系人；Tab / Shift-Tab 在各栏之间切换
    pub fn open_contact_edit(&mut self, row: Option<contacts::ContactRow>, window: &mut Window, cx: &mut Context<Self>) {
        let is_new = row.is_none();
        let r = row.unwrap_or_default();
        let fields: Vec<Entity<LineInput>> = [("邮箱地址", &r.address), ("姓名", &r.name), ("公司", &r.org), ("电话", &r.phone), ("备注", &r.note)]
            .into_iter()
            .map(|(ph, v)| self.line(ph, v, cx))
            .collect();
        let handles: Vec<FocusHandle> = fields.iter().map(|f| f.focus_handle(cx)).collect();
        let mut subs: Vec<Subscription> = Vec::new();
        for (i, f) in fields.iter().enumerate() {
            let handles = handles.clone();
            subs.push(cx.subscribe_in(f, window, move |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Tab { back } => {
                    let n = handles.len();
                    window.focus(&handles[if *back { (i + n - 1) % n } else { (i + 1) % n }]);
                }
                InputEvent::Submit { .. } if i + 1 < handles.len() => window.focus(&handles[i + 1]),
                InputEvent::Submit { .. } => this.save_contact(window, cx),
                InputEvent::Cancel => this.open_contacts("", window, cx),
                _ => {}
            }));
        }
        self.open_panel(Panel::ContactEdit { is_new, starred: r.starred, fields }, cx);
        self.modal_sub = subs.into_iter().reduce(Subscription::join);
        window.focus(&handles[if is_new { 0 } else { 1 }]);
    }

    pub fn save_contact(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Panel(Panel::ContactEdit { starred, fields, .. })) = &self.modal else { return };
        let t = |i: usize| fields[i].read(cx).text().to_string();
        let row = contacts::ContactRow { address: t(0), name: t(1), org: t(2), phone: t(3), note: t(4), starred: *starred, ..Default::default() };
        match contacts::save(&self.store, &row) {
            Ok(()) => self.open_contacts("", window, cx),
            Err(e) => self.toast(format!("{e:#}"), true, cx),
        }
    }

    fn import_vcard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("导入 vCard".into()),
            allowed_extensions: None,
        });
        cx.spawn_in(window, async move |this: WeakEntity<Self>, cx| {
            let Some(path) = rx.await.ok().and_then(|r| r.ok()).flatten().and_then(|v| v.into_iter().next()) else { return };
            this.update_in(cx, |a, window, cx| match std::fs::read(&path) {
                Ok(raw) => {
                    let rows = contacts::from_vcard(&String::from_utf8_lossy(&raw));
                    if rows.is_empty() {
                        a.toast("文件里没有带邮箱的联系人", true, cx);
                        return;
                    }
                    match contacts::import(&a.store, &rows) {
                        Ok(n) => {
                            a.open_contacts("", window, cx);
                            a.toast(format!("已导入 {n} 个联系人（已有的只补充空白字段）"), false, cx);
                        }
                        Err(e) => a.toast(format!("导入失败：{e:#}"), true, cx),
                    }
                }
                Err(e) => a.toast(format!("读取失败：{e}"), true, cx),
            })
            .ok();
        })
        .detach();
    }

    fn export_vcard(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Panel(Panel::Contacts { rows, .. })) = &self.modal else { return };
        let vcf = contacts::to_vcard(rows);
        let n = rows.len();
        let dl = dirs::download_dir().unwrap_or_else(|| PathBuf::from("."));
        let rx = cx.prompt_for_new_path(&dl, Some("EasyMail-联系人.vcf"));
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Some(path) = rx.await.ok().and_then(|r| r.ok()).flatten() else { return };
            this.update(cx, |a, cx| match std::fs::write(&path, vcf) {
                Ok(()) => {
                    a.toast(format!("已导出 {n} 个联系人"), false, cx);
                    cx.reveal_path(&path);
                }
                Err(e) => a.toast(format!("导出失败：{e}"), true, cx),
            })
            .ok();
        })
        .detach();
    }

    pub fn open_panel_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.open_panel(Panel::Shortcuts, cx);
    }

    pub fn open_invoices(&mut self, month: Option<String>, cx: &mut Context<Self>) {
        let rows = invoice::list(&self.store, month.as_deref()).unwrap_or_default();
        let months = invoice::months(&self.store).unwrap_or_default();
        let cov = invoice::coverage(&self.store).unwrap_or_default();
        self.open_panel(Panel::Invoices { rows, months, month, scanning: false, cov }, cx);
    }

    /// 扫描：先补扫已缓存的邮件，再把主题像发票、正文还没下载的邮件全部取回来识别（取不回的会继续算作「未统计」）
    fn scan_invoices(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Panel(Panel::Invoices { scanning, .. })) = &mut self.modal else { return };
        if *scanning {
            return;
        }
        *scanning = true;
        cx.notify();
        let (store, eng) = (self.store.clone(), self.eng.clone());
        self.run(
            async move {
                while invoice::scan_backlog(&store, 100)? > 0 {}
                let ids = invoice::remote_candidates(&store, 5000)?;
                let mut fetched = 0;
                for id in &ids {
                    if eng.ensure_body(*id).await.is_ok() {
                        fetched += 1;
                    }
                }
                while invoice::scan_backlog(&store, 100)? > 0 {}
                Ok((ids.len(), fetched))
            },
            cx,
            |a, r, cx| {
                let month = match &a.modal {
                    Some(Modal::Panel(Panel::Invoices { month, .. })) => month.clone(),
                    _ => return,
                };
                a.open_invoices(month, cx);
                match r {
                    Ok((0, _)) => a.toast("扫描完成", false, cx),
                    Ok((n, f)) => a.toast(format!("扫描完成：取回 {f}/{n} 封疑似发票邮件的正文"), false, cx),
                    Err(e) => a.toast(format!("扫描失败：{e:#}"), true, cx),
                }
            },
        );
    }

    fn export_invoices(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Panel(Panel::Invoices { month: Some(month), .. })) = &self.modal else { return };
        let month = month.clone();
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("导出到这里".into()),
            allowed_extensions: None,
        });
        let store = self.store.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Some(dir) = rx.await.ok().and_then(|r| r.ok()).flatten().and_then(|v| v.into_iter().next()) else { return };
            let r = cx.background_spawn(async move { invoice::export_month(&store, &month, &dir) }).await;
            this.update(cx, |a, cx| match r {
                Ok(rep) => {
                    a.toast(format!("已导出 {} 张发票，合计 {}", rep.count, invoice::fmt_amount(rep.total_cents)), false, cx);
                    cx.reveal_path(&rep.zip);
                }
                Err(e) => a.toast(format!("导出失败：{e:#}"), true, cx),
            })
            .ok();
        })
        .detach();
    }

    /// 完整备份面板：可选设置密码；留空 = 不加密（备份目录），设了密码 = 单个 .embak 加密文件
    pub fn open_backup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pw = self.line("备份密码（可选，留空 = 不加密）", "", cx);
        let pw2 = self.line("再输入一次密码", "", cx);
        pw.update(cx, |i, cx| i.set_masked(true, cx));
        pw2.update(cx, |i, cx| i.set_masked(true, cx));
        let (h1, h2) = (pw.focus_handle(cx), pw2.focus_handle(cx));
        let s1 = cx.subscribe_in(&pw, window, {
            let h2 = h2.clone();
            move |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Tab { .. } | InputEvent::Submit { .. } => window.focus(&h2),
                InputEvent::Cancel => this.close_modal(window, cx),
                _ => {}
            }
        });
        let s2 = cx.subscribe_in(&pw2, window, move |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::Tab { .. } => window.focus(&h1),
            InputEvent::Submit { .. } => this.start_backup(window, cx),
            InputEvent::Cancel => this.close_modal(window, cx),
            _ => {}
        });
        let first = pw.focus_handle(cx);
        self.open_panel(Panel::Backup { pw, pw2, error: None }, cx);
        self.modal_sub = Some(Subscription::join(s1, s2));
        window.focus(&first);
    }

    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        window.focus(&self.focus);
        cx.notify();
    }

    fn start_backup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Panel(Panel::Backup { pw, pw2, error })) = &mut self.modal else { return };
        let (p1, p2) = (pw.read(cx).text(), pw2.read(cx).text());
        if p1 != p2 {
            *error = Some("两次输入的密码不一致".into());
            cx.notify();
            return;
        }
        let password = p1.to_string();
        self.close_modal(window, cx);
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("备份到这里".into()),
            allowed_extensions: None,
        });
        let store = self.store.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Some(dir) = rx.await.ok().and_then(|r| r.ok()).flatten().and_then(|v| v.into_iter().next()) else { return };
            this.update(cx, |a, cx| a.toast(if password.is_empty() { "正在备份…" } else { "正在备份并加密…" }, false, cx)).ok();
            let enc = !password.is_empty();
            let r = cx
                .background_spawn(async move {
                    if enc {
                        backup_pack::backup_encrypted(&store, &dir, &password)
                    } else {
                        archive::backup(&store, &dir)
                    }
                })
                .await;
            this.update(cx, |a, cx| match r {
                Ok(rep) if rep.verified => {
                    let what = if enc { "加密备份完成并校验通过" } else { "备份完成并校验通过" };
                    a.toast(format!("{what}：{} 封、{}（{}）", rep.messages, fmt_size(rep.bytes), rep.dir.display()), false, cx);
                    cx.reveal_path(&rep.dir);
                }
                Ok(rep) => a.toast(format!("备份完成，但校验发现 {} 个问题，详见 校验报告.txt", rep.problems.len()), true, cx),
                Err(e) => a.toast(format!("备份失败：{e:#}"), true, cx),
            })
            .ok();
        })
        .detach();
    }

    /// 从备份恢复：先选备份（目录或 .embak 文件）；加密的要输入密码，校验通过后确认并重启
    pub fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: Some("选择备份".into()),
            allowed_extensions: None,
        });
        cx.spawn_in(window, async move |this: WeakEntity<Self>, cx| {
            let Some(src) = rx.await.ok().and_then(|r| r.ok()).flatten().and_then(|v| v.into_iter().next()) else { return };
            this.update_in(cx, |a, window, cx| {
                if backup_pack::is_encrypted(&src) {
                    let pw = a.line("备份密码", "", cx);
                    pw.update(cx, |i, cx| i.set_masked(true, cx));
                    let sub = cx.subscribe_in(&pw, window, |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                        InputEvent::Submit { .. } => this.start_restore(window, cx),
                        InputEvent::Cancel => this.close_modal(window, cx),
                        _ => {}
                    });
                    let h = pw.focus_handle(cx);
                    a.open_panel(Panel::Restore { source: src, pw, busy: false, error: None }, cx);
                    a.modal_sub = Some(sub);
                    window.focus(&h);
                } else {
                    a.stage_restore(src, None, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn start_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Panel(Panel::Restore { source, pw, busy, error })) = &mut self.modal else { return };
        if *busy {
            return;
        }
        *busy = true;
        *error = None;
        let (src, password) = (source.clone(), pw.read(cx).text().to_string());
        cx.notify();
        self.stage_restore(src, Some(password), window, cx);
    }

    /// 后台校验 / 解出备份到暂存目录；通过后让用户确认，再重启切换数据
    fn stage_restore(&mut self, src: PathBuf, password: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if password.is_none() {
            self.toast("正在校验备份…", false, cx);
        }
        let data = self.store.dir().to_path_buf();
        cx.spawn_in(window, async move |this: WeakEntity<Self>, cx| {
            let d = data.clone();
            let r = cx.background_spawn(async move { backup_pack::stage_restore(&src, password.as_deref(), &d) }).await;
            this.update_in(cx, |a, window, cx| match r {
                Err(e) => {
                    if let Some(Modal::Panel(Panel::Restore { busy, error, .. })) = &mut a.modal {
                        *busy = false;
                        *error = Some(format!("{e:#}"));
                        cx.notify();
                    } else {
                        a.toast(format!("无法恢复：{e:#}"), true, cx);
                    }
                }
                Ok(info) => {
                    a.close_modal(window, cx);
                    let answer = window.prompt(
                        PromptLevel::Warning,
                        &format!("用备份替换当前数据？（{} 个账号、{} 封邮件、{} 封有正文）", info.accounts, info.messages, info.with_body),
                        Some("当前的邮件、账号、设置会被备份里的内容替换，应用随后自动重启。\n\n旧数据不会被删除：会保留在数据目录旁的 data.before-restore-… 文件夹里，确认没问题后可以手动删除。"),
                        &["恢复并重启", "取消"],
                        cx,
                    );
                    let data = data.clone();
                    cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                        if answer.await.ok() == Some(0) {
                            this.update(cx, |a, cx| a.relaunch_for_restore(cx)).ok();
                        } else {
                            let _ = std::fs::remove_dir_all(backup_pack::pending_dir(&data));
                        }
                    })
                    .detach();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 退出并重新打开应用：启动时 `apply_pending` 完成数据切换
    fn relaunch_for_restore(&mut self, cx: &mut Context<Self>) {
        let pid = std::process::id().to_string();
        let target = updater::installed_app().map(|p| p.display().to_string());
        match &target {
            Some(app) => {
                use std::os::unix::process::CommandExt;
                let ok = std::process::Command::new("/bin/sh")
                    .args(["-c", "while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; open \"$2\"", "sh", &pid, app])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .process_group(0)
                    .spawn()
                    .is_ok();
                if !ok {
                    self.toast("恢复数据已就绪，请手动重新打开应用", false, cx);
                    return;
                }
                cx.quit();
            }
            None => {
                self.toast("恢复数据已就绪：请退出后重新启动应用（开发模式不会自动重启）", false, cx);
            }
        }
    }

    pub fn slim(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let days = 365;
        let pv = archive::slim_preview(&self.store, days).unwrap_or_default();
        if pv.messages == 0 {
            self.toast("没有可以清理的旧正文缓存", false, cx);
            return;
        }
        let answer = window.prompt(
            PromptLevel::Info,
            &format!("清理 {} 封一年前邮件的本地正文缓存？", pv.messages),
            Some(&format!(
                "约释放 {}。只清理服务器上仍有副本的邮件，本地归档（导入的邮件）不受影响；主题、发件人与全文搜索照常可用，打开时自动从服务器取回。",
                fmt_size(pv.bytes)
            )),
            &["清理", "取消"],
            cx,
        );
        let store = self.store.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if answer.await.ok() != Some(0) {
                return;
            }
            let r = cx.background_spawn(async move { archive::slim(&store, days) }).await;
            this.update(cx, |a, cx| match r {
                Ok(r) => a.toast(format!("已清理 {} 封，释放 {}", r.messages, fmt_size(r.bytes)), false, cx),
                Err(e) => a.toast(format!("瘦身失败：{e:#}"), true, cx),
            })
            .ok();
        })
        .detach();
    }

    /// 检查更新：`manual` 为手动触发（没有新版本也提示）
    pub fn check_update(&mut self, manual: bool, cx: &mut Context<Self>) {
        if !manual && !tools::update_due(&self.store) {
            return;
        }
        tools::mark_update_checked(&self.store);
        let proxy = self.eng.proxy();
        self.run(async move { tools::check_update(proxy.as_ref()).await }, cx, move |a, r, cx| match r {
            Ok(Some(rel)) => {
                let msg = format!("发现新版本 {}（当前 {}）", rel.version, crate::core::config::VERSION);
                if manual {
                    a.open_update(rel, cx);
                } else if updater::can_self_update(&rel) {
                    a.toast_with(msg, false, Some(ToastAction::Update(rel)), cx)
                } else {
                    a.toast_with(msg, false, Some(ToastAction::Open(rel.url)), cx)
                }
            }
            Ok(None) if manual => a.toast(format!("已是最新版本 {}", crate::core::config::VERSION), false, cx),
            Err(e) if manual => a.toast(format!("检查更新失败：{e:#}"), true, cx),
            _ => {}
        });
    }

    pub fn open_update(&mut self, rel: tools::Release, cx: &mut Context<Self>) {
        self.open_panel(Panel::Update { rel, state: UpdateState::Idle }, cx);
    }

    /// 下载 → 验签 → 替换 → 重启。失败时留在面板里显示原因，用户仍可手动下载
    fn start_update(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Panel(Panel::Update { rel, state })) = &mut self.modal else { return };
        if *state == UpdateState::Working {
            return;
        }
        *state = UpdateState::Working;
        let rel = rel.clone();
        let proxy = self.eng.proxy();
        let proxy_url = self.store.settings().proxy_url;
        self.run(async move { updater::prepare(&rel, proxy.as_ref(), &proxy_url).await }, cx, |a, r, cx| {
            let fail = |a: &mut Self, e: String, cx: &mut Context<Self>| {
                if let Some(Modal::Panel(Panel::Update { state, .. })) = &mut a.modal {
                    *state = UpdateState::Failed(e);
                }
                cx.notify();
            };
            match r {
                Ok(staged) => match updater::relaunch(&staged) {
                    Ok(()) => cx.quit(),
                    Err(e) => fail(a, format!("{e:#}"), cx),
                },
                Err(e) => fail(a, format!("{e:#}"), cx),
            }
        });
        cx.notify();
    }

    fn sender_bulk(&mut self, address: String, action: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let ids = queries::sender_message_ids(&self.store, &address, None).unwrap_or_default();
        if ids.is_empty() {
            return;
        }
        match action {
            "read" => self.set_seen(ids, true, cx),
            "delete" => {
                let n = ids.len();
                let answer = window.prompt(
                    PromptLevel::Warning,
                    &format!("删除 {address} 的全部 {n} 封邮件？"),
                    Some("会移入各账号的垃圾箱，可在撤销窗口内撤销。"),
                    &["删除", "取消"],
                    cx,
                );
                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    if answer.await.ok() == Some(0) {
                        this.update(cx, |a, cx| {
                            a.do_delete(ids, cx);
                            a.open_senders(cx);
                        })
                        .ok();
                    }
                })
                .detach();
                return;
            }
            "unsub" => {
                let mid: Option<i64> = self
                    .store
                    .r()
                    .query_row("SELECT id FROM messages WHERE from_addr = ? AND unsub != '' ORDER BY date DESC LIMIT 1", [&address], |r| r.get(0))
                    .ok();
                if let Some(m) = mid {
                    self.unsubscribe(m, cx);
                }
            }
            _ => {}
        }
        self.open_senders(cx);
    }

    fn run_diagnose(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Panel(Panel::Diagnose { account: Some(acc_id), running, steps })) = &mut self.modal else { return };
        *running = true;
        *steps = None;
        let acc_id = *acc_id;
        let Some(acc) = self.accounts.iter().find(|a| a.id == acc_id).cloned() else { return };
        let store = self.store.clone();
        let proxy = self.eng.proxy();
        self.run(async move { Ok(tools::diagnose(&store, &acc, proxy.as_ref()).await) }, cx, |a, r: anyhow::Result<Vec<tools::Step>>, cx| {
            if let Some(Modal::Panel(Panel::Diagnose { steps, running, .. })) = &mut a.modal {
                *running = false;
                *steps = r.ok();
            }
            cx.notify();
        });
        cx.notify();
    }

    fn run_migrate(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Panel(Panel::Migrate { src: Some(src), dest: Some(dest), progress, result })) = &mut self.modal else { return };
        let (src, dest) = (*src, *dest);
        *progress = Some((0, 0));
        *result = None;
        let eng = self.eng.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<(usize, usize)>();
        let fut = self.eng.call(archive::copy_folder(eng, src, dest, move |i, n| {
            let _ = tx.unbounded_send((i, n));
        }));
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let poll = async {
                while let Some(p) = rx.next().await {
                    let ok = this
                        .update(cx, |a, cx| {
                            if let Some(Modal::Panel(Panel::Migrate { progress, .. })) = &mut a.modal {
                                *progress = Some(p);
                                cx.notify();
                            }
                        })
                        .is_ok();
                    if !ok {
                        break;
                    }
                }
            };
            poll.await;
            let r = fut.await;
            this.update(cx, |a, cx| {
                if let Some(Modal::Panel(Panel::Migrate { progress, result, .. })) = &mut a.modal {
                    *progress = None;
                    *result = Some(match r {
                        Ok(r) => format!(
                            "复制 {} 封，之前已复制 {} 封，失败 {} 封{}",
                            r.copied,
                            r.skipped,
                            r.failed,
                            if r.errors.is_empty() { String::new() } else { format!("：{}", r.errors.join("；")) }
                        ),
                        Err(e) => format!("出错：{e:#}"),
                    });
                }
                a.load_meta(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn folder_chips(&self, t: &Theme, idp: &'static str, sel: Option<i64>, cx: &mut Context<Self>, pick: fn(&mut Panel, i64)) -> Div {
        // 不限高：外层面板内容区本身可滚动；flex_none 防止被压扁（之前限高 260 会把后面的账号裁掉）
        let mut col = div().flex().flex_col().flex_none().gap(px(2.));
        for a in &self.accounts {
            let fs: Vec<&Folder> = self.folders.iter().filter(|f| f.account_id == a.id && f.role != "flagged" && f.role != "all").collect();
            if fs.is_empty() {
                continue;
            }
            col = col.child(div().flex_none().pt(px(6.)).text_size(px(11.)).text_color(t.muted).child(if a.is_local() {
                "本地归档".to_string()
            } else {
                a.email.clone()
            }));
            let mut row = div().flex().flex_none().flex_wrap().gap(px(4.));
            for f in fs {
                let fid = f.id;
                row = row.child(
                    div()
                        .id(SharedString::from(format!("{idp}-{fid}")))
                        .px(px(8.))
                        .py(px(3.))
                        .rounded(px(6.))
                        .text_size(px(12.))
                        .cursor_pointer()
                        .border_1()
                        .border_color(if sel == Some(fid) { t.accent } else { t.border })
                        .when(sel == Some(fid), |x| x.bg(t.accent_soft))
                        .child(f.label())
                        .on_click(cx.listener(move |a, _, _, cx| {
                            if let Some(Modal::Panel(p)) = &mut a.modal {
                                pick(p, fid);
                            }
                            cx.notify();
                        })),
                );
            }
            col = col.child(row);
        }
        col
    }

    pub(super) fn render_panel(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let close = button("panel-x", t, Kind::Ghost, Some("close"), "", true).on_click(cx.listener(|a, _, w, cx| {
            a.modal = None;
            w.focus(&a.focus);
            cx.notify();
        }));
        let Some(Modal::Panel(p)) = &self.modal else { return div().into_any_element() };
        let body = div().id("panel-body").px(px(22.)).py(px(14.)).flex_1().min_h(px(0.)).max_h(px(520.)).overflow_y_scroll().flex().flex_col().gap(px(6.));
        let row = |id: SharedString| {
            div().id(id).flex().flex_none().items_center().gap(px(10.)).px(px(8.)).py(px(6.)).rounded(px(7.)).text_size(px(12.5)).hover(|s| s.bg(t.row_hover))
        };
        match p {
            Panel::Palette { input, sel } => {
                let list = filter_cmds(&input.read(cx).text());
                let sel = (*sel).min(list.len().saturating_sub(1));
                let focused = input.read(cx).is_focused(window);
                let total = list.len();
                // 全部命令都能滚动到；↑↓ 选择时选中项自动滚进可视区
                let mut b =
                    div().id("cmd-list").flex().flex_col().gap(px(2.)).p(px(10.)).max_h(px(380.)).overflow_y_scroll().track_scroll(&self.palette_scroll);
                for (i, (c, label)) in list.into_iter().enumerate() {
                    b = b.child(
                        div()
                            .id(("cmd", i))
                            .px(px(10.))
                            .py(px(7.))
                            .rounded(px(7.))
                            .text_size(px(13.))
                            .cursor_pointer()
                            .when(i == sel, |x| x.bg(t.accent_soft))
                            .hover(|s| s.bg(t.row_hover))
                            .child(label)
                            .on_click(cx.listener(move |a, _, w, cx| a.run_cmd(c, w, cx))),
                    );
                }
                div()
                    .w(px(520.))
                    .bg(t.panel)
                    .rounded(px(12.))
                    .border_1()
                    .border_color(t.border)
                    .overflow_hidden()
                    .child(
                        div()
                            .p(px(12.))
                            .border_b_1()
                            .border_color(t.border_soft)
                            .child(field_box(t, focused).child(icon("search", 14., t.muted)).child(div().flex_1().child(input.clone()))),
                    )
                    .child(b)
                    .child(
                        div()
                            .px(px(16.))
                            .py(px(8.))
                            .text_size(px(11.))
                            .text_color(t.faint)
                            .child(format!("共 {total} 项 · ↑↓ 选择 · ↵ 执行 · Esc 关闭 · ⌘K 随时打开")),
                    )
                    .into_any_element()
            }
            Panel::Senders(rows) => {
                let mut b = body.child(div().text_size(px(11.5)).text_color(t.muted).child("按发件人汇总（不含已发送 / 草稿 / 垃圾箱），封数从多到少。"));
                for (i, s) in rows.iter().enumerate() {
                    let addr = s.address.clone();
                    let (a1, a2, a3, a4, a5) = (addr.clone(), addr.clone(), addr.clone(), addr.clone(), addr.clone());
                    b = b.child(
                        row(SharedString::from(format!("snd-{i}")))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(if s.name.is_empty() {
                                        s.address.clone()
                                    } else {
                                        format!("{} <{}>", s.name, s.address)
                                    }))
                                    .child(
                                        div().text_size(px(11.)).text_color(t.muted).child(
                                            crate::i18n::tr(&format!(
                                                "{} 封 · 未读 {} · {} · 最近 {}",
                                                s.count,
                                                s.unread,
                                                fmt_size(s.bytes as u64),
                                                fmt_full(s.last)
                                            ))
                                            .into_owned(),
                                        ),
                                    ),
                            )
                            .child(button(SharedString::from(format!("snd-v-{i}")), t, Kind::Ghost, None, "查看", true).on_click(cx.listener(
                                move |a, _, w, cx| {
                                    a.modal = None;
                                    a.search.update(cx, |s, cx| s.set_text(&format!("from:{a1}"), cx));
                                    a.set_query(format!("from:{a1}"), cx);
                                    w.focus(&a.focus);
                                },
                            )))
                            .when(s.unread > 0, |x| {
                                x.child(
                                    button(SharedString::from(format!("snd-r-{i}")), t, Kind::Ghost, None, "已读", true)
                                        .on_click(cx.listener(move |a, _, w, cx| a.sender_bulk(a2.clone(), "read", w, cx))),
                                )
                            })
                            .when(s.unsub, |x| {
                                x.child(
                                    button(SharedString::from(format!("snd-u-{i}")), t, Kind::Ghost, None, "退订", true)
                                        .on_click(cx.listener(move |a, _, w, cx| a.sender_bulk(a3.clone(), "unsub", w, cx))),
                                )
                            })
                            .child(
                                button(SharedString::from(format!("snd-a-{i}")), t, Kind::Ghost, None, "自动归档", true)
                                    .on_click(cx.listener(move |a, _, _, cx| a.rule_for_sender(&a4, cx))),
                            )
                            .child(
                                button(SharedString::from(format!("snd-d-{i}")), t, Kind::Danger, None, "全删", true)
                                    .on_click(cx.listener(move |a, _, w, cx| a.sender_bulk(a5.clone(), "delete", w, cx))),
                            ),
                    );
                }
                modal_shell(t, 760., "发件人视图", format!("{} 位发件人", rows.len()), close).child(b).into_any_element()
            }
            Panel::Privacy(rows) => {
                let total: i64 = rows.iter().map(|r| r.pixels).sum();
                let mut b = body.child(div().text_size(px(12.5)).text_color(t.ink_2).child(format!(
                    "过去 30 天，EasyMail 在已缓存的邮件里拦截了 {total} 个追踪像素（来自 {} 位发件人）。远程图片默认不加载，所以对方无法知道你是否、何时打开了邮件。",
                    rows.len()
                )));
                for (i, r) in rows.iter().enumerate().take(100) {
                    b = b.child(
                        row(SharedString::from(format!("pv-{i}")))
                            .child(icon("eyeOff", 13., t.muted))
                            .child(
                                div().flex_1().min_w_0().child(r.sender.clone()).child(div().text_size(px(11.)).text_color(t.muted).child(r.hosts.join("、"))),
                            )
                            .child(div().text_color(t.muted).child(format!("{} 封 · {} 个像素", r.mails, r.pixels))),
                    );
                }
                modal_shell(t, 640., "隐私报告", "谁在追踪你", close).child(b).into_any_element()
            }
            Panel::Outbox(items) => {
                let mut b = body;
                if items.is_empty() {
                    b = b.child(div().py(px(30.)).text_center().text_color(t.muted).child("发件箱是空的"));
                }
                for (i, it) in items.iter().enumerate() {
                    let id = it.id;
                    let state = match it.state.as_str() {
                        "queued" if it.attempts > 0 => format!("等待重试 · {}", fmt_full(it.send_at)),
                        "queued" => format!("计划 {} 发出", fmt_full(it.send_at)),
                        "sending" => "发送中…".into(),
                        "accepted" => "服务器已收下，正在存「已发送」…".into(),
                        _ => "发送失败".into(),
                    };
                    b = b.child(
                        row(SharedString::from(format!("ob-{i}")))
                            .child(icon("outbox", 13., if it.state == "failed" { t.danger } else { t.muted }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(if it.subject.is_empty() {
                                        "（无主题）".to_string()
                                    } else {
                                        it.subject.clone()
                                    }))
                                    .child(div().text_size(px(11.)).text_color(t.muted).child(format!("{} · {}", it.rcpt, state)))
                                    .when(!it.last_error.is_empty(), |x| x.child(div().text_size(px(11.)).text_color(t.danger).child(it.last_error.clone()))),
                            )
                            .when(it.state != "sending" && it.state != "accepted", |x| {
                                x.child(
                                    button(
                                        SharedString::from(format!("ob-now-{i}")),
                                        t,
                                        Kind::Ghost,
                                        None,
                                        if it.state == "failed" { "重试" } else { "立即发送" },
                                        true,
                                    )
                                    .on_click(cx.listener(move |a, _, _, cx| {
                                        let _ = outbox::retry(&a.store, id);
                                        a.open_outbox(cx);
                                    })),
                                )
                                .child(
                                    button(SharedString::from(format!("ob-edit-{i}")), t, Kind::Secondary, None, "撤回编辑", true)
                                        .on_click(cx.listener(move |a, _, w, cx| a.unsend(id, w, cx))),
                                )
                            }),
                    );
                }
                modal_shell(t, 640., "发件箱", "待发送、定时发送与发送失败的邮件", close).child(b).into_any_element()
            }
            Panel::Rules { list, field, action, target, value } => {
                let (field, action, target) = (*field, *action, *target);
                let focused = value.read(cx).is_focused(window);
                let folder_name = |id: Option<i64>| id.and_then(|i| self.folders.iter().find(|f| f.id == i)).map(|f| f.label()).unwrap_or_default();
                let scroll = self.rules_scroll.clone();
                let thumb = t.muted;
                let vp = f32::from(window.viewport_size().height);
                let shell_h = (vp * 0.88).clamp(520., 740.).min((vp - 16.).max(360.));
                let mut rules_box = div()
                    .id("rule-list")
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.rules_scroll)
                    .pr(px(12.));
                if list.is_empty() {
                    rules_box = rules_box.child(div().py(px(18.)).text_center().text_size(px(12.)).text_color(t.muted).child("还没有规则"));
                }
                for (i, r) in list.iter().enumerate() {
                    let id = r.id;
                    let on = r.enabled;
                    let fl = rules::FIELDS.iter().find(|f| f.0 == r.field).map(|f| f.1).unwrap_or("");
                    let al = rules::ACTIONS.iter().find(|f| f.0 == r.action).map(|f| f.1).unwrap_or("");
                    rules_box = rules_box.child(
                        row(SharedString::from(format!("rule-{i}")))
                            .child(check_row(SharedString::from(format!("rule-on-{i}")), t, on, "").on_click(cx.listener(move |a, _, _, cx| {
                                let _ = rules::set_enabled(&a.store, id, !on);
                                a.open_rules(cx);
                            })))
                            .child(
                                div().flex_1().child(
                                    crate::i18n::tr(&format!(
                                        "{fl}包含「{}」→ {al}{}",
                                        r.value,
                                        if r.action == "move" {
                                            let name = folder_name(r.target);
                                            let mail = r
                                                .target
                                                .and_then(|id| self.folders.iter().find(|f| f.id == id))
                                                .and_then(|f| {
                                                    self.accounts.iter().find(|a| a.id == f.account_id).map(|a| {
                                                        if a.is_local() { "本地归档".to_string() } else { a.email.clone() }
                                                    })
                                                })
                                                .unwrap_or_default();
                                            if mail.is_empty() { format!("「{name}」") } else { format!("「{mail} / {name}」") }
                                        } else {
                                            String::new()
                                        }
                                    ))
                                    .into_owned(),
                                ),
                            )
                            .child(div().text_size(px(11.)).text_color(t.muted).child(format!("命中 {}", r.hits)))
                            .child(button(SharedString::from(format!("rule-del-{i}")), t, Kind::Ghost, Some("trash"), "", true).on_click(cx.listener(
                                move |a, _, _, cx| {
                                    let _ = rules::remove(&a.store, id);
                                    a.open_rules(cx);
                                },
                            ))),
                    );
                }
                let chips = |items: &[(&'static str, &'static str)], cur: usize, idp: &'static str, set: fn(&mut Panel, usize)| {
                    let mut r = div().flex().flex_none().flex_wrap().gap(px(4.));
                    for (i, (_, label)) in items.iter().enumerate() {
                        r = r.child(
                            div()
                                .id(SharedString::from(format!("{idp}-{i}")))
                                .px(px(8.))
                                .py(px(3.))
                                .rounded(px(6.))
                                .text_size(px(12.))
                                .cursor_pointer()
                                .border_1()
                                .border_color(if i == cur { t.accent } else { t.border })
                                .when(i == cur, |x| x.bg(t.accent_soft))
                                .child(*label)
                                .on_click(cx.listener(move |a, _, _, cx| {
                                    if let Some(Modal::Panel(p)) = &mut a.modal {
                                        set(p, i);
                                    }
                                    cx.notify();
                                })),
                        );
                    }
                    r
                };
                let action_labels: [(&str, &str); 5] =
                    [("move", "移到文件夹"), ("read", "标为已读"), ("star", "加星标"), ("archive", "归档"), ("delete", "放进垃圾箱")];
                let action_key = rules::ACTIONS[action].0;
                let typed = value.read(cx).text();
                let needle = typed.trim().to_string();
                let field_name = rules::FIELDS[field].1;
                let mailbox = target.and_then(|id| self.folders.iter().find(|f| f.id == id)).and_then(|f| {
                    self.accounts.iter().find(|a| a.id == f.account_id).map(|a| if a.is_local() { "本地归档".to_string() } else { a.email.clone() })
                });
                let action_note = match action_key {
                    "move" => "选好后，在下面点这个邮箱里的目标文件夹。",
                    "read" => "新信一到收件箱就标成已读。每个邮箱都会执行。",
                    "star" => "新信加上星标，仍留在收件箱。每个邮箱都会执行。",
                    "archive" => "新信会被移出收件箱，放进各自邮箱的归档。每个邮箱都会执行。",
                    _ => "新信会被放进各自邮箱的垃圾箱，不是彻底删除。每个邮箱都会执行。",
                };
                let preview = if needle.is_empty() {
                    "先在上面写下要找的文字，比如某个发件人的邮箱，或主题里的「发票」。".to_string()
                } else if action_key == "move" && target.is_none() {
                    format!("找到{field_name}里包含「{needle}」的新信后，还要在下面点一个文件夹。")
                } else if action_key == "move" {
                    format!(
                        "以后：{field_name}包含「{needle}」的新信（仅 {}）会移到「{}」。",
                        mailbox.as_deref().unwrap_or("这个邮箱"),
                        folder_name(target)
                    )
                } else {
                    let verb = match action_key {
                        "read" => "标为已读，仍留在收件箱",
                        "star" => "加上星标，仍留在收件箱",
                        "archive" => "被归档，从收件箱移走",
                        _ => "被放进垃圾箱",
                    };
                    format!("以后：{field_name}包含「{needle}」的新信，会{verb}。")
                };
                let step = |n: &'static str, title: &'static str| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .mt(px(10.))
                        .child(
                            div()
                                .size(px(18.))
                                .flex_none()
                                .rounded_full()
                                .bg(t.accent_soft)
                                .text_color(t.accent_deep)
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(11.))
                                .font_weight(FontWeight::BOLD)
                                .child(n),
                        )
                        .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(title))
                };
                let mut form = div()
                    .id("rule-form")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(22.))
                    .pt(px(4.))
                    .pb(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(div().mt(px(4.)).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child("新建规则"))
                    .child(step("1", "如何筛选新进邮件"))
                    .child(div().text_size(px(11.5)).text_color(t.muted).child("选一项，再写下要包含的文字。不用写完整，也不区分大小写。"))
                    .child(chips(&rules::FIELDS, field, "rf", |p, i| {
                        if let Panel::Rules { field, .. } = p {
                            *field = i
                        }
                    }))
                    .child(field_box(t, focused).flex_none().child(div().flex_1().child(value.clone())))
                    .child(step("2", "怎么处理"))
                    .child(chips(&action_labels, action, "ra", |p, i| {
                        if let Panel::Rules { action, .. } = p {
                            *action = i
                        }
                    }))
                    .child(div().text_size(px(11.5)).text_color(t.muted).child(action_note));
                if action_key == "move" {
                    form = form
                        .child(step("3", "放到哪个文件夹"))
                        .child(div().text_size(px(11.5)).text_color(t.muted).child("点一个文件夹。规则只处理这个邮箱以后的新信，不会挪到别的邮箱。"))
                        .child(self.folder_chips(t, "rt", target, cx, |p, f| {
                            if let Panel::Rules { target, .. } = p {
                                *target = Some(f)
                            }
                        }));
                }
                let rules_wrap = div().relative().h(px(160.)).flex_none().mx(px(14.)).rounded(px(8.)).border_1().border_color(t.border_soft).child(rules_box).child(
                    canvas(
                        |bounds, _, _| bounds,
                        move |bounds, _, window, _| {
                            let max = scroll.max_offset().height;
                            if max <= px(0.5) || bounds.size.height <= px(24.) {
                                return;
                            }
                            let view = bounds.size.height;
                            let thumb_h = (view * (view / (view + max))).max(px(24.)).min(view);
                            let top = bounds.top() + (view - thumb_h) * ((-scroll.offset().y).clamp(px(0.), max) / max);
                            window.paint_quad(
                                fill(Bounds::new(point(bounds.right() - px(6.), top), size(px(4.), thumb_h)), thumb.opacity(0.55)).corner_radii(px(2.)),
                            );
                        },
                    )
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(10.)),
                );
                let column = div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .flex_none()
                            .px(px(22.))
                            .pt(px(12.))
                            .pb(px(8.))
                            .text_size(px(11.5))
                            .text_color(t.muted)
                            .child("新信同步进收件箱时按顺序匹配；移动 / 归档 / 删除类规则每封信只执行第一条命中的。只处理之后到达的新信。"),
                    )
                    .child(rules_wrap)
                    .child(form);
                let preview_bar = div()
                    .flex_none()
                    .px(px(22.))
                    .py(px(8.))
                    .bg(t.accent_soft)
                    .text_size(px(12.5))
                    .text_color(t.ink)
                    .child(preview);
                let footer = modal_footer(t).child(div().flex_1()).child(button("rule-add", t, Kind::Primary, Some("plus"), "添加规则", false).on_click(
                    cx.listener(|a, _, _, cx| {
                        let Some(Modal::Panel(Panel::Rules { field, action, target, value, .. })) = &a.modal else { return };
                        let account_id = if rules::ACTIONS[*action].0 == "move" {
                            target.and_then(|fid| a.folders.iter().find(|f| f.id == fid).map(|f| f.account_id))
                        } else {
                            None
                        };
                        let r = rules::Rule {
                            field: rules::FIELDS[*field].0.into(),
                            action: rules::ACTIONS[*action].0.into(),
                            target: *target,
                            account_id,
                            value: value.read(cx).text(),
                            ..Default::default()
                        };
                        match rules::add(&a.store, &r) {
                            Ok(_) => a.open_rules(cx),
                            Err(e) => a.toast(e.to_string(), true, cx),
                        }
                    }),
                ));
                modal_shell(t, 640., "本地规则", format!("{} 条", list.len()), close)
                    .h(px(shell_h))
                    .child(column)
                    .child(preview_bar)
                    .child(footer)
                    .into_any_element()
            }
            Panel::Migrate { src, dest, progress, result } => {
                let (src, dest, progress, result) = (*src, *dest, *progress, result.clone());
                let b = body
                    .child(
                        div().text_size(px(11.5)).text_color(t.muted).child(
                            crate::i18n::tr(
                                "把一个文件夹里的邮件复制到另一个账号（或本地归档）的文件夹，保留已读 / 星标。原邮件不删除；中断后再点「开始」会从断点继续。",
                            )
                            .into_owned(),
                        ),
                    )
                    .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).mt(px(6.)).child("① 从"))
                    .child(self.folder_chips(t, "ms", src, cx, |p, f| {
                        if let Panel::Migrate { src, .. } = p {
                            *src = Some(f)
                        }
                    }))
                    .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).mt(px(6.)).child("② 复制到"))
                    .child(self.folder_chips(t, "md", dest, cx, |p, f| {
                        if let Panel::Migrate { dest, .. } = p {
                            *dest = Some(f)
                        }
                    }))
                    .when_some(progress, |x, (i, n)| x.child(div().mt(px(8.)).text_color(t.accent).child(format!("正在复制 {i} / {n}…"))))
                    .when_some(result, |x, r| x.child(div().mt(px(8.)).text_color(t.ink_2).child(r)));
                let ready = src.is_some() && dest.is_some() && src != dest && progress.is_none();
                let footer = modal_footer(t).child(div().flex_1()).child(
                    disabled(button("mig-go", t, Kind::Primary, None, "开始复制", false), !ready).on_click(cx.listener(move |a, _, _, cx| {
                        if ready {
                            a.run_migrate(cx)
                        }
                    })),
                );
                modal_shell(t, 680., "账号间搬家", "IMAP 复制，可断点续传", close).child(b).child(footer).into_any_element()
            }
            Panel::Diagnose { account, steps, running } => {
                let (account, running) = (*account, *running);
                let mut chips = div().flex().flex_wrap().gap(px(4.));
                for a in self.accounts.iter().filter(|a| !a.is_local()) {
                    let id = a.id;
                    chips = chips.child(
                        div()
                            .id(("dg", id as u64))
                            .px(px(8.))
                            .py(px(3.))
                            .rounded(px(6.))
                            .text_size(px(12.))
                            .cursor_pointer()
                            .border_1()
                            .border_color(if account == Some(id) { t.accent } else { t.border })
                            .child(a.email.clone())
                            .on_click(cx.listener(move |a, _, _, cx| {
                                if let Some(Modal::Panel(Panel::Diagnose { account, steps, .. })) = &mut a.modal {
                                    *account = Some(id);
                                    *steps = None;
                                }
                                cx.notify();
                            })),
                    );
                }
                let mut b = body.child(chips);
                if running {
                    b = b.child(div().py(px(12.)).text_color(t.accent).child("正在逐步检查…"));
                }
                if let Some(st) = steps {
                    for (i, s) in st.iter().enumerate() {
                        b = b.child(
                            row(SharedString::from(format!("dgs-{i}")))
                                .child(div().w(px(16.)).text_color(if s.ok { t.live } else { t.danger }).child(if s.ok { "✓" } else { "✗" }))
                                .child(div().w(px(170.)).flex_none().child(s.name.clone()))
                                .child(div().flex_1().min_w_0().text_color(t.ink_2).child(s.detail.clone()))
                                .child(div().text_size(px(11.)).text_color(t.muted).child(format!("{} ms", s.ms))),
                        );
                    }
                }
                let footer = modal_footer(t).child(div().flex_1()).child(
                    disabled(button("dg-go", t, Kind::Primary, None, "开始诊断", false), running || account.is_none())
                        .on_click(cx.listener(|a, _, _, cx| a.run_diagnose(cx))),
                );
                modal_shell(t, 700., "连接诊断", "代理 → 域名解析 → 端口 → TLS → 登录", close).child(b).child(footer).into_any_element()
            }
            Panel::Update { rel, state } => {
                let can = updater::can_self_update(rel);
                let working = *state == UpdateState::Working;
                let mut b = body;
                if !rel.notes.trim().is_empty() {
                    b = b.child(div().text_size(px(12.)).line_height(px(19.)).text_color(t.ink_2).child(rel.notes.trim().to_string()));
                }
                b = b.child(div().mt(px(8.)).text_size(px(11.5)).text_color(t.muted).child(if can {
                    "更新包用 Ed25519 签名校验后才会安装；完成后应用会自动重启。邮件数据不受影响。"
                } else {
                    "这个版本不能自动更新（没有签名文件，或不是从 .app 启动的），请前往下载页手动安装。"
                }));
                match state {
                    UpdateState::Working => b = b.child(div().mt(px(8.)).text_color(t.accent).child("正在下载并校验更新包…（完成后自动重启）")),
                    UpdateState::Failed(e) => b = b.child(div().mt(px(8.)).text_color(t.danger).child(format!("更新失败：{e}"))),
                    UpdateState::Idle => {}
                }
                let url = rel.url.clone();
                let footer = modal_footer(t)
                    .child(button("up-page", t, Kind::Ghost, None, "查看发布页", false).on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))))
                    .child(div().flex_1())
                    .child(disabled(button("up-later", t, Kind::Secondary, None, "稍后", false), working).on_click(cx.listener(|a, _, w, cx| {
                        a.modal = None;
                        w.focus(&a.focus);
                        cx.notify();
                    })))
                    .child(if can {
                        disabled(button("up-go", t, Kind::Primary, None, if working { "更新中…" } else { "立即更新" }, false), working)
                            .on_click(cx.listener(|a, _, _, cx| a.start_update(cx)))
                    } else {
                        let url = rel.url.clone();
                        button("up-dl", t, Kind::Primary, None, "前往下载", false).on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
                    });
                modal_shell(t, 560., format!("发现新版本 {}", rel.version), format!("当前 {}", crate::core::config::VERSION), close)
                    .child(b)
                    .child(footer)
                    .into_any_element()
            }
            Panel::Invoices { rows, months, month, scanning, cov } => {
                let scanning = *scanning;
                let cov = cov.clone();
                let total: i64 = rows.iter().filter_map(|r| r.amount_cents).sum();
                let unknown = rows.iter().filter(|r| r.amount_cents.is_none()).count();
                let mut chips = div().flex().flex_wrap().gap(px(6.));
                let chip = |id: SharedString, label: String, on: bool| {
                    div()
                        .id(id)
                        .px(px(10.))
                        .py(px(4.))
                        .rounded(px(14.))
                        .border_1()
                        .border_color(if on { t.accent } else { t.border })
                        .bg(if on { t.accent_soft } else { t.paper })
                        .text_size(px(12.))
                        .cursor_pointer()
                        .child(label)
                };
                chips = chips.child(chip("inv-all".into(), "全部".into(), month.is_none()).on_click(cx.listener(|a, _, _, cx| a.open_invoices(None, cx))));
                for (i, (m, n, sum, miss)) in months.iter().enumerate() {
                    let target = m.clone();
                    chips = chips.child(
                        chip(
                            SharedString::from(format!("inv-m-{i}")),
                            if *miss > 0 {
                                format!("{m} · {n} 张 · {}+（{miss} 张金额未识别）", invoice::fmt_amount(*sum))
                            } else {
                                format!("{m} · {n} 张 · {}", invoice::fmt_amount(*sum))
                            },
                            month.as_deref() == Some(m.as_str()),
                        )
                        .on_click(cx.listener(move |a, _, _, cx| a.open_invoices(Some(target.clone()), cx))),
                    );
                }
                let mut b = body.child(chips);
                // 有邮件没取回正文时禁止导出（会漏票）；覆盖情况：汇总只含已识别的发票，没统计进去的要明说，不让数字看起来像「全部」
                if !cov.complete() {
                    let mut parts = Vec::new();
                    if cov.pending > 0 {
                        parts.push(format!("{} 封还没取回正文（点「扫描」取回）", cov.pending));
                    }
                    if cov.unrecognized > 0 {
                        parts.push(format!("{} 封没找到可识别的 PDF 发票（可能是链接式发票、OFD、图片或扫描件，需要手动处理）", cov.unrecognized));
                    }
                    b = b.child(
                        div()
                            .px(px(12.))
                            .py(px(8.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(t.danger)
                            .bg(t.danger.opacity(0.06))
                            .text_size(px(12.))
                            .text_color(t.danger)
                            .child(format!(
                                "汇总不完整：主题像发票的邮件共 {} 封，只有 {} 封已识别出发票；{}。下面的张数和金额只包含已识别的，不是全部。",
                                cov.candidates,
                                cov.recognized,
                                parts.join("；")
                            )),
                    );
                }
                if rows.is_empty() {
                    b = b.child(
                        div()
                            .py(px(24.))
                            .text_center()
                            .text_color(t.muted)
                            .child("还没有识别到发票。点下面的「扫描」：会识别已缓存邮件里的 PDF 发票，并取回主题像发票的邮件。"),
                    );
                }
                for (i, r) in rows.iter().enumerate().take(300) {
                    let id = r.message_id;
                    b = b.child(
                        row(SharedString::from(format!("inv-{i}")))
                            .child(div().w(px(84.)).flex_none().text_color(t.muted).child(r.date.clone()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(if r.seller.is_empty() {
                                        r.subject.clone()
                                    } else {
                                        r.seller.clone()
                                    }))
                                    .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(11.)).text_color(t.muted).child(format!(
                                        "{} · {}{}",
                                        r.kind,
                                        r.filename,
                                        if r.number.is_empty() { String::new() } else { format!(" · {}", r.number) }
                                    ))),
                            )
                            .child(
                                div()
                                    .w(px(96.))
                                    .flex_none()
                                    .text_right()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(r.amount_cents.map(invoice::fmt_amount).unwrap_or_else(|| "未识别".into())),
                            )
                            .on_click(cx.listener(move |a, _, w, cx| {
                                a.close_modal(w, cx);
                                a.open_message(id, cx);
                            })),
                    );
                }
                let sub = format!(
                    "已识别 {} 张 · 已识别金额合计 {}{}{}",
                    rows.len(),
                    invoice::fmt_amount(total),
                    if unknown > 0 { format!("（另有 {unknown} 张金额未识别，未计入）") } else { String::new() },
                    if cov.duplicates > 0 { format!(" · 已合并 {} 张重复", cov.duplicates) } else { String::new() }
                );
                let footer = modal_footer(t)
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.5))
                            .text_color(t.muted)
                            .child("只统计主题含发票 / 收据 / receipt / 电子客票 的邮件；金额取自 PDF 文字，请以原件为准"),
                    )
                    .child(
                        disabled(button("inv-scan", t, Kind::Secondary, Some("refresh"), if scanning { "扫描中…" } else { "扫描" }, false), scanning)
                            .on_click(cx.listener(|a, _, _, cx| a.scan_invoices(cx))),
                    )
                    .child(
                        disabled(
                            button("inv-export", t, Kind::Primary, Some("download"), "导出所选月份 ZIP", false),
                            month.is_none() || rows.is_empty() || cov.pending > 0,
                        )
                        .on_click(cx.listener(|a, _, _, cx| a.export_invoices(cx))),
                    );
                modal_shell(t, 760., "发票汇总", sub, close).child(b).child(footer).into_any_element()
            }
            Panel::Shortcuts => {
                let groups: [(&str, &[(&str, &str)]); 4] = [
                    (
                        "列表与阅读（焦点在列表时）",
                        &[("j / ↓", "下一封"), ("k / ↑", "上一封"), ("[ / ]", "会话内上一封 / 下一封"), ("/", "搜索"), ("?", "这个面板")],
                    ),
                    (
                        "邮件操作",
                        &[
                            ("r", "回复"),
                            ("a", "全部回复"),
                            ("f", "转发"),
                            ("s", "星标 / 取消"),
                            ("u", "已读 / 未读"),
                            ("e", "归档"),
                            ("# / Delete", "删除"),
                            ("⌘Z", "撤销移动 / 删除"),
                        ],
                    ),
                    (
                        "全局",
                        &[
                            ("⌘N / c", "写新邮件"),
                            ("⌘K", "命令面板"),
                            ("⌘F", "搜索"),
                            ("⌘R", "立即同步"),
                            ("⌘,", "账号与设置"),
                            ("⌘⇧D", "切换配色"),
                            ("⌘W", "隐藏窗口（后台继续收信）"),
                            ("⌘Q", "退出"),
                        ],
                    ),
                    ("鼠标", &[("右键邮件", "上下文菜单"), ("Shift + 点击", "范围选择"), ("⌘ + 点击", "加选 / 取消"), ("拖到侧栏文件夹", "移动邮件")]),
                ];
                let mut b = body;
                for (title, items) in groups {
                    b = b.child(div().mt(px(6.)).text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD).text_color(t.muted).child(title));
                    for (k, v) in items {
                        b = b.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .py(px(2.))
                                .child(div().w(px(150.)).flex_none().font_family(MONO).text_size(px(12.)).text_color(t.accent_deep).child(k.to_string()))
                                .child(div().text_color(t.ink_2).child(v.to_string())),
                        );
                    }
                }
                modal_shell(t, 520., "快捷键", "输入框里不会触发单键快捷键", close).child(b).into_any_element()
            }
            Panel::Contacts { rows, q } => {
                let focused = q.read(cx).is_focused(window);
                // 搜索框在滚动区外面，列表单独滚动，输入时不会被顶走。
                let search = div()
                    .flex_none()
                    .px(px(22.))
                    .pt(px(12.))
                    .pb(px(8.))
                    .child(field_box(t, focused).child(div().flex_1().child(q.clone())));
                let mut list = div()
                    .id("ct-list")
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .px(px(22.))
                    .pt(px(4.))
                    .pb(px(14.))
                    .max_h(px(460.))
                    .overflow_y_scroll();
                if rows.is_empty() {
                    list = list.child(
                        div()
                            .py(px(24.))
                            .text_center()
                            .text_color(t.muted)
                            .child("没有匹配的联系人。往来过的人会自动出现在这里，也可以点「新建」或导入 vCard。"),
                    );
                }
                for (i, c) in rows.iter().enumerate().take(200) {
                    let (addr, addr2, addr3, addr4) = (c.address.clone(), c.address.clone(), c.address.clone(), c.address.clone());
                    let row_data = c.clone();
                    let initial: String = c.display().chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
                    let mut stats = Vec::new();
                    if !c.org.is_empty() {
                        stats.push(c.org.clone());
                    }
                    if c.received + c.sent > 0 {
                        stats.push(format!("收 {} · 发 {} · 最近 {}", c.received, c.sent, fmt_short(c.last)));
                    }
                    list = list.child(
                        row(SharedString::from(format!("ct-{i}")))
                            .child(
                                div()
                                    .size(px(30.))
                                    .flex_none()
                                    .rounded_full()
                                    .bg(t.accent_soft)
                                    .text_color(t.accent_deep)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .font_weight(FontWeight::BOLD)
                                    .child(initial),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(c.display().to_string()),
                                    )
                                    .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(11.)).text_color(t.muted).child(
                                        if c.name.is_empty() {
                                            stats.join(" · ")
                                        } else {
                                            format!("{}{}{}", c.address, if stats.is_empty() { "" } else { " · " }, stats.join(" · "))
                                        },
                                    )),
                            )
                            .child(
                                button(
                                    SharedString::from(format!("ct-star-{i}")),
                                    t,
                                    Kind::Ghost,
                                    Some(if c.starred { "star-fill" } else { "star" }),
                                    "",
                                    true,
                                )
                                .text_color(if c.starred { t.gold } else { t.muted })
                                .on_click(cx.listener(move |a, _, w, cx| {
                                    let on = !contacts::list(&a.store, &addr, 1).ok().and_then(|v| v.into_iter().next()).is_some_and(|c| c.starred);
                                    let _ = contacts::set_starred(&a.store, &addr, on);
                                    let text = match &a.modal {
                                        Some(Modal::Panel(Panel::Contacts { q, .. })) => q.read(cx).text().to_string(),
                                        _ => String::new(),
                                    };
                                    a.open_contacts(&text, w, cx);
                                })),
                            )
                            .child(button(SharedString::from(format!("ct-mail-{i}")), t, Kind::Ghost, Some("edit"), "写信", true).on_click(cx.listener(
                                move |a, _, w, cx| {
                                    a.modal = None;
                                    a.compose_to(&addr2, w, cx);
                                },
                            )))
                            .child(button(SharedString::from(format!("ct-find-{i}")), t, Kind::Ghost, Some("envelope"), "邮件", true).on_click(cx.listener(
                                move |a, _, w, cx| {
                                    a.close_modal(w, cx);
                                    a.set_query(addr3.clone(), cx);
                                },
                            )))
                            .child(button(SharedString::from(format!("ct-edit-{i}")), t, Kind::Ghost, None, "编辑", true).on_click(cx.listener(
                                move |a, _, w, cx| {
                                    let _ = &addr4;
                                    a.open_contact_edit(Some(row_data.clone()), w, cx)
                                },
                            ))),
                    );
                }
                let footer = modal_footer(t)
                    .child(
                        button("ct-new", t, Kind::Secondary, Some("plus"), "新建", false).on_click(cx.listener(|a, _, w, cx| a.open_contact_edit(None, w, cx))),
                    )
                    .child(
                        button("ct-import", t, Kind::Ghost, Some("download"), "导入 vCard", false).on_click(cx.listener(|a, _, w, cx| a.import_vcard(w, cx))),
                    )
                    .child(button("ct-export", t, Kind::Ghost, Some("download"), "导出 vCard", false).on_click(cx.listener(|a, _, _, cx| a.export_vcard(cx))))
                    .child(div().flex_1());
                let b = div().flex().flex_col().child(search).child(list);
                modal_shell(t, 720., "联系人", format!("{} 位 · 往来统计来自本机邮件", rows.len()), close).child(b).child(footer).into_any_element()
            }
            Panel::ContactEdit { is_new, starred, fields } => {
                let labels = ["邮箱地址", "姓名", "公司", "电话", "备注"];
                let mut b = body;
                for (i, f) in fields.iter().enumerate() {
                    let foc = f.read(cx).is_focused(window);
                    b = b.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(div().w(px(64.)).flex_none().text_color(t.muted).text_size(px(12.)).child(labels[i]))
                            .child(field_box(t, foc).flex_1().child(div().flex_1().child(f.clone()))),
                    );
                }
                b = b.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(t.muted)
                        .child("Tab 切换栏位，回车保存。邮件里出现过的名字会自动识别，这里填的优先；写信时输入姓名也能联想到。"),
                );
                let is_new = *is_new;
                let starred = *starred;
                let addr = fields[0].read(cx).text().to_string();
                let footer = modal_footer(t)
                    .child(button("ce-back", t, Kind::Ghost, None, "返回列表", false).on_click(cx.listener(|a, _, w, cx| a.open_contacts("", w, cx))))
                    .child(div().flex_1())
                    .when(!is_new, |d| {
                        d.child(button("ce-del", t, Kind::Ghost, Some("trash"), "清除手动信息", false).text_color(t.danger).on_click(cx.listener(
                            move |a, _, w, cx| {
                                let _ = contacts::delete(&a.store, &addr);
                                a.open_contacts("", w, cx)
                            },
                        )))
                    })
                    .child(
                        button("ce-star", t, Kind::Ghost, Some(if starred { "star-fill" } else { "star" }), if starred { "取消星标" } else { "星标" }, false)
                            .on_click(cx.listener(|a, _, _, cx| {
                                if let Some(Modal::Panel(Panel::ContactEdit { starred, .. })) = &mut a.modal {
                                    *starred = !*starred;
                                    cx.notify();
                                }
                            })),
                    )
                    .child(button("ce-save", t, Kind::Primary, None, "保存", false).on_click(cx.listener(|a, _, w, cx| a.save_contact(w, cx))));
                modal_shell(t, 560., if is_new { "新建联系人" } else { "编辑联系人" }, "姓名 / 公司 / 电话 / 备注", close)
                    .child(b)
                    .child(footer)
                    .into_any_element()
            }
            Panel::Backup { pw, pw2, error } => {
                let (f1, f2) = (pw.read(cx).is_focused(window), pw2.read(cx).is_focused(window));
                let enc = !pw.read(cx).text().is_empty();
                let mut b = body
                    .child(div().text_size(px(12.)).line_height(px(19.)).text_color(t.ink_2).child("备份包含数据库快照、每封邮件的原文、发信附件，以及账号凭据的加密密钥。密码留空 = 保存成一个文件夹（不加密）；设置密码 = 保存成单个 .embak 加密文件，密钥也在密文里。",))
                    .child(field_box(t, f1).child(div().flex_1().child(pw.clone())))
                    .child(field_box(t, f2).child(div().flex_1().child(pw2.clone())))
                    .child(div().text_size(px(11.5)).text_color(t.muted).child("密码无法找回：忘记密码就无法恢复这份备份。"));
                if let Some(e) = error {
                    b = b.child(div().text_color(t.danger).text_size(px(12.)).child(e.clone()));
                }
                let footer = modal_footer(t).child(div().flex_1()).child(
                    button("bk-go", t, Kind::Primary, Some("download"), if enc { "选择位置并加密备份" } else { "选择位置并备份" }, false)
                        .on_click(cx.listener(|a, _, w, cx| a.start_backup(w, cx))),
                );
                modal_shell(t, 560., "完整备份", "附校验报告，可选密码加密", close).child(b).child(footer).into_any_element()
            }
            Panel::Restore { source, pw, busy, error } => {
                let focused = pw.read(cx).is_focused(window);
                let busy = *busy;
                let mut b = body
                    .child(div().text_size(px(12.)).text_color(t.ink_2).child(format!("加密备份：{}", source.display())))
                    .child(field_box(t, focused).child(div().flex_1().child(pw.clone())));
                if busy {
                    b = b.child(div().text_color(t.accent).child("正在解密并校验备份…（大备份需要一会儿）"));
                }
                if let Some(e) = error {
                    b = b.child(div().text_color(t.danger).text_size(px(12.)).child(e.clone()));
                }
                let footer = modal_footer(t).child(div().flex_1()).child(
                    disabled(button("rs-go", t, Kind::Primary, None, if busy { "校验中…" } else { "校验并继续" }, false), busy)
                        .on_click(cx.listener(|a, _, w, cx| a.start_restore(w, cx))),
                );
                modal_shell(t, 560., "从加密备份恢复", "输入创建备份时设置的密码", close).child(b).child(footer).into_any_element()
            }
            Panel::Templates { list, name, body: tb } => {
                let (f1, f2) = (name.read(cx).is_focused(window), tb.focus_handle(cx).is_focused(window));
                // 外壳必须有确定高度：父级是随内容收缩的包裹层，max_h(90%) 算不出来，
                // 列表就会按内容无限长高，窗口被顶出屏幕，滚动条也不会出现。
                let vp = f32::from(window.viewport_size().height);
                let shell_h = (vp * 0.88).clamp(320., 680.).min(vp - 16.);
                let scroll = self.templates_scroll.clone();
                let thumb = t.muted;
                // 上面是独立滚动的模板列表，下面「新建模板」表单固定不动。
                let mut listing = div()
                    .id("tp-list")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .overflow_y_scroll()
                    .track_scroll(&self.templates_scroll)
                    .px(px(22.))
                    .py(px(10.));
                if list.is_empty() {
                    listing = listing.child(div().py(px(18.)).text_center().text_size(px(12.)).text_color(t.muted).child("还没有模板，在下面新建一个。"));
                }
                for (i, tp) in list.iter().enumerate() {
                    listing = listing.child(
                        row(SharedString::from(format!("tp-{i}")))
                            .child(
                                div()
                                    .w(px(120.))
                                    .flex_none()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(tp.name.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_color(t.ink_2)
                                    .child(one_line(&tp.body, 200)),
                            )
                            .child(button(SharedString::from(format!("tp-del-{i}")), t, Kind::Ghost, Some("trash"), "", true).on_click(cx.listener(
                                move |a, _, w, cx| {
                                    let mut v = load_templates(&a.store);
                                    if i < v.len() {
                                        v.remove(i);
                                    }
                                    save_templates(&a.store, &v);
                                    a.open_templates(w, cx);
                                },
                            ))),
                    );
                }
                let form = div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(6.))
                    .px(px(22.))
                    .pt(px(10.))
                    .pb(px(14.))
                    .border_t_1()
                    .border_color(t.border_soft)
                    .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child("新建模板"))
                    .child(field_box(t, f1).flex_none().child(div().flex_1().child(name.clone())))
                    .child(
                        div()
                            .h(px(160.))
                            .flex_none()
                            .p(px(10.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(if f2 { t.accent } else { t.border })
                            .bg(t.paper)
                            .text_size(px(13.))
                            .text_color(t.ink)
                            .child(tb.clone()),
                    );
                let b = div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        div()
                            .flex_none()
                            .px(px(22.))
                            .pt(px(12.))
                            .text_size(px(11.5))
                            .text_color(t.muted)
                            .child("在写信窗口点「模板」插入。Tab 在名称和正文之间切换。"),
                    )
                    .child(
                        div().relative().flex().flex_col().flex_1().min_h_0().child(listing).child(
                            canvas(
                                |bounds, _, _| bounds,
                                move |bounds, _, window, _| {
                                    let max = scroll.max_offset().height;
                                    if max <= px(0.5) || bounds.size.height <= px(24.) {
                                        return;
                                    }
                                    let view = bounds.size.height;
                                    let thumb_h = (view * (view / (view + max))).max(px(24.)).min(view);
                                    let top = bounds.top() + (view - thumb_h) * ((-scroll.offset().y).clamp(px(0.), max) / max);
                                    window.paint_quad(
                                        fill(Bounds::new(point(bounds.right() - px(7.), top), size(px(4.), thumb_h)), thumb.opacity(0.55))
                                            .corner_radii(px(2.)),
                                    );
                                },
                            )
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(10.)),
                        ),
                    )
                    .child(form);
                let footer = modal_footer(t)
                    .child(div().flex_1())
                    .child(button("tp-add", t, Kind::Primary, Some("plus"), "添加模板", false).on_click(cx.listener(|a, _, w, cx| a.add_template(w, cx))));
                modal_shell(t, 620., "回复模板", format!("{} 个", list.len()), close).h(px(shell_h)).child(b).child(footer).into_any_element()
            }
        }
    }
}
