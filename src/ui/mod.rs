//! GPUI 界面层：一个窗口，三栏（侧栏 / 邮件列表 / 阅读区）+ 写信、账号设置、导入三个模态。

pub mod accounts;
pub mod app;
#[cfg(feature = "dev")]
pub mod autotest;
pub mod composer;
pub mod freeze;
pub mod import;
pub mod line_input;
pub mod macos;
pub mod palettes;
pub mod reader_window;
pub mod text_area;
pub mod theme;
pub mod widgets;
pub mod window_state;

use gpui::*;
use std::borrow::Cow;

pub use theme::{theme, Theme};

pub const SERIF: &str = "Songti SC";
pub const SANS: &str = "PingFang SC";
pub const MONO: &str = "Menlo";

/// 内嵌资源：assets/icons/*.svg 与 logo.svg（编译进二进制）
pub struct Assets;

macro_rules! icons {
    ($($n:literal),* $(,)?) => {
        fn icon_bytes(name: &str) -> Option<&'static [u8]> {
            match name {
                $($n => Some(include_bytes!(concat!("../../assets/icons/", $n, ".svg"))),)*
                "logo" => Some(include_bytes!("../../assets/logo.svg")),
                _ => None,
            }
        }
    };
}

icons!(
    "archive",
    "caret",
    "check",
    "clip",
    "clock",
    "close",
    "download",
    "draft",
    "edit",
    "envelope",
    "eye",
    "eyeOff",
    "folder",
    "forward",
    "gear",
    "image",
    "inbox",
    "mail",
    "more",
    "move",
    "palette",
    "pen",
    "plus",
    "outbox",
    "read",
    "receipt",
    "refresh",
    "reply",
    "panel-right",
    "replyAll",
    "rule",
    "search",
    "send",
    "sent",
    "shield",
    "spam",
    "star-fill",
    "star",
    "trash",
    "unread",
    "user",
    "users",
);

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        let name = path.trim_start_matches("icons/").trim_end_matches(".svg");
        Ok(icon_bytes(name).map(Cow::Borrowed))
    }

    fn list(&self, _: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}

pub fn icon(name: &'static str, size: f32, color: Hsla) -> Svg {
    svg().path(format!("icons/{name}.svg")).size(px(size)).flex_none().text_color(color)
}

/// 文件夹角色 → 图标
pub fn role_icon(role: &str) -> &'static str {
    match role {
        "inbox" => "inbox",
        "sent" => "sent",
        "drafts" => "draft",
        "trash" => "trash",
        "junk" => "spam",
        "archive" => "archive",
        _ => "folder",
    }
}

/// 品牌标识：圆角方牌 + 信封 + 火漆印（品牌色固定，不随主题；assets/logo-256.png 由 logo.svg 栅格化）
pub fn logo(size: f32) -> Img {
    use std::sync::{Arc, LazyLock};
    static LOGO: LazyLock<Arc<Image>> = LazyLock::new(|| Arc::new(Image::from_bytes(ImageFormat::Png, include_bytes!("../../assets/logo-256.png").to_vec())));
    img(LOGO.clone()).size(px(size)).flex_none()
}

// ───────────────────────────── 格式化 ─────────────────────────────

use chrono::{Datelike, Local, TimeZone};

fn local(ts: i64) -> chrono::DateTime<Local> {
    Local.timestamp_millis_opt(ts).single().unwrap_or_else(Local::now)
}

pub fn fmt_full(ts: i64) -> String {
    local(ts).format("%Y-%m-%d %H:%M").to_string()
}

/// 列表里的日期：今天显示时间，昨天显示「昨天」，今年显示月日，更早显示年月日
pub fn fmt_short(ts: i64) -> String {
    fmt_short_at(ts, Local::now())
}

pub fn fmt_short_at(ts: i64, now: chrono::DateTime<Local>) -> String {
    let d = local(ts);
    if d.date_naive() == now.date_naive() {
        return d.format("%H:%M").to_string();
    }
    if now.date_naive().pred_opt() == Some(d.date_naive()) {
        return "昨天".into();
    }
    if d.year() == now.year() {
        return d.format("%m/%d").to_string();
    }
    d.format("%Y/%m/%d").to_string()
}

