//! 邮件解析：信封头（同步时只拉头部）、完整原文（正文 / 附件 / cid 内嵌图）。
//!
//! 同步时不用 IMAP 的 ENVELOPE：实测 AgentMail 返回的 ENVELOPE 会让 async-imap 的解析器陷入死循环，
//! 而且 ENVELOPE 里的主题是未解码的 RFC 2047。改为取 `BODY.PEEK[HEADER.FIELDS (…)]` 交给 mail-parser，
//! 各种字符集（GBK / Big5 / ISO-2022-JP …）一并解决。

use super::model::{Addr, AttMeta};
use base64::Engine;
use mail_parser::{Address, HeaderValue, Message, MessageParser, MimeHeaders, PartType};

/// 同步时请求的头部字段
pub const HEADER_FIELDS: &str =
    "FROM TO CC REPLY-TO SUBJECT DATE MESSAGE-ID IN-REPLY-TO REFERENCES CONTENT-TYPE LIST-UNSUBSCRIBE LIST-UNSUBSCRIBE-POST AUTHENTICATION-RESULTS";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Envelope {
    pub subject: String,
    pub from: Addr,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub reply_to: String,
    pub date: Option<i64>,
    pub message_id: String,
    pub in_reply_to: String,
    pub refs: String,
    /// multipart/mixed 等：大概率带附件（正文取回后以真实附件为准）
    pub maybe_att: bool,
    /// List-Unsubscribe 原文（`<mailto:…>, <https://…>`）
    pub unsub: String,
    /// 有 `List-Unsubscribe-Post: List-Unsubscribe=One-Click`（RFC 8058 一键退订）
    pub unsub_post: bool,
    /// 收件服务器写的 Authentication-Results（SPF / DKIM / DMARC 结论）
    pub auth_res: String,
}

impl Envelope {
    /// 会话键：References 的第一个（会话的根）→ In-Reply-To → 自己的 Message-ID；都没有返回空串（调用方用 `id:<行号>`）
    pub fn thread_key(&self) -> String {
        thread_key(&self.message_id, &self.in_reply_to, &self.refs)
    }
}

pub fn thread_key(message_id: &str, in_reply_to: &str, refs: &str) -> String {
    refs.split_whitespace()
        .next()
        .map(String::from)
        .or_else(|| (!in_reply_to.is_empty()).then(|| in_reply_to.to_string()))
        .unwrap_or_else(|| message_id.to_string())
}

