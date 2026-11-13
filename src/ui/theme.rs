//! 主题：11 套配色 + 「跟随系统」（浅色 / 深色各选一套）。界面代码只引用语义色。

use super::palettes::{Palette, PALETTES};
use gpui::{rgb, App, Global, Hsla, WindowAppearance};

#[derive(Clone)]
pub struct Theme {
    pub id: &'static str,
    pub name: &'static str,
    pub dark: bool,
    pub bg: Hsla,
    pub panel: Hsla,
    pub panel_2: Hsla,
    pub border: Hsla,
    pub border_soft: Hsla,
    pub ink: Hsla,
    pub ink_2: Hsla,
    pub body_ink: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub accent: Hsla,
    pub accent_deep: Hsla,
    pub accent_soft: Hsla,
    pub on_accent: Hsla,
    pub gold: Hsla,
    pub danger: Hsla,
    pub live: Hsla,
    pub toast_bg: Hsla,
    pub toast_fg: Hsla,
    pub row_hover: Hsla,
    pub paper: Hsla,
    pub line_2: Hsla,
    pub shadow: Hsla,
    pub overlay: Hsla,
    // 输入控件共用
    pub text: Hsla,
    pub placeholder: Hsla,
    pub selection: Hsla,
    /// 原始色值（WebView 纯文本正文要用 CSS 颜色）
    pub hex_ink: u32,
    pub hex_paper: u32,
    pub hex_muted: u32,
    pub hex_accent: u32,
}

impl Global for Theme {}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

impl Theme {
    pub fn from_palette(p: &Palette) -> Theme {
        let accent = c(p.accent);
        Theme {
            id: p.id,
            name: p.name,
            dark: p.dark,
            bg: c(p.bg),
            panel: c(p.panel),
            panel_2: c(p.panel_2),
            border: c(p.border),
            border_soft: c(p.border_soft),
            ink: c(p.ink),
            ink_2: c(p.ink_2),
            body_ink: c(p.body_ink),
            muted: c(p.muted),
            faint: c(p.faint),
            accent,
            accent_deep: c(p.accent_deep),
            accent_soft: c(p.accent_soft),
            on_accent: c(p.on_accent),
            gold: c(p.gold),
            danger: c(p.danger),
            live: c(p.live),
            toast_bg: c(p.toast_bg),
            toast_fg: c(p.toast_fg),
            row_hover: c(p.row_hover),
            paper: c(p.paper),
            line_2: c(p.line_2),
            shadow: c(p.shadow_rgb).opacity(if p.dark { 0.45 } else { 0.16 }),
            overlay: c(p.overlay_rgb).opacity(if p.dark { 0.55 } else { 0.34 }),
            text: c(p.ink),
            placeholder: c(p.faint),
            selection: accent.opacity(0.22),
            hex_ink: p.body_ink,
            hex_paper: p.paper,
            hex_muted: p.muted,
            hex_accent: p.accent,
        }
    }

    pub fn by_id(id: &str) -> Theme {
        Theme::from_palette(palette(id))
    }

    /// 账号颜色名（accent / gold / danger / live / muted / ink-2）→ 颜色
    pub fn named(&self, name: &str) -> Hsla {
        match name {
            "gold" => self.gold,
            "danger" => self.danger,
            "live" => self.live,
            "muted" => self.muted,
            "ink-2" => self.ink_2,
            _ => self.accent,
        }
    }
}

pub fn palette(id: &str) -> &'static Palette {
    PALETTES.iter().find(|p| p.id == id).unwrap_or(&PALETTES[0])
}

pub fn theme(cx: &App) -> &Theme {
    cx.global::<Theme>()
}

/// 用户偏好：具体主题 id，或 "auto"（跟随系统，浅 / 深各一套）
#[derive(Clone, Debug, PartialEq)]
pub struct ThemePref {
    pub pref: String,
    pub light: String,
    pub dark: String,
}

impl Default for ThemePref {
    fn default() -> Self {
        ThemePref { pref: "moss".into(), light: "moss".into(), dark: "midnight".into() }
    }
}

impl ThemePref {
    pub fn resolve(&self, appearance: WindowAppearance) -> &str {
        if self.pref == "auto" {
            match appearance {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => &self.dark,
                _ => &self.light,
            }
        } else {
            &self.pref
        }
    }
}

pub fn hex_css(v: u32) -> String {
    format!("#{v:06x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palettes_complete_and_resolve() {
        assert_eq!(PALETTES.len(), 11);
        assert_eq!(PALETTES.iter().filter(|p| !p.dark).count(), 8);
        assert_eq!(PALETTES.iter().filter(|p| p.dark).count(), 3);
        assert_eq!(palette("nope").id, "light");
        assert_eq!(palette("light").name, "Light");
        let p = ThemePref { pref: "auto".into(), light: "teal".into(), dark: "dusk".into() };
        assert_eq!(p.resolve(WindowAppearance::Dark), "dusk");
        assert_eq!(p.resolve(WindowAppearance::Light), "teal");
        assert_eq!(ThemePref::default().resolve(WindowAppearance::Dark), "moss");
        assert_eq!(hex_css(0x3d5c47), "#3d5c47");
    }
}
