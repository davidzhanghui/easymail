//! 发信 / 草稿 / 附件。
//!
//! 附件：选中的文件先复制到 data/uploads/<token>（草稿里只存 token），发信成功后删除。
//! 已发送：Gmail / QQ 服务端必存；其余先按 Message-ID 在「已发送」里查（163 实测会自己存），查不到才 APPEND。

use super::accounts;
use super::db::{now_ms, Store};
use super::engine::{Engine, Op};
use super::model::{Draft, DraftAtt};
use super::parse;
use super::providers;
use super::smtp;
use anyhow::{anyhow, bail, Context, Result};
use mail_builder::headers::address::Address;
use mail_builder::mime::MimePart;
use mail_builder::MessageBuilder;
use rusqlite::{params, OptionalExtension};
use std::path::Path;

pub const MAX_UPLOAD: u64 = 25 * 1024 * 1024;

// ---------------- 附件 ----------------

pub fn mime_of(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" | "log" | "md" => "text/plain",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "eml" => "message/rfc822",
        _ => "application/octet-stream",
    }
}

// ---------------- 正文里的内嵌图片 ----------------
// 草稿正文是纯文本；图片用独占一行的标记 `\u{FFFC}<upload token>\u{FFFC}` 表示，文件就是普通的上传（uploads/<token>）。

pub const IMG_MARK: char = '\u{FFFC}';

#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Text(String),
    Image(String),
}

pub fn img_line(token: &str) -> String {
    format!("{IMG_MARK}{token}{IMG_MARK}")
}

fn img_token(line: &str) -> Option<&str> {
    let t = line.trim_end_matches('\r').strip_prefix(IMG_MARK)?.strip_suffix(IMG_MARK)?;
    (!t.is_empty() && !t.contains(char::is_whitespace) && !t.contains(IMG_MARK)).then_some(t)
}

/// 正文 → 文字段与图片段交替（首尾一定是文字段，可能为空串）；`segs.join` 的逆运算见 `serialize_body`
pub fn parse_body(body: &str) -> Vec<Seg> {
    let mut out = Vec::new();
    let mut buf: Vec<&str> = Vec::new();
    for line in body.split('\n') {
        match img_token(line) {
            Some(tok) => {
                out.push(Seg::Text(buf.join("\n")));
                buf.clear();
                out.push(Seg::Image(tok.to_string()));
            }
            None => buf.push(line),
        }
    }
    out.push(Seg::Text(buf.join("\n")));
    out
}

