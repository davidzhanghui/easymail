//! 生成截图 / 宣传用的演示数据：往本机 GreenMail 投放一批逼真的中文邮件，建好账号并同步到指定数据目录。
//!
//! 用法：cargo run --example seed_demo -- <数据目录>
//! 由 scripts/screenshots.sh 调用（它负责以演示用户拉起 GreenMail：linxia@example.com、lin@studio.example.com，密码 pw）。

use anyhow::Result;
use chrono::{Duration, Local};
use easymail::core::db::Store;
use easymail::core::engine::Engine;
use easymail::core::imap::Imap;
use easymail::core::model::Account;
use easymail::core::{accounts, transfer};

const IMAP: u16 = 3143;
const SMTP: u16 = 3025;

fn account(email: &str) -> Account {
    Account {
        email: email.into(),
        name: email.split('@').next().unwrap().into(),
        provider: "custom".into(),
        imap_host: "127.0.0.1".into(),
        imap_port: IMAP,
        smtp_host: "127.0.0.1".into(),
        smtp_port: SMTP,
        smtp_secure: false,
        sync_enabled: true,
        ..Default::default()
    }
}

struct Mail {
    from: &'static str,
    to: &'static str,
    subject: &'static str,
    /// 距现在多少分钟
    ago: i64,
    flags: &'static str,
    html: Option<&'static str>,
    text: &'static str,
    attach: Option<(&'static str, &'static str, &'static [u8])>,
}

fn raw(m: &Mail, n: usize) -> Vec<u8> {
    let date = (Local::now() - Duration::minutes(m.ago)).to_rfc2822();
    let mut s = format!(
        "From: {}\r\nTo: {}\r\nSubject: =?UTF-8?B?{}?=\r\nDate: {date}\r\nMessage-ID: <demo-{n}@easymail.demo>\r\nMIME-Version: 1.0\r\n",
        enc_addr(m.from),
        enc_addr(m.to),
        b64(m.subject.as_bytes())
    );
    let alt = |html: &str| {
        format!(
            "Content-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n--alt\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--alt\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--alt--\r\n",
            b64_wrap(m.text.as_bytes()),
            b64_wrap(html.as_bytes())
        )
    };
    let body = match m.html {
        Some(h) => alt(h),
        None => format!("Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n", b64_wrap(m.text.as_bytes())),
    };
    match m.attach {
        Some((name, ctype, data)) => s.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"mix\"\r\n\r\n--mix\r\n{body}--mix\r\nContent-Type: {ctype}; name=\"=?UTF-8?B?{n}?=\"\r\nContent-Disposition: attachment; filename=\"=?UTF-8?B?{n}?=\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--mix--\r\n",
            b64_wrap(data),
            n = b64(name.as_bytes())
        )),
        None => s.push_str(&body),
    }
    s.into_bytes()
}

