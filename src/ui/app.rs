//! 主窗口：顶栏 + 侧栏 + 邮件列表 + 阅读区，外加弹层、模态与提示条。

use super::accounts::{AccountsEvent, AccountsModal, Tab};
use super::composer::{build_reply, Composer, ComposerEvent};
use super::import::{ImportEvent, ImportModal};
use super::line_input::{InputEvent, LineInput};
use super::theme::{hex_css, palette, theme, Theme, ThemePref};
use super::widgets::*;
use super::*;
use crate::core::calendar;
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

mod list;
mod modal;
pub(crate) mod panels;
mod pop;
pub(crate) mod reader;
mod sidebar;
use sidebar::{Sidebar, SidebarEvent};

const PAGE: usize = 60;
const ROW_H: f32 = 84.;

pub const VIEWS: [View; 9] = [View::Inbox, View::Unread, View::Starred, View::Snoozed, View::Attachments, View::Invoices, View::Sent, View::Drafts, View::All];

pub fn view_label(v: View) -> &'static str {
    match v {
        View::Inbox => "统一收件箱",
        View::Unread => "未读邮件",
        View::Starred => "星标邮件",
        View::Attachments => "包含附件",
        View::Sent => "已发送",
        View::Drafts => "草稿",
        View::All => "全部邮件",
        View::Snoozed => "稍后处理",
        View::Invoices => "发票",
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
        View::Snoozed => "clock",
        View::Invoices => "receipt",
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
    /// 邮件行右键菜单
    Row(i64),
    /// 稍后处理的时间选择
    Snooze(Vec<i64>),
    /// 工具菜单（备份、瘦身、发件人、隐私报告…）
    Tools,
    Theme,
    Export,
    Move(Vec<i64>),
    Folder(i64),
    Account(i64),
}

/// 从列表拖出去的邮件（拖到侧栏文件夹上移动）
#[derive(Clone)]
pub struct DragMails {
    pub ids: Vec<i64>,
    pub label: String,
}

/// 拖拽时跟着鼠标的小标签
pub struct DragGhost(pub String);

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        div()
            .px(px(10.))
            .py(px(6.))
            .rounded(px(8.))
            .bg(t.accent)
            .text_color(t.paper)
            .text_size(px(12.))
            .shadow(vec![BoxShadow { color: t.shadow, offset: point(px(0.), px(4.)), blur_radius: px(12.), spread_radius: px(0.) }])
            .child(self.0.clone())
    }
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
    /// 工具面板（发件人视图、隐私报告、发件箱、规则…）
    Panel(panels::Panel),
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
    pub action: Option<ToastAction>,
}

/// 提示条上的按钮
#[derive(Clone, Debug, PartialEq)]
pub enum ToastAction {
    /// 撤销最近一次移动 / 删除（⌘Z 同效）
    Undo,
    /// 撤回发件箱里还没发出的信
    Unsend(i64),
    /// 在浏览器中打开链接（新版本下载页等）
    Open(String),
    /// 复制文字（验证码）
    Copy(String),
    /// 打开「发现新版本」面板（可一键更新）
    Update(crate::core::tools::Release),
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
    /// Shift 范围选择的起点（上一次点选的邮件）
    pub anchor: Option<i64>,
    /// 当前打开的邮件。用 Rc：每帧渲染只复制指针，不复制可能几 MB 的 HTML
    pub detail: Option<Rc<Detail>>,
    /// (邮件 id, HTML 长度, 是否引用了远程资源)：正则只在换信时跑一次，不是每帧
    remote_cache: Option<(i64, usize, bool)>,
    /// 阅读区的提示信息（验证码、行程、认证结果、钓鱼链接…），换信时算一次
    extras: Option<(i64, usize, Rc<reader::Extras>)>,
    /// 列表按会话合并（kv `threads`）
    pub threads: bool,
    /// load_meta 的请求代号：只应用最新一次后台查询的结果
    meta_gen: u64,
    pub detail_state: DetailState,
    pub remote_ok: HashSet<i64>,
    pub text_mode: bool,
    sidebar: Entity<Sidebar>,
    /// 阅读区会话时间线的滚动句柄，以及已经为哪一封滚动过（换信时把当前这封滚进可视区）
    thread_scroll: ScrollHandle,
    thread_scrolled: Option<i64>,
    /// 命令面板列表的滚动句柄（↑↓ 选择时让选中项保持可见）
    palette_scroll: ScrollHandle,
    pub sync: HashMap<i64, SyncState>,
    pub toasts: Vec<Toast>,
    toast_seq: u64,
    pub pop: Option<(Pop, Point<Pixels>)>,
    pop_closed_at: Instant,
    sidebar_w: f32,
    /// 上次看到的窗口位置 / 大小，以及是否已有一个防抖保存在排队
    win_seen: Option<WindowBounds>,
    win_save_pending: bool,
    list_w: f32,
    /// 正在拖拽的分隔条：0 = 侧栏 | 列表，1 = 列表 | 阅读区
    resizing: Option<u8>,
    /// 阅读区折叠：列表占满右侧
    reader_collapsed: bool,
    pub modal: Option<Modal>,
    freeze: super::freeze::Freeze,
    doc: Option<(u64, Arc<str>)>,
    list_scroll: UniformListScrollHandle,
    refresh_task: Option<Task<()>>,
    /// 最近一次收到新邮件提醒（测试用）
    pub last_newmail: Option<String>,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
    /// 当前模态的订阅：同一时间只有一个模态，换掉时旧订阅随之释放（以前一直往 _subs 里追加、从不清理）
    modal_sub: Option<Subscription>,
    /// 当前打开的邮件里的日历邀请：（邮件 id，解析结果，原始 ics）
    pub invite: Option<(i64, calendar::Invite, String)>,
    /// 深色主题下这些邮件按原样显示（不反色）
    mail_light: HashSet<i64>,
}

