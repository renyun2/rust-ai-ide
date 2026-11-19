use crate::canvas::{FontStyle, Rect, SurfaceCanvas};

#[derive(Debug, Clone)]
pub struct WrappedLineSeg {
    pub start_char: usize,
    pub end_char: usize,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct TextInputField {
    pub text: String,
    pub cursor: usize,
    pub selection_anchor: Option<usize>,
    pub multiline: bool,
    pub scroll_x: i32,
    pub scroll_y: usize,
    pub history: Vec<String>,
    pub history_cursor: Option<usize>,
}

impl TextInputField {
    pub fn single_line() -> Self {
        Self::new(false, "")
    }

    pub fn multi_line() -> Self {
        Self::new(true, "")
    }

    pub fn new(multiline: bool, initial: &str) -> Self {
        let normalized = normalize_input(initial, multiline);
        let len = normalized.chars().count();
        Self {
            text: normalized,
            cursor: len,
            selection_anchor: None,
            multiline,
            scroll_x: 0,
            scroll_y: 0,
            history: Vec::new(),
            history_cursor: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn set_text(&mut self, s: &str) {
        self.text = normalize_input(s, self.multiline);
        self.cursor = self.char_len();
        self.selection_anchor = None;
        self.scroll_x = 0;
        self.scroll_y = 0;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.selection_anchor = None;
        self.scroll_x = 0;
        self.scroll_y = 0;
        self.history_cursor = None;
    }

    pub fn take_and_push_history(&mut self) -> String {
        let taken = std::mem::take(&mut self.text);
        let trimmed = taken.trim();
        if !trimmed.is_empty() && self.history.last().map(|s| s.as_str()) != Some(trimmed) {
            self.history.push(trimmed.to_string());
        }
        self.clear();
        taken
    }

    pub fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let next = match self.history_cursor {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_cursor = Some(next);
        let item = self.history[next].clone();
        self.set_text(&item);
        true
    }

    pub fn history_next(&mut self) -> bool {
        let Some(idx) = self.history_cursor else {
            return false;
        };
        if idx + 1 >= self.history.len() {
            self.history_cursor = None;
            self.text.clear();
            self.cursor = 0;
            self.selection_anchor = None;
        } else {
            let next = idx + 1;
            self.history_cursor = Some(next);
            let item = self.history[next].clone();
            self.set_text(&item);
        }
        true
    }

    pub fn selected_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor {
            None
        } else {
            Some((anchor.min(self.cursor), anchor.max(self.cursor)))
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        let (s, e) = self.selected_range()?;
        Some(self.slice_chars(s, e))
    }

    pub fn select_all(&mut self) {
        let len = self.char_len();
        if len == 0 {
            self.selection_anchor = None;
            self.cursor = 0;
        } else {
            self.selection_anchor = Some(0);
            self.cursor = len;
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
    }

    pub fn insert_str(&mut self, s: &str) {
        let norm = normalize_input(s, self.multiline);
        if norm.is_empty() {
            return;
        }
        self.delete_selection_internal();
        let byte_idx = char_to_byte_idx(&self.text, self.cursor);
        self.text.insert_str(byte_idx, &norm);
        self.cursor += norm.chars().count();
        self.selection_anchor = None;
        self.history_cursor = None;
    }

    pub fn backspace(&mut self) {
        if self.delete_selection_internal() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let end_b = char_to_byte_idx(&self.text, self.cursor);
        let start_b = char_to_byte_idx(&self.text, self.cursor - 1);
        self.text.replace_range(start_b..end_b, "");
        self.cursor -= 1;
        self.history_cursor = None;
    }

    pub fn delete_forward(&mut self) {
        if self.delete_selection_internal() {
            return;
        }
        let len = self.char_len();
        if self.cursor >= len {
            return;
        }
        let start_b = char_to_byte_idx(&self.text, self.cursor);
        let end_b = char_to_byte_idx(&self.text, self.cursor + 1);
        self.text.replace_range(start_b..end_b, "");
        self.history_cursor = None;
    }

    pub fn delete_word_backward(&mut self) {
        if self.delete_selection_internal() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let target = self.prev_word_boundary(self.cursor);
        let start_b = char_to_byte_idx(&self.text, target);
        let end_b = char_to_byte_idx(&self.text, self.cursor);
        self.text.replace_range(start_b..end_b, "");
        self.cursor = target;
        self.history_cursor = None;
    }

    pub fn delete_word_forward(&mut self) {
        if self.delete_selection_internal() {
            return;
        }
        let len = self.char_len();
        if self.cursor >= len {
            return;
        }
        let target = self.next_word_boundary(self.cursor);
        let start_b = char_to_byte_idx(&self.text, self.cursor);
        let end_b = char_to_byte_idx(&self.text, target);
        self.text.replace_range(start_b..end_b, "");
        self.history_cursor = None;
    }

    pub fn cut_selected(&mut self) -> Option<String> {
        let text = self.selected_text()?;
        self.delete_selection_internal();
        Some(text)
    }

    pub fn move_left(&mut self, extend: bool) {
        if !extend {
            if let Some((s, _)) = self.selected_range() {
                self.cursor = s;
                self.selection_anchor = None;
                return;
            }
        } else if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        self.cursor = self.cursor.saturating_sub(1);
        if !extend {
            self.selection_anchor = None;
        }
    }

    pub fn move_right(&mut self, extend: bool) {
        let len = self.char_len();
        if !extend {
            if let Some((_, e)) = self.selected_range() {
                self.cursor = e;
                self.selection_anchor = None;
                return;
            }
        } else if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        self.cursor = (self.cursor + 1).min(len);
        if !extend {
            self.selection_anchor = None;
        }
    }

    pub fn move_word_left(&mut self, extend: bool) {
        if extend && self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        self.cursor = self.prev_word_boundary(self.cursor);
        if !extend {
            self.selection_anchor = None;
        }
    }

    pub fn move_word_right(&mut self, extend: bool) {
        if extend && self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        self.cursor = self.next_word_boundary(self.cursor);
        if !extend {
            self.selection_anchor = None;
        }
    }

    pub fn move_home(&mut self, extend: bool) {
        if extend && self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        if !self.multiline {
            self.cursor = 0;
        } else {
            let chars: Vec<char> = self.text.chars().collect();
            let mut pos = self.cursor.min(chars.len());
            while pos > 0 && chars[pos - 1] != '\n' {
                pos -= 1;
            }
            self.cursor = pos;
        }
        if !extend {
            self.selection_anchor = None;
        }
    }

    pub fn move_end(&mut self, extend: bool) {
        if extend && self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        let chars: Vec<char> = self.text.chars().collect();
        if !self.multiline {
            self.cursor = chars.len();
        } else {
            let mut pos = self.cursor.min(chars.len());
            while pos < chars.len() && chars[pos] != '\n' {
                pos += 1;
            }
            self.cursor = pos;
        }
        if !extend {
            self.selection_anchor = None;
        }
    }

    pub fn move_vertical_in_wrapped(
        &mut self,
        delta_lines: isize,
        extend: bool,
        canvas: &SurfaceCanvas,
        max_width: i32,
        style: FontStyle,
    ) -> bool {
        let segs = self.compute_wrapped_lines(canvas, max_width, style);
        if segs.is_empty() {
            return false;
        }
        let (cur_line, cur_col_chars) = self.cursor_line_and_col(&segs);
        let target_line = cur_line as isize + delta_lines;
        if target_line < 0 || target_line >= segs.len() as isize {
            return false;
        }
        let target_line = target_line as usize;
        let seg = &segs[target_line];
        let seg_len = seg.end_char.saturating_sub(seg.start_char);
        let next_cursor = seg.start_char + cur_col_chars.min(seg_len);

        if extend && self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        } else if !extend {
            self.selection_anchor = None;
        }
        self.cursor = next_cursor;
        true
    }

    pub fn set_cursor_at(&mut self, char_idx: usize, extend: bool) {
        let clamped = char_idx.min(self.char_len());
        if extend {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor);
            }
        } else {
            self.selection_anchor = None;
        }
        self.cursor = clamped;
    }

