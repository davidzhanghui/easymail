//! 同步引擎：独立的 tokio 运行时，每个账号一个串行 worker + 一个 IDLE 监听器 + 一条读信连接。
//!
//! ```text
//!   界面 ──ops/sync 请求──▶ Worker(账号)  ── 一条 IMAP 连接，串行执行；同步过程中穿插处理用户操作
//!                          Watcher(账号) ── 常驻 INBOX 做 IDLE（不支持则 45 秒 NOOP），有动静就请求同步
//!                          Reader(账号)  ── 打开邮件时现拉正文，不排在长同步后面
//!   Engine ──Evt──▶ 界面（同步状态、文件夹变化、新邮件、提示）
//! ```
//!
//! 增量同步与 just-mail 相同：
//!   ① 文件夹列表 ② UID > last_uid 的新信封（首次只拉最近 N 封） ③ 标记（CONDSTORE 增量）+ 服务器端删除
//!   ④ 每个文件夹最新 N 封预取正文 ⑤ 空闲时按 500 封一批回填更早的信封

use super::accounts;
use super::db::{now_ms, Store};
use super::error::{kind_of, ErrorKind};
use super::imap::{Flags, Imap};
use super::messages::{self, FolderRef};
use super::model::{Account, Folder};
use super::net::Proxy;
use super::pending::{self, Outcome, Pending, Task};
use super::providers::{self, folder_role, role_rank};
use anyhow::{anyhow, bail, Result};
use futures::channel::mpsc::UnboundedSender;
use rusqlite::params;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

type StopRx = watch::Receiver<bool>;

async fn stopped(mut rx: StopRx) {
    let _ = rx.wait_for(|v| *v).await;
}

#[derive(Clone, Debug, PartialEq)]
pub enum WatchMode {
    Off,
    Idle,
    Poll,
}

#[derive(Clone, Debug)]
pub enum Evt {
    /// 同步状态（syncing / 当前文件夹 / 错误）
    Sync {
        account_id: i64,
        syncing: bool,
        folder: String,
        error: String,
    },
    /// 某个文件夹的数据变了（列表 / 侧栏需要刷新）
    Folder {
        account_id: i64,
        folder_id: i64,
        added: usize,
    },
    /// 一次同步结束
    Synced {
        account_id: i64,
        added: usize,
    },
    /// 新邮件到达（来自实时监听）
    NewMail {
        account_id: i64,
        email: String,
        count: usize,
        from: String,
        subject: String,
    },
    Watch {
        account_id: i64,
        mode: WatchMode,
    },
    Accounts,
    Toast {
        msg: String,
        error: bool,
    },
}

#[derive(Clone, Debug, Default)]
pub struct SyncState {
    pub syncing: bool,
    pub folder: String,
    pub error: String,
}

/// worker 上直接执行（不进持久化队列）的远端写操作。
/// 标记 / 移动 / 删除走 `pending_ops` 队列（见 `pending.rs`），这里只剩带原文字节、不适合落库的追加类操作。
#[derive(Debug, Clone)]
pub enum Op {
    /// 发信后存「已发送」：先按 Message-ID 查，服务商已自动存了就跳过
    SaveSent {
        path: String,
        message_id: String,
        raw: Vec<u8>,
    },
    Append {
        path: String,
        flags: String,
        raw: Vec<u8>,
    },
}

impl Op {
    /// 重复执行结果不变（可以放心自动重试）
    fn idempotent(&self) -> bool {
        match self {
            // 先按 Message-ID 查、查到就跳过，重试不会存两份
            Op::SaveSent { .. } => true,
            // 响应超时时可能已经存进去了
            Op::Append { .. } => false,
        }
    }
}

struct OpJob {
    op: Op,
    reply: oneshot::Sender<Result<()>>,
}

/// 发给账号 worker 的用户请求（优先于同步处理）
enum Job {
    Op(OpJob),
    /// 重放持久化队列，返回这些待办 id 的结果
    Flush {
        ids: Vec<i64>,
        reply: oneshot::Sender<HashMap<i64, Outcome>>,
    },
}

/// 后台重放（同步过程中顺带执行）时跳过刚写入、还没试过的待办：它们的提交者马上会发 Flush 并亲自报告结果，
/// 免得同一条「被拒绝」既在后台弹一次提示、又在提交处报一次错
const FRESH_MS: i64 = 3_000;

#[derive(Default)]
struct SyncJob {
    only_folder: Option<i64>,
    extras: bool,
    reply: Vec<oneshot::Sender<Result<usize>>>,
}

struct Handle {
    ops: mpsc::UnboundedSender<Job>,
    syncs: mpsc::UnboundedSender<SyncJob>,
    stop: watch::Sender<bool>,
    reader: Arc<tokio::sync::Mutex<Option<Imap>>>,
    watching: bool,
}

/// 按账号的失败退避：连接类错误按 1 → 2 → 5 → 15 → 60 分钟拉长自动重试的间隔；
/// 认证类错误（密码 / 授权码错）直接停止自动重试，等用户改了账号再说，免得被服务商临时封禁。
/// 只约束「自动」行为（定时同步、实时监听重连），用户手动点同步不受限制。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Backoff {
    pub fails: u32,
    /// 这个时间点（毫秒）之前不自动重试
    pub until: i64,
    pub auth: bool,
}

const BACKOFF_MIN: [i64; 5] = [1, 2, 5, 15, 60];

impl Backoff {
    pub fn fail(&mut self, kind: ErrorKind, now: i64) {
        if kind == ErrorKind::Auth {
            self.auth = true;
            return;
        }
        self.fails += 1;
        let step = BACKOFF_MIN[(self.fails as usize - 1).min(BACKOFF_MIN.len() - 1)];
        self.until = now + step * 60_000;
    }

    pub fn blocked(&self, now: i64) -> bool {
        self.auth || now < self.until
    }
}

struct Inner {
    store: Store,
    backoff: Mutex<HashMap<i64, Backoff>>,
    /// 只在 Drop 里取走；其余时候一定是 Some
    rt: Option<tokio::runtime::Runtime>,
    evt: UnboundedSender<Evt>,
    handles: Mutex<HashMap<i64, Handle>>,
    states: Mutex<HashMap<i64, SyncState>>,
    watch: Mutex<HashMap<i64, WatchMode>>,
    started: Mutex<bool>,
}

impl Inner {
    fn rt(&self) -> &tokio::runtime::Runtime {
        self.rt.as_ref().expect("runtime")
    }
}

