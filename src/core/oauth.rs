//! OAuth2（XOAUTH2）登录：Microsoft 365 / Outlook.com 已不再接受基本认证，Gmail 也在收紧应用专用密码。
//!
//! 流程参考 Thunderbird / aerc / mutt_oauth2.py：桌面「公共客户端」+ PKCE + 本机回环重定向。
//! 1. 在 127.0.0.1 上随便监听一个端口，生成 code_verifier / code_challenge(S256) / state
//! 2. 用系统浏览器打开授权页；用户登录、同意后浏览器跳回 http://localhost:端口/?code=…&state=…
//! 3. 用 code + code_verifier 换 access_token / refresh_token
//! 4. access_token 一小时左右过期，用 refresh_token 换新的（Microsoft 每次会回一个新的 refresh_token，要存回去）
//!
//! 令牌和密码一样 AES-256-GCM 加密后存在 accounts.secret 里（`oauth2:` + JSON），账号的 auth 列为 `oauth2`。

use super::db::{now_ms, Store};
use super::http;
use super::model::Account;
use super::net::Proxy;
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 内置的 Microsoft 应用（Azure「移动和桌面应用」平台，重定向 http://localhost，支持个人与组织账号）
pub const MS_CLIENT_ID: &str = "27a951d1-6157-4b99-89e3-26a805e97cda";
const MS_AUTH: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/authorize";
const MS_TOKEN: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";
const MS_SCOPE: &str = "offline_access openid email profile https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send";
const G_AUTH: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const G_TOKEN: &str = "https://oauth2.googleapis.com/token";
const G_SCOPE: &str = "openid email https://mail.google.com/";
pub const SECRET_PREFIX: &str = "oauth2:";
/// 等用户在浏览器里登录的最长时间
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Microsoft,
    Google,
}

impl Kind {
    pub fn for_provider(provider: &str) -> Option<Kind> {
        match provider {
            "outlook" => Some(Kind::Microsoft),
            "gmail" => Some(Kind::Google),
            _ => None,
        }
    }
    fn endpoints(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Kind::Microsoft => (MS_AUTH, MS_TOKEN, MS_SCOPE),
            Kind::Google => (G_AUTH, G_TOKEN, G_SCOPE),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Token {
    pub kind: Option<Kind>,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_secret: String,
    pub refresh_token: String,
    #[serde(default)]
    pub access_token: String,
    /// 毫秒时间戳
    #[serde(default)]
    pub expires_at: i64,
    /// 登录时识别出的邮箱地址
    #[serde(default)]
    pub email: String,
}

impl Token {
    pub fn to_secret(&self) -> String {
        format!("{SECRET_PREFIX}{}", serde_json::to_string(self).unwrap_or_default())
    }
    pub fn from_secret(s: &str) -> Option<Token> {
        serde_json::from_str(s.strip_prefix(SECRET_PREFIX)?).ok()
    }
    fn fresh(&self) -> bool {
        !self.access_token.is_empty() && self.expires_at > now_ms() + 120_000
    }
}

/// 客户端：Microsoft 用内置的；Google 用户必须在设置里填自己的（Google 桌面客户端还要 client_secret）
pub fn client_for(store: &Store, kind: Kind) -> Result<(String, String)> {
    match kind {
        Kind::Microsoft => {
            let custom = store.get_kv("oauth.ms_client_id").unwrap_or_default();
            Ok((if custom.trim().is_empty() { MS_CLIENT_ID.to_string() } else { custom.trim().to_string() }, String::new()))
        }
        Kind::Google => {
            let id = store.get_kv("oauth.google_client_id").unwrap_or_default();
            let secret = store.get_kv("oauth.google_client_secret").unwrap_or_default();
            if id.trim().is_empty() {
                bail!("Gmail 的 OAuth 登录需要先在「设置」里填写你自己的 Google Client ID（或继续使用应用专用密码）");
            }
            Ok((id.trim().to_string(), secret.trim().to_string()))
        }
    }
}

pub fn pkce() -> (String, String) {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn random_state() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// 交互式登录：开浏览器、等回调、换令牌。`open` 负责用系统浏览器打开地址（界面层传入）。
pub async fn login(kind: Kind, client_id: &str, client_secret: &str, hint: &str, proxy: Option<&Proxy>, open: impl FnOnce(&str)) -> Result<Token> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("无法在本机开启登录回调端口")?;
    let port = listener.local_addr()?.port();
    let redirect = format!("http://localhost:{port}");
    let (verifier, challenge) = pkce();
    let state = random_state();
    let (auth, token_url, scope) = kind.endpoints();
    let mut u = url::Url::parse(auth)?;
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("client_id", client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect)
            .append_pair("scope", scope)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state);
        if kind == Kind::Google {
            q.append_pair("access_type", "offline").append_pair("prompt", "consent");
        } else {
            q.append_pair("response_mode", "query").append_pair("prompt", "select_account");
        }
        if !hint.trim().is_empty() {
            q.append_pair("login_hint", hint.trim());
        }
    }
    open(u.as_str());
    let code = tokio::time::timeout(LOGIN_TIMEOUT, wait_code(&listener, &state)).await.map_err(|_| anyhow!("等待浏览器登录超时（5 分钟）"))??;
    let mut form = vec![
        ("client_id", client_id),
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("code_verifier", verifier.as_str()),
    ];
    if !client_secret.is_empty() {
        form.push(("client_secret", client_secret));
    }
    let r = http::post_form(token_url, &http::form(&form), proxy).await?;
    let mut t = token_from(&r, kind, client_id, client_secret, "")?;
    if t.refresh_token.is_empty() {
        bail!("服务器没有返回 refresh_token，请重试");
    }
    if t.email.is_empty() {
        t.email = hint.trim().to_lowercase();
    }
    Ok(t)
}