    pub fn select_word_at(&mut self, char_idx: usize) {
        let chars: Vec<char> = self.text.chars().collect();
        if chars.is_empty() {
            self.cursor = 0;
            self.selection_anchor = None;
            return;
        }
        let pos = char_idx.min(chars.len().saturating_sub(1));
        let is_word = |c: char| c.is_alphanumeric() || c == '_' || !c.is_ascii();
        let target_is_word = is_word(chars[pos]);
        let mut s = pos;
        while s > 0 && is_word(chars[s - 1]) == target_is_word && chars[s - 1] != '\n' {
            s -= 1;
        }
        let mut e = pos + 1;
        while e < chars.len() && is_word(chars[e]) == target_is_word && chars[e] != '\n' {
            e += 1;
        }
        self.selection_anchor = Some(s);
        self.cursor = e;
    }

    pub fn select_line_at(&mut self, char_idx: usize) {
        if !self.multiline {
            self.select_all();
            return;
        }
        let chars: Vec<char> = self.text.chars().collect();
        if chars.is_empty() {
            self.cursor = 0;
            self.selection_anchor = None;
            return;
        }
        let pos = char_idx.min(chars.len());
        let mut s = pos;
        while s > 0 && chars[s - 1] != '\n' {
            s -= 1;
        }
        let mut e = pos;
        while e < chars.len() && chars[e] != '\n' {
            e += 1;
        }
        self.selection_anchor = Some(s);
        self.cursor = e;
    }

