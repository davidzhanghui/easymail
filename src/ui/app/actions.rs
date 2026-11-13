//! 邮件操作：已读 / 星标 / 移动 / 删除 / 归档 / 撤回。

use super::*;

impl MailApp {
    /// ⌘Z / 提示条「撤销」
    pub(crate) fn undo(&mut self, cx: &mut Context<Self>) {
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
    pub(crate) fn unsend(&mut self, outbox_id: i64, window: &mut Window, cx: &mut Context<Self>) {
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

    /// 打开的邮件标为已读（本地立即生效，远端走持久化队列）
    pub(crate) fn mark_opened_read(&mut self, id: i64, cx: &mut Context<Self>) {
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

    /// 回复日历邀请
    pub(crate) fn rsvp(&mut self, id: i64, r: calendar::Rsvp, cx: &mut Context<Self>) {
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

    pub(crate) fn patch(&mut self, ids: &[i64], seen: Option<bool>, flagged: Option<bool>) {
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

    pub(crate) fn remove_from_list(&mut self, ids: &[i64], cx: &mut Context<Self>) {
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

    pub(crate) fn set_seen(&mut self, ids: Vec<i64>, seen: bool, cx: &mut Context<Self>) {
        self.patch(&ids, Some(seen), None);
        let eng = self.eng.clone();
        self.run_op(
            ops::set_flags(eng, ids, ops::FlagChange { seen: Some(seen), flagged: None }),
            move |_| if seen { "已标为已读".into() } else { "已标为未读".into() },
            cx,
        );
        cx.notify();
    }

    pub(crate) fn set_flagged(&mut self, ids: Vec<i64>, flagged: bool, quiet: bool, cx: &mut Context<Self>) {
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

    pub(crate) fn delete(&mut self, ids: Vec<i64>, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(crate) fn do_delete(&mut self, ids: Vec<i64>, cx: &mut Context<Self>) {
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
    pub(crate) fn drop_mails(&mut self, ids: Vec<i64>, folder_id: i64, cx: &mut Context<Self>) {
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
    pub(crate) fn targets(&self, id: i64) -> Vec<i64> {
        if self.checked.contains(&id) {
            self.checked.iter().copied().collect()
        } else {
            vec![id]
        }
    }

    /// Shift 点选：把 anchor 到 id 之间（含）的邮件全部勾上
    pub(crate) fn select_range(&mut self, id: i64) {
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

    pub(crate) fn move_to(&mut self, ids: Vec<i64>, folder_id: i64, cx: &mut Context<Self>) {
        let label = self.folders.iter().find(|f| f.id == folder_id).map(|f| f.label()).unwrap_or_else(|| "文件夹".into());
        self.pop = None;
        self.remove_from_list(&ids, cx);
        let eng = self.eng.clone();
        self.run_op_undo(ops::move_messages(eng, ids, folder_id), move |_| format!("已移动到「{label}」"), cx);
    }

    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn to_role(&mut self, ids: Vec<i64>, role: &'static str, msg: &'static str, cx: &mut Context<Self>) {
        self.remove_from_list(&ids, cx);
        let eng = self.eng.clone();
        self.run_op_undo(async move { ops::move_to_role(eng, ids, role).await }, move |_| msg.into(), cx);
    }

    pub(crate) fn has_role(&self, account_id: i64, role: &str) -> bool {
        self.folders.iter().any(|f| f.account_id == account_id && f.role == role)
    }

    pub(crate) fn sync_now(&mut self, cx: &mut Context<Self>) {
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

    pub(crate) fn folder_action(&mut self, action: &str, fid: i64, window: &mut Window, cx: &mut Context<Self>) {
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
            "toggle-watch" => {
                let watched = crate::core::engine::Engine::watched_folders(&self.store, f.account_id).contains(&fid);
                crate::core::engine::Engine::set_folder_watch(&self.store, f.account_id, fid, !watched);
                let msg = if watched {
                    format!("「{label}」不再实时监听")
                } else {
                    format!("「{label}」已开启实时监听（服务器不支持 NOTIFY，下一轮检查后生效；每个账号最多 3 个）")
                };
                self.toast(msg, false, cx);
            }
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

    pub(crate) fn export(&mut self, kind: transfer::ExportKind, cx: &mut Context<Self>) {
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

    pub(crate) fn save_attachment(&mut self, msg_id: i64, index: usize, filename: String, cx: &mut Context<Self>) {
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

    pub(crate) fn save_eml(&mut self, msg_id: i64, subject: &str, cx: &mut Context<Self>) {
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

    pub(crate) fn open_link(&mut self, url: &str, cx: &mut Context<Self>) {
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

    pub(crate) fn tray_command(&mut self, c: macos::Command, cx: &mut Context<Self>) {
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
}
