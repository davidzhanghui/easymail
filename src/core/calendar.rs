//! 日历邀请（iCalendar / iMIP）：解析邮件里的 `text/calendar`，显示活动信息，
//! 回复「接受 / 待定 / 拒绝」（给组织者发一封带 `METHOD:REPLY` 的邮件），或交给「日历」App 添加。
//!
//! 只实现需要的那部分：第一个 VEVENT 的常用属性。时区用 TZID 的原文展示（没有时区数据库，不做换算），
//! UTC（以 Z 结尾）的时间换成本机时区。

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct When {
    /// 能确定绝对时间时的毫秒时间戳（UTC 时间 / 全天活动按本机时区的 0 点）
    pub epoch_ms: Option<i64>,
    /// 给人看的文字
    pub text: String,
    pub all_day: bool,
    /// 原始属性行（回复时原样带回去），如 `DTSTART;TZID=Asia/Shanghai:20251113T100000`
    pub line: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Person {
    pub name: String,
    pub email: String,
    /// ACCEPTED / DECLINED / TENTATIVE / NEEDS-ACTION
    pub partstat: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Invite {
    /// REQUEST / REPLY / CANCEL / PUBLISH …（缺省按 REQUEST）
    pub method: String,
    pub uid: String,
    pub summary: String,
    pub start: When,
    pub end: Option<When>,
    pub location: String,
    pub description: String,
    pub organizer: Option<Person>,
    pub attendees: Vec<Person>,
    pub sequence: i64,
    pub rrule: String,
}

/// 展开折行并拆成（属性名, 参数, 值）
fn lines(ics: &str) -> Vec<(String, Vec<(String, String)>, String, String)> {
    let mut unfolded: Vec<String> = Vec::new();
    for l in ics.replace("\r\n", "\n").split('\n') {
        if (l.starts_with(' ') || l.starts_with('\t')) && !unfolded.is_empty() {
            unfolded.last_mut().expect("last").push_str(&l[1..]);
        } else {
            unfolded.push(l.to_string());
        }
    }
    unfolded
        .into_iter()
        .filter_map(|l| {
            // 冒号可能出现在带引号的参数值里：找第一个不在引号内的冒号
            let mut in_q = false;
            let mut colon = None;
            for (i, c) in l.char_indices() {
                match c {
                    '"' => in_q = !in_q,
                    ':' if !in_q => {
                        colon = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            let i = colon?;
            let (head, value) = (&l[..i], &l[i + 1..]);
            let mut parts = head.split(';');
            let name = parts.next()?.to_uppercase();
            let params = parts.filter_map(|p| p.split_once('=').map(|(k, v)| (k.to_uppercase(), v.trim_matches('"').to_string()))).collect();
            Some((name, params, value.to_string(), l.clone()))
        })
        .collect()
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn when(params: &[(String, String)], value: &str, line: &str) -> When {
    let tzid = params.iter().find(|(k, _)| k == "TZID").map(|(_, v)| v.clone());
    let date_only = params.iter().any(|(k, v)| k == "VALUE" && v == "DATE") || value.len() == 8;
    let mut w = When { line: line.to_string(), ..Default::default() };
    if date_only {
        if let Ok(d) = NaiveDate::parse_from_str(&value[..8.min(value.len())], "%Y%m%d") {
            w.all_day = true;
            w.text = d.format("%Y-%m-%d").to_string();
            w.epoch_ms = Local.from_local_datetime(&d.and_hms_opt(0, 0, 0).expect("0")).single().map(|t| t.timestamp_millis());
        }
        return w;
    }
    let utc = value.ends_with('Z');
    let v = value.trim_end_matches('Z');
    if let Ok(n) = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S") {
        if utc {
            let t = Utc.from_utc_datetime(&n).with_timezone(&Local);
            w.epoch_ms = Some(t.timestamp_millis());
            w.text = t.format("%Y-%m-%d %H:%M").to_string();
        } else {
            w.text = format!("{}{}", n.format("%Y-%m-%d %H:%M"), tzid.map(|z| format!("（{z}）")).unwrap_or_default());
        }
    } else {
        w.text = value.to_string();
    }
    w
}

fn person(params: &[(String, String)], value: &str) -> Person {
    let email = value.trim().trim_start_matches("mailto:").trim_start_matches("MAILTO:").to_string();
    let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    Person { name: get("CN"), email, partstat: get("PARTSTAT") }
}

/// 解析第一个 VEVENT；不是日历 / 没有活动返回 None
pub fn parse(ics: &str) -> Option<Invite> {
    let all = lines(ics);
    if !all.iter().any(|(n, _, v, _)| n == "BEGIN" && v.eq_ignore_ascii_case("VEVENT")) {
        return None;
    }
    let mut inv = Invite { method: "REQUEST".into(), ..Default::default() };
    let mut in_event = false;
    let mut done = false;
    for (name, params, value, line) in &all {
        match name.as_str() {
            "METHOD" if !in_event => inv.method = value.trim().to_uppercase(),
            "BEGIN" if value.eq_ignore_ascii_case("VEVENT") && !done => in_event = true,
            "END" if value.eq_ignore_ascii_case("VEVENT") && in_event => {
                in_event = false;
                done = true;
            }
            _ if !in_event => {}
            "UID" => inv.uid = value.trim().to_string(),
            "SUMMARY" => inv.summary = unescape(value),
            "LOCATION" => inv.location = unescape(value),
            "DESCRIPTION" => inv.description = unescape(value),
            "DTSTART" => inv.start = when(params, value.trim(), line),
            "DTEND" => inv.end = Some(when(params, value.trim(), line)),
            "ORGANIZER" => inv.organizer = Some(person(params, value)),
            "ATTENDEE" => inv.attendees.push(person(params, value)),
            "SEQUENCE" => inv.sequence = value.trim().parse().unwrap_or(0),
            "RRULE" => inv.rrule = value.trim().to_string(),
            _ => {}
        }
    }
    done.then_some(inv)
}

impl Invite {
    /// 「2025-11-13 10:00 – 11:00」
    pub fn when_text(&self) -> String {
        let s = &self.start.text;
        match &self.end {
            Some(e) if !e.text.is_empty() => {
                // 同一天只写结束时间
                match (s.split_once(' '), e.text.split_once(' ')) {
                    (Some((d1, _)), Some((d2, t2))) if d1 == d2 => format!("{s} – {t2}"),
                    _ if self.start.all_day => s.clone(),
                    _ => format!("{s} – {}", e.text),
                }
            }
            _ => s.clone(),
        }
    }

    /// 邮件账号对应的参会人状态
    pub fn my_status(&self, my_email: &str) -> Option<&Person> {
        self.attendees.iter().find(|a| a.email.eq_ignore_ascii_case(my_email))
    }
}

/// 回复用的状态
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rsvp {
    Accept,
    Tentative,
    Decline,
}

impl Rsvp {
    pub fn partstat(self) -> &'static str {
        match self {
            Rsvp::Accept => "ACCEPTED",
            Rsvp::Tentative => "TENTATIVE",
            Rsvp::Decline => "DECLINED",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Rsvp::Accept => "已接受",
            Rsvp::Tentative => "待定",
            Rsvp::Decline => "已拒绝",
        }
    }

    pub fn from_partstat(s: &str) -> Option<Rsvp> {
        match s {
            "ACCEPTED" => Some(Rsvp::Accept),
            "TENTATIVE" => Some(Rsvp::Tentative),
            "DECLINED" => Some(Rsvp::Decline),
            _ => None,
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace('\n', "\\n")
}

/// 回复用的 iCalendar（METHOD:REPLY）。DTSTART / DTEND 原样带回，让对方的日历能对上同一场活动
pub fn reply_ics(inv: &Invite, my_email: &str, my_name: &str, r: Rsvp) -> String {
    let mut out = vec![
        "BEGIN:VCALENDAR".to_string(),
        "PRODID:-//EasyMail//Calendar//CN".into(),
        "VERSION:2.0".into(),
        "METHOD:REPLY".into(),
        "BEGIN:VEVENT".into(),
        format!("UID:{}", inv.uid),
        format!("SEQUENCE:{}", inv.sequence),
        format!("DTSTAMP:{}", Utc::now().format("%Y%m%dT%H%M%SZ")),
    ];
    if !inv.start.line.is_empty() {
        out.push(inv.start.line.clone());
    }
    if let Some(e) = inv.end.as_ref().filter(|e| !e.line.is_empty()) {
        out.push(e.line.clone());
    }
    if !inv.summary.is_empty() {
        out.push(format!("SUMMARY:{}", esc(&inv.summary)));
    }
    if let Some(o) = &inv.organizer {
        out.push(format!("ORGANIZER:mailto:{}", o.email));
    }
    let cn = if my_name.is_empty() { String::new() } else { format!(";CN=\"{}\"", my_name.replace('"', "")) };
    out.push(format!("ATTENDEE;PARTSTAT={}{cn}:mailto:{my_email}", r.partstat()));
    out.push("END:VEVENT".into());
    out.push("END:VCALENDAR".into());
    out.join("\r\n") + "\r\n"
}

/// 组装回复邮件（给组织者）。返回（原文，收件人，Message-ID）
pub fn build_reply(inv: &Invite, from_email: &str, from_name: &str, r: Rsvp) -> anyhow::Result<(Vec<u8>, Vec<String>, String)> {
    use mail_builder::mime::MimePart;
    use mail_builder::MessageBuilder;
    let org = inv.organizer.as_ref().filter(|o| !o.email.is_empty()).ok_or_else(|| anyhow::anyhow!("邀请里没有组织者地址，无法回复"))?;
    let domain = from_email.split('@').nth(1).unwrap_or("localhost");
    let mid = format!("{}@{}", uuid::Uuid::new_v4(), domain);
    let verb = match r {
        Rsvp::Accept => "已接受",
        Rsvp::Tentative => "待定",
        Rsvp::Decline => "已拒绝",
    };
    let subject = format!("{verb}：{}", inv.summary);
    let who = if from_name.is_empty() { from_email.to_string() } else { format!("{from_name} <{from_email}>") };
    let text = format!("{who} {verb}了邀请：{}\n时间：{}\n", inv.summary, inv.when_text());
    let ics = reply_ics(inv, from_email, from_name, r);
    let cal =
        MimePart::new(mail_builder::headers::content_type::ContentType::new("text/calendar").attribute("method", "REPLY").attribute("charset", "UTF-8"), ics);
    let root = MimePart::new("multipart/alternative", vec![MimePart::new("text/plain", text), cal]);
    let raw = MessageBuilder::new()
        .from((from_name.to_string(), from_email.to_string()))
        .to((org.name.clone(), org.email.clone()))
        .subject(subject)
        .message_id(mid.clone())
        .date(Utc::now().timestamp())
        .header("X-Mailer", mail_builder::headers::text::Text::new(format!("EasyMail {}", super::config::VERSION)))
        .body(root)
        .write_to_vec()?;
    Ok((raw, vec![org.email.clone()], format!("<{mid}>")))
}

/// 回复邀请：给组织者发一封 METHOD:REPLY 邮件，并把结果记在本机（下次打开同一邀请时显示）
pub async fn respond(eng: super::engine::Engine, msg_id: i64, r: Rsvp) -> anyhow::Result<()> {
    use anyhow::Context;
    let store = eng.store().clone();
    let acc_id: i64 = store.r().query_row("SELECT account_id FROM messages WHERE id = ?", [msg_id], |x| x.get(0))?;
    let acc = super::accounts::get(&store, acc_id)?.context("邮件所属账号已删除")?;
    if acc.is_local() {
        anyhow::bail!("「本地归档」里的邀请不能回复（它不是真实的邮箱账号）");
    }
    let raw = eng.source(msg_id).await?;
    let inv = super::parse::calendar_part(&raw).and_then(|ics| parse(&ics)).context("这封邮件里没有日历邀请")?;
    // 回复里的参会人地址：邀请里写的是这个账号就用它，否则也用账号地址（别名转发的情况对方可能认不出，但总比不回好）
    let (mail, rcpts, _mid) = build_reply(&inv, &acc.email, acc.display_name(), r)?;
    let pw = super::accounts::credential(&store, &acc, eng.proxy().as_ref()).await?;
    super::smtp::send(&acc, &pw, eng.proxy().as_ref(), &rcpts, &mail).await.context("发送回复失败")?;
    store.set_kv(&format!("rsvp.{}", inv.uid), r.partstat());
    Ok(())
}

/// 交给 macOS「日历」添加：写成临时 .ics 再 `open -a Calendar`
pub fn add_to_calendar(ics: &str, uid: &str) -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join("easymail-ics");
    std::fs::create_dir_all(&dir)?;
    let safe: String = uid.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(40).collect();
    let path = dir.join(format!("{}.ics", if safe.is_empty() { "invite".into() } else { safe }));
    std::fs::write(&path, ics)?;
    let st = std::process::Command::new("/usr/bin/open").args(["-a", "Calendar"]).arg(&path).status()?;
    if !st.success() {
        anyhow::bail!("无法打开「日历」");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:abc-123@x.com\r\nSEQUENCE:2\r\nDTSTART:20251113T020000Z\r\nDTEND:20251113T030000Z\r\nSUMMARY:周会\\, 评审\r\nLOCATION:会议室 A\r\nDESCRIPTION:第一行\\n第二行\r\nORGANIZER;CN=\"张三\":mailto:zhang@x.com\r\nATTENDEE;CN=\"我\";PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:me@x.com\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:li@x.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn parses_a_request() {
        let i = parse(ICS).unwrap();
        assert_eq!((i.method.as_str(), i.uid.as_str(), i.sequence), ("REQUEST", "abc-123@x.com", 2));
        assert_eq!(i.summary, "周会, 评审");
        assert_eq!(i.location, "会议室 A");
        assert_eq!(i.description, "第一行\n第二行");
        assert_eq!(i.organizer.as_ref().unwrap().email, "zhang@x.com");
        assert_eq!(i.organizer.as_ref().unwrap().name, "张三");
        assert_eq!(i.attendees.len(), 2);
        assert_eq!(i.my_status("ME@x.com").unwrap().partstat, "NEEDS-ACTION");
        // UTC 换成本机时区：用同样的方式算期望值，不依赖运行机器的时区
        let exp = Utc.with_ymd_and_hms(2025, 11, 13, 2, 0, 0).unwrap().with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string();
        assert_eq!(i.start.text, exp);
        assert!(i.when_text().starts_with(&exp) && i.when_text().contains(" – "));
    }

    #[test]
    fn tzid_all_day_folding_and_non_calendar() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:u1\nDTSTART;TZID=Asia/Shanghai:20251113T100000\nDTEND;TZID=Asia/Shanghai:20251113T110000\nSUMMARY:很长很长很长\n 的标题\nEND:VEVENT\nEND:VCALENDAR\n";
        let i = parse(ics).unwrap();
        assert_eq!(i.summary, "很长很长很长的标题");
        assert_eq!(i.start.text, "2025-11-13 10:00（Asia/Shanghai）");
        assert_eq!(i.when_text(), "2025-11-13 10:00（Asia/Shanghai） – 11:00（Asia/Shanghai）");
        let d = parse("BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:u2\nDTSTART;VALUE=DATE:20251201\nSUMMARY:放假\nEND:VEVENT\nEND:VCALENDAR").unwrap();
        assert!(d.start.all_day);
        assert_eq!(d.when_text(), "2025-12-01");
        assert!(parse("hello").is_none());
        assert!(parse("BEGIN:VCALENDAR\nEND:VCALENDAR").is_none());
    }

    #[test]
    fn reply_is_well_formed() {
        let i = parse(ICS).unwrap();
        let (raw, rcpts, mid) = build_reply(&i, "me@x.com", "我", Rsvp::Accept).unwrap();
        assert_eq!(rcpts, vec!["zhang@x.com"]);
        assert!(mid.starts_with('<') && mid.ends_with("@x.com>"));
        let head = String::from_utf8_lossy(&raw);
        assert!(head.contains("multipart/alternative") && head.contains("text/calendar"), "{head}");
        let ics = crate::core::parse::calendar_part(&raw).expect("回复里有 text/calendar 部分");
        assert!(
            ics.contains("METHOD:REPLY")
                && ics.contains("PARTSTAT=ACCEPTED")
                && ics.contains("UID:abc-123@x.com")
                && ics.contains("ORGANIZER:mailto:zhang@x.com"),
            "{ics}"
        );
        // 回复本身能被解析回来，且保留了 DTSTART 原行
        let back = parse(&reply_ics(&i, "me@x.com", "我", Rsvp::Decline)).unwrap();
        assert_eq!(back.method, "REPLY");
        assert_eq!(back.attendees[0].partstat, "DECLINED");
        assert_eq!(back.start.line, i.start.line);
        assert_eq!(Rsvp::from_partstat("TENTATIVE"), Some(Rsvp::Tentative));
        let mut no_org = i.clone();
        no_org.organizer = None;
        assert!(build_reply(&no_org, "me@x.com", "", Rsvp::Accept).is_err());
    }
}
