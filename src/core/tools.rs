//! 杂项工具：代理探测、检查更新、一键退订、连接诊断。

use super::db::{now_ms, Store};
use super::engine::Engine;
use super::http;
use super::model::Account;
use super::net::{self, Proxy};
use anyhow::{anyhow, bail, Result};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

pub const RELEASES_API: &str = "https://api.github.com/repos/davidzhanghui/easymail/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/davidzhanghui/easymail/releases/latest";

// ---------------- 代理探测 ----------------

/// 本机常见代理端口：(端口, 协议)。Clash / Clash Verge 的混合端口 7890 / 7897、V2RayU 的 1080 / 1087、Surge 6152/6153
const PROXY_PORTS: [(u16, &str); 6] = [(1080, "socks5"), (7890, "http"), (7897, "http"), (1087, "http"), (6153, "socks5"), (6152, "http")];

/// 探测本机上正在监听的代理端口，返回第一个可用的地址；都没有返回 None
pub fn probe_proxy() -> Option<String> {
    PROXY_PORTS.iter().find_map(|(port, scheme)| {
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().ok()?;
        TcpStream::connect_timeout(&addr, Duration::from_millis(150)).ok().map(|_| format!("{scheme}://127.0.0.1:{port}"))
    })
}

/// 首次启动：代理还没设置过时探测一次（以前默认写死 socks5://127.0.0.1:1080，没装代理的用户勾了「走代理」就连不上）
pub fn init_proxy(store: &Store) {
    if store.get_kv("proxy_probed").is_some() {
        return;
    }
    store.set_kv("proxy_probed", "1");
    let mut s = store.settings();
    if !s.proxy_url.trim().is_empty() {
        return;
    }
    if let Some(p) = probe_proxy() {
        log::info!("[proxy] 探测到本机代理 {p}");
        s.proxy_url = p;
        let _ = store.save_settings(&s);
    }
}

// ---------------- 检查更新 ----------------

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub notes: String,
}

/// "v1.2.3" / "1.2.3-beta" → (1, 2, 3)
fn ver(s: &str) -> (u64, u64, u64) {
    let core = s.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next().unwrap_or("");
    let mut it = core.split('.').map(|x| x.parse::<u64>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

pub fn newer(remote: &str, local: &str) -> bool {
    ver(remote) > ver(local)
}

/// 查 GitHub Releases 的最新版本；比当前新就返回。只读一个公开 JSON，不上传任何信息（不带设备标识）
pub async fn check_update(proxy: Option<&Proxy>) -> Result<Option<Release>> {
    let r = http::request("GET", RELEASES_API, &[("Accept", "application/vnd.github+json")], None, proxy).await?;
    if !r.ok() {
        bail!("GitHub 返回 HTTP {}", r.status);
    }
    let j = r.json()?;
    let tag = j.get("tag_name").and_then(|v| v.as_str()).unwrap_or("");
    if tag.is_empty() || !newer(tag, super::config::VERSION) {
        return Ok(None);
    }
    Ok(Some(Release {
        version: tag.trim_start_matches('v').to_string(),
        url: j.get("html_url").and_then(|v| v.as_str()).unwrap_or(RELEASES_PAGE).to_string(),
        notes: j.get("body").and_then(|v| v.as_str()).unwrap_or("").chars().take(600).collect(),
    }))
}

/// 每天最多自动检查一次；设置里可以关掉（kv `check_updates` = "0"）
pub fn update_due(store: &Store) -> bool {
    if store.get_kv("check_updates").as_deref() == Some("0") {
        return false;
    }
    let last: i64 = store.get_kv("update_checked_at").and_then(|v| v.parse().ok()).unwrap_or(0);
    now_ms() - last > 24 * 3600 * 1000
}

pub fn mark_update_checked(store: &Store) {
    store.set_kv("update_checked_at", &now_ms().to_string());
}

// ---------------- 一键退订 ----------------

#[derive(Clone, Debug, PartialEq)]
pub enum Unsub {
    /// RFC 8058 一键退订已完成（POST 成功）
    Done,
    /// 已发出退订邮件
    Mailed(String),
    /// 只能打开网页退订
    Open(String),
}

/// 退订：优先 RFC 8058 一键 POST（不用打开网页）；其次发 mailto 退订信；都不行就返回网页地址让界面打开
pub async fn unsubscribe(eng: &Engine, msg_id: i64) -> Result<Unsub> {
    let store = eng.store().clone();
    let (acc_id, raw, post): (i64, String, bool) = store
        .r()
        .query_row("SELECT account_id, unsub, unsub_post FROM messages WHERE id = ?", [msg_id], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)))?;
    let (http_url, mailto) = super::parse::unsub_targets(&raw);
    if let (Some(u), true) = (&http_url, post) {
        if u.starts_with("https://") {
            let r =
                http::request("POST", u, &[("Content-Type", "application/x-www-form-urlencoded")], Some(b"List-Unsubscribe=One-Click"), eng.proxy().as_ref())
                    .await?;
            if r.ok() || r.status == 202 {
                return Ok(Unsub::Done);
            }
            log::warn!("[unsub] 一键退订返回 HTTP {}，改用其它方式", r.status);
        }
    }
    if let Some(m) = mailto {
        let u = url::Url::parse(&m).map_err(|_| anyhow!("退订地址不正确：{m}"))?;
        let to = percent_encoding::percent_decode_str(u.path()).decode_utf8_lossy().to_string();
        let subject = u.query_pairs().find(|(k, _)| k.eq_ignore_ascii_case("subject")).map(|(_, v)| v.into_owned()).unwrap_or_else(|| "unsubscribe".into());
        let body = u.query_pairs().find(|(k, _)| k.eq_ignore_ascii_case("body")).map(|(_, v)| v.into_owned()).unwrap_or_else(|| "unsubscribe".into());
        let acc = super::accounts::must(&store, acc_id)?;
        if acc.is_local() {
            bail!("本地归档的邮件没有发件账号，无法发送退订邮件");
        }
        let o = super::mailer::Outgoing { account_id: acc_id, mode: "new".into(), to: to.clone(), subject, text: body, ..Default::default() };
        super::outbox::enqueue(&store, &o, Some(now_ms()))?;
        return Ok(Unsub::Mailed(to));
    }
    match http_url {
        Some(u) => Ok(Unsub::Open(u)),
        None => bail!("这封邮件没有提供退订方式"),
    }
}

