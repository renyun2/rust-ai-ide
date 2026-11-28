use anyhow::{bail, Context, Result};
use ide_core::LanguageId;
use ropey::Rope;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct UndoSnapshot {
    rope: Rope,
    cursor_line: usize,
    cursor_col: usize,
    selection_anchor: Option<(usize, usize)>,
    revision: usize,
}

/// 基于 Ropey 的高性能代码编辑器文本缓冲区（支持选区、拖拽/双击选词、完整 IDEA 编辑快捷键、光标与脏状态同步 Undo/Redo）
#[derive(Debug, Clone)]
pub struct TextBuffer {
    pub path: PathBuf,
    pub language: LanguageId,
    pub rope: Rope,
    pub dirty: bool,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub selection_anchor: Option<(usize, usize)>,
    pub selection_lines: Option<(usize, usize)>,
    revision: usize,
    saved_revision: usize,
    undo_stack: Vec<UndoSnapshot>,
    redo_stack: Vec<UndoSnapshot>,
}

impl TextBuffer {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;
        Ok(Self::from_text(path, &content))
    }

    pub fn from_text(path: PathBuf, text: &str) -> Self {
        let language = LanguageId::from_path(&path);
        let rope = if text.contains("\r\n") {
            let normalized = text.replace("\r\n", "\n");
            Rope::from_str(&normalized)
        } else {
            Rope::from_str(text)
        };
        Self {
            path,
            language,
            rope,
            dirty: false,
            cursor_line: 0,
            cursor_col: 0,
            selection_anchor: None,
            selection_lines: None,
            revision: 0,
            saved_revision: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn revision(&self) -> usize {
        self.revision
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines().max(1)
    }

    pub fn line_len_chars(&self, line_idx: usize) -> usize {
        if line_idx >= self.rope.len_lines() {
            return 0;
        }
        let s = self.rope.line(line_idx);
        let mut len = s.len_chars();
        while len > 0 {
            let ch = s.char(len - 1);
            if ch == '\n' || ch == '\r' {
                len -= 1;
            } else {
                break;
            }
        }
        len
    }

    pub fn line_text(&self, line_idx: usize) -> String {
        if line_idx >= self.rope.len_lines() {
            return String::new();
        }
        let mut s = self.rope.line(line_idx).to_string();
        while s.ends_with('\n') || s.ends_with('\r') {
            s.pop();
        }
        s
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// Estimates a single line's pixel width without allocating a `String`.
    pub fn line_display_width_est(&self, line: usize, char_w: i32) -> i32 {
        if line >= self.line_count() {
            return 0;
        }
        let mut est = 0i32;
        for c in self.rope.line(line).chars() {
            if c == '\n' || c == '\r' {
                break;
            }
            est += if c.is_ascii() { char_w } else { char_w * 2 };
        }
        est
    }

    pub fn clamp_pos(&self, line: usize, col: usize) -> (usize, usize) {
        let max_line = self.line_count().saturating_sub(1);
        let l = line.min(max_line);
        let c = col.min(self.line_len_chars(l));
        (l, c)
    }

    fn pos_to_char(&self, line: usize, col: usize) -> usize {
        let (l, c) = self.clamp_pos(line, col);
        let line_start = self.rope.line_to_char(l);
        (line_start + c).min(self.rope.len_chars())
    }

    fn char_to_pos(&self, char_idx: usize) -> (usize, usize) {
        let idx = char_idx.min(self.rope.len_chars());
        let line = self.rope.char_to_line(idx);
        let line_start = self.rope.line_to_char(line);
        let col = idx.saturating_sub(line_start).min(self.line_len_chars(line));
        (line, col)
    }

    fn sync_selection_lines(&mut self) {
        self.selection_lines = self.selection_range().map(|((sl, _), (el, _))| (sl, el));
    }

    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
        self.selection_lines = None;
    }

    pub fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.clamp_pos(self.selection_anchor?.0, self.selection_anchor?.1);
        let cursor = self.clamp_pos(self.cursor_line, self.cursor_col);
        if anchor == cursor {
            None
        } else if anchor < cursor {
            Some((anchor, cursor))
        } else {
            Some((cursor, anchor))
        }
    }

    pub fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    pub fn selected_text(&self) -> Option<String> {
        let ((sl, sc), (el, ec)) = self.selection_range()?;
        let start = self.pos_to_char(sl, sc);
        let end = self.pos_to_char(el, ec);
        if start >= end {
            None
        } else {
            Some(self.rope.slice(start..end).to_string())
        }
    }

    pub fn select_all(&mut self) {
        let last_line = self.line_count().saturating_sub(1);
        let last_col = self.line_len_chars(last_line);
        self.selection_anchor = Some((0, 0));
        self.cursor_line = last_line;
        self.cursor_col = last_col;
        self.sync_selection_lines();
    }

    pub fn set_cursor(&mut self, line: usize, col: usize, extend: bool) {
        let old = self.clamp_pos(self.cursor_line, self.cursor_col);
        let next = self.clamp_pos(line, col);
        if extend {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(old);
            }
        } else {
            self.selection_anchor = None;
        }
        self.cursor_line = next.0;
        self.cursor_col = next.1;
        if self.selection_anchor == Some((self.cursor_line, self.cursor_col)) && !extend {
            self.selection_anchor = None;
        }
        self.sync_selection_lines();
    }

    pub fn select_word_at(&mut self, line: usize, col: usize) {
        let (l, c) = self.clamp_pos(line, col);
        let chars: Vec<char> = self.line_text(l).chars().collect();
        if chars.is_empty() {
            self.set_cursor(l, 0, false);
            return;
        }
        let idx = c.min(chars.len().saturating_sub(1));
        let kind = char_word_kind_at(&chars, idx);
        let mut start = idx;
        while start > 0 && char_word_kind_at(&chars, start - 1) == kind {
            start -= 1;
        }
        let mut end = idx + 1;
        while end < chars.len() && char_word_kind_at(&chars, end) == kind {
            end += 1;
        }
        self.selection_anchor = Some((l, start));
        self.cursor_line = l;
        self.cursor_col = end;
        self.sync_selection_lines();
    }

    /// 获取指定行列位置处的标识符/单词及其在当前行中的 `[start_col, end_col)` 范围
    pub fn word_at_pos(&self, line: usize, col: usize) -> Option<(String, usize, usize)> {
        let (l, c) = self.clamp_pos(line, col);
        let chars: Vec<char> = self.line_text(l).chars().collect();
        if chars.is_empty() {
            return None;
        }
        let probe = if c < chars.len() && char_word_kind_at(&chars, c) == WordCharKind::Word {
            c
        } else if c > 0 && char_word_kind_at(&chars, c - 1) == WordCharKind::Word {
            c - 1
        } else {
            return None;
        };
        let mut start = probe;
        while start > 0 && char_word_kind_at(&chars, start - 1) == WordCharKind::Word {
            start -= 1;
        }
        let mut end = probe + 1;
        while end < chars.len() && char_word_kind_at(&chars, end) == WordCharKind::Word {
            end += 1;
        }
        let word: String = chars[start..end].iter().collect();
        if word.is_empty() {
            None
        } else {
            Some((word, start, end))
        }
    }

    /// 获取当前光标下的标识符（若有单行选区且为合法标识符则优先返回选区）
    pub fn word_at_cursor(&self) -> Option<String> {
        if let Some(sel) = self.selected_text() {
            let trimmed = sel.trim();
            if !trimmed.is_empty() && !trimmed.contains('\n') && is_word_token_str(trimmed) {
                return Some(trimmed.to_string());
            }
        }
        self.word_at_pos(self.cursor_line, self.cursor_col)
            .map(|(w, _, _)| w)
    }

    /// 查找当前文档中所有匹配的 `(line, start_col, end_col)`
    pub fn find_all_matches(&self, query: &str, case_sensitive: bool) -> Vec<(usize, usize, usize)> {
        if query.is_empty() {
            return Vec::new();
        }
        let q_chars: Vec<char> = if case_sensitive {
            query.chars().collect()
        } else {
            query.to_lowercase().chars().collect()
        };
        let q_len = q_chars.len();
        if q_len == 0 {
            return Vec::new();
        }
        let mut matches = Vec::new();
        for line_idx in 0..self.line_count() {
            let line_str = self.line_text(line_idx);
            let l_chars: Vec<char> = if case_sensitive {
                line_str.chars().collect()
            } else {
                line_str.to_lowercase().chars().collect()
            };
            if l_chars.len() < q_len {
                continue;
            }
            let mut col = 0;
            while col + q_len <= l_chars.len() {
                if l_chars[col..col + q_len] == q_chars[..] {
                    matches.push((line_idx, col, col + q_len));
                    col += q_len;
                } else {
                    col += 1;
                }
            }
        }
        matches
    }

    /// 替换指定行与列区间的文本（带 Undo 记录）
    pub fn replace_range_on_line(
        &mut self,
        line: usize,
        start_col: usize,
        end_col: usize,
        replacement: &str,
    ) -> bool {
        if line >= self.line_count() || start_col > end_col {
            return false;
        }
        let line_len = self.line_len_chars(line);
        let sc = start_col.min(line_len);
        let ec = end_col.min(line_len);
        let start_char = self.rope.line_to_char(line) + sc;
        let end_char = self.rope.line_to_char(line) + ec;
        self.push_undo();
        if start_char < end_char {
            self.rope.remove(start_char..end_char);
        }
        self.rope.insert(start_char, replacement);
        self.cursor_line = line;
        self.cursor_col = sc + replacement.chars().count();
        self.clear_selection();
        self.mark_modified();
        true
    }

    /// 一次性替换文档中所有匹配项（单步 Undo）
    pub fn replace_all_matches(
        &mut self,
        query: &str,
        replacement: &str,
        case_sensitive: bool,
    ) -> usize {
        let matches = self.find_all_matches(query, case_sensitive);
        if matches.is_empty() {
            return 0;
        }
        self.push_undo();
        for &(line, sc, ec) in matches.iter().rev() {
            let line_start = self.rope.line_to_char(line);
            let start_char = line_start + sc;
            let end_char = line_start + ec;
            if start_char < end_char {
                self.rope.remove(start_char..end_char);
            }
            self.rope.insert(start_char, replacement);
        }
        let count = matches.len();
        let (cl, cc) = self.clamp_pos(self.cursor_line, self.cursor_col);
        self.cursor_line = cl;
        self.cursor_col = cc;
        self.clear_selection();
        self.mark_modified();
        count
    }

    /// 重命名当前缓冲区中所有完整单词匹配的标识符（`Shift+F6` Refactor Rename，支持单步 Undo）
    pub fn rename_identifier_in_buffer(&mut self, old_name: &str, new_name: &str) -> usize {
        let old_trimmed = old_name.trim();
        let new_trimmed = new_name.trim();
        if old_trimmed.is_empty() || new_trimmed.is_empty() || old_trimmed == new_trimmed {
            return 0;
        }
        let all_hits = self.find_all_matches(old_trimmed, true);
        let mut word_hits = Vec::new();
        for (line, sc, ec) in all_hits {
            let chars: Vec<char> = self.line_text(line).chars().collect();
            let prev_is_word = sc > 0 && char_word_kind_at(&chars, sc - 1) == WordCharKind::Word;
            let next_is_word = ec < chars.len() && char_word_kind_at(&chars, ec) == WordCharKind::Word;
            if !prev_is_word && !next_is_word {
                word_hits.push((line, sc, ec));
            }
        }
        if word_hits.is_empty() {
            return 0;
        }
        self.push_undo();
        for &(line, sc, ec) in word_hits.iter().rev() {
            let line_start = self.rope.line_to_char(line);
            self.rope.remove((line_start + sc)..(line_start + ec));
            self.rope.insert(line_start + sc, new_trimmed);
        }
        let count = word_hits.len();
        let (cl, cc) = self.clamp_pos(self.cursor_line, self.cursor_col);
        self.cursor_line = cl;
        self.cursor_col = cc;
        self.clear_selection();
        self.mark_modified();
        count
    }

    /// 查找光标处或紧邻处的匹配括号 `()` / `{}` / `[]` 位置 `((open_l, open_c), (close_l, close_c))`
    pub fn find_matching_bracket(&self, line: usize, col: usize) -> Option<((usize, usize), (usize, usize))> {
        let (l, c) = self.clamp_pos(line, col);
        let chars: Vec<char> = self.line_text(l).chars().collect();
        if chars.is_empty() {
            return None;
        }
        let (target_col, ch) = if c < chars.len() && is_bracket_char(chars[c]) {
            (c, chars[c])
        } else if c > 0 && c - 1 < chars.len() && is_bracket_char(chars[c - 1]) {
            (c - 1, chars[c - 1])
        } else {
            return None;
        };

        let (pair, forward) = match ch {
            '(' => (')', true),
            '[' => (']', true),
            '{' => ('}', true),
            ')' => ('(', false),
            ']' => ('[', false),
            '}' => ('{', false),
            _ => return None,
        };

        let max_scan_lines = 400usize;
        let mut depth = 0i32;
        if forward {
            let end_line = (l + max_scan_lines).min(self.line_count().saturating_sub(1));
            for cur_l in l..=end_line {
                let l_chars: Vec<char> = self.line_text(cur_l).chars().collect();
                let start_c = if cur_l == l { target_col } else { 0 };
                for (cur_c, &cur_ch) in l_chars.iter().enumerate().skip(start_c) {
                    if cur_ch == ch {
                        depth += 1;
                    } else if cur_ch == pair {
                        depth -= 1;
                        if depth == 0 {
                            return Some(((l, target_col), (cur_l, cur_c)));
                        }
                    }
                }
            }
        } else {
            let start_line = l.saturating_sub(max_scan_lines);
            for cur_l in (start_line..=l).rev() {
                let l_chars: Vec<char> = self.line_text(cur_l).chars().collect();
                let end_c = if cur_l == l {
                    target_col + 1
                } else {
                    l_chars.len()
                };
                for (cur_c, &cur_ch) in l_chars[..end_c.min(l_chars.len())].iter().enumerate().rev() {
                    if cur_ch == ch {
                        depth += 1;
                    } else if cur_ch == pair {
                        depth -= 1;
                        if depth == 0 {
                            return Some(((cur_l, cur_c), (l, target_col)));
                        }
                    }
                }
            }
        }
        None
    }

    pub fn select_line(&mut self, line: usize) {
        let (l, _) = self.clamp_pos(line, 0);
        let total = self.line_count();
        self.selection_anchor = Some((l, 0));
        if l + 1 < total {
            self.cursor_line = l + 1;
            self.cursor_col = 0;
        } else {
            self.cursor_line = l;
            self.cursor_col = self.line_len_chars(l);
        }
        self.sync_selection_lines();
    }

    fn delete_selection_internal(&mut self) -> bool {
        let Some(((sl, sc), (el, ec))) = self.selection_range() else {
            self.clear_selection();
            return false;
        };
        let start = self.pos_to_char(sl, sc);
        let end = self.pos_to_char(el, ec);
        if start < end {
            self.rope.remove(start..end);
        }
        self.cursor_line = sl.min(self.line_count().saturating_sub(1));
        self.cursor_col = sc.min(self.line_len_chars(self.cursor_line));
        self.clear_selection();
        self.mark_modified();
        true
    }

    pub fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            self.clear_selection();
            return false;
        }
        self.push_undo();
        self.delete_selection_internal()
    }

    pub fn slice_lines(&self, start_line: usize, end_line_inclusive: usize) -> String {
        let total = self.line_count();
        if total == 0 || start_line >= total {
            return String::new();
        }
        let end = end_line_inclusive.min(total - 1);
        let start_char = self.rope.line_to_char(start_line);
        let end_char = if end + 1 < total {
            self.rope.line_to_char(end + 1)
        } else {
            self.rope.len_chars()
        };
        self.rope.slice(start_char..end_char).to_string()
    }

    pub fn replace_line_range(
        &mut self,
        start_line: usize,
        end_line_inclusive: usize,
        replacement: &str,
    ) {
        self.push_undo();
        self.replace_line_range_no_undo(start_line, end_line_inclusive, replacement);
    }

    fn replace_line_range_no_undo(
        &mut self,
        start_line: usize,
        end_line_inclusive: usize,
        replacement: &str,
    ) {
        let total = self.line_count();
        let start = start_line.min(total.saturating_sub(1));
        let end = end_line_inclusive.min(total.saturating_sub(1));
        let start_char = self.rope.line_to_char(start);
        let end_char = if end + 1 < total {
            self.rope.line_to_char(end + 1)
        } else {
            self.rope.len_chars()
        };
        self.rope.remove(start_char..end_char);
        let mut normalized = replacement.replace("\r\n", "\n");
        if !normalized.is_empty() && !normalized.ends_with('\n') && end + 1 < total {
            normalized.push('\n');
        }
        self.rope.insert(start_char, &normalized);
        self.clear_selection();
        let (cl, cc) = self.clamp_pos(self.cursor_line, self.cursor_col);
        self.cursor_line = cl;
        self.cursor_col = cc;
        self.mark_modified();
    }

    /// IDEA `Ctrl+D`: 复制选中行或当前行并插入到下方
    pub fn duplicate_current_line(&mut self) {
        if let Some(((sl, _), (el, ec))) = self.selection_range() {
            let end_line = if ec == 0 && el > sl { el - 1 } else { el };
            let block = self.slice_lines(sl, end_line);
            let mut block_trimmed = block.trim_end_matches('\n').to_string();
            let duplicated = format!("{block_trimmed}\n{block_trimmed}");
            self.push_undo();
            self.replace_line_range_no_undo(sl, end_line, &duplicated);
            let count = end_line - sl + 1;
            self.cursor_line = (end_line + count).min(self.line_count().saturating_sub(1));
            self.cursor_col = self.cursor_col.min(self.line_len_chars(self.cursor_line));
            let _ = &mut block_trimmed;
            return;
        }
        let line = self.cursor_line.min(self.line_count().saturating_sub(1));
        let current = self.line_text(line);
        let replaced = format!("{current}\n{current}");
        self.push_undo();
        self.replace_line_range_no_undo(line, line, &replaced);
        self.cursor_line = (line + 1).min(self.line_count().saturating_sub(1));
        self.cursor_col = self.cursor_col.min(self.line_len_chars(self.cursor_line));
    }

    /// IDEA `Ctrl+Y`: 删除当前光标所在整行（或选区覆盖行）
    pub fn delete_current_line(&mut self) {
        let (sl, el) = if let Some(((s, _), (e, ec))) = self.selection_range() {
            let end_l = if ec == 0 && e > s { e - 1 } else { e };
            (s, end_l)
        } else {
            let l = self.cursor_line.min(self.line_count().saturating_sub(1));
            (l, l)
        };
        self.push_undo();
        self.clear_selection();
        if sl == 0 && el + 1 >= self.line_count() {
            self.rope = Rope::from_str("");
            self.cursor_line = 0;
            self.cursor_col = 0;
            self.mark_modified();
            return;
        }
        let start_char = self.rope.line_to_char(sl);
        let end_char = if el + 1 < self.line_count() {
            self.rope.line_to_char(el + 1)
        } else {
            // If deleting trailing lines, also remove preceding newline
            self.rope.len_chars()
        };
        let actual_start = if el + 1 >= self.line_count() && start_char > 0 {
            start_char - 1
        } else {
            start_char
        };
        self.rope.remove(actual_start..end_char);
        self.cursor_line = sl.min(self.line_count().saturating_sub(1));
        self.cursor_col = self.cursor_col.min(self.line_len_chars(self.cursor_line));
        self.mark_modified();
    }

    fn detect_sfc_comment_style(&self, target_line: usize) -> (&'static str, Option<&'static str>) {
        if !matches!(
            self.language,
            LanguageId::Vue | LanguageId::Svelte | LanguageId::Astro | LanguageId::Html | LanguageId::Xml
        ) {
            return (self.language.line_comment_prefix(), None);
        }
        if self.language == LanguageId::Xml {
            return ("<!-- ", Some(" -->"));
        }
        let mut in_script = false;
        let mut in_style = false;
        let mut in_fm = false;
        let mut fm_done = false;
        let mut seen_non_empty = false;
        for l in 0..=target_line.min(self.line_count().saturating_sub(1)) {
            let line = self.line_text(l);
            let t = line.trim();
            if self.language == LanguageId::Astro && !fm_done {
                if !seen_non_empty && t == "---" {
                    seen_non_empty = true;
                    in_fm = true;
                    continue;
                }
                if in_fm {
                    if t == "---" {
                        in_fm = false;
                        fm_done = true;
                    }
                    continue;
                }
            }
            if !t.is_empty() {
                seen_non_empty = true;
            }
            let lower = t.to_ascii_lowercase();
            if in_script {
                if lower.contains("</script") {
                    in_script = false;
                }
            } else if in_style {
                if lower.contains("</style") {
                    in_style = false;
                }
            } else if lower.starts_with("<script") && !lower.contains("</script") && !lower.ends_with("/>") {
                in_script = true;
            } else if lower.starts_with("<style") && !lower.contains("</style") && !lower.ends_with("/>") {
                in_style = true;
            }
        }
        if in_script || in_fm {
            ("// ", None)
        } else if in_style {
            ("/* ", Some(" */"))
        } else {
            ("<!-- ", Some(" -->"))
        }
    }

    /// IDEA `Ctrl+/`: 切换选中行或当前行的行注释
    pub fn toggle_line_comment(&mut self) {
        let (sl, el) = if let Some(((s, _), (e, ec))) = self.selection_range() {
            let end_l = if ec == 0 && e > s { e - 1 } else { e };
            (s, end_l)
        } else {
            let l = self.cursor_line.min(self.line_count().saturating_sub(1));
            (l, l)
        };
        let (prefix, suffix) = self.detect_sfc_comment_style(sl);
        let prefix_trimmed = prefix.trim();
        let suffix_trimmed = suffix.map(|s| s.trim());
        let all_commented = (sl..=el).all(|l| {
            let raw = self.line_text(l);
            let t = raw.trim();
            t.is_empty()
                || (t.starts_with(prefix_trimmed)
                    && suffix_trimmed.is_none_or(|sfx| t.ends_with(sfx)))
        });
        let mut out_lines = Vec::with_capacity(el - sl + 1);
        for l in sl..=el {
            let raw = self.line_text(l);
            let trimmed_start = raw.trim_start();
            let leading = raw.len() - trimmed_start.len();
            let indent = &raw[..leading];
            if all_commented {
                let mut inner = if let Some(rest) = trimmed_start.strip_prefix(prefix) {
                    rest
                } else if let Some(rest) = trimmed_start.strip_prefix(prefix_trimmed) {
                    rest
                } else {
                    out_lines.push(raw);
                    continue;
                };
                if let Some(sfx) = suffix {
                    let t_end = inner.trim_end();
                    inner = t_end
                        .strip_suffix(sfx)
                        .or_else(|| t_end.strip_suffix(sfx.trim()))
                        .unwrap_or(t_end);
                }
                out_lines.push(format!("{indent}{inner}"));
            } else if trimmed_start.is_empty() {
                out_lines.push(raw);
            } else if let Some(sfx) = suffix {
                out_lines.push(format!(
                    "{indent}{prefix}{}{sfx}",
                    trimmed_start.trim_end()
                ));
            } else {
                out_lines.push(format!("{indent}{prefix}{trimmed_start}"));
            }
        }
        let joined = out_lines.join("\n");
        let saved_cursor = (self.cursor_line, self.cursor_col);
        let saved_anchor = self.selection_anchor;
        self.push_undo();
        self.replace_line_range_no_undo(sl, el, &joined);
        self.cursor_line = saved_cursor.0.min(self.line_count().saturating_sub(1));
        self.cursor_col = saved_cursor.1.min(self.line_len_chars(self.cursor_line));
        self.selection_anchor = saved_anchor.map(|(al, ac)| self.clamp_pos(al, ac));
        self.sync_selection_lines();
    }

    pub fn insert_char_at_cursor(&mut self, ch: char) {
        self.push_undo();
        self.delete_selection_internal();
        let (line, col) = self.clamp_pos(self.cursor_line, self.cursor_col);
        let line_start = self.rope.line_to_char(line);
        self.rope.insert_char(line_start + col, ch);
        if ch == '\n' {
            self.cursor_line = line + 1;
            self.cursor_col = 0;
        } else {
            self.cursor_line = line;
            self.cursor_col = col + 1;
        }
        self.clear_selection();
        self.mark_modified();
    }

    pub fn insert_newline_with_auto_indent(&mut self) {
        self.push_undo();
        self.delete_selection_internal();
        let (line, col) = self.clamp_pos(self.cursor_line, self.cursor_col);
        let line_str = self.line_text(line);
        let chars: Vec<char> = line_str.chars().collect();
        let indent: String = chars
            .iter()
            .take_while(|c| **c == ' ' || **c == '\t')
            .collect();
        let before = chars[..col].iter().collect::<String>();
        let after = chars[col..].iter().collect::<String>();
        let before_trim = before.trim_end();
        let after_trim = after.trim_start();
        let opens_block = before_trim.ends_with('{')
            || before_trim.ends_with('(')
            || before_trim.ends_with('[')
            || (matches!(self.language, LanguageId::Python) && before_trim.ends_with(':'));
        let closes_immediately = (before_trim.ends_with('{') && after_trim.starts_with('}'))
            || (before_trim.ends_with('(') && after_trim.starts_with(')'))
            || (before_trim.ends_with('[') && after_trim.starts_with(']'));

        let char_idx = self.rope.line_to_char(line) + col;
        if closes_immediately {
            let inner_indent = format!("{indent}    ");
            let ins = format!("\n{inner_indent}\n{indent}");
            self.rope.insert(char_idx, &ins);
            self.cursor_line = line + 1;
            self.cursor_col = inner_indent.chars().count();
        } else {
            let extra = if opens_block { "    " } else { "" };
            let new_indent = format!("{indent}{extra}");
            let ins = format!("\n{new_indent}");
            self.rope.insert(char_idx, &ins);
            self.cursor_line = line + 1;
            self.cursor_col = new_indent.chars().count();
        }
        self.clear_selection();
        self.mark_modified();
    }

    pub fn insert_str_at_cursor(&mut self, s: &str) {
        let normalized = s.replace("\r\n", "\n").replace('\r', "\n");
        if normalized.is_empty() && !self.has_selection() {
            return;
        }
        self.push_undo();
        self.delete_selection_internal();
        if normalized.is_empty() {
            return;
        }
        let (line, col) = self.clamp_pos(self.cursor_line, self.cursor_col);
        let char_idx = self.rope.line_to_char(line) + col;
        self.rope.insert(char_idx, &normalized);
        let (nl, nc) = self.char_to_pos(char_idx + normalized.chars().count());
        self.cursor_line = nl;
        self.cursor_col = nc;
        self.clear_selection();
        self.mark_modified();
    }

    pub fn indent_selection_or_line(&mut self) {
        if let Some(((sl, sc), (el, ec))) = self.selection_range() {
            if el > sl || (sc == 0 && ec == self.line_len_chars(el)) {
                let end_l = if ec == 0 && el > sl { el - 1 } else { el };
                self.push_undo();
                for l in (sl..=end_l).rev() {
                    let idx = self.rope.line_to_char(l);
                    self.rope.insert(idx, "    ");
                }
                if let Some((al, ac)) = self.selection_anchor {
                    if al >= sl && al <= end_l {
                        self.selection_anchor = Some((al, ac + 4));
                    }
                }
                if self.cursor_line >= sl && self.cursor_line <= end_l {
                    self.cursor_col += 4;
                }
                self.sync_selection_lines();
                self.mark_modified();
                return;
            }
        }
        self.insert_str_at_cursor("    ");
    }

    pub fn unindent_selection_or_line(&mut self) {
        let (sl, el) = if let Some(((s, _), (e, ec))) = self.selection_range() {
            let end_l = if ec == 0 && e > s { e - 1 } else { e };
            (s, end_l)
        } else {
            let l = self.cursor_line.min(self.line_count().saturating_sub(1));
            (l, l)
        };
        let mut any_removed = false;
        let mut removed_counts = Vec::with_capacity(el - sl + 1);
        for l in sl..=el {
            let text = self.line_text(l);
            let remove_n = if text.starts_with('\t') {
                1
            } else {
                text.chars().take(4).take_while(|c| *c == ' ').count()
            };
            if remove_n > 0 {
                any_removed = true;
            }
            removed_counts.push((l, remove_n));
        }
        if !any_removed {
            return;
        }
        self.push_undo();
        for (l, remove_n) in removed_counts.iter().rev() {
            if *remove_n > 0 {
                let start = self.rope.line_to_char(*l);
                self.rope.remove(start..(start + *remove_n));
            }
        }
        for (l, remove_n) in &removed_counts {
            if self.cursor_line == *l {
                self.cursor_col = self.cursor_col.saturating_sub(*remove_n);
            }
            if let Some((al, ac)) = self.selection_anchor {
                if al == *l {
                    self.selection_anchor = Some((al, ac.saturating_sub(*remove_n)));
                }
            }
        }
        self.sync_selection_lines();
        self.mark_modified();
    }

    pub fn reformat_basic(&mut self) {
        let original = self.text();
        let formatted = if self.language == LanguageId::Markdown {
            format_markdown_source(&original)
        } else {
            let mut lines: Vec<String> = original
                .lines()
                .map(|l| l.trim_end().to_string())
                .collect();
            while lines.last().is_some_and(|l| l.is_empty()) {
                lines.pop();
            }
            let mut out = lines.join("\n");
            out.push('\n');
            out
        };
        if formatted == original {
            return;
        }
        self.push_undo();
        self.rope = Rope::from_str(&formatted);
        let (cl, cc) = self.clamp_pos(self.cursor_line, self.cursor_col);
        self.cursor_line = cl;
        self.cursor_col = cc;
        self.clear_selection();
        self.mark_modified();
    }

    pub fn move_left(&mut self, extend: bool) {
        if !extend {
            if let Some((start, _)) = self.selection_range() {
                self.set_cursor(start.0, start.1, false);
                return;
            }
        }
        let (l, c) = self.clamp_pos(self.cursor_line, self.cursor_col);
        if c > 0 {
            self.set_cursor(l, c - 1, extend);
        } else if l > 0 {
            let prev_len = self.line_len_chars(l - 1);
            self.set_cursor(l - 1, prev_len, extend);
        } else if !extend {
            self.clear_selection();
        }
    }

    pub fn move_right(&mut self, extend: bool) {
        if !extend {
            if let Some((_, end)) = self.selection_range() {
                self.set_cursor(end.0, end.1, false);
                return;
            }
        }
        let (l, c) = self.clamp_pos(self.cursor_line, self.cursor_col);
        let line_len = self.line_len_chars(l);
        if c < line_len {
            self.set_cursor(l, c + 1, extend);
        } else if l + 1 < self.line_count() {
            self.set_cursor(l + 1, 0, extend);
        } else if !extend {
            self.clear_selection();
        }
    }

    pub fn move_up(&mut self, extend: bool) {
        let (l, c) = self.clamp_pos(self.cursor_line, self.cursor_col);
        if l > 0 {
            self.set_cursor(l - 1, c, extend);
        } else if !extend {
            self.clear_selection();
        }
    }

    pub fn move_down(&mut self, extend: bool) {
        let (l, c) = self.clamp_pos(self.cursor_line, self.cursor_col);
        if l + 1 < self.line_count() {
            self.set_cursor(l + 1, c, extend);
        } else {
            let end_col = self.line_len_chars(l);
            self.set_cursor(l, end_col, extend);
        }
    }

    pub fn move_home(&mut self, extend: bool) {
        let (l, c) = self.clamp_pos(self.cursor_line, self.cursor_col);
        let text = self.line_text(l);
        let first_non_ws = text
            .chars()
            .take_while(|ch| *ch == ' ' || *ch == '\t')
            .count();
        let target = if c != first_non_ws && first_non_ws > 0 {
            first_non_ws
        } else {
            0
        };
        self.set_cursor(l, target, extend);
    }

    pub fn move_end(&mut self, extend: bool) {
        let (l, _) = self.clamp_pos(self.cursor_line, self.cursor_col);
        let len = self.line_len_chars(l);
        self.set_cursor(l, len, extend);
    }

    pub fn move_doc_start(&mut self, extend: bool) {
        self.set_cursor(0, 0, extend);
    }

    pub fn move_doc_end(&mut self, extend: bool) {
        let last_l = self.line_count().saturating_sub(1);
        let last_c = self.line_len_chars(last_l);
        self.set_cursor(last_l, last_c, extend);
    }

    pub fn move_page(&mut self, delta_lines: isize, extend: bool) {
        let (l, c) = self.clamp_pos(self.cursor_line, self.cursor_col);
        let target_l = (l as isize + delta_lines).clamp(0, self.line_count().saturating_sub(1) as isize) as usize;
        self.set_cursor(target_l, c, extend);
    }

    pub fn move_word_left(&mut self, extend: bool) {
        let (l, c) = self.clamp_pos(self.cursor_line, self.cursor_col);
        if c == 0 {
            if l > 0 {
                self.set_cursor(l - 1, self.line_len_chars(l - 1), extend);
            }
            return;
        }
        let chars: Vec<char> = self.line_text(l).chars().collect();
        let mut i = c.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        if i > 0 {
            let kind = char_word_kind_at(&chars, i - 1);
            while i > 0 && char_word_kind_at(&chars, i - 1) == kind {
                i -= 1;
            }
        }
        self.set_cursor(l, i, extend);
    }

    pub fn move_word_right(&mut self, extend: bool) {
        let (l, c) = self.clamp_pos(self.cursor_line, self.cursor_col);
        let chars: Vec<char> = self.line_text(l).chars().collect();
        if c >= chars.len() {
            if l + 1 < self.line_count() {
                self.set_cursor(l + 1, 0, extend);
            }
            return;
        }
        let mut i = c;
        if chars[i].is_whitespace() {
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
        } else {
            let kind = char_word_kind_at(&chars, i);
            while i < chars.len() && char_word_kind_at(&chars, i) == kind {
                i += 1;
            }
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
        }
        self.set_cursor(l, i, extend);
    }

    pub fn delete_forward_at_cursor(&mut self) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        let (line, col) = self.clamp_pos(self.cursor_line, self.cursor_col);
        let char_idx = self.rope.line_to_char(line) + col;
        if char_idx < self.rope.len_chars() {
            self.push_undo();
            self.rope.remove(char_idx..(char_idx + 1));
            self.mark_modified();
        }
    }

    pub fn backspace_at_cursor(&mut self) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        let (line, col) = self.clamp_pos(self.cursor_line, self.cursor_col);
        let char_idx = self.rope.line_to_char(line) + col;
        if char_idx == 0 {
            return;
        }
        self.push_undo();
        if col > 0 {
            self.rope.remove((char_idx - 1)..char_idx);
            self.cursor_line = line;
            self.cursor_col = col - 1;
        } else if line > 0 {
            let prev_len = self.line_len_chars(line - 1);
            self.rope.remove((char_idx - 1)..char_idx);
            self.cursor_line = line - 1;
            self.cursor_col = prev_len;
        }
        self.clear_selection();
        self.mark_modified();
    }

    pub fn delete_word_backward(&mut self) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        let end = self.clamp_pos(self.cursor_line, self.cursor_col);
        self.move_word_left(false);
        let start = self.clamp_pos(self.cursor_line, self.cursor_col);
        if start < end {
            self.selection_anchor = Some(end);
            self.cursor_line = start.0;
            self.cursor_col = start.1;
            self.delete_selection();
        }
    }

    pub fn delete_word_forward(&mut self) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        let start = self.clamp_pos(self.cursor_line, self.cursor_col);
        self.move_word_right(false);
        let end = self.clamp_pos(self.cursor_line, self.cursor_col);
        if start < end {
            self.selection_anchor = Some(start);
            self.cursor_line = end.0;
            self.cursor_col = end.1;
            self.delete_selection();
        }
    }

    pub fn save(&mut self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, self.rope.to_string())?;
        self.saved_revision = self.revision;
        self.dirty = false;
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(UndoSnapshot {
                rope: self.rope.clone(),
                cursor_line: self.cursor_line,
                cursor_col: self.cursor_col,
                selection_anchor: self.selection_anchor,
                revision: self.revision,
            });
            self.rope = prev.rope;
            self.cursor_line = prev.cursor_line.min(self.line_count().saturating_sub(1));
            self.cursor_col = prev.cursor_col.min(self.line_len_chars(self.cursor_line));
            self.selection_anchor = prev
                .selection_anchor
                .map(|(al, ac)| self.clamp_pos(al, ac));
            self.revision = prev.revision;
            self.dirty = self.revision != self.saved_revision;
            self.sync_selection_lines();
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(UndoSnapshot {
                rope: self.rope.clone(),
                cursor_line: self.cursor_line,
                cursor_col: self.cursor_col,
                selection_anchor: self.selection_anchor,
                revision: self.revision,
            });
            self.rope = next.rope;
            self.cursor_line = next.cursor_line.min(self.line_count().saturating_sub(1));
            self.cursor_col = next.cursor_col.min(self.line_len_chars(self.cursor_line));
            self.selection_anchor = next
                .selection_anchor
                .map(|(al, ac)| self.clamp_pos(al, ac));
            self.revision = next.revision;
            self.dirty = self.revision != self.saved_revision;
            self.sync_selection_lines();
            true
        } else {
            false
        }
    }

    fn mark_modified(&mut self) {
        self.revision += 1;
        self.dirty = self.revision != self.saved_revision;
    }

    fn push_undo(&mut self) {
        if self.undo_stack.len() >= 100 {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(UndoSnapshot {
            rope: self.rope.clone(),
            cursor_line: self.cursor_line,
            cursor_col: self.cursor_col,
            selection_anchor: self.selection_anchor,
            revision: self.revision,
        });
        self.redo_stack.clear();
        if self.redo_stack.capacity() > 16 {
            self.redo_stack.shrink_to_fit();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordCharKind {
    Word,
    Space,
    Punct,
}

fn is_bracket_char(ch: char) -> bool {
    matches!(ch, '(' | ')' | '[' | ']' | '{' | '}')
}

#[inline]
fn is_base_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

#[inline]
fn is_connector_hyphen(chars: &[char], idx: usize) -> bool {
    chars.get(idx).copied() == Some('-')
        && idx > 0
        && idx + 1 < chars.len()
        && is_base_word_char(chars[idx - 1])
        && is_base_word_char(chars[idx + 1])
}

#[inline]
fn char_word_kind_at(chars: &[char], idx: usize) -> WordCharKind {
    let ch = chars[idx];
    if is_base_word_char(ch) || is_connector_hyphen(chars, idx) {
        WordCharKind::Word
    } else if ch.is_whitespace() {
        WordCharKind::Space
    } else {
        WordCharKind::Punct
    }
}

fn is_word_token_str(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    !chars.is_empty()
        && (0..chars.len()).all(|idx| char_word_kind_at(&chars, idx) == WordCharKind::Word)
}

/// 左侧 Project 工具窗口的文件树节点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTreeEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
    pub language: LanguageId,
}

/// 可交互文件树状态管理器（支持任意目录点击展开/折叠、新建文件、新建目录、删除、重命名）
#[derive(Debug, Clone)]
pub struct ProjectTreeState {
    pub root: PathBuf,
    pub expanded_dirs: BTreeSet<PathBuf>,
    pub selected_path: Option<PathBuf>,
    pub visible_entries: Vec<FileTreeEntry>,
}

impl ProjectTreeState {
    pub fn new(root: &Path) -> Self {
        let mut expanded_dirs = BTreeSet::new();
        expanded_dirs.insert(root.to_path_buf());
        // 默认展开 crates 及其一级子目录，或 Java/Maven/Gradle 的 src/main/java 源码树
        let crates_dir = root.join("crates");
        if crates_dir.exists() {
            expanded_dirs.insert(crates_dir.clone());
            if let Ok(rd) = fs::read_dir(&crates_dir) {
                for entry in rd.flatten().take(4) {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        expanded_dirs.insert(entry.path());
                        let src_dir = entry.path().join("src");
                        if src_dir.exists() {
                            expanded_dirs.insert(src_dir);
                        }
                    }
                }
            }
        }
        let src_dir = root.join("src");
        if src_dir.is_dir()
            && (root.join("pom.xml").is_file()
                || root.join("build.gradle").is_file()
                || root.join("build.gradle.kts").is_file()
                || src_dir.join("main/java").is_dir())
        {
            expanded_dirs.insert(src_dir.clone());
            for sub in ["main", "main/java", "test", "test/java"] {
                let candidate = src_dir.join(sub);
                if candidate.is_dir() {
                    expanded_dirs.insert(candidate);
                }
            }
        }

        let mut tree = Self {
            root: root.to_path_buf(),
            expanded_dirs,
            selected_path: None,
            visible_entries: Vec::new(),
        };
        tree.rebuild();
        tree
    }

    pub fn rebuild(&mut self) {
        self.visible_entries.clear();
        self.collect_visible(&self.root.clone(), 0, 2000);
    }

    /// Re-sync visible tree entries with the filesystem while keeping valid expanded folders.
    pub fn refresh(&mut self) {
        let root = self.root.clone();
        self.expanded_dirs.retain(|p| p == &root || p.is_dir());
        if let Some(sel) = &self.selected_path {
            if !sel.exists() {
                self.selected_path = None;
            }
        }
        self.rebuild();
    }

    /// 展开目标文件所在的全部祖先目录并选中该文件，返回其在 `visible_entries` 中的索引
    pub fn reveal_path(&mut self, target: &Path) -> Option<usize> {
        let mut rel_components: Vec<std::ffi::OsString> = Vec::new();
        let mut cur = Some(target);
        let mut matched_root = false;
        while let Some(p) = cur {
            if paths_equal_tree(p, &self.root) {
                matched_root = true;
                break;
            }
            if let Some(name) = p.file_name() {
                rel_components.push(name.to_os_string());
            }
            cur = p.parent();
        }
        if !matched_root || rel_components.is_empty() {
            return None;
        }
        rel_components.reverse();

        let mut walker = self.root.clone();
        self.expanded_dirs.insert(walker.clone());
        for (idx, comp) in rel_components.iter().enumerate() {
            let is_last = idx + 1 == rel_components.len();
            let direct = walker.join(comp);
            let next_path = if direct.exists() {
                direct
            } else if let Ok(rd) = fs::read_dir(&walker) {
                let comp_str = comp.to_string_lossy();
                rd.filter_map(|e| e.ok())
                    .find(|e| {
                        let name = e.file_name();
                        #[cfg(windows)]
                        {
                            name.to_string_lossy().eq_ignore_ascii_case(&comp_str)
                        }
                        #[cfg(not(windows))]
                        {
                            name.to_string_lossy() == comp_str
                        }
                    })
                    .map(|e| e.path())
                    .unwrap_or(direct)
            } else {
                direct
            };
            if !is_last {
                self.expanded_dirs.insert(next_path.clone());
            }
            walker = next_path;
        }

        self.rebuild();
        let idx = self
            .visible_entries
            .iter()
            .position(|e| paths_equal_tree(&e.path, &walker))?;
        self.selected_path = Some(self.visible_entries[idx].path.clone());
        Some(idx)
    }

    pub fn toggle_dir(&mut self, dir: &Path) {
        if self.expanded_dirs.contains(dir) {
            self.expanded_dirs.remove(dir);
        } else {
            self.expanded_dirs.insert(dir.to_path_buf());
        }
        self.selected_path = Some(dir.to_path_buf());
        self.rebuild();
    }

    pub fn expand_all(&mut self) {
        self.expand_recursive(&self.root.clone(), 0);
        self.rebuild();
    }

    pub fn collapse_all(&mut self) {
        self.expanded_dirs.clear();
        self.expanded_dirs.insert(self.root.clone());
        self.rebuild();
    }

    /// 获取当前选中节点所在的目录（用于在当前位置新建文件或文件夹）
    pub fn active_target_dir(&self) -> PathBuf {
        if let Some(sel) = &self.selected_path {
            if sel.is_dir() {
                return sel.clone();
            }
            if let Some(parent) = sel.parent() {
                return parent.to_path_buf();
            }
        }
        self.root.clone()
    }

    pub fn create_new_file(&mut self, relative_or_name: &str) -> Result<PathBuf> {
        let base = self.active_target_dir();
        let target = base.join(relative_or_name.trim());
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
            self.expanded_dirs.insert(parent.to_path_buf());
        }
        if !target.exists() {
            let lang = LanguageId::from_path(&target);
            let content = match lang {
                LanguageId::Rust => "// Created in Rust AI IDE\npub fn init() {\n    println!(\"Ready\");\n}\n".to_string(),
                LanguageId::TypeScript | LanguageId::JavaScript => "// Created in Rust AI IDE\nexport function init(): void {\n  console.log('Ready');\n}\n".to_string(),
                LanguageId::Python => "# Created in Rust AI IDE\ndef main() -> None:\n    print('Ready')\n\nif __name__ == '__main__':\n    main()\n".to_string(),
                LanguageId::Java => build_java_file_template(&target),
                LanguageId::Go => "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"Ready\")\n}\n".to_string(),
                LanguageId::Kotlin => "fun main() {\n    println(\"Ready\")\n}\n".to_string(),
                LanguageId::Dart => "void main() {\n  print('Ready');\n}\n".to_string(),
                LanguageId::Vue => "<script setup lang=\"ts\">\nimport { ref } from 'vue'\n\nconst count = ref(0)\n</script>\n\n<template>\n  <div class=\"container\">\n    <button @click=\"count++\">Count: {{ count }}</button>\n  </div>\n</template>\n\n<style scoped>\n.container {\n  padding: 16px;\n}\n</style>\n".to_string(),
                LanguageId::Svelte => "<script lang=\"ts\">\n  let count = $state(0);\n</script>\n\n<main>\n  <button onclick={() => count++}>Count: {count}</button>\n</main>\n\n<style>\n  main {\n    padding: 16px;\n  }\n</style>\n".to_string(),
                LanguageId::Astro => "---\nconst title = 'Astro Component';\n---\n\n<section>\n  <h1>{title}</h1>\n</section>\n".to_string(),
                _ => String::new(),
            };
            fs::write(&target, content)?;
        }
        self.selected_path = Some(target.clone());
        self.rebuild();
        Ok(target)
    }

    pub fn create_new_directory(&mut self, dir_name: &str) -> Result<PathBuf> {
        let base = self.active_target_dir();
        let target = base.join(dir_name.trim());
        fs::create_dir_all(&target)?;
        self.expanded_dirs.insert(target.clone());
        self.selected_path = Some(target.clone());
        self.rebuild();
        Ok(target)
    }

    pub fn rename_path(&mut self, old_path: &Path, new_name: &str) -> Result<PathBuf> {
        if old_path == self.root {
            bail!("Cannot rename workspace root");
        }
        let trimmed = new_name.trim();
        if trimmed.is_empty() || trimmed.contains(['/', '\\']) {
            bail!("Invalid file name: {trimmed}");
        }
        let parent = old_path
            .parent()
            .context("Target path has no parent directory")?;
        let new_path = parent.join(trimmed);
        if new_path == old_path {
            return Ok(new_path);
        }
        if new_path.exists() {
            bail!("Target already exists: {}", new_path.display());
        }
        fs::rename(old_path, &new_path)
            .with_context(|| format!("Failed to rename {} to {}", old_path.display(), new_path.display()))?;
        if self.expanded_dirs.remove(old_path) {
            self.expanded_dirs.insert(new_path.clone());
        }
        self.selected_path = Some(new_path.clone());
        self.rebuild();
        Ok(new_path)
    }

    pub fn delete_path(&mut self, target: &Path) -> Result<()> {
        if target == self.root {
            return Ok(());
        }
        if target.is_dir() {
            fs::remove_dir_all(target)?;
            self.expanded_dirs.remove(target);
        } else if target.exists() {
            fs::remove_file(target)?;
        }
        self.selected_path = None;
        self.rebuild();
        Ok(())
    }

    /// Copy a file or directory into `target_dir` without modifying the source (copy mode).
    pub fn copy_path_to_dir(&mut self, src: &Path, target_dir: &Path) -> Result<PathBuf> {
        if !src.exists() {
            bail!("Source path does not exist: {}", src.display());
        }
        if !target_dir.is_dir() {
            bail!("Target directory does not exist: {}", target_dir.display());
        }
        if path_is_same_or_descendant_tree(src, target_dir) {
            bail!(
                "Cannot copy '{}' into itself or a subdirectory",
                src.display()
            );
        }
        let file_name = src
            .file_name()
            .context("Source path has no file name")?;
        let dest = next_available_copy_path(target_dir, file_name, src.is_dir());
        if src.is_dir() {
            copy_dir_recursive_tree(src, &dest)?;
        } else {
            fs::copy(src, &dest).with_context(|| {
                format!("Failed to copy {} to {}", src.display(), dest.display())
            })?;
        }
        self.expanded_dirs.insert(target_dir.to_path_buf());
        if self.reveal_path(&dest).is_none() {
            self.selected_path = Some(dest.clone());
            self.rebuild();
        }
        Ok(dest)
    }

    fn expand_recursive(&mut self, dir: &Path, depth: usize) {
        if depth > 4 {
            return;
        }
        self.expanded_dirs.insert(dir.to_path_buf());
        let Ok(rd) = fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if matches!(
                name.as_str(),
                ".git" | "target" | "node_modules" | "__pycache__" | ".venv"
            ) {
                continue;
            }
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                self.expand_recursive(&entry.path(), depth + 1);
            }
        }
    }

    fn collect_visible(&mut self, dir: &Path, depth: usize, max_entries: usize) {
        if self.visible_entries.len() >= max_entries || depth > 16 {
            return;
        }
        let Ok(read_dir) = fs::read_dir(dir) else {
            return;
        };
        let mut children: Vec<_> = read_dir.filter_map(|e| e.ok()).collect();
        children.sort_by(|a, b| {
            let a_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let b_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
            match (a_dir, b_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.file_name().cmp(&b.file_name()),
            }
        });

        for child in children {
            if self.visible_entries.len() >= max_entries {
                break;
            }
            let name = child.file_name().to_string_lossy().to_string();
            if matches!(
                name.as_str(),
                ".git" | "target" | "node_modules" | "__pycache__" | ".venv"
            ) {
                continue;
            }
            let path = child.path();
            let is_dir = child.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let expanded = is_dir && self.expanded_dirs.contains(&path);
            self.visible_entries.push(FileTreeEntry {
                name,
                language: LanguageId::from_path(&path),
                path: path.clone(),
                is_dir,
                depth,
                expanded,
            });
            if expanded {
                self.collect_visible(&path, depth + 1, max_entries);
            }
        }
    }
}