fn b64(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn b64_wrap(b: &[u8]) -> String {
    b64(b).as_bytes().chunks(76).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("\r\n")
}

/// "名字 <addr>" → 名字做 RFC 2047 编码
fn enc_addr(a: &str) -> String {
    match a.split_once(" <") {
        Some((name, rest)) => format!("=?UTF-8?B?{}?= <{rest}", b64(name.as_bytes())),
        None => a.to_string(),
    }
}

const ME: &str = "林夏 <linxia@example.com>";
const WORK: &str = "林夏 <lin@studio.example.com>";

const PDF: &[u8] = b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj 2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj 3 0 obj<</Type/Page/MediaBox[0 0 595 842]/Parent 2 0 R>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n";

const WEEKLY: &str = r#"<div style="font-family:-apple-system,'PingFang SC',sans-serif;max-width:620px;color:#1f2328;line-height:1.7">
<h2 style="margin:0 0 4px">产品周报 · 第 46 周</h2>
<p style="color:#6b6862;margin:0 0 18px">本周重点：离线队列上线、搜索提速、两项体验修复</p>
<table style="border-collapse:collapse;width:100%;font-size:14px">
<tr style="background:#f6f5f2"><th style="text-align:left;padding:8px 10px">事项</th><th style="text-align:left;padding:8px 10px">负责人</th><th style="text-align:left;padding:8px 10px">状态</th></tr>
<tr><td style="padding:8px 10px;border-top:1px solid #eee">离线操作队列</td><td style="padding:8px 10px;border-top:1px solid #eee">周明</td><td style="padding:8px 10px;border-top:1px solid #eee;color:#1a7f37">✔ 已上线</td></tr>
<tr><td style="padding:8px 10px;border-top:1px solid #eee">中文全文搜索提速 3×</td><td style="padding:8px 10px;border-top:1px solid #eee">林夏</td><td style="padding:8px 10px;border-top:1px solid #eee;color:#1a7f37">✔ 已上线</td></tr>
<tr><td style="padding:8px 10px;border-top:1px solid #eee">附件预览（Quick Look）</td><td style="padding:8px 10px;border-top:1px solid #eee">陈一</td><td style="padding:8px 10px;border-top:1px solid #eee;color:#9a6700">● 测试中</td></tr>
<tr><td style="padding:8px 10px;border-top:1px solid #eee">深色主题细节打磨</td><td style="padding:8px 10px;border-top:1px solid #eee">许可</td><td style="padding:8px 10px;border-top:1px solid #eee;color:#6b6862">○ 下周</td></tr>
</table>
<p style="margin-top:18px">下周一 10:00 例会照常，议程见 <a href="https://example.com/agenda">会议纪要</a>。有问题直接回复这封邮件。</p>
<p>—— 周明</p></div>"#;

const NEWSLETTER: &str = r#"<div style="font-family:-apple-system,'PingFang SC',sans-serif;max-width:600px;margin:auto">
<img src="https://cdn.example.com/banner.png" width="600" height="200" alt="本周精选">
<h2>独立开发者周刊 · 第 128 期</h2>
<p>本期精选：本地优先软件的复兴、用 Rust 写桌面应用的五个经验、以及一份极简主义邮件工作流。</p>
<img src="https://track.example.com/open.gif?u=lin" width="1" height="1">
<p><a href="https://example.com/issue/128">在浏览器中阅读 →</a></p></div>"#;

const FLIGHT: &str = r#"<div style="font-family:-apple-system,'PingFang SC',sans-serif;max-width:560px;border:1px solid #e7e5e0;border-radius:12px;padding:20px 24px">
<p style="color:#6b6862;margin:0">行程确认 · 订单号 EM2611208</p>
<h2 style="margin:6px 0 16px">上海虹桥 → 北京首都</h2>
<table style="font-size:15px;width:100%"><tr><td><b style="font-size:26px">08:30</b><br>SHA T2</td><td style="text-align:center;color:#6b6862">MU5101<br>——— 2 小时 15 分 ———</td><td style="text-align:right"><b style="font-size:26px">10:45</b><br>PEK T2</td></tr></table>
<p style="margin-top:16px">乘机人：林夏 · 经济舱 · 座位 32A</p></div>"#;

fn inbox() -> Vec<Mail> {
    vec![
        Mail {
            from: "周明 <zhouming@studio.example.com>",
            to: ME,
            subject: "产品周报 · 第 46 周：离线队列上线",
            ago: 12,
            flags: "",
            html: Some(WEEKLY),
            text: "产品周报 · 第 46 周\n离线操作队列已上线；中文全文搜索提速 3 倍。",
            attach: None,
        },
        Mail {
            from: "陈一 <chenyi@example.com>",
            to: ME,
            subject: "周末徒步路线定了，附上地图",
            ago: 48,
            flags: "",
            html: None,
            text: "夏夏：\n\n周六早上 7 点在龙井村口集合，走九溪十八涧，全程大约 9 公里。\n记得带水和防晒，中午在山里吃农家菜。\n\n路线图在附件里。\n\n一一",
            attach: Some(("徒步路线.pdf", "application/pdf", PDF)),
        },
        Mail {
            from: "云端航旅 <noreply@trip.example.com>",
            to: ME,
            subject: "行程确认：11 月 20 日 上海 → 北京 MU5101",
            ago: 130,
            flags: "\\Flagged",
            html: Some(FLIGHT),
            text: "行程确认：上海虹桥 08:30 → 北京首都 10:45，MU5101。",
            attach: None,
        },
        Mail {
            from: "独立开发者周刊 <weekly@news.example.com>",
            to: ME,
            subject: "第 128 期：本地优先软件的复兴",
            ago: 60 * 5,
            flags: "",
            html: Some(NEWSLETTER),
            text: "独立开发者周刊 第 128 期",
            attach: None,
        },
        Mail {
            from: "许可 <xuke@example.com>",
            to: ME,
            subject: "Re: 深色主题的配色稿",
            ago: 60 * 20,
            flags: "\\Seen",
            html: None,
            text: "我把侧栏底色往暖调了一点，对比度现在 7.2:1，你看看是不是更舒服。\n\n> 夜间模式下列表的分隔线有点抢眼……",
            attach: None,
        },
        Mail {
            from: "招商银行 <service@bank.example.com>",
            to: ME,
            subject: "您的 10 月信用卡电子账单",
            ago: 60 * 28,
            flags: "\\Seen",
            html: None,
            text: "尊敬的林女士：您 10 月账单应还金额 ¥3,286.40，到期还款日 11 月 25 日。",
            attach: None,
        },
        Mail {
            from: "GitHub <noreply@github.example.com>",
            to: ME,
            subject: "[easymail] PR #42 合并：支持 maildir 导出",
            ago: 60 * 33,
            flags: "\\Seen",
            html: None,
            text: "Merged #42 into main.\n\n+ Maildir 导出补齐 new/ tmp/ 与标准文件名",
            attach: None,
        },
        Mail {
            from: "妈妈 <mom@example.com>",
            to: ME,
            subject: "冬天的衣服寄出去了",
            ago: 60 * 50,
            flags: "\\Seen \\Flagged",
            html: None,
            text: "快递单号 SF1234567890，大概后天到。降温了多穿点。",
            attach: None,
        },
        Mail {
            from: "豆瓣读书 <books@example.com>",
            to: ME,
            subject: "你想读的《夜晚的潜水艇》到货了",
            ago: 60 * 72,
            flags: "\\Seen",
            html: None,
            text: "你标记为想读的图书现在有货了。",
            attach: None,
        },
        Mail {
            from: "Figma <team@figma.example.com>",
            to: ME,
            subject: "许可 邀请你编辑「迷你邮件 · 设计稿」",
            ago: 60 * 96,
            flags: "\\Seen",
            html: None,
            text: "许可 invited you to edit a file.",
            attach: None,
        },
    ]
}

fn work_inbox() -> Vec<Mail> {
    vec![
        Mail {
            from: "王总 <wang@client.example.com>",
            to: WORK,
            subject: "合同第二版已确认，下周签",
            ago: 35,
            flags: "",
            html: None,
            text: "林总监：\n\n第二版合同法务已经看过，没有问题。下周二上午来我们这儿签，顺便聊下一期需求。\n\n王",
            attach: Some(("合同-第二版.pdf", "application/pdf", PDF)),
        },
        Mail {
            from: "财务部 <finance@studio.example.com>",
            to: WORK,
            subject: "11 月报销单请在 20 号前提交",
            ago: 60 * 6,
            flags: "",
            html: None,
            text: "各位同事，11 月报销请于 20 日前在系统中提交。",
            attach: None,
        },
        Mail {
            from: "HR <hr@studio.example.com>",
            to: WORK,
            subject: "年会投票：今年去哪儿？",
            ago: 60 * 30,
            flags: "\\Seen",
            html: None,
            text: "选项：千岛湖 / 莫干山 / 安吉。",
            attach: None,
        },
    ]
}

fn sent() -> Vec<Mail> {
    vec![Mail {
        from: ME,
        to: "许可 <xuke@example.com>",
        subject: "深色主题的配色稿",
        ago: 60 * 22,
        flags: "\\Seen",
        html: None,
        text: "夜间模式下列表的分隔线有点抢眼，能不能再柔和一点？",
        attach: None,
    }]
}

#[tokio::main]
async fn main() -> Result<()> {
    let data = std::path::PathBuf::from(std::env::args().nth(1).expect("用法：seed_demo <数据目录>"));
    let mut n = 0;
    for (email, boxes) in [("linxia@example.com", vec![("INBOX", inbox()), ("Sent", sent())]), ("lin@studio.example.com", vec![("INBOX", work_inbox())])] {
        let mut c = Imap::connect(&account(email), "pw", None).await?;
        for f in ["Sent", "Trash", "Junk", "Drafts"] {
            c.create_folder(f).await.ok();
        }
        if email.starts_with("lin@") {
            c.create_folder("客户").await.ok();
        }
        for (folder, mails) in boxes {
            // 按时间从旧到新投放，UID 顺序与日期一致
            for m in mails.iter().rev() {
                n += 1;
                c.append(folder, &if m.flags.is_empty() { String::new() } else { format!("({})", m.flags) }, &raw(m, n)).await?;
            }
        }
        c.logout().await;
    }

    let store = Store::open(&data)?;
    let a1 = accounts::create(&store, &account("linxia@example.com"), "pw", "林夏", "—— 林夏")?;
    let a2 = accounts::create(&store, &account("lin@studio.example.com"), "pw", "林夏 · 工作室", "林夏 · 设计总监")?;
    accounts::ensure_local(&store)?;
    // 本地归档：导入一份 mbox
    let mbox = data.join("demo-archive.mbox");
    let mut buf = Vec::new();
    for m in [
        Mail {
            from: "旧同事 <old@example.com>",
            to: ME,
            subject: "2019 年团建照片合集",
            ago: 60 * 24 * 900,
            flags: "",
            html: None,
            text: "照片都在网盘里了。",
            attach: None,
        },
        Mail {
            from: "学校 <alumni@univ.example.edu>",
            to: ME,
            subject: "校友会：十周年返校邀请",
            ago: 60 * 24 * 600,
            flags: "",
            html: None,
            text: "诚邀返校。",
            attach: None,
        },
    ] {
        n += 1;
        buf.extend_from_slice(b"From demo@easymail Thu Jan  1 00:00:00 2020\n");
        buf.extend_from_slice(&raw(&m, n).iter().copied().filter(|&b| b != b'\r').collect::<Vec<_>>());
        buf.push(b'\n');
    }
    std::fs::write(&mbox, buf)?;
    transfer::import_file(&store, &mbox, "旧邮件归档", |_, _, _| {})?;
    std::fs::remove_file(&mbox).ok();

    let (tx, _rx) = futures::channel::mpsc::unbounded();
    let eng = Engine::new(store, tx)?;
    for id in [a1.id, a2.id] {
        let got = eng.sync_and_wait(id, None, true).await?;
        println!("账号 {id} 同步 {got} 封");
    }
    Ok(())
}