// ---------------- 连接诊断 ----------------

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub ms: u128,
}

/// 逐步诊断一个账号的连接：DNS → TCP（IMAP / SMTP 端口，经代理时测代理）→ TLS → 登录，每步给出耗时与原因
pub async fn diagnose(store: &Store, acc: &Account, proxy: Option<&Proxy>) -> Vec<Step> {
    let mut out = Vec::new();
    let proxy = match net::effective_proxy(acc.use_proxy, proxy) {
        Ok(p) => p,
        Err(e) => {
            out.push(Step { name: "代理设置".into(), ok: false, detail: format!("{e:#}"), ms: 0 });
            return out;
        }
    };
    if let Some(p) = proxy {
        let (h, port) = match p {
            Proxy::Socks5 { host, port, .. } | Proxy::Http { host, port, .. } => (host.clone(), *port),
        };
        let t = Instant::now();
        let r = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect((h.as_str(), port))).await;
        let ok = matches!(r, Ok(Ok(_)));
        out.push(Step {
            name: format!("代理 {h}:{port}"),
            ok,
            detail: if ok { "可连接".into() } else { "连不上代理：请确认代理软件已启动、端口正确".into() },
            ms: t.elapsed().as_millis(),
        });
        if !ok {
            return out;
        }
    }
    for (label, host, port) in [("IMAP", acc.imap_host.as_str(), acc.imap_port), ("SMTP", acc.smtp_host.as_str(), acc.smtp_port)] {
        if proxy.is_none() {
            let t = Instant::now();
            let r = tokio::net::lookup_host((host, port)).await;
            match r {
                Ok(mut it) => {
                    let ip = it.next().map(|a| a.ip().to_string()).unwrap_or_default();
                    out.push(Step { name: format!("{label} 域名解析 {host}"), ok: true, detail: ip, ms: t.elapsed().as_millis() });
                }
                Err(e) => {
                    out.push(Step {
                        name: format!("{label} 域名解析 {host}"),
                        ok: false,
                        detail: format!("{e}（检查网络或 DNS）"),
                        ms: t.elapsed().as_millis(),
                    });
                    continue;
                }
            }
        }
        let t = Instant::now();
        match net::tunnel(host, port, proxy).await {
            Ok(tcp) => {
                out.push(Step { name: format!("{label} 端口 {port}"), ok: true, detail: "TCP 已连通".into(), ms: t.elapsed().as_millis() });
                let implicit = (label == "IMAP" && port == 993) || (label == "SMTP" && acc.smtp_secure);
                if implicit {
                    let t = Instant::now();
                    match net::tls_wrap(host, tcp).await {
                        Ok(_) => {
                            out.push(Step { name: format!("{label} TLS"), ok: true, detail: "证书有效、握手成功".into(), ms: t.elapsed().as_millis() })
                        }
                        Err(e) => out.push(Step { name: format!("{label} TLS"), ok: false, detail: format!("{e:#}"), ms: t.elapsed().as_millis() }),
                    }
                }
            }
            Err(e) => out.push(Step {
                name: format!("{label} 端口 {port}"),
                ok: false,
                detail: format!("{e:#}（端口被防火墙拦截、服务器地址不对，或需要走代理）"),
                ms: t.elapsed().as_millis(),
            }),
        }
    }
    if out.iter().all(|s| s.ok) {
        let t = Instant::now();
        match super::accounts::credential(store, acc, proxy).await {
            Ok(pw) => {
                match super::imap::test_login(acc, &pw, proxy).await {
                    Ok(n) => out.push(Step { name: "IMAP 登录".into(), ok: true, detail: format!("成功，{n} 个文件夹"), ms: t.elapsed().as_millis() }),
                    Err(e) => out.push(Step { name: "IMAP 登录".into(), ok: false, detail: format!("{e:#}"), ms: t.elapsed().as_millis() }),
                }
                let t = Instant::now();
                match super::smtp::test_login(acc, &pw, proxy).await {
                    Ok(()) => out.push(Step { name: "SMTP 登录".into(), ok: true, detail: "成功".into(), ms: t.elapsed().as_millis() }),
                    Err(e) => out.push(Step { name: "SMTP 登录".into(), ok: false, detail: format!("{e:#}"), ms: t.elapsed().as_millis() }),
                }
            }
            Err(e) => out.push(Step { name: "读取凭据".into(), ok: false, detail: format!("{e:#}"), ms: t.elapsed().as_millis() }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("v1.0.1", "1.0.0"));
        assert!(newer("2.0.0-beta", "1.9.9"));
        assert!(!newer("v1.0.0", "1.0.0"));
        assert!(!newer("0.9.0", "1.0.0"));
        assert!(!newer("garbage", "1.0.0"));
    }

    #[test]
    fn probe_finds_listener() {
        // 不保证本机有代理：只验证函数不 panic、返回的地址格式正确
        if let Some(p) = probe_proxy() {
            assert!(Proxy::parse(&p).is_ok());
        }
    }
}