fn paths_equal_tree(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    #[cfg(windows)]
    {
        a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn path_is_same_or_descendant_tree(ancestor: &Path, candidate: &Path) -> bool {
    let anc = fs::canonicalize(ancestor).unwrap_or_else(|_| ancestor.to_path_buf());
    let cand = fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    if paths_equal_tree(&anc, &cand) {
        return true;
    }
    let mut cur = cand.parent();
    while let Some(p) = cur {
        if paths_equal_tree(p, &anc) {
            return true;
        }
        cur = p.parent();
    }
    false
}

fn next_available_copy_path(
    target_dir: &Path,
    file_name: &std::ffi::OsStr,
    is_dir: bool,
) -> PathBuf {
    let direct = target_dir.join(file_name);
    if !direct.exists() {
        return direct;
    }
    let name_str = file_name.to_string_lossy();
    let (stem, ext) = if !is_dir {
        match name_str.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s.to_string(), Some(e.to_string())),
            _ => (name_str.to_string(), None),
        }
    } else {
        (name_str.to_string(), None)
    };
    for idx in 1..1000usize {
        let candidate_name = match (&ext, idx) {
            (Some(e), 1) => format!("{stem}_copy.{e}"),
            (Some(e), n) => format!("{stem}_copy_{n}.{e}"),
            (None, 1) => format!("{stem}_copy"),
            (None, n) => format!("{stem}_copy_{n}"),
        };
        let candidate = target_dir.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    direct
}

fn copy_dir_recursive_tree(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)
        .with_context(|| format!("Failed to create directory: {}", dest.display()))?;
    for entry in fs::read_dir(src)
        .with_context(|| format!("Failed to read directory: {}", src.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let child_src = entry.path();
        let child_dest = dest.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive_tree(&child_src, &child_dest)?;
        } else if file_type.is_file() {
            fs::copy(&child_src, &child_dest).with_context(|| {
                format!(
                    "Failed to copy {} to {}",
                    child_src.display(),
                    child_dest.display()
                )
            })?;
        }
    }
    Ok(())
}