    pub fn compute_wrapped_lines(
        &self,
        canvas: &SurfaceCanvas,
        max_width: i32,
        style: FontStyle,
    ) -> Vec<WrappedLineSeg> {
        let max_w = max_width.max(40);
        let chars: Vec<char> = self.text.chars().collect();
        let mut out = Vec::new();
        let mut line_start = 0usize;
        let mut idx = 0usize;

        while idx <= chars.len() {
            if idx == chars.len() || chars[idx] == '\n' {
                let raw_segment: String = chars[line_start..idx].iter().collect();
                if raw_segment.is_empty() {
                    out.push(WrappedLineSeg {
                        start_char: line_start,
                        end_char: line_start,
                        text: String::new(),
                    });
                } else {
                    let (full_w, _) = canvas.measure_text(&raw_segment, style);
                    if full_w <= max_w {
                        out.push(WrappedLineSeg {
                            start_char: line_start,
                            end_char: idx,
                            text: raw_segment,
                        });
                    } else {
                        let mut seg_start = line_start;
                        while seg_start < idx {
                            let rem_str: String = chars[seg_start..idx].iter().collect();
                            let (rw, _) = canvas.measure_text(&rem_str, style);
                            if rw <= max_w {
                                out.push(WrappedLineSeg {
                                    start_char: seg_start,
                                    end_char: idx,
                                    text: rem_str,
                                });
                                break;
                            }
                            let mut take_n = canvas
                                .measure_caret_col_in_line(&rem_str, max_w, style)
                                .max(1);
                            let rem_len = idx - seg_start;
                            if take_n >= rem_len {
                                take_n = rem_len.saturating_sub(1).max(1);
                            }
                            let mut cand: String =
                                chars[seg_start..seg_start + take_n].iter().collect();
                            if take_n > 1 && canvas.measure_text(&cand, style).0 > max_w {
                                take_n -= 1;
                                cand = chars[seg_start..seg_start + take_n].iter().collect();
                            }
                            let split_at = seg_start + take_n;
                            out.push(WrappedLineSeg {
                                start_char: seg_start,
                                end_char: split_at,
                                text: cand,
                            });
                            seg_start = split_at;
                        }
                    }
                }
                line_start = idx + 1;
            }
            idx += 1;
        }

        if out.is_empty() {
            out.push(WrappedLineSeg {
                start_char: 0,
                end_char: 0,
                text: String::new(),
            });
        }
        out
    }

    pub fn cursor_line_and_col(&self, segs: &[WrappedLineSeg]) -> (usize, usize) {
        let cur = self.cursor.min(self.char_len());
        let chars: Vec<char> = self.text.chars().collect();
        for (i, seg) in segs.iter().enumerate() {
            let is_last = i + 1 == segs.len();
            // 若当前位置正好在硬换行前的行尾（且不是被自动折行拆开的前一段），归属该行末尾
            let ends_with_newline = seg.end_char < chars.len() && chars[seg.end_char] == '\n';
            let inclusive_end = is_last || ends_with_newline;
            if cur >= seg.start_char
                && (cur < seg.end_char || (inclusive_end && cur <= seg.end_char))
            {
                return (i, cur - seg.start_char);
            }
        }
        let last = segs.len().saturating_sub(1);
        let len = segs[last].end_char.saturating_sub(segs[last].start_char);
        (last, len)
    }

