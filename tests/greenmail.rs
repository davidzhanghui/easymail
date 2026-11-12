//! 端到端集成测试：本机 GreenMail（Java，非 docker）提供真实的 IMAP / SMTP。
//!
//! 覆盖：同步 → 标记 → 移动（占位认领、正文保留）→ 删除（垃圾箱 / 彻底删除）→ 发信（存「已发送」不重复）
//!      → 回复加 \Answered → IDLE 实时新邮件 → 按需取正文 → 导出 mbox 再导入。
//!
//! GreenMail 的拉起 / 重置 / 测试账号见 tests/common/mod.rs。

use easymail::core::db::Store;
use easymail::core::engine::{Engine, Evt};
use easymail::core::imap::Imap;
use easymail::core::mailer::{self, Outgoing};
use easymail::core::ops::{self, FlagChange};
use easymail::core::queries::{self, ListParams, View};
use easymail::core::{accounts, smtp, transfer};
use futures::executor::block_on;
use futures::StreamExt;
use std::time::{Duration, Instant};

mod common;
use common::{account, ensure_greenmail, reset, serial};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

fn deliver(rt: &tokio::runtime::Runtime, from: &str, to: &str, subject: &str, body: &str, extra: &str) -> String {
    let mid = format!("<{}@local.test>", uuid::Uuid::new_v4());
    let raw = format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: {mid}\r\nDate: Thu, 06 Nov 2025 10:00:00 +0800\r\nMIME-Version: 1.0\r\n{extra}Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    );
    rt.block_on(smtp::send(&account(from), "pw", None, &[to.to_string()], raw.as_bytes())).unwrap();
    mid
}

fn ids(store: &Store, p: ListParams) -> Vec<i64> {
    queries::list(store, &p, None, 200).unwrap().items.iter().map(|i| i.id).collect()
}

fn folder_id(store: &Store, acc: i64, role: &str) -> i64 {
    store.r().query_row("SELECT id FROM folders WHERE account_id = ? AND role = ?", rusqlite::params![acc, role], |r| r.get(0)).unwrap()
}

/// 用独立连接看服务器上的真实状态
fn server_uids(rt: &tokio::runtime::Runtime, email: &str, path: &str, query: &str) -> Vec<u32> {
    rt.block_on(async {
        let mut c = Imap::connect(&account(email), "pw", None).await.unwrap();
        c.select(path).await.unwrap();
        let v = c.uid_search(query).await.unwrap();
        c.logout().await;
        v
    })
}

fn wait_evt(rx: &mut futures::channel::mpsc::UnboundedReceiver<Evt>, secs: u64, mut pred: impl FnMut(&Evt) -> bool) -> Option<Evt> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    block_on(async {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let timer = futures_timer(left);
            futures::pin_mut!(timer);
            match futures::future::select(rx.next(), timer).await {
                futures::future::Either::Left((Some(e), _)) => {
                    if pred(&e) {
                        return Some(e);
                    }
                }
                _ => return None,
            }
        }
    })
}

fn futures_timer(d: Duration) -> impl std::future::Future<Output = ()> {
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(d);
        let _ = tx.send(());
    });
    async move {
        let _ = rx.await;
    }
}

