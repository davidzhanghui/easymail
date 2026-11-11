//! 脚本驱动的端到端测试（dev 功能）：`EASYMAIL_SCRIPT=<场景>` 启动真实窗口，
//! 用真实按键 / 动作驱动界面，检查界面状态、数据库与截图像素，最后打印 `RESULT PASS|FAIL` 并退出。
//!
//! 场景：
//!   shots  只截图（看外观用，配合任意数据目录）
//!   promo  宣传片素材：抓拍新邮件到达、逐字搜索、十套配色等真实状态序列（配合 scripts/screenshots.sh 的演示数据）
//!   e2e    完整流程（tests/ui_e2e.rs 准备好 GreenMail 账号与邮件后调用）

use super::app::{DetailState, MailApp, Modal, Nav, Pop};
use super::theme::theme;
use crate::core::queries::View;
use crate::{testkit, webview};
use gpui::*;
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct T {
    win: WindowHandle<MailApp>,
    dir: PathBuf,
    fails: Vec<String>,
}

impl T {
    async fn wait(&self, cx: &mut AsyncApp, ms: u64) {
        cx.background_executor().timer(Duration::from_millis(ms)).await;
    }

    fn with<R>(&self, cx: &mut AsyncApp, f: impl FnOnce(&mut MailApp, &mut Window, &mut Context<MailApp>) -> R) -> Option<R> {
        self.win.update(cx, |app, window, cx| f(app, window, cx)).ok()
    }

    fn read<R>(&self, cx: &mut AsyncApp, f: impl FnOnce(&MailApp, &App) -> R) -> Option<R> {
        self.win.update(cx, |app, _, cx| f(app, cx)).ok()
    }

    fn key(&self, cx: &mut AsyncApp, k: &str) {
        let ks = Keystroke::parse(k).expect("keystroke");
        let any: AnyWindowHandle = self.win.into();
        cx.update_window(any, |_, w, cx| w.dispatch_keystroke(ks, cx)).ok();
    }

    async fn shot(&self, cx: &mut AsyncApp, name: &str) -> Option<(image::RgbaImage, f32)> {
        // 等界面画完新的一帧再截
        self.wait(cx, 350).await;
        let path = self.dir.join(format!("{name}.png"));
        let any: AnyWindowHandle = self.win.into();
        cx.update_window(any, |_, w, _| testkit::self_shot(w, &path).map(|i| (i, w.scale_factor()))).ok().flatten()
    }

    async fn until(&self, cx: &mut AsyncApp, secs: u64, mut pred: impl FnMut(&MailApp, &App) -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(secs) {
            if self.read(cx, |a, cx| pred(a, cx)).unwrap_or(false) {
                return true;
            }
            self.wait(cx, 100).await;
        }
        false
    }

    fn check(&mut self, name: &str, ok: bool) {
        println!("CHECK {} {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            self.fails.push(name.to_string());
        }
    }
}

pub fn run(spec: String, win: WindowHandle<MailApp>, cx: &mut App) {
    let dir = PathBuf::from(std::env::var("SHOT_DIR").unwrap_or_else(|_| "/tmp/easymail-shots".into()));
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut t = T { win, dir, fails: vec![] };
        t.wait(cx, 800).await;
        match spec.as_str() {
            "shots" => shots(&mut t, cx).await,
            "promo" => promo(&mut t, cx).await,
            _ => e2e(&mut t, cx).await,
        }
        println!("RESULT {}", if t.fails.is_empty() { "PASS".to_string() } else { format!("FAIL {:?}", t.fails) });
        cx.update(|cx| cx.quit()).ok();
    })
    .detach();
}