impl Drop for Inner {
    /// 后台任务各自持有 Engine 的克隆，最后一个引用可能在 runtime 自己的工作线程上释放；
    /// 普通的 drop(Runtime) 在那里会 panic，shutdown_background 不阻塞、在哪都能调用
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

impl Engine {
    pub fn new(store: Store, evt: UnboundedSender<Evt>) -> Result<Engine> {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(3).enable_all().thread_name("easymail-sync").build()?;
        Ok(Engine {
            inner: Arc::new(Inner {
                store,
                backoff: Mutex::new(HashMap::new()),
                rt: Some(rt),
                evt,
                handles: Mutex::new(HashMap::new()),
                states: Mutex::new(HashMap::new()),
                watch: Mutex::new(HashMap::new()),
                started: Mutex::new(false),
            }),
        })
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn emit(&self, e: Evt) {
        let _ = self.inner.evt.unbounded_send(e);
    }

    pub fn toast(&self, msg: impl Into<String>, error: bool) {
        self.emit(Evt::Toast { msg: msg.into(), error });
    }

    /// 在引擎运行时上跑一个 future，返回可在任意执行器上 await 的结果
    pub fn call<T: Send + 'static>(&self, fut: impl Future<Output = Result<T>> + Send + 'static) -> impl Future<Output = Result<T>> + Send + 'static {
        let (tx, rx) = oneshot::channel();
        self.inner.rt().spawn(async move {
            let _ = tx.send(fut.await);
        });
        async move { rx.await.unwrap_or_else(|_| Err(anyhow!("后台任务中断"))) }
    }

    pub fn spawn(&self, fut: impl Future<Output = ()> + Send + 'static) {
        self.inner.rt().spawn(fut);
    }

    pub fn proxy(&self) -> Option<Proxy> {
        let url = self.store().settings().proxy_url;
        if url.trim().is_empty() {
            return None;
        }
        Proxy::parse(&url).ok()
    }

    pub fn sync_state(&self, id: i64) -> SyncState {
        self.inner.states.lock().unwrap().get(&id).cloned().unwrap_or_default()
    }

    pub fn watch_mode(&self, id: i64) -> WatchMode {
        self.inner.watch.lock().unwrap().get(&id).cloned().unwrap_or(WatchMode::Off)
    }

    fn set_state(&self, id: i64, syncing: bool, folder: &str, error: &str) {
        let s = SyncState { syncing, folder: folder.into(), error: error.into() };
        self.inner.states.lock().unwrap().insert(id, s);
        self.emit(Evt::Sync { account_id: id, syncing, folder: folder.into(), error: error.into() });
    }

    pub fn backoff(&self, id: i64) -> Backoff {
        self.inner.backoff.lock().unwrap().get(&id).cloned().unwrap_or_default()
    }

    fn record_failure(&self, id: i64, e: &anyhow::Error) {
        let mut m = self.inner.backoff.lock().unwrap();
        let b = m.entry(id).or_default();
        b.fail(kind_of(e), now_ms());
        log::info!("[backoff] 账号 {id}：连续失败 {} 次，认证失败={}，{} 秒内不自动重试", b.fails, b.auth, (b.until - now_ms()).max(0) / 1000);
    }

    fn clear_backoff(&self, id: i64) {
        self.inner.backoff.lock().unwrap().remove(&id);
    }

    fn set_watch(&self, id: i64, mode: WatchMode) {
        let changed = self.inner.watch.lock().unwrap().insert(id, mode.clone()).as_ref() != Some(&mode);
        if changed {
            self.emit(Evt::Watch { account_id: id, mode });
        }
    }

    // ---------- 生命周期 ----------

    /// 启动：为每个启用同步的账号起 worker + 监听器，并开定时全量同步
    pub fn start(&self) {
        {
            let mut s = self.inner.started.lock().unwrap();
            if *s {
                return;
            }
            *s = true;
        }
        for a in accounts::list(self.store()).unwrap_or_default() {
            if !a.is_local() && a.sync_enabled {
                self.ensure_account(a.id, true);
            }
        }
        let me = self.clone();
        self.spawn(async move {
            loop {
                me.scheduler_tick();
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    fn scheduler_tick(&self) {
        let interval = (self.store().settings().sync_interval_min.max(1) as i64) * 60_000;
        for a in accounts::list(self.store()).unwrap_or_default() {
            if a.is_local() || !a.sync_enabled || self.sync_state(a.id).syncing || self.backoff(a.id).blocked(now_ms()) {
                continue;
            }
            if a.last_sync_at.is_none_or(|t| now_ms() - t >= interval) {
                self.request_sync(a.id, None, true);
            }
        }
    }

    /// 账号新增 / 修改连接参数后调用：重建 worker 与监听器
    pub fn restart_account(&self, id: i64) {
        self.stop_account(id);
        // 用户改了密码 / 代理：之前的失败记录作废，立刻按新参数重试
        self.clear_backoff(id);
        if let Ok(Some(a)) = accounts::get(self.store(), id) {
            if !a.is_local() && a.sync_enabled {
                self.ensure_account(id, true);
                self.request_sync(id, None, true);
            }
        }
    }

    pub fn stop_account(&self, id: i64) {
        if let Some(h) = self.inner.handles.lock().unwrap().remove(&id) {
            let _ = h.stop.send(true);
        }
        self.set_watch(id, WatchMode::Off);
        self.inner.states.lock().unwrap().remove(&id);
    }

    fn ensure_account(&self, id: i64, watcher: bool) -> (mpsc::UnboundedSender<Job>, mpsc::UnboundedSender<SyncJob>) {
        let mut hs = self.inner.handles.lock().unwrap();
        let alive = hs.get(&id).is_some_and(|h| !h.ops.is_closed());
        if !alive {
            let (otx, orx) = mpsc::unbounded_channel();
            let (stx, srx) = mpsc::unbounded_channel();
            let (stop, stop_rx) = watch::channel(false);
            let w = Worker { eng: self.clone(), acc_id: id, conn: None, ops: orx, syncs: srx, stop: stop_rx, recent: HashMap::new() };
            self.inner.rt().spawn(w.run());
            hs.insert(id, Handle { ops: otx, syncs: stx, stop, reader: Arc::new(tokio::sync::Mutex::new(None)), watching: false });
        }
        let h = hs.get_mut(&id).expect("handle");
        if watcher && !h.watching {
            h.watching = true;
            let me = self.clone();
            let rx = h.stop.subscribe();
            self.inner.rt().spawn(async move { me.watch_loop(id, rx).await });
        }
        (h.ops.clone(), h.syncs.clone())
    }

    // ---------- 请求 ----------

    /// 请求同步（不等待）
    pub fn request_sync(&self, id: i64, only_folder: Option<i64>, extras: bool) {
        let (_, s) = self.ensure_account(id, false);
        let _ = s.send(SyncJob { only_folder, extras, reply: vec![] });
    }

    /// 请求同步并等待完成，返回新增封数
    pub fn sync_and_wait(&self, id: i64, only_folder: Option<i64>, extras: bool) -> impl Future<Output = Result<usize>> + Send + 'static {
        let (_, s) = self.ensure_account(id, false);
        let (tx, rx) = oneshot::channel();
        let sent = s.send(SyncJob { only_folder, extras, reply: vec![tx] }).is_ok();
        async move {
            if !sent {
                bail!("同步队列已关闭");
            }
            rx.await.unwrap_or_else(|_| Err(anyhow!("同步被中断")))
        }
    }

    pub fn sync_all(&self) {
        for a in accounts::list(self.store()).unwrap_or_default() {
            if !a.is_local() && a.sync_enabled {
                self.request_sync(a.id, None, true);
            }
        }
    }

    /// 在账号 worker 上执行远端写操作，等待结果
    pub fn remote(&self, id: i64, op: Op) -> impl Future<Output = Result<()>> + Send + 'static {
        let (o, _) = self.ensure_account(id, false);
        let (tx, rx) = oneshot::channel();
        let sent = o.send(Job::Op(OpJob { op, reply: tx })).is_ok();
        async move {
            if !sent {
                bail!("操作队列已关闭");
            }
            rx.await.unwrap_or_else(|_| Err(anyhow!("操作被中断")))
        }
    }

    /// 让账号 worker 重放持久化队列，返回 `ids` 这几条待办的结果（见 `pending.rs`）
    pub fn flush(&self, id: i64, ids: Vec<i64>) -> impl Future<Output = Result<HashMap<i64, Outcome>>> + Send + 'static {
        let (o, _) = self.ensure_account(id, false);
        let (tx, rx) = oneshot::channel();
        let sent = o.send(Job::Flush { ids, reply: tx }).is_ok();
        async move {
            if !sent {
                bail!("操作队列已关闭");
            }
            rx.await.map_err(|_| anyhow!("操作被中断"))
        }
    }

    /// 打开邮件时：正文没缓存就现拉（独立的读信连接，不排在同步后面）
    pub fn ensure_body(&self, msg_id: i64) -> impl Future<Output = Result<()>> + Send + 'static {
        let me = self.clone();
        self.call(async move { me.ensure_body_inner(msg_id).await })
    }

