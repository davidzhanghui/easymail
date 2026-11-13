//! IMAP 客户端封装（async-imap）。
//!
//! - 每条命令都有超时：服务器卡死时不会把整个同步线程挂住
//! - 文件夹路径在库里是 UTF-8，发命令前编码成 modified UTF-7
//! - 不取 ENVELOPE（AgentMail 会让解析器死循环），取 HEADER.FIELDS 交给 mail-parser
//! - 163 / 126 登录后必须发 ID 命令，否则 SELECT 报 "Unsafe Login"

use super::error;
use super::model::Account;
use super::net::{self, Conn, Proxy};
use super::parse::{self, Envelope, HEADER_FIELDS};
use super::utf7;
use anyhow::{bail, Context, Result};
use async_imap::types::{Flag, NameAttribute};
use async_imap::Session;
use futures::{Stream, StreamExt, TryStreamExt};
use std::future::Future;
use std::time::Duration;

pub const CMD_TIMEOUT: Duration = Duration::from_secs(60);
const FETCH_CHUNK: usize = 100;

/// 每条命令都套超时；错误按「服务器拒绝 / 连接问题」分类（见 `error.rs`）
async fn timed<T>(what: &str, f: impl Future<Output = async_imap::error::Result<T>>) -> Result<T> {
    match tokio::time::timeout(CMD_TIMEOUT, f).await {
        Ok(r) => r.map_err(|e| error::from_imap(what, e)),
        Err(_) => Err(error::connection(format!("{what}超时（{} 秒无响应）", CMD_TIMEOUT.as_secs()))),
    }
}

