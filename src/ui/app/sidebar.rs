//! 侧栏：视图、账号与文件夹树。
//!
//! 独立的 GPUI Entity：自己持有「账号折叠」状态并负责持久化；
//! 用户的点击一律以 [`SidebarEvent`] 发出，由 `MailApp` 订阅处理（切视图、开菜单、同步、导入…），
//! 不再直接改 `MailApp` 的字段。显示用的数据（账号、文件夹、未读数…）在渲染时从 `MailApp` 取一份快照，
//! 并通过 observe 在 `MailApp` 变化时自动重绘。

use super::*;
use crate::core::engine::SyncState;

/// 侧栏发出的事件
#[derive(Clone, Debug)]
pub enum SidebarEvent {
    /// 点了某个视图（收件箱 / 未读 / …）
    View(View),
    /// 点了某条保存的搜索
    Saved(String),
    Folder(i64),
    /// 账号 / 文件夹的「更多」菜单，带点击位置
    AccountMenu(i64, Point<Pixels>),
    FolderMenu(i64, Point<Pixels>),
    /// 立即同步某个账号
    Sync(i64),
    /// 把邮件拖到了某个文件夹上
    DropMails(Vec<i64>, i64),
    Import,
    Settings,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

pub struct Sidebar {
    app: WeakEntity<MailApp>,
    store: Store,
    /// 折叠起来的账号（kv `collapsed`）
    collapsed: HashSet<i64>,
    _observe: Subscription,
}

/// 渲染一次所需的 `MailApp` 数据
struct Snapshot {
    nav: Nav,
    searching: bool,
    q: String,
    stats: Stats,
    accounts: Vec<Account>,
    folders: Vec<Folder>,
    sync: HashMap<i64, SyncState>,
    /// 本地没有同步、靠视图过滤出来的「虚拟」文件夹（Gmail 的所有邮件 / 已加星标）
    virtual_folders: HashSet<i64>,
    width: f32,
}

impl Sidebar {
    pub fn new(app: &Entity<MailApp>, store: Store, cx: &mut Context<Self>) -> Self {
        let collapsed = store.get_kv("collapsed").and_then(|s| serde_json::from_str::<Vec<i64>>(&s).ok()).unwrap_or_default().into_iter().collect();
        // MailApp 的任何变化都重绘侧栏（nav、未读数、同步状态、宽度……）
        let _observe = cx.observe(app, |_, _, cx| cx.notify());
        Sidebar { app: app.downgrade(), store, collapsed, _observe }
    }

    fn toggle_collapsed(&mut self, account: i64, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&account) {
            self.collapsed.insert(account);
        }
        let v: Vec<i64> = self.collapsed.iter().copied().collect();
        self.store.set_kv("collapsed", &serde_json::to_string(&v).unwrap_or_default());
        cx.notify();
    }

