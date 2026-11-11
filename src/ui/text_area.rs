//! 多行纯文本输入（写信正文、签名），支持输入法、自动折行、纵向滚动。
//!
//! 改自 stone-brook 的 text_area（同样基于 GPUI 官方 input 示例），去掉了富文本分段、代码模式与对齐。

use super::line_input::clamp_str_range;
use super::theme::theme;
use gpui::*;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

actions!(
    text_area,
    [
        TBackspace,
        TDelete,
        TWordBack,
        TLeft,
        TRight,
        TUp,
        TDown,
        TSelectLeft,
        TSelectRight,
        TSelectUp,
        TSelectDown,
        THome,
        TEnd,
        TDocStart,
        TDocEnd,
        TSelectAll,
        TNewline,
        TCommit,
        TCancel,
        TTab,
        TShiftTab,
        TCopy,
        TCut,
        TPaste
    ]
);

pub fn bind_keys(cx: &mut App) {
    let c = Some("TextArea");
    cx.bind_keys([
        KeyBinding::new("backspace", TBackspace, c),
        KeyBinding::new("delete", TDelete, c),
        KeyBinding::new("alt-backspace", TWordBack, c),
        KeyBinding::new("left", TLeft, c),
        KeyBinding::new("right", TRight, c),
        KeyBinding::new("up", TUp, c),
        KeyBinding::new("down", TDown, c),
        KeyBinding::new("shift-left", TSelectLeft, c),
        KeyBinding::new("shift-right", TSelectRight, c),
        KeyBinding::new("shift-up", TSelectUp, c),
        KeyBinding::new("shift-down", TSelectDown, c),
        KeyBinding::new("cmd-left", THome, c),
        KeyBinding::new("cmd-right", TEnd, c),
        KeyBinding::new("home", THome, c),
        KeyBinding::new("end", TEnd, c),
        KeyBinding::new("cmd-up", TDocStart, c),
        KeyBinding::new("cmd-down", TDocEnd, c),
        KeyBinding::new("cmd-a", TSelectAll, c),
        KeyBinding::new("enter", TNewline, c),
        KeyBinding::new("shift-enter", TNewline, c),
        KeyBinding::new("cmd-enter", TCommit, c),
        KeyBinding::new("escape", TCancel, c),
        KeyBinding::new("tab", TTab, c),
        KeyBinding::new("shift-tab", TShiftTab, c),
        KeyBinding::new("cmd-c", TCopy, c),
        KeyBinding::new("cmd-x", TCut, c),
        KeyBinding::new("cmd-v", TPaste, c),
    ]);
}

pub enum TextAreaEvent {
    Changed,
    /// ⌘↵
    Commit,
    /// Esc
    Cancel,
    Tab,
    BackTab,
    /// 粘贴了文件（访达里复制的）
    PasteFiles(Vec<std::path::PathBuf>),
    /// 粘贴了图片（截图、从网页 / 图片软件复制）
    PasteImage(Image),
    /// 光标在首行按 ↑ / 末行按 ↓：交给外层去上一块 / 下一块
    Edge {
        up: bool,
    },
    /// 光标在最开头按退格（外层可借此删掉前面的图片块）
    BackspaceAtStart,
    /// 光标在最末尾按 Delete
    DeleteAtEnd,
}

impl EventEmitter<TextAreaEvent> for TextArea {}

#[derive(Clone, Default)]
struct Layout {
    paras: Vec<(usize, WrappedLine, Point<Pixels>)>,
    line_h: Pixels,
}

impl Layout {
    fn point_for(&self, ix: usize) -> Option<Point<Pixels>> {
        for (start, line, origin) in &self.paras {
            if ix >= *start && ix <= start + line.len() {
                let p = line.position_for_index(ix - start, self.line_h)?;
                return Some(point(origin.x + p.x, origin.y + p.y));
            }
        }
        None
    }

    fn rows(line: &WrappedLine) -> Vec<(usize, usize)> {
        let mut starts = vec![0];
        for b in line.wrap_boundaries() {
            if let Some(g) = line.runs().get(b.run_ix).and_then(|r| r.glyphs.get(b.glyph_ix)) {
                starts.push(g.index);
            }
        }
        starts.iter().enumerate().map(|(i, s)| (*s, starts.get(i + 1).copied().unwrap_or(line.len()))).collect()
    }

