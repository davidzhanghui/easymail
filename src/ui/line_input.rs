//! 单行输入框（支持输入法），用于快速打开、查找、链接编辑。
//! 基于 GPUI 官方 input 示例改写，增加了事件与外观。

use super::theme::theme;
use gpui::*;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

actions!(
    line_input,
    [
        IBackspace,
        IDelete,
        ILeft,
        IRight,
        ISelectLeft,
        ISelectRight,
        ISelectAll,
        IHome,
        IEnd,
        IPaste,
        ICut,
        ICopy,
        ISubmit,
        IShiftSubmit,
        ICancel,
        IUp,
        IDown,
        IWordBack,
        ITab,
        IShiftTab
    ]
);

pub enum InputEvent {
    Changed,
    Submit { shift: bool },
    Cancel,
    Up,
    Down,
    Tab { back: bool },
}

impl EventEmitter<InputEvent> for LineInput {}

pub struct LineInput {
    focus: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    /// 横向滚动量：文字比框宽时左移，保证光标始终可见
    scroll: Pixels,
    selecting: bool,
    /// 密码框：显示圆点
    masked: bool,
}

impl LineInput {
    pub fn new(placeholder: &str, cx: &mut Context<Self>) -> Self {
        LineInput {
            focus: cx.focus_handle(),
            content: "".into(),
            placeholder: placeholder.to_string().into(),
            selected: 0..0,
            reversed: false,
            marked: None,
            last_layout: None,
            last_bounds: None,
            scroll: px(0.),
            selecting: false,
            masked: false,
        }
    }

    pub fn set_masked(&mut self, m: bool, cx: &mut Context<Self>) {
        self.masked = m;
        cx.notify();
    }

    pub fn set_placeholder(&mut self, p: &str, cx: &mut Context<Self>) {
        self.placeholder = p.to_string().into();
        cx.notify();
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// 内容字节偏移 → 显示文本字节偏移（密码框里每个字符显示为 3 字节的「•」）
    fn disp(&self, ix: usize) -> usize {
        if self.masked {
            self.content[..ix.min(self.content.len())].chars().count() * '•'.len_utf8()
        } else {
            ix
        }
    }

    fn undisp(&self, d: usize) -> usize {
        if self.masked {
            let n = d / '•'.len_utf8();
            self.content.char_indices().nth(n).map(|(i, _)| i).unwrap_or(self.content.len())
        } else {
            d
        }
    }

    pub fn text(&self) -> String {
        self.content.to_string()
    }

    pub fn set_text(&mut self, s: &str, cx: &mut Context<Self>) {
        self.content = s.to_string().into();
        self.selected = 0..s.len();
        self.reversed = false;
        cx.notify();
    }

    /// 光标移到末尾、不选中（非聚焦的输入框初始化时用，避免整段高亮）。
    pub fn collapse_to_end(&mut self, cx: &mut Context<Self>) {
        let n = self.content.len();
        self.selected = n..n;
        cx.notify();
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.selected.start
        } else {
            self.selected.end
        }
    }

    fn move_to(&mut self, o: usize, cx: &mut Context<Self>) {
        self.selected = o..o;
        cx.notify();
    }

    fn select_to(&mut self, o: usize, cx: &mut Context<Self>) {
        if self.reversed {
            self.selected.start = o
        } else {
            self.selected.end = o
        }
        if self.selected.end < self.selected.start {
            self.reversed = !self.reversed;
            self.selected = self.selected.end..self.selected.start;
        }
        cx.notify();
    }

    fn prev_boundary(&self, o: usize) -> usize {
        self.content.grapheme_indices(true).rev().find_map(|(i, _)| (i < o).then_some(i)).unwrap_or(0)
    }

    fn next_boundary(&self, o: usize) -> usize {
        self.content.grapheme_indices(true).find_map(|(i, _)| (i > o).then_some(i)).unwrap_or(self.content.len())
    }

