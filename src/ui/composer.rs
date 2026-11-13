//! 写信 / 回复 / 转发：联系人补全、抄送密送、附件（选择 / 拖入）、
//! ⌘↵ 发送。正文是富文本编辑器（见 `crate::editor`）：加粗 / 斜体 / 字体颜色 / 列表 / 链接 / 表情 / 图片，思路参照 Gmail。

use super::line_input::{InputEvent, LineInput};
use super::theme::{theme, Theme};
use super::widgets::*;
use super::{fmt_full, fmt_size, icon, one_line};
use crate::core::engine::Engine;
use crate::core::mailer::{self, Outgoing, SendResult};
use crate::core::model::{addrs_text, Account, Contact, Detail, Draft};
use crate::core::queries;
use crate::editor;
use crate::webview;
use base64::Engine as _;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::time::Duration;

actions!(composer, [SendMail]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-enter", SendMail, Some("Composer"))]);
}

pub enum ComposerEvent {
    Closed {
        saved: bool,
    },
    Sent(SendResult),
    /// 已放进发件箱：`send_at` 到点后由引擎发出（撤回窗口 / 定时发送）
    Queued {
        outbox_id: i64,
        send_at: i64,
        scheduled: bool,
    },
    Toast(String, bool),
}

impl EventEmitter<ComposerEvent> for Composer {}

/// 最后一次输入后多久自动保存草稿
const AUTOSAVE_MS: u64 = 2_500;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Field {
    To,
    Cc,
    Bcc,
}

const BODY_PLACEHOLDER: &str = "见字如面……（可直接粘贴 / 拖入图片，或用下方的按钮插入链接、表情、图片）";

/// 访达 / 剪贴板里的文件按扩展名判断是不是图片：图片直接显示在正文里，其余作为附件
fn is_image_file(p: &std::path::Path) -> bool {
    matches!(p.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff"))
}

/// 编辑器里显示用的图片地址：原图不大就直接内嵌，太大的缩到 1600 像素以内再内嵌（发送时仍用原图）
fn image_data_uri(bytes: &[u8], content_type: &str) -> String {
    const LIMIT: usize = 1_500_000;
    let b64 = base64::engine::general_purpose::STANDARD;
    if bytes.len() > LIMIT && content_type != "image/svg+xml" && content_type != "image/gif" {
        if let Ok(img) = image::load_from_memory(bytes) {
            let img = if img.width().max(img.height()) > 1600 { img.resize(1600, 1600, image::imageops::FilterType::Triangle) } else { img };
            let mut out = Vec::new();
            let (res, mime) = if img.color().has_alpha() {
                (img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png), "image/png")
            } else {
                (image::DynamicImage::ImageRgb8(img.to_rgb8()).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Jpeg), "image/jpeg")
            };
            if res.is_ok() {
                return format!("data:{mime};base64,{}", b64.encode(&out));
            }
        }
    }
    format!("data:{content_type};base64,{}", b64.encode(bytes))
}

/// 草稿里的 `<img data-token>` 补回 `src`（读上传区的文件）；文件已被清理的图片直接去掉
fn inflate_images(eng: &Engine, html: &str) -> String {
    use std::sync::LazyLock;
    static IMG: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"(?i)<img\b[^>]*?data-token="([0-9A-Za-z-]+)"[^>]*>"#).expect("regex"));
    IMG.replace_all(html, |c: &regex::Captures| {
        let tok = &c[1];
        let uri =
            mailer::upload_info(eng.store(), tok).and_then(|(_, ct)| std::fs::read(eng.store().uploads_dir().join(tok)).ok().map(|b| image_data_uri(&b, &ct)));
        match uri {
            Some(u) => c[0].replacen(&format!("data-token=\"{tok}\""), &format!("data-token=\"{tok}\" src=\"{u}\""), 1),
            None => String::new(),
        }
    })
    .into_owned()
}

/// 回复 / 转发引用的原邮件 HTML：内嵌图片（`data:` URI）收进上传区，换成 `<img data-token>`，
/// 编辑器里只显示（必要时缩小的）预览，草稿与编辑器往返只带 token，发送时用原图转成 `cid:`。
/// 这样大图不会撑爆编辑器的 data URI 上限，也不会每次改动都把几 MB 的 base64 回传一遍。
fn tokenize_quote_images(store: &crate::core::db::Store, html: &str) -> String {
    use std::sync::LazyLock;
    // 原邮件里自带的 data-token 一律废掉：token 只能由这里生成，防止外来邮件引用本机上传区里的别的文件
    static FORGED: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)\bdata-token\s*=").expect("regex"));
    static IMG: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?i)<img\b[^>]*?\bsrc\s*=\s*["']?data:(image/[a-z0-9.+-]+);base64,([A-Za-z0-9+/=\s]+)["']?[^>]*>"#).expect("regex")
    });
    let html = FORGED.replace_all(html, "data-x=");
    let mut n = 0;
    IMG.replace_all(&html, |c: &regex::Captures| {
        let clean: String = c[2].chars().filter(|ch| !ch.is_whitespace()).collect();
        let Ok(data) = base64::engine::general_purpose::STANDARD.decode(clean) else { return String::new() };
        let ct = c[1].to_lowercase();
        n += 1;
        let ext = ct.trim_start_matches("image/").replace("jpeg", "jpg").replace("svg+xml", "svg");
        match mailer::add_upload_bytes(store, &format!("引用图片-{n}.{ext}"), &ct, &data) {
            Ok(att) => format!("<img data-token=\"{}\" src=\"{}\">", att.token, image_data_uri(&data, &ct)),
            Err(_) => String::new(),
        }
    })
    .into_owned()
}

