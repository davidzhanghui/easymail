//! 主窗口：顶栏 + 侧栏 + 邮件列表 + 阅读区，外加弹层、模态与提示条。
//!
//! 这里只留「编排」：`MailApp` 的字段、构造、整体渲染、引擎事件分发。
//! 具体实现按职责拆在同目录的几个文件里：
//! - `data`：侧栏元数据、列表、详情与后台刷新
//! - `actions`：已读 / 星标 / 移动 / 删除 / 归档 / 撤回
//! - `shell`：导航、搜索、选中、窗口与分栏
//! - `dialogs`：弹层、模态、主题、提示条、命令
//! - `list` / `reader` / `sidebar` / `pop` / `panels` / `modal`：各区域的渲染
//!
//! 写信是独立窗口（`ui::composer_window`），阅读也可以在新窗口打开（`ui::reader_window`）。

use super::accounts::{AccountsEvent, AccountsModal, Tab};
use super::composer::build_reply;
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

mod actions;
mod data;
mod dialogs;
mod list;
mod modal;
pub(crate) mod panels;
mod pop;
pub(crate) mod reader;
mod shell;
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
    /// 回复模板列表的滚动句柄（用来画滚动条）
    templates_scroll: ScrollHandle,
    /// 本地规则列表的滚动句柄（用来画滚动条）
    rules_scroll: ScrollHandle,
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
            templates_scroll: ScrollHandle::new(),
            rules_scroll: ScrollHandle::new(),
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
}

// ───────────────────────────── 渲染 ─────────────────────────────

fn title_drag(ev: &MouseDownEvent, window: &mut Window) {
    if ev.click_count >= 2 {
        window.zoom_window();
    } else {
        window.start_window_move();
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
                    .child(div().flex_1().child(crate::i18n::tr(&tt.msg).into_owned()))
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
pub(crate) struct Meta {
    accounts: Vec<Account>,
    folders: Vec<Folder>,
    stats: Stats,
    inbox_unread: i64,
}

impl Meta {
    pub(crate) fn fetch(s: &Store) -> Meta {
        Meta {
            accounts: accounts::list(s).unwrap_or_default(),
            folders: queries::folders(s).unwrap_or_default(),
            stats: queries::stats(s).unwrap_or_default(),
            inbox_unread: queries::inbox_unread(s),
        }
    }
}