async fn shots(t: &mut T, cx: &mut AsyncApp) {
    t.until(cx, 20, |a, _| !a.loading && !a.items.is_empty()).await;
    t.wait(cx, 600).await;
    t.shot(cx, "01-main").await;
    t.key(cx, "j");
    t.until(cx, 20, |a, _| a.detail_state == DetailState::Ready).await;
    t.wait(cx, 1500).await;
    t.shot(cx, "02-reader").await;
    t.key(cx, "r");
    t.wait(cx, 900).await;
    t.shot(cx, "03-compose").await;
    t.key(cx, "escape");
    t.wait(cx, 300).await;
    t.with(cx, |a, _, cx| a.toggle_pop_for_test(Pop::Theme, cx));
    t.wait(cx, 700).await;
    t.shot(cx, "04-theme").await;
    t.with(cx, |a, w, cx| {
        a.pop = None;
        let mut p = a.pref.clone();
        p.pref = "midnight".into();
        a.set_theme_pref(p, w, cx);
    });
    t.wait(cx, 1200).await;
    t.shot(cx, "05-dark").await;
    // 暗色主题下回复：引用原文要看得清（回归检查用，不进 README）
    t.key(cx, "r");
    t.wait(cx, 900).await;
    t.shot(cx, "x-dark-compose").await;
    t.key(cx, "escape");
    t.wait(cx, 300).await;
    t.with(cx, |a, w, cx| {
        let mut p = a.pref.clone();
        p.pref = "moss".into();
        a.set_theme_pref(p, w, cx);
        a.open_accounts(super::accounts::Tab::Accounts, false, w, cx)
    });
    t.wait(cx, 700).await;
    t.shot(cx, "06-accounts").await;
    t.with(cx, |a, w, cx| {
        a.modal = None;
        a.open_import(w, cx)
    });
    t.wait(cx, 500).await;
    t.shot(cx, "07-import").await;
    t.with(cx, |a, _, cx| {
        a.modal = None;
        cx.notify()
    });
    // 搜索语法
    t.with(cx, |a, w, cx| {
        let s = a.search_input().clone();
        w.focus(&s.focus_handle(cx));
        s.update(cx, |i, cx| i.set_text("has:attachment", cx));
        a.set_query("has:attachment".into(), cx)
    });
    t.until(cx, 10, |a, _| !a.loading && a.q == "has:attachment").await;
    t.wait(cx, 800).await;
    t.shot(cx, "08-search").await;
    t.with(cx, |a, w, cx| {
        a.search_input().update(cx, |i, cx| i.set_text("", cx));
        a.set_query(String::new(), cx);
        w.focus(a.focus_handle())
    });
    // 带远程图片 / 追踪像素的邮件：默认拦截
    t.until(cx, 10, |a, _| !a.loading && a.items.len() > 2).await;
    let id = t.read(cx, |a, _| a.items.iter().find(|m| m.subject.contains("第 128 期")).map(|m| m.id)).flatten();
    if let Some(id) = id {
        t.with(cx, |a, _, cx| a.open_message(id, cx));
        t.until(cx, 20, |a, _| a.detail_state == DetailState::Ready && a.detail.as_ref().is_some_and(|d| d.id == id)).await;
        t.wait(cx, 1500).await;
        t.shot(cx, "09-blocked").await;
    }
}