fn editor_colors(t: &Theme) -> editor::Colors {
    editor::Colors {
        fg: t.ink,
        bg: t.panel,
        muted: t.muted,
        border: t.border,
        accent: t.accent,
        accent_soft: t.accent_soft,
        panel: t.panel,
        panel2: t.panel_2,
        hover: t.row_hover,
        dark: t.dark,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Slot {
    To,
    Cc,
    Bcc,
    Subject,
    Body,
}

pub struct Composer {
    eng: Engine,
    sendable: Vec<Account>,
    pub draft: Draft,
    pub to: Entity<LineInput>,
    pub cc: Entity<LineInput>,
    pub bcc: Entity<LineInput>,
    pub subject: Entity<LineInput>,
    /// 富文本编辑器：当前正文（草稿格式：`HTML_MARK` + HTML；没改过就还是载入时的原文）
    body_cache: String,
    /// 编辑器文档与标识（标识变了才重新加载）
    editor_doc: std::sync::Arc<str>,
    editor_key: u64,
    editor_ready: bool,
    /// 编辑器还没就绪时攒下的脚本
    pending_js: Vec<String>,
    /// 就绪后把键盘焦点给编辑器（回复时光标直接落在正文）
    want_body_focus: bool,
    /// GPUI 侧代表「焦点在正文编辑器里」的句柄：收件人等输入框据此失焦，快捷键上下文也靠它
    body_focus: FocusHandle,
    /// 附件 / 内嵌图片的原地预览（Quick Look），盖在写信窗口之上
    preview: Option<(String, std::path::PathBuf)>,
    show_cc: bool,
    saved_at: Option<i64>,
    pub sending: bool,
    dirty: bool,
    save_gen: u64,
    /// 自动保存的防抖任务：最后一次输入 2.5 秒后存草稿
    autosave: Option<Task<()>>,
    /// 在独立窗口里显示（不是主窗口上的模态）：外壳铺满窗口
    pub window_mode: bool,
    /// 定时发送菜单是否展开
    schedule_menu: bool,
    /// 回复模板菜单是否展开
    template_menu: bool,
    suggest: Vec<Contact>,
    suggest_for: Option<Field>,
    /// 联系人补全的请求代号：后台查询回来时只认最新一次输入的结果
    suggest_gen: u64,
    hi: usize,
    acct_menu: bool,
    drop_hover: bool,
    _subs: Vec<Subscription>,
    _poll: Task<()>,
}

impl Drop for Composer {
    fn drop(&mut self) {
        editor::hide_key(self.editor_key);
    }
}

fn title_of(mode: &str) -> &'static str {
    match mode {
        "reply" => "回复",
        "replyAll" => "全部回复",
        "forward" => "转发",
        _ => "写一封新信",
    }
}

/// 地址框里最后一个分隔符后的片段
fn last_token(v: &str) -> &str {
    v.rsplit([',', ';', '，', '；']).next().unwrap_or("").trim()
}

/// 用联系人替换最后一个片段，并补上「, 」方便继续输入
pub fn pick_contact(v: &str, c: &Contact) -> String {
    let mut parts: Vec<String> = v.split([',', ';', '，', '；']).map(|s| s.trim().to_string()).collect();
    let full = if c.name.is_empty() { c.address.clone() } else { format!("{} <{}>", c.name, c.address) };
    if let Some(last) = parts.last_mut() {
        *last = full;
    }
    parts.retain(|p| !p.is_empty());
    parts.join(", ") + ", "
}

impl Composer {
    pub fn new(eng: Engine, accounts: Vec<Account>, d: Draft, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sendable: Vec<Account> = accounts.into_iter().filter(|a| !a.is_local()).collect();
        let mut draft = d;
        if draft.account_id.is_none_or(|id| !sendable.iter().any(|a| a.id == id)) {
            draft.account_id = sendable.first().map(|a| a.id);
        }
        if draft.mode.is_empty() {
            draft.mode = "new".into();
        }
        let mk = |text: &str, ph: &str, cx: &mut Context<Self>| {
            let text = text.to_string();
            let ph = ph.to_string();
            cx.new(move |cx| {
                let mut i = LineInput::new(&ph, cx);
                i.set_text(&text, cx);
                i.collapse_to_end(cx);
                i
            })
        };
        let to = mk(&draft.to, "多个地址用逗号分隔", cx);
        let cc = mk(&draft.cc, "", cx);
        let bcc = mk(&draft.bcc, "", cx);
        let subject = mk(&draft.subject, "（无主题）", cx);
        let mut subs = vec![];
        for (f, input) in [(Some(Field::To), &to), (Some(Field::Cc), &cc), (Some(Field::Bcc), &bcc), (None, &subject)] {
            subs.push(cx.subscribe_in(input, window, move |this: &mut Self, _, ev: &InputEvent, window, cx| this.on_input(f, ev, window, cx)));
        }
        let body_html = inflate_images(&eng, &mailer::body_to_editor_html(&draft.body));
        let editor_doc: std::sync::Arc<str> = {
            let quote = (!draft.quote_html.is_empty()).then(|| tokenize_quote_images(eng.store(), &draft.quote_html));
            let q = quote.as_deref().map(|h| (h, draft.mode != "forward"));
            editor::document(&body_html, q, BODY_PLACEHOLDER, &editor_colors(theme(cx)))
        }
        .into();
        // 回复：光标放正文开头（引用在下方）；新信 / 转发：光标在收件人
        let want_body_focus = !(draft.mode == "new" || draft.mode == "forward");
        if !want_body_focus {
            window.focus(&to.focus_handle(cx));
        }
        let body_focus = cx.focus_handle();
        let show_cc = !draft.cc.is_empty() || !draft.bcc.is_empty();
        let saved_at = draft.id.map(|_| draft.updated_at);
        // 编辑器（WebView）的事件由页面脚本推进队列，这里每 30ms 取一次
        let poll = cx.spawn_in(window, async move |this: WeakEntity<Self>, cx| loop {
            cx.background_executor().timer(Duration::from_millis(30)).await;
            let evs = editor::take_events();
            if evs.is_empty() {
                if this.upgrade().is_none() {
                    break;
                }
                continue;
            }
            if this.update_in(cx, |c, window, cx| c.on_editor_events(evs, window, cx)).is_err() {
                break;
            }
        });
        Composer {
            eng,
            sendable,
            body_cache: draft.body.clone(),
            draft,
            to,
            cc,
            bcc,
            subject,
            editor_doc,
            editor_key: rand::random(),
            editor_ready: false,
            pending_js: vec![],
            want_body_focus,
            body_focus,
            preview: None,
            show_cc,
            saved_at,
            sending: false,
            dirty: false,
            save_gen: 0,
            autosave: None,
            window_mode: false,
            schedule_menu: false,
            template_menu: false,
            suggest: vec![],
            suggest_for: None,
            suggest_gen: 0,
            hi: 0,
            acct_menu: false,
            drop_hover: false,
            _subs: subs,
            _poll: poll,
        }
    }

    // ───────────── 富文本正文 ─────────────

    /// 编辑器就绪就立刻执行，否则先攒着
    fn run_js(&mut self, js: String) {
        if self.editor_ready {
            editor::run_js(&js);
        } else {
            self.pending_js.push(js);
        }
    }

    fn on_editor_events(&mut self, evs: Vec<editor::Event>, window: &mut Window, cx: &mut Context<Self>) {
        for ev in evs {
            match ev {
                editor::Event::Ready => {
                    self.editor_ready = true;
                    for js in std::mem::take(&mut self.pending_js) {
                        editor::run_js(&js);
                    }
                    if self.want_body_focus {
                        self.want_body_focus = false;
                        self.focus_body(true, window, cx);
                    }
                }
                editor::Event::Changed(html) => {
                    self.body_cache = mailer::html_to_body(&html);
                    self.touch(cx);
                }
                editor::Event::Send => self.send(window, cx),
                editor::Event::Cancel => self.close(window, cx),
                editor::Event::Tab => self.focus_step(Slot::Body, false, window, cx),
                editor::Event::BackTab => self.focus_step(Slot::Body, true, window, cx),
                editor::Event::Focus => {
                    window.focus(&self.body_focus);
                    if self.acct_menu || !self.suggest.is_empty() {
                        self.acct_menu = false;
                        self.suggest.clear();
                        cx.notify();
                    }
                }
                editor::Event::PickImage => self.pick_images(cx),
                editor::Event::Files(items) => {
                    let dir = std::env::temp_dir().join("EasyMail-paste").join(uuid::Uuid::new_v4().to_string());
                    let mut srcs = Vec::new();
                    for it in items {
                        let name: String = it.name.chars().map(|c| if "/\\:\0".contains(c) { '_' } else { c }).collect();
                        let name = if name.trim().is_empty() { "文件".to_string() } else { name };
                        // 剪贴板里的截图常叫 image.png，给个有意义的名字
                        let name = if name == "image.png" { format!("粘贴的图片-{}.png", chrono::Local::now().format("%Y%m%d-%H%M%S")) } else { name };
                        let path = dir.join(&name);
                        if std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &it.data)).is_ok() {
                            srcs.push((path, true));
                        } else {
                            cx.emit(ComposerEvent::Toast(format!("{name}：写入临时文件失败"), true));
                        }
                    }
                    self.ingest(srcs, cx);
                }
                editor::Event::Toast(m) => cx.emit(ComposerEvent::Toast(m, true)),
            }
        }
    }

    /// 键盘焦点交给正文编辑器；`start`：光标放到开头（否则末尾）
    fn focus_body(&mut self, start: bool, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.body_focus);
        if self.editor_ready {
            editor::focus_native();
            editor::run_js(&format!("EM.focus({start})"));
        } else {
            self.want_body_focus = true;
        }
        cx.notify();
    }

    #[cfg(feature = "dev")]
    pub fn focus_body_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_body(true, window, cx);
    }

    /// 测试用：像「插入图片」选了文件一样收进本地文件（图片插进正文）
    #[cfg(feature = "dev")]
    pub fn ingest_for_test(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        self.ingest(vec![(path, false)], cx);
    }

    /// 当前正文（草稿格式）
    pub fn body_text(&self, _cx: &App) -> String {
        self.body_cache.clone()
    }

    /// 测试 / 外部预填用：在光标处插入文字
    pub fn insert_body_text(&mut self, s: &str, _cx: &mut Context<Self>) {
        self.run_js(format!("EM.insertText({})", editor::js_str(s)));
    }

    /// 测试用：直接对编辑器执行命令（bold / foreColor 等）
    pub fn editor_exec(&mut self, cmd: &str, val: Option<&str>) {
        self.run_js(format!("EM.exec({}, {})", editor::js_str(cmd), val.map(editor::js_str).unwrap_or_else(|| "undefined".into())));
    }

    pub fn has_preview(&self) -> bool {
        self.preview.is_some()
    }

    /// 把本地文件收进上传区（后台）：图片插进正文，其余作为附件。`(路径, 是否临时文件)`，临时文件收完就删。
    fn ingest(&mut self, srcs: Vec<(std::path::PathBuf, bool)>, cx: &mut Context<Self>) {
        if srcs.is_empty() {
            return;
        }
        let store = self.eng.store().clone();
        let task = cx.background_spawn(async move {
            srcs.into_iter()
                .map(|(src, tmp)| {
                    let name = src.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    let r = mailer::add_upload(&store, &src).map_err(|e| format!("{name}：{e}"));
                    let uri =
                        if r.is_ok() && is_image_file(&src) { std::fs::read(&src).ok().map(|b| image_data_uri(&b, mailer::mime_of(&name))) } else { None };
                    if tmp {
                        let _ = std::fs::remove_file(&src);
                        if let Some(d) = src.parent() {
                            let _ = std::fs::remove_dir(d);
                        }
                    }
                    r.map(|att| (att, uri))
                })
                .collect::<Vec<Result<(crate::core::model::DraftAtt, Option<String>), String>>>()
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let results = task.await;
            this.update(cx, |c, cx| {
                for r in results {
                    match r {
                        Ok((att, Some(uri))) => {
                            c.run_js(format!("EM.insertImage({}, {}, {})", editor::js_str(&att.token), editor::js_str(&att.filename), editor::js_str(&uri)))
                        }
                        Ok((att, None)) => c.draft.attachments.push(att),
                        Err(e) => cx.emit(ComposerEvent::Toast(e, true)),
                    }
                }
                c.touch(cx);
            })
            .ok();
        })
        .detach();
    }

    /// 工具栏「插入图片」：选文件（图片进正文，别的当附件）
    fn pick_images(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("插入图片".into()),
            allowed_extensions: None,
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if let Some(paths) = rx.await.ok().and_then(|r| r.ok()).flatten() {
                this.update(cx, |c, cx| c.ingest(paths.into_iter().filter(|p| !p.is_dir()).map(|p| (p, false)).collect(), cx)).ok();
            }
        })
        .detach();
    }

    /// 打开原地预览：把路径交给 Quick Look
    fn open_preview_path(&mut self, name: String, path: std::path::PathBuf, cx: &mut Context<Self>) {
        self.preview = Some((name, path));
        cx.notify();
    }

    /// 上传的文件在预览用的副本（带扩展名；启动时随预览缓存清空，需要时重建）
    fn display_copy(eng: &Engine, token: &str, name: &str) -> Option<std::path::PathBuf> {
        let dir = super::app::preview_dir().join("inline").join(token);
        let safe: String = name.chars().map(|c| if "/\\:\0".contains(c) { '_' } else { c }).collect();
        let path = dir.join(if safe.trim().is_empty() { "image" } else { safe.as_str() });
        if !path.exists() {
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::copy(eng.store().uploads_dir().join(token), &path).ok()?;
        }
        Some(path)
    }

    /// 点附件：把上传的文件拷成带扩展名的临时文件（后台），再预览
    fn open_att_preview(&mut self, token: String, name: String, cx: &mut Context<Self>) {
        let eng = self.eng.clone();
        let n = name.clone();
        let task = cx.background_spawn(async move { Self::display_copy(&eng, &token, &n) });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let path = task.await;
            this.update(cx, |c, cx| match path {
                Some(p) => c.open_preview_path(name, p, cx),
                None => cx.emit(ComposerEvent::Toast("无法预览：附件文件读取失败".into(), true)),
            })
            .ok();
        })
        .detach();
    }

    pub fn set_to(&mut self, to: &str, cx: &mut Context<Self>) {
        self.to.update(cx, |i, cx| {
            i.set_text(to, cx);
            i.collapse_to_end(cx);
        });
        self.touch(cx);
    }

    fn input_of(&self, f: Field) -> &Entity<LineInput> {
        match f {
            Field::To => &self.to,
            Field::Cc => &self.cc,
            Field::Bcc => &self.bcc,
        }
    }

    fn on_input(&mut self, f: Option<Field>, ev: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            InputEvent::Changed => {
                self.touch(cx);
                if let Some(f) = f {
                    let v = self.input_of(f).read(cx).text();
                    let tok = last_token(&v).to_string();
                    self.suggest_gen += 1;
                    self.suggest_for = Some(f);
                    if tok.is_empty() {
                        self.suggest.clear();
                    } else {
                        // 联系人查询会扫已发送邮件，放到后台，别让每敲一个字都卡主线程
                        let gen = self.suggest_gen;
                        let store = self.eng.store().clone();
                        let task = cx.background_spawn(async move { queries::contacts(&store, &tok).unwrap_or_default() });
                        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                            let v = task.await;
                            this.update(cx, |c, cx| {
                                if c.suggest_gen == gen {
                                    c.suggest = v.into_iter().take(8).collect();
                                    c.hi = 0;
                                    cx.notify();
                                }
                            })
                            .ok();
                        })
                        .detach();
                    }
                    cx.notify();
                }
            }
            InputEvent::Down if f.is_some() && f == self.suggest_for && !self.suggest.is_empty() => {
                self.hi = (self.hi + 1).min(self.suggest.len() - 1);
                cx.notify();
            }
            InputEvent::Up if f.is_some() && f == self.suggest_for && !self.suggest.is_empty() => {
                self.hi = self.hi.saturating_sub(1);
                cx.notify();
            }
            InputEvent::Submit { .. } => {
                if let (Some(fd), true) = (f, f == self.suggest_for && !self.suggest.is_empty()) {
                    let c = self.suggest[self.hi.min(self.suggest.len() - 1)].clone();
                    self.pick(fd, &c, cx);
                } else {
                    // 回车跳到下一栏
                    let next = match f {
                        Some(Field::To) if self.show_cc => self.cc.focus_handle(cx),
                        Some(Field::Cc) => self.bcc.focus_handle(cx),
                        Some(_) => self.subject.focus_handle(cx),
                        None => {
                            self.focus_body(true, window, cx);
                            return;
                        }
                    };
                    window.focus(&next);
                }
            }
            InputEvent::Tab { back } => {
                // 补全列表打开时，Tab 接受高亮的联系人（和 Gmail / Outlook 一致）；否则按顺序跳栏
                if let (Some(fd), false, true) = (f, *back, f == self.suggest_for && !self.suggest.is_empty()) {
                    let c = self.suggest[self.hi.min(self.suggest.len() - 1)].clone();
                    self.pick(fd, &c, cx);
                } else {
                    let slot = match f {
                        Some(Field::To) => Slot::To,
                        Some(Field::Cc) => Slot::Cc,
                        Some(Field::Bcc) => Slot::Bcc,
                        None => Slot::Subject,
                    };
                    self.suggest.clear();
                    self.focus_step(slot, *back, window, cx);
                }
            }
            InputEvent::Cancel => {
                if !self.suggest.is_empty() {
                    self.suggest.clear();
                    cx.notify();
                } else {
                    self.close(window, cx);
                }
            }
            _ => {}
        }
    }

    /// Tab / Shift+Tab：收件人 → (抄送 → 密送) → 主题 → 正文，首尾相接
    fn focus_step(&mut self, from: Slot, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut order = vec![Slot::To];
        if self.show_cc {
            order.push(Slot::Cc);
            order.push(Slot::Bcc);
        }
        order.push(Slot::Subject);
        order.push(Slot::Body);
        let Some(i) = order.iter().position(|s| *s == from) else { return };
        let n = order.len();
        let next = order[if back { (i + n - 1) % n } else { (i + 1) % n }];
        let h = match next {
            Slot::To => self.to.focus_handle(cx),
            Slot::Cc => self.cc.focus_handle(cx),
            Slot::Bcc => self.bcc.focus_handle(cx),
            Slot::Subject => self.subject.focus_handle(cx),
            Slot::Body => {
                self.focus_body(!back, window, cx);
                return;
            }
        };
        // 离开正文：原生焦点先还给 GPUI，输入框才收得到键盘
        editor::resign();
        window.focus(&h);
    }

    fn pick(&mut self, f: Field, c: &Contact, cx: &mut Context<Self>) {
        let v = self.input_of(f).read(cx).text();
        let nv = pick_contact(&v, c);
        self.input_of(f).update(cx, |i, cx| {
            i.set_text(&nv, cx);
            i.collapse_to_end(cx);
        });
        self.suggest.clear();
        self.touch(cx);
    }

    fn collect(&mut self, cx: &App) {
        self.draft.to = self.to.read(cx).text();
        self.draft.cc = self.cc.read(cx).text();
        self.draft.bcc = self.bcc.read(cx).text();
        self.draft.subject = self.subject.read(cx).text();
        self.draft.body = self.body_text(cx);
    }

    fn has_content(&self) -> bool {
        let d = &self.draft;
        !d.to.trim().is_empty() || !d.subject.trim().is_empty() || !mailer::body_is_blank(&d.body) || !d.attachments.is_empty()
    }

    /// 有改动：标记，并在最后一次输入 2.5 秒后自动存草稿（崩溃 / 误关窗口都不会丢稿）
    fn touch(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        self.save_gen += 1;
        let gen = self.save_gen;
        self.autosave = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(Duration::from_millis(AUTOSAVE_MS)).await;
            this.update(cx, |c, cx| {
                if c.save_gen == gen && !c.sending && c.dirty {
                    c.save_draft(cx);
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// 草稿同步到服务器的「草稿箱」（后台进行，失败只记日志）
    fn sync_remote(&self) {
        if let Some(id) = self.draft.id {
            let eng = self.eng.clone();
            let fut = self.eng.call(mailer::sync_draft(eng, id));
            self.eng.spawn(async move {
                if let Err(e) = fut.await {
                    log::warn!("[draft] 同步到服务器草稿箱失败：{e:#}");
                }
            });
        }
    }

    pub fn save_draft(&mut self, cx: &mut Context<Self>) -> Option<i64> {
        self.collect(cx);
        if !self.dirty || !self.has_content() {
            return self.draft.id;
        }
        self.dirty = false;
        match mailer::save_draft(self.eng.store(), &self.draft) {
            Ok(d) => {
                self.draft.id = d.id;
                self.saved_at = Some(d.updated_at);
                cx.notify();
                d.id
            }
            Err(e) => {
                self.dirty = true;
                cx.emit(ComposerEvent::Toast(format!("草稿保存失败：{e}"), true));
                self.draft.id
            }
        }
    }

    pub fn close(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.take().is_some() {
            webview::hide_quicklook();
            cx.notify();
            return;
        }
        if self.sending {
            return;
        }
        self.collect(cx);
        // Esc / ✕：有内容就存为草稿再关（以前直接丢弃，写了半天的信一按 Esc 就没了）；空信直接关
        if !self.has_content() {
            if self.draft.id.is_none() {
                self.drop_uploads();
            }
            self.save_gen += 1;
            cx.emit(ComposerEvent::Closed { saved: false });
            return;
        }
        let changed = self.dirty;
        self.save_draft(cx);
        if self.dirty {
            // 保存失败：不关窗口，免得丢稿（错误已提示）
            return;
        }
        if changed {
            self.sync_remote();
        }
        self.save_gen += 1;
        cx.emit(ComposerEvent::Closed { saved: changed });
    }

    /// 显式保存为草稿并关闭窗口
    pub(super) fn save_and_close(&mut self, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        self.collect(cx);
        if !self.has_content() {
            cx.emit(ComposerEvent::Toast("这封信还是空的，没有可保存的内容".into(), true));
            return;
        }
        self.dirty = true;
        self.save_draft(cx);
        if self.draft.id.is_some() && !self.dirty {
            self.sync_remote();
            self.save_gen += 1;
            cx.emit(ComposerEvent::Closed { saved: true });
        }
    }

    /// 新建（尚未存过草稿）的信被丢弃时，清掉已上传的附件与内嵌图片
    fn drop_uploads(&self) {
        let store = self.eng.store();
        for a in &self.draft.attachments {
            mailer::delete_upload(store, &a.token);
        }
        for t in mailer::inline_tokens(&self.draft.body) {
            mailer::delete_upload(store, &t);
        }
    }

    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.collect(cx);
        if !self.has_content() {
            self.do_discard(cx);
            return;
        }
        let answer = window.prompt(PromptLevel::Warning, "丢弃这封信？", Some("草稿与已添加的附件都会删除。"), &["丢弃", "取消"], cx);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if answer.await.ok() == Some(0) {
                this.update(cx, |c, cx| c.do_discard(cx)).ok();
            }
        })
        .detach();
    }

    fn do_discard(&mut self, cx: &mut Context<Self>) {
        let store = self.eng.store();
        match self.draft.id {
            Some(id) => {
                let _ = store;
                let _ = mailer::delete_draft_everywhere(&self.eng, id, false);
            }
            None => self.drop_uploads(),
        }
        self.save_gen += 1;
        cx.emit(ComposerEvent::Closed { saved: false });
    }

    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        self.collect(cx);
        if self.draft.to.trim().is_empty() && self.draft.cc.trim().is_empty() && self.draft.bcc.trim().is_empty() {
            cx.emit(ComposerEvent::Toast("请先填写收件人".into(), true));
            return;
        }
        if self.draft.account_id.is_none() {
            cx.emit(ComposerEvent::Toast("没有可用的发件账号".into(), true));
            return;
        }
        if self.draft.subject.trim().is_empty() {
            let answer = window.prompt(PromptLevel::Info, "这封信没有主题，仍然发送？", None, &["发送", "取消"], cx);
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                if answer.await.ok() == Some(0) {
                    this.update(cx, |c, cx| c.do_send(cx)).ok();
                }
            })
            .detach();
            return;
        }
        self.do_send(cx);
    }

    pub fn do_send(&mut self, cx: &mut Context<Self>) {
        self.send_at(None, cx);
    }

    /// 放进发件箱：`at` 为 None 时按撤回窗口（默认 5 秒）发出，否则定时发送。
    /// 草稿先存好：撤回后信回到草稿；断网时信留在发件箱里，联网后自动发出
    pub fn send_at(&mut self, at: Option<i64>, cx: &mut Context<Self>) {
        self.schedule_menu = false;
        self.dirty = true;
        self.save_gen += 1;
        let draft_id = self.save_draft(cx);
        let d = &self.draft;
        let o = Outgoing {
            account_id: d.account_id.unwrap_or_default(),
            mode: d.mode.clone(),
            to: d.to.clone(),
            cc: d.cc.clone(),
            bcc: d.bcc.clone(),
            subject: d.subject.clone(),
            text: d.body.clone(),
            attachments: d.attachments.clone(),
            source_id: d.source_id,
            include_original: d.include_original,
            draft_id,
            ..Default::default()
        };
        match crate::core::outbox::enqueue(self.eng.store(), &o, at) {
            Ok(id) => {
                let send_at = at.unwrap_or_else(|| crate::core::db::now_ms() + crate::core::outbox::undo_secs(self.eng.store()) * 1000);
                self.save_gen += 1;
                cx.emit(ComposerEvent::Queued { outbox_id: id, send_at, scheduled: at.is_some() });
            }
            Err(e) => cx.emit(ComposerEvent::Toast(e.to_string(), true)),
        }
        cx.notify();
    }

    /// 定时发送前的检查（收件人、发件账号）
    fn schedule(&mut self, at: i64, cx: &mut Context<Self>) {
        self.collect(cx);
        if self.draft.to.trim().is_empty() && self.draft.cc.trim().is_empty() && self.draft.bcc.trim().is_empty() {
            self.schedule_menu = false;
            cx.emit(ComposerEvent::Toast("请先填写收件人".into(), true));
            return;
        }
        self.send_at(Some(at), cx);
    }

    /// 附件拷贝（单个最大 25 MB）在后台做，完成后再挂到草稿上
    pub fn add_files(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let store = self.eng.store().clone();
        let task = cx.background_spawn(async move {
            paths
                .into_iter()
                .filter(|p| !p.is_dir())
                .map(|p| {
                    let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    mailer::add_upload(&store, &p).map_err(|e| format!("{name}：{e}"))
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let results = task.await;
            this.update(cx, |c, cx| {
                for r in results {
                    match r {
                        Ok(a) => c.draft.attachments.push(a),
                        Err(e) => cx.emit(ComposerEvent::Toast(e, true)),
                    }
                }
                c.touch(cx);
            })
            .ok();
        })
        .detach();
    }

    fn pick_files(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("添加附件".into()),
            allowed_extensions: None,
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if let Some(paths) = rx.await.ok().and_then(|r| r.ok()).flatten() {
                this.update(cx, |c, cx| c.add_files(paths, cx)).ok();
            }
        })
        .detach();
    }

    fn remove_att(&mut self, token: &str, cx: &mut Context<Self>) {
        self.draft.attachments.retain(|a| a.token != token);
        mailer::delete_upload(self.eng.store(), token);
        self.touch(cx);
    }

    /// 仅供界面端到端测试：打开 / 关闭「模板」菜单
    pub fn set_template_menu_for_test(&mut self, on: bool, cx: &mut Context<Self>) {
        self.template_menu = on;
        self.schedule_menu = false;
        cx.notify();
    }

    fn render_addr(&self, t: &Theme, f: Field, label: &'static str, window: &Window, cx: &mut Context<Self>) -> Div {
        let input = self.input_of(f).clone();
        let focused = input.read(cx).is_focused(window);
        let mut col = div().w_full().child(
            div()
                .flex()
                .w_full()
                .items_center()
                .gap(px(10.))
                .h(px(40.))
                .border_b_1()
                .border_color(if focused { t.accent } else { t.border_soft })
                .child(div().w(px(64.)).flex_none().text_size(px(12.)).text_color(t.muted).child(label))
                .child(div().flex_1().min_w_0().text_size(px(13.)).child(input))
                .when(f == Field::To && !self.show_cc, |d| {
                    d.child(button("show-cc", t, Kind::Ghost, None, "抄送 / 密送", true).on_click(cx.listener(|c, _, _, cx| {
                        c.show_cc = true;
                        cx.notify();
                    })))
                }),
        );
        if self.suggest_for == Some(f) && focused && !self.suggest.is_empty() {
            let mut list = div().ml(px(74.)).my(px(4.)).p(px(4.)).rounded(px(8.)).border_1().border_color(t.border).bg(t.panel);
            for (i, c) in self.suggest.iter().enumerate() {
                let c2 = c.clone();
                list = list.child(
                    menu_row(
                        SharedString::from(format!("sg-{i}")),
                        t,
                        Some("user"),
                        if c.name.is_empty() { c.address.clone() } else { c.name.clone() },
                        i == self.hi,
                    )
                    .child(div().text_size(px(11.)).text_color(t.muted).child(if c.name.is_empty() { String::new() } else { c.address.clone() }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |comp, _, _, cx| {
                            cx.stop_propagation();
                            comp.pick(f, &c2, cx)
                        }),
                    ),
                );
            }
            col = col.child(list);
        }
        col
    }
}