pub fn fmt_ago(ts: Option<i64>) -> String {
    let Some(ts) = ts.filter(|t| *t > 0) else { return "从未".into() };
    let s = (crate::core::db::now_ms() - ts) / 1000;
    if s < 60 {
        "刚刚".into()
    } else if s < 3600 {
        format!("{} 分钟前", s / 60)
    } else if s < 86400 {
        format!("{} 小时前", s / 3600)
    } else {
        fmt_full(ts)
    }
}

pub fn fmt_size(n: u64) -> String {
    if n == 0 {
        String::new()
    } else if n >= 1_048_576 {
        format!("{:.1} MB", n as f64 / 1_048_576.)
    } else if n >= 1024 {
        format!("{} KB", (n as f64 / 1024.).round() as u64)
    } else {
        format!("{n} B")
    }
}

/// 列表总数：超过计数上限时显示「10,000+」
pub fn fmt_total(n: i64) -> String {
    if n > crate::core::queries::COUNT_CAP {
        format!("{}+", fmt_num(crate::core::queries::COUNT_CAP))
    } else {
        fmt_num(n)
    }
}

pub fn fmt_num(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// 截断成单行（列表摘要用）
pub fn one_line(s: &str, max: usize) -> String {
    let t: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > max {
        t.chars().take(max).collect::<String>() + "…"
    } else {
        t
    }
}

// ───────────────────────────── 全局动作与按键 ─────────────────────────────

actions!(
    easymail,
    [
        Quit,
        About,
        Compose,
        SyncAll,
        FocusSearch,
        OpenSettings,
        OpenImport,
        NextMail,
        PrevMail,
        NextInThread,
        PrevInThread,
        Reply,
        ReplyAll,
        Forward,
        ToggleStar,
        ToggleSeen,
        Archive,
        DeleteMail,
        Escape,
        ShowWindow,
        HideWindow,
        CycleTheme,
        CommandPalette,
        ShowShortcuts,
        Undo
    ]
);

pub fn bind_keys(cx: &mut App) {
    let m = Some("MailApp");
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-n", Compose, None),
        KeyBinding::new("cmd-r", SyncAll, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-w", HideWindow, None),
        KeyBinding::new("cmd-f", FocusSearch, None),
        KeyBinding::new("cmd-shift-d", CycleTheme, None),
        KeyBinding::new("cmd-k", CommandPalette, None),
        // ⌘Z 只在列表 / 阅读区上下文生效，输入框里仍是文本撤销
        KeyBinding::new("cmd-z", Undo, Some("MailList")),
        // 列表 / 阅读区单键快捷键（输入框获得焦点时不生效：它们只绑定在 MailList 上下文）
        KeyBinding::new("/", FocusSearch, Some("MailList")),
        KeyBinding::new("j", NextMail, Some("MailList")),
        KeyBinding::new("down", NextMail, Some("MailList")),
        KeyBinding::new("k", PrevMail, Some("MailList")),
        KeyBinding::new("up", PrevMail, Some("MailList")),
        // 会话内上一封 / 下一封
        KeyBinding::new("[", PrevInThread, Some("MailList")),
        KeyBinding::new("]", NextInThread, Some("MailList")),
        KeyBinding::new("?", ShowShortcuts, Some("MailList")),
        KeyBinding::new("shift-/", ShowShortcuts, Some("MailList")),
        KeyBinding::new("c", Compose, Some("MailList")),
        KeyBinding::new("r", Reply, Some("MailList")),
        KeyBinding::new("a", ReplyAll, Some("MailList")),
        KeyBinding::new("f", Forward, Some("MailList")),
        KeyBinding::new("s", ToggleStar, Some("MailList")),
        KeyBinding::new("u", ToggleSeen, Some("MailList")),
        KeyBinding::new("e", Archive, Some("MailList")),
        KeyBinding::new("#", DeleteMail, Some("MailList")),
        KeyBinding::new("shift-3", DeleteMail, Some("MailList")),
        KeyBinding::new("delete", DeleteMail, Some("MailList")),
        KeyBinding::new("backspace", DeleteMail, Some("MailList")),
        KeyBinding::new("escape", Escape, m),
    ]);
    line_input::bind_keys(cx);
    text_area::bind_keys(cx);
}

