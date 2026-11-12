//! 极简 SMTP 客户端：465 隐式 TLS / 587 STARTTLS，AUTH PLAIN / LOGIN，可走代理。
//!
//! 只实现发信必需的子集（EHLO、STARTTLS、AUTH、MAIL FROM、RCPT TO、DATA、QUIT），
//! 好处是和 IMAP 共用同一套代理 / TLS 连接代码。

use super::model::Account;
use super::net::{self, Conn, Proxy};
use anyhow::{anyhow, bail, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};

/// 普通命令的写超时
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);

/// 正文的写超时：按大小放宽（最慢按 20 KB/s 估算），至少 2 分钟
fn data_timeout(len: usize) -> Duration {
    Duration::from_secs((120 + len as u64 / (20 * 1024)).min(3600))
}

struct Smtp {
    io: BufReader<Conn>,
}

#[derive(Debug)]
struct Reply {
    code: u16,
    lines: Vec<String>,
}

impl Reply {
    fn text(&self) -> String {
        self.lines.join(" ")
    }
}

impl Smtp {
    async fn read(&mut self) -> Result<Reply> {
        let mut lines = Vec::new();
        loop {
            let l = net::read_line(&mut self.io).await?;
            // 用 get 而不是 [..3]：服务器返回非 ASCII 开头的行时按字节切片会 panic
            let code: u16 = l.get(..3).and_then(|c| c.parse().ok()).ok_or_else(|| anyhow!("SMTP 响应格式错误：{l}"))?;
            let more = l.as_bytes().get(3) == Some(&b'-');
            lines.push(l.get(4..).unwrap_or("").to_string());
            if !more {
                return Ok(Reply { code, lines });
            }
        }
    }

    async fn cmd(&mut self, line: &str) -> Result<Reply> {
        let mut buf = Vec::with_capacity(line.len() + 2);
        buf.extend_from_slice(line.as_bytes());
        buf.extend_from_slice(b"\r\n");
        self.write(&buf, WRITE_TIMEOUT).await?;
        self.read().await
    }

    /// 写入带超时：服务器停止读取（窗口满）时不会无限挂住发信
    async fn write(&mut self, data: &[u8], limit: Duration) -> Result<()> {
        let w = self.io.get_mut();
        tokio::time::timeout(limit, async {
            w.write_all(data).await?;
            w.flush().await
        })
        .await
        .map_err(|_| anyhow!("向 SMTP 服务器写入超时（{} 秒）", limit.as_secs()))??;
        Ok(())
    }

    async fn expect(&mut self, line: &str, ok: &[u16], what: &str) -> Result<Reply> {
        let r = self.cmd(line).await?;
        if !ok.contains(&r.code) {
            return Err(reply_error(&r, &format!("{what}被拒绝")));
        }
        Ok(r)
    }
}

/// 5xx 是永久性错误（收件人不存在、内容被拒…），重试也一样：标为「拒绝」；4xx 是临时错误，可以稍后重试
fn reply_error(r: &Reply, what: &str) -> anyhow::Error {
    let msg = format!("{what}：{} {}", r.code, r.text());
    if r.code >= 500 {
        super::error::rejected(msg)
    } else {
        super::error::connection(msg)
    }
}

fn ehlo_name() -> String {
    "easymail.local".to_string()
}