/// 等浏览器带着 code 回来；state 不对的请求直接拒绝（防 CSRF）
async fn wait_code(listener: &tokio::net::TcpListener, state: &str) -> Result<String> {
    loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = tokio::time::timeout(Duration::from_secs(10), sock.read(&mut buf)).await.unwrap_or(Ok(0)).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).to_string();
        let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("/").to_string();
        let u = url::Url::parse(&format!("http://localhost{path}")).ok();
        let get = |k: &str| u.as_ref().and_then(|u| u.query_pairs().find(|(a, _)| a == k).map(|(_, v)| v.into_owned()));
        let (body, result) = match (get("code"), get("state"), get("error")) {
            (Some(code), Some(s), _) if s == state => (page("登录成功", "可以关闭这个页面，回到 EasyMail 了。"), Some(Ok(code))),
            (_, _, Some(err)) => {
                let desc = get("error_description").unwrap_or_default();
                (page("登录失败", &format!("{err}：{desc}")), Some(Err(anyhow!("授权失败：{err} {desc}"))))
            }
            _ => ("".to_string(), None),
        };
        if result.is_none() {
            // favicon 之类的请求
            let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            continue;
        }
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.shutdown().await;
        return result.expect("result");
    }
}

fn page(title: &str, msg: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        "<!doctype html><meta charset=utf-8><title>{t}</title><body style=\"font:15px -apple-system,sans-serif;display:flex;align-items:center;justify-content:center;height:90vh;color:#333\"><div><h2>EasyMail · {t}</h2><p>{m}</p></div>",
        t = esc(title),
        m = esc(msg)
    )
}

fn token_from(r: &http::Response, kind: Kind, client_id: &str, client_secret: &str, old_refresh: &str) -> Result<Token> {
    let j = r.json()?;
    if !r.ok() || j.get("access_token").is_none() {
        let err = j.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
        let desc = j.get("error_description").and_then(|v| v.as_str()).unwrap_or("");
        let first = desc.lines().next().unwrap_or("");
        if err == "invalid_grant" {
            return Err(super::error::auth(format!("登录已失效，请在账号设置里重新登录（{first}）")));
        }
        bail!("换取令牌失败：{err} {first}");
    }
    let s = |k: &str| j.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let expires = j.get("expires_in").and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|x| x.parse().ok()))).unwrap_or(3600);
    let refresh = s("refresh_token");
    Ok(Token {
        kind: Some(kind),
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        refresh_token: if refresh.is_empty() { old_refresh.to_string() } else { refresh },
        access_token: s("access_token"),
        expires_at: now_ms() + expires * 1000,
        email: email_from_id_token(&s("id_token")).unwrap_or_default(),
    })
}