/// 宣传片素材：真实应用里的几段状态变化，逐帧抓拍
async fn promo(t: &mut T, cx: &mut AsyncApp) {
    t.until(cx, 20, |a, _| !a.loading && !a.items.is_empty()).await;
    t.wait(cx, 600).await;
    t.shot(cx, "p-main").await;

    // 1. 新邮件经 IDLE 实时到达
    let target = t.read(cx, |a, _| a.accounts.iter().find(|x| !x.is_local()).cloned()).flatten();
    if let Some(acc) = target {
        let acc_id = acc.id;
        t.until(cx, 30, |a, _| a.eng.watch_mode(acc_id) == crate::core::engine::WatchMode::Idle).await;
        t.wait(cx, 2000).await;
        t.shot(cx, "p-mail-0").await;
        let store = t.read(cx, |a, _| a.store.clone()).unwrap();
        let pw = crate::core::accounts::password(&store, &acc).unwrap_or_default();
        let raw = format!(
            "From: =?UTF-8?B?6ZmI5LiA?= <chenyi@example.com>\r\nTo: linxia@example.com\r\nSubject: =?UTF-8?B?{}?=\r\nDate: {}\r\nMessage-ID: <promo-live@easymail.demo>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, "周六天气晴，照常出发".as_bytes()),
            chrono::Local::now().to_rfc2822(),
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, "预报说周六全天晴，早上七点龙井村口见，别忘了带水。".as_bytes())
        );
        let eng = t.read(cx, |a, _| a.eng.clone()).unwrap();
        let _ = eng
            .call(async move {
                let mut c = crate::core::imap::Imap::connect(&acc, &pw, None).await?;
                c.append("INBOX", "", raw.as_bytes()).await?;
                c.logout().await;
                Ok(())
            })
            .await;
        t.until(cx, 30, |a, _| a.last_newmail.is_some() && a.items.iter().any(|m| m.subject.contains("照常出发"))).await;
        t.shot(cx, "p-mail-1").await;
        t.wait(cx, 1200).await;
        t.shot(cx, "p-mail-2").await;
        t.wait(cx, 3500).await;
    }

    // 2. 逐字搜索：中文子串
    t.with(cx, |a, w, cx| {
        let s = a.search_input().clone();
        w.focus(&s.focus_handle(cx));
    });
    t.shot(cx, "p-search-0").await;
    for (i, q) in ["上", "上海"].iter().enumerate() {
        t.with(cx, |a, _, cx| {
            a.search_input().update(cx, |inp, cx| inp.set_text(q, cx));
            a.set_query(q.to_string(), cx)
        });
        t.until(cx, 10, |a, _| !a.loading && a.q == *q).await;
        t.wait(cx, 500).await;
        t.shot(cx, &format!("p-search-{}", i + 1)).await;
    }
    let hit = t.read(cx, |a, _| a.items.first().map(|m| m.id)).flatten();
    if let Some(id) = hit {
        t.with(cx, |a, _, cx| a.open_message(id, cx));
        t.until(cx, 20, |a, _| a.detail_state == DetailState::Ready).await;
        t.wait(cx, 1500).await;
        t.shot(cx, "p-search-3").await;
    }
    t.with(cx, |a, w, cx| {
        a.search_input().update(cx, |i, cx| i.set_text("", cx));
        a.set_query(String::new(), cx);
        w.focus(a.focus_handle())
    });
    t.until(cx, 10, |a, _| !a.loading && a.items.len() > 2).await;

    // 3. 远程图片 / 追踪像素默认拦截
    let id = t.read(cx, |a, _| a.items.iter().find(|m| m.subject.contains("第 128 期")).map(|m| m.id)).flatten();
    if let Some(id) = id {
        t.with(cx, |a, _, cx| a.open_message(id, cx));
        t.until(cx, 20, |a, _| a.detail_state == DetailState::Ready && a.detail.as_ref().is_some_and(|d| d.id == id)).await;
        t.wait(cx, 1500).await;
        t.shot(cx, "p-blocked").await;
    }

    // 4. 十套配色（阅读周报）
    let id = t.read(cx, |a, _| a.items.iter().find(|m| m.subject.contains("周报")).map(|m| m.id)).flatten();
    if let Some(id) = id {
        t.with(cx, |a, _, cx| a.open_message(id, cx));
        t.until(cx, 20, |a, _| a.detail_state == DetailState::Ready && a.detail.as_ref().is_some_and(|d| d.id == id)).await;
        t.wait(cx, 1200).await;
    }
    for p in super::palettes::PALETTES.iter() {
        let pid = p.id;
        t.with(cx, |a, w, cx| {
            let mut pr = a.pref.clone();
            pr.pref = pid.into();
            a.set_theme_pref(pr, w, cx);
        });
        t.wait(cx, 1000).await;
        t.shot(cx, &format!("p-theme-{pid}")).await;
    }
    t.with(cx, |a, w, cx| {
        let mut pr = a.pref.clone();
        pr.pref = "moss".into();
        a.set_theme_pref(pr, w, cx);
    });
}

fn db_flag(a: &MailApp, id: i64, col: &str) -> i64 {
    a.store.r().query_row(&format!("SELECT {col} FROM messages WHERE id = ?"), [id], |r| r.get(0)).unwrap_or(-1)
}

