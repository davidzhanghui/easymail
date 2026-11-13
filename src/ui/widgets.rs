//! 通用小部件：按钮、标签、复选框、弹层、模态外壳、表单行。

use super::theme::Theme;
use super::{icon, SANS, SERIF};
use gpui::prelude::FluentBuilder;
use gpui::*;

pub const H_SM: f32 = 28.;
pub const H_MD: f32 = 36.;
pub const H_ROW: f32 = 32.;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// 按钮：图标 + 文字。`sm` 为 28px 高的紧凑款。
pub fn button(id: impl Into<ElementId>, t: &Theme, kind: Kind, ic: Option<&'static str>, label: impl Into<SharedString>, sm: bool) -> Stateful<Div> {
    let label: SharedString = crate::i18n::tr_shared(label.into());
    let (bg, fg, border, hover) = match kind {
        Kind::Primary => (t.accent, t.on_accent, t.accent, t.accent_deep),
        Kind::Secondary => (t.panel, t.ink_2, t.border, t.row_hover),
        Kind::Ghost => (transparent_black(), t.ink_2, transparent_black(), t.row_hover),
        Kind::Danger => (t.panel, t.danger, t.border, t.danger.opacity(0.08)),
    };
    let h = if sm { H_SM } else { H_MD };
    let only_icon = label.is_empty();
    let id: ElementId = id.into();
    let hint = if only_icon { icon_hint(&id.to_string()) } else { None };
    let (tip_bg, tip_fg, tip_border) = (t.toast_bg, t.toast_fg, t.border);
    div()
        .id(id)
        .when_some(hint, |d, h| {
            d.tooltip(move |_, cx| cx.new(|_| Tip { text: crate::i18n::tr(h).into_owned().into(), bg: tip_bg, fg: tip_fg, border: tip_border }).into())
        })
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .h(px(h))
        .when(only_icon, |d| d.w(px(h)))
        .when(!only_icon, |d| d.px(px(if sm { 10. } else { 14. })))
        .rounded(px(if sm { 6. } else { 8. }))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_color(fg)
        .text_size(px(if sm { 12. } else { 13. }))
        .font_family(SANS)
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .when_some(ic, |d, i| d.child(icon(i, if sm { 13. } else { 14. }, fg)))
        .when(!only_icon, |d| d.child(div().whitespace_nowrap().child(label)))
}

pub fn disabled(b: Stateful<Div>, off: bool) -> Stateful<Div> {
    if off {
        b.opacity(0.5).cursor_default()
    } else {
        b
    }
}

/// 筛选标签（全部 / 未读 / 星标 / 附件、服务商、标签页）
pub fn chip(id: impl Into<ElementId>, t: &Theme, label: impl Into<SharedString>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(H_SM))
        .px(px(10.))
        .rounded(px(6.))
        .border_1()
        .text_size(px(12.))
        .cursor_pointer()
        .map(|d| {
            if on {
                d.bg(t.accent_soft).text_color(t.accent_deep).border_color(t.accent.opacity(0.35))
            } else {
                d.bg(t.panel).text_color(t.ink_2).border_color(t.border).hover(|s| s.bg(t.row_hover))
            }
        })
        .child(crate::i18n::tr_shared(label.into()))
}

/// 计数徽标
pub fn badge(t: &Theme, text: impl Into<SharedString>, strong: bool) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .h(px(18.))
        .flex()
        .items_center()
        .rounded(px(9.))
        .text_size(px(10.5))
        .map(|d| if strong { d.bg(t.accent_soft).text_color(t.accent_deep) } else { d.bg(t.panel_2).text_color(t.muted) })
        .child(crate::i18n::tr_shared(text.into()))
}

pub fn checkbox(id: impl Into<ElementId>, t: &Theme, checked: bool) -> Stateful<Div> {
    div().id(id).size(px(14.)).flex_none().flex().items_center().justify_center().rounded(px(3.5)).border_1().cursor_pointer().map(|d| {
        if checked {
            d.bg(t.accent).border_color(t.accent).child(icon("check", 11., t.on_accent))
        } else {
            d.bg(t.panel).border_color(t.line_2)
        }
    })
}

/// 勾选行：复选框 + 文字
pub fn check_row(id: impl Into<ElementId>, t: &Theme, checked: bool, label: impl Into<SharedString>) -> Stateful<Div> {
    let id: ElementId = id.into();
    div()
        .id(id.clone())
        .flex()
        .items_center()
        .gap(px(8.))
        .cursor_pointer()
        .text_size(px(12.5))
        .text_color(t.ink_2)
        .child(checkbox(ElementId::Name(format!("{id}-box").into()), t, checked))
        .child(div().flex_1().child(crate::i18n::tr_shared(label.into())))
}

/// 弹层容器（菜单 / 主题切换）。调用方负责定位（absolute + top/right）。
pub fn popover(t: &Theme, w: f32) -> Div {
    div()
        .absolute()
        .w(px(w))
        .p(px(6.))
        .bg(t.panel)
        .border_1()
        .border_color(t.border)
        .rounded(px(10.))
        .shadow(vec![BoxShadow { color: t.shadow, offset: point(px(0.), px(8.)), blur_radius: px(28.), spread_radius: px(0.) }])
        .flex()
        .flex_col()
        .text_color(t.ink)
        .font_family(SANS)
}

/// 菜单项
pub fn menu_row(id: impl Into<ElementId>, t: &Theme, ic: Option<&'static str>, label: impl Into<SharedString>, active: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(9.))
        .h(px(30.))
        .px(px(9.))
        .rounded(px(6.))
        .text_size(px(12.5))
        .cursor_pointer()
        .text_color(if active { t.accent_deep } else { t.ink })
        .when(active, |d| d.bg(t.accent_soft))
        .hover(|s| s.bg(t.row_hover))
        .when_some(ic, |d, i| d.child(icon(i, 13., t.muted)))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(crate::i18n::tr_shared(label.into())))
}