async fn collect<T, S>(what: &str, s: S) -> Result<Vec<T>>
where
    S: Stream<Item = async_imap::error::Result<T>>,
{
    match tokio::time::timeout(CMD_TIMEOUT, s.try_collect::<Vec<T>>()).await {
        Ok(r) => r.map_err(|e| error::from_imap(what, e)),
        Err(_) => Err(error::connection(format!("{what}超时（{} 秒无响应）", CMD_TIMEOUT.as_secs()))),
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Caps {
    pub idle: bool,
    pub condstore: bool,
    pub mv: bool,
    pub uidplus: bool,
    pub id: bool,
    /// RFC 5819：一条 LIST 命令顺带返回每个文件夹的 STATUS
    pub list_status: bool,
    /// RFC 5465：IDLE 时同时监听多个文件夹
    pub notify: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Flags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub deleted: bool,
}

impl Flags {
    fn from_iter<'a>(it: impl Iterator<Item = Flag<'a>>) -> Flags {
        let mut f = Flags::default();
        for x in it {
            match x {
                Flag::Seen => f.seen = true,
                Flag::Flagged => f.flagged = true,
                Flag::Answered => f.answered = true,
                Flag::Draft => f.draft = true,
                Flag::Deleted => f.deleted = true,
                _ => {}
            }
        }
        f
    }
}

#[derive(Clone, Debug)]
pub struct RemoteFolder {
    /// 解码后的 UTF-8 路径（入库用）
    pub path: String,
    pub name: String,
    pub delimiter: String,
    pub special: Vec<String>,
    pub noselect: bool,
}

#[derive(Clone, Debug, Default)]
pub struct MailboxInfo {
    pub exists: u32,
    pub uidvalidity: u32,
    pub uidnext: u32,
    pub highest_modseq: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct HeaderItem {
    pub uid: u32,
    pub flags: Flags,
    pub size: u32,
    pub internal_date: Option<i64>,
    pub env: Envelope,
}

pub struct Imap {
    s: Option<Session<Conn>>,
    pub caps: Caps,
    selected: Option<String>,
    pub email: String,
}

/// 把登录失败转成可读的提示
pub fn explain_login_error(acc: &Account, e: &str) -> String {
    let l = e.to_lowercase();
    if l.contains("authenticated but not connected") {
        // 令牌已被接受，但 Exchange 不让这个邮箱走 IMAP：不是重新授权能解决的
        return format!(
            "令牌有效，但服务器拒绝该邮箱的 IMAP 连接：请确认邮箱已启用 IMAP、账号已在网页端完成初始化且没有被风控；组织账号还需管理员放行（{}）",
            e.trim()
        );
    }
    if l.contains("auth") || l.contains("login") || l.contains("password") || l.contains("credentials") || l.contains("invalid") || e.contains("密码") {
        format!("登录被拒绝：{}（{}）", super::providers::provider(&acc.provider).auth_hint, e.trim())
    } else {
        e.trim().to_string()
    }
}

impl Imap {
    /// `secret`：密码 / 授权码；OAuth 账号（`acc.is_oauth()`）传 access_token，走 SASL XOAUTH2
    pub async fn connect(acc: &Account, secret: &str, proxy: Option<&Proxy>) -> Result<Imap> {
        let proxy = net::effective_proxy(acc.use_proxy, proxy)?;
        let loopback = net::is_loopback(&acc.imap_host);
        let ctx = || format!("连接 {}:{} 失败{}", acc.imap_host, acc.imap_port, if proxy.is_some() { "（经代理）" } else { "" });
        // 993：直接 TLS；143（非本机）：先明文握手再 STARTTLS 升级，升级失败就断开，绝不明文登录；
        // 本机地址的其它端口（本机桥接 / 测试服务器）走明文
        let (conn, greeted) = if acc.imap_port == 143 && !loopback {
            let tcp = net::tunnel(&acc.imap_host, acc.imap_port, proxy).await.with_context(ctx)?;
            (starttls(&acc.imap_host, tcp).await.with_context(ctx)?, true)
        } else {
            let tls = !(loopback && acc.imap_port != 993);
            (net::connect(&acc.imap_host, acc.imap_port, proxy, tls).await.with_context(ctx)?, false)
        };
        let mut client = async_imap::Client::new(conn);
        if !greeted {
            match tokio::time::timeout(Duration::from_secs(30), client.read_response()).await {
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(e))) => bail!("读取服务器问候失败：{e}"),
                Ok(None) => bail!("服务器关闭了连接"),
                Err(_) => bail!("服务器 30 秒没有问候，可能被墙或端口不对"),
            }
        }
        let login = async {
            if acc.is_oauth() {
                client.authenticate("XOAUTH2", XOAuth2(super::oauth::xoauth2(&acc.email, secret))).await
            } else {
                client.login(&acc.email, secret).await
            }
        };
        let s = match tokio::time::timeout(CMD_TIMEOUT, login).await {
            Ok(Ok(s)) => s,
            // 服务器明确拒绝登录 → 认证类错误：停止自动重试，等用户改密码
            Ok(Err((e @ (async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_)), _))) => {
                let mut msg = explain_login_error(acc, &e.to_string());
                if acc.is_oauth() {
                    msg = format!("{msg}［{}］", super::oauth::token_summary(secret));
                }
                return Err(error::auth(msg));
            }
            Ok(Err((e, _))) => bail!("{}", explain_login_error(acc, &e.to_string())),
            Err(_) => bail!("登录超时"),
        };
        let mut me = Imap { s: Some(s), caps: Caps::default(), selected: None, email: acc.email.clone() };
        let caps = timed("读取能力", me.ss().capabilities()).await?;
        me.caps = Caps {
            idle: caps.has_str("IDLE"),
            condstore: caps.has_str("CONDSTORE"),
            mv: caps.has_str("MOVE"),
            uidplus: caps.has_str("UIDPLUS"),
            id: caps.has_str("ID"),
            list_status: caps.has_str("LIST-STATUS"),
            notify: caps.has_str("NOTIFY"),
        };
        if me.caps.id {
            let ver = super::config::VERSION;
            let _ = timed("ID", me.ss().id([("name", Some("EasyMail")), ("version", Some(ver)), ("vendor", Some("easymail"))])).await;
        }
        Ok(me)
    }

    fn ss(&mut self) -> &mut Session<Conn> {
        self.s.as_mut().expect("session")
    }

    fn drain_unsolicited(&mut self) -> bool {
        let mut any = false;
        if let Some(s) = self.s.as_mut() {
            while s.unsolicited_responses.try_recv().is_ok() {
                any = true;
            }
        }
        any
    }

    /// 订阅「已订阅文件夹」+ 当前文件夹的新邮件 / 删除 / 标记变化（RFC 5465）。失败返回 false，调用方退回只监听收件箱
    pub async fn enable_notify(&mut self) -> bool {
        if !self.caps.notify {
            return false;
        }
        let cmd = "NOTIFY SET (selected (MessageNew MessageExpunge FlagChange)) (subscribed (MessageNew (UIDNEXT MESSAGES) MessageExpunge FlagChange))";
        match tokio::time::timeout(CMD_TIMEOUT, self.ss().run_command_and_check_ok(cmd)).await {
            Ok(Ok(())) => true,
            _ => {
                log::info!("[imap] NOTIFY 订阅被服务器拒绝，只监听收件箱");
                false
            }
        }
    }

    pub async fn noop(&mut self) -> Result<()> {
        timed("NOOP", self.ss().noop()).await
    }

    pub async fn logout(mut self) {
        if let Some(mut s) = self.s.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), s.logout()).await;
        }
    }

    pub async fn list_folders(&mut self) -> Result<Vec<RemoteFolder>> {
        let stream = timed("列出文件夹", self.ss().list(Some(""), Some("*"))).await?;
        let names = collect("列出文件夹", stream).await?;
        let mut out = Vec::new();
        for n in names {
            let raw = n.name().to_string();
            let path = utf7::decode(&raw);
            let delimiter = n.delimiter().unwrap_or("/").to_string();
            let mut special = Vec::new();
            let mut noselect = false;
            for a in n.attributes() {
                let s = match a {
                    NameAttribute::NoSelect => {
                        noselect = true;
                        continue;
                    }
                    NameAttribute::All => "\\All",
                    NameAttribute::Archive => "\\Archive",
                    NameAttribute::Drafts => "\\Drafts",
                    NameAttribute::Flagged => "\\Flagged",
                    NameAttribute::Junk => "\\Junk",
                    NameAttribute::Sent => "\\Sent",
                    NameAttribute::Trash => "\\Trash",
                    NameAttribute::Extension(e) => {
                        if e.eq_ignore_ascii_case("\\Noselect") || e.eq_ignore_ascii_case("\\NonExistent") {
                            noselect = true;
                        }
                        special.push(e.to_string());
                        continue;
                    }
                    _ => continue,
                };
                special.push(s.to_string());
            }
            let name = if delimiter.is_empty() { path.clone() } else { path.rsplit(delimiter.as_str()).next().unwrap_or(&path).to_string() };
            out.push(RemoteFolder { path, name, delimiter, special, noselect });
        }
        Ok(out)
    }

    pub async fn select(&mut self, path: &str) -> Result<MailboxInfo> {
        let enc = utf7::encode(path);
        let condstore = self.caps.condstore;
        let mb = if condstore {
            timed(&format!("打开「{path}」"), self.ss().select_condstore(&enc)).await?
        } else {
            timed(&format!("打开「{path}」"), self.ss().select(&enc)).await?
        };
        self.selected = Some(path.to_string());
        self.drain_unsolicited();
        Ok(MailboxInfo { exists: mb.exists, uidvalidity: mb.uid_validity.unwrap_or(0), uidnext: mb.uid_next.unwrap_or(0), highest_modseq: mb.highest_modseq })
    }

    /// 确保选中了某个文件夹（已选中则跳过）
    pub async fn ensure_selected(&mut self, path: &str) -> Result<()> {
        if self.selected.as_deref() != Some(path) {
            self.select(path).await?;
        }
        Ok(())
    }

    pub async fn uid_search(&mut self, query: &str) -> Result<Vec<u32>> {
        let set = timed("搜索", self.ss().uid_search(query)).await?;
        let mut v: Vec<u32> = set.into_iter().collect();
        v.sort_unstable();
        Ok(v)
    }

    /// 拉信封（分块，避免一次请求过大；单块失败会整体报错）
    pub async fn fetch_headers(&mut self, uids: &[u32]) -> Result<Vec<HeaderItem>> {
        let mut out = Vec::with_capacity(uids.len());
        for chunk in uids.chunks(FETCH_CHUNK) {
            let set = uid_set(chunk);
            let q = format!("(UID FLAGS RFC822.SIZE INTERNALDATE BODY.PEEK[HEADER.FIELDS ({HEADER_FIELDS})])");
            let stream = timed("拉取信封", self.ss().uid_fetch(&set, &q)).await?;
            let items = collect("拉取信封", stream).await?;
            for f in items {
                let Some(uid) = f.uid else { continue };
                let env = f.header().map(parse::parse_headers).unwrap_or_default();
                out.push(HeaderItem {
                    uid,
                    flags: Flags::from_iter(f.flags()),
                    size: f.size.unwrap_or(0),
                    internal_date: f.internal_date().map(|d| d.timestamp_millis()),
                    env,
                });
            }
        }
        out.sort_by_key(|h| h.uid);
        Ok(out)
    }

    /// 拉标记：`changed_since` 有值时走 CONDSTORE 增量
    pub async fn fetch_flags(&mut self, range: &str, changed_since: Option<u64>) -> Result<Vec<(u32, Flags)>> {
        let q = match changed_since {
            Some(m) if self.caps.condstore => format!("(UID FLAGS) (CHANGEDSINCE {m})"),
            _ => "(UID FLAGS)".to_string(),
        };
        let stream = timed("拉取标记", self.ss().uid_fetch(range, &q)).await?;
        let items = collect("拉取标记", stream).await?;
        Ok(items.into_iter().filter_map(|f| f.uid.map(|u| (u, Flags::from_iter(f.flags())))).collect())
    }

    /// 取完整原文（不改已读状态）
    pub async fn fetch_raw(&mut self, uid: u32) -> Result<Option<Vec<u8>>> {
        Ok(self.fetch_raw_many(&[uid]).await?.into_iter().find(|(u, _)| *u == uid).map(|(_, b)| b))
    }

    /// 一次取多封原文（AgentMail 多封一起取会超时，调用方对它逐封取）
    pub async fn fetch_raw_many(&mut self, uids: &[u32]) -> Result<Vec<(u32, Vec<u8>)>> {
        if uids.is_empty() {
            return Ok(vec![]);
        }
        let f = self.ss().uid_fetch(uid_set(uids), "(UID BODY.PEEK[])");
        let stream = match tokio::time::timeout(Duration::from_secs(180), f).await {
            Ok(r) => r.map_err(|e| error::from_imap("下载正文", e))?,
            Err(_) => bail!("下载正文超时"),
        };
        let items = match tokio::time::timeout(Duration::from_secs(180), stream.try_collect::<Vec<_>>()).await {
            Ok(r) => r.map_err(|e| error::from_imap("下载正文", e))?,
            Err(_) => bail!("下载正文超时"),
        };
        Ok(items.into_iter().filter_map(|f| Some((f.uid?, f.body()?.to_vec()))).collect())
    }

    /// STATUS：(总数, 未读数)
    pub async fn status(&mut self, path: &str) -> Result<(u32, u32)> {
        let mb = timed("读取文件夹状态", self.ss().status(utf7::encode(path), "(MESSAGES UNSEEN)")).await?;
        Ok((mb.exists, mb.unseen.unwrap_or(0)))
    }

    /// LIST-STATUS（RFC 5819）：一条命令拿到所有文件夹的 (总数, 未读数)，键为 UTF-8 路径。
    /// 服务器不支持返回空表；结果经 async-imap 的 unsolicited 通道（容量 100）收回，
    /// 文件夹很多时可能收不全，调用方对缺的再逐个 STATUS。
    pub async fn list_status(&mut self) -> Result<std::collections::HashMap<String, (u32, u32)>> {
        use async_imap::types::{StatusAttribute, UnsolicitedResponse};
        let mut out = std::collections::HashMap::new();
        if !self.caps.list_status {
            return Ok(out);
        }
        self.drain_unsolicited();
        timed("读取文件夹状态", self.ss().run_command_and_check_ok("LIST \"\" \"*\" RETURN (STATUS (MESSAGES UNSEEN))")).await?;
        let s = self.ss();
        while let Ok(r) = s.unsolicited_responses.try_recv() {
            if let UnsolicitedResponse::Status { mailbox, attributes } = r {
                let (mut total, mut unseen) = (0, 0);
                for a in attributes {
                    match a {
                        StatusAttribute::Messages(n) => total = n,
                        StatusAttribute::Unseen(n) => unseen = n,
                        _ => {}
                    }
                }
                out.insert(utf7::decode(&mailbox), (total, unseen));
            }
        }
        Ok(out)
    }

    pub async fn store_flag(&mut self, uids: &[u32], flag: &str, add: bool) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        let q = format!("{}FLAGS.SILENT ({flag})", if add { "+" } else { "-" });
        let stream = timed("修改标记", self.ss().uid_store(uid_set(uids), &q)).await?;
        collect("修改标记", stream).await?;
        Ok(())
    }

    async fn expunge_uids(&mut self, uids: &[u32]) -> Result<()> {
        if self.caps.uidplus {
            let stream = timed("彻底删除", self.ss().uid_expunge(uid_set(uids))).await?;
            collect("彻底删除", stream).await?;
        } else {
            // 没有 UIDPLUS 只能整夹 EXPUNGE，会把其他客户端标了「已删除」但还没清除的信一起清掉。
            // 先查一下：带 \Deleted 的恰好只有我们这几封才清除；否则只留删除标记，交给标记它们的客户端（或下次）处理。
            let marked: std::collections::HashSet<u32> = self.uid_search("DELETED").await?.into_iter().collect();
            let ours: std::collections::HashSet<u32> = uids.iter().copied().collect();
            if marked.is_subset(&ours) {
                let stream = timed("彻底删除", self.ss().expunge()).await?;
                collect("彻底删除", stream).await?;
            } else {
                log::info!(
                    "[imap] 服务器不支持 UIDPLUS，且文件夹里还有 {} 封别处标记删除的信，这次只打删除标记、不执行 EXPUNGE",
                    marked.difference(&ours).count()
                );
            }
        }
        Ok(())
    }

    /// 移动到另一个文件夹（无 MOVE 时退化为 COPY + 删除）
    pub async fn move_uids(&mut self, uids: &[u32], dest: &str) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        let enc = utf7::encode(dest);
        if self.caps.mv {
            timed("移动", self.ss().uid_mv(uid_set(uids), &enc)).await?;
        } else {
            timed("复制", self.ss().uid_copy(uid_set(uids), &enc)).await?;
            self.store_flag(uids, "\\Deleted", true).await?;
            self.expunge_uids(uids).await?;
        }
        self.drain_unsolicited();
        Ok(())
    }

    pub async fn delete_uids(&mut self, uids: &[u32]) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        self.store_flag(uids, "\\Deleted", true).await?;
        self.expunge_uids(uids).await?;
        self.drain_unsolicited();
        Ok(())
    }

    pub async fn append(&mut self, path: &str, flags: &str, raw: &[u8]) -> Result<()> {
        let enc = utf7::encode(path);
        let flags = if flags.is_empty() { None } else { Some(flags) };
        timed(&format!("保存到「{path}」"), self.ss().append(&enc, flags, None, raw)).await
    }

    pub async fn create_folder(&mut self, path: &str) -> Result<()> {
        timed("新建文件夹", self.ss().create(utf7::encode(path))).await?;
        let _ = timed("订阅文件夹", self.ss().subscribe(utf7::encode(path))).await;
        Ok(())
    }

    pub async fn delete_folder(&mut self, path: &str) -> Result<()> {
        if self.selected.as_deref() == Some(path) {
            let _ = timed("关闭文件夹", self.ss().close()).await;
            self.selected = None;
        }
        timed("删除文件夹", self.ss().delete(utf7::encode(path))).await
    }

    pub async fn rename_folder(&mut self, from: &str, to: &str) -> Result<()> {
        if self.selected.as_deref() == Some(from) {
            let _ = timed("关闭文件夹", self.ss().close()).await;
            self.selected = None;
        }
        timed("重命名文件夹", self.ss().rename(utf7::encode(from), utf7::encode(to))).await
    }

    /// 按 Message-ID 在当前选中的文件夹里查 UID（发信后判断服务商是否已自动存了「已发送」）
    pub async fn find_message_id(&mut self, path: &str, message_id: &str) -> Result<Vec<u32>> {
        self.ensure_selected(path).await?;
        self.uid_search(&format!("HEADER Message-ID {}", quote(message_id))).await
    }

    /// IDLE 等待最多 `dur`；返回 (self, 是否有新动静)。不支持 IDLE 时用 NOOP 代替。
    pub async fn idle(mut self, dur: Duration, stop: impl Future<Output = ()> + Send) -> Result<(Imap, bool)> {
        if !self.caps.idle {
            tokio::select! {
                _ = tokio::time::sleep(dur) => {}
                _ = stop => {}
            }
            self.noop().await?;
            let changed = self.drain_unsolicited();
            return Ok((self, changed));
        }
        let session = self.s.take().expect("session");
        let mut handle = session.idle();
        timed("进入 IDLE", handle.init()).await?;
        let (fut, interrupt) = handle.wait_with_timeout(dur);
        let res = tokio::select! {
            r = fut => Some(r),
            _ = stop => { drop(interrupt); None }
        };
        let changed = match res {
            Some(Ok(async_imap::extensions::idle::IdleResponse::NewData(_))) => true,
            Some(Ok(_)) => false,
            Some(Err(e)) => bail!("IDLE 中断：{e}"),
            None => false,
        };
        let s = timed("退出 IDLE", handle.done()).await?;
        self.s = Some(s);
        let more = self.drain_unsolicited();
        Ok((self, changed || more))
    }
}