async fn e2e(t: &mut T, cx: &mut AsyncApp) {
    // 1. 首次同步完成，统一收件箱出现邮件
    let synced = t.until(cx, 60, |a, _| a.items.len() >= 3 && !a.sync.values().any(|s| s.syncing)).await;
    t.check("同步后统一收件箱有邮件", synced);
    let n = t.read(cx, |a, _| a.items.len()).unwrap_or(0);
    t.check("标题 / 侧栏", t.read(cx, |a, _| a.title() == "统一收件箱" && a.folders.iter().any(|f| f.role == "inbox")).unwrap_or(false));
    t.shot(cx, "e2e-01-list").await;

    // 2. j 打开第一封：正文渲染在 WebView，自动标为已读
    t.key(cx, "j");
    let opened = t.until(cx, 20, |a, _| a.detail_state == DetailState::Ready).await;
    t.check("j 打开第一封", opened);
    t.wait(cx, 1200).await;
    let first = t.read(cx, |a, _| a.selected.unwrap_or(0)).unwrap_or(0);
    t.check("WebView 已显示", webview::is_live() && !webview::is_frozen());
    let body = webview::eval(cx, "document.body.innerText").await.unwrap_or_default();
    t.check("正文进入 WebView", body.contains("正文"));
    t.check("打开即已读（数据库）", t.until(cx, 5, |a, _| db_flag(a, first, "seen") == 1).await);
    if let Some((img, s)) = t.shot(cx, "e2e-02-reader").await {
        // 侧栏背景（panel_2）与主题一致
        let want = t.read(cx, |_, cx| theme(cx).panel_2.to_rgb()).unwrap();
        let p = testkit::pixel(&img, s, 8., 300.);
        let near = |v: f32, b: u8| ((v * 255.) as i32 - b as i32).abs() <= 24;
        t.check("截图：侧栏底色", near(want.r, p[0]) && near(want.g, p[1]) && near(want.b, p[2]));
    }

    // 3. s 星标 / u 未读：本地立即生效，并写回服务器（数据库标记）
    t.key(cx, "s");
    t.check("s 加星标", t.until(cx, 5, |a, _| a.detail.as_ref().is_some_and(|d| d.flagged) && db_flag(a, first, "flagged") == 1).await);
    t.key(cx, "u");
    t.check("u 标为未读", t.until(cx, 5, |a, _| db_flag(a, first, "seen") == 0).await);
    t.key(cx, "u");

    // 4. 弹层打开时 WebView 冻结成快照，关闭后恢复
    t.with(cx, |a, _, cx| a.toggle_pop_for_test(Pop::Theme, cx));
    t.check("弹层冻结 WebView", t.until(cx, 5, |_, _| webview::is_frozen()).await);
    t.shot(cx, "e2e-03-popover").await;
    t.key(cx, "escape");
    t.check("关闭弹层恢复 WebView", t.until(cx, 5, |a, _| a.pop.is_none() && !webview::is_frozen()).await);

    // 5. 切换深色主题
    t.with(cx, |a, w, cx| {
        let mut p = a.pref.clone();
        p.pref = "midnight".into();
        a.set_theme_pref(p, w, cx)
    });
    t.wait(cx, 600).await;
    t.check("切换到深色主题", t.read(cx, |_, cx| theme(cx).dark).unwrap_or(false));
    t.shot(cx, "e2e-04-dark").await;
    t.with(cx, |a, w, cx| {
        let mut p = a.pref.clone();
        p.pref = "moss".into();
        a.set_theme_pref(p, w, cx)
    });

    // 6. r 回复：预填收件人 / 主题，补一句正文后 ⌘↵ 发送
    t.key(cx, "r");
    let composer = t.read(cx, |a, _| matches!(a.modal, Some(Modal::Composer(_)))).unwrap_or(false);
    t.check("r 打开回复窗口", composer);
    t.check("回复时 WebView 冻结", t.until(cx, 5, |_, _| webview::is_frozen()).await);
    let (to, subj) = t
        .read(cx, |a, cx| match &a.modal {
            Some(Modal::Composer(c)) => (c.read(cx).to.read(cx).text(), c.read(cx).subject.read(cx).text()),
            _ => Default::default(),
        })
        .unwrap_or_default();
    t.check("回复预填收件人", to.contains("bob@local.test"));
    t.check("回复预填主题", subj.starts_with("Re: "));
    t.with(cx, |a, _, cx| {
        if let Some(Modal::Composer(c)) = &a.modal {
            c.update(cx, |c, cx| c.insert_body_text("界面自动化测试的回复", cx));
        }
    });
    // 富文本编辑器：WebView 加载后，全选加粗、插 emoji，草稿正文应是带格式的 HTML
    t.check("富文本编辑器已加载", t.until(cx, 10, |_, _| crate::editor::is_live()).await);
    t.wait(cx, 1200).await;
    let txt = crate::editor::eval(cx, "document.getElementById('ed').innerText").await.unwrap_or_default();
    t.check("编辑器里有回复正文与引用", txt.contains("界面自动化测试的回复") && txt.contains("写道"));
    crate::editor::eval(
        cx,
        "(function(){ var e=document.getElementById('ed'); e.focus(); document.execCommand('selectAll'); EM.exec('bold'); EM.insertText('😀'); return 1 })()",
    )
    .await;
    t.wait(cx, 300).await;
    let body = t.read(cx, |a, cx| match &a.modal {
        Some(Modal::Composer(c)) => c.read(cx).body_text(cx),
        _ => String::new(),
    });
    let body = body.unwrap_or_default();
    t.check("正文是 HTML 且含 emoji", crate::core::mailer::is_html_body(&body) && body.contains('😀'));
    t.shot(cx, "e2e-05-reply").await;
    t.key(cx, "cmd-enter");
    let sent = t.until(cx, 30, |a, _| a.modal.is_none() && a.toasts.iter().any(|x| x.msg.starts_with("已发送"))).await;
    t.check("⌘↵ 发送成功", sent);
    t.check("原邮件标记已回复", t.until(cx, 10, |a, _| db_flag(a, first, "answered") == 1).await);

    // 7. 新邮件实时到达（IDLE）：测试进程投递，界面收到提醒并刷新列表
    println!("READY_FOR_NEWMAIL");
    let arrived = t.until(cx, 45, |a, _| a.last_newmail.is_some() && a.items.iter().any(|m| m.subject == "实时到达的新邮件")).await;
    t.check("IDLE 新邮件提醒并出现在列表", arrived);

    // 8. 搜索
    t.with(cx, |a, w, cx| {
        let s = a.search_input().clone();
        w.focus(&s.focus_handle(cx));
        s.update(cx, |i, cx| i.set_text("subject:发票", cx));
        a.set_query("subject:发票".into(), cx)
    });
    t.check("搜索主题", t.until(cx, 10, |a, _| !a.loading && a.items.len() == 1 && a.items[0].subject.contains("发票")).await);
    t.shot(cx, "e2e-06-search").await;
    t.with(cx, |a, w, cx| {
        a.search_input().update(cx, |i, cx| i.set_text("", cx));
        a.set_query(String::new(), cx);
        w.focus(a.focus_handle())
    });
    t.until(cx, 10, |a, _| !a.loading && a.items.len() >= n).await;

    // 9. 筛选：星标
    t.with(cx, |a, _, cx| a.set_filter(crate::core::queries::Filter::Starred, cx));
    t.check("筛选星标", t.until(cx, 10, |a, _| !a.loading && a.items.len() == 1 && a.items[0].id == first).await);
    t.with(cx, |a, _, cx| a.set_filter(crate::core::queries::Filter::All, cx));
    t.until(cx, 10, |a, _| !a.loading && a.items.len() >= n).await;

    // 10. 批量选择 → 删除（移入垃圾箱）
    let victim = t.read(cx, |a, _| a.items.iter().find(|m| m.subject.contains("要删除")).map(|m| m.id)).flatten();
    t.check("找到待删除邮件", victim.is_some());
    if let Some(v) = victim {
        t.with(cx, |a, w, cx| {
            a.checked.insert(v);
            w.focus(a.focus_handle());
            cx.notify()
        });
        t.key(cx, "#");
        t.check("# 删除后从列表消失", t.until(cx, 5, |a, _| !a.items.iter().any(|m| m.id == v)).await);
        t.check(
            "删除进了垃圾箱",
            t.until(cx, 15, |a, _| {
                a.store
                    .r()
                    .query_row(
                        "SELECT COUNT(*) FROM messages m JOIN folders f ON f.id = m.folder_id WHERE f.role = 'trash' AND m.subject LIKE '%要删除%'",
                        [],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0)
                    == 1
            })
            .await,
        );
    }

    // 11. 进入文件夹 → 已发送：刚发的回复存在（只存一份）
    let sent_fid = t.read(cx, |a, _| a.folders.iter().find(|f| f.role == "sent").map(|f| f.id)).flatten();
    if let Some(fid) = sent_fid {
        t.with(cx, |a, _, cx| a.set_nav(Nav::Folder(fid), cx));
        t.with(cx, |a, _, _| a.eng.request_sync(a.folders.iter().find(|f| f.id == fid).map(|f| f.account_id).unwrap_or(0), Some(fid), false));
        t.check("已发送里有一封回复", t.until(cx, 20, |a, _| !a.loading && a.items.iter().filter(|m| m.subject.starts_with("Re: ")).count() == 1).await);
        t.shot(cx, "e2e-07-sent").await;
    } else {
        t.check("存在已发送文件夹", false);
    }

    // 12. 草稿：Esc 直接丢弃；点「保存为草稿」才进草稿视图
    let fill = |a: &mut MailApp, w: &mut Window, cx: &mut Context<MailApp>| {
        a.set_nav(Nav::View(View::Inbox), cx);
        a.compose_new(w, cx);
        if let Some(Modal::Composer(c)) = &a.modal {
            c.update(cx, |c, cx| {
                c.subject.update(cx, |i, cx| i.set_text("没写完的草稿", cx));
                c.insert_body_text("明天再写", cx);
            });
        }
    };
    t.with(cx, fill);
    t.wait(cx, 300).await;
    t.key(cx, "escape");
    t.check("Esc 关闭写信窗口", t.until(cx, 5, |a, _| a.modal.is_none()).await);
    t.wait(cx, 2500).await; // 以前 2 秒会自动存草稿
    t.with(cx, |a, _, cx| a.set_nav(Nav::View(View::Drafts), cx));
    t.wait(cx, 300).await;
    t.check("Esc 不存草稿", t.read(cx, |a, _| !a.drafts.iter().any(|d| d.subject == "没写完的草稿")).unwrap_or(false));
    t.with(cx, fill);
    t.wait(cx, 300).await;
    t.with(cx, |a, _, cx| {
        if let Some(Modal::Composer(c)) = &a.modal {
            c.update(cx, |c, cx| c.save_and_close(cx));
        }
    });
    t.check("保存为草稿并关闭", t.until(cx, 5, |a, _| a.modal.is_none() && a.toasts.iter().any(|x| x.msg.contains("草稿箱"))).await);
    t.with(cx, |a, _, cx| a.set_nav(Nav::View(View::Drafts), cx));
    t.check("草稿视图", t.until(cx, 5, |a, _| a.drafts.iter().any(|d| d.subject == "没写完的草稿")).await);
    t.shot(cx, "e2e-08-drafts").await;

    // 12b. 回复 HTML 邮件：原文以富文本引用（粗体 / 链接 / 表格 / 内嵌图保留，脚本被清洗掉）
    t.with(cx, |a, w, cx| {
        let d = crate::core::model::Draft {
            mode: "reply".into(),
            account_id: a.accounts.iter().find(|x| !x.is_local()).map(|x| x.id),
            body: "\n\n在 某日，Bob 写道：".into(),
            quote_html: "<style>p{color:red}</style><table><tr><td><b>粗体原文</b> <a href=\"https://a.com/x\">链接</a></td></tr></table><img src=\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==\"><script>alert(1)</script><img src=\"https://t.co/p.gif\">".into(),
            ..Default::default()
        };
        a.open_composer(d, w, cx);
    });
    t.check("HTML 回复：编辑器加载", t.until(cx, 10, |_, _| crate::editor::is_live()).await);
    t.wait(cx, 1500).await;
    let html = crate::editor::eval(cx, "document.getElementById('ed').innerHTML").await.unwrap_or_default();
    t.check(
        "原邮件富文本被引用且已清洗",
        html.contains("<blockquote")
            && html.contains("粗体原文")
            && html.contains("href=\"https://a.com/x\"")
            && html.contains("<img")
            && !html.contains("alert")
            && !html.contains("t.co")
            && !html.contains("<style"),
    );
    // 方向键只应移动光标，不能往正文里插字符（曾出现 U+F700 一类的方框）
    t.with(cx, |a, w, cx| {
        if let Some(Modal::Composer(c)) = &a.modal {
            c.update(cx, |c, cx| c.focus_body_for_test(w, cx));
        }
    });
    t.wait(cx, 400).await;
    for (code, ch) in [(126u16, "\u{F700}"), (125, "\u{F701}"), (123, "\u{F702}"), (124, "\u{F703}"), (0, "a")] {
        crate::editor::test_key(code, ch);
        t.wait(cx, 100).await;
    }
    let codes = crate::editor::eval(
        cx,
        "Array.from(document.getElementById('ed').innerText).filter(function(c){var n=c.charCodeAt(0);return n>=0xE000&&n<=0xF8FF||n===0xFFFC}).length",
    )
    .await
    .unwrap_or_default();
    t.check("方向键不会往正文里插字符", codes == "0");
    let first = crate::editor::eval(cx, "document.getElementById('ed').innerText.charAt(0)").await.unwrap_or_default();
    let line2 = crate::editor::eval(cx, "document.getElementById('ed').innerText.split('\\n').indexOf('a')").await.unwrap_or_default();
    let _ = first;
    t.check("原生按键确实送达编辑器", line2 != "-1");

    // 新信：先在收件人里，再点击编辑区下方的空白处，然后按方向键
    t.key(cx, "escape");
    t.check("关闭 HTML 回复窗口", t.until(cx, 5, |a, _| a.modal.is_none()).await);
    t.with(cx, |a, w, cx| a.compose_new(w, cx));
    t.wait(cx, 1500).await;
    crate::editor::test_click(300., 150.);
    t.wait(cx, 300).await;
    for (code, ch) in [(126u16, "\u{F700}"), (125, "\u{F701}"), (123, "\u{F702}"), (124, "\u{F703}")] {
        crate::editor::test_key(code, ch);
        t.wait(cx, 100).await;
    }
    let codes = crate::editor::eval(
        cx,
        "Array.from(document.getElementById('ed').innerText).filter(function(c){var n=c.charCodeAt(0);return n>=0xE000&&n<=0xF8FF||n===0xFFFC}).length",
    )
    .await
    .unwrap_or_default();
    t.check("点击空白后按方向键不插字符", codes == "0");
    t.shot(cx, "e2e-07c-arrows").await;
    // 即使功能键字符真的进了输入流，也会被拦下 / 清掉
    crate::editor::eval(
        cx,
        "(function(){ var e=document.getElementById('ed'); e.focus(); document.execCommand('insertText', false, 'x\\uF700y'); return 1 })()",
    )
    .await;
    t.wait(cx, 200).await;
    let txt = crate::editor::eval(cx, "document.getElementById('ed').innerText").await.unwrap_or_default();
    t.check("功能键私有区字符被过滤", txt.contains("xy") && !txt.contains('\u{F700}'));
    t.key(cx, "escape");
    t.check("关闭新信窗口", t.until(cx, 5, |a, _| a.modal.is_none()).await);
    t.shot(cx, "e2e-07b-html-reply").await;

    // 13. 账号与设置 / 导入窗口能打开
    t.with(cx, |a, w, cx| a.open_accounts(super::accounts::Tab::Accounts, false, w, cx));
    t.wait(cx, 400).await;
    t.check("账号窗口", t.read(cx, |a, _| matches!(a.modal, Some(Modal::Accounts(_)))).unwrap_or(false));
    t.shot(cx, "e2e-09-accounts").await;
    t.key(cx, "escape");
    t.with(cx, |a, w, cx| a.open_import(w, cx));
    t.wait(cx, 300).await;
    t.check("导入窗口", t.read(cx, |a, _| matches!(a.modal, Some(Modal::Import(_)))).unwrap_or(false));
    t.key(cx, "escape");
    t.check("Esc 关闭模态", t.until(cx, 3, |a, _| a.modal.is_none()).await);

    // 14. Dock 角标与收件箱未读一致
    t.with(cx, |a, _, cx| a.load_meta(cx));
    let unread = t.read(cx, |a, _| crate::core::queries::inbox_unread(&a.store)).unwrap_or(-1);
    t.check("Dock 角标 = 收件箱未读", super::macos::unread() == unread);

    images(t, cx).await;
}

