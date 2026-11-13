//! 网络连接：直连 / socks5 代理 / http CONNECT 代理，之上可选 TLS。
//!
//! IMAP 与 SMTP 共用：先拿到一条到目标主机的 TCP 隧道，再按需要套 rustls。
//! 只有目标是本机回环地址时才允许明文（本地测试服务器用），其余一律 TLS。

use anyhow::{anyhow, bail, Context, Result};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context as TaskCx, Poll};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// 代理地址：socks5://host:port、socks5h://…、http://host:port，都可以带 `user:pass@`。
/// 用 `url` crate 解析：IPv6 字面量（`[::1]:1080`）、密码里的 `@` `:`（百分号编码）都能正确处理。
#[derive(Clone, Debug, PartialEq)]
pub enum Proxy {
    Socks5 { host: String, port: u16, auth: Option<(String, String)> },
    Http { host: String, port: u16, auth: Option<(String, String)> },
}

impl Proxy {
    pub fn parse(raw: &str) -> Result<Proxy> {
        let raw = raw.trim();
        if !raw.contains("://") {
            bail!("代理地址缺少协议，例如 socks5://127.0.0.1:1080");
        }
        let u = url::Url::parse(raw).map_err(|e| anyhow!("代理地址不正确：{raw}（{e}）"))?;
        let host = match u.host() {
            Some(url::Host::Ipv6(a)) => a.to_string(),
            Some(h) => h.to_string(),
            None => bail!("代理地址缺少主机：{raw}"),
        };
        // url 对 http 会补默认端口 80；这里要求显式写端口，免得悄悄连错
        let port = explicit_port(raw).ok_or_else(|| anyhow!("代理地址缺少端口或端口不正确：{raw}"))?;
        let decode = |s: &str| percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned();
        let auth = (!u.username().is_empty()).then(|| (decode(u.username()), decode(u.password().unwrap_or(""))));
        match u.scheme() {
            "socks5" | "socks5h" | "socks" => Ok(Proxy::Socks5 { host, port, auth }),
            "http" | "https" => Ok(Proxy::Http { host, port, auth }),
            other => bail!("不支持的代理协议：{other}（支持 socks5 / http）"),
        }
    }
}

/// 取地址里显式写出的端口（`url` 会把 http 的 :80 规范化掉，所以自己从原文里取）
fn explicit_port(raw: &str) -> Option<u16> {
    let rest = raw.split_once("://")?.1;
    let hostport = rest.split(['/', '?', '#']).next()?;
    let hostport = hostport.rsplit_once('@').map_or(hostport, |x| x.1);
    let port = if let Some(after) = hostport.strip_prefix('[') { after.split_once("]:")?.1 } else { hostport.rsplit_once(':')?.1 };
    port.parse().ok()
}