/// UID 列表压缩成 IMAP 序列集：1,2,3,7 → 1:3,7
pub fn uid_set(uids: &[u32]) -> String {
    let mut v = uids.to_vec();
    v.sort_unstable();
    v.dedup();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let start = v[i];
        let mut end = start;
        while i + 1 < v.len() && v[i + 1] == end + 1 {
            i += 1;
            end = v[i];
        }
        out.push(if start == end { start.to_string() } else { format!("{start}:{end}") });
        i += 1;
    }
    out.join(",")
}

/// 连接测试（添加 / 编辑账号时）：能登录、能列出文件夹即算成功
/// IMAP 带引号字符串：转义 `\` 和 `"`，去掉换行（RFC 3501 quoted 不允许 CR/LF）
pub fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '\\' | '"' => {
                o.push('\\');
                o.push(c);
            }
            '\r' | '\n' => {}
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// SASL XOAUTH2：只有一步，初始响应就是全部；服务器若回错误详情（base64 JSON）则回空行结束
struct XOAuth2(String);

impl async_imap::Authenticator for XOAuth2 {
    type Response = String;
    fn process(&mut self, challenge: &[u8]) -> Self::Response {
        if challenge.is_empty() {
            std::mem::take(&mut self.0)
        } else {
            String::new()
        }
    }
}