/// 视图里所有图片的（已加载, 自然宽度, 显示宽度）
const IMG_STATE_JS: &str =
    "JSON.stringify(Array.from(document.images).map(function(i){return [i.complete, i.naturalWidth, Math.round(i.getBoundingClientRect().width)]}))";

/// 二维码（480 宽）与大图猫（896×1184，约 2 MB）都已解码且显示出来
fn both_rendered(state: &str) -> bool {
    let v: Vec<(bool, i64, i64)> = serde_json::from_str(state).unwrap_or_default();
    [480, 896].iter().all(|w| v.iter().any(|&(done, natural, shown)| done && natural == *w && shown > 0))
}

/// 反复取页面里的图片状态，直到两张图都显示（图片解码需要一点时间）
async fn wait_images(t: &mut T, cx: &mut AsyncApp, editor: bool) -> String {
    let mut state = String::new();
    for _ in 0..40 {
        t.wait(cx, 200).await;
        state = if editor { crate::editor::eval(cx, IMG_STATE_JS).await } else { webview::eval(cx, IMG_STATE_JS).await }.unwrap_or_default();
        if both_rendered(&state) {
            break;
        }
    }
    state
}

/// 打开统一收件箱里主题为 `subject` 的邮件，返回阅读视图的图片状态
async fn open_by_subject(t: &mut T, cx: &mut AsyncApp, subject: &str) -> String {
    t.with(cx, |a, _, cx| {
        a.set_nav(Nav::View(View::Inbox), cx);
        if let Some(acc) = a.accounts.iter().find(|x| !x.is_local()) {
            a.eng.request_sync(acc.id, None, false);
        }
    });
    let arrived = t.until(cx, 30, |a, _| !a.loading && a.items.iter().any(|m| m.subject == subject)).await;
    t.check(&format!("收到「{subject}」"), arrived);
    let id = t.read(cx, |a, _| a.items.iter().find(|m| m.subject == subject).map(|m| m.id)).flatten();
    let Some(id) = id else { return String::new() };
    t.with(cx, |a, _, cx| a.open_message(id, cx));
    t.until(cx, 20, |a, _| a.detail_state == DetailState::Ready && a.detail.as_ref().is_some_and(|d| d.id == id)).await;
    wait_images(t, cx, false).await
}