pub fn serialize_body(segs: &[Seg]) -> String {
    segs.iter()
        .map(|s| match s {
            Seg::Text(t) => t.clone(),
            Seg::Image(tok) => img_line(tok),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn inline_tokens(body: &str) -> Vec<String> {
    if is_html_body(body) {
        return html_tokens(body);
    }
    if !body.contains(IMG_MARK) {
        return vec![];
    }
    parse_body(body).into_iter().filter_map(|s| if let Seg::Image(t) = s { Some(t) } else { None }).collect()
}

// ---------------- 富文本正文 ----------------
// 编辑器（WebView 里的 contenteditable）产出的是 HTML；草稿里以 `HTML_MARK` 开头与旧的纯文本草稿区分。
// 内嵌图片写成 `<img data-token="上传 token">`（不含图片数据，草稿体积小）；
// 载入编辑器时补回 data: 地址用于显示，发送时换成 `cid:`。

pub const HTML_MARK: &str = "<!--em-html-->";

pub fn is_html_body(body: &str) -> bool {
    body.starts_with(HTML_MARK)
}

/// 编辑器吐出的 HTML → 草稿正文
pub fn html_to_body(html: &str) -> String {
    format!("{HTML_MARK}{html}")
}

fn html_tokens(body: &str) -> Vec<String> {
    use std::sync::LazyLock;
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"data-token="([0-9A-Za-z-]+)""#).expect("regex"));
    let mut v: Vec<String> = Vec::new();
    for c in RE.captures_iter(body) {
        let t = c[1].to_string();
        if !v.contains(&t) {
            v.push(t);
        }
    }
    v
}

/// 草稿正文 → 编辑器初始 HTML（旧的纯文本草稿按原来的规则转换，图片标记变成 `<img data-token>`）。
pub fn body_to_editor_html(body: &str) -> String {
    if let Some(h) = body.strip_prefix(HTML_MARK) {
        return h.to_string();
    }
    let mut out = String::new();
    for seg in parse_body(body) {
        match seg {
            Seg::Text(t) => {
                if !t.is_empty() {
                    out.push_str(&text_fragment(&t));
                }
            }
            Seg::Image(tok) => out.push_str(&format!("<div><img data-token=\"{tok}\"></div>")),
        }
    }
    if out.is_empty() {
        out.push_str("<div><br></div>");
    }
    out
}

/// 正文是否没有任何内容（没有文字也没有图片）
pub fn body_is_blank(body: &str) -> bool {
    if !is_html_body(body) {
        return body.trim().is_empty();
    }
    if body.contains("data-token=") {
        return false;
    }
    let text = parse::html_to_text(&body[HTML_MARK.len()..]);
    text.chars().all(|c| c.is_whitespace() || c == '\u{a0}')
}

/// 草稿列表里的一行摘要
pub fn body_snippet(body: &str) -> String {
    if is_html_body(body) {
        let t = parse::html_to_text(&body[HTML_MARK.len()..]);
        return if t.trim().is_empty() && body.contains("data-token=") { "[图片]".into() } else { t };
    }
    body.replace(IMG_MARK, "")
}

/// 给 HTML 片段套上默认字体（发出去的邮件在收件人那边也是这个样子）
fn wrap_html(inner: &str) -> String {
    format!("<div style=\"font-family:-apple-system,'PingFang SC','Microsoft YaHei',sans-serif;font-size:14px;line-height:1.7\">{inner}</div>")
}

/// 发送：HTML 正文里的 `<img data-token>`（上传的图片）与 `<img src="data:…">`（引用原邮件里的图片）都换成 `cid:` 引用。
/// 返回（HTML、上传图片 token 列表、引用图片 (cid, content-type, 数据)）
fn html_for_send(body: &str) -> (String, Vec<String>, Vec<(String, String, Vec<u8>)>) {
    use base64::Engine as _;
    use std::sync::LazyLock;
    static IMG: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"(?i)<img\b[^>]*?data-token="([0-9A-Za-z-]+)"[^>]*>"#).expect("regex"));
    static DATA: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?i)<img\b[^>]*?src="data:(image/[a-z0-9.+-]+);base64,([A-Za-z0-9+/=\s]+)"[^>]*>"#).expect("regex"));
    let inner = &body[HTML_MARK.len()..];
    let tokens = html_tokens(body);
    let html = IMG.replace_all(inner, |c: &regex::Captures| format!("<img src=\"cid:{}@easymail\" style=\"max-width:100%;height:auto\">", &c[1]));
    let mut quoted = Vec::new();
    let html = DATA.replace_all(&html, |c: &regex::Captures| {
        let clean: String = c[2].chars().filter(|ch| !ch.is_whitespace()).collect();
        match base64::engine::general_purpose::STANDARD.decode(clean) {
            Ok(data) => {
                let cid = format!("q{}-{}@easymail", quoted.len(), uuid::Uuid::new_v4().simple());
                quoted.push((cid.clone(), c[1].to_string(), data));
                format!("<img src=\"cid:{cid}\" style=\"max-width:100%;height:auto\">")
            }
            Err(_) => String::new(),
        }
    });
    (wrap_html(&html), tokens, quoted)
}

/// 上传记录里的 (文件名, content-type)
pub fn upload_info(store: &Store, token: &str) -> Option<(String, String)> {
    store.r().query_row("SELECT filename, content_type FROM uploads WHERE token = ?", [token], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok().flatten()
}

/// 把本地文件收进 uploads/，返回草稿附件条目
pub fn add_upload(store: &Store, src: &Path) -> Result<DraftAtt> {
    let meta = std::fs::metadata(src).with_context(|| format!("读不到文件 {}", src.display()))?;
    if meta.is_dir() {
        bail!("不能添加文件夹作为附件");
    }
    if meta.len() > MAX_UPLOAD {
        bail!("单个附件不能超过 25 MB（多数邮箱服务商的上限）");
    }
    let filename: String =
        src.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "attachment".into()).replace(['/', '\\'], "_").chars().take(200).collect();
    let token = uuid::Uuid::new_v4().to_string();
    std::fs::copy(src, store.uploads_dir().join(&token))?;
    store.w().execute(
        "INSERT INTO uploads (token, filename, content_type, size, created_at) VALUES (?,?,?,?,?)",
        params![token, filename, mime_of(&filename), meta.len() as i64, now_ms()],
    )?;
    Ok(DraftAtt { token, filename, size: meta.len() })
}

/// 把内存里的数据收进 uploads/（回复 / 转发时引用的原邮件内嵌图片）
pub fn add_upload_bytes(store: &Store, filename: &str, content_type: &str, data: &[u8]) -> Result<DraftAtt> {
    if data.len() as u64 > MAX_UPLOAD {
        bail!("单个附件不能超过 25 MB（多数邮箱服务商的上限）");
    }
    let token = uuid::Uuid::new_v4().to_string();
    std::fs::write(store.uploads_dir().join(&token), data)?;
    store.w().execute(
        "INSERT INTO uploads (token, filename, content_type, size, created_at) VALUES (?,?,?,?,?)",
        params![token, filename, content_type, data.len() as i64, now_ms()],
    )?;
    Ok(DraftAtt { token, filename: filename.to_string(), size: data.len() as u64 })
}

pub fn delete_upload(store: &Store, token: &str) {
    let _ = store.w().execute("DELETE FROM uploads WHERE token = ?", [token]);
    let safe = Path::new(token).file_name().map(|s| s.to_owned()).unwrap_or_default();
    let _ = std::fs::remove_file(store.uploads_dir().join(safe));
}

/// 启动时清理：7 天前、且没被任何草稿引用的上传文件
pub fn cleanup_uploads(store: &Store) {
    let used: std::collections::HashSet<String> =
        list_drafts(store).unwrap_or_default().into_iter().flat_map(|d| d.attachments.into_iter().map(|a| a.token).chain(inline_tokens(&d.body))).collect();
    let old: Vec<String> = {
        let c = store.r();
        let Ok(mut st) = c.prepare("SELECT token FROM uploads WHERE created_at < ?") else { return };
        st.query_map([now_ms() - 7 * 86_400_000], |r| r.get(0)).map(|it| it.filter_map(|x| x.ok()).collect()).unwrap_or_default()
    };
    for t in old.iter().filter(|t| !used.contains(*t)) {
        delete_upload(store, t);
    }
}

// ---------------- 草稿 ----------------

fn draft_row(r: &rusqlite::Row) -> rusqlite::Result<Draft> {
    Ok(Draft {
        id: Some(r.get(0)?),
        account_id: r.get(1)?,
        mode: r.get(2)?,
        to: r.get(3)?,
        cc: r.get(4)?,
        bcc: r.get(5)?,
        subject: r.get(6)?,
        body: r.get(7)?,
        attachments: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
        source_id: r.get(9)?,
        include_original: r.get::<_, i64>(10)? != 0,
        updated_at: r.get(11)?,
        quote_html: String::new(),
    })
}

const DRAFT_COLS: &str = "id, account_id, mode, to_addr, cc_addr, bcc_addr, subject, body, attachments, source_id, include_orig, updated_at";

pub fn list_drafts(store: &Store) -> Result<Vec<Draft>> {
    let c = store.r();
    let mut st = c.prepare(&format!("SELECT {DRAFT_COLS} FROM drafts ORDER BY updated_at DESC"))?;
    let v = st.query_map([], draft_row)?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

pub fn get_draft(store: &Store, id: i64) -> Result<Option<Draft>> {
    Ok(store.r().query_row(&format!("SELECT {DRAFT_COLS} FROM drafts WHERE id = ?"), [id], draft_row).optional()?)
}

pub fn save_draft(store: &Store, d: &Draft) -> Result<Draft> {
    let atts = serde_json::to_string(&d.attachments)?;
    let mode = if d.mode.is_empty() { "new" } else { d.mode.as_str() };
    let existing = d.id.and_then(|id| get_draft(store, id).ok().flatten());
    let id = {
        let c = store.w();
        if let Some(e) = existing {
            c.execute(
                "UPDATE drafts SET account_id = ?, mode = ?, to_addr = ?, cc_addr = ?, bcc_addr = ?, subject = ?, body = ?, attachments = ?, source_id = ?, include_orig = ?, updated_at = ? WHERE id = ?",
                params![d.account_id, mode, d.to, d.cc, d.bcc, d.subject, d.body, atts, d.source_id, d.include_original as i64, now_ms(), e.id],
            )?;
            e.id.unwrap_or(0)
        } else {
            c.execute(
                "INSERT INTO drafts (account_id, mode, to_addr, cc_addr, bcc_addr, subject, body, attachments, source_id, include_orig, updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                params![d.account_id, mode, d.to, d.cc, d.bcc, d.subject, d.body, atts, d.source_id, d.include_original as i64, now_ms()],
            )?;
            c.last_insert_rowid()
        }
    };
    get_draft(store, id)?.ok_or_else(|| anyhow!("草稿保存失败"))
}

pub fn delete_draft(store: &Store, id: i64, keep_uploads: bool) -> Result<()> {
    if let Some(d) = get_draft(store, id)? {
        if !keep_uploads {
            for a in &d.attachments {
                delete_upload(store, &a.token);
            }
            for t in inline_tokens(&d.body) {
                delete_upload(store, &t);
            }
        }
        store.w().execute("DELETE FROM drafts WHERE id = ?", [id])?;
    }
    Ok(())
}

// ---------------- 正文 ----------------

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// 纯文本 → 简洁 HTML：段落、换行、以 > 开头的行转成引用块
pub fn text_to_html(text: &str) -> String {
    wrap_html(&text_fragment(text))
}

/// 纯文本 → HTML 片段（不带外层字体容器）
fn text_fragment(text: &str) -> String {
    let mut html = String::new();
    let mut quote: Vec<String> = Vec::new();
    let flush = |quote: &mut Vec<String>, html: &mut String| {
        if !quote.is_empty() {
            let inner: Vec<String> = quote.iter().map(|l| if l.is_empty() { "&nbsp;".into() } else { esc(l) }).collect();
            html.push_str(&format!(
                "<blockquote style=\"margin:0 0 0 .8ex;border-left:2px solid #ccc;padding-left:1ex;color:#666\">{}</blockquote>",
                inner.join("<br>")
            ));
            quote.clear();
        }
    };
    for l in text.split('\n') {
        if let Some(rest) = l.strip_prefix('>') {
            quote.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            continue;
        }
        flush(&mut quote, &mut html);
        if l.trim().is_empty() {
            html.push_str("<div><br></div>");
        } else {
            html.push_str(&format!("<div>{}</div>", esc(l)));
        }
    }
    flush(&mut quote, &mut html);
    html
}

pub fn split_addrs(s: &str) -> Vec<String> {
    s.split([',', ';', '，', '；', '\n']).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

/// "张三 <a@b.c>" → ("张三", "a@b.c")；"a@b.c" → ("", "a@b.c")
pub fn parse_addr(s: &str) -> (String, String) {
    let s = s.trim();
    if let (Some(l), Some(r)) = (s.rfind('<'), s.rfind('>')) {
        if l < r {
            let name = s[..l].trim().trim_matches('"').trim().to_string();
            return (name, s[l + 1..r].trim().to_string());
        }
    }
    (String::new(), s.to_string())
}

fn addr_list(v: &[String]) -> Address<'static> {
    let list: Vec<Address<'static>> = v
        .iter()
        .map(|s| {
            let (n, a) = parse_addr(s);
            if n.is_empty() {
                Address::new_address(None::<String>, a)
            } else {
                Address::new_address(Some(n), a)
            }
        })
        .collect();
    Address::new_list(list)
}

// ---------------- 发信 ----------------

#[derive(Clone, Debug, Default)]
pub struct Outgoing {
    pub account_id: i64,
    pub mode: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub text: String,
    pub attachments: Vec<DraftAtt>,
    pub source_id: Option<i64>,
    pub include_original: bool,
    pub draft_id: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct SendResult {
    pub message_id: String,
    pub saved_to_sent: bool,
}

pub struct Built {
    /// SMTP 投递用：**不含** Bcc 头，否则 To / Cc 收件人查看原文就能看到密送名单
    pub raw_smtp: Vec<u8>,
    /// 存「已发送」用：在头部补上 Bcc，自己日后还能看到当时密送了谁
    pub raw_sent: Vec<u8>,
    pub rcpts: Vec<String>,
    pub message_id: String,
}

/// 去掉引用了不存在的 cid 的 `<img>`
fn drop_dead_cids(html: &str, have: &[(String, String, Vec<u8>)]) -> String {
    use std::sync::LazyLock;
    static IMG: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"(?i)<img\b[^>]*?src="cid:([^"]+)"[^>]*>"#).expect("regex"));
    IMG.replace_all(html, |c: &regex::Captures| if have.iter().any(|(cid, ..)| *cid == c[1]) { c[0].to_string() } else { String::new() }).into_owned()
}

/// 组装 MIME 原文。密送对象只出现在 SMTP 信封（`rcpts`）里，投递的原文不带 Bcc 头。
pub fn build(store: &Store, acc: &super::model::Account, o: &Outgoing, orig_atts: Vec<(String, String, Vec<u8>)>) -> Result<Built> {
    let to = split_addrs(&o.to);
    let cc = split_addrs(&o.cc);
    let bcc = split_addrs(&o.bcc);
    if to.is_empty() && cc.is_empty() && bcc.is_empty() {
        bail!("收件人为空");
    }
    for a in to.iter().chain(&cc).chain(&bcc) {
        if !accounts::valid_email(&parse_addr(a).1) {
            bail!("收件人地址不正确：{a}");
        }
    }
    let domain = acc.email.split('@').nth(1).unwrap_or("localhost");
    let message_id = format!("{}@{}", uuid::Uuid::new_v4(), domain);
    let subject = if o.subject.trim().is_empty() { "(无主题)".to_string() } else { o.subject.trim().to_string() };
    let mut b = MessageBuilder::new()
        .from((acc.display_name().to_string(), acc.email.clone()))
        .subject(subject)
        .message_id(message_id.clone())
        .date(now_ms() / 1000)
        .header("X-Mailer", mail_builder::headers::text::Text::new(format!("EasyMail {}", super::config::VERSION)));
    // 正文里的内嵌图片：HTML 里用 cid 引用，纯文本版留一个 [图片] 占位
    let mut inline_imgs: Vec<(String, String, Vec<u8>)> = Vec::new(); // (cid, content-type, 数据)
    let (plain, html) = if is_html_body(&o.text) {
        let (html, tokens, quoted) = html_for_send(&o.text);
        inline_imgs.extend(quoted);
        for tok in tokens {
            let Some((_, ct)) = upload_info(store, &tok) else { continue };
            let Ok(data) = std::fs::read(store.uploads_dir().join(&tok)) else { continue };
            inline_imgs.push((format!("{tok}@easymail"), ct, data));
        }
        // 引用到的图片读不出来（上传被清理了）：留一个空的 img 会显示成破图，直接去掉
        let html = drop_dead_cids(&html, &inline_imgs);
        (parse::html_to_text(&html), html)
    } else if o.text.contains(IMG_MARK) {
        let (mut plain, mut html) = (Vec::new(), String::new());
        for seg in parse_body(&o.text) {
            match seg {
                Seg::Text(t) => {
                    if !t.is_empty() {
                        html.push_str(&text_fragment(&t));
                        plain.push(t);
                    }
                }
                Seg::Image(tok) => {
                    let Some((name, ct)) = upload_info(store, &tok) else { continue };
                    let Ok(data) = std::fs::read(store.uploads_dir().join(&tok)) else { continue };
                    let cid = format!("{tok}@easymail");
                    html.push_str(&format!(
                        "<div><img src=\"cid:{cid}\" alt=\"{}\" style=\"max-width:100%;height:auto\"></div>",
                        esc(&name).replace('"', "&quot;")
                    ));
                    plain.push(format!("[图片：{name}]"));
                    inline_imgs.push((cid, ct, data));
                }
            }
        }
        (plain.join("\n"), wrap_html(&html))
    } else {
        (o.text.clone(), text_to_html(&o.text))
    };
    let mut files: Vec<(String, String, Vec<u8>)> = Vec::new(); // (content-type, 文件名, 数据)
    if !to.is_empty() {
        b = b.to(addr_list(&to));
    }
    if !cc.is_empty() {
        b = b.cc(addr_list(&cc));
    }
    if let Some(src) = o.source_id.filter(|_| o.mode == "reply" || o.mode == "replyAll") {
        let r: Option<(String, String)> =
            store.r().query_row("SELECT message_id, refs FROM messages WHERE id = ?", [src], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        if let Some((mid, refs)) = r.filter(|r| !r.0.is_empty()) {
            let strip = |s: &str| s.trim().trim_start_matches('<').trim_end_matches('>').to_string();
            b = b.in_reply_to(strip(&mid));
            let mut all: Vec<String> = refs.split_whitespace().map(strip).collect();
            all.push(strip(&mid));
            b = b.references(all);
        }
    }
    for a in &o.attachments {
        let u = upload_info(store, &a.token);
        let Some((name, ct)) = u else { bail!("附件「{}」已失效，请重新添加", a.filename) };
        let data = std::fs::read(store.uploads_dir().join(&a.token)).with_context(|| format!("读不到附件「{name}」"))?;
        files.push((ct, name, data));
    }
    for (name, ct, data) in orig_atts {
        files.push((ct, name, data));
    }
    if inline_imgs.is_empty() {
        b = b.text_body(plain).html_body(html);
        for (ct, name, data) in files {
            b = b.attachment(ct, name, data);
        }
    } else {
        // mixed[ alternative[ text, related[ html, 内嵌图… ] ], 附件… ]：Outlook / Apple Mail / Gmail 都认
        let mut related = vec![MimePart::new("text/html", html)];
        for (cid, ct, data) in inline_imgs {
            related.push(MimePart::new(ct, data).inline().cid(cid));
        }
        let alt = MimePart::new("multipart/alternative", vec![MimePart::new("text/plain", plain), MimePart::new("multipart/related", related)]);
        let root = if files.is_empty() {
            alt
        } else {
            let mut parts = vec![alt];
            parts.extend(files.into_iter().map(|(ct, name, data)| MimePart::new(ct, data).attachment(name)));
            MimePart::new("multipart/mixed", parts)
        };
        b = b.body(root);
    }
    let raw_smtp = b.write_to_vec()?;
    let raw_sent = if bcc.is_empty() {
        raw_smtp.clone()
    } else {
        // 头部的顺序无关紧要：把 Bcc 行插在最前面，其余字节与投递版完全一致
        use mail_builder::headers::Header;
        let mut v = b"Bcc: ".to_vec();
        addr_list(&bcc).write_header(&mut v, 5)?;
        if !v.ends_with(b"\r\n") {
            v.extend_from_slice(b"\r\n");
        }
        v.extend_from_slice(&raw_smtp);
        v
    };
    let rcpts: Vec<String> = to.iter().chain(&cc).chain(&bcc).map(|a| parse_addr(a).1).collect();
    Ok(Built { raw_smtp, raw_sent, rcpts, message_id: format!("<{message_id}>") })
}

pub async fn send(eng: Engine, o: Outgoing) -> Result<SendResult> {
    let store = eng.store().clone();
    let acc = accounts::get(&store, o.account_id)?.ok_or_else(|| anyhow!("请选择发件账号"))?;
    if acc.is_local() {
        bail!("「本地归档」是本地目录，不能发信；请在上方选择真实的发件账号");
    }
    let mut orig = Vec::new();
    if let Some(src) = o.source_id.filter(|_| o.mode == "forward" && o.include_original) {
        let raw = eng.source(src).await?;
        orig = parse::file_attachments(&raw);
    }
    let built = build(&store, &acc, &o, orig)?;
    let pw = accounts::password(&store, &acc)?;
    smtp::send(&acc, &pw, eng.proxy().as_ref(), &built.rcpts, &built.raw_smtp).await.map_err(|e| anyhow!("发送失败：{e:#}"))?;

    let sent: Option<(i64, String)> =
        store.r().query_row("SELECT id, path FROM folders WHERE account_id = ? AND role = 'sent'", [acc.id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    let mut saved = false;
    if let Some((sent_id, path)) = &sent {
        if providers::provider(&acc.provider).auto_save_sent {
            saved = true;
        } else {
            match eng.remote(acc.id, Op::SaveSent { path: path.clone(), message_id: built.message_id.clone(), raw: built.raw_sent.clone() }).await {
                Ok(()) => saved = true,
                Err(e) => log::warn!("[send] 存「已发送」失败：{e:#}"),
            }
        }
        eng.request_sync(acc.id, Some(*sent_id), false);
    }
    if let Some(src) = o.source_id.filter(|_| o.mode == "reply" || o.mode == "replyAll") {
        let _ = super::ops::set_answered(eng.clone(), src).await;
    }
    if let Some(d) = o.draft_id {
        let _ = delete_draft(&store, d, true);
    }
    for a in &o.attachments {
        delete_upload(&store, &a.token);
    }
    for t in inline_tokens(&o.text) {
        delete_upload(&store, &t);
    }
    Ok(SendResult { message_id: built.message_id, saved_to_sent: saved })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_and_addrs() {
        let h = text_to_html("你好\n\n> 原文 <b>\n> 第二行\n结尾");
        assert!(h.contains("<div>你好</div><div><br></div><blockquote"));
        assert!(h.contains("原文 &lt;b&gt;<br>第二行</blockquote><div>结尾</div>"));
        assert_eq!(split_addrs("a@b.c，张三 <z@s.cn>; x@y.z\n"), vec!["a@b.c", "张三 <z@s.cn>", "x@y.z"]);
        assert_eq!(parse_addr("\"张 三\" <z@s.cn>"), ("张 三".into(), "z@s.cn".into()));
        assert_eq!(parse_addr("z@s.cn"), ("".into(), "z@s.cn".into()));
        assert_eq!(mime_of("报告.PDF"), "application/pdf");
    }

    #[test]
    fn drafts_uploads_build() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.w()
            .execute(
                "INSERT INTO accounts(id,email,name,provider,imap_host,smtp_host,secret,created_at) VALUES(1,'me@x.com','大卫','custom','h','h','x',0)",
                [],
            )
            .unwrap();
        s.w().execute("INSERT INTO folders(id,account_id,path,name,role) VALUES(1,1,'INBOX','INBOX','inbox')", []).unwrap();
        s.w().execute("INSERT INTO messages(id,account_id,folder_id,uid,date,message_id,refs) VALUES(7,1,1,1,0,'<orig@x>','<root@x>')", []).unwrap();
        let f = dir.path().join("发票.pdf");
        std::fs::write(&f, b"%PDF-1.4 test").unwrap();
        let att = add_upload(&s, &f).unwrap();
        assert_eq!(att.filename, "发票.pdf");
        let d =
            save_draft(&s, &Draft { account_id: Some(1), to: "a@b.c".into(), subject: "草稿".into(), attachments: vec![att.clone()], ..Default::default() })
                .unwrap();
        let d2 = save_draft(&s, &Draft { id: d.id, body: "改了".into(), ..d.clone() }).unwrap();
        assert_eq!(d2.id, d.id);
        assert_eq!(list_drafts(&s).unwrap().len(), 1);
        assert_eq!(list_drafts(&s).unwrap()[0].body, "改了");
        let acc = accounts::must(&s, 1).unwrap();
        let o = Outgoing {
            account_id: 1,
            mode: "reply".into(),
            to: "张三 <z@s.cn>".into(),
            bcc: "secret@s.cn".into(),
            subject: "Re: 你好".into(),
            text: "收到\n> 原文".into(),
            attachments: vec![att.clone()],
            source_id: Some(7),
            ..Default::default()
        };
        let b = build(&s, &acc, &o, vec![("原附件.txt".into(), "text/plain".into(), b"orig".to_vec())]).unwrap();
        assert_eq!(b.rcpts, vec!["z@s.cn", "secret@s.cn"]);
        // 投递的原文里不能出现密送名单；存「已发送」的那份保留
        let smtp_text = String::from_utf8_lossy(&b.raw_smtp).to_lowercase();
        assert!(!smtp_text.contains("bcc:") && !smtp_text.contains("secret@s.cn"), "SMTP 原文泄漏了 Bcc");
        assert!(String::from_utf8_lossy(&b.raw_sent).starts_with("Bcc: <secret@s.cn>\r\n"));
        assert!(b.raw_sent.ends_with(&b.raw_smtp));
        assert_eq!(parse::parse_full(&b.raw_sent).unwrap().env.subject, "Re: 你好");
        let p = parse::parse_full(&b.raw_smtp).unwrap();
        assert_eq!(p.env.subject, "Re: 你好");
        assert_eq!(p.env.from.name, "大卫");
        assert_eq!(p.env.in_reply_to, "<orig@x>");
        assert_eq!(p.env.refs, "<root@x> <orig@x>");
        assert_eq!(p.env.message_id, b.message_id);
        assert_eq!(p.atts.iter().map(|a| a.filename.as_str()).collect::<Vec<_>>(), vec!["发票.pdf", "原附件.txt"]);
        assert!(p.text.contains("收到"));
        assert!(p.html.contains("blockquote"));
        assert!(build(&s, &acc, &Outgoing { to: "bad".into(), ..o.clone() }, vec![]).is_err());
        delete_draft(&s, d.id.unwrap(), false).unwrap();
        assert!(!s.uploads_dir().join(&att.token).exists());
        assert!(list_drafts(&s).unwrap().is_empty());
    }

    #[test]
    fn body_markers_roundtrip_and_inline_build() {
        let body = format!("前文\n{}\n\n{}\n后文\n", img_line("t1"), img_line("t2"));
        let segs = parse_body(&body);
        assert_eq!(segs, vec![Seg::Text("前文".into()), Seg::Image("t1".into()), Seg::Text("".into()), Seg::Image("t2".into()), Seg::Text("后文\n".into())]);
        assert_eq!(serialize_body(&segs), body);
        assert_eq!(inline_tokens(&body), vec!["t1", "t2"]);
        assert_eq!(parse_body("纯文本\n第二行"), vec![Seg::Text("纯文本\n第二行".into())]);

        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.w()
            .execute(
                "INSERT INTO accounts(id,email,name,provider,imap_host,smtp_host,secret,created_at) VALUES(1,'me@x.com','大卫','custom','h','h','x',0)",
                [],
            )
            .unwrap();
        let f = dir.path().join("猫.png");
        std::fs::write(&f, b"\x89PNG\r\n\x1a\nfake").unwrap();
        let img = add_upload(&s, &f).unwrap();
        let acc = accounts::must(&s, 1).unwrap();
        let o =
            Outgoing { account_id: 1, to: "a@b.c".into(), subject: "图".into(), text: format!("看图\n{}\n完", img_line(&img.token)), ..Default::default() };
        let b = build(&s, &acc, &o, vec![]).unwrap();
        let raw = String::from_utf8_lossy(&b.raw_smtp).to_string();
        assert!(raw.contains(&format!("cid:{}@easymail", img.token)), "HTML 里应引用 cid");
        assert!(raw.contains("multipart/related"), "内嵌图片应放在 multipart/related 里：{raw}");
        let p = parse::parse_full(&b.raw_smtp).unwrap();
        assert!(p.text.contains("看图") && p.text.contains("[图片：猫.png]") && p.text.contains('完'));
        // 删草稿 / 发信后内嵌图的上传也被清掉
        let d = save_draft(&s, &Draft { account_id: Some(1), body: o.text.clone(), ..Default::default() }).unwrap();
        delete_draft(&s, d.id.unwrap(), false).unwrap();
        assert!(!s.uploads_dir().join(&img.token).exists());
    }

    #[test]
    fn rich_body_roundtrip_and_build() {
        let tok = "11111111-2222-3333-4444-555555555555";
        let body = html_to_body(&format!(
            "<div>你好 <b>世界</b> <a href=\"https://a.com\">链接</a></div><div><img data-token=\"{tok}\" style=\"max-width:100%\"></div>"
        ));
        assert!(is_html_body(&body));
        assert_eq!(inline_tokens(&body), vec![tok]);
        assert!(!body_is_blank(&body));
        assert!(body_is_blank(&html_to_body("<div><br></div>")));
        assert!(body_is_blank("  \n"));
        assert_eq!(body_snippet(&html_to_body("<div>摘要 <b>x</b></div>")).trim(), "摘要 x");
        let h = body_to_editor_html(&format!("a<b\n{}\n> q", img_line("t1")));
        assert!(h.contains("a&lt;b") && h.contains("<img data-token=\"t1\">") && h.contains("<blockquote"));
        assert_eq!(body_to_editor_html(""), "<div><br></div>");

        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        s.w()
            .execute(
                "INSERT INTO accounts(id,email,name,provider,imap_host,smtp_host,secret,created_at) VALUES(1,'me@x.com','大卫','custom','h','h','x',0)",
                [],
            )
            .unwrap();
        let f = dir.path().join("猫.png");
        std::fs::write(&f, b"\x89PNG\r\n\x1a\nfake").unwrap();
        let img = add_upload(&s, &f).unwrap();
        let acc = accounts::must(&s, 1).unwrap();
        let text = html_to_body(&format!(
            "<div>你好 <b>世界</b> <a href=\"https://a.com\">链接</a></div><div><img data-token=\"{}\"></div><div><img data-token=\"gone\"></div><blockquote><img src=\"data:image/png;base64,aGk=\"></blockquote>",
            img.token
        ));
        let o = Outgoing { account_id: 1, to: "a@b.c".into(), subject: "富文本".into(), text, ..Default::default() };
        let b = build(&s, &acc, &o, vec![]).unwrap();
        let p = parse::parse_full(&b.raw_smtp).unwrap();
        assert!(p.html.contains("<b>世界</b>") && p.html.contains("href=\"https://a.com\""));
        assert!(p.html.contains(&format!("cid:{}@easymail", img.token)));
        assert!(!p.html.contains("cid:gone") && !p.html.contains("data:image"));
        assert!(p.html.matches("cid:").count() == 2, "引用的 data 图片也要变成 cid：{}", p.html);
        assert!(p.text.contains("你好") && p.text.contains("世界"));
        assert!(String::from_utf8_lossy(&b.raw_smtp).contains("multipart/related"));
    }
}