    async fn ensure_body_inner(&self, msg_id: i64) -> Result<()> {
        let store = self.store().clone();
        if messages::has_body(&store, msg_id) {
            return Ok(());
        }
        let row: Option<(i64, i64, String)> = store
            .r()
            .query_row("SELECT m.account_id, m.uid, f.path FROM messages m JOIN folders f ON f.id = m.folder_id WHERE m.id = ?", [msg_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .ok();
        let Some((acc_id, uid, path)) = row else { bail!("邮件不存在") };
        let acc = accounts::must(&store, acc_id)?;
        if acc.is_local() {
            return Ok(());
        }
        if uid <= 0 {
            bail!("邮件刚被移动，服务器还在处理，请稍后再打开");
        }
        let reader = {
            self.ensure_account(acc_id, false);
            self.inner.handles.lock().unwrap().get(&acc_id).map(|h| h.reader.clone())
        };
        let Some(reader) = reader else { bail!("账号未启动") };
        let mut guard = reader.lock().await;
        for attempt in 0..2 {
            if guard.is_none() {
                let pw = accounts::password(&store, &acc)?;
                *guard = Some(Imap::connect(&acc, &pw, self.proxy().as_ref()).await?);
            }
            let c = guard.as_mut().expect("conn");
            let r = async {
                c.select(&path).await?;
                c.fetch_raw(uid as u32).await
            }
            .await;
            match r {
                Ok(Some(raw)) => {
                    messages::store_body(&store, msg_id, &raw)?;
                    return Ok(());
                }
                Ok(None) => bail!("服务器上已找不到这封邮件（可能已在其它客户端删除）"),
                Err(e) => {
                    *guard = None;
                    if attempt == 1 {
                        return Err(e);
                    }
                }
            }
        }
        unreachable!()
    }

    /// 取原文：本地有就读本地，否则现拉
    pub fn source(&self, msg_id: i64) -> impl Future<Output = Result<Vec<u8>>> + Send + 'static {
        let me = self.clone();
        self.call(async move {
            if let Some(b) = messages::read_local_source(me.store(), msg_id) {
                return Ok(b);
            }
            me.ensure_body_inner(msg_id).await?;
            messages::read_local_source(me.store(), msg_id).ok_or_else(|| anyhow!("找不到邮件原文"))
        })
    }

    // ---------- 实时监听 ----------

    async fn watch_loop(&self, id: i64, stop: StopRx) {
        tokio::time::sleep(Duration::from_secs(3)).await;
        loop {
            // 退避期间（含密码错误）不连服务器，只在本地等
            let b = self.backoff(id);
            if b.blocked(now_ms()) {
                let wait = if b.auth { 30 } else { ((b.until - now_ms()) / 1000).clamp(1, 3600) as u64 };
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(wait)) => continue,
                    _ = stopped(stop.clone()) => return,
                }
            }
            let mut established = false;
            let delay = match self.watch_once(id, stop.clone(), &mut established).await {
                Ok(true) => return, // 停止
                Ok(false) => 5,
                // 已经连上并选中 INBOX 之后才断的：服务器 / 代理 / NAT 回收空闲连接，属预期内，
                // 不进退避（否则这个账号的自动同步会被挡 1 分钟），稍等即重连
                Err(e) if established && kind_of(&e) == ErrorKind::Connection => {
                    log::info!("[watch] {id}: 监听连接被对端断开，稍后重连：{e:#}");
                    5
                }
                Err(e) => {
                    log::warn!("[watch] {id}: {e:#}");
                    self.record_failure(id, &e);
                    60
                }
            };
            self.set_watch(id, WatchMode::Off);
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(delay)) => {}
                _ = stopped(stop.clone()) => return,
            }
        }
    }

    /// 返回 Ok(true) 表示被要求停止
    async fn watch_once(&self, id: i64, stop: StopRx, established: &mut bool) -> Result<bool> {
        let Some(acc) = accounts::get(self.store(), id)? else { return Ok(true) };
        if !acc.sync_enabled || acc.is_local() {
            return Ok(true);
        }
        let pw = accounts::password(self.store(), &acc)?;
        let mut c = Imap::connect(&acc, &pw, self.proxy().as_ref()).await?;
        c.select("INBOX").await?;
        *established = true;
        let idle = c.caps.idle;
        self.set_watch(id, if idle { WatchMode::Idle } else { WatchMode::Poll });
        // IDLE：每 5 分钟重进一次（代理 / NAT 会悄悄掐掉长时间无流量的连接）；不支持 IDLE：45 秒一次 NOOP
        let period = if idle { Duration::from_secs(300) } else { Duration::from_secs(45) };
        loop {
            let st = stop.clone();
            let halt = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = halt.clone();
            let (next, changed) = c
                .idle(period, async move {
                    stopped(st).await;
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                })
                .await?;
            c = next;
            if halt.load(std::sync::atomic::Ordering::SeqCst) {
                c.logout().await;
                return Ok(true);
            }
            if changed {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                let inbox: Option<i64> = self.store().r().query_row("SELECT id FROM folders WHERE account_id = ? AND role = 'inbox'", [id], |r| r.get(0)).ok();
                let n = self.sync_and_wait(id, inbox, false).await.unwrap_or(0);
                if n > 0 {
                    let (from, subject) = inbox
                        .and_then(|f| {
                            self.store()
                                .r()
                                .query_row(
                                    "SELECT CASE WHEN from_name != '' THEN from_name ELSE from_addr END, subject FROM messages WHERE folder_id = ? ORDER BY uid DESC LIMIT 1",
                                    [f],
                                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                                )
                                .ok()
                        })
                        .unwrap_or_default();
                    self.emit(Evt::NewMail { account_id: id, email: acc.email.clone(), count: n, from, subject });
                    self.request_sync(id, None, true);
                }
            }
        }
    }
}

