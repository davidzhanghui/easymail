//! 本地只读 MCP 服务器（Model Context Protocol，stdio + JSON-RPC 2.0）。
//!
//! 让 Claude Desktop / Cursor 等本机 AI 工具检索、阅读你的本地邮件库，**只读**：不能发信、删信、改标记，
//! 不联网（只查本地 SQLite 与 .eml 缓存）。每一次工具调用都追加写入数据目录下的 `mcp-audit.log`，
//! 可以随时查看 AI 读过哪些邮件。
//!
//! 配置示例（Claude Desktop 的 claude_desktop_config.json）：
//! ```json
//! { "mcpServers": { "easymail": { "command": "/Applications/EasyMail.app/Contents/MacOS/easymail-cli", "args": ["mcp"] } } }
//! ```

use super::db::Store;
use super::queries::{self, ListParams, View};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const PROTOCOL: &str = "2024-11-05";
/// 单封正文返回给模型的最大字符数
const MAX_BODY: usize = 20_000;

fn tools() -> Value {
    json!([
        {
            "name": "search_mail",
            "description": "搜索本地邮件库（所有账号）。支持 EasyMail 搜索语法：普通关键词、from:、to:、subject:、is:unread、is:starred、has:attachment、before:YYYY-MM-DD、after:YYYY-MM-DD、filename:、in:sent、-排除词。返回 id、时间、发件人、主题、摘要。",
            "inputSchema": {"type": "object", "properties": {
                "query": {"type": "string", "description": "搜索条件"},
                "limit": {"type": "integer", "description": "最多返回条数，默认 20，最大 100"}
            }, "required": ["query"]}
        },
        {
            "name": "read_mail",
            "description": "按 id 读取一封邮件的头部与纯文本正文（正文未缓存时只有摘要）。",
            "inputSchema": {"type": "object", "properties": {"id": {"type": "integer"}}, "required": ["id"]}
        },
        {
            "name": "list_folders",
            "description": "列出所有账号与文件夹，以及每个文件夹的邮件数和未读数。",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "mail_stats",
            "description": "邮件库概况：收件箱、未读、星标、草稿、本地已缓存数量。",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ])
}

fn audit(store: &Store, tool: &str, args: &Value, summary: &str) {
    let line = format!("{}\t{tool}\t{}\t{summary}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), args);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(store.dir().join("mcp-audit.log")) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn fmt_time(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ms).single().map(|t| t.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// 执行一个工具，返回给模型的文本
pub fn call_tool(store: &Store, name: &str, args: &Value) -> anyhow::Result<String> {
    match name {
        "search_mail" => {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).clamp(1, 100) as usize;
            let page = queries::list(store, &ListParams { view: View::All, q: q.clone(), ..Default::default() }, None, limit)?;
            audit(store, name, args, &format!("{} 条结果", page.items.len()));
            let mut out = format!(
                "共 {} 封（显示 {} 封）\n",
                if page.total > queries::COUNT_CAP { format!("{}+", queries::COUNT_CAP) } else { page.total.to_string() },
                page.items.len()
            );
            for m in page.items {
                out.push_str(&format!(
                    "\n[id {}] {} | {} <{}> | {} | {}\n  {}\n",
                    m.id,
                    fmt_time(m.date),
                    m.from.name,
                    m.from.address,
                    m.account_email,
                    m.subject,
                    m.snippet.chars().take(160).collect::<String>()
                ));
            }
            Ok(out)
        }
        "read_mail" => {
            let id = args.get("id").and_then(|v| v.as_i64()).ok_or_else(|| anyhow::anyhow!("缺少 id"))?;
            let d = queries::detail(store, id)?.ok_or_else(|| anyhow::anyhow!("邮件 {id} 不存在"))?;
            audit(store, name, args, &d.subject);
            let addrs = |v: &[super::model::Addr]| {
                v.iter().map(|a| if a.name.is_empty() { a.address.clone() } else { format!("{} <{}>", a.name, a.address) }).collect::<Vec<_>>().join(", ")
            };
            let mut body = if d.text.trim().is_empty() && !d.html.is_empty() { super::parse::html_to_text(&d.html) } else { d.text.clone() };
            if body.trim().is_empty() {
                body = "（正文还没有缓存到本地，请在 EasyMail 里打开一次这封邮件）".into();
            }
            let body: String = body.chars().take(MAX_BODY).collect();
            let atts = d.attachments.iter().map(|a| a.filename.clone()).collect::<Vec<_>>().join(", ");
            Ok(format!(
                "主题：{}\n发件人：{} <{}>\n收件人：{}\n抄送：{}\n时间：{}\n文件夹：{} / {}\n附件：{}\n\n{}",
                d.subject,
                d.from.name,
                d.from.address,
                addrs(&d.to),
                addrs(&d.cc),
                fmt_time(d.date),
                d.account_email,
                d.folder_label,
                if atts.is_empty() { "无".into() } else { atts },
                body
            ))
        }
        "list_folders" => {
            let fs = queries::folders(store)?;
            let accs = super::accounts::list(store)?;
            audit(store, name, args, &format!("{} 个文件夹", fs.len()));
            let mut out = String::new();
            for a in accs {
                out.push_str(&format!("\n{}（{}）\n", a.email, a.provider_name()));
                for f in fs.iter().filter(|f| f.account_id == a.id) {
                    out.push_str(&format!("  - {}：{} 封，未读 {}\n", f.label(), f.local, f.unseen));
                }
            }
            Ok(out)
        }
        "mail_stats" => {
            let s = queries::stats(store)?;
            audit(store, name, args, "");
            Ok(format!(
                "收件箱 {}，未读 {}，星标 {}，草稿 {}，本地共 {} 封，其中 {} 封已缓存正文",
                s.inbox, s.unread, s.starred, s.drafts, s.archived, s.with_body
            ))
        }
        other => anyhow::bail!("未知工具：{other}"),
    }
}

/// 处理一条 JSON-RPC 消息，返回要回复的消息（通知不回复）
pub fn handle(store: &Store, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "easymail", "version": super::config::VERSION},
            "instructions": "EasyMail 本地邮件库（只读）。先用 search_mail 找到邮件 id，再用 read_mail 读正文。"
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let p = msg.get("params").cloned().unwrap_or(Value::Null);
            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = p.get("arguments").cloned().unwrap_or(json!({}));
            Ok(match call_tool(store, name, &args) {
                Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
                Err(e) => json!({"content": [{"type": "text", "text": format!("出错：{e:#}")}], "isError": true}),
            })
        }
        m if m.starts_with("notifications/") => return None,
        _ => Err(json!({"code": -32601, "message": format!("不支持的方法：{method}")})),
    };
    let id = id?;
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    })
}

/// stdio 主循环：一行一条 JSON
pub fn serve(store: &Store) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(v) => handle(store, &v),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}})),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}")?;
            out.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_roundtrip_and_audit() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        crate::core::queries::tests::seed(&s);
        let init = handle(&s, &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}})).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "easymail");
        assert!(handle(&s, &json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
        let list = handle(&s, &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 4);
        let r = handle(&s, &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_mail","arguments":{"query":"增值税"}}})).unwrap();
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("[id 1]"), "{text}");
        let r = handle(&s, &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"read_mail","arguments":{"id":1}}})).unwrap();
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("增值税发票已开具"));
        let r = handle(&s, &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"delete_mail","arguments":{}}})).unwrap();
        assert_eq!(r["result"]["isError"], true, "没有写操作工具");
        let log = std::fs::read_to_string(dir.path().join("mcp-audit.log")).unwrap();
        assert_eq!(log.lines().count(), 2);
        assert!(log.contains("search_mail") && log.contains("read_mail"));
    }
}
