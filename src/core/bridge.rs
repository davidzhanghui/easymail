//! 本机 HTTP 桥：把只读的 MCP 工具暴露在 127.0.0.1 上，给不想走 stdio 的本机程序用。
//!
//! 默认关闭。打开后要带 kv `http_bridge_token` 里的 Bearer 令牌。账号 / 文件夹授权和 MCP 是同一套。
//! 只接受 POST /tool，正文是 `{"name","arguments"}`。

use super::db::Store;
use super::mcp;
use rand::RngCore;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

pub const PORT: u16 = 47321;

static STARTED: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);

pub fn enabled(store: &Store) -> bool {
    store.get_kv("http_bridge").as_deref() == Some("1")
}

pub fn set_enabled(store: &Store, on: bool) {
    store.set_kv("http_bridge", if on { "1" } else { "0" });
    if on && store.get_kv("http_bridge_token").is_none_or(|t| t.is_empty()) {
        let mut buf = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut buf);
        store.set_kv("http_bridge_token", &hex_token(&buf));
    }
}

fn hex_token(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn token(store: &Store) -> String {
    store.get_kv("http_bridge_token").unwrap_or_default()
}

/// 按当前开关决定要不要听端口。已经在听时，关开关会让下一轮请求拒绝并退出。
pub fn apply(store: Store) {
    if enabled(&store) && mcp::enabled(&store) {
        STOP.store(false, Ordering::SeqCst);
        if STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        std::thread::spawn(move || {
            if let Err(e) = listen(store) {
                log::warn!("[bridge] {e:#}");
            }
            STARTED.store(false, Ordering::SeqCst);
        });
    } else {
        STOP.store(true, Ordering::SeqCst);
    }
}

fn listen(store: Store) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", PORT))?;
    listener.set_nonblocking(false)?;
    log::info!("[bridge] http://127.0.0.1:{PORT}/tool");
    for conn in listener.incoming() {
        if STOP.load(Ordering::SeqCst) || !enabled(&store) {
            break;
        }
        let Ok(mut conn) = conn else { continue };
        let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = [0u8; 64 * 1024];
        let n = conn.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let resp = handle(&store, &req);
        let _ = conn.write_all(resp.as_bytes());
    }
    Ok(())
}

fn handle(store: &Store, req: &str) -> String {
    if !enabled(store) || !mcp::enabled(store) {
        return http(403, r#"{"error":"本机接口已关闭"}"#);
    }
    let Some((head, body)) = req.split_once("\r\n\r\n") else {
        return http(400, r#"{"error":"请求不完整"}"#);
    };
    let first = head.lines().next().unwrap_or("");
    if first.starts_with("GET /health") {
        return http(200, r#"{"ok":true}"#);
    }
    if !first.starts_with("POST /tool") {
        return http(404, r#"{"error":"只有 POST /tool"}"#);
    }
    let bearer = head.lines().find_map(|l| l.strip_prefix("Authorization:").or_else(|| l.strip_prefix("authorization:"))).map(|s| s.trim()).unwrap_or("");
    let Some(got) = bearer.strip_prefix("Bearer ") else {
        return http(401, r#"{"error":"需要 Bearer 令牌"}"#);
    };
    if got != token(store) {
        return http(401, r#"{"error":"令牌不对"}"#);
    }
    let Ok(v) = serde_json::from_str::<Value>(body.trim()) else {
        return http(400, r#"{"error":"正文不是 JSON"}"#);
    };
    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
    let args = v.get("arguments").cloned().unwrap_or(json!({}));
    match mcp::call_tool(store, name, &args) {
        Ok(text) => http(200, &serde_json::to_string(&json!({"text": text})).unwrap_or_else(|_| "{}".into())),
        Err(e) => http(400, &serde_json::to_string(&json!({"error": format!("{e:#}")})).unwrap_or_else(|_| "{}".into())),
    }
}

fn http(code: u16, body: &str) -> String {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        _ => "Not Found",
    };
    format!("HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

static _KEEP: OnceLock<()> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_without_token_and_reads_when_open() {
        let _ = _KEEP.get_or_init(|| ());
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        crate::core::queries::tests::seed(&s);
        let closed = handle(&s, "POST /tool HTTP/1.1\r\n\r\n{\"name\":\"mail_stats\"}");
        assert!(closed.starts_with("HTTP/1.1 403"));
        mcp::set_enabled(&s, true);
        set_enabled(&s, true);
        let denied = handle(&s, "POST /tool HTTP/1.1\r\nAuthorization: Bearer no\r\n\r\n{\"name\":\"mail_stats\"}");
        assert!(denied.starts_with("HTTP/1.1 401"));
        let tok = token(&s);
        let ok = handle(&s, &format!("POST /tool HTTP/1.1\r\nAuthorization: Bearer {tok}\r\n\r\n{{\"name\":\"mail_stats\",\"arguments\":{{}}}}"));
        assert!(ok.contains("收件箱"), "{ok}");
    }
}