// ======================= Worker =======================

struct Worker {
    eng: Engine,
    acc_id: i64,
    conn: Option<Imap>,
    ops: mpsc::UnboundedReceiver<Job>,
    syncs: mpsc::UnboundedReceiver<SyncJob>,
    stop: StopRx,
    /// 最近一批待办的结果：提交者的 Flush 到达前，待办可能已经被别的重放处理掉了
    recent: HashMap<i64, Outcome>,
}

impl Worker {
    fn store(&self) -> &Store {
        self.eng.store()
    }

    async fn run(mut self) {
        loop {
            tokio::select! {
                biased;
                _ = stopped(self.stop.clone()) => break,
                Some(job) = self.ops.recv() => self.handle(job).await,
                Some(job) = self.syncs.recv() => {
                    // 合并排队中的同步请求：任一要全量就做全量
                    let mut job = job;
                    while let Ok(more) = self.syncs.try_recv() {
                        if job.only_folder != more.only_folder {
                            job.only_folder = None;
                        }
                        job.extras |= more.extras;
                        job.reply.extend(more.reply);
                    }
                    let r = self.sync_account(job.only_folder, job.extras).await;
                    for tx in job.reply {
                        let _ = tx.send(r.as_ref().map(|n| *n).map_err(|e| anyhow!("{e:#}")));
                    }
                }
                else => break,
            }
        }
        if let Some(c) = self.conn.take() {
            c.logout().await;
        }
    }

    async fn conn(&mut self) -> Result<&mut Imap> {
        if self.conn.is_none() {
            let acc = accounts::must(self.store(), self.acc_id)?;
            if acc.is_local() {
                bail!("本地归档没有服务器");
            }
            let pw = accounts::password(self.store(), &acc)?;
            self.conn = Some(Imap::connect(&acc, &pw, self.eng.proxy().as_ref()).await?);
        }
        Ok(self.conn.as_mut().expect("conn"))
    }

    /// 同步过程中穿插执行用户操作，让界面操作不必等整轮同步结束；顺带重放积压的待办（比如离线时做的操作）
    async fn pump_ops(&mut self) {
        while let Ok(job) = self.ops.try_recv() {
            self.handle(job).await;
        }
        self.replay(true).await;
    }

    async fn handle(&mut self, job: Job) {
        match job {
            Job::Op(op) => self.exec(op).await,
            Job::Flush { ids, reply } => {
                self.replay(false).await;
                let out = ids.iter().map(|id| (*id, self.recent.get(id).cloned().unwrap_or(Outcome::Done))).collect();
                let _ = reply.send(out);
            }
        }
    }

    // ---------- 持久化队列 ----------

    /// 按顺序重放本账号的待办。连接类错误时停下（保持顺序，后面的也一定连不上），留到下次。
    /// `background`：同步途中顺带执行，跳过刚写入的（它们的提交者会亲自 Flush），被拒绝时由这里弹提示。
    async fn replay(&mut self, background: bool) {
        let store = self.store().clone();
        let list = match pending::list(&store, self.acc_id) {
            Ok(l) => l,
            Err(e) => {
                log::warn!("[pending] 读取待办失败：{e:#}");
                return;
            }
        };
        if list.is_empty() {
            return;
        }
        if self.recent.len() > 512 {
            self.recent.clear();
        }
        let fresh_after = now_ms() - FRESH_MS;
        let mut blocked: Option<String> = None;
        for p in list {
            if let Some(msg) = &blocked {
                // 前面一条连不上，后面的这一轮都不试了（保持顺序），同样算「稍后重试」。
                // 也记一次尝试：提交者已经拿到了结果，之后由后台重放接手，不再算「刚写入」
                let _ = pending::begin(&store, p.id);
                let _ = pending::deferred(&store, p.id, msg);
                self.recent.insert(p.id, Outcome::Deferred(msg.clone()));
                continue;
            }
            if background && p.attempts == 0 && p.created_at > fresh_after {
                break;
            }
            let _ = pending::begin(&store, p.id);
            let r = self.run_task(&p).await;
            let outcome = match r {
                Ok(()) => Outcome::Done,
                Err(e) if kind_of(&e) == ErrorKind::Rejected => {
                    let msg = format!("{e:#}");
                    log::warn!("[pending] {} 被服务器拒绝，回滚本地：{msg}", p.task.label());
                    if let Err(e2) = self.rollback(&p).await {
                        log::warn!("[pending] 回滚失败：{e2:#}");
                    }
                    if background {
                        self.eng.toast(format!("服务器拒绝了{}，本地已恢复原状：{msg}", p.task.label()), true);
                    }
                    Outcome::Rejected(msg)
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    log::info!("[pending] {} 暂未送达（第 {} 次），稍后重试：{msg}", p.task.label(), p.attempts + 1);
                    self.conn = None;
                    let _ = pending::deferred(&store, p.id, &msg);
                    self.recent.insert(p.id, Outcome::Deferred(msg.clone()));
                    blocked = Some(msg);
                    continue;
                }
            };
            let _ = pending::done(&store, p.id);
            self.recent.insert(p.id, outcome);
        }
    }

    async fn run_task(&mut self, p: &Pending) -> Result<()> {
        let retry = p.attempts > 0;
        let c = self.conn().await?;
        match &p.task {
            Task::Flag { path, uids, flag, add } => {
                c.ensure_selected(path).await?;
                c.store_flag(uids, flag, *add).await
            }
            Task::AllSeen { path } => {
                let info = c.select(path).await?;
                if info.exists > 0 {
                    let unseen = c.uid_search("UNSEEN").await?;
                    c.store_flag(&unseen, "\\Seen", true).await?;
                }
                Ok(())
            }
            Task::Delete { path, uids } => {
                c.ensure_selected(path).await?;
                c.delete_uids(uids).await
            }
            Task::Move { path, items, dest } => {
                let mut todo: Vec<(u32, String)> = items.clone();
                if retry {
                    // 上次可能做了一部分：只处理还留在源文件夹里的
                    c.ensure_selected(path).await?;
                    let uids: Vec<u32> = todo.iter().map(|t| t.0).collect();
                    let present: HashSet<u32> = c.uid_search(&format!("UID {}", super::imap::uid_set(&uids))).await?.into_iter().collect();
                    todo.retain(|t| present.contains(&t.0));
                    if todo.is_empty() {
                        return Ok(());
                    }
                    if !c.caps.mv {
                        // COPY + 删除的退化路径：目标夹里已经有同一 Message-ID 的，说明上次 COPY 成功了，只需删源
                        let mut copied = Vec::new();
                        for (u, mid) in &todo {
                            if !mid.is_empty() && !c.find_message_id(dest, mid).await?.is_empty() {
                                copied.push(*u);
                            }
                        }
                        c.select(path).await?;
                        if !copied.is_empty() {
                            c.delete_uids(&copied).await?;
                            todo.retain(|t| !copied.contains(&t.0));
                        }
                    }
                }
                c.ensure_selected(path).await?;
                let uids: Vec<u32> = todo.iter().map(|t| t.0).collect();
                c.move_uids(&uids, dest).await
            }
        }
    }

