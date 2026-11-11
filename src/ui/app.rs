//! 主窗口：顶栏 + 侧栏 + 邮件列表 + 阅读区，外加弹层、模态与提示条。对应 just-mail 的 App.jsx。

use super::accounts::{AccountsEvent, AccountsModal, Tab};
use super::composer::{build_reply, Composer, ComposerEvent};
use super::import::{ImportEvent, ImportModal};
use super::line_input::{InputEvent, LineInput};
use super::theme::{hex_css, palette, theme, Theme, ThemePref};
use super::widgets::*;
use super::*;
use crate::core::db::Store;
use crate::core::engine::{Engine, Evt, SyncState};
use crate::core::model::{addrs_text, Account, Detail, Draft, Folder, ListItem, Stats};
use crate::core::queries::{self, Filter, ListParams, View};
use crate::core::{accounts, mailer, messages, ops, parse, transfer};
use crate::webview;
use futures::StreamExt;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

const PAGE: usize = 60;
const ROW_H: f32 = 84.;

pub const VIEWS: [View; 7] = [View::Inbox, View::Unread, View::Starred, View::Attachments, View::Sent, View::Drafts, View::All];

pub fn view_label(v: View) -> &'static str {
    match v {
        View::Inbox => "统一收件箱",
        View::Unread => "未读邮件",
        View::Starred => "星标邮件",
        View::Attachments => "包含附件",
        View::Sent => "已发送",
        View::Drafts => "草稿",
        View::All => "全部邮件",
    }
}