pub fn addrs(a: Option<&Address>) -> Vec<Addr> {
    a.map(|a| {
        a.iter()
            .map(|x| Addr { name: x.name().unwrap_or("").trim().to_string(), address: x.address().unwrap_or("").trim().to_string() })
            .filter(|x| !x.address.is_empty() || !x.name.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

fn ids(v: &HeaderValue) -> Vec<String> {
    match v {
        HeaderValue::Text(t) => vec![t.to_string()],
        HeaderValue::TextList(l) => l.iter().map(|s| s.to_string()).collect(),
        _ => vec![],
    }
}

fn angle(id: &str) -> String {
    let id = id.trim();
    if id.is_empty() || id.starts_with('<') {
        id.to_string()
    } else {
        format!("<{id}>")
    }
}

fn envelope_of(m: &Message) -> Envelope {
    let from = addrs(m.from()).into_iter().next().unwrap_or_default();
    let ct = m.content_type();
    let maybe_att = ct.is_some_and(|c| c.ctype().eq_ignore_ascii_case("multipart") && c.subtype().is_some_and(|s| s.eq_ignore_ascii_case("mixed")));
    Envelope {
        subject: m.subject().unwrap_or("").trim().to_string(),
        from: Addr { name: from.name, address: from.address.to_lowercase() },
        to: addrs(m.to()),
        cc: addrs(m.cc()),
        reply_to: addrs(m.reply_to()).into_iter().next().map(|a| a.address).unwrap_or_default(),
        date: m.date().map(|d| d.to_timestamp() * 1000),
        message_id: m.message_id().map(angle).unwrap_or_default(),
        in_reply_to: ids(m.in_reply_to()).first().map(|s| angle(s)).unwrap_or_default(),
        refs: ids(m.references()).iter().map(|s| angle(s)).collect::<Vec<_>>().join(" "),
        maybe_att,
        unsub: raw_header(m, "List-Unsubscribe"),
        unsub_post: raw_header(m, "List-Unsubscribe-Post").to_ascii_lowercase().contains("one-click"),
        auth_res: raw_header(m, "Authentication-Results"),
    }
}

/// 头部原文（折行展开、首尾去空白）
fn raw_header(m: &Message, name: &str) -> String {
    m.header_raw(name).map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default()
}

/// List-Unsubscribe 里的地址：(https 地址, mailto 地址)
pub fn unsub_targets(raw: &str) -> (Option<String>, Option<String>) {
    let mut http = None;
    let mut mailto = None;
    for part in raw.split(',') {
        let u = part.trim().trim_start_matches('<').trim_end_matches('>').trim();
        if (u.starts_with("https://") || u.starts_with("http://")) && http.is_none() {
            http = Some(u.to_string());
        } else if u.to_ascii_lowercase().starts_with("mailto:") && mailto.is_none() {
            mailto = Some(u.to_string());
        }
    }
    (http, mailto)
}

/// SPF / DKIM / DMARC 的结论（pass / fail / none …），取不到为空
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AuthVerdict {
    pub spf: String,
    pub dkim: String,
    pub dmarc: String,
}

impl AuthVerdict {
    /// 有明确失败
    pub fn failed(&self) -> bool {
        [&self.spf, &self.dkim, &self.dmarc].iter().any(|v| matches!(v.as_str(), "fail" | "softfail" | "permerror"))
    }
    pub fn is_empty(&self) -> bool {
        self.spf.is_empty() && self.dkim.is_empty() && self.dmarc.is_empty()
    }
}

pub fn auth_verdict(raw: &str) -> AuthVerdict {
    let get = |key: &str| {
        let l = raw.to_ascii_lowercase();
        let pat = format!("{key}=");
        l.match_indices(&pat)
            .find(|(i, _)| *i == 0 || !l.as_bytes()[i - 1].is_ascii_alphanumeric())
            .map(|(i, _)| l[i + pat.len()..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>())
            .unwrap_or_default()
    };
    AuthVerdict { spf: get("spf"), dkim: get("dkim"), dmarc: get("dmarc") }
}

/// 正文里引用的远程资源：(主机名列表（去重）, 疑似追踪像素个数)。
/// 追踪像素：1×1 / 0×0 的图片，或地址里带 open / track / pixel / beacon 这类字眼的图片。
pub fn remote_trackers(html: &str) -> (Vec<String>, usize) {
    use std::sync::LazyLock;
    static IMG: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"(?is)<img\b[^>]*>"#).expect("regex"));
    static SRC: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"(?i)\bsrc\s*=\s*["']?(https?://[^"'\s>]+)"#).expect("regex"));
    static TINY: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"(?i)\b(width|height)\s*[=:]\s*["']?\s*[01](px)?\b"#).expect("regex"));
    static HINT: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?i)(/open|track|pixel|beacon|/o\.gif|/wf/open|openrate|/e/o/)").expect("regex"));
    let mut hosts: Vec<String> = Vec::new();
    let mut pixels = 0;
    for tag in IMG.find_iter(html) {
        let tag = tag.as_str();
        let Some(src) = SRC.captures(tag).map(|c| c[1].to_string()) else { continue };
        if let Some(h) = url::Url::parse(&src).ok().and_then(|u| u.host_str().map(|h| h.to_lowercase())) {
            if !hosts.contains(&h) {
                hosts.push(h);
            }
        }
        if TINY.is_match(tag) || HINT.is_match(&src) {
            pixels += 1;
        }
    }
    (hosts, pixels)
}

/// 解析 `HEADER.FIELDS` 拿到的头部字节。
pub fn parse_headers(raw: &[u8]) -> Envelope {
    // 补一个空行，让解析器把它当成一封只有头部的信
    let mut buf = raw.to_vec();
    if !buf.ends_with(b"\r\n\r\n") && !buf.ends_with(b"\n\n") {
        buf.extend_from_slice(b"\r\n\r\n");
    }
    MessageParser::default().parse_headers(&buf).map(|m| envelope_of(&m)).unwrap_or_default()
}

/// 解析完整原文得到的可存储内容
#[derive(Clone, Debug, Default)]
pub struct Parsed {
    pub env: Envelope,
    pub text: String,
    pub html: String,
    pub snippet: String,
    pub atts: Vec<AttMeta>,
    /// 非内嵌附件个数
    pub files: usize,
    pub status: String,
    pub x_status: String,
}

pub fn html_to_text(html: &str) -> String {
    // 去掉 style / script / 图片后再转，避免把 CSS 当正文
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?is)<(style|script|head)[^>]*>.*?</(style|script|head)\s*>").expect("regex"));
    let re = &*RE;
    let cleaned = re.replace_all(html, "");
    let text = html2text::config::plain().no_link_wrapping().no_table_borders().string_from_read(cleaned.as_bytes(), 100_000).unwrap_or_default();
    // html2text 会把图片渲染成 [alt]，链接渲染成 [文字][n] —— 去掉角标
    static RE2: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"\[(\d+)\]").expect("regex"));
    let re2 = &*RE2;
    // 文末的脚注链接表（`[1]: https://…`）：角标已去掉，留着只是一串没头没尾的网址
    static FOOT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"^\[\d+\]:\s").expect("regex"));
    let text = text.lines().filter(|l| !FOOT.is_match(l)).collect::<Vec<_>>().join("\n");
    let text = re2.replace_all(&text, "");
    tidy_text(&text)
}