    fn left(&mut self, _: &ILeft, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(self.prev_boundary(self.cursor()), cx)
        } else {
            self.move_to(self.selected.start, cx)
        }
    }
    fn right(&mut self, _: &IRight, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(self.next_boundary(self.selected.end), cx)
        } else {
            self.move_to(self.selected.end, cx)
        }
    }
    fn select_left(&mut self, _: &ISelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.prev_boundary(self.cursor()), cx)
    }
    fn select_right(&mut self, _: &ISelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor()), cx)
    }
    fn select_all(&mut self, _: &ISelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = 0..self.content.len();
        self.reversed = false;
        cx.notify()
    }
    fn home(&mut self, _: &IHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx)
    }
    fn end(&mut self, _: &IEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx)
    }
    fn backspace(&mut self, _: &IBackspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.select_to(self.prev_boundary(self.cursor()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }
    fn word_back(&mut self, _: &IWordBack, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            let c = self.cursor();
            let start =
                self.content.split_word_bound_indices().rev().find_map(|(i, w)| (i < c && w.chars().any(|ch| ch.is_alphanumeric())).then_some(i)).unwrap_or(0);
            self.select_to(start, cx);
        }
        self.replace_text_in_range(None, "", window, cx)
    }
    fn delete(&mut self, _: &IDelete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.select_to(self.next_boundary(self.cursor()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }
    fn paste(&mut self, _: &IPaste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = cx.read_from_clipboard().and_then(|i| i.text()) {
            self.replace_text_in_range(None, &t.replace('\n', " "), window, cx);
        }
    }
    fn copy(&mut self, _: &ICopy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.clamp_range(self.selected.clone())].to_string()));
        }
    }
    fn cut(&mut self, _: &ICut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.clamp_range(self.selected.clone())].to_string()));
            self.replace_text_in_range(None, "", window, cx)
        }
    }
    fn submit(&mut self, _: &ISubmit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Submit { shift: false })
    }
    fn shift_submit(&mut self, _: &IShiftSubmit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Submit { shift: true })
    }
    fn cancel(&mut self, _: &ICancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Cancel)
    }
    fn tab(&mut self, _: &ITab, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Tab { back: false })
    }
    fn shift_tab(&mut self, _: &IShiftTab, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Tab { back: true })
    }
    fn up(&mut self, _: &IUp, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Up)
    }
    fn down(&mut self, _: &IDown, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Down)
    }

    fn index_for_mouse(&self, p: Point<Pixels>) -> usize {
        let (Some(b), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref()) else {
            return 0;
        };
        if p.y < b.top() {
            return 0;
        }
        // 空内容时排版的是占位文字，按它算下标会把光标放到占位文字中间
        if self.content.is_empty() || p.y > b.bottom() {
            return self.content.len();
        }
        self.undisp(line.closest_index_for_x(p.x - b.left() + self.scroll)).min(self.content.len())
    }

    fn byte_to_utf16(&self, o: usize) -> usize {
        self.content[..o.min(self.content.len())].encode_utf16().count()
    }
    fn utf16_to_byte(&self, o: usize) -> usize {
        let mut u = 0;
        for (i, ch) in self.content.char_indices() {
            if u >= o {
                return i;
            }
            u += ch.len_utf16();
        }
        self.content.len()
    }
    fn range_byte_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.byte_to_utf16(r.start)..self.byte_to_utf16(r.end)
    }
    fn range_utf16_to_byte(&self, r: &Range<usize>) -> Range<usize> {
        self.utf16_to_byte(r.start)..self.utf16_to_byte(r.end)
    }

    /// 把字节区间夹进文本范围并对齐到 char 边界（见 [`clamp_str_range`]）。
    fn clamp_range(&self, r: Range<usize>) -> Range<usize> {
        clamp_str_range(&self.content, r)
    }
}

/// 把字节区间夹进 `0..=len` 并对齐到 char 边界。
///
/// 输入法（尤其第三方 IME）回传的 range 是以「它上次看到的文本」为基准的，
/// 中间只要我们自己改过 content（提交、粘贴、清空），range 就可能越界，
/// 甚至在多字节字符中间 —— 直接拿去切片 `&content[a..b]` 会 panic 让整个应用退出。
pub(crate) fn clamp_str_range(s: &str, r: Range<usize>) -> Range<usize> {
    let len = s.len();
    let mut a = r.start.min(len);
    while a > 0 && !s.is_char_boundary(a) {
        a -= 1;
    }
    let mut b = r.end.min(len).max(a);
    while b < len && !s.is_char_boundary(b) {
        b += 1;
    }
    a..b
}