/// 从 id_token（JWT）里取邮箱：只用来预填表单，不做签名校验（令牌是我们自己刚从 HTTPS 端点拿到的）
pub fn email_from_id_token(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let j: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    ["email", "preferred_username", "upn"].iter().find_map(|k| j.get(*k).and_then(|v| v.as_str()).filter(|s| s.contains('@')).map(|s| s.to_lowercase()))
}

/// 诊断用：摘出 access_token 里的 aud / scp / tid（只解码不校验；消费者账号的令牌可能不是 JWT）。绝不包含令牌本身。
pub fn token_summary(jwt: &str) -> String {
    let claims = jwt
        .split('.')
        .nth(1)
        .and_then(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p.trim_end_matches('=')).ok())
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok());
    match claims {
        Some(j) => {
            let g = |k: &str| j.get(k).and_then(|v| v.as_str()).unwrap_or("-").to_string();
            format!("令牌 aud={} scp={} tid={}", g("aud"), g("scp"), g("tid"))
        }
        None => "令牌不是 JWT（消费者账号常见），无法解码".to_string(),
    }
}

/// 同一账号同时只刷新一次（多个连接同时发现过期时不重复换令牌，也避免旧 refresh_token 被用两次）
static REFRESH: Mutex<()> = Mutex::new(());

/// 取可用的 access_token；快过期就刷新并把新令牌写回库里
pub async fn access_token(store: &Store, acc: &Account, proxy: Option<&Proxy>) -> Result<String> {
    let t = current(store, acc)?;
    if t.fresh() {
        return Ok(t.access_token);
    }
    let kind = t.kind.ok_or_else(|| anyhow!("令牌缺少类型"))?;
    let (_, token_url, scope) = kind.endpoints();
    let mut form = vec![("client_id", t.client_id.as_str()), ("grant_type", "refresh_token"), ("refresh_token", t.refresh_token.as_str())];
    if kind == Kind::Microsoft {
        form.push(("scope", scope));
    }
    if !t.client_secret.is_empty() {
        form.push(("client_secret", t.client_secret.as_str()));
    }
    let r = http::post_form(token_url, &http::form(&form), proxy).await?;
    let mut nt = token_from(&r, kind, &t.client_id, &t.client_secret, &t.refresh_token)?;
    if nt.email.is_empty() {
        nt.email = t.email.clone();
    }
    {
        let _g = REFRESH.lock().unwrap_or_else(|e| e.into_inner());
        let enc = store.crypto().encrypt(&nt.to_secret());
        store.w().execute("UPDATE accounts SET secret = ? WHERE id = ?", rusqlite::params![enc, acc.id])?;
    }
    Ok(nt.access_token)
}

/// 库里最新的令牌（不用调用方手里可能已过时的 Account.secret）
fn current(store: &Store, acc: &Account) -> Result<Token> {
    let secret: String = store.r().query_row("SELECT secret FROM accounts WHERE id = ?", [acc.id], |r| r.get(0)).unwrap_or_else(|_| acc.secret.clone());
    let plain = store.crypto().decrypt(&secret)?;
    Token::from_secret(&plain).ok_or_else(|| anyhow!("账号的登录令牌损坏，请重新登录"))
}

/// SASL XOAUTH2 的初始响应（未 base64）
pub fn xoauth2(user: &str, token: &str) -> String {
    format!("user={user}\x01auth=Bearer {token}\x01\x01")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_and_secret_roundtrip() {
        let (v, c) = pkce();
        assert_eq!(v.len(), 43);
        assert_eq!(c, base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(v.as_bytes())));
        let t = Token { kind: Some(Kind::Microsoft), client_id: "x".into(), refresh_token: "r".into(), ..Default::default() };
        assert_eq!(Token::from_secret(&t.to_secret()), Some(t));
        assert!(Token::from_secret("plainpassword").is_none());
        assert_eq!(xoauth2("a@b.c", "T"), "user=a@b.c\x01auth=Bearer T\x01\x01");
    }

    #[test]
    fn id_token_email() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"preferred_username":"Me@Outlook.com"}"#);
        assert_eq!(email_from_id_token(&format!("h.{payload}.s")), Some("me@outlook.com".into()));
        assert_eq!(email_from_id_token("bad"), None);
    }

    #[test]
    fn rfc7636_vector() {
        let c = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"));
        assert_eq!(c, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }
}