/// 明文连上 143 后升级 TLS：读问候 → `a0 STARTTLS` → 等 `a0 OK` → TLS 握手。
/// 逐字节读，保证不会把 TLS 握手数据读进缓冲区；服务器不支持就报错，不会退回明文。
async fn starttls(host: &str, mut tcp: tokio::net::TcpStream) -> Result<Conn> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn line(tcp: &mut tokio::net::TcpStream) -> Result<String> {
        let mut v = Vec::new();
        let mut b = [0u8; 1];
        loop {
            let n = tokio::time::timeout(Duration::from_secs(30), tcp.read(&mut b)).await.map_err(|_| anyhow::anyhow!("服务器响应超时"))??;
            if n == 0 {
                bail!("服务器关闭了连接");
            }
            v.push(b[0]);
            if v.ends_with(b"\r\n") || v.len() > 8192 {
                return Ok(String::from_utf8_lossy(&v).trim_end().to_string());
            }
        }
    }
    let greet = line(&mut tcp).await?;
    if !greet.starts_with("* OK") {
        bail!("服务器问候异常：{greet}");
    }
    tcp.write_all(b"a0 STARTTLS\r\n").await?;
    loop {
        let l = line(&mut tcp).await?;
        if let Some(rest) = l.strip_prefix("a0 ") {
            if rest.to_ascii_uppercase().starts_with("OK") {
                break;
            }
            bail!("服务器不支持 STARTTLS（{rest}），为保护密码已中止连接");
        }
    }
    net::tls_wrap(host, tcp).await
}

pub async fn test_login(acc: &Account, password: &str, proxy: Option<&Proxy>) -> Result<usize> {
    let mut c = Imap::connect(acc, password, proxy).await?;
    let n = c.list_folders().await?.len();
    c.logout().await;
    Ok(n)
}

#[allow(dead_code)]
fn _assert_stream_send() {
    fn is_send<T: Send>() {}
    is_send::<Imap>();
    let _ = futures::stream::empty::<()>().boxed();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets() {
        assert_eq!(uid_set(&[1, 2, 3, 7, 9, 10]), "1:3,7,9:10");
        assert_eq!(uid_set(&[5]), "5");
        assert_eq!(uid_set(&[3, 1, 2, 2]), "1:3");
        assert_eq!(uid_set(&[]), "");
    }

    #[test]
    fn quoting() {
        assert_eq!(quote(r#"<a"b\c@x>"#), r#""<a\"b\\c@x>""#);
        assert_eq!(quote("<a\r\nb>"), "\"<ab>\"");
    }
}