impl EntityInputHandler for LineInput {
    fn text_for_range(&mut self, r: Range<usize>, actual: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let range = self.clamp_range(self.range_utf16_to_byte(&r));
        actual.replace(self.range_byte_to_utf16(&range));
        Some(self.content[range].to_string())
    }
    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_byte_to_utf16(&self.selected), reversed: self.reversed })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|r| self.range_byte_to_utf16(r))
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }
    fn replace_text_in_range(&mut self, r: Option<Range<usize>>, new: &str, _: &mut Window, cx: &mut Context<Self>) {
        let range = self.clamp_range(r.as_ref().map(|r| self.range_utf16_to_byte(r)).or(self.marked.clone()).unwrap_or(self.selected.clone()));
        self.content = (self.content[0..range.start].to_owned() + new + &self.content[range.end..]).into();
        self.selected = range.start + new.len()..range.start + new.len();
        self.marked = None;
        self.changed(cx);
    }
    fn replace_and_mark_text_in_range(&mut self, r: Option<Range<usize>>, new: &str, sel: Option<Range<usize>>, _: &mut Window, cx: &mut Context<Self>) {
        let range = self.clamp_range(r.as_ref().map(|r| self.range_utf16_to_byte(r)).or(self.marked.clone()).unwrap_or(self.selected.clone()));
        self.content = (self.content[0..range.start].to_owned() + new + &self.content[range.end..]).into();
        self.marked = (!new.is_empty()).then(|| range.start..range.start + new.len());
        self.selected = sel
            .map(|s| {
                let base = range.start;
                let sub: String = new.to_string();
                let to8 = |u: usize| {
                    let mut c = 0;
                    for (i, ch) in sub.char_indices() {
                        if c >= u {
                            return i;
                        }
                        c += ch.len_utf16();
                    }
                    sub.len()
                };
                base + to8(s.start)..base + to8(s.end)
            })
            .unwrap_or(range.start + new.len()..range.start + new.len());
        self.changed(cx);
    }
    fn bounds_for_range(&mut self, r: Range<usize>, bounds: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        // 还没排版过（刚打开就开始输入）：至少给出输入框本身的位置，别让候选框落到屏幕角落
        let Some(line) = self.last_layout.as_ref() else { return Some(bounds) };
        let range = self.range_utf16_to_byte(&r);
        Some(Bounds::from_corners(
            point(bounds.left() - self.scroll + line.x_for_index(range.start), bounds.top()),
            point(bounds.left() - self.scroll + line.x_for_index(range.end), bounds.bottom()),
        ))
    }
    fn character_index_for_point(&mut self, p: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let local = self.last_bounds?.localize(&p)?;
        let line = self.last_layout.as_ref()?;
        let ix = line.index_for_x(p.x - local.x + self.scroll)?;
        Some(self.byte_to_utf16(ix))
    }
}

struct TextEl {
    input: Entity<LineInput>,
}

struct Prepaint {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    scroll: Pixels,
}

impl IntoElement for TextEl {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for TextEl {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let t = theme(cx).clone();
        let input = self.input.read(cx);
        let content = input.content.clone();
        let sel = input.selected.clone();
        let cursor = input.cursor();
        let scroll0 = input.scroll;
        let style = window.text_style();
        let (text, color) = if content.is_empty() {
            (input.placeholder.clone(), t.placeholder)
        } else if input.masked {
            (SharedString::from("•".repeat(content.chars().count())), t.text)
        } else {
            (content, t.text)
        };
        let run = TextRun { len: text.len(), font: style.font(), color, background_color: None, underline: None, strikethrough: None };
        let runs = if let Some(m) = input.marked.as_ref().filter(|_| !input.masked) {
            let m = input.clamp_range(m.clone());
            vec![
                TextRun { len: m.start, ..run.clone() },
                TextRun { len: m.end - m.start, underline: Some(UnderlineStyle { color: Some(run.color), thickness: px(1.), wavy: false }), ..run.clone() },
                TextRun { len: text.len() - m.end, ..run },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect()
        } else {
            vec![run]
        };
        let size = style.font_size.to_pixels(window.rem_size());
        let line = window.text_system().shape_line(text, size, &runs, None);
        let cx_ = line.x_for_index(input.disp(cursor));
        // 光标跑出可视区时滚动；内容变短时回收多余的滚动量。占位文字不滚动（超出部分被裁掉）
        let w = bounds.size.width - px(2.);
        let scroll = if input.content.is_empty() {
            px(0.)
        } else {
            let mut s = scroll0;
            if cx_ - s > w {
                s = cx_ - w;
            }
            if cx_ < s {
                s = cx_;
            }
            s.min((line.width - w).max(px(0.))).max(px(0.))
        };
        let left = bounds.left() - scroll;
        let (selection, cursor) = if sel.is_empty() {
            (None, Some(fill(Bounds::new(point(left + cx_, bounds.top() + px(2.)), size_(px(1.5), bounds.size.height - px(4.))), t.accent)))
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(left + line.x_for_index(input.disp(sel.start)), bounds.top()),
                        point(left + line.x_for_index(input.disp(sel.end)), bounds.bottom()),
                    ),
                    t.selection,
                )),
                None,
            )
        };
        Prepaint { line: Some(line), cursor, selection, scroll }
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        p: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let line = p.line.take().unwrap();
        let scroll = p.scroll;
        // 裁剪到输入框内：长文本 / 长占位文字不再画出框外（光标宽 1.5px，右侧多留一点）
        let mask = ContentMask { bounds: Bounds::new(bounds.origin, size(bounds.size.width + px(2.), bounds.size.height)) };
        window.with_content_mask(Some(mask), |window| {
            if let Some(s) = p.selection.take() {
                window.paint_quad(s)
            }
            line.paint(point(bounds.left() - scroll, bounds.top()), window.line_height(), window, cx).ok();
            if focus.is_focused(window) {
                if let Some(c) = p.cursor.take() {
                    window.paint_quad(c);
                }
            }
        });
        self.input.update(cx, |i, _| {
            i.last_layout = Some(line);
            i.last_bounds = Some(bounds);
            i.scroll = scroll;
        });
    }
}