    /// 服务器明确拒绝：按 undo 把本地改回去
    async fn rollback(&mut self, p: &Pending) -> Result<()> {
        let store = self.store().clone();
        let u = &p.undo;
        let mut touched: HashSet<i64> = HashSet::new();
        let mut refetch: HashMap<i64, Vec<u32>> = HashMap::new();
        if u.refetch_folder > 0 && !u.refetch_uids.is_empty() {
            refetch.entry(u.refetch_folder).or_default().extend(&u.refetch_uids);
        }
        store.tx(|tx| {
            if matches!(u.column.as_str(), "seen" | "flagged" | "answered") {
                for (id, prev) in &u.flags {
                    let cur: Option<(i64, bool)> = tx
                        .query_row(&format!("SELECT folder_id, {} FROM messages WHERE id = ?", u.column), [id], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)))
                        .ok();
                    let Some((fid, cur)) = cur else { continue };
                    if cur == *prev {
                        continue;
                    }
                    tx.execute(&format!("UPDATE messages SET {} = ? WHERE id = ?", u.column), params![*prev as i64, id])?;
                    if u.column == "seen" {
                        tx.execute("UPDATE folders SET unseen = MAX(0, unseen + ?) WHERE id = ?", params![if *prev { -1 } else { 1 }, fid])?;
                    }
                    touched.insert(fid);
                }
            }
            for (id, src_folder, src_uid) in &u.moved {
                let cur: Option<(i64, i64)> = tx.query_row("SELECT folder_id, seen FROM messages WHERE id = ?", [id], |r| Ok((r.get(0)?, r.get(1)?))).ok();
                match cur {
                    Some((fid, seen)) if fid != *src_folder => {
                        tx.execute("UPDATE OR IGNORE messages SET folder_id = ?, uid = ? WHERE id = ?", params![src_folder, src_uid, id])?;
                        let unseen = (seen == 0) as i64;
                        tx.execute("UPDATE folders SET total = MAX(0, total - 1), unseen = MAX(0, unseen - ?) WHERE id = ?", params![unseen, fid])?;
                        tx.execute("UPDATE folders SET total = total + 1, unseen = unseen + ? WHERE id = ?", params![unseen, src_folder])?;
                        touched.extend([fid, *src_folder]);
                    }
                    Some(_) => {}
                    // 本地行已经没了（比如又被删了一次）：从服务器重新拉
                    None => refetch.entry(*src_folder).or_default().push(*src_uid as u32),
                }
            }
            Ok(())
        })?;
        for (fid, uids) in refetch {
            let path: Option<String> = store.r().query_row("SELECT path FROM folders WHERE id = ?", [fid], |r| r.get(0)).ok();
            let Some(path) = path else { continue };
            let c = self.conn().await?;
            c.select(&path).await?;
            let headers = c.fetch_headers(&uids).await?;
            let n = messages::insert_envelopes(&store, &FolderRef { id: fid, account_id: p.account_id }, &headers)?;
            store.w().execute("UPDATE folders SET total = total + ? WHERE id = ?", params![n, fid])?;
            touched.insert(fid);
        }
        for fid in touched {
            self.eng.emit(Evt::Folder { account_id: p.account_id, folder_id: fid, added: 0 });
        }
        Ok(())
    }

    /// 只有连接类错误才重连重试一次；服务器明确拒绝的不重试（重试也一样被拒），
    /// 不幂等的操作（Append：响应超时时可能已经存进去了）也不自动重试，免得存两份。
    async fn exec(&mut self, job: OpJob) {
        let mut r = self.exec_op(&job.op).await;
        if let Err(e) = &r {
            if kind_of(e) != ErrorKind::Rejected {
                self.conn = None;
                if job.op.idempotent() {
                    r = self.exec_op(&job.op).await;
                    if r.as_ref().is_err_and(|e| kind_of(e) != ErrorKind::Rejected) {
                        self.conn = None;
                    }
                }
            }
        }
        let _ = job.reply.send(r);
    }

    async fn exec_op(&mut self, op: &Op) -> Result<()> {
        let c = self.conn().await?;
        match op {
            Op::SaveSent { path, message_id, raw } => {
                // 等服务器把自动保存的副本落地再查（163 / QQ 偶尔慢半拍）
                for i in 0..3 {
                    if !message_id.is_empty() {
                        c.select(path).await?;
                        if !c.find_message_id(path, message_id).await?.is_empty() {
                            return Ok(());
                        }
                    }
                    if i < 2 {
                        tokio::time::sleep(Duration::from_millis(1200)).await;
                    }
                }
                c.append(path, "(\\Seen)", raw).await
            }
            Op::Append { path, flags, raw } => c.append(path, flags, raw).await,
        }
    }

    // ---------- 同步 ----------

    async fn sync_account(&mut self, only_folder: Option<i64>, extras: bool) -> Result<usize> {
        let id = self.acc_id;
        let Some(acc) = accounts::get(self.store(), id)? else { return Ok(0) };
        if acc.is_local() {
            return Ok(0);
        }
        self.eng.set_state(id, true, "", "");
        let reused = self.conn.is_some();
        let mut r = self.sync_inner(&acc, only_folder, extras).await;
        // 复用的旧连接可能早被服务器（163 等）悄悄断开：第一条命令就 Broken pipe。丢掉后用新连接重试一次
        if reused && matches!(&r, Err(e) if kind_of(e) == ErrorKind::Connection) {
            log::info!("[sync] {}: 旧连接已失效，重连后重试：{:#}", acc.email, r.as_ref().err().unwrap());
            self.conn = None;
            r = self.sync_inner(&acc, only_folder, extras).await;
        }
        let added = r.as_ref().map(|x| x.0).unwrap_or(0);
        match &r {
            Ok((_, warn)) => {
                self.eng.clear_backoff(id);
                if !warn.is_empty() {
                    log::warn!("[sync] {}: {warn}", acc.email);
                    accounts::set_error(self.store(), id, warn);
                }
                self.eng.set_state(id, false, "", warn)
            }
            Err(e) => {
                let msg = format!("{e:#}");
                log::warn!("[sync] {}: {msg}", acc.email);
                self.eng.record_failure(id, e);
                accounts::set_error(self.store(), id, &msg);
                self.eng.set_state(id, false, "", &msg);
                self.conn = None;
            }
        }
        self.eng.emit(Evt::Synced { account_id: id, added });
        r.map(|x| x.0)
    }

    /// 返回 (新增封数, 部分文件夹失败时的汇总提示)。
    /// 单个文件夹被服务器拒绝（删了 / 没权限）只记下来、继续同步其余文件夹；连接类错误直接中止（后面的也连不上）。
    async fn sync_inner(&mut self, acc: &Account, only_folder: Option<i64>, extras: bool) -> Result<(usize, String)> {
        // 单文件夹同步（IDLE 触发的那种）不做 LIST + 逐夹 STATUS，直接用库里的文件夹信息
        let known = if only_folder.is_some() { folders_of(self.store(), acc.id)? } else { vec![] };
        let mut folders = match only_folder {
            Some(fid) if known.iter().any(|f| f.id == fid) => known,
            _ => self.sync_folder_list(acc).await?,
        };
        if let Some(fid) = only_folder {
            folders.retain(|f| f.id == fid);
        }
        let mut added = 0;
        let mut failed: Vec<String> = Vec::new();
        for f in &folders {
            self.pump_ops().await;
            self.eng.set_state(acc.id, true, &f.label(), "");
            match self.sync_folder(f).await {
                Ok(n) => {
                    added += n;
                    self.eng.emit(Evt::Folder { account_id: acc.id, folder_id: f.id, added: n });
                }
                Err(e) if kind_of(&e) == ErrorKind::Rejected => failed.push(format!("「{}」：{e:#}", f.label())),
                Err(e) => return Err(e),
            }
        }
        {
            let c = self.store().w();
            if only_folder.is_none() {
                c.execute("UPDATE accounts SET last_sync_at = ?, last_error = '' WHERE id = ?", params![now_ms(), acc.id])?;
            } else {
                c.execute("UPDATE accounts SET last_error = '' WHERE id = ?", [acc.id])?;
            }
        }
        if extras {
            let settings = self.store().settings();
            for f in &folders {
                self.pump_ops().await;
                self.eng.set_state(acc.id, true, &format!("{} · 正文", f.label()), "");
                if let Err(e) = self.prefetch(acc, f, &settings).await {
                    log::warn!("[prefetch] {}: {e:#}", f.path);
                    self.conn = None;
                }
            }
            if settings.backfill {
                for f in &folders {
                    self.pump_ops().await;
                    self.eng.set_state(acc.id, true, &format!("{} · 回填", f.label()), "");
                    match self.backfill(f).await {
                        Ok(n) if n > 0 => self.eng.emit(Evt::Folder { account_id: acc.id, folder_id: f.id, added: n }),
                        Ok(_) => {}
                        Err(e) if kind_of(&e) == ErrorKind::Rejected => failed.push(format!("「{}」回填：{e:#}", f.label())),
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        let warn = if failed.is_empty() { String::new() } else { format!("{} 个文件夹同步失败：{}", failed.len(), failed.join("；")) };
        Ok((added, warn))
    }

    async fn sync_folder_list(&mut self, acc: &Account) -> Result<Vec<Folder>> {
        let remote = self.conn().await?.list_folders().await?;
        let skip = providers::provider(&acc.provider).skip_roles;
        let store = self.store().clone();
        let mut seen = HashSet::new();
        let selectable = remote.iter().filter(|b| !b.noselect).count();
        let mut orphan_rows: Vec<(i64, i64)> = Vec::new();
        store.tx(|tx| {
            for b in remote.iter().filter(|b| !b.noselect) {
                let sp: Vec<&str> = b.special.iter().map(|s| s.as_str()).collect();
                let role = folder_role(&b.path, &b.name, &sp);
                seen.insert(b.path.clone());
                tx.execute(
                    "INSERT INTO folders (account_id, path, name, delimiter, role, sync_enabled) VALUES (?,?,?,?,?,?)
                     ON CONFLICT(account_id, path) DO UPDATE SET name = excluded.name, delimiter = excluded.delimiter, role = excluded.role",
                    params![acc.id, b.path, b.name, b.delimiter, role, (!skip.contains(&role)) as i64],
                )?;
            }
            let existing: Vec<(i64, String)> = {
                let mut st = tx.prepare("SELECT id, path FROM folders WHERE account_id = ?")?;
                let v = st.query_map([acc.id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                v
            };
            let gone: Vec<i64> = existing.iter().filter(|(_, p)| !seen.contains(p)).map(|(id, _)| *id).collect();
            // LIST 偶发返回不完整时不能据此删本地文件夹（连同整夹的正文缓存）：结果为空或一下少了一半以上就先不删
            if !gone.is_empty() && (selectable == 0 || (existing.len() >= 4 && gone.len() * 2 > existing.len())) {
                log::warn!(
                    "[sync] {} 的文件夹列表明显缩水（服务器 {selectable} 个，本地 {} 个，缺 {} 个），本轮不删除本地文件夹",
                    acc.email,
                    existing.len(),
                    gone.len()
                );
                return Ok(());
            }
            for fid in gone {
                // 先删消息行（.eml 在事务提交后删），不要靠外键级联悄悄删掉，那样磁盘上的 .eml 会成为孤儿
                let mut st = tx.prepare("SELECT id, account_id FROM messages WHERE folder_id = ?")?;
                let rows = st.query_map([fid], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<(i64, i64)>, _>>()?;
                messages::delete_rows_in(tx, &rows)?;
                orphan_rows.extend(rows);
                tx.execute("DELETE FROM folders WHERE id = ?", [fid])?;
            }
            Ok(())
        })?;
        messages::remove_files(&store, &orphan_rows);
        let all = folders_of(&store, acc.id)?;
        // 未读数以服务器为准（本地可能只同步了一部分）
        for f in &all {
            if let Ok((total, unseen)) = self.conn().await?.status(&f.path).await {
                store.w().execute("UPDATE folders SET total = ?, unseen = ? WHERE id = ?", params![total, unseen, f.id])?;
            }
        }
        let mut v: Vec<Folder> = folders_of(&store, acc.id)?.into_iter().filter(|f| f.sync_enabled).collect();
        v.sort_by(|a, b| role_rank(&a.role).cmp(&role_rank(&b.role)).then(a.path.cmp(&b.path)));
        Ok(v)
    }

    async fn sync_folder(&mut self, folder: &Folder) -> Result<usize> {
        let store = self.store().clone();
        let settings = store.settings();
        // pump_ops 里失败的用户操作会把连接置空，这里按需重连，而不是让整轮同步报「连接已断开」
        let c = self.conn().await?;
        let info = c.select(&folder.path).await?;
        let uv = info.uidvalidity.to_string();
        let mut f = folder.clone();
        let fr = FolderRef { id: f.id, account_id: f.account_id };
        if !f.uidvalidity.is_empty() && f.uidvalidity != uv {
            // UIDVALIDITY 变了：服务器重建了 UID，本地这一夹作废重拉
            let rows = messages::ids_with_account(&store, "SELECT id, account_id FROM messages WHERE folder_id = ?", [f.id])?;
            messages::remove(&store, &rows)?;
            store.w().execute("UPDATE folders SET last_uid = 0, oldest_uid = 0, backfill_done = 0, modseq = '' WHERE id = ?", [f.id])?;
            f.last_uid = 0;
            f.oldest_uid = 0;
            f.backfill_done = false;
            f.modseq.clear();
        }
        if info.exists == 0 {
            let busy = pending::busy_ids(&store.w(), f.account_id)?;
            let mut rows = messages::ids_with_account(&store, "SELECT id, account_id FROM messages WHERE folder_id = ?", [f.id])?;
            rows.retain(|(id, _)| !busy.contains(id));
            messages::remove(&store, &rows)?;
            store.w().execute(
                "UPDATE folders SET uidvalidity = ?, total = 0, unseen = 0, synced_at = ?, backfill_done = 1 WHERE id = ?",
                params![uv, now_ms(), f.id],
            )?;
            return Ok(0);
        }

        // ② 新邮件
        let fresh: Vec<u32> = if f.last_uid == 0 {
            let all = c.uid_search("ALL").await?;
            let n = settings.initial_sync_per_folder.max(1) as usize;
            all[all.len().saturating_sub(n)..].to_vec()
        } else if info.uidnext != 0 && info.uidnext <= f.last_uid + 1 {
            vec![]
        } else {
            c.uid_search(&format!("UID {}:*", f.last_uid + 1)).await?.into_iter().filter(|u| *u > f.last_uid).collect()
        };
        let headers = c.fetch_headers(&fresh).await?;
        let added = messages::insert_envelopes(&store, &fr, &headers)?;
        // 还有待办的行：移动没送达前占位不能清，标记没送达前不拿服务器上的旧值覆盖
        let busy = pending::busy_ids(&store.w(), f.account_id)?;
        messages::drop_stale_placeholders(&store, f.id, &busy)?;
        let max_uid = headers.iter().map(|h| h.uid).max().unwrap_or(0).max(f.last_uid);
        let min_uid = headers.iter().map(|h| h.uid).min().map(|m| if f.oldest_uid == 0 { m } else { m.min(f.oldest_uid) }).unwrap_or(f.oldest_uid);

        // ③ 标记变化 + 服务器端删除。尽量少做（见 `plan_reconcile`）：
        //    EXISTS 与 UIDNEXT 都没变 → 没有增删，不用 UID SEARCH 找被删的；
        //    有 CONDSTORE 且 HIGHESTMODSEQ 没变 → 标记也没变；没有 CONDSTORE 时只核对最近 RECENT_WINDOW 封的标记；
        //    每天做一次全量核对兜底。
        let prev = folder_sync_state(&store, f.id);
        let now = now_ms();
        let plan = plan_reconcile(&prev, &info, uv == f.uidvalidity, f.last_uid, &f.modseq, now);
        if f.last_uid > 0 && (plan.gone || plan.flags != FlagScan::None) {
            let window_lo: u32 = if plan.flags == FlagScan::Recent && !plan.gone {
                store
                    .w()
                    .query_row(
                        "SELECT uid FROM messages WHERE folder_id = ? AND uid > 0 AND uid <= ? ORDER BY uid DESC LIMIT 1 OFFSET ?",
                        params![f.id, f.last_uid, RECENT_WINDOW - 1],
                        |r| r.get::<_, i64>(0),
                    )
                    .map(|u| u as u32)
                    .unwrap_or(1)
            } else {
                1
            };
            let known: Vec<(i64, i64, u32, Flags)> = {
                let w = store.w();
                let mut st =
                    w.prepare("SELECT id, account_id, uid, seen, flagged, answered FROM messages WHERE folder_id = ? AND uid >= ? AND uid > 0 AND uid <= ?")?;
                let v = st
                    .query_map(params![f.id, window_lo, f.last_uid], |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get::<_, i64>(2)? as u32,
                            Flags {
                                seen: r.get::<_, i64>(3)? != 0,
                                flagged: r.get::<_, i64>(4)? != 0,
                                answered: r.get::<_, i64>(5)? != 0,
                                ..Default::default()
                            },
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                v
            };
            if !known.is_empty() {
                let lo = known.iter().map(|k| k.2).min().unwrap_or(1);
                if plan.gone {
                    let server: HashSet<u32> = c.uid_search(&format!("UID {lo}:{}", f.last_uid)).await?.into_iter().collect();
                    let gone: Vec<(i64, i64)> = known.iter().filter(|k| !server.contains(&k.2)).map(|k| (k.0, k.1)).collect();
                    messages::remove(&store, &gone)?;
                }
                if plan.flags != FlagScan::None {
                    let since = if plan.flags == FlagScan::Changed { info.highest_modseq.and(f.modseq.parse::<u64>().ok()) } else { None };
                    let flags = c.fetch_flags(&format!("{lo}:{}", f.last_uid), since).await?;
                    let by_uid: HashMap<u32, &(i64, i64, u32, Flags)> = known.iter().map(|k| (k.2, k)).collect();
                    let changes: Vec<(i64, Flags)> = flags
                        .into_iter()
                        .filter_map(|(u, fl)| {
                            let k = by_uid.get(&u)?;
                            if busy.contains(&k.0) {
                                return None;
                            }
                            (k.3.seen != fl.seen || k.3.flagged != fl.flagged || k.3.answered != fl.answered).then_some((k.0, fl))
                        })
                        .collect();
                    if !changes.is_empty() {
                        store.tx(|tx| {
                            for (id, fl) in &changes {
                                tx.execute(
                                    "UPDATE messages SET seen = ?, flagged = ?, answered = ? WHERE id = ?",
                                    params![fl.seen as i64, fl.flagged as i64, fl.answered as i64, id],
                                )?;
                            }
                            Ok(())
                        })?;
                    }
                }
            }
        }
        store.w().execute(
            "INSERT INTO folder_sync (folder_id, exists_n, uidnext, full_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(folder_id) DO UPDATE SET exists_n = ?2, uidnext = ?3, full_at = ?4",
            params![f.id, info.exists, info.uidnext, if plan.full || f.last_uid == 0 { now } else { prev.full_at }],
        )?;
        let backfill_done = f.backfill_done || (f.last_uid == 0 && info.exists <= settings.initial_sync_per_folder);
        store.w().execute(
            "UPDATE folders SET uidvalidity = ?, modseq = ?, last_uid = ?, oldest_uid = ?, total = ?, backfill_done = ?, synced_at = ? WHERE id = ?",
            params![uv, info.highest_modseq.map(|m| m.to_string()).unwrap_or_default(), max_uid, min_uid, info.exists, backfill_done as i64, now_ms(), f.id],
        )?;
        Ok(added)
    }

    async fn prefetch(&mut self, acc: &Account, f: &Folder, s: &super::config::Settings) -> Result<()> {
        if s.prefetch_bodies == 0 || f.role == "trash" || f.role == "junk" {
            return Ok(());
        }
        let store = self.store().clone();
        let todo: Vec<(i64, u32)> = {
            let c = store.w();
            let mut st = c.prepare(
                "SELECT id, uid FROM (SELECT id, uid, body_state, size, date FROM messages WHERE folder_id = ? ORDER BY date DESC LIMIT ?)
                 WHERE body_state = 0 AND size <= ? AND uid > 0 ORDER BY date DESC",
            )?;
            let v = st
                .query_map(params![f.id, s.prefetch_bodies, s.max_prefetch_size as i64], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u32)))?
                .collect::<Result<Vec<_>, _>>()?;
            v
        };
        if todo.is_empty() {
            return Ok(());
        }
        // AgentMail 一次取多封原文会超时，逐封取
        let chunk = if acc.provider == "agentmail" { 1 } else { 10 };
        self.conn().await?.select(&f.path).await?;
        for part in todo.chunks(chunk) {
            self.pump_ops().await;
            let c = self.conn().await?;
            c.ensure_selected(&f.path).await?;
            let uids: Vec<u32> = part.iter().map(|p| p.1).collect();
            let got = c.fetch_raw_many(&uids).await?;
            let map: HashMap<u32, Vec<u8>> = got.into_iter().collect();
            for (id, uid) in part {
                match map.get(uid) {
                    Some(raw) => {
                        if messages::store_body(&store, *id, raw).is_err() {
                            store.w().execute("UPDATE messages SET body_state = 2 WHERE id = ?", [id])?;
                        }
                    }
                    None => {
                        store.w().execute("UPDATE messages SET body_state = 2 WHERE id = ?", [id])?;
                    }
                }
            }
        }
        self.eng.emit(Evt::Folder { account_id: f.account_id, folder_id: f.id, added: 0 });
        Ok(())
    }

    async fn backfill(&mut self, folder: &Folder) -> Result<usize> {
        let store = self.store().clone();
        let f = store.r().query_row(&format!("SELECT {} FROM folders WHERE id = ?", Folder::COLS), [folder.id], Folder::from_row)?;
        if f.backfill_done {
            return Ok(0);
        }
        if f.oldest_uid <= 1 {
            store.w().execute("UPDATE folders SET backfill_done = 1 WHERE id = ?", [f.id])?;
            return Ok(0);
        }
        let c = self.conn().await?;
        c.ensure_selected(&f.path).await?;
        let older = c.uid_search(&format!("UID 1:{}", f.oldest_uid - 1)).await?;
        let older: Vec<u32> = older.into_iter().filter(|u| *u < f.oldest_uid).collect();
        if older.is_empty() {
            store.w().execute("UPDATE folders SET backfill_done = 1 WHERE id = ?", [f.id])?;
            return Ok(0);
        }
        let chunk = &older[older.len().saturating_sub(500)..];
        let headers = c.fetch_headers(chunk).await?;
        let added = messages::insert_envelopes(&store, &FolderRef { id: f.id, account_id: f.account_id }, &headers)?;
        store.w().execute("UPDATE folders SET oldest_uid = ?, backfill_done = ? WHERE id = ?", params![chunk[0], (older.len() <= 500) as i64, f.id])?;
        Ok(added)
    }
}

/// 无 CONDSTORE 时平时只核对最近这么多封的标记
const RECENT_WINDOW: i64 = 500;
/// 全量核对（找被删的 + 全部标记）的最长间隔
const FULL_CHECK_MS: i64 = 24 * 3600 * 1000;

#[derive(Clone, Debug, Default, PartialEq)]
struct FolderSyncState {
    exists: i64,
    uidnext: i64,
    full_at: i64,
}

fn folder_sync_state(store: &Store, folder_id: i64) -> FolderSyncState {
    store
        .r()
        .query_row("SELECT exists_n, uidnext, full_at FROM folder_sync WHERE folder_id = ?", [folder_id], |r| {
            Ok(FolderSyncState { exists: r.get(0)?, uidnext: r.get(1)?, full_at: r.get(2)? })
        })
        .unwrap_or(FolderSyncState { exists: -1, uidnext: 0, full_at: 0 })
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum FlagScan {
    None,
    /// CONDSTORE：FETCH … CHANGEDSINCE
    Changed,
    /// 无 CONDSTORE：只看最近 RECENT_WINDOW 封
    Recent,
    All,
}

#[derive(Debug, PartialEq)]
struct ReconcilePlan {
    gone: bool,
    flags: FlagScan,
    full: bool,
}

/// 决定这一轮对已有邮件要核对什么（纯函数，便于测试）
fn plan_reconcile(prev: &FolderSyncState, info: &super::imap::MailboxInfo, same_uv: bool, last_uid: u32, modseq: &str, now: i64) -> ReconcilePlan {
    let full = now - prev.full_at >= FULL_CHECK_MS;
    let same_counts = same_uv && info.uidnext != 0 && prev.exists == info.exists as i64 && prev.uidnext == info.uidnext as i64;
    let flags = match info.highest_modseq {
        Some(m) if m.to_string() == modseq && last_uid > 0 => FlagScan::None,
        Some(_) if !modseq.is_empty() => FlagScan::Changed,
        _ if full => FlagScan::All,
        Some(_) => FlagScan::All,
        None => FlagScan::Recent,
    };
    ReconcilePlan { gone: full || !same_counts, flags, full }
}

pub fn folders_of(store: &Store, acc_id: i64) -> Result<Vec<Folder>> {
    let c = store.r();
    let mut st = c.prepare(&format!("SELECT {} FROM folders WHERE account_id = ? ORDER BY path", Folder::COLS))?;
    let v = st.query_map([acc_id], Folder::from_row)?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_plan_skips_unchanged() {
        use super::super::imap::MailboxInfo;
        let now = 10 * FULL_CHECK_MS;
        let prev = FolderSyncState { exists: 10, uidnext: 50, full_at: now - 1000 };
        let cond = MailboxInfo { exists: 10, uidvalidity: 1, uidnext: 50, highest_modseq: Some(7) };
        // 什么都没变：完全跳过
        assert_eq!(plan_reconcile(&prev, &cond, true, 49, "7", now), ReconcilePlan { gone: false, flags: FlagScan::None, full: false });
        // 只有标记变了：增量取标记，不找被删的
        assert_eq!(plan_reconcile(&prev, &cond, true, 49, "6", now).flags, FlagScan::Changed);
        assert!(!plan_reconcile(&prev, &cond, true, 49, "6", now).gone);
        // 数量变了：要找被删的
        assert!(plan_reconcile(&prev, &MailboxInfo { exists: 9, ..cond.clone() }, true, 49, "7", now).gone);
        // 没有 CONDSTORE：平时只看最近的，到期做全量
        let plain = MailboxInfo { highest_modseq: None, ..cond.clone() };
        assert_eq!(plan_reconcile(&prev, &plain, true, 49, "", now), ReconcilePlan { gone: false, flags: FlagScan::Recent, full: false });
        let old = FolderSyncState { full_at: 0, ..prev.clone() };
        assert_eq!(plan_reconcile(&old, &plain, true, 49, "", now), ReconcilePlan { gone: true, flags: FlagScan::All, full: true });
        // 从没记录过（升级后第一次）：按变化处理
        assert!(plan_reconcile(&FolderSyncState { exists: -1, uidnext: 0, full_at: now }, &cond, true, 49, "7", now).gone);
    }

    #[test]
    fn backoff_grows_and_auth_stops() {
        let mut b = Backoff::default();
        assert!(!b.blocked(0));
        let mins: Vec<i64> = (0..7)
            .map(|_| {
                b.fail(ErrorKind::Connection, 0);
                b.until / 60_000
            })
            .collect();
        assert_eq!(mins, vec![1, 2, 5, 15, 60, 60, 60]);
        assert!(b.blocked(59 * 60_000) && !b.blocked(60 * 60_000));
        let mut a = Backoff::default();
        a.fail(ErrorKind::Auth, 0);
        assert!(a.blocked(i64::MAX / 2), "认证失败后不再自动重试");
    }
}