pub fn scan_workspace_tree(root: &Path, _max_entries: usize) -> Vec<FileTreeEntry> {
    ProjectTreeState::new(root).visible_entries
}

fn build_java_file_template(target: &Path) -> String {
    let raw_stem = target
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Main");
    let class_name: String = raw_stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    let class_name = if class_name.is_empty() {
        "Main".to_string()
    } else {
        class_name
    };

    let norm = target.to_string_lossy().replace('\\', "/");
    let pkg = ["src/main/java/", "src/test/java/", "src/"]
        .iter()
        .filter_map(|marker| norm.rfind(marker).map(|idx| idx + marker.len()))
        .max()
        .and_then(|start| {
            let after = &norm[start..];
            let dir_part = after.rsplit_once('/')?.0;
            let segments: Vec<&str> = dir_part
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            if segments.is_empty() {
                None
            } else {
                Some(segments.join("."))
            }
        });

    let mut out = String::new();
    if let Some(p) = pkg {
        out.push_str(&format!("package {p};\n\n"));
    }

    let is_test = class_name.ends_with("Test")
        || class_name.ends_with("Tests")
        || norm.contains("/src/test/java/");
    if is_test {
        out.push_str("import org.junit.jupiter.api.Test;\n");
        out.push_str("import static org.junit.jupiter.api.Assertions.*;\n\n");
        out.push_str(&format!(
            "class {class_name} {{\n    @Test\n    void shouldWork() {{\n        assertTrue(true);\n    }}\n}}\n"
        ));
    } else if matches!(class_name.as_str(), "Main" | "App" | "Application") {
        out.push_str(&format!(
            "public class {class_name} {{\n    public static void main(String[] args) {{\n        System.out.println(\"Ready\");\n    }}\n}}\n"
        ));
    } else {
        out.push_str(&format!("public class {class_name} {{\n    \n}}\n"));
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MdFmtAlign {
    None,
    Left,
    Center,
    Right,
}

fn md_display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let u = c as u32;
            if (0x1100..=0x115F).contains(&u)
                || (0x2E80..=0xA4CF).contains(&u)
                || (0xAC00..=0xD7A3).contains(&u)
                || (0xF900..=0xFAFF).contains(&u)
                || (0xFE10..=0xFE6F).contains(&u)
                || (0xFF00..=0xFF60).contains(&u)
                || (0xFFE0..=0xFFE6).contains(&u)
                || (0x1F300..=0x1FAFF).contains(&u)
            {
                2
            } else {
                1
            }
        })
        .sum()
}