pub fn menu_title(t: &Theme, s: impl Into<SharedString>) -> Div {
    div().px(px(9.)).pt(px(7.)).pb(px(4.)).text_size(px(10.5)).text_color(t.faint).child(crate::i18n::tr_shared(s.into()))
}

pub fn menu_sep(t: &Theme) -> Div {
    div().h(px(1.)).my(px(4.)).bg(t.border_soft)
}

/// 模态外壳：标题、副标题、内容、底栏。点遮罩关闭由调用方决定。
pub fn modal_shell(t: &Theme, w: f32, title: impl Into<SharedString>, sub: impl Into<SharedString>, close: Stateful<Div>) -> Div {
    shell_frame(
        t,
        title,
        sub,
        close,
        div().w(px(w)).max_w(relative(0.94)).max_h(relative(0.9)).border_1().border_color(t.border).rounded(px(14.)).shadow(vec![BoxShadow {
            color: t.shadow,
            offset: point(px(0.), px(18.)),
            blur_radius: px(60.),
            spread_radius: px(0.),
        }]),
    )
}

/// 独立窗口里的「模态」外壳：和 `modal_shell` 一样的标题栏，但铺满整个窗口，没有圆角 / 边框 / 阴影。
pub fn window_shell(t: &Theme, title: impl Into<SharedString>, sub: impl Into<SharedString>, close: Stateful<Div>) -> Div {
    shell_frame(t, title, sub, close, div().size_full())
}

fn shell_frame(t: &Theme, title: impl Into<SharedString>, sub: impl Into<SharedString>, close: Stateful<Div>, frame: Div) -> Div {
    let sub: SharedString = crate::i18n::tr_shared(sub.into());
    frame.bg(t.panel).flex().flex_col().overflow_hidden().text_color(t.ink).font_family(SANS).child(
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .px(px(22.))
            .pt(px(18.))
            .pb(px(14.))
            .border_b_1()
            .border_color(t.border_soft)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_family(SERIF).text_size(px(18.)).font_weight(FontWeight::BOLD).child(crate::i18n::tr_shared(title.into())))
                    .when(!sub.is_empty(), |d| d.child(div().text_size(px(11.5)).text_color(t.muted).mt(px(2.)).child(sub))),
            )
            .child(close),
    )
}

pub fn modal_footer(t: &Theme) -> Div {
    div().flex().flex_none().items_center().gap(px(8.)).px(px(22.)).py(px(12.)).border_t_1().border_color(t.border_soft).bg(t.panel_2)
}

/// 输入框外框（包住 LineInput）
pub fn field_box(t: &Theme, focused: bool) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(H_MD))
        .px(px(10.))
        .rounded(px(8.))
        .border_1()
        .border_color(if focused { t.accent } else { t.border })
        .bg(t.paper)
        .text_size(px(13.))
        .text_color(t.ink)
}

/// 表单行：左侧标签 + 右侧控件
pub fn form_row(t: &Theme, label: impl Into<SharedString>, top: bool) -> Div {
    div()
        .flex()
        .gap(px(12.))
        .when(!top, |d| d.items_center())
        .mb(px(10.))
        .child(div().w(px(84.)).flex_none().text_size(px(12.)).text_color(t.muted).when(top, |d| d.pt(px(9.))).child(crate::i18n::tr_shared(label.into())))
}

pub fn section_title(t: &Theme, s: impl Into<SharedString>) -> Div {
    div().text_size(px(11.)).text_color(t.muted).mb(px(10.)).mt(px(4.)).child(crate::i18n::tr_shared(s.into()))
}

pub fn dot(color: Hsla, size: f32) -> Div {
    div().size(px(size)).flex_none().rounded_full().bg(color)
}

struct Tip {
    text: SharedString,
    bg: Hsla,
    fg: Hsla,
    border: Hsla,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .bg(self.bg)
            .border_1()
            .border_color(self.border)
            .text_color(self.fg)
            .text_size(px(12.))
            .font_family(SANS)
            .child(self.text.clone())
    }
}

/// 给可交互元素加悬停提示
pub fn tip(b: Stateful<Div>, t: &Theme, text: impl Into<SharedString>) -> Stateful<Div> {
    let text: SharedString = crate::i18n::tr_shared(text.into());
    let (bg, fg, border) = (t.toast_bg, t.toast_fg, t.border);
    b.tooltip(move |_, cx| cx.new(|_| Tip { text: text.clone(), bg, fg, border }).into())
}

/// 纯图标按钮没有文字，按 id 给出悬停提示
fn icon_hint(id: &str) -> Option<&'static str> {
    Some(match id {
        "toggle-reader" => "折叠阅读区，列表占满",
        "expand-reader" => "展开阅读区",
        "clear-q" => "清除搜索",
        "mark-all" => "全部标为已读",
        "list-refresh" => "刷新列表",
        "list-export" => "导出当前列表",
        "t-star" => "星标 / 取消星标",
        "t-seen" => "标为已读 / 未读",
        "t-move" => "移动到文件夹",
        "t-archive" => "归档",
        "t-spam" => "标为垃圾邮件 / 移回收件箱",
        "t-eml" => "下载 .eml 原文",
        "prompt-x" => "关闭",
        _ if id.starts_with("rm-img-") => "移除图片",
        _ if id.starts_with("acct-sync-") => "同步此账号",
        _ if id.starts_with("acct-more-") => "更多操作",
        _ => return None,
    })
}