/// 整理 html2text 的输出：`<hr>` / 表格边框画成的整行制表符（─ ┬ │ …）改成一条短分隔线，
/// 否则一行 80~上万个字符，在阅读区里会折成一排排横线；再压缩连续空行。
pub fn tidy_text(text: &str) -> String {
    let is_rule = |l: &str| {
        let t = l.trim();
        t.chars().count() >= 8
            && t.chars()
                .all(|c| matches!(c, '─' | '━' | '┬' | '┴' | '┼' | '│' | '┌' | '┐' | '└' | '┘' | '├' | '┤' | '═' | '-' | '_' | '=') || c.is_whitespace())
    };
    let mut out: Vec<String> = Vec::new();
    let mut blank = 0;
    for l in text.lines() {
        let l = l.trim_end();
        let l = if is_rule(l) { "────────────────" } else { l };
        if l.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        // 相邻的分隔线只留一条
        if l.starts_with('─') && out.last().is_some_and(|p| p.starts_with('─')) {
            continue;
        }
        out.push(l.to_string());
    }
    out.join("\n").trim().to_string()
}

fn part_name(p: &mail_parser::MessagePart, i: usize) -> String {
    p.attachment_name().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("附件-{}", i + 1))
}

fn part_mime(p: &mail_parser::MessagePart) -> String {
    p.content_type()
        .map(|c| match c.subtype() {
            Some(s) => format!("{}/{}", c.ctype(), s).to_lowercase(),
            None => c.ctype().to_lowercase(),
        })
        .unwrap_or_else(|| "application/octet-stream".into())
}

fn is_inline(p: &mail_parser::MessagePart) -> bool {
    p.content_id().is_some() && !p.content_disposition().is_some_and(|d| d.is_attachment())
}

/// 显示用：从原文里取出内嵌图片，把 html 里的 `cid:xxx` 换成 data URI（离线也能看内嵌图）
pub fn inline_cids_from_raw(html: &str, raw: &[u8]) -> String {
    if !html.contains("cid:") {
        return html.to_string();
    }
    match MessageParser::default().parse(raw) {
        Some(m) => inline_cids(html, &m),
        None => html.to_string(),
    }
}

/// 把 html 里的 `cid:xxx` 换成 data URI
fn inline_cids(html: &str, m: &Message) -> String {
    if !html.contains("cid:") {
        return html.to_string();
    }
    let mut out = html.to_string();
    for p in m.attachments() {
        let Some(cid) = p.content_id() else { continue };
        let cid = cid.trim_matches(|c| c == '<' || c == '>');
        if cid.is_empty() {
            continue;
        }
        let needle = format!("cid:{cid}");
        if out.contains(&needle) {
            let data = format!("data:{};base64,{}", part_mime(p), base64::engine::general_purpose::STANDARD.encode(p.contents()));
            out = replace_cid(&out, &needle, &data);
        }
    }
    out
}