fn md_split_table_cells(line: &str) -> Vec<String> {
    let mut trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix('|') {
        trimmed = rest;
    }
    if let Some(rest) = trimmed.strip_suffix('|') {
        if !rest.ends_with('\\') {
            trimmed = rest;
        }
    }
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut in_code = false;
    let mut chars = trimmed.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                if next == '|' {
                    cur.push('\\');
                    cur.push(chars.next().unwrap());
                    continue;
                }
            }
            cur.push(c);
        } else if c == '`' {
            in_code = !in_code;
            cur.push(c);
        } else if c == '|' && !in_code {
            cells.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

fn md_parse_table_separator(line: &str) -> Option<Vec<MdFmtAlign>> {
    if !line.contains('|') && !line.contains('-') {
        return None;
    }
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let cells = md_split_table_cells(trimmed);
    if cells.is_empty() {
        return None;
    }
    let mut aligns = Vec::with_capacity(cells.len());
    for cell in cells {
        let c = cell.trim();
        if c.is_empty() {
            return None;
        }
        let left = c.starts_with(':');
        let right = c.ends_with(':');
        let inner = c.trim_start_matches(':').trim_end_matches(':');
        if inner.is_empty() || !inner.chars().all(|ch| ch == '-') {
            return None;
        }
        aligns.push(match (left, right) {
            (true, true) => MdFmtAlign::Center,
            (true, false) => MdFmtAlign::Left,
            (false, true) => MdFmtAlign::Right,
            (false, false) => MdFmtAlign::None,
        });
    }
    Some(aligns)
}

fn md_is_horizontal_rule(trimmed: &str) -> bool {
    if trimmed.len() < 3 {
        return false;
    }
    let mut marker: Option<char> = None;
    let mut count = 0usize;
    for c in trimmed.chars() {
        if c == ' ' || c == '\t' {
            continue;
        }
        if matches!(c, '-' | '*' | '_') {
            if let Some(m) = marker {
                if m != c {
                    return false;
                }
            } else {
                marker = Some(c);
            }
            count += 1;
        } else {
            return false;
        }
    }
    count >= 3
}

fn md_fence_opener(trimmed: &str) -> Option<(char, usize)> {
    let ch = trimmed.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let count = trimmed.chars().take_while(|&c| c == ch).count();
    if count >= 3 {
        let rest = &trimmed[count..];
        if ch == '`' && rest.contains('`') {
            return None;
        }
        Some((ch, count))
    } else {
        None
    }
}

fn md_parse_atx_heading(trimmed: &str) -> Option<(usize, String)> {
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = trimmed[hashes..].trim();
    if rest.is_empty() {
        return None;
    }
    let cleaned = rest
        .trim_end_matches(|c: char| c == '#' || c == ' ' || c == '\t')
        .trim();
    if cleaned.is_empty() {
        return None;
    }
    Some((hashes, cleaned.to_string()))
}

fn md_format_list_line(line: &str) -> Option<String> {
    let indent_bytes = line.len() - line.trim_start_matches([' ', '\t']).len();
    let indent_spaces = line[..indent_bytes]
        .chars()
        .map(|c| if c == '\t' { 2 } else { 1 })
        .sum::<usize>();
    let normalized_indent = " ".repeat((indent_spaces / 2) * 2);
    let rest = &line[indent_bytes..];
    for marker in ["- ", "* ", "+ "] {
        if let Some(after) = rest.strip_prefix(marker) {
            let trimmed_after = after.trim_start();
            if let Some(task) = trimmed_after
                .strip_prefix("[ ]")
                .or_else(|| trimmed_after.strip_prefix("[x]"))
                .or_else(|| trimmed_after.strip_prefix("[X]"))
            {
                let checked = !trimmed_after.starts_with("[ ]");
                let box_str = if checked { "[x]" } else { "[ ]" };
                let task_body = task.trim();
                return Some(if task_body.is_empty() {
                    format!("{normalized_indent}- {box_str}")
                } else {
                    format!("{normalized_indent}- {box_str} {task_body}")
                });
            }
            return Some(format!("{normalized_indent}- {}", trimmed_after.trim_end()));
        }
    }
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let after_digits = &rest[digits..];
        if let Some(after_dot) = after_digits
            .strip_prefix(". ")
            .or_else(|| after_digits.strip_prefix(") "))
        {
            let num = &rest[..digits];
            return Some(format!(
                "{normalized_indent}{num}. {}",
                after_dot.trim_start().trim_end()
            ));
        }
    }
    None
}