fn view_icon(v: View) -> &'static str {
    match v {
        View::Inbox => "inbox",
        View::Unread => "unread",
        View::Starred => "star",
        View::Attachments => "clip",
        View::Sent => "sent",
        View::Drafts => "draft",
        View::All => "mail",
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Nav {
    View(View),
    Folder(i64),
}

#[derive(Clone, Debug, PartialEq)]
pub enum DetailState {
    Idle,
    Loading,
    Error(String),
    Ready,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pop {
    Theme,
    Export,
    Move(Vec<i64>),
    Folder(i64),
    Account(i64),
}

pub enum Modal {
    Composer(Entity<Composer>),
    Accounts(Entity<AccountsModal>),
    Import(Entity<ImportModal>),
    Prompt {
        title: String,
        input: Entity<LineInput>,
        action: PromptAction,
    },
    /// 附件预览：文件已解出到缓存目录，用系统 Quick Look 显示
    Preview {
        msg_id: i64,
        index: usize,
        filename: String,
        size: u64,
        path: std::path::PathBuf,
    },
}

#[derive(Clone)]
pub enum PromptAction {
    NewFolder { account_id: i64, parent: Option<i64> },
    Rename { folder_id: i64 },
}

pub struct Toast {
    pub id: u64,
    pub msg: String,
    pub error: bool,
}

pub struct MailApp {
    pub eng: Engine,
    pub store: Store,
    focus: FocusHandle,
    pub pref: ThemePref,
    pub accounts: Vec<Account>,
    pub folders: Vec<Folder>,
    pub stats: Stats,
    pub nav: Nav,
    pub filter: Filter,
    search: Entity<LineInput>,
    pub q: String,
    search_gen: u64,
    pub items: Vec<ListItem>,
    pub drafts: Vec<Draft>,
    pub total: i64,
    next: Option<(i64, i64)>,
    pub loading: bool,
    list_gen: u64,
    pub selected: Option<i64>,
    pub checked: BTreeSet<i64>,
    /// 当前打开的邮件。用 Rc：每帧渲染只复制指针，不复制可能几 MB 的 HTML
    pub detail: Option<Rc<Detail>>,
    /// (邮件 id, HTML 长度, 是否引用了远程资源)：正则只在换信时跑一次，不是每帧
    remote_cache: Option<(i64, usize, bool)>,
    /// load_meta 的请求代号：只应用最新一次后台查询的结果
    meta_gen: u64,
    pub detail_state: DetailState,
    pub remote_ok: HashSet<i64>,
    pub text_mode: bool,
    collapsed: HashSet<i64>,
    pub sync: HashMap<i64, SyncState>,
    pub toasts: Vec<Toast>,
    toast_seq: u64,
    pub pop: Option<(Pop, Point<Pixels>)>,
    pop_closed_at: Instant,
    sidebar_w: f32,
    list_w: f32,
    /// 正在拖拽的分隔条：0 = 侧栏 | 列表，1 = 列表 | 阅读区
    resizing: Option<u8>,
    /// 阅读区折叠：列表占满右侧
    reader_collapsed: bool,
    pub modal: Option<Modal>,
    snap: Option<Arc<Image>>,
    snapping: bool,
    doc: Option<(u64, Arc<str>)>,
    list_scroll: UniformListScrollHandle,
    refresh_task: Option<Task<()>>,
    /// 最近一次收到新邮件提醒（测试用）
    pub last_newmail: Option<String>,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
    /// 当前模态的订阅：同一时间只有一个模态，换掉时旧订阅随之释放（以前一直往 _subs 里追加、从不清理）
    modal_sub: Option<Subscription>,
}

impl MailApp {
    pub fn new(eng: Engine, rx: futures::channel::mpsc::UnboundedReceiver<Evt>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = eng.store().clone();
        let pref = store
            .get_kv("theme")
            .and_then(|s| serde_json::from_str::<(String, String, String)>(&s).ok())
            .map(|(pref, light, dark)| ThemePref { pref, light, dark })
            .unwrap_or_default();
        cx.set_global(Theme::by_id(pref.resolve(window.appearance())));
        let search = cx.new(|cx| LineInput::new("搜索主题、联系人、正文…", cx));
        let mut subs = vec![cx.subscribe_in(&search, window, |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::Changed => this.search_changed(cx),
            InputEvent::Cancel => {
                if this.search.read(cx).text().is_empty() {
                    window.focus(&this.focus);
                } else {
                    this.search.update(cx, |s, cx| s.set_text("", cx));
                    this.search_changed(cx);
                }
            }
            InputEvent::Submit { .. } | InputEvent::Down => window.focus(&this.focus),
            _ => {}
        })];
        subs.push(cx.observe_window_appearance(window, |this: &mut Self, window, cx| {
            if this.pref.pref == "auto" {
                this.apply_theme(window.appearance(), cx);
            }
        }));
        let collapsed = store.get_kv("collapsed").and_then(|s| serde_json::from_str::<Vec<i64>>(&s).ok()).unwrap_or_default().into_iter().collect();
        let mut tasks = vec![];
        // 引擎事件 → 界面
        let mut rx = rx;
        tasks.push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            while let Some(e) = rx.next().await {
                if this.update(cx, |a, cx| a.on_evt(e, cx)).is_err() {
                    break;
                }
            }
        }));
        // WebView 里点的链接、托盘菜单命令：原生回调写入队列后发唤醒信号，这里 await 信号再取，空闲时不唤醒
        let mut wake = macos::take_wake_receiver();
        tasks.push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| loop {
            match wake.as_mut() {
                Some(rx) => {
                    if rx.next().await.is_none() {
                        break;
                    }
                }
                // 接收端已被取走（理论上不会）：退回轮询
                None => cx.background_executor().timer(Duration::from_millis(120)).await,
            }
            let links = webview::take_clicked();
            let cmds = macos::take_commands();
            if links.is_empty() && cmds.is_empty() {
                if this.upgrade().is_none() {
                    break;
                }
                continue;
            }
            let ok = this
                .update(cx, |a, cx| {
                    for l in links {
                        a.open_link(&l, cx);
                    }
                    for c in cmds {
                        a.tray_command(c, cx);
                    }
                })
                .is_ok();
            if !ok {
                break;
            }
        }));
        let mut app = MailApp {
            eng,
            store,
            focus: cx.focus_handle(),
            pref,
            accounts: vec![],
            folders: vec![],
            stats: Stats::default(),
            nav: Nav::View(View::Inbox),
            filter: Filter::All,
            search,
            q: String::new(),
            search_gen: 0,
            items: vec![],
            drafts: vec![],
            total: 0,
            next: None,
            loading: false,
            list_gen: 0,
            selected: None,
            checked: BTreeSet::new(),
            detail: None,
            remote_cache: None,
            meta_gen: 0,
            detail_state: DetailState::Idle,
            remote_ok: HashSet::new(),
            text_mode: false,
            collapsed,
            sync: HashMap::new(),
            toasts: vec![],
            toast_seq: 0,
            pop: None,
            pop_closed_at: Instant::now(),
            sidebar_w: 220.,
            list_w: 380.,
            resizing: None,
            reader_collapsed: false,
            modal: None,
            snap: None,
            snapping: false,
            doc: None,
            list_scroll: UniformListScrollHandle::new(),
            refresh_task: None,
            last_newmail: None,
            _tasks: tasks,
            _subs: subs,
            modal_sub: None,
        };
        // 首屏同步读一次，免得窗口先空一下；之后的刷新都在后台
        let meta = Meta::fetch(&app.store);
        app.apply_meta(meta, cx);
        app.load_list(false, false, cx);
        window.focus(&app.focus);
        app
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn search_input(&self) -> &Entity<LineInput> {
        &self.search
    }

    // ───────────────────────── 数据 ─────────────────────────

    /// 账号、文件夹（含逐夹计数）、统计、未读数：在后台查，查完回主线程应用
    pub fn load_meta(&mut self, cx: &mut Context<Self>) {
        self.meta_gen += 1;
        let gen = self.meta_gen;
        let store = self.store.clone();
        let task = cx.background_spawn(async move { Meta::fetch(&store) });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let m = task.await;
            this.update(cx, |a, cx| {
                if a.meta_gen == gen {
                    a.apply_meta(m, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply_meta(&mut self, m: Meta, cx: &mut Context<Self>) {
        self.accounts = m.accounts;
        self.folders = m.folders;
        self.stats = m.stats;
        for a in &self.accounts {
            let st = self.eng.sync_state(a.id);
            self.sync.insert(a.id, st);
        }
        if let Nav::Folder(id) = self.nav {
            if !self.folders.iter().any(|f| f.id == id) {
                self.nav = Nav::View(View::Inbox);
            }
        }
        macos::set_unread(m.inbox_unread);
        cx.notify();
    }

    fn is_drafts(&self) -> bool {
        self.nav == Nav::View(View::Drafts) && self.q.is_empty()
    }

    /// 正在看某个账号的「草稿箱」文件夹：返回该账号 id（本机草稿也要显示在这里）
    fn folder_drafts_account(&self) -> Option<i64> {
        match self.nav {
            Nav::Folder(id) if self.q.is_empty() => self.folders.iter().find(|f| f.id == id && f.role == "drafts").map(|f| f.account_id),
            _ => None,
        }
    }

    /// 未同步的 Gmail 虚拟文件夹映射成本地视图：「所有邮件」→ 该账号全部邮件，「已加星标」→ 该账号星标邮件
    fn virtual_folder(&self, id: i64) -> Option<(View, i64)> {
        let f = self.folders.iter().find(|f| f.id == id && !f.sync_enabled)?;
        let view = match f.role.as_str() {
            "all" => View::All,
            "flagged" => View::Starred,
            _ => return None,
        };
        Some((view, f.account_id))
    }

    pub fn params(&self) -> ListParams {
        let mut p = ListParams::default();
        match self.nav {
            Nav::Folder(id) => match self.virtual_folder(id) {
                Some((v, acc)) => {
                    p.view = v;
                    p.account_id = Some(acc);
                }
                None => p.folder_id = Some(id),
            },
            Nav::View(v) => p.view = if self.q.is_empty() { v } else { View::All },
        }
        if self.q.is_empty() {
            p.filter = self.filter;
        } else {
            p.q = self.q.clone();
        }
        p
    }

    /// silent：后台刷新（保留现有条目、至少重取当前已加载的数量）；append：加载更早的一页
    pub fn load_list(&mut self, silent: bool, append: bool, cx: &mut Context<Self>) {
        self.list_gen += 1;
        let gen = self.list_gen;
        if self.is_drafts() {
            self.drafts = mailer::list_drafts(&self.store).unwrap_or_default();
            self.items.clear();
            self.total = self.drafts.len() as i64;
            self.next = None;
            self.loading = false;
            cx.notify();
            return;
        }
        self.drafts = match self.folder_drafts_account() {
            Some(acc) => mailer::list_drafts(&self.store).unwrap_or_default().into_iter().filter(|d| d.account_id == Some(acc)).collect(),
            None => Vec::new(),
        };
        if !silent && !append {
            self.items.clear();
            self.list_scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        self.loading = true;
        let limit = if silent { self.items.len().clamp(PAGE, 400) } else { PAGE };
        let cursor = if append { self.next } else { None };
        let store = self.store.clone();
        let p = self.params();
        let task = cx.background_spawn(async move { queries::list(&store, &p, cursor, limit) });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let r = task.await;
            this.update(cx, |a, cx| {
                if a.list_gen != gen {
                    return;
                }
                a.loading = false;
                match r {
                    Ok(page) => {
                        if append {
                            let have: HashSet<i64> = a.items.iter().map(|m| m.id).collect();
                            a.items.extend(page.items.into_iter().filter(|m| !have.contains(&m.id)));
                        } else {
                            a.items = page.items;
                        }
                        a.total = page.total;
                        a.next = page.next;
                        let ids: HashSet<i64> = a.items.iter().map(|m| m.id).collect();
                        a.checked.retain(|id| ids.contains(id));
                    }
                    Err(e) => a.toast(format!("读取列表失败：{e}"), true, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 防抖的后台刷新（同步事件频繁时合并）
    fn soft_refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            this.update(cx, |a, cx| {
                a.load_meta(cx);
                a.load_list(true, false, cx);
                a.refresh_detail(cx);
            })
            .ok();
        }));
    }

    fn refresh_detail(&mut self, cx: &mut Context<Self>) {
        let (Some(id), DetailState::Ready) = (self.selected, &self.detail_state) else { return };
        let store = self.store.clone();
        let task = cx.background_spawn(async move { queries::detail(&store, id) });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Ok(Some(d)) = task.await else { return };
            this.update(cx, |a, cx| {
                if a.selected == Some(id) && matches!(a.detail_state, DetailState::Ready) && a.detail.as_deref() != Some(&d) {
                    a.detail = Some(Rc::new(d));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn on_evt(&mut self, e: Evt, cx: &mut Context<Self>) {
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
                self.toast(msg.clone(), false, cx);
                let body = if count == 1 && !subject.is_empty() { format!("{from}：{subject}") } else { msg.clone() };
                macos::notify("新邮件", &body);
                self.last_newmail = Some(msg);
                self.soft_refresh(cx);
            }
            Evt::Watch { .. } => cx.notify(),
            Evt::Toast { msg, error } => self.toast(msg, error, cx),
        }
    }

    pub fn toast(&mut self, msg: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        self.toast_seq += 1;
        let id = self.toast_seq;
        self.toasts.push(Toast { id, msg: msg.into(), error });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(Duration::from_millis(if error { 6000 } else { 3200 })).await;
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
    fn run<T: Send + 'static>(
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
    fn run_op<T: Send + 'static>(
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

    // ───────────────────────── 导航 ─────────────────────────

    pub fn set_nav(&mut self, nav: Nav, cx: &mut Context<Self>) {
        self.nav = nav;
        self.filter = Filter::All;
        self.checked.clear();
        self.close_detail(cx);
        // 搜索词保留：切换文件夹后在该文件夹内继续搜索
        self.load_list(false, false, cx);
    }

    pub fn set_filter(&mut self, f: Filter, cx: &mut Context<Self>) {
        self.filter = f;
        self.checked.clear();
        self.load_list(false, false, cx);
    }

    fn search_changed(&mut self, cx: &mut Context<Self>) {
        self.search_gen += 1;
        let gen = self.search_gen;
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            this.update(cx, |a, cx| {
                if a.search_gen != gen {
                    return;
                }
                let q = a.search.read(cx).text().trim().to_string();
                if q != a.q {
                    a.set_query(q, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn set_query(&mut self, q: String, cx: &mut Context<Self>) {
        self.q = q;
        self.checked.clear();
        self.close_detail(cx);
        self.load_list(false, false, cx);
    }

    fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.detail = None;
        self.detail_state = DetailState::Idle;
        cx.notify();
    }

    pub fn open_message(&mut self, id: i64, cx: &mut Context<Self>) {
        self.selected = Some(id);
        self.detail_state = DetailState::Loading;
        self.text_mode = false;
        if self.pop.as_ref().is_some_and(|p| matches!(p.0, Pop::Move(_))) {
            self.pop = None;
        }
        cx.notify();
        let store = self.store.clone();
        let eng = self.eng.clone();
        let fut = self.eng.call(async move {
            if !messages::has_body(&store, id) {
                eng.ensure_body(id).await?;
            }
            queries::detail(&store, id)
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let r = fut.await;
            this.update(cx, |a, cx| {
                if a.selected != Some(id) {
                    return;
                }
                match r {
                    Ok(Some(d)) => {
                        let unseen = !d.seen;
                        a.detail = Some(Rc::new(d));
                        a.detail_state = DetailState::Ready;
                        if unseen {
                            a.patch(&[id], Some(true), None);
                            let eng = a.eng.clone();
                            a.run(ops::set_flags(eng, vec![id], ops::FlagChange { seen: Some(true), flagged: None }), cx, |a, r, cx| {
                                if let Err(e) = r {
                                    a.toast(e.to_string(), true, cx);
                                }
                                a.load_meta(cx);
                            });
                        }
                    }
                    Ok(None) => a.detail_state = DetailState::Error("邮件不存在（可能已被删除）".into()),
                    Err(e) => {
                        // 正文取不到时仍显示信封信息（离线 / 服务器错误），并给出原因
                        match queries::detail(&a.store, id) {
                            Ok(Some(d)) if !d.text.is_empty() || !d.html.is_empty() => {
                                a.detail = Some(Rc::new(d));
                                a.detail_state = DetailState::Ready;
                            }
                            _ => a.detail_state = DetailState::Error(format!("取不回这封信：{e}")),
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn patch(&mut self, ids: &[i64], seen: Option<bool>, flagged: Option<bool>) {
        for m in self.items.iter_mut().filter(|m| ids.contains(&m.id)) {
            if let Some(s) = seen {
                m.seen = s;
            }
            if let Some(f) = flagged {
                m.flagged = f;
            }
        }
        if let Some(d) = self.detail.as_mut().filter(|d| ids.contains(&d.id)).map(Rc::make_mut) {
            if let Some(s) = seen {
                d.seen = s;
            }
            if let Some(f) = flagged {
                d.flagged = f;
            }
        }
    }

    fn remove_from_list(&mut self, ids: &[i64], cx: &mut Context<Self>) {
        let idx = self.selected.and_then(|s| self.items.iter().position(|m| m.id == s));
        self.items.retain(|m| !ids.contains(&m.id));
        self.total = (self.total - ids.len() as i64).max(0);
        self.checked.clear();
        if self.selected.is_some_and(|s| ids.contains(&s)) {
            match idx.and_then(|i| self.items.get(i.min(self.items.len().saturating_sub(1)))).map(|m| m.id) {
                Some(next) => self.open_message(next, cx),
                None => self.close_detail(cx),
            }
        }
        cx.notify();
    }

    pub fn set_seen(&mut self, ids: Vec<i64>, seen: bool, cx: &mut Context<Self>) {
        self.patch(&ids, Some(seen), None);
        let eng = self.eng.clone();
        self.run_op(
            ops::set_flags(eng, ids, ops::FlagChange { seen: Some(seen), flagged: None }),
            move |_| if seen { "已标为已读".into() } else { "已标为未读".into() },
            cx,
        );
        cx.notify();
    }

    pub fn set_flagged(&mut self, ids: Vec<i64>, flagged: bool, quiet: bool, cx: &mut Context<Self>) {
        self.patch(&ids, None, Some(flagged));
        let eng = self.eng.clone();
        self.run_op(
            ops::set_flags(eng, ids, ops::FlagChange { seen: None, flagged: Some(flagged) }),
            move |_| {
                if quiet {
                    String::new()
                } else if flagged {
                    "已加星标".into()
                } else {
                    "已取消星标".into()
                }
            },
            cx,
        );
        cx.notify();
    }

    pub fn delete(&mut self, ids: Vec<i64>, window: &mut Window, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let local_ids: HashSet<i64> = self.accounts.iter().filter(|a| a.is_local()).map(|a| a.id).collect();
        let locals = self.items.iter().filter(|m| ids.contains(&m.id) && local_ids.contains(&m.account_id)).count()
            + self.detail.as_ref().filter(|d| ids.contains(&d.id) && d.account_local && !self.items.iter().any(|m| m.id == d.id)).map_or(0, |_| 1);
        if locals > 0 {
            let answer = window.prompt(
                PromptLevel::Warning,
                &format!("其中 {locals} 封来自「本地归档」，删除后无法恢复。"),
                Some("邮箱服务器里的邮件不受影响。确认删除？"),
                &["删除", "取消"],
                cx,
            );
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                if answer.await.ok() == Some(0) {
                    this.update(cx, |a, cx| a.do_delete(ids, cx)).ok();
                }
            })
            .detach();
            return;
        }
        self.do_delete(ids, cx);
    }

    fn do_delete(&mut self, ids: Vec<i64>, cx: &mut Context<Self>) {
        self.remove_from_list(&ids, cx);
        let n = ids.len();
        let eng = self.eng.clone();
        self.run_op(
            ops::delete_messages(eng, ids),
            move |r| {
                if r.purged > 0 && r.trashed == 0 {
                    format!("已彻底删除 {} 封", r.purged)
                } else {
                    format!("已移入垃圾箱 · {} 封", if r.trashed > 0 { r.trashed } else { n })
                }
            },
            cx,
        );
    }

    pub fn move_to(&mut self, ids: Vec<i64>, folder_id: i64, cx: &mut Context<Self>) {
        let label = self.folders.iter().find(|f| f.id == folder_id).map(|f| f.label()).unwrap_or_else(|| "文件夹".into());
        self.pop = None;
        self.remove_from_list(&ids, cx);
        let eng = self.eng.clone();
        self.run_op(ops::move_messages(eng, ids, folder_id), move |_| format!("已移动到「{label}」"), cx);
    }

    pub fn to_role(&mut self, ids: Vec<i64>, role: &'static str, msg: &'static str, cx: &mut Context<Self>) {
        self.remove_from_list(&ids, cx);
        let eng = self.eng.clone();
        self.run_op(async move { ops::move_to_role(eng, ids, role).await }, move |_| msg.into(), cx);
    }

    fn has_role(&self, account_id: i64, role: &str) -> bool {
        self.folders.iter().any(|f| f.account_id == account_id && f.role == role)
    }

    pub fn compose_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub fn compose_to(&mut self, to: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.compose_new(window, cx);
        if let Some(Modal::Composer(c)) = &self.modal {
            let to = to.to_string();
            c.update(cx, |c, cx| c.set_to(&to, cx));
        }
    }

    pub fn reply(&mut self, mode: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.as_ref().filter(|_| self.detail_state == DetailState::Ready) else { return };
        if !self.accounts.iter().any(|a| !a.is_local()) {
            self.open_accounts(Tab::Accounts, true, window, cx);
            return;
        }
        let draft = build_reply(d, mode, &self.accounts);
        self.open_composer(draft, window, cx);
    }

    pub fn open_composer(&mut self, d: Draft, window: &mut Window, cx: &mut Context<Self>) {
        let eng = self.eng.clone();
        let accs = self.accounts.clone();
        let c = cx.new(|cx| Composer::new(eng, accs, d, window, cx));
        let sub = cx.subscribe_in(&c, window, |this: &mut Self, _, ev: &ComposerEvent, window, cx| {
            match ev {
                ComposerEvent::Closed { saved } => {
                    this.modal = None;
                    if *saved {
                        this.toast("已保存到「草稿箱」", false, cx);
                    }
                }
                ComposerEvent::Sent(r) => {
                    this.modal = None;
                    this.toast(format!("已发送{}", if r.saved_to_sent { " · 已存入「已发送」" } else { "" }), false, cx);
                }
                ComposerEvent::Toast(m, err) => {
                    this.toast(m.clone(), *err, cx);
                    return;
                }
            }
            window.focus(&this.focus);
            this.load_meta(cx);
            this.load_list(true, false, cx);
            this.refresh_detail(cx);
        });
        self.modal_sub = Some(sub);
        self.pop = None;
        self.modal = Some(Modal::Composer(c));
        cx.notify();
    }

    pub fn open_accounts(&mut self, tab: Tab, adding: bool, window: &mut Window, cx: &mut Context<Self>) {
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

    pub fn open_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn open_prompt(&mut self, title: String, initial: &str, action: PromptAction, window: &mut Window, cx: &mut Context<Self>) {
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

    fn submit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Prompt { input, action, .. }) = &self.modal else { return };
        let name = input.read(cx).text().trim().to_string();
        let action = action.clone();
        if name.is_empty() {
            return;
        }
        let r = match action {
            PromptAction::NewFolder { account_id, parent } => ops::create_folder(&self.store, account_id, &name, parent).map(|_| "文件夹已创建"),
            PromptAction::Rename { folder_id } => ops::rename_folder(&self.store, folder_id, &name).map(|_| "已重命名"),
        };
        match r {
            Ok(m) => {
                self.modal = None;
                window.focus(&self.focus);
                self.toast(m, false, cx);
                self.load_meta(cx);
                self.load_list(true, false, cx);
            }
            Err(e) => self.toast(e.to_string(), true, cx),
        }
    }

    pub fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.accounts.iter().all(|a| a.is_local()) {
            self.toast("还没有接入邮箱账号", false, cx);
            return;
        }
        if let Nav::Folder(fid) = self.nav {
            if let Some(f) = self.folders.iter().find(|f| f.id == fid) {
                if !self.accounts.iter().any(|a| a.id == f.account_id && a.is_local()) {
                    // 映射到本地视图的虚拟文件夹自身不同步，刷新整个账号
                    let target = if self.virtual_folder(fid).is_some() { None } else { Some(fid) };
                    self.eng.request_sync(f.account_id, target, false);
                    self.toast("正在同步此文件夹…", false, cx);
                    return;
                }
            }
        }
        self.eng.sync_all();
        self.toast("正在同步全部账号…", false, cx);
    }

    fn folder_action(&mut self, action: &str, fid: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.pop = None;
        let Some(f) = self.folders.iter().find(|f| f.id == fid).cloned() else { return };
        let label = f.label();
        match action {
            "read-all" => {
                let eng = self.eng.clone();
                self.run(ops::mark_folder_read(eng, fid), cx, move |a, r, cx| {
                    match r {
                        Ok(()) => a.toast(format!("「{label}」已全部标为已读"), false, cx),
                        Err(e) => a.toast(e.to_string(), true, cx),
                    }
                    a.load_meta(cx);
                    a.load_list(true, false, cx);
                });
            }
            "sync" => {
                self.eng.request_sync(f.account_id, Some(fid), false);
                self.toast(format!("正在同步「{label}」"), false, cx);
            }
            "toggle-sync" => match ops::set_folder_sync(&self.store, fid, !f.sync_enabled) {
                Ok(()) => {
                    self.toast(if f.sync_enabled { format!("「{label}」已暂停同步") } else { format!("「{label}」已恢复同步") }, false, cx);
                    self.load_meta(cx);
                }
                Err(e) => self.toast(e.to_string(), true, cx),
            },
            "new-sub" => self.open_prompt(
                format!("在「{label}」下新建子文件夹"),
                "",
                PromptAction::NewFolder { account_id: f.account_id, parent: Some(fid) },
                window,
                cx,
            ),
            "rename" => self.open_prompt("新的文件夹名称".into(), &f.name, PromptAction::Rename { folder_id: fid }, window, cx),
            "delete" => {
                let answer = window.prompt(
                    PromptLevel::Warning,
                    &format!("删除文件夹「{label}」？"),
                    Some("其中的邮件会一并从本机删除，无法恢复。"),
                    &["删除", "取消"],
                    cx,
                );
                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    if answer.await.ok() == Some(0) {
                        this.update(cx, |a, cx| {
                            if a.nav == Nav::Folder(fid) {
                                a.set_nav(Nav::View(View::Inbox), cx);
                            }
                            match ops::delete_folder(&a.store, fid) {
                                Ok(()) => a.toast("文件夹已删除", false, cx),
                                Err(e) => a.toast(e.to_string(), true, cx),
                            }
                            a.load_meta(cx);
                        })
                        .ok();
                    }
                })
                .detach();
            }
            _ => {}
        }
    }

    fn export(&mut self, kind: transfer::ExportKind, cx: &mut Context<Self>) {
        self.pop = None;
        let mut p = self.params();
        if !self.checked.is_empty() {
            p = ListParams { ids: self.checked.iter().copied().collect(), ..Default::default() };
        }
        let label = self.title();
        let dl = dirs::download_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default());
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M").to_string();
        let safe: String = label.chars().map(|c| if "/\\:*?\"<>|「」".contains(c) { '_' } else { c }).collect();
        let rx = match kind {
            transfer::ExportKind::Mbox => Some(cx.prompt_for_new_path(&dl, Some(&format!("{safe}-{stamp}.mbox")))),
            transfer::ExportKind::Csv => Some(cx.prompt_for_new_path(&dl, Some(&format!("{safe}-{stamp}.csv")))),
            transfer::ExportKind::Maildir => None,
        };
        let dirs_rx = if rx.is_none() {
            Some(cx.prompt_for_paths(PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some("导出到这里".into()),
                allowed_extensions: None,
            }))
        } else {
            None
        };
        let eng = self.eng.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let dest = if let Some(rx) = rx {
                rx.await.ok().and_then(|r| r.ok()).flatten()
            } else if let Some(rx) = dirs_rx {
                rx.await.ok().and_then(|r| r.ok()).flatten().and_then(|v| v.into_iter().next())
            } else {
                None
            };
            let Some(dest) = dest else { return };
            this.update(cx, |a, cx| a.toast("正在导出…", false, cx)).ok();
            let r = eng.call(transfer::export(eng.clone(), p, kind, dest, label)).await;
            this.update(cx, |a, cx| match r {
                Ok(s) => {
                    let skipped = if s.failed > 0 { format!("（{} 封正文取不到，已跳过）", s.failed) } else { String::new() };
                    let skipped = if s.truncated {
                        format!("{skipped}（命中超过 {} 封，只导出了最早的 {} 封，请缩小范围分批导出）", transfer::EXPORT_CAP, transfer::EXPORT_CAP)
                    } else {
                        skipped
                    };
                    a.toast(format!("已导出 {} 封到 {}{skipped}", s.written, s.path.display()), false, cx);
                    cx.reveal_path(&s.path);
                }
                Err(e) => a.toast(format!("导出失败：{e}"), true, cx),
            })
            .ok();
        })
        .detach();
    }

    /// 点附件：解出到缓存目录后弹窗预览（弹窗里可「另存为」）
    fn open_preview(&mut self, msg_id: i64, index: usize, filename: String, size: u64, cx: &mut Context<Self>) {
        let src = self.eng.source(msg_id);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let r = async {
                let raw = src.await?;
                let name = filename.clone();
                cx.background_spawn(async move {
                    let (_, _, data) = parse::attachment(&raw, index).ok_or_else(|| anyhow::anyhow!("附件不存在"))?;
                    // 每个附件一个子目录，保留原文件名（Quick Look 靠扩展名识别类型）
                    let safe: String = name.chars().map(|c| if "/\\:\0".contains(c) { '_' } else { c }).collect();
                    let safe = if safe.trim().is_empty() { "attachment".to_string() } else { safe };
                    let dir = preview_dir().join(format!("{msg_id}-{index}"));
                    std::fs::create_dir_all(&dir)?;
                    let path = dir.join(safe);
                    std::fs::write(&path, data)?;
                    anyhow::Ok(path)
                })
                .await
            }
            .await;
            this.update(cx, |a, cx| match r {
                Ok(path) => {
                    a.pop = None;
                    a.modal = Some(Modal::Preview { msg_id, index, filename, size, path });
                    cx.notify();
                }
                Err(e) => a.toast(format!("预览失败：{e}"), true, cx),
            })
            .ok();
        })
        .detach();
    }

    fn save_attachment(&mut self, msg_id: i64, index: usize, filename: String, cx: &mut Context<Self>) {
        let dl = dirs::download_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default());
        let rx = cx.prompt_for_new_path(&dl, Some(&filename));
        let src = self.eng.source(msg_id);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Some(path) = rx.await.ok().and_then(|r| r.ok()).flatten() else { return };
            let r = async {
                let raw = src.await?;
                // 解析整封原文 + 写文件放在后台线程，不占前台执行器
                cx.background_spawn(async move {
                    let (_, _, data) = parse::attachment(&raw, index).ok_or_else(|| anyhow::anyhow!("附件不存在"))?;
                    std::fs::write(&path, data)?;
                    anyhow::Ok(path)
                })
                .await
            }
            .await;
            this.update(cx, |a, cx| match r {
                Ok(p) => a.toast(format!("已保存到 {}", p.display()), false, cx),
                Err(e) => a.toast(format!("保存失败：{e}"), true, cx),
            })
            .ok();
        })
        .detach();
    }

    fn save_eml(&mut self, msg_id: i64, subject: &str, cx: &mut Context<Self>) {
        let dl = dirs::download_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default());
        let name: String =
            if subject.is_empty() { "message".into() } else { subject.chars().take(60).map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect() };
        let rx = cx.prompt_for_new_path(&dl, Some(&format!("{name}.eml")));
        let src = self.eng.source(msg_id);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Some(path) = rx.await.ok().and_then(|r| r.ok()).flatten() else { return };
            let r = async {
                let raw = src.await?;
                std::fs::write(&path, raw)?;
                anyhow::Ok(path)
            }
            .await;
            this.update(cx, |a, cx| match r {
                Ok(p) => a.toast(format!("已保存到 {}", p.display()), false, cx),
                Err(e) => a.toast(format!("保存失败：{e}"), true, cx),
            })
            .ok();
        })
        .detach();
    }

    fn open_link(&mut self, url: &str, cx: &mut Context<Self>) {
        if let Some(addr) = url.strip_prefix("mailto:") {
            let addr = addr.split('?').next().unwrap_or("").to_string();
            let Some(win) = cx.windows().first().copied() else { return };
            let me = cx.entity();
            cx.defer(move |cx| {
                win.update(cx, |_, window, cx| me.update(cx, |a, cx| a.compose_to(&addr, window, cx))).ok();
            });
        } else if url.starts_with("http://") || url.starts_with("https://") {
            cx.open_url(url);
        }
    }

    fn tray_command(&mut self, c: macos::Command, cx: &mut Context<Self>) {
        let Some(win) = cx.windows().first().copied() else { return };
        let me = cx.entity();
        match c {
            macos::Command::Show => {
                cx.activate(true);
                win.update(cx, |_, w, _| w.activate_window()).ok();
            }
            macos::Command::Compose => {
                cx.activate(true);
                cx.defer(move |cx| {
                    win.update(cx, |_, window, cx| {
                        window.activate_window();
                        me.update(cx, |a, cx| a.compose_new(window, cx))
                    })
                    .ok();
                });
            }
            macos::Command::SyncAll => self.sync_now(cx),
            macos::Command::RevealData => cx.reveal_path(self.store.dir()),
            macos::Command::Quit => cx.quit(),
        }
    }

    // ───────────────────────── 主题 ─────────────────────────

    pub fn set_theme_pref(&mut self, pref: ThemePref, window: &mut Window, cx: &mut Context<Self>) {
        self.pref = pref;
        self.store.set_kv("theme", &serde_json::to_string(&(&self.pref.pref, &self.pref.light, &self.pref.dark)).unwrap_or_default());
        self.apply_theme(window.appearance(), cx);
    }

    fn apply_theme(&mut self, appearance: WindowAppearance, cx: &mut Context<Self>) {
        cx.set_global(Theme::by_id(self.pref.resolve(appearance)));
        self.doc = None;
        cx.notify();
    }

    fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cur = theme(cx).id;
        let ix = palettes::PALETTES.iter().position(|p| p.id == cur).unwrap_or(0);
        let next = palettes::PALETTES[(ix + 1) % palettes::PALETTES.len()].id;
        let mut p = self.pref.clone();
        p.pref = next.into();
        self.set_theme_pref(p, window, cx);
        self.toast(format!("配色：{}", palette(next).name), false, cx);
    }

    // ───────────────────────── 弹层 ─────────────────────────

    fn toggle_pop(&mut self, pop: Pop, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.pop.as_ref().is_some_and(|p| p.0 == pop) || (self.pop.is_none() && self.pop_closed_at.elapsed() < Duration::from_millis(250)) {
            self.pop = None;
        } else {
            self.pop = Some((pop, at));
        }
        cx.notify();
    }

    /// 测试用：在固定位置打开弹层
    pub fn toggle_pop_for_test(&mut self, pop: Pop, cx: &mut Context<Self>) {
        self.pop = Some((pop, point(px(900.), px(60.))));
        cx.notify();
    }

    fn overlay_open(&self) -> bool {
        self.pop.is_some() || self.modal.is_some()
    }

    /// 弹层 / 模态打开时把 WebView 冻结成快照（原生视图会盖住 GPUI 画的弹层）
    fn sync_freeze(&mut self, cx: &mut Context<Self>) {
        let want = self.overlay_open();
        if !want {
            if webview::is_frozen() {
                webview::set_frozen(false);
            }
            self.snap = None;
            return;
        }
        if !webview::is_live() || webview::is_frozen() || self.snapping {
            return;
        }
        self.snapping = true;
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let png = webview::snapshot(cx).await;
            this.update(cx, |a, cx| {
                a.snapping = false;
                if a.overlay_open() {
                    a.snap = png.map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b)));
                    webview::set_frozen(true);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn title(&self) -> String {
        if !self.q.is_empty() {
            return format!("搜索「{}」", self.q);
        }
        match self.nav {
            Nav::Folder(id) => self.folders.iter().find(|f| f.id == id).map(|f| f.label()).unwrap_or_default(),
            Nav::View(v) => view_label(v).into(),
        }
    }

    fn subtitle(&self) -> String {
        let folder = match self.nav {
            Nav::Folder(id) => self.folders.iter().find(|f| f.id == id),
            _ => None,
        };
        if !self.q.is_empty() {
            return format!("{} 封结果 · {}", fmt_num(self.total), folder.map(|f| format!("仅「{}」", f.label())).unwrap_or_else(|| "全部账号与文件夹".into()));
        }
        if let Some(f) = folder {
            let acc = self.accounts.iter().find(|a| a.id == f.account_id);
            if self.virtual_folder(f.id).is_some() {
                return format!("{} · {} 封（取自本地已同步的文件夹）", acc.map(|a| a.email.as_str()).unwrap_or(""), fmt_num(self.total));
            }
            return if acc.is_some_and(|a| a.is_local()) {
                format!("本地归档 · 共 {} 封（只在本机）", fmt_num(f.local))
            } else {
                format!("{} · 服务器 {} 封 · 本地 {} 封", acc.map(|a| a.email.as_str()).unwrap_or(""), fmt_num(f.total), fmt_num(f.local))
            };
        }
        if self.is_drafts() {
            return format!("{} 封草稿（保存在本机）", self.total);
        }
        let n = self.accounts.iter().filter(|a| !a.is_local()).count();
        format!("{} 封邮件{}", fmt_num(self.total), if n > 1 { format!(" · {n} 个账号") } else { String::new() })
    }

    // ───────────────────────── 键盘 ─────────────────────────

    fn go(&mut self, delta: i64, cx: &mut Context<Self>) {
        if self.modal.is_some() || self.items.is_empty() {
            return;
        }
        let idx = self.selected.and_then(|s| self.items.iter().position(|m| m.id == s));
        let next = match idx {
            None => 0,
            Some(i) => (i as i64 + delta).clamp(0, self.items.len() as i64 - 1) as usize,
        };
        if Some(next) == idx {
            return;
        }
        let id = self.items[next].id;
        self.list_scroll.scroll_to_item(next, ScrollStrategy::Center);
        self.open_message(id, cx);
    }

    fn current_ids(&self) -> Vec<i64> {
        if !self.checked.is_empty() {
            self.checked.iter().copied().collect()
        } else {
            self.detail.as_ref().filter(|_| self.detail_state == DetailState::Ready).map(|d| vec![d.id]).unwrap_or_default()
        }
    }

    fn on_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        if self.pop.is_some() {
            self.pop = None;
        } else if self.modal.is_some() {
            match self.modal.take() {
                Some(Modal::Composer(c)) => {
                    self.modal = Some(Modal::Composer(c.clone()));
                    c.update(cx, |c, cx| c.close(window, cx));
                }
                Some(Modal::Prompt { .. }) | Some(Modal::Preview { .. }) | None => {}
                Some(m @ Modal::Import(_)) => {
                    if let Modal::Import(i) = &m {
                        if i.read(cx).busy() {
                            self.modal = Some(m);
                            return;
                        }
                    }
                    self.load_meta(cx);
                }
                Some(Modal::Accounts(_)) => {
                    self.load_meta(cx);
                }
            }
            window.focus(&self.focus);
        } else if !self.checked.is_empty() {
            self.checked.clear();
        } else {
            self.close_detail(cx);
        }
        cx.notify();
    }
}

// ───────────────────────────── 渲染 ─────────────────────────────

fn title_drag(ev: &MouseDownEvent, window: &mut Window) {
    if ev.click_count >= 2 {
        window.zoom_window();
    } else {
        window.start_window_move();
    }
}

impl MailApp {
    fn render_header(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

    /// 列间分隔条：1px 分隔线 + 5px 可拖拽热区
    fn splitter(&self, which: u8, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self.resizing == Some(which);
        let (line, hover) = (t.border, t.accent.opacity(0.5));
        div()
            .id(SharedString::from(format!("split-{which}")))
            .flex_none()
            .w(px(5.))
            .h_full()
            .flex()
            .justify_center()
            .cursor(CursorStyle::ResizeLeftRight)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |a, _, _, cx| {
                    a.resizing = Some(which);
                    cx.notify();
                }),
            )
            .child(div().w(px(1.)).h_full().bg(if on { hover } else { line }))
            .hover(move |s| s.bg(hover.opacity(0.15)))
    }

    fn render_sidebar(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
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

    fn render_list(&mut self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
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
                            cx.notify();
                        })))
                    })
                    .when(!drafts, |d| {
                        d.child(
                            button("list-export", t, Kind::Ghost, Some("download"), "", true)
                                .on_click(cx.listener(|a, ev: &ClickEvent, _, cx| a.toggle_pop(Pop::Export, ev.position(), cx))),
                        )
                    }),
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
    fn draft_row(&self, t: &Theme, d: Draft, emails: &HashMap<i64, String>, cx: &mut Context<Self>) -> Stateful<Div> {
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

    fn render_row(&mut self, t: &Theme, ix: usize, unified: bool, cx: &mut Context<Self>) -> Stateful<Div> {
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
            .child(div().absolute().left(px(10.)).top(px(13.)).child(checkbox(("chk", id as u64), t, checked).on_click(cx.listener(move |a, _, _, cx| {
                cx.stop_propagation();
                if !a.checked.remove(&id) {
                    a.checked.insert(id);
                }
                cx.notify();
            }))))
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
            .on_click(cx.listener(move |a, _, window, cx| {
                window.focus(&a.focus);
                a.reader_collapsed = false;
                a.open_message(id, cx)
            }))
    }

    fn render_reader(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let pane = div().flex().flex_col().flex_1().min_w_0().h_full().bg(t.bg);
        let d = match (&self.detail_state, self.detail.clone()) {
            (DetailState::Ready, Some(d)) => d,
            (state, _) => {
                webview::hide();
                let bar = div().flex().flex_none().justify_end().px(px(14.)).py(px(10.)).child(
                    button("toggle-reader", t, Kind::Ghost, Some("panel-right"), "", true).on_click(cx.listener(|a, _, _, cx| {
                        a.reader_collapsed = true;
                        cx.notify();
                    })),
                );
                return pane.child(bar).child(self.render_empty(t, state.clone(), cx)).into_any_element();
            }
        };
        let id = d.id;
        let local = d.account_local;
        let junk = d.folder_role == "junk";
        let has_archive = !local && d.folder_role != "archive" && self.has_role(d.account_id, "archive");
        let files: Vec<_> = d.attachments.iter().filter(|a| !a.inline).cloned().collect();
        let show_html = !d.html.is_empty() && !self.text_mode;
        let remote = self.remote_ok.contains(&id);
        let has_remote = match self.remote_cache {
            Some((cid, len, v)) if cid == id && len == d.html.len() => v,
            _ => {
                let v = webview::has_remote_content(&d.html);
                self.remote_cache = Some((id, d.html.len(), v));
                v
            }
        };
        let blocked = show_html && !remote && has_remote;
        let purge = d.folder_role == "trash" || local;

        // 文档缓存：换信 / 换模式 / 放行图片 / 换主题才重建
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (id, show_html, remote, t.id, d.html.len(), d.text.len()).hash(&mut h);
            h.finish()
        };
        if self.doc.as_ref().map(|x| x.0) != Some(key) {
            let doc = if show_html {
                webview::mail_document(&d.html, remote)
            } else {
                webview::text_document(&d.text, &hex_css(t.hex_ink), &hex_css(t.hex_paper), &hex_css(t.hex_muted), &hex_css(t.hex_accent))
            };
            self.doc = Some((key, doc.into()));
        }
        let doc = self.doc.as_ref().map(|x| x.1.clone()).unwrap_or_else(|| Arc::from(""));
        let frozen_snap = self.snap.clone().filter(|_| self.overlay_open());

        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .px(px(22.))
            .h(px(52.))
            .border_b_1()
            .border_color(t.border_soft)
            .child(button("t-reply", t, Kind::Secondary, Some("reply"), "回复", true).on_click(cx.listener(|a, _, w, cx| a.reply("reply", w, cx))))
            .child(button("t-replyall", t, Kind::Secondary, Some("replyAll"), "全部回复", true).on_click(cx.listener(|a, _, w, cx| a.reply("replyAll", w, cx))))
            .child(button("t-forward", t, Kind::Secondary, Some("forward"), "转发", true).on_click(cx.listener(|a, _, w, cx| a.reply("forward", w, cx))))
            .child(div().w(px(6.)))
            .child(
                button("t-star", t, Kind::Secondary, Some(if d.flagged { "star-fill" } else { "star" }), "", true)
                    .text_color(if d.flagged { t.gold } else { t.ink_2 })
                    .on_click(cx.listener(move |a, _, _, cx| {
                        let f = a.detail.as_ref().is_some_and(|d| d.flagged);
                        a.set_flagged(vec![id], !f, false, cx)
                    })),
            )
            .child(button("t-seen", t, Kind::Secondary, Some(if d.seen { "unread" } else { "read" }), "", true).on_click(cx.listener(move |a, _, _, cx| {
                let s = a.detail.as_ref().is_some_and(|d| d.seen);
                a.set_seen(vec![id], !s, cx)
            })))
            .child(
                button("t-move", t, Kind::Secondary, Some("move"), "", true)
                    .on_click(cx.listener(move |a, ev: &ClickEvent, _, cx| a.toggle_pop(Pop::Move(vec![id]), ev.position(), cx))),
            )
            .when(has_archive, |x| {
                x.child(
                    button("t-archive", t, Kind::Secondary, Some("archive"), "", true)
                        .on_click(cx.listener(move |a, _, _, cx| a.to_role(vec![id], "archive", "已归档", cx))),
                )
            })
            .when(!local, |x| {
                x.child(button("t-spam", t, Kind::Secondary, Some(if junk { "inbox" } else { "spam" }), "", true).on_click(cx.listener(move |a, _, _, cx| {
                    if junk {
                        a.to_role(vec![id], "inbox", "已移回收件箱", cx)
                    } else {
                        a.to_role(vec![id], "junk", "已标为垃圾邮件", cx)
                    }
                })))
            })
            .child({
                let subj = d.subject.clone();
                button("t-eml", t, Kind::Secondary, Some("download"), "", true).on_click(cx.listener(move |a, _, _, cx| a.save_eml(id, &subj, cx)))
            })
            .child(div().flex_1())
            .child(button("toggle-reader", t, Kind::Ghost, Some("panel-right"), "", true).on_click(cx.listener(|a, _, _, cx| {
                a.reader_collapsed = true;
                cx.notify();
            })))
            .child(
                button("t-delete", t, Kind::Danger, Some("trash"), if purge { "彻底删除" } else { "删除" }, true)
                    .on_click(cx.listener(move |a, _, w, cx| a.delete(vec![id], w, cx))),
            );

        let meta_row = |k: &'static str, v: AnyElement| {
            div().flex().gap(px(8.)).child(div().w(px(52.)).flex_none().text_color(t.muted).child(k)).child(div().flex_1().min_w_0().child(v))
        };
        let mono = |s: String| div().font_family(MONO).text_size(px(11.5)).child(s).into_any_element();
        let header = div()
            .flex_none()
            .px(px(36.))
            .pt(px(24.))
            .child(
                div()
                    .font_family(SERIF)
                    .text_size(px(24.))
                    .font_weight(FontWeight::BOLD)
                    .line_height(px(32.))
                    .text_color(t.ink)
                    .child(if d.subject.is_empty() { "(无主题)".to_string() } else { d.subject.clone() }),
            )
            .child(
                div()
                    .mt(px(12.))
                    .text_size(px(12.5))
                    .line_height(px(23.))
                    .text_color(t.ink_2)
                    .child(meta_row(
                        "发件人",
                        div().flex().gap(px(6.)).child(d.from.name.clone()).child(mono(format!("<{}>", d.from.address))).into_any_element(),
                    ))
                    .child(meta_row("收件人", mono(if d.to.is_empty() { "—".into() } else { addrs_text(&d.to) })))
                    .when(!d.cc.is_empty(), |x| x.child(meta_row("抄送", mono(addrs_text(&d.cc)))))
                    .when(!d.reply_to.is_empty() && d.reply_to != d.from.address, |x| x.child(meta_row("回复至", mono(d.reply_to.clone()))))
                    .child(meta_row("时间", div().child(fmt_full(d.date)).into_any_element()))
                    .child(meta_row(
                        "位置",
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(dot(t.named(&d.account_color), 7.))
                            .child(format!("{} / {}", if local { "本地归档".to_string() } else { d.account_email.clone() }, d.folder_label))
                            .into_any_element(),
                    )),
            )
            .child(div().h(px(1.)).bg(t.border_soft).mt(px(14.)))
            .when(!d.html.is_empty(), |x| {
                x.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .mt(px(10.))
                        .px(px(12.))
                        .h(px(36.))
                        .rounded(px(8.))
                        .when(blocked, |x| x.bg(t.panel_2))
                        .text_size(px(12.))
                        .text_color(t.ink_2)
                        .when(blocked, |x| x.child(icon("image", 13., t.muted)).child("为保护隐私，已阻止加载远程图片与追踪像素。"))
                        .child(div().flex_1())
                        .when(blocked, |x| {
                            x.child(button("show-images", t, Kind::Secondary, None, "显示图片", true).on_click(cx.listener(move |a, _, _, cx| {
                                a.remote_ok.insert(id);
                                cx.notify();
                            })))
                        })
                        .child(button("toggle-mode", t, Kind::Ghost, None, if self.text_mode { "原始排版" } else { "纯文本" }, true).on_click(cx.listener(
                            |a, _, _, cx| {
                                a.text_mode = !a.text_mode;
                                cx.notify();
                            },
                        ))),
                )
            });

        let body = div()
            .id("mail-body")
            .flex_1()
            .min_h(px(120.))
            .mx(px(if show_html { 24. } else { 32. }))
            .mt(px(10.))
            .relative()
            .rounded(px(6.))
            .overflow_hidden()
            .child(
                canvas(
                    |b, _, _| b,
                    move |b, _, window, _| {
                        webview::show(window, b, key, &doc);
                    },
                )
                .size_full(),
            )
            .when_some(frozen_snap, |x, s| x.child(img(s).absolute().top_0().left_0().size_full()));

        let mut footer = div().flex_none().px(px(36.)).pb(px(16.)).pt(px(10.)).flex().flex_col().gap(px(10.));
        if !files.is_empty() {
            let mut row = div().flex().flex_wrap().gap(px(8.));
            for a in files.iter() {
                let (ix, name, size) = (a.index, a.filename.clone(), a.size);
                row = row.child(
                    button(SharedString::from(format!("att-{ix}")), t, Kind::Secondary, Some("clip"), one_line(&a.filename, 36), true)
                        .child(div().text_size(px(10.5)).text_color(t.muted).child(fmt_size(a.size)))
                        .on_click(cx.listener(move |app, _, _, cx| app.open_preview(id, ix, name.clone(), size, cx))),
                );
            }
            footer = footer.child(div().text_size(px(11.)).text_color(t.muted).child(format!("附  件 · {}", files.len()))).child(row);
        }
        if d.thread.len() > 1 {
            let mut list = div().id("thread").flex().flex_col().max_h(px(130.)).overflow_y_scroll();
            for th in d.thread.iter() {
                let tid = th.id;
                let cur = tid == id;
                list = list.child(
                    div()
                        .id(("th", tid as u64))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .h(px(28.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .text_size(px(12.5))
                        .cursor_pointer()
                        .when(cur, |x| x.bg(t.accent_soft))
                        .hover(|s| s.bg(t.row_hover))
                        .child(icon("mail", 13., t.muted))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(format!(
                            "{} · {}",
                            th.from.short(),
                            th.subject
                        )))
                        .child(div().text_size(px(11.)).text_color(t.muted).child(format!("{} · {}", th.folder, fmt_full(th.date))))
                        .on_click(cx.listener(move |a, _, _, cx| {
                            if !cur {
                                a.open_message(tid, cx)
                            }
                        })),
                );
            }
            footer = footer.child(div().text_size(px(11.)).text_color(t.muted).child(format!("同一会话 · {} 封", d.thread.len()))).child(list);
        }

        let card = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .max_w(px(880.))
            .mx_auto()
            .my(px(18.))
            .bg(t.paper)
            .border_1()
            .border_color(t.border)
            .rounded(px(12.))
            .overflow_hidden()
            .child(header)
            .child(body)
            .child(footer);

        pane.child(toolbar).child(div().flex().flex_1().min_h_0().px(px(22.)).child(card)).into_any_element()
    }

    fn render_empty(&mut self, t: &Theme, state: DetailState, cx: &mut Context<Self>) -> impl IntoElement {
        let has_accounts = self.accounts.iter().any(|a| !a.is_local());
        let base = div().flex().flex_col().flex_1().items_center().justify_center().px(px(40.)).text_color(t.ink);
        let envelope = || div().mb(px(22.)).child(icon("envelope", 54., t.line_2).with_transformation(Transformation::rotate(radians(-0.12))));
        match state {
            DetailState::Loading => base.child(envelope()).child(div().text_size(px(13.)).text_color(t.ink_2).child("正在取回这封信…")),
            DetailState::Error(e) => base.child(envelope()).child(div().text_size(px(13.)).text_color(t.danger).max_w(px(460.)).child(e)),
            _ if has_accounts || self.stats.archived > 0 => base
                .child(envelope())
                .child(div().font_family(SERIF).text_size(px(11.)).text_color(t.muted).mb(px(18.)).child("A  L E T T E R  F R O M  T H E  P A S T"))
                .child(div().font_family(SERIF).text_size(px(32.)).font_weight(FontWeight::BOLD).mb(px(22.)).child("把时间，慢慢读回来。"))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(t.ink_2)
                        .line_height(px(25.))
                        .text_center()
                        .child("选择一封邮件，开始阅读。")
                        .child("所有邮件都会增量存档到本机，断网也能读、能搜。"),
                )
                .child(div().w_full().max_w(px(540.)).h(px(1.)).bg(t.border).mt(px(38.)).mb(px(28.)))
                .child(
                    div()
                        .flex()
                        .gap(px(64.))
                        .child(stat(t, fmt_num(self.stats.archived), "封已存档"))
                        .child(stat(t, self.accounts.iter().filter(|a| !a.is_local()).count().to_string(), "个账号"))
                        .child(stat(t, fmt_num(self.stats.unread), "封未读")),
                ),
            _ => base
                .child(div().mb(px(22.)).child(logo(72.)))
                .child(div().font_family(SERIF).text_size(px(11.)).text_color(t.muted).mb(px(18.)).child("E A S Y M A I L  ·  迷 你 邮 件"))
                .child(div().font_family(SERIF).text_size(px(32.)).font_weight(FontWeight::BOLD).mb(px(22.)).child("先接入一个邮箱。"))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(t.ink_2)
                        .line_height(px(25.))
                        .text_center()
                        .child("支持 Gmail（应用专用密码 + 代理）、网易 163、QQ 邮箱（授权码）与 AgentMail。")
                        .child("凭据 AES-256-GCM 加密保存在本机。"),
                )
                .child(
                    button("empty-add", t, Kind::Primary, Some("plus"), "添加邮箱账号", false)
                        .mt(px(26.))
                        .on_click(cx.listener(|a, _, w, cx| a.open_accounts(Tab::Accounts, true, w, cx))),
                ),
        }
    }

    fn render_pop(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (pop, at) = self.pop.clone()?;
        let mut p = popover(t, 230.).id("popover").occlude().max_h(px(f32::from(window.viewport_size().height) * 0.75)).overflow_y_scroll();
        match pop {
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
                p = p.w(px(250.)).child(menu_title(t, if n > 0 { format!("导出选中的 {n} 封") } else { format!("导出当前列表 {} 封", fmt_num(self.total)) }));
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
                } else {
                    p = p
                        .child(menu_sep(t))
                        .child(item("fm-new", "plus", "新建子文件夹…".into(), "new-sub", cx))
                        .child(item("fm-rename", "edit", "重命名…".into(), "rename", cx))
                        .child(item("fm-delete", "trash", "删除文件夹…".into(), "delete", cx).text_color(t.danger));
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
                        );
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

    fn render_modal(&mut self, t: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(modal) = self.modal.as_ref() else {
            webview::hide_quicklook();
            crate::editor::hide();
            return None;
        };
        if !matches!(modal, Modal::Composer(_)) {
            crate::editor::hide();
        }
        // 附件预览（弹窗，或写信窗口里的原地预览）之外不保留原生预览视图
        let composer_preview = matches!(modal, Modal::Composer(c) if c.read(cx).has_preview());
        if !matches!(modal, Modal::Preview { .. }) && !composer_preview {
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
            Modal::Composer(c) => (c.clone().into_any_element(), false),
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

/// 附件预览的临时文件目录（启动时清空）
pub fn preview_dir() -> std::path::PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("EasyMail").join("preview")
}

fn stat(t: &Theme, v: String, k: &'static str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .child(div().font_family(SERIF).text_size(px(30.)).font_weight(FontWeight::SEMIBOLD).text_color(t.ink).child(v))
        .child(div().text_size(px(11.5)).text_color(t.muted).mt(px(6.)).child(k))
}

fn empty_list(t: &Theme, s: &str) -> impl IntoElement {
    let mut d = div().py(px(60.)).px(px(20.)).flex().flex_col().items_center().text_color(t.muted).text_size(px(13.)).font_family(SERIF).line_height(px(25.));
    for l in s.split('\n') {
        d = d.child(l.to_string());
    }
    d
}

impl Render for MailApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        self.sync_freeze(cx);
        let header = self.render_header(&t, window, cx);
        let sidebar = self.render_sidebar(&t, cx);
        let list = self.render_list(&t, cx);
        let reader = if self.reader_collapsed {
            webview::hide();
            None
        } else {
            Some(self.render_reader(&t, cx))
        };
        let split_r = self.splitter(1, &t, cx);
        let pop = self.render_pop(&t, window, cx);
        let modal = self.render_modal(&t, window, cx);
        // 阅读区是原生 WebView，会盖住 GPUI 画的内容：toast 放在列表栏底部，不与 WebView 重叠
        let (toast_left, toast_w) = (self.sidebar_w + 5. + 14., if self.reader_collapsed { 560. } else { (self.list_w - 28.).max(200.) });
        // 有弹窗时（写信等，此时阅读区已冻结成快照）toast 必须在弹窗之上：居中显示在窗口底部
        let on_top = self.modal.is_some();
        let mut toasts = if on_top {
            div().absolute().bottom(px(22.)).left_0().w_full().flex().flex_col().items_center().gap(px(8.))
        } else {
            div().absolute().bottom(px(22.)).left(px(toast_left)).w(px(toast_w)).flex().flex_col().items_start().gap(px(8.))
        };
        let toast_w = if on_top { 560. } else { toast_w };
        for tt in &self.toasts {
            toasts = toasts.child(
                div()
                    .max_w(px(toast_w))
                    .px(px(16.))
                    .py(px(9.))
                    .rounded(px(9.))
                    .bg(if tt.error { t.danger } else { t.toast_bg })
                    .text_color(if tt.error { gpui::white() } else { t.toast_fg })
                    .text_size(px(12.5))
                    .shadow(vec![BoxShadow { color: t.shadow, offset: point(px(0.), px(6.)), blur_radius: px(20.), spread_radius: px(0.) }])
                    .child(tt.msg.clone()),
            );
        }
        let (toasts_below, toasts_above) = if on_top { (None, Some(toasts)) } else { (Some(toasts), None) };
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.ink)
            .font_family(SANS)
            .key_context("MailApp")
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(|a, _: &Compose, w, cx| {
                if a.modal.is_none() {
                    a.compose_new(w, cx)
                }
            }))
            .on_action(cx.listener(|a, _: &SyncAll, _, cx| a.sync_now(cx)))
            .on_action(cx.listener(|a, _: &FocusSearch, w, cx| {
                if a.modal.is_none() {
                    w.focus(&a.search.focus_handle(cx));
                    a.search.update(cx, |s, cx| {
                        let t = s.text();
                        s.set_text(&t, cx)
                    });
                }
            }))
            .on_action(cx.listener(|a, _: &OpenSettings, w, cx| a.open_accounts(Tab::Accounts, false, w, cx)))
            .on_action(cx.listener(|a, _: &OpenImport, w, cx| a.open_import(w, cx)))
            .on_action(cx.listener(|a, _: &CycleTheme, w, cx| a.cycle_theme(w, cx)))
            .child(header)
            .child(
                div()
                    .id("main")
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .key_context("MailList")
                    .track_focus(&self.focus)
                    .on_action(cx.listener(|a, _: &NextMail, _, cx| a.go(1, cx)))
                    .on_action(cx.listener(|a, _: &PrevMail, _, cx| a.go(-1, cx)))
                    .on_action(cx.listener(|a, _: &Reply, w, cx| a.reply("reply", w, cx)))
                    .on_action(cx.listener(|a, _: &ReplyAll, w, cx| a.reply("replyAll", w, cx)))
                    .on_action(cx.listener(|a, _: &Forward, w, cx| a.reply("forward", w, cx)))
                    .on_action(cx.listener(|a, _: &ToggleStar, _, cx| {
                        if let Some(d) = a.detail.as_ref().filter(|_| a.detail_state == DetailState::Ready) {
                            let (id, f) = (d.id, d.flagged);
                            a.set_flagged(vec![id], !f, false, cx)
                        }
                    }))
                    .on_action(cx.listener(|a, _: &ToggleSeen, _, cx| {
                        if let Some(d) = a.detail.as_ref().filter(|_| a.detail_state == DetailState::Ready) {
                            let (id, s) = (d.id, d.seen);
                            a.set_seen(vec![id], !s, cx)
                        }
                    }))
                    .on_action(cx.listener(|a, _: &Archive, _, cx| {
                        if let Some(d) = a.detail.as_ref().filter(|_| a.detail_state == DetailState::Ready) {
                            if !d.account_local && d.folder_role != "archive" && a.has_role(d.account_id, "archive") {
                                let id = d.id;
                                a.to_role(vec![id], "archive", "已归档", cx)
                            }
                        }
                    }))
                    .on_action(cx.listener(|a, _: &DeleteMail, w, cx| {
                        if a.modal.is_none() {
                            let ids = a.current_ids();
                            a.delete(ids, w, cx)
                        }
                    }))
                    .on_mouse_move(cx.listener(|a, ev: &MouseMoveEvent, _, cx| {
                        let Some(which) = a.resizing else { return };
                        if ev.pressed_button != Some(MouseButton::Left) {
                            a.resizing = None;
                            return;
                        }
                        let x: f32 = ev.position.x.into();
                        if which == 0 {
                            a.sidebar_w = x.clamp(160., 420.);
                        } else {
                            a.list_w = (x - a.sidebar_w).clamp(280., 720.);
                        }
                        cx.notify();
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(|a, _, _, _| a.resizing = None))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(|a, _, _, _| a.resizing = None))
                    .child(sidebar)
                    .child(self.splitter(0, &t, cx))
                    .child(list)
                    .children(reader.map(|r| div().flex().flex_1().min_w_0().h_full().child(split_r).child(r))),
            )
            .children(toasts_below)
            .children(modal)
            .children(pop)
            .children(toasts_above)
    }
}

/// 侧栏 / 角标要的元数据，一次在后台查齐
struct Meta {
    accounts: Vec<Account>,
    folders: Vec<Folder>,
    stats: Stats,
    inbox_unread: i64,
}

impl Meta {
    fn fetch(s: &Store) -> Meta {
        Meta {
            accounts: accounts::list(s).unwrap_or_default(),
            folders: queries::folders(s).unwrap_or_default(),
            stats: queries::stats(s).unwrap_or_default(),
            inbox_unread: queries::inbox_unread(s),
        }
    }
}