async fn open(acc: &Account, password: &str, proxy: Option<&Proxy>) -> Result<Smtp> {
    let proxy = net::effective_proxy(acc.use_proxy, proxy)?;
    let host = acc.smtp_host.as_str();
    let mut s = if acc.smtp_secure {
        Smtp { io: BufReader::new(net::connect(host, acc.smtp_port, proxy, true).await?) }
    } else {
        let tcp = net::tunnel(host, acc.smtp_port, proxy).await?;
        Smtp { io: BufReader::new(Conn::Plain(tcp)) }
    };
    let greet = s.read().await?;
    if greet.code != 220 {
        bail!("SMTP 服务器拒绝连接：{} {}", greet.code, greet.text());
    }
    let mut ehlo = s.expect(&format!("EHLO {}", ehlo_name()), &[250], "EHLO").await?;
    if !acc.smtp_secure {
        let has_starttls = ehlo.lines.iter().any(|l| l.eq_ignore_ascii_case("STARTTLS"));
        if has_starttls {
            s.expect("STARTTLS", &[220], "STARTTLS").await?;
            let Conn::Plain(tcp) = s.io.into_inner() else { bail!("连接状态异常") };
            s = Smtp { io: BufReader::new(net::tls_wrap(host, tcp).await?) };
            ehlo = s.expect(&format!("EHLO {}", ehlo_name()), &[250], "EHLO").await?;
        } else if !net::is_loopback(host) {
            bail!("服务器不支持 STARTTLS，出于安全考虑拒绝明文发送密码");
        }
    }
    let auth_line = ehlo.lines.iter().find(|l| l.to_uppercase().starts_with("AUTH")).map(|l| l.to_uppercase()).unwrap_or_default();
    let explain = |r: &Reply| {
        let base = format!("{} {}", r.code, r.text());
        if r.code == 535 || r.code == 534 || r.code == 530 {
            format!("SMTP 登录被拒绝：{}（{base}）", super::providers::provider(&acc.provider).auth_hint)
        } else {
            format!("SMTP 登录失败：{base}")
        }
    };
    if acc.is_oauth() {
        let token = B64.encode(super::oauth::xoauth2(&acc.email, password));
        let mut r = s.cmd(&format!("AUTH XOAUTH2 {token}")).await?;
        if r.code == 334 {
            // 服务器回了一段 base64 的错误详情：按 RFC 回空行结束，拿到最终的失败码
            r = s.cmd("").await?;
        }
        if r.code != 235 {
            return Err(super::error::auth(format!("SMTP OAuth 登录被拒绝：{} {}（请在账号设置里重新登录）", r.code, r.text())));
        }
    } else if auth_line.contains("PLAIN") || !auth_line.contains("LOGIN") {
        let token = B64.encode(format!("\0{}\0{}", acc.email, password));
        let r = s.cmd(&format!("AUTH PLAIN {token}")).await?;
        if r.code != 235 {
            return Err(super::error::auth(explain(&r)));
        }
    } else {
        s.expect("AUTH LOGIN", &[334], "AUTH LOGIN").await?;
        s.expect(&B64.encode(&acc.email), &[334], "AUTH LOGIN").await?;
        let r = s.cmd(&B64.encode(password)).await?;
        if r.code != 235 {
            return Err(super::error::auth(explain(&r)));
        }
    }
    Ok(s)
}

/// 行尾统一成 CRLF，并做点填充（以 . 开头的行前面再加一个 .）
pub fn dot_stuff(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 64);
    let mut line_start = true;
    let mut i = 0;
    while i < raw.len() {
        let b = raw[i];
        if line_start && b == b'.' {
            out.push(b'.');
        }
        match b {
            b'\r' if raw.get(i + 1) == Some(&b'\n') => {
                out.extend_from_slice(b"\r\n");
                i += 2;
                line_start = true;
                continue;
            }
            b'\n' | b'\r' => {
                out.extend_from_slice(b"\r\n");
                line_start = true;
            }
            _ => {
                out.push(b);
                line_start = false;
            }
        }
        i += 1;
    }
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out
}

pub async fn send(acc: &Account, password: &str, proxy: Option<&Proxy>, rcpts: &[String], raw: &[u8]) -> Result<()> {
    if rcpts.is_empty() {
        bail!("没有收件人");
    }
    let mut s = tokio::time::timeout(std::time::Duration::from_secs(60), open(acc, password, proxy)).await.map_err(|_| anyhow!("连接 SMTP 服务器超时"))??;
    s.expect(&format!("MAIL FROM:<{}>", acc.email), &[250], "发件人").await?;
    for r in rcpts {
        s.expect(&format!("RCPT TO:<{}>", r.trim()), &[250, 251], &format!("收件人 {r} ")).await?;
    }
    s.expect("DATA", &[354], "DATA").await?;
    let mut body = dot_stuff(raw);
    body.extend_from_slice(b".\r\n");
    s.write(&body, data_timeout(body.len())).await?;
    let r = s.read().await?;
    if r.code != 250 {
        return Err(reply_error(&r, "服务器拒收邮件"));
    }
    let _ = s.cmd("QUIT").await;
    Ok(())
}

pub async fn test_login(acc: &Account, password: &str, proxy: Option<&Proxy>) -> Result<()> {
    let mut s = tokio::time::timeout(std::time::Duration::from_secs(45), open(acc, password, proxy)).await.map_err(|_| anyhow!("连接 SMTP 服务器超时"))??;
    let _ = s.cmd("QUIT").await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuffing() {
        assert_eq!(dot_stuff(b"a\n.b\r\n..c"), b"a\r\n..b\r\n...c\r\n");
        assert_eq!(dot_stuff(b".x\r\n"), b"..x\r\n");
        assert_eq!(dot_stuff(b"x\ry"), b"x\r\ny\r\n");
    }

    #[test]
    fn data_timeout_scales() {
        assert_eq!(data_timeout(0).as_secs(), 120);
        assert_eq!(data_timeout(20 * 1024 * 100).as_secs(), 220);
        assert_eq!(data_timeout(usize::MAX / 2).as_secs(), 3600);
    }
}