/// 只替换完整的 `cid:xxx` 引用：后面紧跟引号、括号、空白或结尾才算，`cid:logo1` 不会误伤 `cid:logo10`
fn replace_cid(html: &str, needle: &str, data: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find(needle) {
        let after = &rest[i + needle.len()..];
        let boundary = after.chars().next().is_none_or(|c| matches!(c, '"' | '\'' | ')' | '>' | ';' | '&') || c.is_whitespace());
        out.push_str(&rest[..i]);
        out.push_str(if boundary { data } else { needle });
        rest = after;
    }
    out.push_str(rest);
    out
}

pub fn parse_full(raw: &[u8]) -> Option<Parsed> {
    let m = MessageParser::default().parse(raw)?;
    let env = envelope_of(&m);
    // 库里只存原始 HTML（保留 cid:）：内嵌图片已经在 .eml 里了，不再 base64 展开一份写进数据库，
    // 显示时由 `inline_cids_from_raw` 现取替换
    let html = m.html_part(0).filter(|p| matches!(p.body, PartType::Html(_))).and_then(|p| p.text_contents().map(String::from)).unwrap_or_default();
    let text_part = m.text_part(0).filter(|p| matches!(p.body, PartType::Text(_))).and_then(|p| p.text_contents()).unwrap_or("").trim().to_string();
    // 纯 HTML 邮件没有 text 部分：转一份纯文本，用于摘要、全文索引和回复引用
    let text = if !text_part.is_empty() {
        text_part
    } else if !html.is_empty() {
        html_to_text(&html)
    } else {
        String::new()
    };
    let snippet: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect();
    let atts: Vec<AttMeta> = m
        .attachments()
        .enumerate()
        .map(|(i, p)| AttMeta { index: i, filename: part_name(p, i), content_type: part_mime(p), size: p.contents().len() as u64, inline: is_inline(p) })
        .collect();
    let files = atts.iter().filter(|a| !a.inline).count();
    let hdr = |name: &str| m.header_raw(name).map(|s| s.trim().to_string()).unwrap_or_default();
    Some(Parsed { env, text, html, snippet, atts, files, status: hdr("Status"), x_status: hdr("X-Status") })
}

/// 取第 index 个附件（与 att_json 的 index 对应）：(文件名, MIME, 内容)
pub fn attachment(raw: &[u8], index: usize) -> Option<(String, String, Vec<u8>)> {
    let m = MessageParser::default().parse(raw)?;
    let p = m.attachments().nth(index)?;
    Some((part_name(p, index), part_mime(p), p.contents().to_vec()))
}

/// 全部非内嵌附件（转发时附带原附件）
pub fn file_attachments(raw: &[u8]) -> Vec<(String, String, Vec<u8>)> {
    let Some(m) = MessageParser::default().parse(raw) else { return vec![] };
    m.attachments().enumerate().filter(|(_, p)| !is_inline(p)).map(|(i, p)| (part_name(p, i), part_mime(p), p.contents().to_vec())).collect()
}

