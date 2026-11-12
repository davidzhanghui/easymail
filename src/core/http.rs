//! 极简 HTTPS 客户端：OAuth2 令牌、检查更新、一键退订、自动配置查询都只需要发一两个请求，
//! 不值得为此引入 reqwest + hyper 一整套。复用 `net::connect`，所以和收发信一样走用户设置的代理、同一套证书校验。
//!
//! 只支持 HTTP/1.1 + `Connection: close`，响应体支持 Content-Length / chunked / 读到关闭三种。

use super::net::{self, Proxy};
use anyhow::{anyhow, bail, Context, Result};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn json(&self) -> Result<serde_json::Value> {
        serde_json::from_slice(&self.body).with_context(|| format!("响应不是 JSON（HTTP {}）", self.status))
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// 表单编码（application/x-www-form-urlencoded）
pub fn form(pairs: &[(&str, &str)]) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    s.finish()
}

pub async fn get(url: &str, proxy: Option<&Proxy>) -> Result<Response> {
    request("GET", url, &[], None, proxy).await
}

pub async fn post_form(url: &str, body: &str, proxy: Option<&Proxy>) -> Result<Response> {
    request("POST", url, &[("Content-Type", "application/x-www-form-urlencoded")], Some(body.as_bytes()), proxy).await
}

/// 发一个请求；跟随最多 3 次重定向（只跟 GET）
pub async fn request(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>, proxy: Option<&Proxy>) -> Result<Response> {
    let mut url = url.to_string();
    for _ in 0..4 {
        let r = tokio::time::timeout(TIMEOUT, once(method, &url, headers, body, proxy)).await.map_err(|_| anyhow!("请求超时：{url}"))??;
        if method == "GET" && matches!(r.status, 301 | 302 | 303 | 307 | 308) {
            if let Some(loc) = r.header("location") {
                url = url::Url::parse(&url)?.join(loc)?.to_string();
                continue;
            }
        }
        return Ok(r);
    }
    bail!("重定向次数过多：{url}")
}

async fn once(method: &str, url: &str, headers: &[(&str, &str)], body: Option<&[u8]>, proxy: Option<&Proxy>) -> Result<Response> {
    let u = url::Url::parse(url).with_context(|| format!("地址不正确：{url}"))?;
    let host = u.host_str().ok_or_else(|| anyhow!("地址缺少主机：{url}"))?.to_string();
    let tls = match u.scheme() {
        "https" => true,
        "http" if net::is_loopback(&host) => false,
        "http" => bail!("只支持 https 地址：{url}"),
        s => bail!("不支持的协议：{s}"),
    };
    let port = u.port_or_known_default().unwrap_or(443);
    let mut conn = net::connect(&host, port, proxy, tls).await?;
    let path = match u.query() {
        Some(q) => format!("{}?{q}", u.path()),
        None => u.path().to_string(),
    };
    let mut req =
        format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: EasyMail/{}\r\nAccept: */*\r\nConnection: close\r\n", super::config::VERSION);
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    } else if method == "POST" {
        req.push_str("Content-Length: 0\r\n");
    }
    req.push_str("\r\n");
    conn.write_all(req.as_bytes()).await?;
    if let Some(b) = body {
        conn.write_all(b).await?;
    }
    conn.flush().await?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match conn.read(&mut buf).await {
            Ok(n) => n,
            // 有些服务器不发 close_notify 直接断开：已经读到完整响应就当正常结束
            Err(e) if !raw.is_empty() && e.kind() == std::io::ErrorKind::UnexpectedEof => 0,
            Err(e) => return Err(e.into()),
        };
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if raw.len() > MAX_BODY {
            bail!("响应过大");
        }
        if let Some(r) = parse_response(&raw)? {
            return Ok(r);
        }
    }
    parse_response_final(&raw)
}

/// 响应已完整时返回 Some
fn parse_response(raw: &[u8]) -> Result<Option<Response>> {
    let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else { return Ok(None) };
    let (status, headers) = parse_head(&raw[..end])?;
    let body = &raw[end + 4..];
    let get = |n: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone());
    if get("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        return Ok(dechunk(body).map(|b| Response { status, headers: headers.clone(), body: b }));
    }
    if let Some(len) = get("content-length").and_then(|v| v.trim().parse::<usize>().ok()) {
        if body.len() >= len {
            return Ok(Some(Response { status, headers, body: body[..len].to_vec() }));
        }
        return Ok(None);
    }
    if status == 204 || status == 304 {
        return Ok(Some(Response { status, headers, body: vec![] }));
    }
    Ok(None)
}

fn parse_response_final(raw: &[u8]) -> Result<Response> {
    if let Some(r) = parse_response(raw)? {
        return Ok(r);
    }
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| anyhow!("响应不完整"))?;
    let (status, headers) = parse_head(&raw[..end])?;
    Ok(Response { status, headers, body: raw[end + 4..].to_vec() })
}

fn parse_head(head: &[u8]) -> Result<(u16, Vec<(String, String)>)> {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or("");
    let status: u16 = first.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| anyhow!("响应状态行不正确：{first}"))?;
    let headers = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
    Ok((status, headers))
}

/// 解 chunked；数据还没收全返回 None
fn dechunk(mut b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let nl = b.windows(2).position(|w| w == b"\r\n")?;
        let size_s = String::from_utf8_lossy(&b[..nl]);
        let size = usize::from_str_radix(size_s.split(';').next()?.trim(), 16).ok()?;
        b = &b[nl + 2..];
        if size == 0 {
            return Some(out);
        }
        if b.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_length_and_chunked() {
        let r = parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello").unwrap().unwrap();
        assert_eq!((r.status, r.text().as_str()), (200, "hello"));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nhello").unwrap().is_none());
        let r = parse_response(b"HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").unwrap().unwrap();
        assert_eq!((r.status, r.text().as_str()), (201, "abcde"));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nab").unwrap().is_none());
        assert_eq!(form(&[("a", "1 2"), ("b", "x&y")]), "a=1+2&b=x%26y");
    }
}