    fn index_for(&self, p: Point<Pixels>) -> usize {
        let Some(first) = self.paras.first() else { return 0 };
        let mut target = first;
        for para in &self.paras {
            if p.y >= para.2.y {
                target = para;
            }
        }
        let (start, line, origin) = target;
        let local_y = (p.y - origin.y).max(px(0.));
        let rows = Self::rows(line);
        let row = ((local_y / self.line_h) as usize).min(rows.len().saturating_sub(1));
        let at = point((p.x - origin.x).max(px(0.)), self.line_h * row as f32 + self.line_h / 2.);
        let local = match line.closest_index_for_position(at, self.line_h) {
            Ok(i) | Err(i) => i,
        };
        start + local.min(line.len())
    }
}

pub struct TextArea {
    focus: FocusHandle,
    content: String,
    placeholder: SharedString,
    selected: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    layout: Layout,
    selecting: bool,
    goal_x: Option<Pixels>,
    scroll_y: Pixels,
    scroll_max: Pixels,
    revealed: Range<usize>,
    pub font_size: f32,
    pub line_height: f32,
    /// 高度跟随内容（嵌在外层滚动容器里用），不再自己滚动
    pub auto_height: bool,
    content_h: Pixels,
}

impl TextArea {
    pub fn new(text: &str, placeholder: &str, cx: &mut Context<Self>) -> Self {
        TextArea {
            focus: cx.focus_handle(),
            content: text.to_string(),
            placeholder: placeholder.to_string().into(),
            selected: 0..0,
            reversed: false,
            marked: None,
            layout: Layout::default(),
            selecting: false,
            goal_x: None,
            scroll_y: px(0.),
            scroll_max: px(0.),
            revealed: 1..1,
            font_size: 14.,
            line_height: 23.,
            auto_height: false,
            content_h: px(0.),
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// 整体替换内容（光标放到开头）
    pub fn set_text(&mut self, s: &str, cx: &mut Context<Self>) {
        self.content = s.to_string();
        self.selected = 0..0;
        self.marked = None;
        self.scroll_y = px(0.);
        cx.notify();
    }

    pub fn set_placeholder(&mut self, p: &str, cx: &mut Context<Self>) {
        if self.placeholder.as_ref() != p {
            self.placeholder = p.to_string().into();
            cx.notify();
        }
    }

    /// 光标字节偏移
    pub fn cursor_pos(&self) -> usize {
        clamp_str_range(&self.content, self.cursor()..self.cursor()).start
    }

    pub fn set_cursor(&mut self, ix: usize, cx: &mut Context<Self>) {
        let ix = clamp_str_range(&self.content, ix..ix).start;
        self.move_to(ix, cx);
    }

    pub fn move_to_start(&mut self) {
        self.selected = 0..0;
        self.revealed = 1..1;
    }

    pub fn insert(&mut self, s: &str, cx: &mut Context<Self>) {
        let r = clamp_str_range(&self.content, self.selected.clone());
        self.content.replace_range(r.clone(), s);
        self.selected = r.start + s.len()..r.start + s.len();
        self.marked = None;
        self.changed(cx);
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.selected.start
        } else {
            self.selected.end
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.goal_x = None;
        cx.emit(TextAreaEvent::Changed);
        cx.notify();
    }

    fn move_to(&mut self, o: usize, cx: &mut Context<Self>) {
        self.selected = o..o;
        self.reversed = false;
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

    fn vertical(&mut self, dir: f32) -> usize {
        let cur = self.cursor();
        let Some(p) = self.layout.point_for(cur) else { return cur };
        let x = *self.goal_x.get_or_insert(p.x);
        let y = p.y + self.layout.line_h * dir + self.layout.line_h / 2.;
        if y < self.layout.paras.first().map_or(px(0.), |p| p.2.y) {
            return 0;
        }
        let last = self.layout.paras.last().map(|(_, l, o)| o.y + l.size(self.layout.line_h).height);
        if last.is_some_and(|b| y > b) {
            return self.content.len();
        }
        self.layout.index_for(point(x, y))
    }

    fn para_bounds(&self, o: usize) -> (usize, usize) {
        let o = o.min(self.content.len());
        let start = self.content[..o].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let end = self.content[o..].find('\n').map(|i| o + i).unwrap_or(self.content.len());
        (start, end)
    }

    fn left(&mut self, _: &TLeft, _: &mut Window, cx: &mut Context<Self>) {
        let o = if self.selected.is_empty() { self.prev_boundary(self.cursor()) } else { self.selected.start };
        self.goal_x = None;
        self.move_to(o, cx)
    }
    fn right(&mut self, _: &TRight, _: &mut Window, cx: &mut Context<Self>) {
        let o = if self.selected.is_empty() { self.next_boundary(self.cursor()) } else { self.selected.end };
        self.goal_x = None;
        self.move_to(o, cx)
    }
    fn on_first_row(&self) -> bool {
        match (self.layout.point_for(self.cursor()), self.layout.paras.first()) {
            (Some(p), Some(f)) => p.y < f.2.y + self.layout.line_h * 0.5,
            _ => true,
        }
    }
    fn on_last_row(&self) -> bool {
        match (self.layout.point_for(self.cursor()), self.layout.paras.last()) {
            (Some(p), Some((_, l, o))) => p.y + self.layout.line_h > o.y + l.size(self.layout.line_h).height - 1.0_f32 * self.layout.line_h * 0.5,
            _ => true,
        }
    }
    fn up(&mut self, _: &TUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.auto_height && self.on_first_row() {
            cx.emit(TextAreaEvent::Edge { up: true });
            return;
        }
        let o = self.vertical(-1.);
        self.selected = o..o;
        cx.notify();
    }
    fn down(&mut self, _: &TDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.auto_height && self.on_last_row() {
            cx.emit(TextAreaEvent::Edge { up: false });
            return;
        }
        let o = self.vertical(1.);
        self.selected = o..o;
        cx.notify();
    }
    fn select_left(&mut self, _: &TSelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.prev_boundary(self.cursor()), cx)
    }
    fn select_right(&mut self, _: &TSelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor()), cx)
    }
    fn select_up(&mut self, _: &TSelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let o = self.vertical(-1.);
        self.select_to(o, cx)
    }
    fn select_down(&mut self, _: &TSelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let o = self.vertical(1.);
        self.select_to(o, cx)
    }
    fn home(&mut self, _: &THome, _: &mut Window, cx: &mut Context<Self>) {
        let o = self.para_bounds(self.cursor()).0;
        self.move_to(o, cx)
    }
    fn end(&mut self, _: &TEnd, _: &mut Window, cx: &mut Context<Self>) {
        let o = self.para_bounds(self.cursor()).1;
        self.move_to(o, cx)
    }
    fn doc_start(&mut self, _: &TDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx)
    }
    fn doc_end(&mut self, _: &TDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx)
    }
    fn select_all(&mut self, _: &TSelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = 0..self.content.len();
        self.reversed = false;
        cx.notify()
    }
    fn backspace(&mut self, _: &TBackspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() && self.cursor() == 0 {
            cx.emit(TextAreaEvent::BackspaceAtStart);
            return;
        }
        if self.selected.is_empty() {
            self.select_to(self.prev_boundary(self.cursor()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }
    fn delete(&mut self, _: &TDelete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() && self.cursor() >= self.content.len() {
            cx.emit(TextAreaEvent::DeleteAtEnd);
            return;
        }
        if self.selected.is_empty() {
            self.select_to(self.next_boundary(self.cursor()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }
    fn word_back(&mut self, _: &TWordBack, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            let c = self.cursor();
            let start =
                self.content.split_word_bound_indices().rev().find_map(|(i, w)| (i < c && w.chars().any(|ch| ch.is_alphanumeric())).then_some(i)).unwrap_or(0);
            self.select_to(start, cx);
        }
        self.replace_text_in_range(None, "", window, cx)
    }
    fn newline(&mut self, _: &TNewline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx)
    }
    fn commit(&mut self, _: &TCommit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextAreaEvent::Commit)
    }
    fn cancel(&mut self, _: &TCancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextAreaEvent::Cancel)
    }
    fn tab(&mut self, _: &TTab, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextAreaEvent::Tab)
    }
    fn back_tab(&mut self, _: &TShiftTab, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextAreaEvent::BackTab)
    }
    fn copy(&mut self, _: &TCopy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            let r = clamp_str_range(&self.content, self.selected.clone());
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[r].to_string()));
        }
    }
    fn cut(&mut self, _: &TCut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            let r = clamp_str_range(&self.content, self.selected.clone());
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[r].to_string()));
            self.replace_text_in_range(None, "", window, cx)
        }
    }
    fn paste(&mut self, _: &TPaste, window: &mut Window, cx: &mut Context<Self>) {
        // 访达里复制的文件：剪贴板里的文本只是文件名，要按文件处理
        let files = super::macos::clipboard_file_paths();
        if !files.is_empty() {
            cx.emit(TextAreaEvent::PasteFiles(files));
            return;
        }
        let Some(item) = cx.read_from_clipboard() else { return };
        if let Some(t) = item.text() {
            self.replace_text_in_range(None, &t.replace("\r\n", "\n"), window, cx);
        } else if let Some(img) = item.entries().iter().find_map(|e| match e {
            ClipboardEntry::Image(i) => Some(i.clone()),
            _ => None,
        }) {
            cx.emit(TextAreaEvent::PasteImage(img));
        }
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        let ix = self.layout.index_for(ev.position);
        if ev.click_count >= 3 {
            let (s, e) = self.para_bounds(ix);
            self.selected = s..e;
        } else if ev.click_count == 2 {
            let (s, e) = self.word_at(ix);
            self.selected = s..e;
        } else if ev.modifiers.shift {
            self.select_to(ix, cx);
        } else {
            self.move_to(ix, cx);
            self.selecting = true;
        }
        self.goal_x = None;
        cx.notify();
    }

    fn word_at(&self, ix: usize) -> (usize, usize) {
        for (i, w) in self.content.split_word_bound_indices() {
            if ix >= i && ix <= i + w.len() && w.chars().any(|c| !c.is_whitespace()) {
                return (i, i + w.len());
            }
        }
        (ix, ix)
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting && ev.pressed_button == Some(MouseButton::Left) {
            let ix = self.layout.index_for(ev.position);
            self.select_to(ix, cx);
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
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
}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for TextArea {
    fn text_for_range(&mut self, r: Range<usize>, actual: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let range = clamp_str_range(&self.content, self.range_utf16_to_byte(&r));
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
        let range = clamp_str_range(&self.content, r.as_ref().map(|r| self.range_utf16_to_byte(r)).or(self.marked.clone()).unwrap_or(self.selected.clone()));
        self.content.replace_range(range.clone(), new);
        self.selected = range.start + new.len()..range.start + new.len();
        self.reversed = false;
        self.marked = None;
        self.changed(cx);
    }
    fn replace_and_mark_text_in_range(&mut self, r: Option<Range<usize>>, new: &str, sel: Option<Range<usize>>, _: &mut Window, cx: &mut Context<Self>) {
        let range = clamp_str_range(&self.content, r.as_ref().map(|r| self.range_utf16_to_byte(r)).or(self.marked.clone()).unwrap_or(self.selected.clone()));
        self.content.replace_range(range.clone(), new);
        self.marked = (!new.is_empty()).then(|| range.start..range.start + new.len());
        let to8 = |u: usize| {
            let mut c = 0;
            for (i, ch) in new.char_indices() {
                if c >= u {
                    return i;
                }
                c += ch.len_utf16();
            }
            new.len()
        };
        self.selected = sel.map(|s| range.start + to8(s.start)..range.start + to8(s.end)).unwrap_or(range.start + new.len()..range.start + new.len());
        self.reversed = false;
        self.changed(cx);
    }
    fn bounds_for_range(&mut self, r: Range<usize>, bounds: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let range = self.range_utf16_to_byte(&r);
        let Some(p) = self.layout.point_for(range.start) else { return Some(bounds) };
        Some(Bounds::new(p, size(px(2.), self.layout.line_h)))
    }
    fn character_index_for_point(&mut self, p: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.byte_to_utf16(self.layout.index_for(p)))
    }
}