/// 全文索引的「联系人」字段
pub fn people_text(from: &Addr, to: &[Addr], cc: &[Addr]) -> String {
    let mut v: Vec<&str> = vec![&from.name, &from.address];
    for a in to.iter().chain(cc) {
        v.push(&a.name);
        v.push(&a.address);
    }
    v.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    /// 稳定版工具链上的「穷人版 fuzz」：对一封真实结构的邮件做几千次随机字节变异，
    /// 所有解析入口都不能 panic。完整的覆盖率引导 fuzz 见 fuzz/（cargo +nightly fuzz run parse_message）。
    #[test]
    fn mutated_messages_never_panic() {
        let seed = concat!(
            "From: =?UTF-8?B?5byg5LiJ?= <a@b.com>\r\nTo: x@y.com\r\nSubject: =?GB2312?B?suLK1A==?=\r\n",
            "List-Unsubscribe: <https://u.example/x>, <mailto:u@example.com?subject=unsub>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n",
            "Authentication-Results: mx; spf=fail; dkim=pass; dmarc=fail\r\nMessage-ID: <m1@b.com>\r\nReferences: <r0@b.com> <r1@b.com>\r\n",
            "MIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=\"B\"\r\n\r\n--B\r\nContent-Type: text/html; charset=utf-8\r\n\r\n",
            "<p>验证码 123456</p><img src=\"cid:i1\"><img width=1 height=1 src=\"https://t.example/p.gif\"><a href=\"https://evil.example\">https://bank.com</a>\r\n",
            "--B\r\nContent-Type: image/png\r\nContent-ID: <i1>\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--B--\r\n"
        )
        .as_bytes()
        .to_vec();
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut rnd = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let specials = [b'<', b'>', b'"', b'=', b'?', b'\r', b'\n', b':', b';', b'-', 0xE4, 0xFF, 0];
        for _ in 0..3000 {
            let mut m = seed.clone();
            for _ in 0..(rnd() % 8 + 1) {
                let i = (rnd() as usize) % m.len().max(1);
                match rnd() % 4 {
                    0 => m[i] = specials[(rnd() as usize) % specials.len()],
                    1 => m.truncate(i),
                    2 => {
                        let b = m[i];
                        m.insert(i, b)
                    }
                    _ => {
                        if i < m.len() {
                            m.remove(i);
                        }
                    }
                }
                if m.is_empty() {
                    break;
                }
            }
            let env = super::parse_headers(&m);
            let _ = env.thread_key();
            if let Some(p) = super::parse_full(&m) {
                let _ = super::inline_cids_from_raw(&p.html, &m);
                let _ = super::remote_trackers(&p.html);
                let _ = crate::core::extract::phishing_links(&p.html);
                let _ = crate::core::extract::trips(&p.html, &p.text);
                let _ = crate::core::extract::verification_code(&p.env.subject, &p.text);
            }
            let _ = super::file_attachments(&m);
            let s = String::from_utf8_lossy(&m);
            let _ = super::unsub_targets(&s);
            let _ = super::auth_verdict(&s);
            let _ = super::html_to_text(&s);
            let _ = crate::core::queries::parse_query(&s);
            let _ = crate::core::extract::strip_tracking(&s);
        }
    }

    #[test]
    fn tidy_text_shortens_rules() {
        let long = "─".repeat(5000);
        let t = tidy_text(&format!("{long}\n你好\n{long}\n{long}\n\n\n\n再见\n┬──┴──┬"));
        assert!(t.lines().all(|l| l.chars().count() < 40), "{t}");
        assert!(!t.contains("\n\n\n"));
        let h = html_to_text("<p>a</p><hr><p>b <a href=\"http://x.y\">link</a></p>");
        assert!(!h.contains("http://x.y") && h.contains("link"), "{h}");
    }

    use super::*;

    const SAMPLE: &str = "From: =?UTF-8?B?5byg5LiJ?= <Zhang@Example.com>\r\n\
To: bob@ex.com, \"Carol\" <carol@ex.com>\r\n\
Cc: dave@ex.com\r\n\
Reply-To: replies@ex.com\r\n\
Subject: =?gb2312?B?xOO6ww==?= world\r\n\
Date: Tue, 04 Nov 2025 10:00:00 +0800\r\n\
Message-ID: <abc@ex.com>\r\n\
In-Reply-To: <prev@ex.com>\r\n\
References: <root@ex.com> <prev@ex.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b1\"\r\n\
\r\n\
--b1\r\n\
Content-Type: multipart/related; boundary=\"b2\"\r\n\
\r\n\
--b2\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><head><style>p{color:red}</style></head><body><p>Hello <b>World</b></p><img src=\"cid:logo1\"></body></html>\r\n\
--b2\r\n\
Content-Type: image/png\r\n\
Content-ID: <logo1>\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--b2--\r\n\
--b1\r\n\
Content-Type: application/pdf; name=\"=?UTF-8?B?5Y+R56WoLnBkZg==?=\"\r\n\
Content-Disposition: attachment; filename=\"=?UTF-8?B?5Y+R56WoLnBkZg==?=\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQ=\r\n\
--b1--\r\n";

    #[test]
    fn headers_decoded() {
        let head = SAMPLE.split("\r\n\r\n").next().unwrap();
        let e = parse_headers(head.as_bytes());
        assert_eq!(e.subject, "你好 world");
        assert_eq!(e.from, Addr { name: "张三".into(), address: "zhang@example.com".into() });
        assert_eq!(e.to.len(), 2);
        assert_eq!(e.to[1].name, "Carol");
        assert_eq!(e.cc[0].address, "dave@ex.com");
        assert_eq!(e.reply_to, "replies@ex.com");
        assert_eq!(e.message_id, "<abc@ex.com>");
        assert_eq!(e.in_reply_to, "<prev@ex.com>");
        assert_eq!(e.refs, "<root@ex.com> <prev@ex.com>");
        assert_eq!(e.date, Some(1762221600000));
        assert!(e.maybe_att);
    }

    #[test]
    fn full_parse() {
        let p = parse_full(SAMPLE.as_bytes()).unwrap();
        assert!(p.html.contains("cid:logo1") && !p.html.contains("base64"), "库里存原始 HTML: {}", p.html);
        let shown = inline_cids_from_raw(&p.html, SAMPLE.as_bytes());
        assert!(shown.contains("data:image/png;base64,iVBORw0KGgo="), "显示时现取内联: {shown}");
        assert!(p.text.contains("Hello") && p.text.contains("World"));
        assert!(!p.text.contains("color:red"));
        assert_eq!(p.atts.len(), 2);
        assert!(p.atts[0].inline);
        assert_eq!(p.atts[1].filename, "发票.pdf");
        assert_eq!(p.atts[1].content_type, "application/pdf");
        assert_eq!(p.files, 1);
        let (n, t, data) = attachment(SAMPLE.as_bytes(), 1).unwrap();
        assert_eq!((n.as_str(), t.as_str()), ("发票.pdf", "application/pdf"));
        assert!(data.starts_with(b"%PDF"));
        assert_eq!(file_attachments(SAMPLE.as_bytes()).len(), 1);
    }

    #[test]
    fn cid_replacement_is_exact() {
        assert_eq!(replace_cid(r#"<img src="cid:a1"><img src="cid:a10">"#, "cid:a1", "X"), r#"<img src="X"><img src="cid:a10">"#);
        assert_eq!(replace_cid("url(cid:a1)", "cid:a1", "X"), "url(X)");
    }

    #[test]
    fn extras_unsub_auth_trackers_thread() {
        let raw = b"From: n@shop.com\r\nSubject: sale\r\nMessage-ID: <m@x>\r\nReferences: <root@x> <p@x>\r\nList-Unsubscribe: <mailto:u@shop.com?subject=unsub>, <https://shop.com/u?id=1>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\nAuthentication-Results: mx.163.com; spf=pass smtp.mailfrom=shop.com; dkim=fail header.d=shop.com; dmarc=pass\r\nContent-Type: text/html\r\n\r\n<p>hi</p><img src=\"https://t.shop.com/open?id=1\" width=\"1\" height=\"1\"><img src='https://cdn.shop.com/a.png'>\r\n";
        let p = parse_full(raw).unwrap();
        assert!(p.env.unsub_post);
        assert_eq!(unsub_targets(&p.env.unsub), (Some("https://shop.com/u?id=1".into()), Some("mailto:u@shop.com?subject=unsub".into())));
        let v = auth_verdict(&p.env.auth_res);
        assert_eq!((v.spf.as_str(), v.dkim.as_str(), v.dmarc.as_str()), ("pass", "fail", "pass"));
        assert!(v.failed());
        let (hosts, pixels) = remote_trackers(&p.html);
        assert_eq!(hosts, vec!["t.shop.com", "cdn.shop.com"]);
        assert_eq!(pixels, 1);
        assert_eq!(p.env.thread_key(), "<root@x>");
        assert_eq!(thread_key("<a@x>", "", ""), "<a@x>");
        assert_eq!(thread_key("<a@x>", "<p@x>", ""), "<p@x>");
    }

    #[test]
    fn gbk_plain_text() {
        let raw = b"From: a@b.c\r\nSubject: =?GBK?B?suLK1A==?=\r\nContent-Type: text/plain; charset=GBK\r\nContent-Transfer-Encoding: base64\r\n\r\nxOO6w6Ost8XVy7rDwcs=\r\n";
        let p = parse_full(raw).unwrap();
        assert_eq!(p.env.subject, "测试");
        assert!(p.text.starts_with("你好"), "{}", p.text);
        assert!(p.html.is_empty());
        assert_eq!(p.snippet, p.text);
    }
}