/// 明文或 TLS 的连接，统一成一个可读写的流。
#[derive(Debug)]
pub enum Conn {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for Conn {
    fn poll_read(self: Pin<&mut Self>, cx: &mut TaskCx<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Conn {
    fn poll_write(self: Pin<&mut Self>, cx: &mut TaskCx<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskCx<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_flush(cx),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut TaskCx<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// 账号实际要用的代理。账号勾了「走代理」却拿不到可用的代理时报错，
/// 绝不悄悄退回直连（那样会从真实 IP 连出去，用户毫不知情）。
pub fn effective_proxy(use_proxy: bool, proxy: Option<&Proxy>) -> Result<Option<&Proxy>> {
    match (use_proxy, proxy) {
        (false, _) => Ok(None),
        (true, Some(p)) => Ok(Some(p)),
        (true, None) => bail!("这个账号设置了走代理，但代理没有配置或地址不对（请到「设置 → 代理」检查），为避免直连已停止连接"),
    }
}

pub fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// 建立到 host:port 的 TCP 隧道（经代理或直连）。
pub async fn tunnel(host: &str, port: u16, proxy: Option<&Proxy>) -> Result<TcpStream> {
    let fut = async {
        match proxy {
            None => TcpStream::connect((host, port)).await.with_context(|| format!("连接 {host}:{port} 失败")),
            Some(Proxy::Socks5 { host: ph, port: pp, auth }) => {
                let target = (host, port);
                let s = match auth {
                    Some((u, p)) => tokio_socks::tcp::Socks5Stream::connect_with_password((ph.as_str(), *pp), target, u, p).await,
                    None => tokio_socks::tcp::Socks5Stream::connect((ph.as_str(), *pp), target).await,
                }
                .with_context(|| format!("经 SOCKS 代理 {ph}:{pp} 连接 {host}:{port} 失败"))?;
                Ok(s.into_inner())
            }
            Some(Proxy::Http { host: ph, port: pp, auth }) => {
                let mut s = TcpStream::connect((ph.as_str(), *pp)).await.with_context(|| format!("连接 HTTP 代理 {ph}:{pp} 失败"))?;
                let target = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
                let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
                if let Some((u, p)) = auth {
                    use base64::Engine as _;
                    let token = base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"));
                    req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
                }
                req.push_str("\r\n");
                s.write_all(req.as_bytes()).await?;
                // 逐字节读到空行为止，避免把隧道后面的数据读进缓冲区
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    let n = tokio::io::AsyncReadExt::read(&mut s, &mut b).await?;
                    if n == 0 || head.len() > 8192 {
                        bail!("HTTP 代理提前断开");
                    }
                    head.push(b[0]);
                }
                let line = String::from_utf8_lossy(&head);
                let code = line.split_whitespace().nth(1).unwrap_or("");
                if code == "407" {
                    bail!("HTTP 代理要求认证（407）：请在代理地址里写上用户名和密码，例如 http://user:pass@host:port");
                }
                if code != "200" {
                    bail!("HTTP 代理拒绝 CONNECT：{}", line.lines().next().unwrap_or(""));
                }
                Ok(s)
            }
        }
    };
    tokio::time::timeout(CONNECT_TIMEOUT, fut).await.map_err(|_| anyhow!("连接 {host}:{port} 超时（timeout）"))?
}

/// 证书校验：先用内置的 webpki 根证书（与以前完全一致）；只有报「未知签发者」时，再交给系统钥匙串判断，
/// 这样企业 / 自建邮箱用的私有 CA（已在系统里信任）也能连上，而公网证书的行为不变。
#[derive(Debug)]
struct Verifier {
    webpki: Arc<rustls::client::WebPkiServerVerifier>,
    platform: Option<rustls_platform_verifier::Verifier>,
}

impl rustls::client::danger::ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        match self.webpki.verify_server_cert(end_entity, intermediates, server_name, ocsp, now) {
            Err(rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)) if self.platform.is_some() => {
                self.platform.as_ref().expect("platform").verify_server_cert(end_entity, intermediates, server_name, ocsp, now)
            }
            r => r,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let webpki = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build().expect("webpki verifier");
        let platform = rustls_platform_verifier::Verifier::new(provider.clone()).map_err(|e| log::warn!("[tls] 系统证书校验不可用：{e}")).ok();
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("tls versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Verifier { webpki, platform }))
            .with_no_client_auth();
        Arc::new(cfg)
    })
    .clone()
}

pub async fn tls_wrap(host: &str, tcp: TcpStream) -> Result<Conn> {
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|_| anyhow!("主机名不正确：{host}"))?;
    let tls = tokio::time::timeout(CONNECT_TIMEOUT, TlsConnector::from(tls_config()).connect(name, tcp))
        .await
        .map_err(|_| anyhow!("TLS 握手超时（timeout）"))?
        .with_context(|| format!("与 {host} 的 TLS 握手失败"))?;
    Ok(Conn::Tls(Box::new(tls)))
}

/// 连接并（除本机回环外）立即 TLS。
pub async fn connect(host: &str, port: u16, proxy: Option<&Proxy>, tls: bool) -> Result<Conn> {
    // 经代理时：代理出口到某些服务（实测 Outlook）会把大部分新连接卡住不动，通的那条 1～3 秒就好。
    // 与其干等 30 秒，不如同时开 3 条连接、谁先通用谁（其余自动丢弃），6 秒都没通就换一轮，最多 3 轮。
    // 只对「超时」重试；证书 / 认证 / 拒绝连接这类立即报错的直接返回
    if proxy.is_some() && tls {
        const ROUNDS: u32 = 5;
        const WIDTH: usize = 3;
        const PER_ROUND: Duration = Duration::from_secs(6);
        for n in 1..=ROUNDS {
            let attempts: Vec<_> = (0..WIDTH).map(|_| Box::pin(connect_once(host, port, proxy, tls))).collect();
            match tokio::time::timeout(PER_ROUND, futures::future::select_ok(attempts)).await {
                Ok(Ok((c, _rest))) => return Ok(c),
                // 全部返回了错误：有一条不是超时类（证书、认证等）就直接报它
                Ok(Err(e)) if !e.to_string().contains("超时") => return Err(e),
                _ if n < ROUNDS => log::info!("[net] {host}:{port} 第 {n} 轮 {} 秒没通，换一轮重试", PER_ROUND.as_secs()),
                _ => bail!("连接 {host}:{port} 超时（timeout）：代理出口不稳定，已连续试了 {ROUNDS} 轮"),
            }
        }
        unreachable!("最后一轮一定返回");
    }
    connect_once(host, port, proxy, tls).await
}

async fn connect_once(host: &str, port: u16, proxy: Option<&Proxy>, tls: bool) -> Result<Conn> {
    let tcp = tunnel(host, port, proxy).await?;
    tcp.set_nodelay(true).ok();
    if tls {
        tls_wrap(host, tcp).await
    } else if is_loopback(host) {
        Ok(Conn::Plain(tcp))
    } else {
        bail!("出于安全考虑，只允许对本机地址使用明文连接")
    }
}

/// 读一行（含 CRLF 去除）。SMTP 用。
pub async fn read_line<R: AsyncRead + Unpin>(r: &mut BufReader<R>) -> Result<String> {
    let mut s = String::new();
    let n = tokio::time::timeout(Duration::from_secs(120), r.read_line(&mut s)).await.map_err(|_| anyhow!("服务器响应超时（timeout）"))??;
    if n == 0 {
        bail!("连接被服务器关闭（Socket closed）");
    }
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_proxies() {
        assert_eq!(Proxy::parse("socks5://127.0.0.1:1080").unwrap(), Proxy::Socks5 { host: "127.0.0.1".into(), port: 1080, auth: None });
        assert_eq!(Proxy::parse("http://127.0.0.1:1087/").unwrap(), Proxy::Http { host: "127.0.0.1".into(), port: 1087, auth: None });
        assert_eq!(
            Proxy::parse("http://bob:p%40ss%3Aw@proxy.lan:3128").unwrap(),
            Proxy::Http { host: "proxy.lan".into(), port: 3128, auth: Some(("bob".into(), "p@ss:w".into())) }
        );
        assert_eq!(Proxy::parse("socks5://[::1]:1080").unwrap(), Proxy::Socks5 { host: "::1".into(), port: 1080, auth: None });
        assert_eq!(Proxy::parse("http://127.0.0.1:80").unwrap(), Proxy::Http { host: "127.0.0.1".into(), port: 80, auth: None });
        assert!(Proxy::parse("http://127.0.0.1").is_err(), "必须写端口");
        assert_eq!(
            Proxy::parse("socks5h://u:p@proxy.lan:7890").unwrap(),
            Proxy::Socks5 { host: "proxy.lan".into(), port: 7890, auth: Some(("u".into(), "p".into())) }
        );
        assert!(Proxy::parse("127.0.0.1:1080").is_err());
        assert!(Proxy::parse("ftp://a:1").is_err());
        assert!(Proxy::parse("socks5://a:notaport").is_err());
    }

    #[test]
    fn proxy_required_never_falls_back_to_direct() {
        let p = Proxy::Http { host: "h".into(), port: 1, auth: None };
        assert_eq!(effective_proxy(false, Some(&p)).unwrap(), None);
        assert_eq!(effective_proxy(true, Some(&p)).unwrap(), Some(&p));
        assert!(effective_proxy(true, None).is_err());
    }

    /// 需要外网：cargo test --lib tls_handshake_public -- --ignored
    #[test]
    #[ignore]
    fn tls_handshake_public() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            connect("imap.qq.com", 993, None, true).await.expect("公网证书照常通过");
            let tcp = tunnel("self-signed.badssl.com", 443, None).await.unwrap();
            assert!(tls_wrap("self-signed.badssl.com", tcp).await.is_err(), "自签证书必须拒绝");
        });
    }
}