    pub fn hit_test_singleline(
        &self,
        canvas: &SurfaceCanvas,
        click_x: i32,
        text_start_x: i32,
        style: FontStyle,
    ) -> usize {
        self.hit_test_singleline_masked(canvas, click_x, text_start_x, style, false)
    }

    pub fn hit_test_singleline_masked(
        &self,
        canvas: &SurfaceCanvas,
        click_x: i32,
        text_start_x: i32,
        style: FontStyle,
        masked: bool,
    ) -> usize {
        let display = if masked {
            "•".repeat(self.char_len())
        } else {
            self.text.clone()
        };
        let vis_offset_px = if self.scroll_x > 0 {
            let start_idx = first_visible_char_for_scroll(&display, canvas, self.scroll_x, style);
            let prefix: String = display.chars().take(start_idx).collect();
            canvas.measure_text(&prefix, style).0
        } else {
            0
        };
        let rel_x = (click_x - text_start_x + vis_offset_px).max(0);
        canvas.measure_caret_col_in_line(&display, rel_x, style)
    }

    pub fn hit_test_multiline(
        &self,
        canvas: &SurfaceCanvas,
        click_x: i32,
        click_y: i32,
        text_x: i32,
        text_y: i32,
        max_width: i32,
        line_h: i32,
        visible_lines: usize,
        style: FontStyle,
    ) -> usize {
        let segs = self.compute_wrapped_lines(canvas, max_width, style);
        let rel_row = ((click_y - text_y).max(0) / line_h.max(1)) as usize;
        let max_scroll = segs.len().saturating_sub(visible_lines.max(1));
        let scroll = self.scroll_y.min(max_scroll);
        let seg_idx = (scroll + rel_row).min(segs.len().saturating_sub(1));
        let seg = &segs[seg_idx];
        let rel_x = (click_x - text_x).max(0);
        let col = canvas.measure_caret_col_in_line(&seg.text, rel_x, style);
        (seg.start_char + col).min(seg.end_char)
    }

    /// 渲染单行输入框文本、选区高亮与光标；返回 IME 锚点坐标 `(x, y)`
    pub fn render_singleline(
        &mut self,
        canvas: &mut SurfaceCanvas,
        box_rect: Rect,
        pad_x: i32,
        text_y: i32,
        placeholder: &str,
        focused: bool,
        caret_visible: bool,
        text_color: u32,
        placeholder_color: u32,
        selection_bg: u32,
        caret_color: u32,
        style: FontStyle,
    ) -> (i32, i32) {
        self.render_singleline_masked(
            canvas,
            box_rect,
            pad_x,
            text_y,
            placeholder,
            focused,
            caret_visible,
            text_color,
            placeholder_color,
            selection_bg,
            caret_color,
            style,
            false,
        )
    }

