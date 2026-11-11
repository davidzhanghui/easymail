//! 验证：async-imap 经 socks5 连 Gmail、163 的 ID 命令、CONDSTORE CHANGEDSINCE、IDLE、trigram 中文搜索。
use easymail::core::{crypto::Crypto, net};
use futures::StreamExt;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let data = std::path::PathBuf::from(std::env::args().nth(1).expect("数据目录"));
    let crypto = Crypto::load_or_create(&data.join("master.key"))?;
    let db = rusqlite::Connection::open_with_flags(data.join("just-mail.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // ③ 旧库 + trigram 中文搜索
    let ver: String = db.query_row("select sqlite_version()", [], |r| r.get(0))?;
    let n: i64 = db.query_row("select count(*) from messages_fts where messages_fts match '\"验证码\"'", [], |r| r.get(0))?;
    let n2: i64 = db.query_row(
        "select count(*) from messages where id in (select rowid from messages_fts where messages_fts match '\"发票\" OR \"通知\"')",
        [],
        |r| r.get(0),
    )?;
    println!("[db] sqlite {ver}; FTS 「验证码」命中 {n}；「发票 OR 通知」命中 {n2}");
    let mut stmt = db.prepare("select id,email,provider,imap_host,imap_port,use_proxy,secret from accounts where provider!='local'")?;
    let accs: Vec<(i64, String, String, String, u16, bool, String)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get::<_, i64>(5)? == 1, r.get(6)?)))?.collect::<Result<_, _>>()?;
    let proxy = net::Proxy::parse("socks5://127.0.0.1:1080")?;
    for (id, email, provider, host, port, use_proxy, secret) in accs {
        let pass = crypto.decrypt(&secret)?.split_whitespace().collect::<String>();
        println!("\n[{id}] {email} ({provider}) via {}", if use_proxy { "socks5" } else { "直连" });
        let t = Instant::now();
        let conn = match net::connect(&host, port, use_proxy.then_some(&proxy), true).await {
            Ok(c) => c,
            Err(e) => {
                println!("  连接失败: {e:#}");
                continue;
            }
        };
        let mut client = async_imap::Client::new(conn);
        let _greet = client.read_response().await;
        let mut s = match client.login(&email, &pass).await {
            Ok(s) => s,
            Err((e, _)) => {
                println!("  登录失败: {e}");
                continue;
            }
        };
        println!("  登录成功 {:?}", t.elapsed());
        let caps = s.capabilities().await?;
        let has = |c: &str| caps.has_str(c);
        println!(
            "  CAP: IDLE={} CONDSTORE={} MOVE={} UIDPLUS={} ID={} SPECIAL-USE={}",
            has("IDLE"),
            has("CONDSTORE"),
            has("MOVE"),
            has("UIDPLUS"),
            has("ID"),
            has("SPECIAL-USE")
        );
        if has("ID") {
            let r = s.id([("name", Some("EasyMail")), ("version", Some("1.0.0")), ("vendor", Some("easymail"))]).await;
            println!("  ID → {:?}", r.map(|m| m.map(|m| m.get("name").cloned())));
        }
        let names: Vec<_> = s.list(None, Some("*")).await?.filter_map(|r| async { r.ok() }).collect().await;
        for nm in names.iter().take(12) {
            println!("    {} {:?} delim={:?}", nm.name(), nm.attributes(), nm.delimiter());
        }
        let mb = if has("CONDSTORE") { s.select_condstore("INBOX").await? } else { s.select("INBOX").await? };
        println!("  INBOX exists={} uidvalidity={:?} uidnext={:?} highestmodseq={:?}", mb.exists, mb.uid_validity, mb.uid_next, mb.highest_modseq);
        if let Some(ms) = mb.highest_modseq {
            let since = ms.saturating_sub(50);
            let q = format!("(UID FLAGS) (CHANGEDSINCE {since})");
            let items: Vec<_> = s.uid_fetch("1:*", &q).await?.filter_map(|r| async { r.ok() }).collect().await;
            println!("  CHANGEDSINCE {since} → {} 条，示例 {:?}", items.len(), items.first().map(|f| (f.uid, f.modseq, f.flags().collect::<Vec<_>>())));
        }
        let uids = s.uid_search("ALL").await?;
        println!("  UID SEARCH ALL → {} 个", uids.len());
        if let Some(maxuid) = uids.iter().max() {
            let items: Vec<_> = s
                .uid_fetch(maxuid.to_string(), "(UID RFC822.SIZE INTERNALDATE FLAGS ENVELOPE BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS (REFERENCES)])")
                .await?
                .filter_map(|r| async { r.ok() })
                .collect()
                .await;
            if let Some(f) = items.first() {
                let env = f.envelope();
                println!(
                    "  最新信封 uid={:?} size={:?} subject={:?}",
                    f.uid,
                    f.size,
                    env.and_then(|e| e.subject.as_ref()).map(|s| String::from_utf8_lossy(s).to_string())
                );
            }
            let raw: Vec<_> = s.uid_fetch(maxuid.to_string(), "(UID BODY.PEEK[])").await?.filter_map(|r| async { r.ok() }).collect().await;
            if let Some(body) = raw.first().and_then(|f| f.body()) {
                let m = mail_parser::MessageParser::default().parse(body);
                println!(
                    "  正文解析 subject={:?} text_len={}",
                    m.as_ref().and_then(|m| m.subject().map(String::from)),
                    m.as_ref().and_then(|m| m.body_text(0).map(|t| t.len())).unwrap_or(0)
                );
            }
        }
        if has("IDLE") {
            let mut idle = s.idle();
            idle.init().await?;
            let t = Instant::now();
            let (fut, _stop) = idle.wait_with_timeout(Duration::from_secs(4));
            let r = fut.await?;
            println!("  IDLE 4s → {:?} ({:?})", matches!(r, async_imap::extensions::idle::IdleResponse::Timeout), t.elapsed());
            let mut s = idle.done().await?;
            s.logout().await.ok();
        } else {
            println!("  服务器不支持 IDLE → 走 NOOP 轮询");
            s.noop().await?;
            s.logout().await.ok();
        }
    }
    Ok(())
}
