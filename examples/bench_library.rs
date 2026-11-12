//! 大库性能基准：生成 N 封（默认 10 万）合成邮件，测列表、计数、会话视图、搜索、发件人汇总、统计的耗时。
//!
//! ```sh
//! scripts/bench.sh            # 10 万封
//! scripts/bench.sh 300000     # 30 万封
//! ```
//!
//! 数据只写到临时目录，跑完即删；不碰真实数据目录。

use easymail::core::db::Store;
use easymail::core::queries::{self, ListParams, View};
use std::time::Instant;

const WORDS: &[&str] = &[
    "发票",
    "合同",
    "会议",
    "周报",
    "报销",
    "项目",
    "上线",
    "评审",
    "invoice",
    "meeting",
    "report",
    "deploy",
    "budget",
    "review",
    "offer",
    "travel",
    "机票",
    "酒店",
    "验证码",
    "订单",
    "快递",
    "账单",
    "newsletter",
    "security",
    "alert",
    "welcome",
];
const SENDERS: usize = 2_000;

fn main() -> anyhow::Result<()> {
    let n: i64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(100_000);
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path())?;
    let t0 = Instant::now();
    {
        let mut c = store.w();
        let tx = c.transaction()?;
        tx.execute_batch(
            "INSERT INTO accounts(id,email,name,provider,imap_host,smtp_host,secret,created_at,color) VALUES(1,'me@bench.test','我','custom','h','h','x',0,'accent');
             INSERT INTO folders(id,account_id,path,name,role) VALUES(1,1,'INBOX','INBOX','inbox'),(2,1,'Sent','Sent','sent'),(3,1,'Archive','Archive','archive');",
        )?;
        let mut seed: u64 = 42;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for id in 1..=n {
            let w = |r: u64| WORDS[(r as usize) % WORDS.len()];
            let subject = format!("{} {} {} #{id}", w(rnd()), w(rnd()), w(rnd()));
            let s = (rnd() as usize) % SENDERS;
            let (fname, faddr) = (format!("发件人{s}"), format!("user{s}@corp{}.test", s % 50));
            let body = format!("{} {} {} {} 正文内容 {id}", w(rnd()), w(rnd()), w(rnd()), w(rnd()));
            let folder = match rnd() % 10 {
                0 => 2,
                1 | 2 => 3,
                _ => 1,
            };
            let thread = format!("<t{}@bench>", id / 3); // 平均 3 封一个会话
            tx.execute(
                "INSERT INTO messages(id,account_id,folder_id,uid,subject,from_name,from_addr,to_json,date,seen,flagged,has_att,message_id,text,thread,size,body_state)
                 VALUES(?,1,?,?,?,?,?,'[]',?,?,?,?,?,?,?,?,1)",
                rusqlite::params![id, folder, id, subject, fname, faddr, 1_600_000_000_000i64 + id * 60_000, (rnd() % 4 != 0) as i64, (rnd() % 30 == 0) as i64, (rnd() % 8 == 0) as i64, format!("<m{id}@bench>"), body, thread, 8_000 + (rnd() % 200_000) as i64],
            )?;
            Store::fts_upsert(&tx, id, &subject, &format!("{fname} {faddr}"), &body)?;
        }
        tx.commit()?;
    }
    println!("生成 {n} 封：{:.1}s（含全文索引）", t0.elapsed().as_secs_f64());
    let db_mb = std::fs::metadata(dir.path().join(easymail::core::db::DB_NAME)).map(|m| m.len()).unwrap_or(0) as f64 / 1048576.0;
    println!("数据库大小：{db_mb:.0} MB\n");

    let cases: Vec<(&str, Box<dyn Fn() -> anyhow::Result<usize>>)> = vec![
        ("收件箱首屏 60 封（含计数）", Box::new(|| Ok(queries::list(&store, &ListParams { view: View::Inbox, ..Default::default() }, None, 60)?.items.len()))),
        (
            "收件箱翻页（不计数）",
            Box::new(|| {
                Ok(queries::list_page(
                    &store,
                    &ListParams { view: View::Inbox, ..Default::default() },
                    Some((1_600_000_000_000 + n / 2 * 60_000, n / 2)),
                    60,
                    false,
                )?
                .items
                .len())
            }),
        ),
        ("会话视图首屏", Box::new(|| Ok(queries::list(&store, &ListParams { view: View::Inbox, threads: true, ..Default::default() }, None, 60)?.items.len()))),
        ("搜索 英文词 invoice", Box::new(|| Ok(queries::list(&store, &ListParams { q: "invoice".into(), ..Default::default() }, None, 60)?.items.len()))),
        ("搜索 中文两字 发票", Box::new(|| Ok(queries::list(&store, &ListParams { q: "发票".into(), ..Default::default() }, None, 60)?.items.len()))),
        (
            "搜索 组合 from:user7 合同 -周报",
            Box::new(|| Ok(queries::list(&store, &ListParams { q: "from:user7 合同 -周报".into(), ..Default::default() }, None, 60)?.items.len())),
        ),
        (
            "搜索 has:att larger:100k",
            Box::new(|| Ok(queries::list(&store, &ListParams { q: "has:att larger:100k".into(), ..Default::default() }, None, 60)?.items.len())),
        ),
        ("发件人汇总 前 200", Box::new(|| Ok(queries::senders(&store, None, 200)?.len()))),
        ("侧栏统计", Box::new(|| queries::stats(&store).map(|_| 1))),
    ];
    println!("{:<34} {:>9} {:>9} {:>7}", "场景", "首次 ms", "中位 ms", "结果");
    for (name, f) in cases {
        let t = Instant::now();
        let rows = f()?;
        let first = t.elapsed().as_secs_f64() * 1000.0;
        let mut v: Vec<f64> = (0..5)
            .map(|_| {
                let t = Instant::now();
                let _ = f();
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("{name:<30} {first:>9.1} {:>9.1} {rows:>7}", v[2]);
    }
    Ok(())
}