    pub fn render_singleline_masked(
        &mut self,
        canvas: &mut SurfaceCanvas,
        box_rect: Rect,
        pad_x: i32,
        text_y: i32,
        placeholder: &str,
        focused: bool,
        caret_visible: bool,
        text_color: u32,
        placeholder_color: u32,
        selection_bg: u32,
        caret_color: u32,
        style: FontStyle,
        masked: bool,
    ) -> (i32, i32) {
        let avail_w = (box_rect.w - pad_x * 2).max(24);
        let text_x = box_rect.x + pad_x;
        let len = self.char_len();
        self.cursor = self.cursor.min(len);
        let display_str = if masked {
            "•".repeat(len)
        } else {
            self.text.clone()
        };

        let before_cursor: String = display_str.chars().take(self.cursor).collect();
        let (cursor_px, line_h) = canvas.measure_text(&before_cursor, style);
        let line_h = line_h.max(16);

        if cursor_px - self.scroll_x > avail_w - 6 {
            self.scroll_x = cursor_px - avail_w + 10;
        } else if cursor_px - self.scroll_x < 0 {
            self.scroll_x = (cursor_px - 8).max(0);
        }
        if display_str.is_empty() {
            self.scroll_x = 0;
        }

        let (start_vis_char, visible_slice) =
            visible_horizontal_slice_for(&display_str, canvas, self.scroll_x, avail_w, style);
        let visible_chars_count = visible_slice.chars().count();
        let end_vis_char = start_vis_char + visible_chars_count;

        let prefix_before_vis: String = display_str.chars().take(start_vis_char).collect();
        let (vis_offset_px, _) = canvas.measure_text(&prefix_before_vis, style);
        let draw_x = text_x;

        if display_str.is_empty() {
            canvas.draw_text_styled(
                draw_x,
                text_y,
                placeholder,
                placeholder_color,
                avail_w,
                style,
            );
            let caret_x = draw_x;
            if focused && caret_visible {
                canvas.fill_rect(Rect::new(caret_x, text_y + 1, 2, line_h), caret_color);
            }
            return (caret_x, text_y + line_h + 2);
        }

        // 绘制选区高亮
        if let Some((sel_s, sel_e)) = self.selected_range() {
            let cs = sel_s.max(start_vis_char).min(end_vis_char);
            let ce = sel_e.max(start_vis_char).min(end_vis_char);
            if ce > cs {
                let s_rel: String = display_str
                    .chars()
                    .skip(start_vis_char)
                    .take(cs - start_vis_char)
                    .collect();
                let e_rel: String = display_str
                    .chars()
                    .skip(start_vis_char)
                    .take(ce - start_vis_char)
                    .collect();
                let (sx, _) = canvas.measure_text(&s_rel, style);
                let (ex, _) = canvas.measure_text(&e_rel, style);
                canvas.fill_rounded_rect(
                    Rect::new(draw_x + sx, text_y, (ex - sx).max(4), line_h),
                    2,
                    selection_bg,
                );
            }
        }

        canvas.draw_text_styled(draw_x, text_y, &visible_slice, text_color, avail_w, style);

        let caret_rel_px = (cursor_px - vis_offset_px).clamp(0, avail_w - 2);
        let caret_x = draw_x + caret_rel_px;
        if focused && caret_visible {
            canvas.fill_rect(Rect::new(caret_x, text_y + 1, 2, line_h), caret_color);
        }
        (caret_x, text_y + line_h + 2)
    }

    /// 渲染多行自适应折行输入框（带选区与精确光标跟踪）；返回 IME 锚点坐标
    pub fn render_multiline(
        &mut self,
        canvas: &mut SurfaceCanvas,
        text_rect: Rect,
        line_h: i32,
        placeholder: &str,
        focused: bool,
        caret_visible: bool,
        text_color: u32,
        placeholder_color: u32,
        selection_bg: u32,
        caret_color: u32,
        style: FontStyle,
    ) -> (i32, i32) {
        let max_lines = (text_rect.h / line_h).max(1) as usize;
        if self.text.is_empty() {
            canvas.draw_text_styled(
                text_rect.x,
                text_rect.y,
                placeholder,
                placeholder_color,
                text_rect.w,
                style,
            );
            if focused && caret_visible {
                canvas.fill_rect(Rect::new(text_rect.x, text_rect.y + 1, 2, 15), caret_color);
            }
            return (text_rect.x, text_rect.y + line_h);
        }

        let segs = self.compute_wrapped_lines(canvas, text_rect.w, style);
        let (cur_line, cur_col) = self.cursor_line_and_col(&segs);

        if cur_line < self.scroll_y {
            self.scroll_y = cur_line;
        } else if cur_line >= self.scroll_y + max_lines {
            self.scroll_y = cur_line + 1 - max_lines;
        }
        let max_scroll = segs.len().saturating_sub(max_lines);
        if self.scroll_y > max_scroll {
            self.scroll_y = max_scroll;
        }

        let sel = self.selected_range();
        let mut ime = (text_rect.x, text_rect.y + line_h);
        let mut y = text_rect.y;

        for (vis_idx, seg) in segs.iter().enumerate().skip(self.scroll_y).take(max_lines) {
            if let Some((sel_s, sel_e)) = sel {
                let ov_s = sel_s.max(seg.start_char).min(seg.end_char);
                let ov_e = sel_e.max(seg.start_char).min(seg.end_char);
                if ov_e > ov_s
                    || (sel_s <= seg.start_char && sel_e > seg.end_char && seg.text.is_empty())
                {
                    let before: String = seg.text.chars().take(ov_s - seg.start_char).collect();
                    let upto: String = seg.text.chars().take(ov_e - seg.start_char).collect();
                    let (sx, _) = canvas.measure_text(&before, style);
                    let (ex, _) = canvas.measure_text(&upto, style);
                    let hl_w = if seg.text.is_empty() {
                        8
                    } else {
                        (ex - sx).max(4)
                    };
                    canvas.fill_rounded_rect(
                        Rect::new(text_rect.x + sx, y, hl_w, line_h - 1),
                        2,
                        selection_bg,
                    );
                }
            }

            if !seg.text.is_empty() {
                canvas.draw_text_styled(text_rect.x, y, &seg.text, text_color, text_rect.w, style);
            }

            if vis_idx == cur_line {
                let prefix: String = seg.text.chars().take(cur_col).collect();
                let (px, _) = canvas.measure_text(&prefix, style);
                let caret_x = (text_rect.x + px).min(text_rect.x + text_rect.w - 2);
                if focused && caret_visible {
                    canvas.fill_rect(Rect::new(caret_x, y + 1, 2, 15), caret_color);
                }
                ime = (caret_x, y + line_h);
            }
            y += line_h;
        }
        ime
    }