pub fn menus() -> Vec<Menu> {
    let name = crate::core::config::APP_NAME;
    vec![
        Menu {
            name: name.into(),
            items: vec![
                MenuItem::action(crate::i18n::tr(&format!("关于{name}")).into_owned(), About),
                MenuItem::separator(),
                MenuItem::action(crate::i18n::tr("账号与设置…").into_owned(), OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu(crate::i18n::tr("服务").into_owned(), SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(crate::i18n::tr(&format!("隐藏{name}")).into_owned(), HideWindow),
                MenuItem::action(crate::i18n::tr(&format!("退出{name}")).into_owned(), Quit),
            ],
        },
        Menu {
            name: crate::i18n::tr("邮件").into_owned().into(),
            items: vec![
                MenuItem::action(crate::i18n::tr("写新邮件").into_owned(), Compose),
                MenuItem::action(crate::i18n::tr("立即同步全部账号").into_owned(), SyncAll),
                MenuItem::action(crate::i18n::tr("导入 .eml / .mbox…").into_owned(), OpenImport),
                MenuItem::separator(),
                MenuItem::action(crate::i18n::tr("回复").into_owned(), Reply),
                MenuItem::action(crate::i18n::tr("全部回复").into_owned(), ReplyAll),
                MenuItem::action(crate::i18n::tr("转发").into_owned(), Forward),
                MenuItem::separator(),
                MenuItem::action(crate::i18n::tr("搜索").into_owned(), FocusSearch),
                MenuItem::action(crate::i18n::tr("命令面板…").into_owned(), CommandPalette),
                MenuItem::action(crate::i18n::tr("快捷键一览").into_owned(), ShowShortcuts),
                MenuItem::action(crate::i18n::tr("撤销移动 / 删除").into_owned(), Undo),
            ],
        },
        Menu {
            name: crate::i18n::tr("编辑").into_owned().into(),
            items: vec![
                MenuItem::os_action(crate::i18n::tr("剪切").into_owned(), line_input::ICut, OsAction::Cut),
                MenuItem::os_action(crate::i18n::tr("复制").into_owned(), line_input::ICopy, OsAction::Copy),
                MenuItem::os_action(crate::i18n::tr("粘贴").into_owned(), line_input::IPaste, OsAction::Paste),
                MenuItem::os_action(crate::i18n::tr("全选").into_owned(), line_input::ISelectAll, OsAction::SelectAll),
            ],
        },
        Menu { name: crate::i18n::tr("视图").into_owned().into(), items: vec![MenuItem::action(crate::i18n::tr("切换配色").into_owned(), CycleTheme)] },
    ]
}

#[cfg(test)]
mod tests {
    use super::{fmt_ago, fmt_num, fmt_short_at, fmt_size, icon_bytes, one_line};
    use chrono::{Local, TimeZone};

    #[test]
    fn formatting() {
        assert_eq!(fmt_size(0), "");
        assert_eq!(fmt_size(512), "512 B");
        assert_eq!(fmt_size(2048), "2 KB");
        assert_eq!(fmt_size(3 * 1_048_576), "3.0 MB");
        assert_eq!(fmt_num(1234567), "1,234,567");
        assert_eq!(fmt_num(12), "12");
        assert_eq!(one_line("a\n  b\tc", 10), "a b c");
        assert_eq!(one_line("一二三四五", 3), "一二三…");
        let now = Local.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap();
        let ts = |y, m, d, h| Local.with_ymd_and_hms(y, m, d, h, 5, 0).unwrap().timestamp_millis();
        assert_eq!(fmt_short_at(ts(2026, 5, 10, 9), now), "09:05");
        assert_eq!(fmt_short_at(ts(2026, 5, 9, 9), now), "昨天");
        assert_eq!(fmt_short_at(ts(2026, 3, 1, 9), now), "03/01");
        assert_eq!(fmt_short_at(ts(2024, 3, 1, 9), now), "2024/03/01");
        assert_eq!(fmt_ago(None), "从未");
        for n in ["inbox", "star-fill", "logo", "trash"] {
            assert!(icon_bytes(n).is_some(), "{n}");
        }
    }
}