struct AreaEl {
    input: Entity<TextArea>,
}

impl IntoElement for AreaEl {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

struct Prepaint {
    scroll: Pixels,
    scroll_max: Pixels,
    layout: Layout,
    total: Pixels,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

impl Element for AreaEl {
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
        style.size.height = relative(1.).into();
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
        let empty = input.content.is_empty();
        let text: SharedString = if empty { input.placeholder.clone() } else { input.content.clone().into() };
        let color = if empty { t.placeholder } else { t.text };
        let font = window.text_style().font();
        let base = TextRun { len: text.len(), font, color, background_color: None, underline: None, strikethrough: None };
        let runs = match input.marked.as_ref().filter(|_| !empty).map(|m| clamp_str_range(&text, m.clone())) {
            Some(m) => vec![
                TextRun { len: m.start, ..base.clone() },
                TextRun { len: m.end - m.start, underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.), wavy: false }), ..base.clone() },
                TextRun { len: text.len() - m.end, ..base },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            None => vec![base],
        };
        let line_h = px(input.line_height);
        let width = (bounds.size.width - px(10.)).max(px(4.));
        let lines = window.text_system().shape_text(text.clone(), px(input.font_size), &runs, Some(width), None).unwrap_or_default();
        let total = lines.iter().fold(px(0.), |acc, l| acc + l.size(line_h).height);
        let mut y = bounds.top();
        let mut paras = Vec::with_capacity(lines.len());
        let mut start = 0;
        for line in lines {
            let h = line.size(line_h).height;
            let len = line.len();
            paras.push((start, line, point(bounds.left(), y)));
            start += len + 1;
            y += h;
        }
        let mut layout = Layout { paras, line_h };
        let scroll_max = (total - bounds.size.height + px(8.)).max(px(0.));
        let mut scroll = input.scroll_y.clamp(px(0.), scroll_max);
        if input.selected != input.revealed {
            if let Some(c) = layout.point_for(input.cursor()) {
                let (top, bottom) = (c.y - bounds.top(), c.y - bounds.top() + line_h);
                if top < scroll {
                    scroll = top;
                } else if bottom > scroll + bounds.size.height {
                    scroll = bottom - bounds.size.height;
                }
                scroll = scroll.clamp(px(0.), scroll_max);
            }
        }
        for para in layout.paras.iter_mut() {
            para.2.y -= scroll;
        }
        let sel = input.selected.clone();
        let mut selection = vec![];
        let mut cursor = None;
        if empty {
            if let Some((_, _, o)) = layout.paras.first() {
                cursor = Some(fill(Bounds::new(*o, size(px(1.5), line_h)), t.accent));
            }
        } else if sel.is_empty() {
            if let Some(p) = layout.point_for(input.cursor()) {
                cursor = Some(fill(Bounds::new(p, size(px(1.5), line_h)), t.accent));
            }
        } else {
            for (pstart, line, origin) in &layout.paras {
                for (row, (rs, re)) in Layout::rows(line).into_iter().enumerate() {
                    let (gs, ge) = (pstart + rs, pstart + re);
                    let (a, b) = (sel.start.max(gs), sel.end.min(ge));
                    if a >= b && !(sel.start <= gs && sel.end > ge) {
                        continue;
                    }
                    let xa = line.position_for_index(a - pstart, line_h).map(|p| p.x).unwrap_or_default();
                    let xb = line.position_for_index(b - pstart, line_h).map(|p| p.x).unwrap_or_default();
                    let y = origin.y + line_h * row as f32;
                    let extra = if sel.end > ge && b == ge { px(5.) } else { px(0.) };
                    selection.push(fill(Bounds::from_corners(point(origin.x + xa, y), point(origin.x + xb + extra, y + line_h)), t.selection));
                }
            }
        }
        Prepaint { scroll, scroll_max, layout, total, cursor, selection }
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
        let mut thumb = None;
        if p.scroll_max > px(0.5) && bounds.size.height > px(20.) {
            let view = bounds.size.height;
            let thumb_h = (view * (view / (view + p.scroll_max))).max(px(28.));
            let top = bounds.top() + (view - thumb_h) * (p.scroll / p.scroll_max);
            thumb = Some(Bounds::new(point(bounds.right() - px(6.), top), size(px(4.), thumb_h)));
        }
        let muted = theme(cx).muted;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for q in p.selection.drain(..) {
                window.paint_quad(q);
            }
            for (_, line, origin) in &p.layout.paras {
                let b = Bounds::new(*origin, size(bounds.size.width, line.size(p.layout.line_h).height));
                line.paint(*origin, p.layout.line_h, TextAlign::Left, Some(b), window, cx).ok();
            }
            if focus.is_focused(window) {
                if let Some(c) = p.cursor.take() {
                    window.paint_quad(c);
                }
            }
        });
        if let Some(t) = thumb {
            window.paint_quad(fill(t, muted.opacity(0.45)).corner_radii(px(2.)));
        }
        let layout = std::mem::take(&mut p.layout);
        let (scroll, scroll_max, total) = (p.scroll, p.scroll_max, p.total);
        self.input.update(cx, |i, cx| {
            if i.auto_height {
                let want = total.max(px(i.line_height)) + px(8.);
                if (i.content_h - want).abs() > px(0.5) {
                    i.content_h = want;
                    cx.notify();
                }
            }
            i.layout = layout;
            i.scroll_y = scroll;
            i.scroll_max = scroll_max;
            i.revealed = i.selected.clone();
        });
    }
}

impl Render for TextArea {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let base = if self.auto_height { div().w_full().h(self.content_h.max(px(self.line_height + 8.))) } else { div().size_full() };
        base.key_context("TextArea")
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::word_back))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::commit))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::tab))
            .on_action(cx.listener(Self::back_tab))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                if this.scroll_max > px(0.) {
                    let dy = ev.delta.pixel_delta(this.layout.line_h).y;
                    this.scroll_y = (this.scroll_y - dy).clamp(px(0.), this.scroll_max);
                    this.revealed = this.selected.clone();
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(AreaEl { input: cx.entity() })
    }
}
