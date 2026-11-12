use easymail::core::{crypto::Crypto, net};
use futures::StreamExt;
use std::time::Duration;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let data = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let crypto = Crypto::load_or_create(&data.join("master.key"))?;
    let db = rusqlite::Connection::open_with_flags(data.join(easymail::core::db::DB_NAME), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let (email, secret): (String, String) = db.query_row("select email,secret from accounts where id=5", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let pass = crypto.decrypt(&secret)?;
    for q in ["(UID BODY.PEEK[])", "(UID BODY[])", "(UID RFC822)"] {
        let conn = net::connect("imap.agentmail.to", 993, None, true).await?;
        let mut client = async_imap::Client::new(conn);
        let _ = client.read_response().await;
        let mut s = client.login(&email, &pass).await.map_err(|e| e.0)?;
        s.select("INBOX").await?;
        let r = tokio::time::timeout(Duration::from_secs(40), async {
            let v: Vec<_> = s.uid_fetch("3", q).await?.collect().await;
            anyhow::Ok(v.len())
        })
        .await;
        println!("{q} → {:?}", r.map(|x| x.map_err(|e| e.to_string())));
    }
    Ok(())
}