fn size_(w: Pixels, h: Pixels) -> Size<Pixels> {
    size(w, h)
}

impl Render for LineInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_1()
            .key_context("LineInput")
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::word_back))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::shift_submit))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::tab))
            .on_action(cx.listener(Self::shift_tab))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, w, cx| {
                    this.selecting = true;
                    w.focus(&this.focus);
                    let ix = this.index_for_mouse(e.position);
                    if e.modifiers.shift {
                        this.select_to(ix, cx)
                    } else {
                        this.move_to(ix, cx)
                    }
                }),
            )
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, _| this.selecting = false))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if this.selecting {
                    let ix = this.index_for_mouse(e.position);
                    this.select_to(ix, cx);
                }
            }))
            .child(TextEl { input: cx.entity() })
    }
}

impl Focusable for LineInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

pub fn bind_keys(cx: &mut App) {
    let c = Some("LineInput");
    cx.bind_keys([
        KeyBinding::new("backspace", IBackspace, c),
        KeyBinding::new("alt-backspace", IWordBack, c),
        KeyBinding::new("delete", IDelete, c),
        KeyBinding::new("left", ILeft, c),
        KeyBinding::new("right", IRight, c),
        KeyBinding::new("shift-left", ISelectLeft, c),
        KeyBinding::new("shift-right", ISelectRight, c),
        KeyBinding::new("cmd-a", ISelectAll, c),
        KeyBinding::new("cmd-v", IPaste, c),
        KeyBinding::new("cmd-c", ICopy, c),
        KeyBinding::new("cmd-x", ICut, c),
        KeyBinding::new("home", IHome, c),
        KeyBinding::new("cmd-left", IHome, c),
        KeyBinding::new("end", IEnd, c),
        KeyBinding::new("cmd-right", IEnd, c),
        KeyBinding::new("enter", ISubmit, c),
        KeyBinding::new("shift-enter", IShiftSubmit, c),
        KeyBinding::new("escape", ICancel, c),
        KeyBinding::new("tab", ITab, c),
        KeyBinding::new("shift-tab", IShiftTab, c),
        KeyBinding::new("up", IUp, c),
        KeyBinding::new("down", IDown, c),
    ]);
}

#[cfg(test)]
mod tests {
    // 别用 `super::*`：会把 gpui 的 `test` 宏引进作用域，遮蔽内置的 `#[test]`
    use super::clamp_str_range;

    #[test]
    fn clamp_range_never_panics_and_stays_on_boundaries() {
        // 「你好」= 6 字节，每个汉字 3 字节
        let s = "你好";
        // 越界
        assert_eq!(clamp_str_range(s, 0..999), 0..6);
        assert_eq!(clamp_str_range(s, 99..999), 6..6);
        // 落在多字节字符中间 → 回退 / 前进到边界
        assert_eq!(clamp_str_range(s, 1..2), 0..3);
        assert_eq!(clamp_str_range(s, 4..5), 3..6);
        // 反转区间（start > end）会被压成空区间（故意构造反向区间，测的就是这种输入）
        #[allow(clippy::reversed_empty_ranges)]
        let r = clamp_str_range(s, 5..2);
        assert!(r.start <= r.end);
        // 夹完的区间一定可以安全切片
        let _ = &s[r];
    }

    #[test]
    fn clamp_range_handles_empty_content() {
        assert_eq!(clamp_str_range("", 0..0), 0..0);
        assert_eq!(clamp_str_range("", 3..7), 0..0);
    }
}