fn md_format_blockquote_line(trimmed: &str) -> Option<String> {
    if !trimmed.starts_with('>') {
        return None;
    }
    let mut depth = 0usize;
    let mut rest = trimmed;
    while let Some(after) = rest.strip_prefix('>') {
        depth += 1;
        rest = after.trim_start();
    }
    let prefix = "> ".repeat(depth);
    if rest.is_empty() {
        Some(prefix.trim_end().to_string())
    } else {
        Some(format!("{prefix}{}", rest.trim_end()))
    }
}

fn md_pad_cell(text: &str, width: usize, align: MdFmtAlign) -> String {
    let cur = md_display_width(text);
    let pad = width.saturating_sub(cur);
    match align {
        MdFmtAlign::Right => format!("{}{}", " ".repeat(pad), text),
        MdFmtAlign::Center => {
            let left = pad / 2;
            let right = pad - left;
            format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
        }
        MdFmtAlign::Left | MdFmtAlign::None => format!("{}{}", text, " ".repeat(pad)),
    }
}

/// Format Markdown source text cleanly: aligns GFM tables, normalizes headings, lists,
/// blockquotes, horizontal rules, and blank lines while preserving fenced code blocks verbatim.
pub fn format_markdown_source(raw: &str) -> String {
    let normalized = raw.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut fence: Option<(char, usize)> = None;
    let mut i = 0usize;

    let push_blank_if_needed = |out: &mut Vec<String>| {
        if let Some(last) = out.last() {
            if !last.is_empty() {
                out.push(String::new());
            }
        }
    };

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if let Some((fch, flen)) = fence {
            out.push(line.to_string());
            let close_cnt = trimmed.chars().take_while(|&c| c == fch).count();
            if close_cnt >= flen && trimmed[close_cnt..].trim().is_empty() {
                fence = None;
            }
            i += 1;
            continue;
        }

        if let Some((fch, flen)) = md_fence_opener(trimmed) {
            fence = Some((fch, flen));
            out.push(line.trim_end().to_string());
            i += 1;
            continue;
        }

        if trimmed.is_empty() {
            push_blank_if_needed(&mut out);
            i += 1;
            continue;
        }

        // GFM Table block (header + separator + body rows)
        if line.contains('|') && i + 1 < lines.len() {
            if let Some(aligns) = md_parse_table_separator(lines[i + 1]) {
                let mut rows: Vec<Vec<String>> = vec![md_split_table_cells(line)];
                let mut j = i + 2;
                while j < lines.len() {
                    let row_line = lines[j];
                    if row_line.trim().is_empty() || !row_line.contains('|') {
                        break;
                    }
                    rows.push(md_split_table_cells(row_line));
                    j += 1;
                }
                let cols = aligns
                    .len()
                    .max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
                let mut col_widths = vec![3usize; cols];
                for r in &rows {
                    for (ci, cell) in r.iter().enumerate() {
                        col_widths[ci] = col_widths[ci].max(md_display_width(cell));
                    }
                }
                push_blank_if_needed(&mut out);
                for (r_idx, r) in rows.iter().enumerate() {
                    let mut row_s = String::from("|");
                    for ci in 0..cols {
                        let cell = r.get(ci).map(|s| s.as_str()).unwrap_or("");
                        let align = aligns.get(ci).copied().unwrap_or(MdFmtAlign::None);
                        row_s.push(' ');
                        row_s.push_str(&md_pad_cell(cell, col_widths[ci], align));
                        row_s.push_str(" |");
                    }
                    out.push(row_s);
                    if r_idx == 0 {
                        let mut sep_s = String::from("|");
                        for ci in 0..cols {
                            let w = col_widths[ci];
                            let align = aligns.get(ci).copied().unwrap_or(MdFmtAlign::None);
                            sep_s.push(' ');
                            match align {
                                MdFmtAlign::None => sep_s.push_str(&"-".repeat(w)),
                                MdFmtAlign::Left => {
                                    sep_s.push(':');
                                    sep_s.push_str(&"-".repeat(w.saturating_sub(1).max(2)));
                                }
                                MdFmtAlign::Right => {
                                    sep_s.push_str(&"-".repeat(w.saturating_sub(1).max(2)));
                                    sep_s.push(':');
                                }
                                MdFmtAlign::Center => {
                                    sep_s.push(':');
                                    sep_s.push_str(&"-".repeat(w.saturating_sub(2).max(1)));
                                    sep_s.push(':');
                                }
                            }
                            sep_s.push_str(" |");
                        }
                        out.push(sep_s);
                    }
                }
                if j < lines.len() && !lines[j].trim().is_empty() {
                    out.push(String::new());
                }
                i = j;
                continue;
            }
        }

        if let Some((level, title)) = md_parse_atx_heading(trimmed) {
            push_blank_if_needed(&mut out);
            out.push(format!("{} {}", "#".repeat(level), title));
            if i + 1 < lines.len() && !lines[i + 1].trim().is_empty() {
                out.push(String::new());
            }
            i += 1;
            continue;
        }

        // Preserve Setext heading underlines without turning them into HR spacing
        if i > 0
            && !lines[i - 1].trim().is_empty()
            && (trimmed.chars().all(|c| c == '=') || trimmed.chars().all(|c| c == '-'))
            && trimmed.len() >= 3
        {
            let ch = trimmed.chars().next().unwrap();
            out.push(ch.to_string().repeat(trimmed.len()));
            i += 1;
            continue;
        }

        if md_is_horizontal_rule(trimmed) {
            push_blank_if_needed(&mut out);
            out.push("---".to_string());
            if i + 1 < lines.len() && !lines[i + 1].trim().is_empty() {
                out.push(String::new());
            }
            i += 1;
            continue;
        }

        if let Some(q) = md_format_blockquote_line(trimmed) {
            out.push(q);
            i += 1;
            continue;
        }

        if let Some(li) = md_format_list_line(line) {
            out.push(li);
            i += 1;
            continue;
        }

        out.push(line.trim_end().to_string());
        i += 1;
    }

    while out.first().is_some_and(|l| l.is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    let mut res = out.join("\n");
    if !res.is_empty() || !normalized.is_empty() {
        res.push('\n');
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rope_buffer_idea_actions() {
        let mut buf = TextBuffer::from_text(
            PathBuf::from("test.rs"),
            "fn main() {\n    println!(\"old\");\n}\n",
        );
        buf.cursor_line = 1;
        buf.toggle_line_comment();
        assert!(buf.line_text(1).trim_start().starts_with("//"));
        buf.toggle_line_comment();
        assert!(!buf.line_text(1).trim_start().starts_with("//"));
        buf.duplicate_current_line();
        assert_eq!(buf.line_count(), 5);
        buf.delete_current_line();
        assert_eq!(buf.line_count(), 4);
    }

    #[test]
    fn test_selection_replace_word_and_undo_redo() {
        let mut buf = TextBuffer::from_text(
            PathBuf::from("demo.rs"),
            "let alpha = beta_value + 1;\n",
        );
        buf.select_word_at(0, 14);
        assert_eq!(buf.selected_text().as_deref(), Some("beta_value"));
        buf.insert_str_at_cursor("gamma");
        assert_eq!(buf.line_text(0), "let alpha = gamma + 1;");
        assert!(buf.dirty);

        assert!(buf.undo());
        assert_eq!(buf.line_text(0), "let alpha = beta_value + 1;");
        assert!(!buf.dirty);
        assert!(buf.redo());
        assert_eq!(buf.line_text(0), "let alpha = gamma + 1;");
        assert!(buf.dirty);
    }

    #[test]
    fn test_multiline_indent_unindent_and_auto_indent() {
        let mut buf = TextBuffer::from_text(PathBuf::from("a.rs"), "fn f() {}");
        buf.set_cursor(0, 8, false);
        buf.insert_newline_with_auto_indent();
        assert_eq!(buf.text(), "fn f() {\n    \n}");
        assert_eq!((buf.cursor_line, buf.cursor_col), (1, 4));

        buf.select_all();
        buf.indent_selection_or_line();
        assert!(buf.line_text(0).starts_with("    fn f()"));
        buf.unindent_selection_or_line();
        assert!(buf.line_text(0).starts_with("fn f()"));
    }

    #[test]
    fn test_project_tree_rename() {
        let tmp = std::env::temp_dir().join("rust_ai_ide_vfs_rename_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let mut tree = ProjectTreeState::new(&tmp);
        let p1 = tree.create_new_file("old_name.rs").unwrap();
        assert!(p1.exists());
        let p2 = tree.rename_path(&p1, "new_name.rs").unwrap();
        assert!(!p1.exists());
        assert!(p2.exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_java_file_template_and_tree_expansion() {
        let unique = format!(
            "rust_ai_ide_vfs_java_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let tmp = std::env::temp_dir().join(unique);
        fs::create_dir_all(tmp.join("src/main/java/com/example")).unwrap();
        fs::create_dir_all(tmp.join("src/test/java/com/example")).unwrap();
        fs::write(tmp.join("pom.xml"), "<project/>").unwrap();

        let mut tree = ProjectTreeState::new(&tmp);
        assert!(tree.expanded_dirs.contains(&tmp.join("src")));
        assert!(tree.expanded_dirs.contains(&tmp.join("src/main/java")));

        let app_file = tree
            .create_new_file("src/main/java/com/example/App.java")
            .unwrap();
        let app_src = fs::read_to_string(&app_file).unwrap();
        assert!(app_src.contains("package com.example;"));
        assert!(app_src.contains("public class App"));
        assert!(app_src.contains("public static void main"));

        tree.selected_path = None;
        let test_file = tree
            .create_new_file("src/test/java/com/example/AppTest.java")
            .unwrap();
        let test_src = fs::read_to_string(&test_file).unwrap();
        assert!(test_src.contains("package com.example;"));
        assert!(test_src.contains("@Test"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_format_markdown_source_and_buffer_reformat() {
        let raw = "#Title   \nSome text\n|Name|Value|\n|:---|---:|\n|foo|12|\n|longer|3|\n*  item 1\n- [X]  done\n>quote\n```rust\nlet   x=1;\n```\n***\n";
        let mut buf = TextBuffer::from_text(PathBuf::from("README.md"), raw);
        buf.reformat_basic();
        let out = buf.text();
        assert!(out.contains("# Title\n\nSome text\n"));
        assert!(out.contains("| Name   | Value |\n| :----- | ----: |\n| foo    |    12 |\n| longer |     3 |\n"));
        assert!(out.contains("- item 1\n- [x] done\n"));
        assert!(out.contains("> quote\n"));
        assert!(out.contains("```rust\nlet   x=1;\n```"));
        assert!(out.contains("\n---\n"));
    }

    #[test]
    fn test_vue_sfc_section_aware_line_comment_toggle() {
        let src = "<script setup lang=\"ts\">\nconst a = 1\n</script>\n<template>\n  <div>hi</div>\n</template>\n<style scoped>\n  .box { color: red; }\n</style>\n";
        let mut buf = TextBuffer::from_text(PathBuf::from("App.vue"), src);
        buf.set_cursor(1, 0, false);
        buf.toggle_line_comment();
        assert_eq!(buf.line_text(1), "// const a = 1");
        buf.toggle_line_comment();
        assert_eq!(buf.line_text(1), "const a = 1");

        buf.set_cursor(4, 0, false);
        buf.toggle_line_comment();
        assert_eq!(buf.line_text(4), "  <!-- <div>hi</div> -->");
        buf.toggle_line_comment();
        assert_eq!(buf.line_text(4), "  <div>hi</div>");

        buf.set_cursor(7, 0, false);
        buf.toggle_line_comment();
        assert_eq!(buf.line_text(7), "  /* .box { color: red; } */");
        buf.toggle_line_comment();
        assert_eq!(buf.line_text(7), "  .box { color: red; }");
    }

    #[test]
    fn test_project_tree_reveal_path_expands_collapsed_ancestors() {
        let unique = format!(
            "rust_ai_ide_vfs_reveal_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let tmp = std::env::temp_dir().join(unique);
        let nested_dir = tmp.join("apps/web/src/components");
        fs::create_dir_all(&nested_dir).unwrap();
        let target_file = nested_dir.join("Header.vue");
        fs::write(&target_file, "<template><header/></template>\n").unwrap();

        let mut tree = ProjectTreeState::new(&tmp);
        // Initially collapsed: Header.vue should not be in visible_entries.
        assert!(!tree
            .visible_entries
            .iter()
            .any(|e| paths_equal_tree(&e.path, &target_file)));

        let idx = tree
            .reveal_path(&target_file)
            .expect("target file should be revealed in project tree");
        assert!(paths_equal_tree(&tree.visible_entries[idx].path, &target_file));
        assert_eq!(
            tree.selected_path.as_deref(),
            Some(tree.visible_entries[idx].path.as_path())
        );
        assert!(tree.expanded_dirs.contains(&tmp.join("apps")));
        assert!(tree.expanded_dirs.contains(&nested_dir));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_hyphenated_identifiers_indexed_and_selected_together() {
        let src = "const pkg = \"antigravity-cli\"; // a - b, x->y, n--, -1\n<EstimateModalThFilter v-model=\"m\" col-key=\"k\" />\n";
        let mut buf = TextBuffer::from_text(PathBuf::from("App.vue"), src);

        // Double-click / word lookup on left half, hyphen, and right half of `antigravity-cli`
        buf.select_word_at(0, 15);
        assert_eq!(buf.selected_text().as_deref(), Some("antigravity-cli"));
        buf.select_word_at(0, 24);
        assert_eq!(buf.selected_text().as_deref(), Some("antigravity-cli"));
        buf.select_word_at(0, 26);
        assert_eq!(buf.selected_text().as_deref(), Some("antigravity-cli"));
        assert_eq!(
            buf.word_at_pos(0, 24).map(|(w, _, _)| w).as_deref(),
            Some("antigravity-cli")
        );
        assert_eq!(buf.word_at_cursor().as_deref(), Some("antigravity-cli"));

        // Kebab-case Vue attributes are also selected/indexed together
        buf.select_word_at(1, 25);
        assert_eq!(buf.selected_text().as_deref(), Some("v-model"));
        buf.select_word_at(1, 37);
        assert_eq!(buf.selected_text().as_deref(), Some("col-key"));

        // Non-connector hyphens (`a - b`, `x->y`, `n--`, `-1`) remain separated
        buf.select_word_at(0, 34);
        assert_eq!(buf.selected_text().as_deref(), Some("a"));
        buf.select_word_at(0, 41);
        assert_eq!(buf.selected_text().as_deref(), Some("x"));
        buf.select_word_at(0, 47);
        assert_eq!(buf.selected_text().as_deref(), Some("n"));
        buf.select_word_at(0, 53);
        assert_eq!(buf.selected_text().as_deref(), Some("1"));

        // Renaming `antigravity` must not match inside `antigravity-cli`, while renaming `antigravity-cli` works
        assert_eq!(buf.rename_identifier_in_buffer("antigravity", "agy"), 0);
        assert_eq!(buf.rename_identifier_in_buffer("antigravity-cli", "agy-cli"), 1);
        assert!(buf.line_text(0).contains("\"agy-cli\""));
    }

    #[test]
    fn test_project_tree_copy_path_to_dir_copies_files_folders_and_avoids_collisions() {
        let unique = format!(
            "rust_ai_ide_vfs_copy_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let tmp = std::env::temp_dir().join(unique);
        let src_ws = tmp.join("ws_a");
        let dst_ws = tmp.join("ws_b");
        let dst_sub = dst_ws.join("nested");
        fs::create_dir_all(src_ws.join("folder/inner")).unwrap();
        fs::create_dir_all(&dst_sub).unwrap();
        let src_file = src_ws.join("hello.rs");
        fs::write(&src_file, "fn main() {}\n").unwrap();
        fs::write(src_ws.join("folder/a.txt"), "alpha").unwrap();
        fs::write(src_ws.join("folder/inner/b.txt"), "beta").unwrap();

        let mut tree = ProjectTreeState::new(&dst_ws);
        // 1. Copy file into target subdirectory
        let copied_1 = tree.copy_path_to_dir(&src_file, &dst_sub).unwrap();
        assert_eq!(copied_1, dst_sub.join("hello.rs"));
        assert_eq!(fs::read_to_string(&copied_1).unwrap(), "fn main() {}\n");
        assert!(src_file.exists(), "copy mode must preserve original file");

        // 2. Copy same file again -> generates non-colliding copy name
        let copied_2 = tree.copy_path_to_dir(&src_file, &dst_sub).unwrap();
        assert_eq!(copied_2, dst_sub.join("hello_copy.rs"));
        assert!(copied_2.exists());

        // 3. Copy folder recursively into workspace root
        let copied_dir = tree.copy_path_to_dir(&src_ws.join("folder"), &dst_ws).unwrap();
        assert_eq!(copied_dir, dst_ws.join("folder"));
        assert_eq!(fs::read_to_string(copied_dir.join("a.txt")).unwrap(), "alpha");
        assert_eq!(
            fs::read_to_string(copied_dir.join("inner/b.txt")).unwrap(),
            "beta"
        );

        // 4. Reject copying a folder into itself or its descendant
        assert!(tree.copy_path_to_dir(&copied_dir, &copied_dir).is_err());
        assert!(tree
            .copy_path_to_dir(&copied_dir, &copied_dir.join("inner"))
            .is_err());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_project_tree_refresh_syncs_external_changes_and_prunes_deleted() {
        let unique = format!(
            "rust_ai_ide_vfs_refresh_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let tmp = std::env::temp_dir().join(unique);
        let sub = tmp.join("sub");
        let gone_dir = tmp.join("gone_dir");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir_all(&gone_dir).unwrap();
        fs::write(sub.join("existing.rs"), "// ok\n").unwrap();

        let mut tree = ProjectTreeState::new(&tmp);
        tree.toggle_dir(&sub);
        tree.toggle_dir(&gone_dir);
        tree.selected_path = Some(gone_dir.clone());
        assert!(tree.visible_entries.iter().any(|e| e.name == "existing.rs"));

        // External FS changes: add a file inside `sub`, remove `gone_dir`
        fs::write(sub.join("external_new.rs"), "// new\n").unwrap();
        fs::remove_dir_all(&gone_dir).unwrap();

        tree.refresh();
        assert!(tree.visible_entries.iter().any(|e| e.name == "external_new.rs"));
        assert!(!tree.visible_entries.iter().any(|e| e.name == "gone_dir"));
        assert!(!tree.expanded_dirs.contains(&gone_dir));
        assert!(tree.expanded_dirs.contains(&sub));
        assert!(tree.selected_path.is_none());

        let _ = fs::remove_dir_all(&tmp);
    }
}