impl Focusable for Composer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.to.focus_handle(cx)
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let acc = self.sendable.iter().find(|a| Some(a.id) == self.draft.account_id).cloned();
        let sub = acc.as_ref().map(|a| format!("经 {}{}", a.smtp_host, if a.use_proxy { " · 代理" } else { "" })).unwrap_or_default();
        let subject_focused = self.subject.read(cx).is_focused(window);
        let close = button("composer-x", &t, Kind::Ghost, Some("close"), "", true).on_click(cx.listener(|c, _, w, cx| c.close(w, cx)));
        let from_label = acc
            .as_ref()
            .map(|a| if a.name.is_empty() { a.email.clone() } else { format!("{} <{}>", a.name, a.email) })
            .unwrap_or_else(|| "（没有可用的发件账号）".into());
        let mut from_row = div().relative().w_full().child(
            div()
                .flex()
                .w_full()
                .items_center()
                .gap(px(10.))
                .h(px(40.))
                .border_b_1()
                .border_color(t.border_soft)
                .child(div().w(px(64.)).flex_none().text_size(px(12.)).text_color(t.muted).child("发件账号"))
                .child(
                    div()
                        .id("from-select")
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(8.))
                        .h(px(28.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(t.row_hover))
                        .text_size(px(13.))
                        .child(from_label)
                        .when(self.sendable.len() > 1, |d| d.child(icon("caret", 12., t.muted)))
                        .on_click(cx.listener(|c, _, _, cx| {
                            c.acct_menu = !c.acct_menu && c.sendable.len() > 1;
                            cx.notify();
                        })),
                ),
        );
        if self.acct_menu {
            let mut m = div().ml(px(74.)).my(px(4.)).p(px(4.)).rounded(px(8.)).border_1().border_color(t.border).bg(t.panel);
            for a in self.sendable.clone() {
                let id = a.id;
                m = m.child(
                    menu_row(
                        SharedString::from(format!("from-{id}")),
                        &t,
                        Some("user"),
                        if a.name.is_empty() { a.email.clone() } else { format!("{} <{}>", a.name, a.email) },
                        Some(id) == self.draft.account_id,
                    )
                    .on_click(cx.listener(move |c, _, _, cx| {
                        c.draft.account_id = Some(id);
                        c.acct_menu = false;
                        c.touch(cx);
                    })),
                );
            }
            from_row = from_row.child(m);
        }

        let mut atts = div().flex().flex_wrap().gap(px(8.));
        for a in self.draft.attachments.clone() {
            let tok = a.token.clone();
            atts = atts.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(28.))
                    .pl(px(10.))
                    .pr(px(2.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.panel_2)
                    .text_size(px(12.))
                    .child(
                        div()
                            .id(SharedString::from(format!("att-open-{}", a.token)))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .cursor_pointer()
                            .child(icon("clip", 12., t.muted))
                            .child(one_line(&a.filename, 40))
                            .child(div().text_size(px(10.5)).text_color(t.muted).child(fmt_size(a.size)))
                            .on_click(cx.listener({
                                let (tok, name) = (a.token.clone(), a.filename.clone());
                                move |c, _, _, cx| c.open_att_preview(tok.clone(), name.clone(), cx)
                            })),
                    )
                    .child(
                        button(SharedString::from(format!("rm-{}", a.token)), &t, Kind::Ghost, Some("close"), "", true)
                            .on_click(cx.listener(move |c, _, _, cx| c.remove_att(&tok, cx))),
                    ),
            );
        }

        // 预览盖住整个窗口时把原生编辑器藏起来（预览自带不透明底，不需要快照）
        let want_hidden = self.preview.is_some();
        if editor::is_frozen() != want_hidden {
            editor::set_frozen(want_hidden);
        }
        let key = self.editor_key;
        let doc = self.editor_doc.clone();
        let editor_box = div()
            .id("compose-body")
            .track_focus(&self.body_focus)
            .flex_1()
            // 模板 / 定时菜单在底栏上方占位（不能盖在原生编辑器上），编辑器要让出高度，否则底栏会被挤出窗口
            .min_h(px(if self.template_menu || self.schedule_menu { 90. } else { 240. }))
            .mt(px(12.))
            .mx(px(22.))
            .p(px(2.))
            .rounded(px(8.))
            .border_1()
            .border_color(if self.drop_hover { t.accent } else { t.border_soft })
            .child(
                canvas(
                    |b, _, _| b,
                    move |b, _, window, _| {
                        let vis = b.intersect(&window.content_mask().bounds);
                        editor::show(window, vis, key, &doc)
                    },
                )
                .size_full(),
            );
        let hint = match self.saved_at {
            Some(ts) => format!("草稿已自动保存 {}", &fmt_full(ts)[11..]),
            None => "⌘ + Enter 发送 · 输入后自动保存草稿".into(),
        };

        let overlay = self.preview.clone().map(|(name, path)| {
            div()
                .id("composer-preview")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(t.panel)
                .occlude()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(10.))
                        .px(px(22.))
                        .py(px(14.))
                        .border_b_1()
                        .border_color(t.border_soft)
                        .child(div().flex_1().min_w_0().font_family(super::SERIF).text_size(px(16.)).font_weight(FontWeight::BOLD).child(one_line(&name, 60)))
                        .child(button("cpreview-x", &t, Kind::Ghost, Some("close"), "", true).on_click(cx.listener(|c, _, _, cx| {
                            c.preview = None;
                            webview::hide_quicklook();
                            cx.notify();
                        }))),
                )
                .child(
                    div().flex_1().min_h_0().bg(t.panel_2).child(
                        canvas(
                            |b, _, _| b,
                            move |b, _, window, _| {
                                let vis = b.intersect(&window.content_mask().bounds);
                                webview::show_quicklook(window, vis, &path)
                            },
                        )
                        .size_full(),
                    ),
                )
        });
        let vp = window.viewport_size();
        let h = (f32::from(vp.height) * 0.9).clamp(460., 780.);
        let shell = if self.window_mode {
            window_shell(&t, title_of(&self.draft.mode), sub, close).size_full()
        } else {
            modal_shell(&t, 760., title_of(&self.draft.mode), sub, close).relative().h(px(h))
        };
        shell
            .key_context("Composer")
            .on_action(cx.listener(|c, _: &SendMail, w, cx| c.send(w, cx)))
            // 点正文以外的地方：原生焦点还给 GPUI（编辑器自己的点击不会到这里）
            .capture_any_mouse_down(|_, _, _| editor::resign())
            .on_drag_move::<ExternalPaths>(cx.listener(|c, ev: &DragMoveEvent<ExternalPaths>, _, cx| {
                let inside = ev.bounds.contains(&ev.event.position);
                if inside != c.drop_hover {
                    c.drop_hover = inside;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|c, paths: &ExternalPaths, _, cx| {
                c.drop_hover = false;
                c.add_files(paths.paths().to_vec(), cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w_full()
                    .px(px(22.))
                    .pt(px(6.))
                    .child(from_row)
                    .child(self.render_addr(&t, Field::To, "收件人", window, cx))
                    .when(self.show_cc, |d| {
                        d.child(self.render_addr(&t, Field::Cc, "抄送", window, cx)).child(self.render_addr(&t, Field::Bcc, "密送", window, cx))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .h(px(40.))
                            .border_b_1()
                            .border_color(if subject_focused { t.accent } else { t.border_soft })
                            .child(div().w(px(64.)).flex_none().text_size(px(12.)).text_color(t.muted).child("主题"))
                            .child(div().flex_1().min_w_0().text_size(px(13.)).child(self.subject.clone())),
                    ),
            )
            .child(editor_box)
            .child(
                div()
                    .flex_none()
                    .px(px(22.))
                    .pt(px(8.))
                    .pb(px(10.))
                    .max_h(px(150.))
                    .overflow_hidden()
                    .when(self.draft.mode == "forward" && self.draft.source_id.is_some(), |d| {
                        d.child(check_row("orig-att", &t, self.draft.include_original, "附带原邮件的附件").mb(px(8.)).on_click(cx.listener(|c, _, _, cx| {
                            c.draft.include_original = !c.draft.include_original;
                            c.touch(cx);
                        })))
                    })
                    .when(!self.draft.attachments.is_empty(), |d| d.child(atts.mb(px(8.))))
                    .child(div().text_size(px(11.)).text_color(t.muted).child(format!(
                        "附件：单个不超过 {} MB，支持任意格式；可点「添加附件」或拖入窗口。部分邮箱会拦截 .exe、.bat 等可执行文件，整封邮件通常有总大小上限（多数 25～50 MB）。",
                        crate::core::mailer::MAX_UPLOAD / 1024 / 1024
                    ))),
            )
            .when(self.template_menu, |d| {
                let list = crate::ui::app::panels::load_templates(self.eng.store());
                let mut menu = div()
                    .id("tpl-menu")
                    .flex_none()
                    .mx(px(22.))
                    .mb(px(8.))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(2.))
                    .p(px(6.))
                    .max_h(px(112.))
                    .overflow_y_scroll()
                    .rounded(px(10.))
                    .bg(t.panel)
                    .border_1()
                    .border_color(t.border)
                    .shadow(vec![BoxShadow { color: t.shadow, offset: point(px(0.), px(8.)), blur_radius: px(24.), spread_radius: px(0.) }])
                    .child(div().px(px(10.)).py(px(6.)).text_size(px(11.5)).text_color(t.muted).child(if list.is_empty() {
                        "还没有模板：⌘K →「回复模板」添加"
                    } else {
                        "插入到光标处"
                    }));
                for (i, tp) in list.into_iter().enumerate() {
                    let body = tp.body.clone();
                    menu = menu.child(
                        div()
                            .id(("tpl", i))
                            .px(px(10.))
                            .py(px(7.))
                            .rounded(px(6.))
                            .text_size(px(13.))
                            .cursor_pointer()
                            .hover(|s| s.bg(t.row_hover))
                            .child(tp.name)
                            .on_click(cx.listener(move |c, _, _, cx| {
                                c.template_menu = false;
                                c.insert_body_text(&body, cx);
                                c.touch(cx);
                                cx.notify();
                            })),
                    );
                }
                d.child(menu)
            })
            .when(self.schedule_menu, |d| {
                let mut menu = div()
                    .mx(px(22.))
                    .mb(px(8.))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(2.))
                    .p(px(6.))
                    .rounded(px(10.))
                    .bg(t.panel)
                    .border_1()
                    .border_color(t.border)
                    .shadow(vec![BoxShadow { color: t.shadow, offset: point(px(0.), px(8.)), blur_radius: px(24.), spread_radius: px(0.) }])
                    .child(div().px(px(10.)).py(px(6.)).text_size(px(11.5)).text_color(t.muted).child("定时发送（到点前可在发件箱撤回）"));
                for (i, (label, at)) in crate::core::snooze::presets().into_iter().enumerate() {
                    let when = fmt_full(at);
                    menu = menu.child(
                        div()
                            .id(("sched", i))
                            .px(px(10.))
                            .py(px(7.))
                            .rounded(px(6.))
                            .flex()
                            .gap(px(6.))
                            .text_size(px(13.))
                            .cursor_pointer()
                            .hover(|s| s.bg(t.row_hover))
                            .child(label)
                            .child(div().text_color(t.muted).text_size(px(11.)).child(when[5..16].to_string()))
                            .on_click(cx.listener(move |c, _, _, cx| c.schedule(at, cx))),
                    );
                }
                d.child(menu)
            })
            .child(
                modal_footer(&t)
                    .child(button("discard", &t, Kind::Ghost, Some("trash"), "丢弃", true).on_click(cx.listener(|c, _, w, cx| c.discard(w, cx))))
                    .child(button("attach", &t, Kind::Ghost, Some("clip"), "添加附件", true).on_click(cx.listener(|c, _, _, cx| c.pick_files(cx))))
                    .child(button("templates", &t, Kind::Ghost, Some("draft"), "模板", true).on_click(cx.listener(|c, _, _, cx| {
                        c.template_menu = !c.template_menu;
                        c.schedule_menu = false;
                        cx.notify();
                    })))
                    // 提示文字可收缩：窗口窄时显示省略号，不把「发送」挤出去
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(11.5)).text_color(t.muted).child(hint))
                    .child(
                        disabled(button("save-draft", &t, Kind::Secondary, Some("draft"), "保存为草稿", false), self.sending)
                            .on_click(cx.listener(|c, _, _, cx| c.save_and_close(cx))),
                    )
                    .child(
                        disabled(button("schedule", &t, Kind::Secondary, Some("clock"), "定时", false), self.sending).on_click(cx.listener(|c, _, _, cx| {
                            c.schedule_menu = !c.schedule_menu;
                            c.template_menu = false;
                            cx.notify();
                        })),
                    )
                    .child(
                        disabled(button("send", &t, Kind::Primary, Some("send"), if self.sending { "发送中…" } else { "发送" }, false), self.sending)
                            .on_click(cx.listener(|c, _, w, cx| c.send(w, cx))),
                    ),
            )
            .children(overlay)
    }
}