impl MailApp {
    pub fn new(eng: Engine, rx: futures::channel::mpsc::UnboundedReceiver<Evt>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = eng.store().clone();
        let threads = store.get_kv("threads").as_deref() == Some("1");
        // 上次的分栏宽度 / 阅读区折叠状态（kv `layout` = "sidebar,list,collapsed"）
        let layout = store
            .get_kv("layout")
            .and_then(|s| {
                let v: Vec<&str> = s.split(',').collect();
                Some((v.first()?.parse::<f32>().ok()?.clamp(160., 420.), v.get(1)?.parse::<f32>().ok()?.clamp(280., 720.), v.get(2) == Some(&"1")))
            })
            .unwrap_or((220., 380., false));
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
        // 侧栏是独立 Entity：它只发事件，这里统一处理
        let me = cx.entity();
        let sidebar = cx.new(|scx| Sidebar::new(&me, store.clone(), scx));
        subs.push(cx.subscribe_in(&sidebar, window, |this: &mut Self, _, ev: &SidebarEvent, window, cx| match ev {
            SidebarEvent::View(v) => {
                if !this.q.is_empty() {
                    this.q.clear();
                    this.search.update(cx, |s, cx| s.set_text("", cx));
                }
                this.set_nav(Nav::View(*v), cx);
                window.focus(&this.focus);
            }
            SidebarEvent::Saved(q) => {
                this.search.update(cx, |s, cx| s.set_text(q, cx));
                this.set_query(q.clone(), cx);
                window.focus(&this.focus);
            }
            SidebarEvent::Folder(id) => {
                this.set_nav(Nav::Folder(*id), cx);
                window.focus(&this.focus);
            }
            SidebarEvent::AccountMenu(id, at) => this.toggle_pop(Pop::Account(*id), *at, cx),
            SidebarEvent::FolderMenu(id, at) => this.toggle_pop(Pop::Folder(*id), *at, cx),
            SidebarEvent::Sync(id) => {
                this.eng.request_sync(*id, None, true);
                this.toast("已开始同步", false, cx);
            }
            SidebarEvent::DropMails(ids, fid) => this.drop_mails(ids.clone(), *fid, cx),
            SidebarEvent::Import => this.open_import(window, cx),
            SidebarEvent::Settings => this.open_accounts(Tab::Accounts, false, window, cx),
        }));
        subs.push(cx.observe_window_appearance(window, |this: &mut Self, window, cx| {
            if this.pref.pref == "auto" {
                this.apply_theme(window.appearance(), cx);
            }
        }));
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
            anchor: None,
            detail: None,
            remote_cache: None,
            extras: None,
            threads,
            meta_gen: 0,
            detail_state: DetailState::Idle,
            remote_ok: HashSet::new(),
            text_mode: false,
            sidebar,
            thread_scroll: ScrollHandle::new(),
            thread_scrolled: None,
            palette_scroll: ScrollHandle::new(),
            sync: HashMap::new(),
            toasts: vec![],
            toast_seq: 0,
            pop: None,
            pop_closed_at: Instant::now(),
            sidebar_w: layout.0,
            win_seen: None,
            win_save_pending: false,
            list_w: layout.1,
            resizing: None,
            reader_collapsed: layout.2,
            modal: None,
            freeze: Default::default(),
            doc: None,
            list_scroll: UniformListScrollHandle::new(),
            refresh_task: None,
            last_newmail: None,
            _tasks: tasks,
            _subs: subs,
            modal_sub: None,
            mail_light: HashSet::new(),
            invite: None,
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
        p.threads = self.threads && !self.is_drafts();
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
        // 加载更早的一页不重新计数（大库上 COUNT 是列表里最慢的一步）
        let task = cx.background_spawn(async move { queries::list_page(&store, &p, cursor, limit, !append) });
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
                        if page.total >= 0 {
                            a.total = page.total;
                        }
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
        }
    }

    pub fn toast(&mut self, msg: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        self.toast_with(msg, error, None, cx);
    }

    /// 带按钮的提示条：撤销 / 撤回类的提示停留到撤销窗口结束
    pub fn toast_with(&mut self, msg: impl Into<String>, error: bool, action: Option<ToastAction>, cx: &mut Context<Self>) {
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

    /// 同 run_op，成功后的提示带「撤销」按钮（撤销窗口内有效）
    fn run_op_undo<T: Send + 'static>(
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

    /// ⌘Z / 提示条「撤销」
    pub fn undo(&mut self, cx: &mut Context<Self>) {
        self.toasts.retain(|t| t.action != Some(ToastAction::Undo));
        let eng = self.eng.clone();
        self.run(crate::core::ops::undo_last(eng), cx, |a, r, cx| {
            match r {
                Ok((label, _)) => a.toast(format!("已撤销「{label}」"), false, cx),
                Err(e) => a.toast(e.to_string(), true, cx),
            }
            a.load_meta(cx);
            a.load_list(true, false, cx);
        });
    }

    /// 撤回发件箱里的信：信回到草稿，重新打开写信窗口
    pub fn unsend(&mut self, outbox_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.toasts.retain(|t| t.action != Some(ToastAction::Unsend(outbox_id)));
        match crate::core::outbox::cancel(&self.store, outbox_id) {
            Ok(draft) => {
                self.toast("已撤回，信已放回草稿", false, cx);
                if let Some(d) = draft.and_then(|id| mailer::get_draft(&self.store, id).ok().flatten()) {
                    self.open_composer(d, window, cx);
                }
            }
            Err(e) => self.toast(e.to_string(), true, cx),
        }
        self.load_meta(cx);
    }

    fn end_resize(&mut self) {
        if self.resizing.take().is_some() {
            self.save_layout();
        }
    }

    /// 窗口移动 / 缩放都会触发重绘：在渲染时比较一下，变了就防抖保存（GPUI 没有 bounds 变化回调）
    fn track_window(&mut self, window: &Window, cx: &mut Context<Self>) {
        let now = window.window_bounds();
        if self.win_seen == Some(now) {
            return;
        }
        self.win_seen = Some(now);
        if self.win_save_pending {
            return;
        }
        self.win_save_pending = true;
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(Duration::from_millis(600)).await;
            this.update(cx, |a, _| {
                a.win_save_pending = false;
                if let Some(wb) = a.win_seen {
                    super::window_state::save(&a.store, wb);
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn save_layout(&self) {
        self.store.set_kv("layout", &format!("{:.0},{:.0},{}", self.sidebar_w, self.list_w, if self.reader_collapsed { 1 } else { 0 }));
    }

    /// 启动后的一次性检查：上次是否异常退出、有没有新版本
    pub fn startup_checks(&mut self, cx: &mut Context<Self>) {
        let log = self.store.dir().join("panic.log");
        if let Ok(mtime) = std::fs::metadata(&log).and_then(|m| m.modified()) {
            let at = mtime.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
            let seen: i64 = self.store.get_kv("panic_seen_at").and_then(|v| v.parse().ok()).unwrap_or(0);
            if at > seen {
                self.store.set_kv("panic_seen_at", &at.to_string());
                if seen > 0 || at > crate::core::db::now_ms() - 7 * 86_400_000 {
                    self.toast_with(
                        "上次运行时程序异常退出了，详细信息记录在数据目录的 panic.log，欢迎附上它到 GitHub 提 issue",
                        true,
                        Some(ToastAction::Open(format!("file://{}", log.display()))),
                        cx,
                    );
                }
            }
        }
        self.check_update(false, cx);
    }

    /// 当前搜索加入 / 移出「保存的搜索」（显示在侧栏）
    pub fn toggle_saved_search(&mut self, cx: &mut Context<Self>) {
        let q = self.q.trim().to_string();
        if q.is_empty() {
            return;
        }
        let mut v = panels::load_saved(&self.store);
        if let Some(i) = v.iter().position(|x| *x == q) {
            v.remove(i);
            self.toast("已从保存的搜索中移除", false, cx);
        } else {
            v.push(q);
            self.toast("已保存到侧栏", false, cx);
        }
        panels::save_saved(&self.store, &v);
        cx.notify();
    }

    fn toast_click(&mut self, action: ToastAction, window: &mut Window, cx: &mut Context<Self>) {
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
        self.invite = None;
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
                        a.invite = messages::read_local_source(&a.store, id)
                            .and_then(|raw| crate::core::parse::calendar_part(&raw))
                            .and_then(|ics| calendar::parse(&ics).map(|i| (id, i, ics)));
                        a.detail = Some(Rc::new(d));
                        a.detail_state = DetailState::Ready;
                        if unseen {
                            let secs: u64 = a.store.get_kv("mark_read_secs").and_then(|v| v.parse().ok()).unwrap_or(0);
                            if secs == 0 {
                                a.mark_opened_read(id, cx);
                            } else {
                                // 停留够久才标已读：期间换了别的邮件就不标
                                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                                    cx.background_executor().timer(Duration::from_secs(secs)).await;
                                    this.update(cx, |a, cx| {
                                        if a.selected == Some(id) && a.detail.as_ref().is_some_and(|d| d.id == id && !d.seen) {
                                            a.mark_opened_read(id, cx);
                                        }
                                    })
                                    .ok();
                                })
                                .detach();
                            }
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

    /// 打开的邮件标为已读（本地立即生效，远端走持久化队列）
    fn mark_opened_read(&mut self, id: i64, cx: &mut Context<Self>) {
        self.patch(&[id], Some(true), None);
        let eng = self.eng.clone();
        self.run(ops::set_flags(eng, vec![id], ops::FlagChange { seen: Some(true), flagged: None }), cx, |a, r, cx| {
            if let Err(e) = r {
                a.toast(e.to_string(), true, cx);
            }
            a.load_meta(cx);
        });
        cx.notify();
    }

    /// 在独立窗口里打开邮件（并排对照）
    pub fn open_in_window(&mut self, id: i64, cx: &mut Context<Self>) {
        if let Err(e) = crate::ui::reader_window::open(&self.store, id, cx) {
            self.toast(format!("{e:#}"), true, cx);
        }
    }

    /// 回复日历邀请
    pub fn rsvp(&mut self, id: i64, r: calendar::Rsvp, cx: &mut Context<Self>) {
        self.toast("正在发送回复…", false, cx);
        let eng = self.eng.clone();
        self.run(calendar::respond(eng, id, r), cx, move |a, res, cx| match res {
            Ok(()) => {
                a.toast(format!("已回复组织者：{}", r.label()), false, cx);
                cx.notify();
            }
            Err(e) => a.toast(format!("回复失败：{e:#}"), true, cx),
        });
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
        // 会被彻底删除（无法恢复）的：本地归档、已在垃圾箱 / 垃圾邮件里的、所在账号没有垃圾箱的
        let local_ids: HashSet<i64> = self.accounts.iter().filter(|a| a.is_local()).map(|a| a.id).collect();
        let mut rows: Vec<(i64, i64, String)> =
            self.items.iter().filter(|m| ids.contains(&m.id)).map(|m| (m.id, m.account_id, m.folder_role.clone())).collect();
        if let Some(d) = self.detail.as_ref().filter(|d| ids.contains(&d.id) && !rows.iter().any(|r| r.0 == d.id)) {
            rows.push((d.id, d.account_id, d.folder_role.clone()));
        }
        let purge = rows.iter().filter(|(_, acc, role)| local_ids.contains(acc) || role == "trash" || role == "junk" || !self.has_role(*acc, "trash")).count();
        if purge > 0 {
            let all = purge == rows.len();
            let title = if all { format!("彻底删除 {purge} 封邮件？") } else { format!("其中 {purge} 封会被彻底删除") };
            let detail = "这些邮件已在垃圾箱 / 垃圾邮件里，或来自「本地归档」、所在邮箱没有垃圾箱，删除后无法恢复，也不能撤销。";
            let answer = window.prompt(PromptLevel::Warning, &title, Some(detail), &["彻底删除", "取消"], cx);
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
        self.run_op_undo(
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

    /// 把邮件拖到侧栏文件夹上：同一账号才能移动；已经在该文件夹里的忽略
    fn drop_mails(&mut self, ids: Vec<i64>, folder_id: i64, cx: &mut Context<Self>) {
        let Some(f) = self.folders.iter().find(|f| f.id == folder_id) else { return };
        let acc = f.account_id;
        let movable: Vec<i64> = self.items.iter().filter(|m| ids.contains(&m.id) && m.account_id == acc && m.folder_id != folder_id).map(|m| m.id).collect();
        if movable.is_empty() {
            let other = self.items.iter().any(|m| ids.contains(&m.id) && m.account_id != acc);
            if other {
                self.toast("不能跨账号拖动邮件（要复制到别的账号，用「账号间搬家」）", true, cx);
            }
            return;
        }
        self.move_to(movable, folder_id, cx);
    }

    /// 操作对象：右键 / 拖拽的邮件在已勾选集合里就是整批，否则只是它自己
    pub fn targets(&self, id: i64) -> Vec<i64> {
        if self.checked.contains(&id) {
            self.checked.iter().copied().collect()
        } else {
            vec![id]
        }
    }

    /// Shift 点选：把 anchor 到 id 之间（含）的邮件全部勾上
    pub fn select_range(&mut self, id: i64) {
        let pos = |x: i64| self.items.iter().position(|m| m.id == x);
        let (Some(a), Some(b)) = (self.anchor.and_then(pos), pos(id)) else {
            self.checked.insert(id);
            self.anchor = Some(id);
            return;
        };
        let (lo, hi) = (a.min(b), a.max(b));
        for m in &self.items[lo..=hi] {
            self.checked.insert(m.id);
        }
    }

    pub fn move_to(&mut self, ids: Vec<i64>, folder_id: i64, cx: &mut Context<Self>) {
        let label = self.folders.iter().find(|f| f.id == folder_id).map(|f| f.label()).unwrap_or_else(|| "文件夹".into());
        self.pop = None;
        self.remove_from_list(&ids, cx);
        let eng = self.eng.clone();
        self.run_op_undo(ops::move_messages(eng, ids, folder_id), move |_| format!("已移动到「{label}」"), cx);
    }

    pub fn to_role(&mut self, ids: Vec<i64>, role: &'static str, msg: &'static str, cx: &mut Context<Self>) {
        self.remove_from_list(&ids, cx);
        let eng = self.eng.clone();
        self.run_op_undo(async move { ops::move_to_role(eng, ids, role).await }, move |_| msg.into(), cx);
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
                ComposerEvent::Queued { outbox_id, send_at, scheduled } => {
                    this.modal = None;
                    let secs = (send_at - crate::core::db::now_ms()).max(0) / 1000;
                    if *scheduled {
                        this.toast_with(
                            format!("已定时：{} 发出（在「发件箱」里可撤回）", fmt_full(*send_at)),
                            false,
                            Some(ToastAction::Unsend(*outbox_id)),
                            cx,
                        );
                    } else if secs > 0 {
                        this.toast_with(format!("{secs} 秒后发送"), false, Some(ToastAction::Unsend(*outbox_id)), cx);
                    } else {
                        this.toast("正在发送…", false, cx);
                    }
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
        let local = self.accounts.iter().any(|a| a.id == f.account_id && a.is_local());
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
                    Some(if local {
                        "其中的邮件会一并从本机删除，无法恢复。"
                    } else {
                        "会在邮箱服务器上删除这个文件夹及其中的邮件，无法恢复。"
                    }),
                    &["删除", "取消"],
                    cx,
                );
                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    if answer.await.ok() == Some(0) {
                        this.update(cx, |a, cx| {
                            if a.nav == Nav::Folder(fid) {
                                a.set_nav(Nav::View(View::Inbox), cx);
                            }
                            let eng = a.eng.clone();
                            a.run(ops::delete_folder_any(eng, fid), cx, |a, r, cx| {
                                match r {
                                    Ok(()) => a.toast("文件夹已删除", false, cx),
                                    Err(e) => a.toast(format!("{e:#}"), true, cx),
                                }
                                a.load_meta(cx);
                            });
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
            // 默认去掉 utm_* / fbclid 等跟踪参数再打开（设置里可关：kv strip_tracking = "0"）
            let url = if self.store.get_kv("strip_tracking").as_deref() == Some("0") { url.to_string() } else { crate::core::extract::strip_tracking(url) };
            cx.open_url(&url);
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
            return format!(
                "{} 封结果 · {}",
                fmt_total(self.total),
                folder.map(|f| format!("仅「{}」", f.label())).unwrap_or_else(|| "全部账号与文件夹".into())
            );
        }
        if let Some(f) = folder {
            let acc = self.accounts.iter().find(|a| a.id == f.account_id);
            if self.virtual_folder(f.id).is_some() {
                return format!("{} · {} 封（取自本地已同步的文件夹）", acc.map(|a| a.email.as_str()).unwrap_or(""), fmt_total(self.total));
            }
            return if acc.is_some_and(|a| a.is_local()) {
                format!("本地归档 · 共 {} 封（只在本机）", fmt_num(f.local))
            } else {
                format!("{} · 服务器 {} 封 · 本地 {} 封", acc.map(|a| a.email.as_str()).unwrap_or(""), fmt_num(f.total), fmt_num(f.local))
            };
        }
        if self.is_drafts() {
            return format!("{} 封草稿（保存在本机）", fmt_num(self.total));
        }
        let n = self.accounts.iter().filter(|a| !a.is_local()).count();
        format!("{} 封邮件{}", fmt_total(self.total), if n > 1 { format!(" · {n} 个账号") } else { String::new() })
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
                Some(Modal::Prompt { .. }) | Some(Modal::Preview { .. }) | Some(Modal::Panel(_)) | None => {}
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
        self.track_window(window, cx);
        self.sync_freeze(cx);
        let header = self.render_header(&t, window, cx);
        let sidebar = self.sidebar.clone();
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
            let action = tt.action.clone().map(|a| {
                let label = match a {
                    ToastAction::Undo => "撤销 ⌘Z",
                    ToastAction::Unsend(_) => "撤回",
                    ToastAction::Open(_) => "查看",
                    ToastAction::Copy(_) => "复制",
                    ToastAction::Update(_) => "更新",
                };
                div()
                    .id(("toast-act", tt.id as usize))
                    .flex_none()
                    .ml(px(14.))
                    .px(px(9.))
                    .py(px(2.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.toast_fg.opacity(0.4))
                    .font_weight(FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .hover(|s| s.bg(t.toast_fg.opacity(0.12)))
                    .child(label)
                    .on_click(cx.listener(move |this, _, w, cx| this.toast_click(a.clone(), w, cx)))
            });
            toasts = toasts.child(
                div()
                    .max_w(px(toast_w))
                    .px(px(16.))
                    .py(px(9.))
                    .rounded(px(9.))
                    .flex()
                    .items_center()
                    .bg(if tt.error { t.danger } else { t.toast_bg })
                    .text_color(if tt.error { gpui::white() } else { t.toast_fg })
                    .text_size(px(12.5))
                    .shadow(vec![BoxShadow { color: t.shadow, offset: point(px(0.), px(6.)), blur_radius: px(20.), spread_radius: px(0.) }])
                    .child(div().flex_1().child(tt.msg.clone()))
                    .children(action),
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
            .on_action(cx.listener(|a, _: &CommandPalette, w, cx| a.open_palette(w, cx)))
            .on_action(cx.listener(|a, _: &Undo, _, cx| a.undo(cx)))
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
                    .on_action(cx.listener(|a, _: &ShowShortcuts, _, cx| a.open_panel_shortcuts(cx)))
                    .on_action(cx.listener(|a, _: &NextInThread, _, cx| a.go_in_thread(1, cx)))
                    .on_action(cx.listener(|a, _: &PrevInThread, _, cx| a.go_in_thread(-1, cx)))
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
                    .on_mouse_up(MouseButton::Left, cx.listener(|a, _, _, _| a.end_resize()))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(|a, _, _, _| a.end_resize()))
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
