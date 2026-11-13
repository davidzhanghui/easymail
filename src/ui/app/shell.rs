//! 导航、搜索、选中与窗口布局。

use super::*;

impl MailApp {
    #[cfg(feature = "dev")]
    pub(crate) fn search_input(&self) -> &Entity<LineInput> {
        &self.search
    }

    pub(crate) fn end_resize(&mut self) {
        if self.resizing.take().is_some() {
            self.save_layout();
        }
    }

    /// 窗口移动 / 缩放都会触发重绘：在渲染时比较一下，变了就防抖保存（GPUI 没有 bounds 变化回调）
    pub(crate) fn track_window(&mut self, window: &Window, cx: &mut Context<Self>) {
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

    pub(crate) fn save_layout(&self) {
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
    pub(crate) fn toggle_saved_search(&mut self, cx: &mut Context<Self>) {
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

    pub(crate) fn set_nav(&mut self, nav: Nav, cx: &mut Context<Self>) {
        self.nav = nav;
        self.filter = Filter::All;
        self.checked.clear();
        self.close_detail(cx);
        // 搜索词保留：切换文件夹后在该文件夹内继续搜索
        self.load_list(false, false, cx);
    }

    pub(crate) fn set_filter(&mut self, f: Filter, cx: &mut Context<Self>) {
        self.filter = f;
        self.checked.clear();
        self.load_list(false, false, cx);
    }

    pub(crate) fn search_changed(&mut self, cx: &mut Context<Self>) {
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

    pub(crate) fn set_query(&mut self, q: String, cx: &mut Context<Self>) {
        self.q = q;
        self.checked.clear();
        self.close_detail(cx);
        self.load_list(false, false, cx);
    }

    pub(crate) fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.invite = None;
        self.selected = None;
        self.detail = None;
        self.detail_state = DetailState::Idle;
        cx.notify();
    }

    pub(crate) fn open_message(&mut self, id: i64, cx: &mut Context<Self>) {
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

    /// 在独立窗口里打开邮件（并排对照）
    pub(crate) fn open_in_window(&mut self, id: i64, cx: &mut Context<Self>) {
        if let Err(e) = crate::ui::reader_window::open(&self.store, id, cx) {
            self.toast(format!("{e:#}"), true, cx);
        }
    }

    /// 点附件：解出到缓存目录后弹窗预览（弹窗里可「另存为」）
    pub(crate) fn open_preview(&mut self, msg_id: i64, index: usize, filename: String, size: u64, cx: &mut Context<Self>) {
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

    pub(crate) fn title(&self) -> String {
        if !self.q.is_empty() {
            return format!("搜索「{}」", self.q);
        }
        match self.nav {
            Nav::Folder(id) => self.folders.iter().find(|f| f.id == id).map(|f| f.label()).unwrap_or_default(),
            Nav::View(v) => view_label(v).into(),
        }
    }

    pub(crate) fn subtitle(&self) -> String {
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

    pub(crate) fn go(&mut self, delta: i64, cx: &mut Context<Self>) {
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

    pub(crate) fn current_ids(&self) -> Vec<i64> {
        if !self.checked.is_empty() {
            self.checked.iter().copied().collect()
        } else {
            self.detail.as_ref().filter(|_| self.detail_state == DetailState::Ready).map(|d| vec![d.id]).unwrap_or_default()
        }
    }

    pub(crate) fn on_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        if self.pop.is_some() {
            self.pop = None;
        } else if self.modal.is_some() {
            match self.modal.take() {
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

    /// 列间分隔条：1px 分隔线 + 5px 可拖拽热区
    pub(crate) fn splitter(&self, which: u8, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
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