/// 阅读视图里各图片 src 的（类型前缀, 长度）
const SRC_JS: &str = "JSON.stringify(Array.from(document.images).map(function(i){var s=i.getAttribute('src')||'';return [s.slice(0,15), s.length]}))";

/// 15. 富文本里的图片（小图二维码 + 2 MB 大图）：插图发信 → 收件方阅读视图能显示 → 回复时引用里保留 → 回复发出后对方看到原图
async fn images(t: &mut T, cx: &mut AsyncApp) {
    let qr = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/qrcode.png"));
    let cat = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cat.png"));
    t.with(cx, |a, w, cx| {
        a.set_nav(Nav::View(View::Inbox), cx);
        a.compose_new(w, cx);
        if let Some(Modal::Composer(c)) = &a.modal {
            c.update(cx, |c, cx| {
                c.to.update(cx, |i, cx| i.set_text("me@local.test", cx));
                c.subject.update(cx, |i, cx| i.set_text("图片渲染测试", cx));
            });
        }
    });
    t.check("图片：编辑器加载", t.until(cx, 10, |_, _| crate::editor::is_live()).await);
    t.wait(cx, 1200).await;
    t.with(cx, |a, _, cx| {
        if let Some(Modal::Composer(c)) = &a.modal {
            c.update(cx, |c, cx| {
                c.insert_body_text("下面是一张二维码和一只猫：", cx);
                c.ingest_for_test(qr.clone(), cx);
                c.ingest_for_test(cat.clone(), cx);
            });
        }
    });
    let ed_state = wait_images(t, cx, true).await;
    t.check(&format!("图片：插入后编辑器里显示 {ed_state}"), both_rendered(&ed_state));
    t.check(
        "图片：草稿正文只带 data-token（不含 base64）",
        t.until(cx, 5, |a, cx| {
            matches!(&a.modal, Some(Modal::Composer(c)) if { let b = c.read(cx).body_text(cx); b.matches("data-token=").count() == 2 && !b.contains("base64") })
        })
        .await,
    );
    t.shot(cx, "e2e-10-image-compose").await;
    t.key(cx, "cmd-enter");
    t.check("图片：发送成功", t.until(cx, 60, |a, _| a.modal.is_none() && a.toasts.iter().any(|x| x.msg.starts_with("已发送"))).await);

    // 收件方（发给自己）阅读视图
    let state = open_by_subject(t, cx, "图片渲染测试").await;
    t.check(&format!("图片：收到的邮件里两张图正常渲染 {state}"), both_rendered(&state));
    let src = webview::eval(cx, SRC_JS).await.unwrap_or_default();
    t.check(&format!("图片：cid 已换成 data URI（{src}）"), src.matches("data:image/png").count() == 2);
    t.shot(cx, "e2e-11-image-read").await;

    // 回复：引用里保留两张图（大图不能被丢掉），草稿正文不带 base64
    t.key(cx, "r");
    t.check("图片：回复编辑器加载", t.until(cx, 10, |a, _| matches!(a.modal, Some(Modal::Composer(_))) && crate::editor::is_live()).await);
    let q_state = wait_images(t, cx, true).await;
    t.check(&format!("图片：回复引用里两张图都显示 {q_state}"), both_rendered(&q_state));
    t.with(cx, |a, _, cx| {
        if let Some(Modal::Composer(c)) = &a.modal {
            c.update(cx, |c, cx| c.insert_body_text("收到，图片看得到", cx));
        }
    });
    t.wait(cx, 600).await;
    t.check(
        "图片：回复草稿里引用图片也是 token",
        t.read(cx, |a, cx| matches!(&a.modal, Some(Modal::Composer(c)) if { let b = c.read(cx).body_text(cx); b.matches("data-token=").count() == 2 && !b.contains("base64") }))
            .unwrap_or(false),
    );
    t.shot(cx, "e2e-12-image-reply").await;
    t.key(cx, "cmd-enter");
    t.check("图片：回复窗口关闭（发送）", t.until(cx, 60, |a, _| a.modal.is_none()).await);

    let state = open_by_subject(t, cx, "Re: 图片渲染测试").await;
    t.check(&format!("图片：回复邮件里引用的两张图正常渲染 {state}"), both_rendered(&state));
    // 发出去的是原图（PNG，约 2 MB → base64 ≈ 2.66M 字符），不是编辑器里的预览
    let src = webview::eval(cx, SRC_JS).await.unwrap_or_default();
    let v: Vec<(String, usize)> = serde_json::from_str(&src).unwrap_or_default();
    t.check(&format!("图片：回复里大图是原图 {src}"), v.iter().any(|(p, n)| p.starts_with("data:image/png") && *n > 2_600_000));
    let txt = webview::eval(cx, "document.body.innerText").await.unwrap_or_default();
    t.check("图片：回复正文在", txt.contains("收到，图片看得到"));
    t.shot(cx, "e2e-13-image-reply-read").await;
}
