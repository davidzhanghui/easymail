//! 数据加载：侧栏元数据、邮件列表、详情、后台刷新。

use super::*;

impl MailApp {
    /// 账号、文件夹（含逐夹计数）、统计、未读数：在后台查，查完回主线程应用
    pub(crate) fn load_meta(&mut self, cx: &mut Context<Self>) {
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

    pub(crate) fn apply_meta(&mut self, m: Meta, cx: &mut Context<Self>) {
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

    pub(crate) fn is_drafts(&self) -> bool {
        self.nav == Nav::View(View::Drafts) && self.q.is_empty()
    }

    /// 正在看某个账号的「草稿箱」文件夹：返回该账号 id（本机草稿也要显示在这里）
    pub(crate) fn folder_drafts_account(&self) -> Option<i64> {
        match self.nav {
            Nav::Folder(id) if self.q.is_empty() => self.folders.iter().find(|f| f.id == id && f.role == "drafts").map(|f| f.account_id),
            _ => None,
        }
    }

    /// 未同步的 Gmail 虚拟文件夹映射成本地视图：「所有邮件」→ 该账号全部邮件，「已加星标」→ 该账号星标邮件
    pub(crate) fn virtual_folder(&self, id: i64) -> Option<(View, i64)> {
        let f = self.folders.iter().find(|f| f.id == id && !f.sync_enabled)?;
        let view = match f.role.as_str() {
            "all" => View::All,
            "flagged" => View::Starred,
            _ => return None,
        };
        Some((view, f.account_id))
    }

    pub(crate) fn params(&self) -> ListParams {
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
    pub(crate) fn load_list(&mut self, silent: bool, append: bool, cx: &mut Context<Self>) {
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
    pub(crate) fn soft_refresh(&mut self, cx: &mut Context<Self>) {
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

    pub(crate) fn refresh_detail(&mut self, cx: &mut Context<Self>) {
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
}
