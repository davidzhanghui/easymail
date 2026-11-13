//! EasyMail 命令行：检索 / 阅读 / 导出本地邮件库，以及只读 MCP 服务器。
//!
//!   easymail-cli search "from:张三 发票" [--limit 20]
//!   easymail-cli show <id>
//!   easymail-cli stats
//!   easymail-cli folders
//!   easymail-cli attachments <id>
//!   easymail-cli attachment <id> <序号>
//!   easymail-cli export <mbox|csv> <输出文件> [搜索条件]
//!   easymail-cli backup <目标目录>
//!   easymail-cli mcp            # stdio MCP 服务器（只读，调用记录写入 mcp-audit.log）
//!
//! 数据目录与图形界面相同（`EASYMAIL_DATA_DIR` 可覆盖）；可以和图形界面同时运行（SQLite WAL）。

use easymail::core::{archive, config, db::Store, mcp, queries, transfer};
use serde_json::json;

fn usage() -> ! {
    eprintln!(
        "EasyMail 命令行 {}\n\n用法：\n  easymail-cli search <条件> [--limit N]\n  easymail-cli show <id>\n  easymail-cli attachments <id>\n  easymail-cli attachment <id> <序号>\n  easymail-cli stats\n  easymail-cli folders\n  easymail-cli export <mbox|csv> <输出文件> [条件]\n  easymail-cli backup <目标目录>\n  easymail-cli mcp\n\n读取邮件需要先在 App「工具 → 本地接口」里打开（默认关闭）。\n",
        config::VERSION
    );
    std::process::exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().cloned() else { usage() };
    let store = match Store::open(&config::default_data_dir()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("打开数据目录失败：{e:#}");
            std::process::exit(1);
        }
    };
    let r: anyhow::Result<()> = (|| {
        match cmd.as_str() {
            "search" => {
                let q = args.get(1).cloned().unwrap_or_default();
                let limit = args.iter().position(|a| a == "--limit").and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<u64>().ok()).unwrap_or(20);
                println!("{}", mcp::call_tool(&store, "search_mail", &json!({"query": q, "limit": limit}))?);
            }
            "show" => {
                let id: i64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or_else(|| usage());
                println!("{}", mcp::call_tool(&store, "read_mail", &json!({"id": id}))?);
            }
            "attachments" => {
                let id: i64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or_else(|| usage());
                println!("{}", mcp::call_tool(&store, "list_attachments", &json!({"id": id}))?);
            }
            "attachment" => {
                let id: i64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or_else(|| usage());
                let index: i64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or_else(|| usage());
                println!("{}", mcp::call_tool(&store, "get_attachment", &json!({"id": id, "index": index}))?);
            }
            "stats" => println!("{}", mcp::call_tool(&store, "mail_stats", &json!({}))?),
            "folders" => println!("{}", mcp::call_tool(&store, "list_folders", &json!({}))?),
            "export" => {
                let kind = match args.get(1).map(String::as_str) {
                    Some("mbox") => transfer::ExportKind::Mbox,
                    Some("csv") => transfer::ExportKind::Csv,
                    _ => usage(),
                };
                let dest = std::path::PathBuf::from(args.get(2).cloned().unwrap_or_else(|| usage()));
                let q = args.get(3).cloned().unwrap_or_default();
                let (tx, _rx) = futures::channel::mpsc::unbounded();
                let eng = easymail::core::engine::Engine::new(store.clone(), tx)?;
                let p = queries::ListParams { view: queries::View::All, q, ..Default::default() };
                let st = futures::executor::block_on(eng.call(transfer::export(eng.clone(), p, kind, dest, String::new())))?;
                println!(
                    "已导出 {} 封到 {}{}",
                    st.written,
                    st.path.display(),
                    if st.failed > 0 { format!("（{} 封正文无法取回）", st.failed) } else { String::new() }
                );
            }
            "backup" => {
                let dest = std::path::PathBuf::from(args.get(1).cloned().unwrap_or_else(|| usage()));
                // 密码从环境变量读，避免出现在进程列表里；设置了就生成 .embak 加密文件
                let rep = match std::env::var("EASYMAIL_BACKUP_PASSWORD").ok().filter(|p| !p.is_empty()) {
                    Some(pw) => easymail::core::backup_pack::backup_encrypted(&store, &dest, &pw)?,
                    None => archive::backup(&store, &dest)?,
                };
                println!(
                    "备份完成：{}\n邮件 {} 封，正文文件 {} 个，{}",
                    rep.dir.display(),
                    rep.messages,
                    rep.eml_copied,
                    if rep.verified { "校验通过" } else { "校验发现问题，见 校验报告.txt" }
                );
            }
            "mcp" => mcp::serve(&store)?,
            _ => usage(),
        }
        Ok(())
    })();
    if let Err(e) = r {
        eprintln!("出错：{e:#}");
        std::process::exit(1);
    }
}
