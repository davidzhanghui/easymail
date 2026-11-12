//! 工具面板：发件人视图、隐私报告、发件箱、本地规则、账号间搬家、连接诊断、回复模板、命令面板。
//!
//! 都是「打开时读一次数据、操作后重读」的简单面板，用一个 [`Panel`] 枚举承载状态，在模态层里渲染。

use super::*;
use crate::core::{archive, outbox, queries::Sender, queries::TrackerRow, rules, tools, updater};

pub enum Panel {
    Senders(Vec<Sender>),
    Privacy(Vec<TrackerRow>),
    Outbox(Vec<outbox::Item>),
    Rules { list: Vec<rules::Rule>, field: usize, action: usize, target: Option<i64>, value: Entity<LineInput> },
    Migrate { src: Option<i64>, dest: Option<i64>, progress: Option<(usize, usize)>, result: Option<String> },
    Diagnose { account: Option<i64>, steps: Option<Vec<tools::Step>>, running: bool },
    Templates { list: Vec<Template>, name: Entity<LineInput>, body: Entity<LineInput> },
    Palette { input: Entity<LineInput>, sel: usize },
    Update { rel: tools::Release, state: UpdateState },
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
        (Cmd::Backup, "完整备份（附校验报告）", "backup beifen"),
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
            Cmd::Templates => self.open_templates(cx),
            Cmd::Prefs => self.open_accounts(Tab::Settings, false, window, cx),
            Cmd::Backup => self.backup(cx),
            Cmd::Slim => self.slim(window, cx),
            Cmd::CheckUpdate => self.check_update(true, cx),
            Cmd::Undo => self.undo(cx),
            Cmd::ToggleThreads => self.toggle_threads(cx),
        }
        if !matches!(self.modal, Some(Modal::Panel(_)) | Some(Modal::Composer(_)) | Some(Modal::Accounts(_)) | Some(Modal::Import(_))) {
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
                InputEvent::Changed => *sel = 0,
                InputEvent::Down => *sel = (*sel + 1).min(n.saturating_sub(1)),
                InputEvent::Up => *sel = sel.saturating_sub(1),
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

    pub fn open_templates(&mut self, cx: &mut Context<Self>) {
        let name = self.line("模板名称", "", cx);
        let body = self.line("模板正文（插入到写信窗口的光标处）", "", cx);
        self.open_panel(Panel::Templates { list: load_templates(&self.store), name, body }, cx);
    }

    pub fn backup(&mut self, cx: &mut Context<Self>) {
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
            this.update(cx, |a, cx| a.toast("正在备份…", false, cx)).ok();
            let r = cx.background_spawn(async move { archive::backup(&store, &dir) }).await;
            this.update(cx, |a, cx| match r {
                Ok(rep) if rep.verified => {
                    a.toast(format!("备份完成并校验通过：{} 封、{}（{}）", rep.messages, fmt_size(rep.bytes), rep.dir.display()), false, cx);
                    cx.reveal_path(&rep.dir);
                }
                Ok(rep) => a.toast(format!("备份完成，但校验发现 {} 个问题，详见 校验报告.txt", rep.problems.len()), true, cx),
                Err(e) => a.toast(format!("备份失败：{e:#}"), true, cx),
            })
            .ok();
        })
        .detach();
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
        let mut col = div().flex().flex_col().gap(px(2.)).max_h(px(260.)).overflow_hidden();
        for a in &self.accounts {
            let fs: Vec<&Folder> = self.folders.iter().filter(|f| f.account_id == a.id && f.role != "flagged" && f.role != "all").collect();
            if fs.is_empty() {
                continue;
            }
            col = col.child(div().pt(px(6.)).text_size(px(11.)).text_color(t.muted).child(if a.is_local() {
                "本地归档".to_string()
            } else {
                a.email.clone()
            }));
            let mut row = div().flex().flex_wrap().gap(px(4.));
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
        let body = div().id("panel-body").px(px(22.)).py(px(14.)).max_h(px(520.)).overflow_y_scroll().flex().flex_col().gap(px(6.));
        let row = |id: SharedString| {
            div().id(id).flex().items_center().gap(px(10.)).px(px(8.)).py(px(6.)).rounded(px(7.)).text_size(px(12.5)).hover(|s| s.bg(t.row_hover))
        };
        match p {
            Panel::Palette { input, sel } => {
                let list = filter_cmds(&input.read(cx).text());
                let sel = (*sel).min(list.len().saturating_sub(1));
                let focused = input.read(cx).is_focused(window);
                let mut b = div().flex().flex_col().gap(px(2.)).p(px(10.)).max_h(px(380.)).overflow_hidden();
                for (i, (c, label)) in list.into_iter().enumerate().take(14) {
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
                    .child(div().px(px(16.)).py(px(8.)).text_size(px(11.)).text_color(t.faint).child("↑↓ 选择 · ↵ 执行 · Esc 关闭 · ⌘K 随时打开"))
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
                                    .child(div().text_size(px(11.)).text_color(t.muted).child(format!(
                                        "{} 封 · 未读 {} · {} · 最近 {}",
                                        s.count,
                                        s.unread,
                                        fmt_size(s.bytes as u64),
                                        fmt_full(s.last)
                                    ))),
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
                            .when(it.state != "sending", |x| {
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
                let mut b = body.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(t.muted)
                        .child("新信同步进收件箱时按顺序匹配；移动 / 归档 / 删除类规则每封信只执行第一条命中的。只处理之后到达的新信。"),
                );
                for (i, r) in list.iter().enumerate() {
                    let id = r.id;
                    let on = r.enabled;
                    let fl = rules::FIELDS.iter().find(|f| f.0 == r.field).map(|f| f.1).unwrap_or("");
                    let al = rules::ACTIONS.iter().find(|f| f.0 == r.action).map(|f| f.1).unwrap_or("");
                    b = b.child(
                        row(SharedString::from(format!("rule-{i}")))
                            .child(check_row(SharedString::from(format!("rule-on-{i}")), t, on, "").on_click(cx.listener(move |a, _, _, cx| {
                                let _ = rules::set_enabled(&a.store, id, !on);
                                a.open_rules(cx);
                            })))
                            .child(div().flex_1().child(format!(
                                "{fl}包含「{}」→ {al}{}",
                                r.value,
                                if r.action == "move" { format!("「{}」", folder_name(r.target)) } else { String::new() }
                            )))
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
                    let mut r = div().flex().gap(px(4.));
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
                b = b
                    .child(div().h(px(1.)).bg(t.border_soft).my(px(8.)))
                    .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child("新建规则"))
                    .child(chips(&rules::FIELDS, field, "rf", |p, i| {
                        if let Panel::Rules { field, .. } = p {
                            *field = i
                        }
                    }))
                    .child(field_box(t, focused).child(div().flex_1().child(value.clone())))
                    .child(chips(&rules::ACTIONS, action, "ra", |p, i| {
                        if let Panel::Rules { action, .. } = p {
                            *action = i
                        }
                    }));
                if rules::ACTIONS[action].0 == "move" {
                    b = b.child(self.folder_chips(t, "rt", target, cx, |p, f| {
                        if let Panel::Rules { target, .. } = p {
                            *target = Some(f)
                        }
                    }));
                }
                let footer = modal_footer(t).child(div().flex_1()).child(button("rule-add", t, Kind::Primary, Some("plus"), "添加规则", false).on_click(
                    cx.listener(|a, _, _, cx| {
                        let Some(Modal::Panel(Panel::Rules { field, action, target, value, .. })) = &a.modal else { return };
                        let r = rules::Rule {
                            field: rules::FIELDS[*field].0.into(),
                            action: rules::ACTIONS[*action].0.into(),
                            target: *target,
                            value: value.read(cx).text(),
                            ..Default::default()
                        };
                        match rules::add(&a.store, &r) {
                            Ok(_) => a.open_rules(cx),
                            Err(e) => a.toast(e.to_string(), true, cx),
                        }
                    }),
                ));
                modal_shell(t, 640., "本地规则", format!("{} 条", list.len()), close).child(b).child(footer).into_any_element()
            }
            Panel::Migrate { src, dest, progress, result } => {
                let (src, dest, progress, result) = (*src, *dest, *progress, result.clone());
                let b = body
                    .child(
                        div().text_size(px(11.5)).text_color(t.muted).child(
                            "把一个文件夹里的邮件复制到另一个账号（或本地归档）的文件夹，保留已读 / 星标。原邮件不删除；中断后再点「开始」会从断点继续。",
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
            Panel::Templates { list, name, body: tb } => {
                let (f1, f2) = (name.read(cx).is_focused(window), tb.read(cx).is_focused(window));
                let mut b = body.child(div().text_size(px(11.5)).text_color(t.muted).child("在写信窗口点「模板」插入。"));
                for (i, tp) in list.iter().enumerate() {
                    b = b.child(
                        row(SharedString::from(format!("tp-{i}")))
                            .child(div().w(px(120.)).flex_none().font_weight(FontWeight::SEMIBOLD).child(tp.name.clone()))
                            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(t.ink_2).child(tp.body.clone()))
                            .child(button(SharedString::from(format!("tp-del-{i}")), t, Kind::Ghost, Some("trash"), "", true).on_click(cx.listener(
                                move |a, _, _, cx| {
                                    let mut v = load_templates(&a.store);
                                    if i < v.len() {
                                        v.remove(i);
                                    }
                                    save_templates(&a.store, &v);
                                    a.open_templates(cx);
                                },
                            ))),
                    );
                }
                b = b
                    .child(div().h(px(1.)).bg(t.border_soft).my(px(8.)))
                    .child(field_box(t, f1).child(div().flex_1().child(name.clone())))
                    .child(field_box(t, f2).child(div().flex_1().child(tb.clone())));
                let footer = modal_footer(t).child(div().flex_1()).child(button("tp-add", t, Kind::Primary, Some("plus"), "添加模板", false).on_click(
                    cx.listener(|a, _, _, cx| {
                        let Some(Modal::Panel(Panel::Templates { name, body, .. })) = &a.modal else { return };
                        let (n, bd) = (name.read(cx).text(), body.read(cx).text());
                        if n.trim().is_empty() || bd.trim().is_empty() {
                            a.toast("请填写模板名称和正文", true, cx);
                            return;
                        }
                        let mut v = load_templates(&a.store);
                        v.push(Template { name: n.trim().into(), body: bd });
                        save_templates(&a.store, &v);
                        a.open_templates(cx);
                    }),
                ));
                modal_shell(t, 620., "回复模板", format!("{} 个", list.len()), close).child(b).child(footer).into_any_element()
            }
        }
    }
}