// ───────────────────────────── 回复 / 转发预填 ─────────────────────────────

fn has_prefix(subject: &str, prefixes: &[&str]) -> bool {
    let s = subject.trim_start().to_lowercase();
    prefixes.iter().any(|p| s.starts_with(&format!("{p}:")) || s.starts_with(&format!("{p}：")))
}

/// HTML 去掉 `data:` 图片数据后的长度
fn html_weight(html: &str) -> usize {
    use std::sync::LazyLock;
    static DATA: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)data:image/[a-z0-9.+-]+;base64,[A-Za-z0-9+/=\s]+").expect("regex"));
    html.len() - DATA.find_iter(html).map(|m| m.len()).sum::<usize>()
}

pub fn build_reply(msg: &Detail, mode: &str, accounts: &[Account]) -> Draft {
    let selfs: std::collections::HashSet<String> = accounts.iter().filter(|a| !a.is_local()).map(|a| a.email.to_lowercase()).collect();
    let acc = accounts.iter().find(|a| a.id == msg.account_id && !a.is_local()).or_else(|| accounts.iter().find(|a| !a.is_local()));
    let sig = acc.filter(|a| !a.signature.is_empty()).map(|a| format!("\n\n-- \n{}", a.signature)).unwrap_or_default();
    let quote = msg.text.split('\n').map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n");
    let when = fmt_full(msg.date);
    // 原邮件是 HTML：引用时保留富文本（编辑器里清洗后接在正文末尾）；太大或没有 HTML 就退回纯文本引用
    // 大小只算 HTML 本身：内嵌图片（cid 已展开成 data URI）会被收进上传区，不进编辑器文档
    let rich = !msg.html.trim().is_empty() && html_weight(&msg.html) <= 3_000_000;
    let quote_html = if rich { msg.html.clone() } else { String::new() };
    if mode == "forward" {
        let mut head = vec![
            "---------- 转发的邮件 ----------".to_string(),
            format!("发件人：{}", msg.from.text()),
            format!("时间：{when}"),
            format!("主题：{}", msg.subject),
            format!("收件人：{}", addrs_text(&msg.to)),
        ];
        if !msg.cc.is_empty() {
            head.push(format!("抄送：{}", addrs_text(&msg.cc)));
        }
        return Draft {
            mode: mode.into(),
            account_id: acc.map(|a| a.id),
            source_id: Some(msg.id),
            subject: if has_prefix(&msg.subject, &["fwd", "fw", "转发"]) { msg.subject.clone() } else { format!("Fwd: {}", msg.subject) },
            body: if rich { format!("{sig}\n\n{}\n", head.join("\n")) } else { format!("{sig}\n\n{}\n\n{}", head.join("\n"), msg.text) },
            quote_html,
            include_original: msg.attachments.iter().any(|a| !a.inline),
            ..Default::default()
        };
    }
    let reply_to = if !msg.reply_to.is_empty() && msg.reply_to != msg.from.address { msg.reply_to.clone() } else { msg.from.text() };
    let mut to = vec![reply_to];
    let mut cc = vec![];
    if mode == "replyAll" {
        let mut skip = selfs.clone();
        skip.insert(msg.from.address.to_lowercase());
        to.extend(msg.to.iter().filter(|a| !skip.contains(&a.address.to_lowercase())).map(|a| a.text()));
        cc = msg.cc.iter().filter(|a| !skip.contains(&a.address.to_lowercase())).map(|a| a.text()).collect();
    }
    Draft {
        mode: mode.into(),
        account_id: acc.map(|a| a.id),
        source_id: Some(msg.id),
        to: to.join(", "),
        cc: cc.join(", "),
        subject: if has_prefix(&msg.subject, &["re", "回复"]) { msg.subject.clone() } else { format!("Re: {}", msg.subject) },
        body: if rich {
            format!("{sig}\n\n在 {when}，{} 写道：", msg.from.text())
        } else {
            format!("{sig}\n\n在 {when}，{} 写道：\n{quote}", msg.from.text())
        },
        quote_html,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    // 不用 `super::*`：会把 gpui 的 `test` 宏引进来遮蔽内置 `#[test]`
    use super::{build_reply, html_weight, last_token, pick_contact, tokenize_quote_images};
    use crate::core::model::{Account, Contact, Detail};
    use crate::core::model::{Addr, AttMeta};

    fn acc(id: i64, email: &str, sig: &str) -> Account {
        Account { id, email: email.into(), provider: "custom".into(), signature: sig.into(), ..Default::default() }
    }

    #[test]
    fn reply_all_skips_self_and_sender() {
        let accounts = vec![acc(1, "me@x.com", "David")];
        let msg = Detail {
            id: 9,
            account_id: 1,
            subject: "周报".into(),
            from: Addr { name: "Bob".into(), address: "bob@x.com".into() },
            to: vec![Addr { name: String::new(), address: "me@x.com".into() }, Addr { name: "Carol".into(), address: "carol@x.com".into() }],
            cc: vec![Addr { name: String::new(), address: "BOB@x.com".into() }, Addr { name: String::new(), address: "dan@x.com".into() }],
            text: "第一行\n第二行".into(),
            date: 1_700_000_000_000,
            ..Default::default()
        };
        let d = build_reply(&msg, "replyAll", &accounts);
        assert_eq!(d.to, "Bob <bob@x.com>, Carol <carol@x.com>");
        assert_eq!(d.cc, "dan@x.com");
        assert_eq!(d.subject, "Re: 周报");
        assert!(d.body.starts_with("\n\n-- \nDavid\n\n在 "));
        assert!(d.body.ends_with("> 第一行\n> 第二行"));
        assert_eq!(d.account_id, Some(1));
        let r = build_reply(&Detail { subject: "回复：周报".into(), ..msg.clone() }, "reply", &accounts);
        assert_eq!(r.subject, "回复：周报");
        assert_eq!(r.to, "Bob <bob@x.com>");
        let f = build_reply(&Detail { attachments: vec![AttMeta { filename: "a.pdf".into(), ..Default::default() }], ..msg }, "forward", &accounts);
        assert_eq!(f.subject, "Fwd: 周报");
        assert!(f.to.is_empty());
        assert!(f.include_original);
        assert!(f.body.contains("---------- 转发的邮件 ----------\n发件人：Bob <bob@x.com>"));
    }

    #[test]
    fn contact_pick() {
        let c = Contact { name: "Bob".into(), address: "bob@x.com".into() };
        assert_eq!(pick_contact("ali@x.com, bo", &c), "ali@x.com, Bob <bob@x.com>, ");
        assert_eq!(pick_contact("bo", &c), "Bob <bob@x.com>, ");
        assert_eq!(last_token("a@x.com，  ca"), "ca");
    }

    #[test]
    fn quote_images_become_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::core::db::Store::open(dir.path()).unwrap();
        // 1×1 PNG；第二张带伪造的 data-token 且引号是单引号
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let html = format!("<p>a</p><img width=10 src=\"data:image/png;base64,{png}\"><img data-token=\"stolen\" src='data:image/png;base64,{png}'><span data-token=\"x\">t</span>");
        let out = tokenize_quote_images(&store, &html);
        assert_eq!(out.matches("<img data-token=\"").count(), 2, "{out}");
        assert!(!out.contains("stolen") && out.contains("data-x=\"x\""), "伪造的 token 要被废掉：{out}");
        let toks = crate::core::mailer::inline_tokens(&format!("{}{out}", crate::core::mailer::HTML_MARK));
        assert_eq!(toks.len(), 2);
        for t in toks {
            assert!(crate::core::mailer::upload_info(&store, &t).is_some());
        }
        assert_eq!(html_weight(&html), html.len() - 2 * format!("data:image/png;base64,{png}").len());
    }
}