    fn snapshot(&self, cx: &App) -> Option<Snapshot> {
        let app = self.app.upgrade()?;
        let a = app.read(cx);
        Some(Snapshot {
            nav: a.nav,
            searching: !a.q.is_empty(),
            q: a.q.clone(),
            stats: a.stats.clone(),
            accounts: a.accounts.clone(),
            folders: a.folders.clone(),
            sync: a.sync.clone(),
            virtual_folders: a.folders.iter().filter(|f| a.virtual_folder(f.id).is_some()).map(|f| f.id).collect(),
            width: a.sidebar_w,
        })
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        let t = &theme;
        let Some(snap) = self.snapshot(cx) else { return div().id("sidebar").into_any_element() };
        let mut col = div()
            .flex()
            .flex_col()
            .min_h_full()
            .px(px(12.))
            .pt(px(16.))
            .pb(px(18.))
            .child(div().text_size(px(11.)).text_color(t.muted).px(px(10.)).mb(px(8.)).child("邮  件"));
        for v in VIEWS {
            let active = snap.nav == Nav::View(v) && !snap.searching;
            let n = match v {
                View::Inbox => snap.stats.inbox,
                View::Unread => snap.stats.unread,
                View::Starred => snap.stats.starred,
                View::Drafts => snap.stats.drafts,
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
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::View(v)))),
            );
        }
        let saved = panels::load_saved(&self.store);
        if !saved.is_empty() {
            col = col.child(div().text_size(px(11.)).text_color(t.muted).px(px(10.)).mt(px(20.)).mb(px(8.)).child("保存的搜索"));
            for (i, q) in saved.into_iter().enumerate() {
                let active = snap.q == q;
                let q2 = q.clone();
                col = col.child(
                    div()
                        .id(("saved", i))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .h(px(H_ROW))
                        .px(px(10.))
                        .rounded(px(7.))
                        .cursor_pointer()
                        .text_size(px(13.))
                        .text_color(if active { t.accent_deep } else { t.ink })
                        .when(active, |d| d.bg(t.accent_soft))
                        .hover(|s| s.bg(t.row_hover))
                        .child(icon("search", 14., t.muted))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(q))
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::Saved(q2.clone())))),
                );
            }
        }
        col = col.child(div().text_size(px(11.)).text_color(t.muted).px(px(10.)).mt(px(20.)).mb(px(8.)).child("账  号"));
        let accounts = snap.accounts.clone();
        for a in accounts.iter() {
            let list: Vec<&Folder> = snap.folders.iter().filter(|f| f.account_id == a.id).collect();
            if a.is_local() && list.is_empty() {
                continue;
            }
            let closed = self.collapsed.contains(&a.id);
            let st = snap.sync.get(&a.id).cloned().unwrap_or_default();
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
                            .on_click(cx.listener(move |sb, _, _, cx| sb.toggle_collapsed(aid, cx))),
                    )
                    .when(!local, |d| {
                        d.child(
                            button(SharedString::from(format!("acct-sync-{aid}")), t, Kind::Ghost, Some("refresh"), "", true)
                                .text_color(if st.syncing { t.accent } else { t.muted })
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::Sync(aid)))),
                        )
                    })
                    .child(
                        button(SharedString::from(format!("acct-more-{aid}")), t, Kind::Ghost, Some("more"), "", true)
                            .on_click(cx.listener(move |_, ev: &ClickEvent, _, cx| cx.emit(SidebarEvent::AccountMenu(aid, ev.position())))),
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
                let active = snap.nav == Nav::Folder(f.id);
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
                        } else if f.sync_enabled || local || snap.virtual_folders.contains(&f.id) {
                            t.ink
                        } else {
                            t.faint
                        })
                        .when(active, |d| d.bg(t.accent_soft).font_weight(FontWeight::SEMIBOLD))
                        .hover(|s| s.bg(if active { t.accent_soft } else { t.row_hover }))
                        .drag_over::<DragMails>({
                            let (soft, acc) = (theme.accent_soft, theme.accent);
                            move |s, _, _, _| s.bg(soft).border_1().border_color(acc)
                        })
                        .on_drop(cx.listener(move |_, d: &DragMails, _, cx| cx.emit(SidebarEvent::DropMails(d.ids.clone(), fid))))
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
                                .on_click(cx.listener(move |_, ev: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(SidebarEvent::FolderMenu(fid, ev.position()))
                                })),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::Folder(fid)))),
                );
            }
        }
        let col = col
            .child(div().flex_1().min_h(px(18.)))
            .child(
                button("side-import", t, Kind::Ghost, Some("download"), "导入邮件", true)
                    .justify_start()
                    .w_full()
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::Import))),
            )
            .child(
                button("side-accounts", t, Kind::Ghost, Some("gear"), "账号与设置", true)
                    .justify_start()
                    .w_full()
                    .mt(px(4.))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::Settings))),
            )
            .child(div().px(px(10.)).pt(px(6.)).text_size(px(10.5)).text_color(t.faint).child(format!(
                "本地存档 {} 封 · 已缓存正文 {} 封",
                fmt_num(snap.stats.archived),
                fmt_num(snap.stats.with_body)
            )));
        // 外层只负责滚动；内层 flex 列的子项一律不收缩（否则内容超高时条目会被压扁）
        div().id("sidebar").flex_none().w(px(snap.width)).h_full().bg(t.panel_2).overflow_y_scroll().child(col).into_any_element()
    }
}