    fn delete_selection_internal(&mut self) -> bool {
        let Some((s, e)) = self.selected_range() else {
            return false;
        };
        let sb = char_to_byte_idx(&self.text, s);
        let eb = char_to_byte_idx(&self.text, e);
        self.text.replace_range(sb..eb, "");
        self.cursor = s;
        self.selection_anchor = None;
        self.history_cursor = None;
        true
    }

    fn slice_chars(&self, start: usize, end: usize) -> String {
        if end <= start {
            return String::new();
        }
        self.text.chars().skip(start).take(end - start).collect()
    }

    fn prev_word_boundary(&self, from: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = from.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    fn next_word_boundary(&self, from: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = from.min(chars.len());
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        i
    }
}

fn first_visible_char_for_scroll(
    display_text: &str,
    canvas: &SurfaceCanvas,
    scroll_px: i32,
    style: FontStyle,
) -> usize {
    if scroll_px <= 0 || display_text.is_empty() {
        return 0;
    }
    canvas
        .measure_caret_col_in_line(display_text, scroll_px, style)
        .min(display_text.chars().count())
}

fn visible_horizontal_slice_for(
    display_text: &str,
    canvas: &SurfaceCanvas,
    scroll_px: i32,
    avail_w: i32,
    style: FontStyle,
) -> (usize, String) {
    if display_text.is_empty() {
        return (0, String::new());
    }
    let start_idx = first_visible_char_for_scroll(display_text, canvas, scroll_px, style);
    let tail_byte = char_to_byte_idx(display_text, start_idx);
    let tail = &display_text[tail_byte..];
    if tail.is_empty() {
        return (start_idx, String::new());
    }
    let (tw, _) = canvas.measure_text(tail, style);
    if tw <= avail_w {
        return (start_idx, tail.to_string());
    }
    let take_n = canvas
        .measure_caret_col_in_line(tail, avail_w, style)
        .max(1);
    let end_byte = char_to_byte_idx(tail, take_n);
    (start_idx, tail[..end_byte].to_string())
}

fn normalize_input(s: &str, multiline: bool) -> String {
    let clean = s.replace("\r\n", "\n").replace('\r', "\n");
    if multiline {
        clean
    } else {
        clean.replace('\n', " ")
    }
}

fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_selection_and_word_edits() {
        let mut f = TextInputField::single_line();
        f.insert_str("hello world rust");
        assert_eq!(f.cursor, 16);

        f.move_word_left(false);
        assert_eq!(f.cursor, 12);
        f.insert_str("ai ");
        assert_eq!(f.as_str(), "hello world ai rust");

        f.move_word_left(true);
        assert_eq!(f.selected_text().as_deref(), Some("ai "));
        f.insert_str("fast ");
        assert_eq!(f.as_str(), "hello world fast rust");

        f.select_all();
        assert_eq!(f.selected_text().as_deref(), Some("hello world fast rust"));
        f.clear_selection();
        assert!(f.selected_text().is_none());
        f.select_all();
        f.backspace();
        assert!(f.is_empty());
    }

    #[test]
    fn test_multiline_wrapping_preserves_trailing_newline() {
        let canvas = SurfaceCanvas::new(800, 600);
        let mut f = TextInputField::multi_line();
        f.insert_str("line1\n");
        let segs = f.compute_wrapped_lines(&canvas, 200, FontStyle::UiSmall);
        assert_eq!(segs.len(), 2);
        let (line, col) = f.cursor_line_and_col(&segs);
        assert_eq!((line, col), (1, 0));
    }
}