#[test]
fn end_to_end_against_greenmail() {
    let _g = serial();
    if !ensure_greenmail() {
        return;
    }
    reset();
    let rt = rt();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let me = accounts::create(&store, &account("me@local.test"), "pw", "我", "").unwrap();
    // 账号连接测试
    assert_eq!(rt.block_on(easymail::core::imap::test_login(&me, "pw", None)).unwrap(), 1);
    rt.block_on(smtp::test_login(&me, "pw", None)).unwrap();
    assert!(rt.block_on(easymail::core::imap::test_login(&me, "wrong", None)).is_err());

    // 服务器上建好系统文件夹（GreenMail 的分隔符是 "."）
    rt.block_on(async {
        let mut c = Imap::connect(&me, "pw", None).await.unwrap();
        for f in ["Trash", "Sent", "Junk", "项目", "项目.二期"] {
            c.create_folder(f).await.unwrap();
        }
        c.logout().await;
    });
    let m1 = deliver(&rt, "bob@local.test", "me@local.test", "发票已开具 第一封", "增值税发票请查收", "");
    deliver(&rt, "bob@local.test", "me@local.test", "Weekly report", "numbers", "");
    deliver(&rt, "bob@local.test", "me@local.test", "带引用", "> quoted\nreply", "");

    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    // 端到端测试不需要撤销窗口：移动 / 删除立即送达服务器并等结果
    easymail::core::pending::UNDO_MS.store(0, std::sync::atomic::Ordering::Relaxed);
    let eng = Engine::new(store.clone(), tx).unwrap();

    // ① 首次同步：文件夹 + 信封 + 正文预取
    let added = block_on(eng.sync_and_wait(me.id, None, true)).unwrap();
    assert_eq!(added, 3);
    let folders = queries::folders(&store).unwrap();
    let paths: Vec<(&str, &str)> = folders.iter().map(|f| (f.path.as_str(), f.role.as_str())).collect();
    assert!(
        paths.contains(&("INBOX", "inbox")) && paths.contains(&("Trash", "trash")) && paths.contains(&("Sent", "sent")) && paths.contains(&("项目.二期", "")),
        "{paths:?}"
    );
    let sub = folders.iter().find(|f| f.path == "项目.二期").unwrap();
    assert_eq!((sub.delimiter.as_str(), sub.leaf().as_str()), (".", "二期"));
    let inbox = folder_id(&store, me.id, "inbox");
    let trash = folder_id(&store, me.id, "trash");
    let sent = folder_id(&store, me.id, "sent");
    let list = ids(&store, ListParams::default());
    assert_eq!(list.len(), 3);
    let with_body: i64 = store.r().query_row("SELECT count(*) FROM messages WHERE body_state = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(with_body, 3, "正文已预取");
    assert_eq!(ids(&store, ListParams { view: View::All, q: "增值税".into(), ..Default::default() }).len(), 1, "全文搜索命中正文");
    let st = queries::stats(&store).unwrap();
    assert_eq!((st.inbox, st.unread), (3, 3));
    let first: i64 = store.r().query_row("SELECT id FROM messages WHERE message_id = ?", [&m1], |r| r.get(0)).unwrap();

    // ② 标记已读 / 星标 → 服务器同步
    block_on(eng.call(ops::set_flags(eng.clone(), vec![first], FlagChange { seen: Some(true), flagged: Some(true) }))).unwrap();
    assert_eq!(server_uids(&rt, "me@local.test", "INBOX", "SEEN").len(), 1);
    assert_eq!(server_uids(&rt, "me@local.test", "INBOX", "FLAGGED").len(), 1);
    assert_eq!(queries::stats(&store).unwrap().inbox, 2, "本地未读数立即更新");
    // 服务器端改标记（模拟其它客户端）→ 增量同步拉回来
    rt.block_on(async {
        let mut c = Imap::connect(&me, "pw", None).await.unwrap();
        c.select("INBOX").await.unwrap();
        let all = c.uid_search("ALL").await.unwrap();
        c.store_flag(&all, "\\Seen", true).await.unwrap();
        c.logout().await;
    });
    block_on(eng.sync_and_wait(me.id, Some(inbox), false)).unwrap();
    assert_eq!(queries::stats(&store).unwrap().unread, 0, "外部改的已读状态同步到本地");

    // ③ 删除 → 移入垃圾箱：本地行保留 id 与正文，目标夹同步后认领新 UID
    let r = block_on(eng.call(ops::delete_messages(eng.clone(), vec![first]))).unwrap();
    assert_eq!(r.trashed, 1);
    let (fid, uid, body): (i64, i64, i64) =
        store.r().query_row("SELECT folder_id, uid, body_state FROM messages WHERE id = ?", [first], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!(fid, trash);
    assert_eq!(server_uids(&rt, "me@local.test", "Trash", "ALL").len(), 1);
    assert_eq!(server_uids(&rt, "me@local.test", "INBOX", "ALL").len(), 2);
    block_on(eng.sync_and_wait(me.id, Some(trash), false)).unwrap();
    let (uid2, body2): (i64, i64) = store.r().query_row("SELECT uid, body_state FROM messages WHERE id = ?", [first], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert!(uid < 0 || uid2 > 0, "占位 uid {uid} → {uid2}");
    assert!(uid2 > 0 && body == 1 && body2 == 1, "认领后保留正文");
    // 再删一次 → 彻底删除
    let r = block_on(eng.call(ops::delete_messages(eng.clone(), vec![first]))).unwrap();
    assert_eq!(r.purged, 1);
    assert!(server_uids(&rt, "me@local.test", "Trash", "ALL").is_empty(), "服务器已 expunge");
    assert!(queries::detail(&store, first).unwrap().is_none());

    // ④ 移动到自定义文件夹 + 服务器端删除被同步
    let second: i64 = store.r().query_row("SELECT id FROM messages WHERE subject = 'Weekly report'", [], |r| r.get(0)).unwrap();
    let proj = folders.iter().find(|f| f.path == "项目").unwrap().id;
    assert_eq!(block_on(eng.call(ops::move_messages(eng.clone(), vec![second], proj))).unwrap(), 1);
    block_on(eng.sync_and_wait(me.id, None, false)).unwrap();
    assert_eq!(ids(&store, ListParams { folder_id: Some(proj), ..Default::default() }), vec![second]);
    rt.block_on(async {
        let mut c = Imap::connect(&me, "pw", None).await.unwrap();
        c.select("项目").await.unwrap();
        let all = c.uid_search("ALL").await.unwrap();
        c.delete_uids(&all).await.unwrap();
        c.logout().await;
    });
    block_on(eng.sync_and_wait(me.id, None, false)).unwrap();
    assert!(ids(&store, ListParams { folder_id: Some(proj), ..Default::default() }).is_empty(), "服务器端删除同步到本地");

    // ⑤ 发信：回复 + 附件；存「已发送」只存一份；原信加 \Answered
    let third: i64 = store.r().query_row("SELECT id FROM messages WHERE subject = '带引用'", [], |r| r.get(0)).unwrap();
    let att_file = dir.path().join("附件.txt");
    std::fs::write(&att_file, "附件内容").unwrap();
    let att = mailer::add_upload(&store, &att_file).unwrap();
    let res = block_on(eng.call(mailer::send(
        eng.clone(),
        Outgoing {
            account_id: me.id,
            mode: "reply".into(),
            to: "Bob <bob@local.test>".into(),
            subject: "Re: 带引用".into(),
            text: "收到\n> quoted".into(),
            attachments: vec![att.clone()],
            source_id: Some(third),
            ..Default::default()
        },
    )))
    .unwrap();
    assert!(res.saved_to_sent);
    assert_eq!(server_uids(&rt, "me@local.test", "Sent", "ALL").len(), 1, "已发送只有一份");
    assert_eq!(server_uids(&rt, "bob@local.test", "INBOX", "ALL").len(), 1, "对方收到");
    assert_eq!(server_uids(&rt, "me@local.test", "INBOX", "ANSWERED").len(), 1, "原信已加 \\Answered");
    assert!(!store.uploads_dir().join(&att.token).exists(), "发出后清理上传文件");
    block_on(eng.sync_and_wait(me.id, Some(sent), true)).unwrap();
    let sent_ids = ids(&store, ListParams { view: View::Sent, ..Default::default() });
    assert_eq!(sent_ids.len(), 1);
    let d = queries::detail(&store, sent_ids[0]).unwrap().unwrap();
    assert_eq!(d.subject, "Re: 带引用");
    assert_eq!(d.attachments.iter().filter(|a| !a.inline).count(), 1);
    let t = queries::detail(&store, third).unwrap().unwrap();
    assert!(t.answered);
    // 会话含当前这封，按时间从旧到新：原信在前，回复在后
    assert_eq!(t.thread.iter().map(|x| x.id).collect::<Vec<_>>(), vec![third, sent_ids[0]], "回复与原信归为一个会话");

    // ⑥ 按需取正文（清掉本地缓存再打开）
    let p = store.eml_abs(me.id, third);
    std::fs::remove_file(&p).unwrap();
    store.w().execute("UPDATE messages SET body_state = 0, text = NULL WHERE id = ?", [third]).unwrap();
    block_on(eng.ensure_body(third)).unwrap();
    assert!(p.exists());
    let dt = queries::detail(&store, third).unwrap().unwrap();
    assert!(dt.text.contains("reply"), "subject={} text={:?}", dt.subject, dt.text);

    // ⑦ IDLE 实时：监听器在线后投递新邮件，应收到 NewMail
    eng.start();
    assert!(wait_evt(&mut rx, 15, |e| matches!(e, Evt::Watch { mode: easymail::core::engine::WatchMode::Idle, .. })).is_some(), "监听器进入 IDLE");
    deliver(&rt, "bob@local.test", "me@local.test", "实时到达", "push", "");
    let e = wait_evt(&mut rx, 20, |e| matches!(e, Evt::NewMail { .. })).expect("收到新邮件事件");
    if let Evt::NewMail { count, subject, .. } = e {
        assert_eq!(count, 1);
        assert_eq!(subject, "实时到达");
    }

    // ⑧ 导出 mbox → 导入本地归档
    let mbox = dir.path().join("out.mbox");
    let ex = block_on(eng.call(transfer::export(
        eng.clone(),
        ListParams { view: View::All, ..Default::default() },
        transfer::ExportKind::Mbox,
        mbox.clone(),
        "全部".into(),
    )))
    .unwrap();
    assert!(ex.written >= 3 && ex.failed == 0, "{ex:?}");
    let im = transfer::import_file(&store, &mbox, "备份", |_, _, _| {}).unwrap();
    assert_eq!(im.imported, ex.written);
    let csv = dir.path().join("out.csv");
    block_on(eng.call(transfer::export(
        eng.clone(),
        ListParams { view: View::Sent, ..Default::default() },
        transfer::ExportKind::Csv,
        csv.clone(),
        "已发送".into(),
    )))
    .unwrap();
    assert!(std::fs::read_to_string(&csv).unwrap().contains("Re: 带引用"));
    let md = block_on(eng.call(transfer::export(eng.clone(), ListParams::default(), transfer::ExportKind::Maildir, dir.path().to_path_buf(), "收件箱".into())))
        .unwrap();
    assert_eq!(std::fs::read_dir(md.path.join("cur")).unwrap().count(), md.written);

    // 本地归档里的操作只改本地
    let local_ids = ids(&store, ListParams { folder_id: Some(im.folder_id), ..Default::default() });
    block_on(eng.call(ops::set_flags(eng.clone(), local_ids.clone(), FlagChange { seen: Some(false), flagged: None }))).unwrap();
    assert_eq!(ids(&store, ListParams { folder_id: Some(im.folder_id), filter: queries::Filter::Unread, ..Default::default() }).len(), local_ids.len());

    // ⑨ 邮箱账号的文件夹管理：新建（子文件夹用服务器分隔符）→ 重命名 → 删除，服务器上实时生效
    let server_paths = |rt: &tokio::runtime::Runtime| -> Vec<String> {
        rt.block_on(async {
            let mut c = Imap::connect(&me, "pw", None).await.unwrap();
            let v = c.list_folders().await.unwrap().into_iter().map(|f| f.path).collect();
            c.logout().await;
            v
        })
    };
    let parent = store.r().query_row("SELECT id FROM folders WHERE account_id = ? AND path = '项目'", [me.id], |r| r.get::<_, i64>(0)).unwrap();
    let nf = block_on(eng.call(ops::create_folder_any(eng.clone(), me.id, "三期".into(), Some(parent)))).unwrap();
    assert!(server_paths(&rt).contains(&"项目.三期".to_string()));
    assert!(block_on(eng.call(ops::create_folder_any(eng.clone(), me.id, "三期".into(), Some(parent)))).is_err(), "同名文件夹应拒绝");
    block_on(eng.call(ops::rename_folder_any(eng.clone(), nf, "四期".into()))).unwrap();
    let sp = server_paths(&rt);
    assert!(sp.contains(&"项目.四期".to_string()) && !sp.contains(&"项目.三期".to_string()), "{sp:?}");
    let local_path: String = store.r().query_row("SELECT path FROM folders WHERE id = ?", [nf], |r| r.get(0)).unwrap();
    assert_eq!(local_path, "项目.四期");
    let inbox = folder_id(&store, me.id, "inbox");
    assert!(block_on(eng.call(ops::delete_folder_any(eng.clone(), inbox))).is_err(), "系统文件夹不能删");
    block_on(eng.call(ops::delete_folder_any(eng.clone(), nf))).unwrap();
    assert!(!server_paths(&rt).contains(&"项目.四期".to_string()));
    assert!(store.r().query_row("SELECT id FROM folders WHERE id = ?", [nf], |r| r.get::<_, i64>(0)).is_err());

    // ⑩ 发件箱：到点的信由 tick 发出；撤回的信不发；发出后出队
    use easymail::core::{db::now_ms, outbox};
    let before = server_uids(&rt, "bob@local.test", "INBOX", "ALL").len();
    let mk = |subject: &str| Outgoing {
        account_id: me.id,
        mode: "new".into(),
        to: "bob@local.test".into(),
        subject: subject.into(),
        text: "outbox".into(),
        ..Default::default()
    };
    let due = outbox::enqueue(&store, &mk("发件箱-到点"), Some(now_ms() - 1)).unwrap();
    let later = outbox::enqueue(&store, &mk("发件箱-撤回"), Some(now_ms() + 3_600_000)).unwrap();
    outbox::cancel(&store, later).unwrap();
    let future = outbox::enqueue(&store, &mk("发件箱-定时"), Some(now_ms() + 3_600_000)).unwrap();
    let e2 = eng.clone();
    assert_eq!(block_on(eng.call(async move { Ok(outbox::tick(&e2).await) })).unwrap(), 1);
    assert_eq!(server_uids(&rt, "bob@local.test", "INBOX", "ALL").len(), before + 1, "只发出到点的一封");
    let left: Vec<i64> = outbox::list(&store).unwrap().iter().map(|i| i.id).collect();
    assert_eq!(left, vec![future], "撤回的 id 不会被复用（AUTOINCREMENT）");
    assert!(future > later && later > due);
    eng.stop_account(me.id);
}

fn pending(store: &Store) -> i64 {
    store.r().query_row("SELECT count(*) FROM pending_ops", [], |r| r.get(0)).unwrap()
}

fn row(store: &Store, id: i64) -> Option<(i64, i64, bool)> {
    store.r().query_row("SELECT folder_id, uid, seen FROM messages WHERE id = ?", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0))).ok()
}

/// 远端失败路径（P0-2）：离线时的删除 / 移动 / 标记不能丢信，联网后自动送达；服务器明确拒绝时回滚本地。
#[test]
fn offline_ops_are_queued_and_rejections_roll_back() {
    let _g = serial();
    if !ensure_greenmail() {
        return;
    }
    reset();
    let rt = rt();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let me = accounts::create(&store, &account("me@local.test"), "pw", "我", "").unwrap();
    rt.block_on(async {
        let mut c = Imap::connect(&me, "pw", None).await.unwrap();
        c.create_folder("Trash").await.unwrap();
        c.append("Trash", "", b"From: bob@local.test\r\nSubject: old-trash\r\nMessage-ID: <old-trash@local.test>\r\n\r\nx\r\n").await.unwrap();
        c.logout().await;
    });
    let ma = deliver(&rt, "bob@local.test", "me@local.test", "A 要删", "a", "");
    let mb = deliver(&rt, "bob@local.test", "me@local.test", "B 要移动", "b", "");
    let mc = deliver(&rt, "bob@local.test", "me@local.test", "C 要已读", "c", "");
    let (tx, _rx) = futures::channel::mpsc::unbounded();
    // 端到端测试不需要撤销窗口：移动 / 删除立即送达服务器并等结果
    easymail::core::pending::UNDO_MS.store(0, std::sync::atomic::Ordering::Relaxed);
    let eng = Engine::new(store.clone(), tx).unwrap();
    block_on(eng.sync_and_wait(me.id, None, false)).unwrap();
    let inbox = folder_id(&store, me.id, "inbox");
    let trash = folder_id(&store, me.id, "trash");
    let id_of = |mid: &str| -> i64 { store.r().query_row("SELECT id FROM messages WHERE message_id = ?", [mid], |r| r.get(0)).unwrap() };
    let (a, b, c, d) = (id_of(&ma), id_of(&mb), id_of(&mc), id_of("<old-trash@local.test>"));

    // ① 断网：把账号指到一个没人监听的端口
    store.w().execute("UPDATE accounts SET imap_port = 1 WHERE id = ?", [me.id]).unwrap();
    eng.restart_account(me.id);
    let r = block_on(eng.call(ops::delete_messages(eng.clone(), vec![a, d]))).expect("离线删除：本地先生效，不报错");
    assert_eq!((r.trashed, r.purged), (1, 1));
    block_on(eng.call(ops::set_flags(eng.clone(), vec![c], FlagChange { seen: Some(true), flagged: None }))).expect("离线标记");
    assert_eq!(row(&store, a).map(|r| r.0), Some(trash), "A 已挂到垃圾箱（占位）");
    assert!(row(&store, d).is_none(), "D 本地已删除");
    assert!(row(&store, c).unwrap().2, "C 本地已读");
    assert_eq!(pending(&store), 3, "三条待办留在队列里");
    assert_eq!(server_uids(&rt, "me@local.test", "INBOX", "ALL").len(), 3, "服务器还没变");
    // 断网期间的同步不会把占位当成「服务器上没有」清掉
    let _ = block_on(eng.sync_and_wait(me.id, None, false));
    assert_eq!(row(&store, a).map(|r| r.0), Some(trash));

    // ② 恢复网络 → 重放
    store.w().execute("UPDATE accounts SET imap_port = ? WHERE id = ?", rusqlite::params![common::IMAP, me.id]).unwrap();
    eng.restart_account(me.id);
    block_on(eng.sync_and_wait(me.id, None, false)).unwrap();
    assert_eq!(pending(&store), 0, "待办全部送达");
    assert_eq!(server_uids(&rt, "me@local.test", "INBOX", "ALL").len(), 2, "A 已移出收件箱");
    assert_eq!(server_uids(&rt, "me@local.test", "INBOX", "SEEN").len(), 1, "C 已读");
    let trash_on_server = server_uids(&rt, "me@local.test", "Trash", "ALL");
    assert_eq!(trash_on_server.len(), 1, "D 已彻底删除、A 在垃圾箱：{trash_on_server:?}");
    let (fa, ua, _) = row(&store, a).unwrap();
    assert!(fa == trash && ua > 0, "A 的占位被认领：folder={fa} uid={ua}");

    // ③ 服务器明确拒绝（目标文件夹在服务器上不存在）→ 回滚到原文件夹、原 UID
    let (_, ub, _) = row(&store, b).unwrap();
    store.w().execute("INSERT INTO folders(account_id, path, name, delimiter, role, sync_enabled) VALUES(?, 'Ghost', 'Ghost', '.', '', 1)", [me.id]).unwrap();
    let ghost: i64 = store.r().query_row("SELECT id FROM folders WHERE path = 'Ghost'", [], |r| r.get(0)).unwrap();
    let e = block_on(eng.call(ops::move_messages(eng.clone(), vec![b], ghost))).expect_err("服务器拒绝要报错");
    assert!(e.to_string().contains("已恢复"), "{e}");
    assert_eq!(row(&store, b).map(|r| (r.0, r.1)), Some((inbox, ub)), "B 回到收件箱");
    assert_eq!(pending(&store), 0);
    eng.stop_account(me.id);
}
