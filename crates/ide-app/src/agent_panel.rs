use crate::canvas::{FontStyle, Rect, SurfaceCanvas};
use crate::ide_state::{
    blend_rgb, write_windows_clipboard_text, FocusTarget, HitAction, IdeApplicationState,
    RightPanelTab, ScrollTrackInfo,
};
use ide_agent_hub::{detect_workspace_file_references, AgentAttachedItem, AgentWorkbenchMode};
use ide_core::{LanguageId, ThemeKind, UiLanguage};
use ide_git::DiffLineKind;
use ide_syntax::{highlight_line, TokenKind};
use std::path::PathBuf;

pub(crate) const AGENT_INPUT_FONT: FontStyle = FontStyle::ChatBody;
pub(crate) const AGENT_INPUT_LINE_H: i32 = 20;

#[derive(Debug, Clone)]
pub(crate) enum ChatSpanKind {
    Plain,
    Bold,
    InlineCode,
    FileLink {
        path: PathBuf,
        line: Option<usize>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ChatTextSpan {
    pub start_char: usize,
    pub end_char: usize,
    pub kind: ChatSpanKind,
}

#[derive(Debug, Clone)]
pub(crate) enum CachedChatRow {
    UserHeader {
        msg_idx: usize,
        bubble_w: i32,
    },
    UserBubbleTop {
        bubble_w: i32,
    },
    UserLine {
        text: String,
        spans: Vec<ChatTextSpan>,
        default_bold: bool,
        bubble_w: i32,
    },
    UserFilesRow {
        files: Vec<(PathBuf, String)>,
        bubble_w: i32,
    },
    UserImageCard {
        item: AgentAttachedItem,
        bubble_w: i32,
    },
    UserBubbleBottom {
        bubble_w: i32,
    },
    AgentHeader {
        msg_idx: usize,
        agent_name: String,
    },
    ThoughtToggle {
        msg_idx: usize,
        preview: String,
        expanded: bool,
    },
    ThoughtLine(String),
    AgentLine {
        text: String,
        spans: Vec<ChatTextSpan>,
        default_bold: bool,
    },
    ParaSpacer {
        is_user: bool,
        bubble_w: i32,
    },
    CodeBlockHeader {
        msg_idx: usize,
        blk_idx: usize,
        lang: String,
        code: String,
        line_count: usize,
        collapsed: bool,
    },
    CodeBlockCollapsedSummary {
        msg_idx: usize,
        blk_idx: usize,
        preview: String,
        line_count: usize,
    },
    CodeBlockLine {
        text: String,
        lang: LanguageId,
    },
    ModifiedFilesHeader(usize),
    ModifiedFileChip {
        path: PathBuf,
        display: String,
    },
    StreamingLine {
        text: String,
        spans: Vec<ChatTextSpan>,
    },
    PatchHeader {
        summary: String,
        applied: bool,
    },
    PatchDiffLine {
        kind: DiffLineKind,
        text: String,
    },
    PatchButtons {
        msg_idx: usize,
        applied: bool,
    },
    BusyIndicator,
    Spacer,
}

impl CachedChatRow {
    pub(crate) fn row_height(&self) -> i32 {
        match self {
            Self::UserHeader { .. } => 21,
            Self::AgentHeader { .. } => 24,
            Self::UserBubbleTop { .. } | Self::UserBubbleBottom { .. } => 7,
            Self::UserFilesRow { .. } => 26,
            Self::UserImageCard { .. } => 86,
            Self::ThoughtToggle { .. } => 22,
            Self::ThoughtLine(_) => 19,
            Self::CodeBlockHeader { .. } => 26,
            Self::CodeBlockCollapsedSummary { .. } => 24,
            Self::CodeBlockLine { .. } => 19,
            Self::ModifiedFilesHeader(_) => 22,
            Self::ModifiedFileChip { .. } => 24,
            Self::PatchHeader { .. } | Self::PatchButtons { .. } => 24,
            Self::PatchDiffLine { .. } => 20,
            Self::BusyIndicator => 26,
            Self::ParaSpacer { .. } => 9,
            Self::Spacer => 14,
            Self::UserLine { .. } | Self::AgentLine { .. } | Self::StreamingLine { .. } => 23,
        }
    }

    pub(crate) fn copyable_text(&self) -> Option<&str> {
        match self {
            Self::UserLine { text, .. }
            | Self::AgentLine { text, .. }
            | Self::ThoughtLine(text)
            | Self::StreamingLine { text, .. }
            | Self::CodeBlockLine { text, .. } => Some(text.as_str()),
            Self::PatchDiffLine { text, .. } => Some(text.as_str()),
            Self::ModifiedFileChip { display, .. } => Some(display.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AgentChatCache {
    pub session_id: String,
    pub msg_count: usize,
    pub signature: u64,
    pub wrap_w: i32,
    pub scale_bits: u32,
    pub is_busy: bool,
    pub rows: Vec<CachedChatRow>,
    pub row_tops: Vec<i32>,
    pub total_height: i32,
}

impl Default for AgentChatCache {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            msg_count: usize::MAX,
            signature: 0,
            wrap_w: 0,
            scale_bits: 0,
            is_busy: false,
            rows: Vec::new(),
            row_tops: Vec::new(),
            total_height: 0,
        }
    }
}

impl AgentChatCache {
    fn recompute_offsets(&mut self) {
        self.row_tops.clear();
        self.row_tops.reserve(self.rows.len());
        let mut y = 0i32;
        for r in &self.rows {
            self.row_tops.push(y);
            y += r.row_height();
        }
        self.total_height = y;
    }

    /// 计算将底部最后一行对齐至 `viewport_h` 所需的最大起始行索引
    fn max_scroll_row_for_height(&self, viewport_h: i32) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        let mut acc = 0i32;
        let mut start = self.rows.len() - 1;
        while start > 0 {
            let h = self.rows[start].row_height();
            if acc + h > viewport_h {
                break;
            }
            acc += h;
            let prev_h = self.rows[start - 1].row_height();
            if acc + prev_h > viewport_h {
                break;
            }
            start -= 1;
        }
        start
    }

    fn visible_rows_from(&self, start_row: usize, viewport_h: i32) -> usize {
        if start_row >= self.rows.len() {
            return 1;
        }
        let mut acc = 0i32;
        let mut cnt = 0usize;
        for r in &self.rows[start_row..] {
            let rh = r.row_height();
            if acc > 0 && acc + rh > viewport_h + 4 {
                break;
            }
            acc += rh;
            cnt += 1;
        }
        cnt.max(1)
    }
}

fn language_from_fence_tag(tag: &str) -> LanguageId {
    match tag.trim().to_ascii_lowercase().as_str() {
        "rs" | "rust" => LanguageId::Rust,
        "ts" | "tsx" | "typescript" => LanguageId::TypeScript,
        "js" | "jsx" | "mjs" | "cjs" | "javascript" => LanguageId::JavaScript,
        "py" | "python" => LanguageId::Python,
        "json" => LanguageId::Json,
        "toml" => LanguageId::Toml,
        "md" | "markdown" => LanguageId::Markdown,
        _ => LanguageId::PlainText,
    }
}

fn parse_markdown_inline_line(raw: &str) -> (String, bool, Vec<ChatTextSpan>) {
    let trimmed_start = raw.trim_start();
    let mut line_bold = false;
    let mut working = raw.to_string();

    if let Some(after_hash) = trimmed_start.strip_prefix("#### ") {
        line_bold = true;
        working = after_hash.to_string();
    } else if let Some(after_hash) = trimmed_start.strip_prefix("### ") {
        line_bold = true;
        working = after_hash.to_string();
    } else if let Some(after_hash) = trimmed_start.strip_prefix("## ") {
        line_bold = true;
        working = after_hash.to_string();
    } else if let Some(after_hash) = trimmed_start.strip_prefix("# ") {
        line_bold = true;
        working = after_hash.to_string();
    } else if let Some(rest) = trimmed_start.strip_prefix("- ") {
        let indent = raw.len() - trimmed_start.len();
        working = format!("{}• {}", &raw[..indent], rest);
    } else if let Some(rest) = trimmed_start.strip_prefix("* ") {
        let indent = raw.len() - trimmed_start.len();
        working = format!("{}• {}", &raw[..indent], rest);
    }

    let chars: Vec<char> = working.chars().collect();
    let mut clean = String::with_capacity(working.len());
    let mut spans = Vec::new();
    let mut clean_len = 0usize;
    let mut i = 0usize;

    while i < chars.len() {
        if chars[i] == '`' {
            if let Some(rel_end) = chars[i + 1..].iter().position(|&c| c == '`') {
                if rel_end > 0 {
                    let start = clean_len;
                    for &c in &chars[i + 1..i + 1 + rel_end] {
                        clean.push(c);
                        clean_len += 1;
                    }
                    spans.push(ChatTextSpan {
                        start_char: start,
                        end_char: clean_len,
                        kind: ChatSpanKind::InlineCode,
                    });
                    i += rel_end + 2;
                    continue;
                }
            }
        }
        if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '*' {
            let mut close_pos = None;
            let mut j = i + 2;
            while j + 1 < chars.len() {
                if chars[j] == '*' && chars[j + 1] == '*' {
                    close_pos = Some(j);
                    break;
                }
                j += 1;
            }
            if let Some(end_idx) = close_pos {
                if end_idx > i + 2 {
                    let start = clean_len;
                    for &c in &chars[i + 2..end_idx] {
                        clean.push(c);
                        clean_len += 1;
                    }
                    spans.push(ChatTextSpan {
                        start_char: start,
                        end_char: clean_len,
                        kind: ChatSpanKind::Bold,
                    });
                    i = end_idx + 2;
                    continue;
                }
            }
        }
        clean.push(chars[i]);
        clean_len += 1;
        i += 1;
    }

    (clean, line_bold, spans)
}

/// 按像素宽度对聊天文本折行，优先在英文空格/标点词边界断开，并返回每行在原行中的字符区间
fn wrap_chat_line_segments(
    canvas: &SurfaceCanvas,
    line: &str,
    max_width: i32,
    style: FontStyle,
) -> Vec<(usize, usize, String)> {
    let max_w = max_width.max(36);
    if line.is_empty() {
        return vec![(0, 0, String::new())];
    }
    let total_chars = line.chars().count();
    let (full_w, _) = canvas.measure_text(line, style);
    if full_w <= max_w {
        return vec![(0, total_chars, line.to_string())];
    }

    let char_bytes: Vec<usize> = line
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(line.len()))
        .collect();
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut start_c = 0usize;

    while start_c < total_chars {
        let rem = &line[char_bytes[start_c]..];
        let (rw, _) = canvas.measure_text(rem, style);
        if rw <= max_w {
            out.push((start_c, total_chars, rem.to_string()));
            break;
        }
        let mut split_rel = canvas.measure_caret_col_in_line(rem, max_w, style).max(1);
        let rem_chars = total_chars - start_c;
        if split_rel >= rem_chars {
            split_rel = rem_chars.saturating_sub(1).max(1);
        }
        while split_rel > 1 {
            let head = &line[char_bytes[start_c]..char_bytes[start_c + split_rel]];
            let (hw, _) = canvas.measure_text(head, style);
            if hw <= max_w {
                break;
            }
            split_rel -= 1;
        }
        // 优先在末尾 ~35% 范围的空白处断词，避免切断英文单词
        let min_word_break = (split_rel * 62) / 100;
        let mut chosen_rel = split_rel;
        for cand in (min_word_break..=split_rel).rev() {
            if cand > 0 && chars[start_c + cand - 1] == ' ' {
                chosen_rel = cand;
                break;
            }
        }
        let end_c = start_c + chosen_rel;
        let seg = line[char_bytes[start_c]..char_bytes[end_c]].to_string();
        out.push((start_c, end_c, seg));
        start_c = end_c;
    }
    out
}

fn project_and_merge_spans(
    root: &std::path::Path,
    seg_text: &str,
    seg_start_char: usize,
    seg_end_char: usize,
    raw_spans: &[ChatTextSpan],
) -> Vec<ChatTextSpan> {
    let seg_len = seg_end_char.saturating_sub(seg_start_char);
    let mut spans = Vec::new();
    for sp in raw_spans {
        let ov_s = sp.start_char.max(seg_start_char);
        let ov_e = sp.end_char.min(seg_end_char);
        if ov_e > ov_s {
            spans.push(ChatTextSpan {
                start_char: ov_s - seg_start_char,
                end_char: ov_e - seg_start_char,
                kind: sp.kind.clone(),
            });
        }
    }

    let refs = detect_workspace_file_references(root, seg_text);
    for (tok, path, line_opt) in refs {
        if let Some(byte_pos) = seg_text.find(&tok) {
            let start_char = seg_text[..byte_pos].chars().count();
            let end_char = (start_char + tok.chars().count()).min(seg_len);
            if end_char <= start_char {
                continue;
            }
            // FileLink 优先替换同区间或重叠的 InlineCode/Bold，确保路径可点击打开
            spans.retain(|s| s.end_char <= start_char || s.start_char >= end_char);
            spans.push(ChatTextSpan {
                start_char,
                end_char,
                kind: ChatSpanKind::FileLink {
                    path,
                    line: line_opt,
                },
            });
        }
    }

    spans.sort_by_key(|s| s.start_char);
    spans
}

fn span_visual_metrics(kind: &ChatSpanKind, default_bold: bool) -> (FontStyle, i32, i32) {
    match kind {
        ChatSpanKind::Plain => (
            if default_bold {
                FontStyle::ChatBodyBold
            } else {
                FontStyle::ChatBody
            },
            0,
            0,
        ),
        ChatSpanKind::Bold => (FontStyle::ChatBodyBold, 0, 0),
        ChatSpanKind::InlineCode => (FontStyle::ChatCode, 6, 6),
        ChatSpanKind::FileLink { .. } => (FontStyle::ChatCode, 4, 4),
    }
}

fn build_normalized_segments(
    char_len: usize,
    spans: &[ChatTextSpan],
) -> Vec<(usize, usize, ChatSpanKind)> {
    let mut out = Vec::new();
    let mut cur = 0usize;
    for sp in spans {
        let s = sp.start_char.min(char_len);
        let e = sp.end_char.min(char_len);
        if s > cur {
            out.push((cur, s, ChatSpanKind::Plain));
        }
        if e > s {
            out.push((s, e, sp.kind.clone()));
            cur = e;
        }
    }
    if cur < char_len {
        out.push((cur, char_len, ChatSpanKind::Plain));
    }
    out
}

fn measure_chat_spans_x(
    canvas: &SurfaceCanvas,
    text: &str,
    spans: &[ChatTextSpan],
    default_bold: bool,
    target_col: usize,
) -> i32 {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let col = target_col.min(len);
    if col == 0 || len == 0 {
        return 0;
    }
    let segs = build_normalized_segments(len, spans);
    let mut x = 0i32;
    for (s, e, kind) in segs {
        if col <= s {
            break;
        }
        let (style, pad_l, pad_r) = span_visual_metrics(&kind, default_bold);
        if col >= e {
            let chunk: String = chars[s..e].iter().collect();
            let (cw, _) = canvas.measure_text(&chunk, style);
            x += pad_l + cw + pad_r;
        } else {
            let chunk: String = chars[s..col].iter().collect();
            let (cw, _) = canvas.measure_text(&chunk, style);
            x += pad_l + cw;
            break;
        }
    }
    x
}

fn hit_test_chat_spans_col(
    canvas: &SurfaceCanvas,
    text: &str,
    spans: &[ChatTextSpan],
    default_bold: bool,
    rel_x: i32,
) -> usize {
    if rel_x <= 0 || text.is_empty() {
        return 0;
    }
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let segs = build_normalized_segments(len, spans);
    let mut x = 0i32;
    for (s, e, kind) in segs {
        let (style, pad_l, pad_r) = span_visual_metrics(&kind, default_bold);
        let chunk: String = chars[s..e].iter().collect();
        let (cw, _) = canvas.measure_text(&chunk, style);
        let seg_total = pad_l + cw + pad_r;
        if rel_x <= x + seg_total {
            let local_x = (rel_x - x - pad_l).max(0);
            let local_col = canvas.measure_caret_col_in_line(&chunk, local_x, style);
            return (s + local_col).min(len);
        }
        x += seg_total;
    }
    len
}

/// 将包含 Markdown 代码块与行内格式的聊天消息拆分为美观排版的行
fn append_message_body_rows(
    root: &std::path::Path,
    canvas: &SurfaceCanvas,
    msg_idx: usize,
    content: &str,
    wrap_w: i32,
    is_user: bool,
    bubble_w: i32,
    collapse_overrides: &std::collections::HashMap<(usize, usize), bool>,
    out: &mut Vec<CachedChatRow>,
) {
    let mut in_fence = false;
    let mut fence_lang = String::new();
    let mut code_lines: Vec<String> = Vec::new();
    let mut prose_buf: Vec<&str> = Vec::new();
    let mut blk_idx = 0usize;

    let flush_prose = |prose: &mut Vec<&str>, out: &mut Vec<CachedChatRow>| {
        if prose.is_empty() {
            return;
        }
        for raw_line in prose.drain(..) {
            if raw_line.trim().is_empty() {
                if !out.is_empty() {
                    out.push(CachedChatRow::ParaSpacer { is_user, bubble_w });
                }
                continue;
            }
            let (clean_line, line_bold, raw_spans) = parse_markdown_inline_line(raw_line);
            let inline_code_cnt = raw_spans
                .iter()
                .filter(|s| matches!(s.kind, ChatSpanKind::InlineCode))
                .count();
            let pill_reserve = (inline_code_cnt as i32 * 12).min(48);
            let base_style = if line_bold {
                FontStyle::ChatBodyBold
            } else {
                FontStyle::ChatBody
            };
            let eff_wrap_w = (wrap_w - pill_reserve).max(60);
            for (seg_s, seg_e, seg_txt) in
                wrap_chat_line_segments(canvas, &clean_line, eff_wrap_w, base_style)
            {
                let spans = project_and_merge_spans(root, &seg_txt, seg_s, seg_e, &raw_spans);
                if is_user {
                    out.push(CachedChatRow::UserLine {
                        text: seg_txt,
                        spans,
                        default_bold: line_bold,
                        bubble_w,
                    });
                } else {
                    out.push(CachedChatRow::AgentLine {
                        text: seg_txt,
                        spans,
                        default_bold: line_bold,
                    });
                }
            }
        }
    };

    let push_code_block = |tag: String,
                           lines: Vec<String>,
                           cur_blk_idx: usize,
                           out: &mut Vec<CachedChatRow>| {
        let full_code = lines.join("\n");
        let line_count = lines.len().max(1);
        let default_collapsed = is_user || line_count >= 28;
        let collapsed = collapse_overrides
            .get(&(msg_idx, cur_blk_idx))
            .copied()
            .unwrap_or(default_collapsed);
        let lang_label = if tag.is_empty() {
            "code".to_string()
        } else {
            tag.clone()
        };
        out.push(CachedChatRow::CodeBlockHeader {
            msg_idx,
            blk_idx: cur_blk_idx,
            lang: lang_label,
            code: full_code,
            line_count,
            collapsed,
        });
        if collapsed {
            let preview = lines
                .iter()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .unwrap_or("...")
                .to_string();
            out.push(CachedChatRow::CodeBlockCollapsedSummary {
                msg_idx,
                blk_idx: cur_blk_idx,
                preview,
                line_count,
            });
        } else {
            let lang_id = language_from_fence_tag(&tag);
            for cl in &lines {
                for (_, _, wl) in
                    wrap_chat_line_segments(canvas, cl, (wrap_w - 20).max(40), FontStyle::ChatCode)
                {
                    out.push(CachedChatRow::CodeBlockLine {
                        text: wl,
                        lang: lang_id,
                    });
                }
            }
        }
    };

    for raw in content.lines() {
        let trimmed = raw.trim_start();
        if !in_fence {
            if let Some(after) = trimmed.strip_prefix("```") {
                flush_prose(&mut prose_buf, out);
                in_fence = true;
                fence_lang = after.trim().to_string();
                code_lines.clear();
            } else {
                prose_buf.push(raw);
            }
        } else if trimmed.starts_with("```") {
            let taken = std::mem::take(&mut code_lines);
            let tag = std::mem::take(&mut fence_lang);
            push_code_block(tag, taken, blk_idx, out);
            blk_idx += 1;
            in_fence = false;
        } else {
            code_lines.push(raw.to_string());
        }
    }

    if in_fence && !code_lines.is_empty() {
        push_code_block(fence_lang, code_lines, blk_idx, out);
    } else {
        flush_prose(&mut prose_buf, out);
    }
}

fn measure_user_bubble_width(
    root: &std::path::Path,
    canvas: &SurfaceCanvas,
    msg: &ide_agent_hub::AgentChatMessage,
    max_content_w: i32,
) -> i32 {
    let mut max_w = 0i32;
    let mut in_fence = false;
    for raw in msg.content.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            max_w = max_w.max(max_content_w);
            continue;
        }
        if in_fence || raw.trim().is_empty() {
            continue;
        }
        let (clean, line_bold, raw_spans) = parse_markdown_inline_line(raw);
        let inline_code_cnt = raw_spans
            .iter()
            .filter(|s| matches!(s.kind, ChatSpanKind::InlineCode))
            .count();
        let pill_reserve = (inline_code_cnt as i32 * 12).min(48);
        let base_style = if line_bold {
            FontStyle::ChatBodyBold
        } else {
            FontStyle::ChatBody
        };
        let eff_wrap = (max_content_w - pill_reserve).max(60);
        for (seg_s, seg_e, seg_txt) in
            wrap_chat_line_segments(canvas, &clean, eff_wrap, base_style)
        {
            let spans = project_and_merge_spans(root, &seg_txt, seg_s, seg_e, &raw_spans);
            let w = measure_chat_spans_x(canvas, &seg_txt, &spans, line_bold, usize::MAX);
            max_w = max_w.max(w);
        }
    }
    if !msg.attachments.is_empty() {
        max_w = max_w.max(210.min(max_content_w));
    }
    (max_w + 26).clamp(86, max_content_w + 26)
}

impl IdeApplicationState {
    pub(crate) fn agent_chat_selection_ordered(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.agent_chat_sel_anchor?;
        let c = self.agent_chat_sel_cursor?;
        if a == c {
            return None;
        }
        if a <= c {
            Some((a, c))
        } else {
            Some((c, a))
        }
    }

    pub(crate) fn copy_agent_chat_selection(&self) -> Option<String> {
        let ((sr, sc), (er, ec)) = self.agent_chat_selection_ordered()?;
        let rows = &self.cached_chat_rows.rows;
        if sr >= rows.len() {
            return None;
        }
        let end_r = er.min(rows.len() - 1);
        let mut parts = Vec::new();
        for r in sr..=end_r {
            let txt = rows[r].copyable_text().unwrap_or("");
            let chars: Vec<char> = txt.chars().collect();
            let c0 = if r == sr { sc.min(chars.len()) } else { 0 };
            let c1 = if r == end_r {
                ec.min(chars.len())
            } else {
                chars.len()
            };
            if c1 > c0 || (r > sr && r < end_r) {
                parts.push(chars[c0..c1].iter().collect::<String>());
            }
        }
        let joined = parts.join("\n");
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    }

    pub(crate) fn copy_all_agent_chat(&self) -> String {
        self.agent_hub
            .messages
            .iter()
            .map(|m| format!("[{}]\n{}", m.agent.short_name(), m.content.trim()))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
    }

    pub(crate) fn select_all_agent_chat(&mut self) -> bool {
        let rows = &self.cached_chat_rows.rows;
        if rows.is_empty() {
            return false;
        }
        let last_idx = rows.len() - 1;
        let last_len = rows[last_idx]
            .copyable_text()
            .map(|s| s.chars().count())
            .unwrap_or(0);
        self.agent_chat_sel_anchor = Some((0, 0));
        self.agent_chat_sel_cursor = Some((last_idx, last_len));
        self.focus = FocusTarget::AgentChatStream;
        true
    }

    pub(crate) fn clear_agent_chat_selection(&mut self) -> bool {
        let had = self.agent_chat_sel_anchor.is_some() || self.agent_chat_sel_cursor.is_some();
        self.agent_chat_sel_anchor = None;
        self.agent_chat_sel_cursor = None;
        had
    }

    pub(crate) fn hit_test_agent_chat_stream(
        &self,
        x: i32,
        y: i32,
        canvas: &SurfaceCanvas,
    ) -> Option<(usize, usize)> {
        if self.agent_chat_line_rects.is_empty() {
            return None;
        }
        let target = self
            .agent_chat_line_rects
            .iter()
            .find(|(rect, _, _, _, _)| y >= rect.y && y < rect.y + rect.h)
            .or_else(|| {
                let first = self.agent_chat_line_rects.first()?;
                let last = self.agent_chat_line_rects.last()?;
                if y < first.0.y {
                    Some(first)
                } else if y >= last.0.y + last.0.h {
                    Some(last)
                } else {
                    None
                }
            })?;
        let (_, text_x, row_idx, line_txt, style) = target;
        let rel_x = (x - *text_x).max(0);
        let col = match self.cached_chat_rows.rows.get(*row_idx) {
            Some(CachedChatRow::UserLine {
                text,
                spans,
                default_bold,
                ..
            })
            | Some(CachedChatRow::AgentLine {
                text,
                spans,
                default_bold,
            }) => hit_test_chat_spans_col(canvas, text, spans, *default_bold, rel_x),
            Some(CachedChatRow::StreamingLine { text, spans }) => {
                hit_test_chat_spans_col(canvas, text, spans, false, rel_x)
            }
            _ => canvas.measure_caret_col_in_line(line_txt, rel_x, *style),
        };
        Some((*row_idx, col))
    }

    pub(crate) fn select_word_in_agent_chat(
        &mut self,
        x: i32,
        y: i32,
        canvas: &SurfaceCanvas,
    ) -> bool {
        let Some((row_idx, col)) = self.hit_test_agent_chat_stream(x, y, canvas) else {
            return false;
        };
        let Some(txt) = self
            .cached_chat_rows
            .rows
            .get(row_idx)
            .and_then(|r| r.copyable_text())
        else {
            return false;
        };
        let chars: Vec<char> = txt.chars().collect();
        if chars.is_empty() {
            return false;
        }
        let pos = col.min(chars.len().saturating_sub(1));
        let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\');
        if !is_word(chars[pos]) {
            self.agent_chat_sel_anchor = Some((row_idx, pos));
            self.agent_chat_sel_cursor = Some((row_idx, (pos + 1).min(chars.len())));
            return true;
        }
        let mut s = pos;
        while s > 0 && is_word(chars[s - 1]) {
            s -= 1;
        }
        let mut e = pos + 1;
        while e < chars.len() && is_word(chars[e]) {
            e += 1;
        }
        self.agent_chat_sel_anchor = Some((row_idx, s));
        self.agent_chat_sel_cursor = Some((row_idx, e));
        true
    }

    pub(crate) fn select_line_in_agent_chat(
        &mut self,
        x: i32,
        y: i32,
        canvas: &SurfaceCanvas,
    ) -> bool {
        let Some((row_idx, _)) = self.hit_test_agent_chat_stream(x, y, canvas) else {
            return false;
        };
        let len = self
            .cached_chat_rows
            .rows
            .get(row_idx)
            .and_then(|r| r.copyable_text())
            .map(|s| s.chars().count())
            .unwrap_or(0);
        self.agent_chat_sel_anchor = Some((row_idx, 0));
        self.agent_chat_sel_cursor = Some((row_idx, len));
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_selection_highlight_in_row(
        &self,
        canvas: &mut SurfaceCanvas,
        row_rect: Rect,
        text_x: i32,
        global_row_idx: usize,
        text: &str,
        spans: &[ChatTextSpan],
        default_bold: bool,
        max_w: i32,
        style: FontStyle,
    ) {
        let Some(((sr, sc), (er, ec))) = self.agent_chat_selection_ordered() else {
            return;
        };
        if global_row_idx < sr || global_row_idx > er {
            return;
        }
        let char_len = text.chars().count();
        let c0 = if global_row_idx == sr {
            sc.min(char_len)
        } else {
            0
        };
        let c1 = if global_row_idx == er {
            ec.min(char_len)
        } else {
            char_len
        };
        if c1 > c0 {
            let (w0, w1) = if !spans.is_empty() || matches!(style, FontStyle::ChatBody | FontStyle::ChatBodyBold) {
                (
                    measure_chat_spans_x(canvas, text, spans, default_bold, c0),
                    measure_chat_spans_x(canvas, text, spans, default_bold, c1),
                )
            } else {
                let chars: Vec<char> = text.chars().collect();
                let p0: String = chars[..c0].iter().collect();
                let p1: String = chars[..c1].iter().collect();
                (
                    canvas.measure_text(&p0, style).0,
                    canvas.measure_text(&p1, style).0,
                )
            };
            let sx = text_x + w0;
            let sw = (w1 - w0).max(4).min((text_x + max_w - sx).max(0));
            if sw > 0 {
                canvas.fill_rect(
                    Rect::new(sx, row_rect.y + 1, sw, row_rect.h - 2),
                    self.theme.editor_selection_bg,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_chat_line_with_spans_and_selection(
        &mut self,
        canvas: &mut SurfaceCanvas,
        row_rect: Rect,
        text_x: i32,
        text_y: i32,
        global_row_idx: usize,
        text: &str,
        spans: &[ChatTextSpan],
        default_bold: bool,
        base_color: u32,
        max_w: i32,
        fallback_style: FontStyle,
    ) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let base_style = if default_bold && matches!(fallback_style, FontStyle::ChatBody) {
            FontStyle::ChatBodyBold
        } else {
            fallback_style
        };
        self.agent_chat_line_rects.push((
            row_rect,
            text_x,
            global_row_idx,
            text.to_string(),
            base_style,
        ));
        self.draw_selection_highlight_in_row(
            canvas,
            row_rect,
            text_x,
            global_row_idx,
            text,
            spans,
            default_bold,
            max_w,
            base_style,
        );

        if spans.is_empty() {
            canvas.draw_text_styled(text_x, text_y, text, base_color, max_w, base_style);
            return;
        }

        let chars: Vec<char> = text.chars().collect();
        let segs = build_normalized_segments(chars.len(), spans);
        let mut cur_x = text_x;
        let right_edge = text_x + max_w;
        let (_, body_h) = canvas.measure_text("Ag中文", FontStyle::ChatBody);
        let (_, code_h) = canvas.measure_text("Ag", FontStyle::ChatCode);
        let code_y_off = ((body_h - code_h) / 2).max(0);
        let inline_pill_bg = if t.kind == ThemeKind::IslandsLight {
            blend_rgb(t.panel_bg, t.border_color, 0.35)
        } else {
            blend_rgb(t.panel_bg, t.window_bg, 0.22)
        };
        let inline_pill_border = blend_rgb(t.border_color, t.panel_bg, 0.45);
        let inline_code_fg = if t.kind == ThemeKind::IslandsLight {
            t.text_primary
        } else {
            blend_rgb(t.text_primary, 0xFFFFFF, 0.25)
        };

        for (s, e, kind) in segs {
            if cur_x >= right_edge {
                break;
            }
            let chunk: String = chars[s..e].iter().collect();
            if chunk.is_empty() {
                continue;
            }
            match kind {
                ChatSpanKind::Plain => {
                    let st = if default_bold {
                        FontStyle::ChatBodyBold
                    } else {
                        base_style
                    };
                    let (cw, _) = canvas.measure_text(&chunk, st);
                    canvas.draw_text_styled(cur_x, text_y, &chunk, base_color, right_edge - cur_x, st);
                    cur_x += cw;
                }
                ChatSpanKind::Bold => {
                    let (cw, _) = canvas.measure_text(&chunk, FontStyle::ChatBodyBold);
                    canvas.draw_text_styled(
                        cur_x,
                        text_y,
                        &chunk,
                        base_color,
                        right_edge - cur_x,
                        FontStyle::ChatBodyBold,
                    );
                    cur_x += cw;
                }
                ChatSpanKind::InlineCode => {
                    let (cw, _) = canvas.measure_text(&chunk, FontStyle::ChatCode);
                    let pill_w = (cw + 8).min((right_edge - cur_x - 2).max(10));
                    let pill_h = (code_h + 3).min(row_rect.h - 2);
                    let pill_rect = Rect::new(
                        cur_x + 2,
                        text_y + code_y_off - 1,
                        pill_w,
                        pill_h,
                    );
                    canvas.stroke_rounded_rect(pill_rect, 4, inline_pill_border, inline_pill_bg);
                    canvas.draw_text_styled(
                        pill_rect.x + 4,
                        text_y + code_y_off,
                        &chunk,
                        inline_code_fg,
                        (pill_w - 6).max(4),
                        FontStyle::ChatCode,
                    );
                    cur_x += cw + 12;
                }
                ChatSpanKind::FileLink { path, line } => {
                    let (lw, _) = canvas.measure_text(&chunk, FontStyle::ChatCode);
                    let pill_w = (lw + 6).min((right_edge - cur_x - 1).max(8));
                    let link_rect = Rect::new(
                        cur_x + 1,
                        text_y + code_y_off - 1,
                        pill_w,
                        (code_h + 3).min(row_rect.h - 2),
                    );
                    let hovered = link_rect.contains(hx, hy);
                    canvas.fill_rounded_rect(
                        link_rect,
                        4,
                        if hovered {
                            t.popup_hover_bg
                        } else {
                            inline_pill_bg
                        },
                    );
                    canvas.fill_rect(
                        Rect::new(
                            link_rect.x + 2,
                            link_rect.y + link_rect.h - 2,
                            (pill_w - 4).max(2),
                            1,
                        ),
                        if hovered { 0xFFFFFF } else { t.accent_blue },
                    );
                    canvas.draw_text_styled(
                        link_rect.x + 3,
                        text_y + code_y_off,
                        &chunk,
                        if hovered { 0xFFFFFF } else { t.syn_function },
                        (pill_w - 4).max(4),
                        FontStyle::ChatCode,
                    );
                    self.hit_regions
                        .push((link_rect, HitAction::OpenFileAtLine(path, line)));
                    cur_x += lw + 8;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_syntax_highlighted_chat_code_line(
        &mut self,
        canvas: &mut SurfaceCanvas,
        row_rect: Rect,
        text_x: i32,
        text_y: i32,
        global_row_idx: usize,
        text: &str,
        lang: LanguageId,
        max_w: i32,
    ) {
        let t = self.theme;
        self.agent_chat_line_rects.push((
            row_rect,
            text_x,
            global_row_idx,
            text.to_string(),
            FontStyle::ChatCode,
        ));
        self.draw_selection_highlight_in_row(
            canvas,
            row_rect,
            text_x,
            global_row_idx,
            text,
            &[],
            false,
            max_w,
            FontStyle::ChatCode,
        );

        let spans = highlight_line(text, lang);
        let right_edge = text_x + max_w;
        let mut cur_x = text_x;
        for span in spans {
            if cur_x >= right_edge {
                break;
            }
            let slice = span.text.as_str();
            if slice.is_empty() {
                continue;
            }
            let font_style = if span.kind == TokenKind::Keyword {
                FontStyle::ChatCodeBold
            } else {
                FontStyle::ChatCode
            };
            let next_x = canvas.draw_text_styled(
                cur_x,
                text_y,
                slice,
                span.kind.color(&t),
                right_edge - cur_x,
                font_style,
            );
            cur_x = next_x;
        }
    }

    /// 渲染右侧 AI 智能体岛屿工作台（现代简洁排版 · 多会话标签 · 右对齐用户气泡 · 行内代码胶囊 · CLI 自适应工作模式）
    pub(crate) fn render_right_agent_hub(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let panel_surface = if t.kind == ThemeKind::IslandsLight {
            t.panel_bg
        } else {
            blend_rgb(t.editor_bg, t.panel_bg, 0.25)
        };
        canvas.stroke_rounded_rect(rect, 8, t.border_color, panel_surface);
        self.agent_chat_line_rects.clear();

        // 顶部简洁头部底带
        let top_hdr = Rect::new(rect.x + 1, rect.y + 1, rect.w - 2, 32);
        canvas.fill_rounded_rect(
            top_hdr,
            7,
            blend_rgb(panel_surface, t.panel_bg, 0.55),
        );

        let is_chat = self.right_panel_tab == RightPanelTab::Chat;
        let chg_cnt = self.git_engine.changed_files.len();

        // 分段 Tab：[✦ AI 助手] [± 更改 (N)]
        let seg_bg = Rect::new(rect.x + 8, rect.y + 4, 192, 24);
        canvas.fill_rounded_rect(seg_bg, 6, blend_rgb(t.window_bg, panel_surface, 0.45));

        let tab1 = Rect::new(seg_bg.x + 2, seg_bg.y + 2, 84, 20);
        if is_chat {
            canvas.fill_rounded_rect(tab1, 5, blend_rgb(t.panel_bg, t.editor_bg, 0.35));
        }
        canvas.draw_ai_sparkle_icon(
            tab1.x + 6,
            tab1.y + 2,
            if is_chat { t.accent_blue } else { t.text_muted },
        );
        canvas.draw_text_styled(
            tab1.x + 23,
            tab1.y + 2,
            self.tr("AI 助手", "AI Chat"),
            if is_chat { t.text_primary } else { t.text_muted },
            58,
            if is_chat { FontStyle::ChatMetaBold } else { FontStyle::ChatMeta },
        );
        self.hit_regions
            .push((tab1, HitAction::SwitchRightPanelTab(RightPanelTab::Chat)));

        let tab2 = Rect::new(seg_bg.x + 88, seg_bg.y + 2, 102, 20);
        if !is_chat {
            canvas.fill_rounded_rect(tab2, 5, blend_rgb(t.panel_bg, t.editor_bg, 0.35));
        }
        canvas.draw_changes_diff_icon(
            tab2.x + 6,
            tab2.y + 2,
            if !is_chat { t.text_primary } else { t.text_muted },
        );
        canvas.draw_text_styled(
            tab2.x + 24,
            tab2.y + 2,
            &format!("{} ({chg_cnt})", self.tr("更改", "Changes")),
            if !is_chat { t.text_primary } else { t.text_muted },
            76,
            if !is_chat { FontStyle::ChatMetaBold } else { FontStyle::ChatMeta },
        );
        self.hit_regions.push((
            tab2,
            HitAction::SwitchRightPanelTab(RightPanelTab::Changes),
        ));

        // 右上角克制扁平动作按钮（默认无边框，悬停亮起）：[✦ 改写] [↻] [清空] [↶]
        let btn_hover_bg = blend_rgb(t.panel_bg, t.text_primary, 0.08);
        let rb_btn = Rect::new(rect.x + rect.w - 30, rect.y + 5, 23, 22);
        if rb_btn.contains(hx, hy) {
            canvas.fill_rounded_rect(rb_btn, 5, btn_hover_bg);
        }
        canvas.draw_rollback_icon(
            rb_btn.x + 4,
            rb_btn.y + 4,
            if rb_btn.contains(hx, hy) { t.warning_yellow } else { t.text_muted },
        );
        self.hit_regions
            .push((rb_btn, HitAction::RevertLastCheckpoint));

        let clr_btn = Rect::new(rb_btn.x - 26, rect.y + 5, 23, 22);
        if clr_btn.contains(hx, hy) {
            canvas.fill_rounded_rect(clr_btn, 5, btn_hover_bg);
        }
        canvas.draw_trash_icon(
            clr_btn.x + 4,
            clr_btn.y + 4,
            if clr_btn.contains(hx, hy) { t.text_primary } else { t.text_muted },
        );
        self.hit_regions.push((clr_btn, HitAction::ClearAgentChat));

        let cli_ready = self.agent_hub.providers.iter().any(|p| p.available);
        let sync_btn = Rect::new(clr_btn.x - 26, rect.y + 5, 23, 22);
        if sync_btn.contains(hx, hy) {
            canvas.fill_rounded_rect(sync_btn, 5, btn_hover_bg);
        }
        canvas.draw_refresh_icon(
            sync_btn.x + 4,
            sync_btn.y + 3,
            if sync_btn.contains(hx, hy) {
                t.accent_blue
            } else if cli_ready {
                t.run_green
            } else {
                t.text_muted
            },
        );
        self.hit_regions
            .push((sync_btn, HitAction::RefreshLocalCliAgents));

        let ck_btn = Rect::new(sync_btn.x - 56, rect.y + 5, 52, 22);
        if ck_btn.x > seg_bg.x + seg_bg.w + 4 {
            let ck_hov = ck_btn.contains(hx, hy);
            if ck_hov {
                canvas.fill_rounded_rect(ck_btn, 5, btn_hover_bg);
            }
            canvas.draw_ai_sparkle_icon(
                ck_btn.x + 4,
                ck_btn.y + 3,
                if ck_hov { t.syn_function } else { t.text_muted },
            );
            canvas.draw_text_styled(
                ck_btn.x + 20,
                ck_btn.y + 3,
                self.tr("改写", "Edit"),
                if ck_hov { t.text_primary } else { t.text_secondary },
                30,
                FontStyle::ChatMeta,
            );
            self.hit_regions
                .push((ck_btn, HitAction::TriggerCtrlKInlineEdit));
        }

        canvas.fill_rect(
            Rect::new(rect.x + 1, rect.y + 33, rect.w - 2, 1),
            blend_rgb(t.border_color, panel_surface, 0.55),
        );

        match self.right_panel_tab {
            RightPanelTab::Changes => {
                self.agent_chat_area_rect = Rect::new(0, 0, 0, 0);
                self.render_right_changes_tab(canvas, rect);
            }
            RightPanelTab::Chat => {
                self.render_right_chat_tab(canvas, rect);
            }
        }
    }

    fn render_right_changes_tab(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        let chg_cnt = self.git_engine.changed_files.len();
        let header_y = rect.y + 41;
        canvas.draw_text_styled(
            rect.x + 12,
            header_y,
            &format!(
                "{} {}  ·  {} {}",
                self.tr("对比", "Compare"),
                self.git_engine.current_branch,
                chg_cnt,
                self.tr("个变更文件", "changed files")
            ),
            t.text_secondary,
            rect.w - 86,
            FontStyle::UiSmall,
        );
        let ref_btn = Rect::new(rect.x + rect.w - 36, header_y - 2, 26, 22);
        canvas.fill_rounded_rect(ref_btn, 4, t.window_bg);
        canvas.draw_refresh_icon(ref_btn.x + 6, ref_btn.y + 4, t.text_secondary);
        self.hit_regions.push((ref_btn, HitAction::RefreshGitStatus));

        let list_top = header_y + 24;
        let list_h = (rect.y + rect.h - list_top - 8).max(24);
        let visible_rows = (list_h / 24).max(1) as usize;
        let files = self.git_engine.changed_files.clone();
        let total = files.len();
        let max_s = total.saturating_sub(visible_rows);
        if self.agent_scroll == usize::MAX || self.agent_scroll > max_s {
            self.agent_scroll = 0;
        }

        let mut y = list_top;
        for ch in files.iter().skip(self.agent_scroll).take(visible_rows) {
            let row = Rect::new(rect.x + 8, y, rect.w - 54, 22);
            let lang = ide_core::LanguageId::from_path(&ch.path);
            canvas.draw_file_type_icon(row.x + 4, y + 4, lang);
            let fname = ch
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file");
            let rel = ch
                .path
                .strip_prefix(&self.workspace_root)
                .unwrap_or(&ch.path)
                .to_string_lossy();
            let label = format!("{fname}  {rel}");
            let status_color = match ch.status {
                ide_git::GitFileStatusKind::Added | ide_git::GitFileStatusKind::Untracked => {
                    t.run_green
                }
                ide_git::GitFileStatusKind::Deleted => t.breakpoint_red,
                ide_git::GitFileStatusKind::Modified => t.syn_function,
            };
            canvas.draw_text_styled(
                row.x + 22,
                y + 3,
                &label,
                status_color,
                row.w - 28,
                FontStyle::UiSmall,
            );
            self.hit_regions
                .push((row, HitAction::OpenFile(ch.path.clone())));

            let undo_btn = Rect::new(rect.x + rect.w - 42, y + 1, 26, 20);
            canvas.draw_rollback_icon(undo_btn.x + 5, undo_btn.y + 3, t.text_muted);
            self.hit_regions
                .push((undo_btn, HitAction::RollbackGitFile(ch.path.clone())));
            y += 24;
        }

        let track = Rect::new(rect.x + rect.w - 9, list_top, 6, list_h);
        self.right_scroll_info = ScrollTrackInfo {
            track,
            visible: visible_rows,
            total,
        };
        canvas.draw_vertical_scrollbar(
            track,
            self.agent_scroll,
            visible_rows,
            total,
            t.border_color,
        );
    }

    fn render_right_chat_tab(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let panel_surface = if t.kind == ThemeKind::IslandsLight {
            t.panel_bg
        } else {
            blend_rgb(t.editor_bg, t.panel_bg, 0.25)
        };
        let pill_hover_bg = blend_rgb(t.panel_bg, t.text_primary, 0.08);

        // 0. 顶部简洁多会话栏：[● 会话1 ×] ... [+ 新会话] [▾ N]
        let sess_bar_y = rect.y + 37;
        let sess_bar_h = 26;
        let sess_strip = Rect::new(rect.x + 8, sess_bar_y, rect.w - 16, sess_bar_h);

        let all_btn_w = 40;
        let new_btn_w = 66;
        let all_btn = Rect::new(
            sess_strip.x + sess_strip.w - all_btn_w,
            sess_bar_y + 2,
            all_btn_w,
            22,
        );
        let new_btn = Rect::new(all_btn.x - new_btn_w - 4, sess_bar_y + 2, new_btn_w, 22);

        let sessions_cnt = self.agent_hub.sessions.len();
        let active_sess_idx = self.agent_hub.active_session_idx;
        let new_hov = new_btn.contains(hx, hy);
        canvas.fill_rounded_rect(
            new_btn,
            5,
            if new_hov {
                blend_rgb(panel_surface, t.accent_blue, 0.22)
            } else {
                blend_rgb(panel_surface, t.text_primary, 0.05)
            },
        );
        canvas.draw_text_centered(
            new_btn,
            self.tr("+ 新会话", "+ New"),
            if new_hov { t.text_primary } else { t.accent_blue },
            FontStyle::ChatMetaBold,
        );
        self.hit_regions.push((new_btn, HitAction::NewAgentSession));

        let all_hov = all_btn.contains(hx, hy) || self.agent_sessions_popup_open;
        if all_hov {
            canvas.fill_rounded_rect(all_btn, 5, pill_hover_bg);
        }
        canvas.draw_text_centered(
            all_btn,
            &format!("▾ {sessions_cnt}"),
            if all_hov { t.text_primary } else { t.text_muted },
            FontStyle::ChatMeta,
        );
        self.hit_regions
            .push((all_btn, HitAction::ToggleAgentSessionsPopup));

        // 水平渲染近期会话 Tabs（极简无重边框风格）
        let tabs_avail_w = (new_btn.x - sess_strip.x - 6).max(40);
        let max_tabs_fit = (tabs_avail_w / 84).max(1) as usize;
        let window_start = if active_sess_idx >= max_tabs_fit {
            active_sess_idx + 1 - max_tabs_fit
        } else {
            0
        };
        let sess_meta: Vec<(usize, String, bool)> = self
            .agent_hub
            .sessions
            .iter()
            .enumerate()
            .skip(window_start)
            .take(max_tabs_fit)
            .map(|(i, s)| (i, s.title.clone(), s.is_busy))
            .collect();
        let mut sx = sess_strip.x;
        for (idx, title, sess_busy) in sess_meta {
            let is_cur = idx == active_sess_idx;
            let (tw, _) = canvas.measure_text(&title, FontStyle::ChatMeta);
            let tab_w = (tw + 32).clamp(64, (tabs_avail_w - (sx - sess_strip.x)).max(64).min(138));
            if sx + tab_w > new_btn.x - 2 && idx != active_sess_idx {
                break;
            }
            let tab_rect = Rect::new(sx, sess_bar_y + 2, tab_w.min(new_btn.x - sx - 2), 22);
            if tab_rect.w < 38 {
                break;
            }
            let hov = tab_rect.contains(hx, hy);
            if is_cur {
                canvas.fill_rounded_rect(
                    tab_rect,
                    5,
                    blend_rgb(panel_surface, t.text_primary, 0.07),
                );
            } else if hov {
                canvas.fill_rounded_rect(tab_rect, 5, pill_hover_bg);
            }

            if sess_busy {
                canvas.draw_subtle_spinner(
                    tab_rect.x + 9,
                    tab_rect.y + 11,
                    3.4,
                    self.spinner_phase(),
                    t.accent_blue,
                    t.border_color,
                    1.0,
                );
            } else {
                canvas.fill_circle(
                    tab_rect.x + 9,
                    tab_rect.y + 11,
                    2,
                    if is_cur { t.run_green } else { t.text_muted },
                );
            }
            canvas.draw_text_styled(
                tab_rect.x + 15,
                tab_rect.y + 3,
                &title,
                if is_cur { t.text_primary } else { t.text_muted },
                (tab_rect.w - 32).max(16),
                if is_cur { FontStyle::ChatMetaBold } else { FontStyle::ChatMeta },
            );
            self.hit_regions
                .push((tab_rect, HitAction::SelectAgentSession(idx)));

            if sessions_cnt > 1 || is_cur {
                let close_r = Rect::new(tab_rect.x + tab_rect.w - 15, tab_rect.y + 4, 13, 14);
                let chov = close_r.contains(hx, hy);
                canvas.draw_text_centered(
                    close_r,
                    "×",
                    if chov { t.breakpoint_red } else { t.text_muted },
                    FontStyle::ChatMeta,
                );
                self.hit_regions
                    .push((close_r, HitAction::CloseAgentSession(idx)));
            }
            sx += tab_rect.w + 4;
        }

        canvas.fill_rect(
            Rect::new(rect.x + 10, sess_bar_y + sess_bar_h + 1, rect.w - 20, 1),
            blend_rgb(t.border_color, panel_surface, 0.65),
        );

        // 1. 动态计算底部 Composer 高度（根据已附加图片缩略图、文件芯片与折叠代码抽屉自适应）
        let attached_items = self.agent_hub.active_session().attached_items.clone();
        let pasted_snippets = self.agent_hub.active_session().pasted_snippets.clone();
        let has_images_in_tray = attached_items.iter().any(|it| it.is_image);
        let has_tray = !attached_items.is_empty() || !pasted_snippets.is_empty();
        let tray_h = if has_images_in_tray {
            58
        } else if has_tray {
            28
        } else {
            0
        };
        let expanded_snip = self.expanded_composer_snippet_id.as_ref().and_then(|eid| {
            pasted_snippets.iter().find(|s| &s.id == eid).cloned()
        });
        let drawer_h = if let Some(ref snip) = expanded_snip {
            let vis = snip.line_count.clamp(2, 5) as i32;
            24 + vis * 16 + 6
        } else {
            0
        };
        let composer_h = 125 + tray_h + drawer_h;
        let composer_rect = Rect::new(
            rect.x + 10,
            rect.y + rect.h - composer_h - 10,
            rect.w - 20,
            composer_h,
        );

        // 2. 中部可滚动对话区（去掉多余常驻快捷方块，保持充足阅读空间）
        let chat_top = sess_bar_y + sess_bar_h + 6;
        let chat_h = (composer_rect.y - chat_top - 8).max(40);
        let chat_area = Rect::new(rect.x + 8, chat_top, rect.w - 16, chat_h);
        self.agent_chat_area_rect = chat_area;
        let wrap_w = (chat_area.w - 28).max(80);

        self.hit_regions
            .push((chat_area, HitAction::FocusAgentChatStream));

        let is_busy = self.agent_hub.is_active_session_busy();
        let stream_preview = self.agent_hub.streaming_preview();
        let stream_len = stream_preview.as_ref().map(|s| s.len()).unwrap_or(0);
        let msg_count = self.agent_hub.messages.len();
        let session_id = self.agent_hub.active_session().id.clone();

        // 若消息包含图片，确保在缓存构建/渲染前加载图像解码
        let image_paths: Vec<PathBuf> = self
            .agent_hub
            .messages
            .iter()
            .flat_map(|m| m.attachments.iter().filter(|a| a.is_image).map(|a| a.path.clone()))
            .collect();
        for img_p in image_paths {
            let _ = self.get_or_load_image_preview(&img_p);
        }

        let applied_sig: u64 = self
            .agent_hub
            .messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let p_bit = m
                    .patch
                    .as_ref()
                    .map(|p| if p.applied { 1u64 << (i & 31) } else { 0 })
                    .unwrap_or(0);
                let mf_bit = (m.modified_files.len() as u64).wrapping_mul(131);
                let att_bit = (m.attachments.len() as u64).wrapping_mul(977);
                p_bit ^ mf_bit ^ att_bit
            })
            .fold(self.agent_chat_signature_salt, |acc, x| acc.wrapping_add(x));
        let scale_bits = canvas.scale_factor.to_bits();

        if msg_count == 0 && !is_busy {
            self.cached_chat_rows = AgentChatCache {
                session_id,
                msg_count: 0,
                signature: applied_sig,
                wrap_w,
                scale_bits,
                is_busy: false,
                rows: Vec::new(),
                row_tops: Vec::new(),
                total_height: 0,
            };
            self.right_scroll_info = ScrollTrackInfo {
                track: Rect::new(rect.x + rect.w - 9, chat_top, 6, chat_h),
                visible: 1,
                total: 0,
            };
            self.render_agent_welcome_card(canvas, chat_area);
        } else {
            self.render_agent_chat_rows(
                canvas,
                rect,
                chat_area,
                wrap_w,
                &session_id,
                msg_count,
                applied_sig,
                scale_bits,
                is_busy,
                stream_preview,
                stream_len,
            );
        }

        // 3. 绘制美观的底部 Composer 卡片（含附件托盘、图片直接缩略图预览、可折叠粘贴代码预览）
        self.render_agent_composer(
            canvas,
            composer_rect,
            &attached_items,
            &pasted_snippets,
            expanded_snip.as_ref(),
            tray_h,
            drawer_h,
            is_busy,
        );

        // 4. 若外部资源管理器或内部文件树正在向 AI 面板拖拽文件/图片，渲染高质感虚线投放指引
        let drag_hint = self
            .active_ide_drag
            .as_ref()
            .filter(|(_, (dx, dy))| rect.contains(*dx, *dy))
            .map(|(p, _)| p.clone())
            .or_else(|| self.external_drag_hover.as_ref().map(|(p, _)| p.clone()));
        if let Some(drag_path) = drag_hint {
            let dz = Rect::new(
                chat_area.x + 4,
                chat_area.y + 4,
                chat_area.w - 8,
                (composer_rect.y - chat_area.y - 10).max(52),
            );
            canvas.fill_rounded_rect_alpha(dz, 8, blend_rgb(t.editor_bg, t.accent_blue, 0.25), 185);
            canvas.draw_dashed_rounded_border(dz, 8, t.accent_blue);
            let fname = drag_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("item");
            let label = if crate::image_preview::is_image_file_path(&drag_path) {
                format!(
                    "🖼 {} `{fname}`",
                    self.tr("释放以添加图片并直接预览:", "Drop image for preview:")
                )
            } else {
                format!(
                    "📎 {} `{fname}`",
                    self.tr("释放以附加到当前对话:", "Drop file to attach:")
                )
            };
            canvas.draw_text_centered(dz, &label, 0xFFFFFF, FontStyle::ChatBodyBold);
        }
    }

    fn render_agent_welcome_card(&mut self, canvas: &mut SurfaceCanvas, area: Rect) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        if area.h < 90 || area.w < 160 {
            return;
        }
        let card_w = (area.w - 20).clamp(180, 410);
        let card_h = (area.h - 24).clamp(130, 310);
        let card_x = area.x + (area.w - card_w) / 2;
        let card_y = area.y + ((area.h - card_h) / 2).max(6);
        let card = Rect::new(card_x, card_y, card_w, card_h);

        canvas.stroke_rounded_rect(
            card,
            10,
            blend_rgb(t.border_color, t.panel_bg, 0.45),
            blend_rgb(t.editor_bg, t.panel_bg, 0.42),
        );

        let emblem = Rect::new(card.x + card.w / 2 - 16, card.y + 16, 32, 32);
        canvas.fill_rounded_rect(emblem, 9, blend_rgb(t.editor_bg, t.accent_blue, 0.20));
        canvas.draw_agent_cube_icon(emblem.x + 9, emblem.y + 9, t.accent_blue);

        canvas.draw_text_centered(
            Rect::new(card.x + 12, card.y + 54, card.w - 24, 22),
            self.tr("新 AI 会话已就绪", "New AI Session Ready"),
            t.text_primary,
            FontStyle::ChatBodyBold,
        );

        let cli_short = self.agent_hub.active_provider.compact_name();
        let model = self.agent_hub.active_model();
        canvas.draw_text_centered(
            Rect::new(card.x + 12, card.y + 76, card.w - 24, 18),
            &format!("{cli_short}  ·  {model}  ·  {}", self.agent_mode.label()),
            t.text_muted,
            FontStyle::ChatMeta,
        );

        let starters: [(&'static str, &'static str, &'static str); 4] = match self.ui_lang {
            UiLanguage::ZhCn => [
                ("✦ 解释当前代码", "分析核心流程", "请解释当前光标所在代码块的功能与关键逻辑"),
                ("⚡ 生成单元测试", "覆盖边界条件", "请为当前函数生成 #[test] 单元测试补丁"),
                ("🛡 安全错误处理", "清理 unwrap()", "请将当前代码中的 unwrap() 替换为安全的 expect()/错误处理"),
                ("🛠 检查工作区编译", "cargo check", "/check"),
            ],
            UiLanguage::EnUs => [
                ("✦ Explain Code", "Trace logic", "Please explain the functionality and key logic of the code around cursor"),
                ("⚡ Unit Tests", "Add #[test]", "Please generate a #[test] unit test patch for the current function"),
                ("🛡 Safe Errors", "Fix unwrap()", "Please replace unwrap() calls with safe expect()/error handling"),
                ("🛠 Workspace Check", "cargo check", "/check"),
            ],
        };

        let grid_y = card.y + 104;
        if card.y + card.h - grid_y >= 104 {
            let gap = 8;
            let tile_w = ((card.w - 28 - gap) / 2).max(76);
            let tile_h = 44;
            for (i, (title, sub, prompt)) in starters.iter().enumerate() {
                let col = (i % 2) as i32;
                let row = (i / 2) as i32;
                let tile = Rect::new(
                    card.x + 14 + col * (tile_w + gap),
                    grid_y + row * (tile_h + gap),
                    tile_w,
                    tile_h,
                );
                let hov = tile.contains(hx, hy);
                canvas.stroke_rounded_rect(
                    tile,
                    8,
                    if hov { t.accent_blue } else { blend_rgb(t.border_color, t.panel_bg, 0.45) },
                    if hov { t.popup_hover_bg } else { blend_rgb(t.panel_bg, t.editor_bg, 0.4) },
                );
                canvas.draw_text_styled(
                    tile.x + 10,
                    tile.y + 6,
                    title,
                    if hov { 0xFFFFFF } else { t.text_primary },
                    tile.w - 18,
                    FontStyle::ChatMetaBold,
                );
                canvas.draw_text_styled(
                    tile.x + 10,
                    tile.y + 23,
                    sub,
                    t.text_muted,
                    tile.w - 18,
                    FontStyle::ChatMeta,
                );
                self.hit_regions
                    .push((tile, HitAction::RunAgentQuickPrompt(prompt)));
            }
        }

        if card.h >= 245 {
            canvas.draw_text_centered(
                Rect::new(card.x + 10, card.y + card.h - 26, card.w - 20, 18),
                self.tr(
                    "支持拖入文件/图片直接预览 · 粘贴长代码自动折叠",
                    "Drag files/images to preview · Pasted code auto-folds",
                ),
                t.text_muted,
                FontStyle::ChatMeta,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_agent_chat_rows(
        &mut self,
        canvas: &mut SurfaceCanvas,
        panel_rect: Rect,
        chat_area: Rect,
        wrap_w: i32,
        session_id: &str,
        msg_count: usize,
        applied_sig: u64,
        scale_bits: u32,
        is_busy: bool,
        stream_preview: Option<String>,
        stream_len: usize,
    ) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let stream_sig = (stream_len as u64).wrapping_mul(104_729);
        let combined_sig = applied_sig ^ stream_sig;

        let cache_key_match = self.cached_chat_rows.session_id == session_id
            && self.cached_chat_rows.msg_count == msg_count
            && self.cached_chat_rows.signature == combined_sig
            && self.cached_chat_rows.wrap_w == wrap_w
            && self.cached_chat_rows.scale_bits == scale_bits
            && self.cached_chat_rows.is_busy == is_busy;

        if !cache_key_match {
            let user_max_content_w = ((wrap_w as f32 * 0.80) as i32).clamp(110, (wrap_w - 24).max(110));
            let mut rebuilt: Vec<CachedChatRow> = Vec::with_capacity(msg_count * 7 + 6);
            for (msg_idx, msg) in self.agent_hub.messages.iter().enumerate() {
                if msg.role == "user" {
                    let bubble_w = measure_user_bubble_width(
                        &self.workspace_root,
                        canvas,
                        msg,
                        user_max_content_w,
                    );
                    rebuilt.push(CachedChatRow::UserHeader { msg_idx, bubble_w });
                    rebuilt.push(CachedChatRow::UserBubbleTop { bubble_w });
                    let non_img_files: Vec<(PathBuf, String)> = msg
                        .attachments
                        .iter()
                        .filter(|a| !a.is_image)
                        .map(|a| (a.path.clone(), a.display_name.clone()))
                        .collect();
                    if !non_img_files.is_empty() {
                        rebuilt.push(CachedChatRow::UserFilesRow {
                            files: non_img_files,
                            bubble_w,
                        });
                    }
                    for att in msg.attachments.iter().filter(|a| a.is_image) {
                        rebuilt.push(CachedChatRow::UserImageCard {
                            item: att.clone(),
                            bubble_w,
                        });
                    }
                    append_message_body_rows(
                        &self.workspace_root,
                        canvas,
                        msg_idx,
                        &msg.content,
                        (bubble_w - 26).max(60),
                        true,
                        bubble_w,
                        &self.collapsed_code_blocks,
                        &mut rebuilt,
                    );
                    rebuilt.push(CachedChatRow::UserBubbleBottom { bubble_w });
                } else {
                    rebuilt.push(CachedChatRow::AgentHeader {
                        msg_idx,
                        agent_name: msg.agent.compact_name().to_string(),
                    });
                    if let Some(thought) = &msg.thought {
                        if !thought.trim().is_empty() && msg.role == "assistant" {
                            let expanded = self
                                .expanded_message_thoughts
                                .get(&msg_idx)
                                .copied()
                                .unwrap_or(false);
                            let one_line: String =
                                thought.lines().next().unwrap_or("").chars().take(48).collect();
                            rebuilt.push(CachedChatRow::ThoughtToggle {
                                msg_idx,
                                preview: one_line,
                                expanded,
                            });
                            if expanded {
                                for raw_tl in thought.lines() {
                                    for (_, _, tl) in wrap_chat_line_segments(
                                        canvas,
                                        raw_tl,
                                        (wrap_w - 20).max(60),
                                        FontStyle::ChatMeta,
                                    ) {
                                        rebuilt.push(CachedChatRow::ThoughtLine(tl));
                                    }
                                }
                            }
                        }
                    }
                    append_message_body_rows(
                        &self.workspace_root,
                        canvas,
                        msg_idx,
                        &msg.content,
                        (wrap_w - 8).max(60),
                        false,
                        0,
                        &self.collapsed_code_blocks,
                        &mut rebuilt,
                    );
                    if !msg.modified_files.is_empty() {
                        rebuilt.push(CachedChatRow::ModifiedFilesHeader(
                            msg.modified_files.len(),
                        ));
                        for path in &msg.modified_files {
                            let rel = path
                                .strip_prefix(&self.workspace_root)
                                .unwrap_or(path)
                                .to_string_lossy()
                                .replace('\\', "/");
                            rebuilt.push(CachedChatRow::ModifiedFileChip {
                                path: path.clone(),
                                display: rel,
                            });
                        }
                    }
                    if let Some(patch) = &msg.patch {
                        rebuilt.push(CachedChatRow::PatchHeader {
                            summary: patch.summary.clone(),
                            applied: patch.applied,
                        });
                        for dline in patch.diff_lines.iter().take(6) {
                            rebuilt.push(CachedChatRow::PatchDiffLine {
                                kind: dline.kind,
                                text: dline.content.clone(),
                            });
                        }
                        rebuilt.push(CachedChatRow::PatchButtons {
                            msg_idx,
                            applied: patch.applied,
                        });
                    }
                }
                rebuilt.push(CachedChatRow::Spacer);
            }

            if let Some(stream_txt) = stream_preview {
                if !stream_txt.trim().is_empty() {
                    rebuilt.push(CachedChatRow::AgentHeader {
                        msg_idx: usize::MAX,
                        agent_name: self.agent_hub.active_provider.compact_name().to_string(),
                    });
                    for raw_l in stream_txt.lines() {
                        if raw_l.trim().is_empty() {
                            rebuilt.push(CachedChatRow::ParaSpacer {
                                is_user: false,
                                bubble_w: 0,
                            });
                            continue;
                        }
                        let (clean_l, _, raw_spans) = parse_markdown_inline_line(raw_l);
                        for (seg_s, seg_e, seg_txt) in wrap_chat_line_segments(
                            canvas,
                            &clean_l,
                            (wrap_w - 14).max(60),
                            FontStyle::ChatBody,
                        ) {
                            let spans = project_and_merge_spans(
                                &self.workspace_root,
                                &seg_txt,
                                seg_s,
                                seg_e,
                                &raw_spans,
                            );
                            rebuilt.push(CachedChatRow::StreamingLine {
                                text: seg_txt,
                                spans,
                            });
                        }
                    }
                }
            }
            if is_busy {
                rebuilt.push(CachedChatRow::BusyIndicator);
            }

            let mut cache = AgentChatCache {
                session_id: session_id.to_string(),
                msg_count,
                signature: combined_sig,
                wrap_w,
                scale_bits,
                is_busy,
                rows: rebuilt,
                row_tops: Vec::new(),
                total_height: 0,
            };
            cache.recompute_offsets();
            self.cached_chat_rows = cache;
        }

        let total_rows = self.cached_chat_rows.rows.len();
        let max_s = self.cached_chat_rows.max_scroll_row_for_height(chat_area.h - 4);
        if self.agent_scroll == usize::MAX || self.agent_scroll > max_s {
            self.agent_scroll = max_s;
        }
        let vis_count = self
            .cached_chat_rows
            .visible_rows_from(self.agent_scroll, chat_area.h - 4);

        let visible_slice: Vec<(usize, CachedChatRow)> = self
            .cached_chat_rows
            .rows
            .iter()
            .enumerate()
            .skip(self.agent_scroll)
            .take(vis_count + 1)
            .map(|(idx, r)| (idx, r.clone()))
            .collect();

        let user_bubble_bg = if t.kind == ThemeKind::IslandsLight {
            blend_rgb(t.panel_bg, t.accent_blue, 0.08)
        } else {
            blend_rgb(t.panel_bg, t.text_primary, 0.06)
        };
        let user_bubble_border = blend_rgb(t.border_color, t.panel_bg, 0.40);
        let prose_fg = if t.kind == ThemeKind::IslandsLight {
            t.text_primary
        } else {
            blend_rgb(t.text_primary, 0xFFFFFF, 0.15)
        };
        let code_block_bg = if t.kind == ThemeKind::IslandsLight {
            t.editor_bg
        } else {
            blend_rgb(t.window_bg, t.editor_bg, 0.55)
        };

        let mut cy = chat_area.y + 4;
        let chat_bottom = chat_area.y + chat_area.h;
        let right_edge_x = chat_area.x + chat_area.w - 14;
        for (global_row_idx, item) in &visible_slice {
            let row_h = item.row_height();
            if cy + row_h > chat_bottom {
                break;
            }
            let draw_h = row_h;
            let row_rect = Rect::new(chat_area.x + 6, cy, chat_area.w - 20, row_h);
            match item {
                CachedChatRow::UserHeader { msg_idx, bubble_w } => {
                    let you_lbl = self.tr("你", "You");
                    let (yw, _) = canvas.measure_text(you_lbl, FontStyle::ChatMeta);
                    let lbl_x = right_edge_x - yw - 2;
                    canvas.draw_text_styled(
                        lbl_x,
                        cy + 3,
                        you_lbl,
                        t.text_muted,
                        yw + 4,
                        FontStyle::ChatMeta,
                    );
                    let copy_btn = Rect::new((right_edge_x - *bubble_w).max(row_rect.x), cy + 2, 34, 17);
                    let chov = copy_btn.contains(hx, hy);
                    if chov {
                        canvas.fill_rounded_rect(copy_btn, 4, blend_rgb(t.panel_bg, t.text_primary, 0.08));
                    }
                    canvas.draw_text_centered(
                        copy_btn,
                        self.tr("复制", "Copy"),
                        if chov {
                            t.text_primary
                        } else {
                            blend_rgb(t.editor_bg, t.text_muted, 0.55)
                        },
                        FontStyle::ChatMeta,
                    );
                    self.hit_regions
                        .push((copy_btn, HitAction::CopyAgentMessage(*msg_idx)));
                }
                CachedChatRow::UserBubbleTop { bubble_w } => {
                    let bx = (right_edge_x - *bubble_w).max(row_rect.x);
                    let bw = (*bubble_w).min(right_edge_x - bx);
                    let cap_h = (row_h + 10).min(chat_bottom - cy);
                    if cap_h >= 6 {
                        canvas.fill_rounded_rect(Rect::new(bx, cy, bw, cap_h), 10, user_bubble_bg);
                    }
                }
                CachedChatRow::UserBubbleBottom { bubble_w } => {
                    let bx = (right_edge_x - *bubble_w).max(row_rect.x);
                    let bw = (*bubble_w).min(right_edge_x - bx);
                    if cy > chat_area.y + 8 {
                        let cap = Rect::new(bx, cy - 8, bw, row_h + 8);
                        canvas.fill_rounded_rect(cap, 10, user_bubble_bg);
                    } else {
                        canvas.fill_rect(Rect::new(bx, cy, bw, draw_h), user_bubble_bg);
                    }
                }
                CachedChatRow::UserLine {
                    text,
                    spans,
                    default_bold,
                    bubble_w,
                } => {
                    let bx = (right_edge_x - *bubble_w).max(row_rect.x);
                    let bw = (*bubble_w).min(right_edge_x - bx);
                    let brect = Rect::new(bx, cy, bw, row_h);
                    canvas.fill_rect(brect, user_bubble_bg);
                    self.draw_chat_line_with_spans_and_selection(
                        canvas,
                        brect,
                        bx + 13,
                        cy + 2,
                        *global_row_idx,
                        text,
                        spans,
                        *default_bold,
                        prose_fg,
                        (bw - 24).max(40),
                        FontStyle::ChatBody,
                    );
                }
                CachedChatRow::UserFilesRow { files, bubble_w } => {
                    let bx = (right_edge_x - *bubble_w).max(row_rect.x);
                    let bw = (*bubble_w).min(right_edge_x - bx);
                    let brect = Rect::new(bx, cy, bw, row_h);
                    canvas.fill_rect(brect, user_bubble_bg);
                    let mut fx = bx + 12;
                    let max_fx = bx + bw - 10;
                    for (path, name) in files {
                        let (nw, _) = canvas.measure_text(name, FontStyle::ChatMeta);
                        let chip_w = (nw + 28).clamp(52, 160);
                        if fx + chip_w > max_fx {
                            break;
                        }
                        let chip = Rect::new(fx, cy + 3, chip_w, 20);
                        let hov = chip.contains(hx, hy);
                        canvas.stroke_rounded_rect(
                            chip,
                            4,
                            if hov { t.accent_blue } else { user_bubble_border },
                            t.editor_bg,
                        );
                        canvas.draw_file_type_icon(chip.x + 5, chip.y + 3, LanguageId::from_path(path));
                        canvas.draw_text_styled(
                            chip.x + 21,
                            chip.y + 2,
                            name,
                            if hov { 0xFFFFFF } else { t.syn_function },
                            chip.w - 25,
                            FontStyle::ChatMeta,
                        );
                        self.hit_regions
                            .push((chip, HitAction::OpenFileAtLine(path.clone(), None)));
                        fx += chip_w + 5;
                    }
                }
                CachedChatRow::UserImageCard { item, bubble_w } => {
                    let bx = (right_edge_x - *bubble_w).max(row_rect.x);
                    let bw = (*bubble_w).min(right_edge_x - bx);
                    let brect = Rect::new(bx, cy, bw, row_h);
                    canvas.fill_rect(brect, user_bubble_bg);
                    if cy + row_h <= chat_bottom + 8 {
                        let card = Rect::new(bx + 12, cy + 4, (bw - 24).max(60), row_h - 8);
                        let hov = card.contains(hx, hy);
                        canvas.stroke_rounded_rect(
                            card,
                            6,
                            if hov { t.accent_blue } else { user_bubble_border },
                            t.editor_bg,
                        );
                        let thumb_rect = Rect::new(card.x + 6, card.y + 4, 68, card.h - 8);
                        if let Some(decoded) = self.image_preview_cache.get(&item.path).cloned() {
                            canvas.draw_rgba_image_fit(
                                thumb_rect,
                                5,
                                decoded.width,
                                decoded.height,
                                &decoded.rgba,
                                t.window_bg,
                                t.border_color,
                            );
                        } else {
                            canvas.stroke_rounded_rect(thumb_rect, 5, t.border_color, t.panel_bg);
                            canvas.draw_text_centered(thumb_rect, "IMG", t.text_muted, FontStyle::ChatMeta);
                        }
                        let info_x = thumb_rect.x + thumb_rect.w + 10;
                        let info_w = (card.x + card.w - info_x - 8).max(40);
                        canvas.draw_text_styled(
                            info_x,
                            card.y + 10,
                            &item.display_name,
                            if hov { 0xFFFFFF } else { t.text_primary },
                            info_w,
                            FontStyle::ChatMetaBold,
                        );
                        let dim_txt = if item.width > 0 && item.height > 0 {
                            format!("{} × {} px", item.width, item.height)
                        } else {
                            self.tr("图片附件", "Image").to_string()
                        };
                        canvas.draw_text_styled(
                            info_x,
                            card.y + 30,
                            &dim_txt,
                            t.syn_function,
                            info_w,
                            FontStyle::ChatCode,
                        );
                        canvas.draw_text_styled(
                            info_x,
                            card.y + 49,
                            self.tr("点击放大预览", "Click to preview"),
                            if hov { t.accent_blue } else { t.text_muted },
                            info_w,
                            FontStyle::ChatMeta,
                        );
                        self.hit_regions
                            .push((card, HitAction::OpenImagePreview(item.path.clone())));
                    }
                }
                CachedChatRow::ParaSpacer { is_user, bubble_w } => {
                    if *is_user {
                        let bx = (right_edge_x - *bubble_w).max(row_rect.x);
                        let bw = (*bubble_w).min(right_edge_x - bx);
                        let brect = Rect::new(bx, cy, bw, row_h);
                        canvas.fill_rect(brect, user_bubble_bg);
                    }
                }
                CachedChatRow::AgentHeader {
                    msg_idx,
                    agent_name,
                } => {
                    let hdr_fg = blend_rgb(t.text_primary, t.text_secondary, 0.25);
                    canvas.draw_agent_cube_icon(row_rect.x + 4, cy + 5, hdr_fg);
                    canvas.draw_text_styled(
                        row_rect.x + 22,
                        cy + 4,
                        agent_name,
                        hdr_fg,
                        wrap_w - 64,
                        FontStyle::ChatMetaBold,
                    );
                    if *msg_idx != usize::MAX {
                        let copy_btn = Rect::new(row_rect.x + row_rect.w - 38, cy + 3, 34, 17);
                        let chov = copy_btn.contains(hx, hy);
                        if chov {
                            canvas.fill_rounded_rect(copy_btn, 4, blend_rgb(t.panel_bg, t.text_primary, 0.08));
                        }
                        canvas.draw_text_centered(
                            copy_btn,
                            self.tr("复制", "Copy"),
                            if chov { t.text_primary } else { t.text_muted },
                            FontStyle::ChatMeta,
                        );
                        self.hit_regions
                            .push((copy_btn, HitAction::CopyAgentMessage(*msg_idx)));
                    }
                }
                CachedChatRow::ThoughtToggle {
                    msg_idx,
                    preview,
                    expanded,
                } => {
                    let pill = Rect::new(row_rect.x + 2, cy + 1, row_rect.w - 4, 20);
                    let hov = pill.contains(hx, hy);
                    if hov {
                        canvas.fill_rounded_rect(pill, 4, blend_rgb(t.panel_bg, t.text_primary, 0.05));
                    }
                    let glyph = if *expanded { "▾" } else { "▸" };
                    let lbl = if *expanded {
                        self.tr("思考过程", "Reasoning").to_string()
                    } else {
                        preview.clone()
                    };
                    canvas.draw_text_styled(
                        pill.x + 4,
                        pill.y + 2,
                        &format!("{glyph}  {lbl}"),
                        if hov { t.text_secondary } else { t.text_muted },
                        pill.w - 10,
                        FontStyle::ChatMeta,
                    );
                    self.hit_regions
                        .push((pill, HitAction::ToggleMessageThoughtCollapse(*msg_idx)));
                }
                CachedChatRow::ThoughtLine(txt) => {
                    canvas.fill_rect(
                        Rect::new(row_rect.x + 8, cy, 1, row_h),
                        blend_rgb(t.border_color, t.panel_bg, 0.45),
                    );
                    self.draw_chat_line_with_spans_and_selection(
                        canvas,
                        row_rect,
                        row_rect.x + 16,
                        cy + 1,
                        *global_row_idx,
                        txt,
                        &[],
                        false,
                        t.text_muted,
                        wrap_w - 16,
                        FontStyle::ChatMeta,
                    );
                }
                CachedChatRow::AgentLine {
                    text,
                    spans,
                    default_bold,
                } => {
                    self.draw_chat_line_with_spans_and_selection(
                        canvas,
                        row_rect,
                        row_rect.x + 4,
                        cy + 2,
                        *global_row_idx,
                        text,
                        spans,
                        *default_bold,
                        prose_fg,
                        wrap_w - 4,
                        FontStyle::ChatBody,
                    );
                }
                CachedChatRow::CodeBlockHeader {
                    msg_idx,
                    blk_idx,
                    lang,
                    code,
                    line_count,
                    collapsed,
                } => {
                    let hdr = Rect::new(row_rect.x + 2, cy + 1, row_rect.w - 4, row_h - 1);
                    canvas.stroke_rounded_rect(
                        hdr,
                        6,
                        blend_rgb(t.border_color, t.panel_bg, 0.45),
                        blend_rgb(code_block_bg, t.panel_bg, 0.35),
                    );
                    let arrow = if *collapsed { "▸" } else { "▾" };
                    let collapse_hit = Rect::new(hdr.x, hdr.y, hdr.w - 64, hdr.h);
                    let col_hov = collapse_hit.contains(hx, hy);
                    canvas.draw_text_styled(
                        hdr.x + 10,
                        hdr.y + 4,
                        &format!("{arrow}  {lang} · {line_count} {}", self.tr("行", "lines")),
                        if col_hov { t.text_primary } else { t.text_secondary },
                        hdr.w - 116,
                        FontStyle::ChatCodeBold,
                    );
                    canvas.draw_text_styled(
                        hdr.x + hdr.w - 104,
                        hdr.y + 5,
                        if *collapsed {
                            self.tr("展开", "Expand")
                        } else {
                            self.tr("折叠", "Fold")
                        },
                        if col_hov { t.accent_blue } else { t.text_muted },
                        38,
                        FontStyle::ChatMeta,
                    );
                    self.hit_regions.push((
                        collapse_hit,
                        HitAction::ToggleCodeBlockCollapse(*msg_idx, *blk_idx),
                    ));

                    let copy_btn = Rect::new(hdr.x + hdr.w - 60, hdr.y + 3, 54, 18);
                    let chov = copy_btn.contains(hx, hy);
                    if chov {
                        canvas.fill_rounded_rect(copy_btn, 4, blend_rgb(t.panel_bg, t.text_primary, 0.1));
                    }
                    canvas.draw_text_centered(
                        copy_btn,
                        self.tr("复制", "Copy"),
                        if chov { t.text_primary } else { t.text_muted },
                        FontStyle::ChatMeta,
                    );
                    self.hit_regions
                        .push((copy_btn, HitAction::CopyAgentCodeBlock(code.clone())));
                }
                CachedChatRow::CodeBlockCollapsedSummary {
                    msg_idx,
                    blk_idx,
                    preview,
                    line_count,
                } => {
                    let srect = Rect::new(row_rect.x + 2, cy, row_rect.w - 4, row_h - 1);
                    let hov = srect.contains(hx, hy);
                    canvas.stroke_rounded_rect(
                        srect,
                        5,
                        if hov { t.accent_blue } else { blend_rgb(t.border_color, t.panel_bg, 0.45) },
                        code_block_bg,
                    );
                    let fold_lbl = match self.ui_lang {
                        UiLanguage::ZhCn => format!("已折叠 {line_count} 行"),
                        UiLanguage::EnUs => format!("{line_count} lines folded"),
                    };
                    canvas.draw_text_styled(
                        srect.x + 10,
                        srect.y + 4,
                        &format!("{preview}  ·  {fold_lbl}"),
                        if hov { t.text_primary } else { t.text_muted },
                        srect.w - 18,
                        FontStyle::ChatCode,
                    );
                    self.hit_regions
                        .push((srect, HitAction::ToggleCodeBlockCollapse(*msg_idx, *blk_idx)));
                }
                CachedChatRow::CodeBlockLine { text, lang } => {
                    let crect = Rect::new(row_rect.x + 2, cy, row_rect.w - 4, row_h);
                    let brd = blend_rgb(t.border_color, t.panel_bg, 0.45);
                    canvas.fill_rect(crect, code_block_bg);
                    canvas.fill_rect(Rect::new(crect.x, crect.y, 1, row_h), brd);
                    canvas.fill_rect(Rect::new(crect.x + crect.w - 1, crect.y, 1, row_h), brd);
                    self.draw_syntax_highlighted_chat_code_line(
                        canvas,
                        crect,
                        crect.x + 10,
                        cy + 2,
                        *global_row_idx,
                        text,
                        *lang,
                        crect.w - 20,
                    );
                }
                CachedChatRow::ModifiedFilesHeader(cnt) => {
                    canvas.draw_text_styled(
                        row_rect.x + 4,
                        cy + 3,
                        &format!(
                            "▸ {} {cnt} {}",
                            self.tr("改动 / 相关", "Modified / Related"),
                            self.tr("个文件", "files")
                        ),
                        t.text_muted,
                        row_rect.w - 8,
                        FontStyle::ChatMeta,
                    );
                }
                CachedChatRow::ModifiedFileChip { path, display } => {
                    let chip = Rect::new(row_rect.x + 4, cy + 1, row_rect.w - 8, row_h - 2);
                    let hov = chip.contains(hx, hy);
                    canvas.stroke_rounded_rect(
                        chip,
                        5,
                        if hov { t.accent_blue } else { blend_rgb(t.border_color, t.panel_bg, 0.45) },
                        if hov { t.popup_hover_bg } else { code_block_bg },
                    );
                    let lang = LanguageId::from_path(path);
                    canvas.draw_file_type_icon(chip.x + 7, chip.y + 3, lang);
                    canvas.draw_text_styled(
                        chip.x + 25,
                        chip.y + 3,
                        display,
                        if hov { 0xFFFFFF } else { t.syn_function },
                        chip.w - 64,
                        FontStyle::ChatCode,
                    );
                    canvas.draw_text_styled(
                        chip.x + chip.w - 36,
                        chip.y + 3,
                        self.tr("打开 →", "Open →"),
                        if hov { 0xFFFFFF } else { t.text_muted },
                        32,
                        FontStyle::ChatMeta,
                    );
                    self.hit_regions
                        .push((chip, HitAction::OpenFileAtLine(path.clone(), None)));
                }
                CachedChatRow::StreamingLine { text, spans } => {
                    self.draw_chat_line_with_spans_and_selection(
                        canvas,
                        row_rect,
                        row_rect.x + 4,
                        cy + 2,
                        *global_row_idx,
                        text,
                        spans,
                        false,
                        prose_fg,
                        wrap_w - 4,
                        FontStyle::ChatBody,
                    );
                }
                CachedChatRow::PatchHeader { summary, applied } => {
                    let hdr = Rect::new(row_rect.x + 2, cy + 1, row_rect.w - 4, row_h - 2);
                    canvas.fill_rounded_rect(hdr, 5, code_block_bg);
                    let status = if *applied {
                        self.tr("已应用 ✓", "Applied ✓")
                    } else {
                        self.tr("待应用 Diff", "Pending Diff")
                    };
                    canvas.draw_text_styled(
                        hdr.x + 8,
                        hdr.y + 3,
                        &format!("{}  {summary}   [{status}]", self.tr("补丁", "Patch")),
                        if *applied { t.run_green } else { t.accent_blue },
                        hdr.w - 16,
                        FontStyle::ChatMetaBold,
                    );
                }
                CachedChatRow::PatchDiffLine { kind, text } => {
                    let (bg, fg, sign) = match kind {
                        DiffLineKind::Added => (t.diff_added_bg, t.diff_added_fg, "+ "),
                        DiffLineKind::Removed => (t.diff_removed_bg, t.diff_removed_fg, "- "),
                        DiffLineKind::Context => (code_block_bg, t.text_secondary, "  "),
                    };
                    let drect = Rect::new(row_rect.x + 2, cy, row_rect.w - 4, row_h - 1);
                    canvas.fill_rect(drect, bg);
                    canvas.draw_text_styled(
                        drect.x + 8,
                        cy + 1,
                        &format!("{sign}{text}"),
                        fg,
                        drect.w - 14,
                        FontStyle::ChatCode,
                    );
                }
                CachedChatRow::PatchButtons { msg_idx, applied } => {
                    let bx = row_rect.x + 2;
                    if *applied {
                        let rev_btn = Rect::new(bx, cy + 2, 136, 20);
                        canvas.stroke_rounded_rect(rev_btn, 5, t.run_green, t.diff_added_bg);
                        canvas.draw_text_centered(
                            rev_btn,
                            self.tr("✓ 已应用 · ↶ 撤销", "✓ Applied · ↶ Revert"),
                            t.diff_added_fg,
                            FontStyle::ChatMeta,
                        );
                        self.hit_regions
                            .push((rev_btn, HitAction::RevertLastCheckpoint));
                    } else {
                        let apply_btn = Rect::new(bx, cy + 2, 108, 20);
                        canvas.fill_rounded_rect(apply_btn, 5, t.run_green);
                        canvas.draw_text_centered(
                            apply_btn,
                            self.tr("✓ 应用到编辑器", "✓ Apply"),
                            0xFFFFFF,
                            FontStyle::ChatMetaBold,
                        );
                        self.hit_regions
                            .push((apply_btn, HitAction::ApplyChatPatch(*msg_idx)));

                        let ins_btn = Rect::new(bx + 114, cy + 2, 78, 20);
                        canvas.stroke_rounded_rect(ins_btn, 5, t.border_color, code_block_bg);
                        canvas.draw_text_centered(
                            ins_btn,
                            self.tr("插入光标", "Insert"),
                            t.text_primary,
                            FontStyle::ChatMeta,
                        );
                        self.hit_regions
                            .push((ins_btn, HitAction::InsertChatPatch(*msg_idx)));

                        let prev_btn = Rect::new(bx + 198, cy + 2, 78, 20);
                        canvas.stroke_rounded_rect(prev_btn, 5, t.accent_blue, code_block_bg);
                        canvas.draw_text_centered(
                            prev_btn,
                            self.tr("行内 Diff", "Inline Diff"),
                            t.accent_blue,
                            FontStyle::ChatMeta,
                        );
                        self.hit_regions
                            .push((prev_btn, HitAction::PreviewPatchInEditor(*msg_idx)));
                    }
                }
                CachedChatRow::BusyIndicator => {
                    canvas.draw_subtle_spinner(
                        row_rect.x + 10,
                        cy + 13,
                        4.8,
                        self.spinner_phase(),
                        t.text_secondary,
                        t.border_color,
                        0.95,
                    );
                    let elapsed_secs = self
                        .agent_busy_started_at
                        .map(|st| st.elapsed().as_secs())
                        .unwrap_or(0);
                    let elapsed_str = if elapsed_secs >= 60 {
                        format!("{}m {}s", elapsed_secs / 60, elapsed_secs % 60)
                    } else {
                        format!("{elapsed_secs}s")
                    };
                    let busy_lbl = format!(
                        "{}  {elapsed_str}",
                        self.tr("处理中...", "Processing...")
                    );
                    canvas.draw_text_styled(
                        row_rect.x + 22,
                        cy + 5,
                        &busy_lbl,
                        t.text_muted,
                        row_rect.w - 28,
                        FontStyle::ChatMeta,
                    );
                }
                CachedChatRow::Spacer => {}
            }
            cy += row_h;
        }

        let track = Rect::new(panel_rect.x + panel_rect.w - 8, chat_area.y, 5, chat_area.h);
        self.right_scroll_info = ScrollTrackInfo {
            track,
            visible: vis_count,
            total: total_rows,
        };
        canvas.draw_vertical_scrollbar(
            track,
            self.agent_scroll,
            vis_count,
            total_rows,
            blend_rgb(t.border_color, t.panel_bg, 0.35),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn render_agent_composer(
        &mut self,
        canvas: &mut SurfaceCanvas,
        composer_rect: Rect,
        attached_items: &[AgentAttachedItem],
        pasted_snippets: &[ide_agent_hub::PastedCodeSnippet],
        expanded_snip: Option<&ide_agent_hub::PastedCodeSnippet>,
        tray_h: i32,
        drawer_h: i32,
        is_busy: bool,
    ) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let is_focused = self.focus == FocusTarget::AgentChatInput;
        let composer_bg = if t.kind == ThemeKind::IslandsLight {
            t.editor_bg
        } else {
            blend_rgb(t.editor_bg, t.panel_bg, 0.14)
        };
        let composer_border = if is_focused {
            blend_rgb(t.border_color, t.accent_blue, 0.55)
        } else {
            blend_rgb(t.border_color, t.panel_bg, 0.35)
        };
        let hover_pill_bg = blend_rgb(composer_bg, t.text_primary, 0.08);
        canvas.stroke_rounded_rect(composer_rect, 10, composer_border, composer_bg);

        // 1. Composer 顶部精简上下文与推理选项行：[@file]   [⚡ Fast] [High]
        let active_buf = self.active_buffer();
        let active_fname = active_buf
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        let ctx_text = if self.attach_active_context {
            format!("@{}:L{}", active_fname, active_buf.cursor_line + 1)
        } else {
            self.tr("+ 关联文件", "+ Attach file").to_string()
        };
        let (cw, _) = canvas.measure_text(&ctx_text, FontStyle::ChatMeta);
        let ctx_pill = Rect::new(composer_rect.x + 10, composer_rect.y + 6, cw + 12, 18);
        let ctx_hov = ctx_pill.contains(hx, hy);
        canvas.fill_rounded_rect(
            ctx_pill,
            4,
            if self.attach_active_context {
                blend_rgb(composer_bg, t.accent_blue, 0.22)
            } else if ctx_hov {
                hover_pill_bg
            } else {
                blend_rgb(composer_bg, t.text_primary, 0.04)
            },
        );
        canvas.draw_text_centered(
            ctx_pill,
            &ctx_text,
            if self.attach_active_context {
                t.accent_blue
            } else {
                t.text_muted
            },
            FontStyle::ChatMeta,
        );
        self.hit_regions
            .push((ctx_pill, HitAction::ToggleAttachActiveContext));

        let mut opt_rx = composer_rect.x + composer_rect.w - 10;
        if self.agent_hub.supports_reasoning_effort() {
            let eff = self.agent_hub.active_reasoning_effort();
            let eff_txt = match self.ui_lang {
                UiLanguage::ZhCn => eff.label_zh(),
                UiLanguage::EnUs => eff.label_en(),
            };
            let (ew, _) = canvas.measure_text(eff_txt, FontStyle::ChatMeta);
            let ew_pill = ew + 14;
            let eff_pill = Rect::new(opt_rx - ew_pill, composer_rect.y + 6, ew_pill, 18);
            if eff_pill.x > ctx_pill.x + ctx_pill.w + 6 {
                let ehov = eff_pill.contains(hx, hy);
                if ehov {
                    canvas.fill_rounded_rect(eff_pill, 4, hover_pill_bg);
                }
                canvas.draw_text_centered(
                    eff_pill,
                    eff_txt,
                    if ehov { t.text_primary } else { t.text_secondary },
                    FontStyle::ChatMeta,
                );
                self.hit_regions
                    .push((eff_pill, HitAction::CycleAgentReasoningEffort));
                opt_rx = eff_pill.x - 4;
            }
        }
        if self.agent_hub.supports_fast_mode() {
            let is_fast = self.agent_hub.active_fast_mode();
            let fast_txt = if is_fast {
                self.tr("⚡ Fast", "⚡ Fast")
            } else {
                self.tr("标准", "Normal")
            };
            let (fw, _) = canvas.measure_text(fast_txt, FontStyle::ChatMeta);
            let fw_pill = fw + 12;
            let fast_pill = Rect::new(opt_rx - fw_pill, composer_rect.y + 6, fw_pill, 18);
            if fast_pill.x > ctx_pill.x + ctx_pill.w + 6 {
                let fhov = fast_pill.contains(hx, hy);
                if is_fast {
                    canvas.fill_rounded_rect(
                        fast_pill,
                        4,
                        blend_rgb(composer_bg, t.warning_yellow, 0.14),
                    );
                } else if fhov {
                    canvas.fill_rounded_rect(fast_pill, 4, hover_pill_bg);
                }
                canvas.draw_text_centered(
                    fast_pill,
                    fast_txt,
                    if is_fast {
                        t.warning_yellow
                    } else if fhov {
                        t.text_primary
                    } else {
                        t.text_muted
                    },
                    FontStyle::ChatMeta,
                );
                self.hit_regions
                    .push((fast_pill, HitAction::ToggleAgentFastMode));
            }
        }

        // 2. 附件与折叠代码托盘（拖拽的图片直接缩略图预览 + 文件芯片 + 折叠粘贴代码芯片）
        let mut cursor_y = composer_rect.y + 30;
        if tray_h > 0 {
            let mut tx = composer_rect.x + 10;
            let max_tx = composer_rect.x + composer_rect.w - 10;
            for item in attached_items.iter().filter(|a| a.is_image) {
                let card_w = 56;
                let card_h = 52;
                if tx + card_w > max_tx {
                    break;
                }
                let card = Rect::new(tx, cursor_y, card_w, card_h);
                let hov = card.contains(hx, hy);
                canvas.stroke_rounded_rect(
                    card,
                    5,
                    if hov { t.accent_blue } else { t.border_color },
                    t.window_bg,
                );
                let inner = Rect::new(card.x + 2, card.y + 2, card.w - 4, card.h - 15);
                if let Some(decoded) = self.image_preview_cache.get(&item.path).cloned() {
                    canvas.draw_rgba_image_fit(
                        inner,
                        3,
                        decoded.width,
                        decoded.height,
                        &decoded.rgba,
                        t.window_bg,
                        t.window_bg,
                    );
                } else {
                    canvas.draw_text_centered(inner, "IMG", t.text_muted, FontStyle::ChatMeta);
                }
                canvas.draw_text_styled(
                    card.x + 3,
                    card.y + card.h - 13,
                    &item.display_name,
                    t.text_muted,
                    card.w - 16,
                    FontStyle::ChatMeta,
                );
                self.hit_regions
                    .push((card, HitAction::OpenImagePreview(item.path.clone())));

                let rm_btn = Rect::new(card.x + card.w - 14, card.y + 1, 13, 13);
                let rm_hov = rm_btn.contains(hx, hy);
                canvas.fill_rounded_rect(
                    rm_btn,
                    3,
                    if rm_hov { t.breakpoint_red } else { t.panel_bg },
                );
                canvas.draw_text_centered(rm_btn, "×", 0xFFFFFF, FontStyle::ChatMeta);
                self.hit_regions
                    .push((rm_btn, HitAction::RemoveComposerAttachment(item.path.clone())));

                tx += card_w + 6;
            }

            let chip_y = if tray_h > 30 { cursor_y + 14 } else { cursor_y + 2 };
            for item in attached_items.iter().filter(|a| !a.is_image) {
                let (nw, _) = canvas.measure_text(&item.display_name, FontStyle::ChatMeta);
                let cw_chip = (nw + 38).clamp(64, 150);
                if tx + cw_chip > max_tx {
                    break;
                }
                let chip = Rect::new(tx, chip_y, cw_chip, 22);
                let hov = chip.contains(hx, hy);
                canvas.stroke_rounded_rect(
                    chip,
                    4,
                    if hov { t.accent_blue } else { t.border_color },
                    t.panel_bg,
                );
                canvas.draw_file_type_icon(chip.x + 5, chip.y + 4, LanguageId::from_path(&item.path));
                canvas.draw_text_styled(
                    chip.x + 21,
                    chip.y + 3,
                    &item.display_name,
                    t.syn_function,
                    chip.w - 38,
                    FontStyle::ChatMeta,
                );
                self.hit_regions
                    .push((chip, HitAction::OpenFileAtLine(item.path.clone(), None)));
                let rm = Rect::new(chip.x + chip.w - 16, chip.y + 3, 14, 16);
                canvas.draw_text_centered(
                    rm,
                    "×",
                    if rm.contains(hx, hy) { t.breakpoint_red } else { t.text_muted },
                    FontStyle::ChatMeta,
                );
                self.hit_regions
                    .push((rm, HitAction::RemoveComposerAttachment(item.path.clone())));
                tx += cw_chip + 5;
            }

            for snip in pasted_snippets {
                let is_exp = self.expanded_composer_snippet_id.as_deref() == Some(snip.id.as_str());
                let arrow = if is_exp { "▾" } else { "▸" };
                let lbl = format!("{arrow} </> {} ({}行)", snip.language, snip.line_count);
                let (lw, _) = canvas.measure_text(&lbl, FontStyle::ChatMeta);
                let sw_chip = (lw + 28).clamp(86, 178);
                if tx + sw_chip > max_tx {
                    break;
                }
                let chip = Rect::new(tx, chip_y, sw_chip, 22);
                let hov = chip.contains(hx, hy) || is_exp;
                canvas.stroke_rounded_rect(
                    chip,
                    4,
                    if hov { t.accent_blue } else { t.border_color },
                    if is_exp { t.popup_hover_bg } else { t.panel_bg },
                );
                canvas.draw_text_styled(
                    chip.x + 6,
                    chip.y + 3,
                    &lbl,
                    if is_exp { 0xFFFFFF } else { t.syn_function },
                    chip.w - 24,
                    FontStyle::ChatMeta,
                );
                self.hit_regions
                    .push((chip, HitAction::ToggleComposerPastedSnippet(snip.id.clone())));
                let rm = Rect::new(chip.x + chip.w - 16, chip.y + 3, 14, 16);
                canvas.draw_text_centered(
                    rm,
                    "×",
                    if rm.contains(hx, hy) { t.breakpoint_red } else { t.text_muted },
                    FontStyle::ChatMeta,
                );
                self.hit_regions
                    .push((rm, HitAction::RemoveComposerPastedSnippet(snip.id.clone())));
                tx += sw_chip + 5;
            }
            cursor_y += tray_h;
        }

        // 3. 可展开粘贴代码抽屉
        if let Some(snip) = expanded_snip {
            let drect = Rect::new(
                composer_rect.x + 10,
                cursor_y,
                composer_rect.w - 20,
                drawer_h - 4,
            );
            canvas.stroke_rounded_rect(drect, 5, t.accent_blue, t.window_bg);
            canvas.draw_text_styled(
                drect.x + 8,
                drect.y + 3,
                &format!("</> {} · {} 行（发送时默认折叠展示）", snip.title, snip.line_count),
                t.syn_function,
                drect.w - 32,
                FontStyle::ChatMeta,
            );
            let close_d = Rect::new(drect.x + drect.w - 20, drect.y + 2, 16, 16);
            canvas.draw_text_centered(close_d, "▾", t.text_muted, FontStyle::ChatMeta);
            self.hit_regions.push((
                close_d,
                HitAction::ToggleComposerPastedSnippet(snip.id.clone()),
            ));
            let mut dy = drect.y + 22;
            let lang_id = language_from_fence_tag(&snip.language);
            for line in snip.content.lines().take(5) {
                let mut cur_x = drect.x + 8;
                let max_x = drect.x + drect.w - 8;
                for sp in highlight_line(line, lang_id) {
                    if cur_x >= max_x {
                        break;
                    }
                    if !sp.text.is_empty() {
                        cur_x = canvas.draw_text_styled(
                            cur_x,
                            dy,
                            &sp.text,
                            sp.kind.color(&t),
                            max_x - cur_x,
                            FontStyle::ChatCode,
                        );
                    }
                }
                dy += 16;
            }
            cursor_y += drawer_h;
        }

        // 4. 原生多行文本输入区域
        let bottom_bar_y = composer_rect.y + composer_rect.h - 29;
        let input_h = (bottom_bar_y - cursor_y - 3).max(34);
        let input_click_area = Rect::new(
            composer_rect.x + 4,
            cursor_y - 2,
            composer_rect.w - 8,
            input_h + 4,
        );
        self.hit_regions
            .push((input_click_area, HitAction::FocusAgentChatInput));

        let text_rect = Rect::new(
            composer_rect.x + 12,
            cursor_y + 1,
            (composer_rect.w - 24).max(80),
            input_h,
        );
        self.agent_input_text_rect = text_rect;
        let ph = if is_focused {
            self.tr(
                "输入问题或指令，拖拽文件/图片，Enter 发送...",
                "Ask anything, drop files/images, Enter to send...",
            )
        } else {
            self.tr(
                "输入指令或提问，@ 引用当前文件...",
                "Plan, ask, or build with local CLI...",
            )
        };
        let ime = self.agent_chat_input.render_multiline(
            canvas,
            text_rect,
            AGENT_INPUT_LINE_H,
            ph,
            is_focused,
            self.caret_visible,
            t.text_primary,
            t.text_muted,
            t.editor_selection_bg,
            t.accent_blue,
            AGENT_INPUT_FONT,
        );
        if is_focused {
            self.ime_spot = ime;
        }

        // 5. Composer 底部极简扁平下拉工具行：[● CLI ⌄]  [Model ⌄]  [◈ Mode ⌄]     [+] [↑ / ■]
        let bar_y = bottom_bar_y;
        let active_agent = self.agent_hub.active_provider;
        let agent_short = active_agent.compact_name();
        let agent_available = self
            .agent_hub
            .providers
            .iter()
            .find(|p| p.kind == active_agent)
            .map(|p| p.available)
            .unwrap_or(true);
        let (aw, _) = canvas.measure_text(agent_short, FontStyle::ChatMeta);
        let agent_pill = Rect::new(composer_rect.x + 8, bar_y, aw + 30, 22);
        let agent_hov = agent_pill.contains(hx, hy) || self.agent_provider_popup_open;
        if agent_hov {
            canvas.fill_rounded_rect(agent_pill, 5, hover_pill_bg);
        }
        canvas.fill_circle(
            agent_pill.x + 8,
            agent_pill.y + 11,
            3,
            if agent_available {
                t.run_green
            } else {
                t.warning_yellow
            },
        );
        canvas.draw_text_styled(
            agent_pill.x + 15,
            agent_pill.y + 3,
            agent_short,
            if agent_hov { t.text_primary } else { t.text_secondary },
            aw + 4,
            FontStyle::ChatMeta,
        );
        canvas.draw_chevron_down(
            agent_pill.x + agent_pill.w - 12,
            agent_pill.y + 9,
            t.text_muted,
        );
        self.agent_provider_pill_rect = agent_pill;
        self.hit_regions
            .push((agent_pill, HitAction::ToggleAgentProviderPopup));

        let send_btn = Rect::new(composer_rect.x + composer_rect.w - 29, bar_y - 1, 23, 23);
        let attach_btn = Rect::new(send_btn.x - 26, bar_y - 1, 23, 23);

        self.agent_mode = self.agent_hub.normalize_mode_for_active(self.agent_mode);
        let mode_label = self.agent_mode.label();
        let (mw, _) = canvas.measure_text(mode_label, FontStyle::ChatMetaBold);
        let mode_w = (mw + 36).max(62);

        let raw_model = self.agent_hub.active_model();
        let short_model = raw_model.rsplit('/').next().unwrap_or(&raw_model);
        let model_x = agent_pill.x + agent_pill.w + 4;
        let avail_for_model = attach_btn.x - model_x - mode_w - 8;
        let model_pill_end_x = if avail_for_model >= 58 {
            let (mod_txt_w, _) = canvas.measure_text(short_model, FontStyle::ChatMeta);
            let mw_pill = (mod_txt_w + 24).clamp(58, avail_for_model);
            let model_pill = Rect::new(model_x, bar_y, mw_pill, 22);
            let model_hov = model_pill.contains(hx, hy) || self.agent_model_popup_open;
            if model_hov {
                canvas.fill_rounded_rect(model_pill, 5, hover_pill_bg);
            }
            canvas.draw_text_styled(
                model_pill.x + 7,
                model_pill.y + 3,
                short_model,
                if model_hov { t.text_primary } else { t.text_secondary },
                mw_pill - 18,
                FontStyle::ChatMeta,
            );
            canvas.draw_chevron_down(
                model_pill.x + model_pill.w - 11,
                model_pill.y + 9,
                t.text_muted,
            );
            self.agent_model_pill_rect = model_pill;
            self.hit_regions
                .push((model_pill, HitAction::ToggleAgentModelPopup));
            model_pill.x + model_pill.w + 4
        } else {
            self.agent_model_pill_rect = Rect::new(model_x, bar_y, 0, 22);
            model_x
        };

        let mode_pill = Rect::new(model_pill_end_x, bar_y, mode_w, 22);
        let mode_hov = mode_pill.contains(hx, hy) || self.agent_mode_popup_open;
        let (mode_icon_kind, mode_accent) = match self.agent_mode {
            AgentWorkbenchMode::Agent => (0u8, t.text_secondary),
            AgentWorkbenchMode::Edit => (1u8, t.run_green),
            AgentWorkbenchMode::Ask => (2u8, t.syn_function),
        };
        if mode_hov {
            canvas.fill_rounded_rect(mode_pill, 5, hover_pill_bg);
        }
        canvas.draw_mode_icon(
            mode_pill.x + 6,
            mode_pill.y + 5,
            mode_icon_kind,
            if mode_hov { t.text_primary } else { mode_accent },
        );
        canvas.draw_text_styled(
            mode_pill.x + 20,
            mode_pill.y + 3,
            mode_label,
            if mode_hov { t.text_primary } else { mode_accent },
            mw + 4,
            FontStyle::ChatMetaBold,
        );
        canvas.draw_chevron_down(
            mode_pill.x + mode_pill.w - 12,
            mode_pill.y + 9,
            t.text_muted,
        );
        self.agent_mode_pill_rect = mode_pill;
        self.hit_regions
            .push((mode_pill, HitAction::ToggleAgentModePopup));

        if is_busy {
            canvas.stroke_rounded_rect(
                send_btn,
                6,
                t.breakpoint_red,
                blend_rgb(composer_bg, t.breakpoint_red, 0.16),
            );
            canvas.fill_rounded_rect(
                Rect::new(send_btn.x + 7, send_btn.y + 7, 9, 9),
                2,
                t.breakpoint_red,
            );
            self.hit_regions
                .push((send_btn, HitAction::CancelAgentRequest));
        } else {
            let send_hov = send_btn.contains(hx, hy);
            let has_text = !self.agent_chat_input.is_empty()
                || !attached_items.is_empty()
                || !pasted_snippets.is_empty();
            canvas.fill_rounded_rect(
                send_btn,
                6,
                if has_text {
                    if send_hov { blend_rgb(t.accent_blue, 0xFFFFFF, 0.15) } else { t.accent_blue }
                } else {
                    blend_rgb(composer_bg, t.text_primary, if send_hov { 0.16 } else { 0.10 })
                },
            );
            canvas.draw_send_up_icon(
                send_btn.x + 5,
                send_btn.y + 5,
                if has_text || send_hov { 0xFFFFFF } else { t.text_secondary },
            );
            self.hit_regions
                .push((send_btn, HitAction::SubmitAgentChatPrompt));
        }

        let attach_hov = attach_btn.contains(hx, hy);
        if attach_hov {
            canvas.fill_rounded_rect(attach_btn, 5, hover_pill_bg);
        }
        canvas.draw_plus_icon(
            attach_btn.x + 5,
            attach_btn.y + 5,
            if attach_hov { t.text_primary } else { t.text_muted },
        );
        self.hit_regions
            .push((attach_btn, HitAction::AttachActiveFileToAgent));
    }

    pub(crate) fn render_agent_sessions_popup(&mut self, canvas: &mut SurfaceCanvas) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let sessions: Vec<(usize, String, usize, bool)> = self
            .agent_hub
            .sessions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let cnt = if i == self.agent_hub.active_session_idx {
                    self.agent_hub.messages.len()
                } else {
                    s.messages.len()
                };
                (i, s.title.clone(), cnt, s.is_busy)
            })
            .collect();
        let max_show = sessions.len().clamp(1, 10);
        let pw = 292.min((self.right_pane_rect.w - 16).max(220));
        let ph = 38 + (max_show as i32) * 28 + 34;
        let px = self.right_pane_rect.x + self.right_pane_rect.w - pw - 10;
        let py = self.right_pane_rect.y + 68;
        let box_rect = Rect::new(px, py, pw, ph);
        self.active_overlay_rects.push(box_rect);

        canvas.fill_rounded_shadow(box_rect, 8);
        canvas.stroke_rounded_rect(box_rect, 8, t.accent_blue, t.popup_bg);
        canvas.draw_text_styled(
            px + 12,
            py + 9,
            self.tr("全部 AI 会话列表", "All AI Sessions"),
            t.text_muted,
            pw - 24,
            FontStyle::UiSmall,
        );

        let mut ry = py + 32;
        for (idx, title, msg_cnt, busy) in sessions.into_iter().take(max_show) {
            let row = Rect::new(px + 6, ry, pw - 12, 26);
            let is_cur = idx == self.agent_hub.active_session_idx;
            if is_cur || row.contains(hx, hy) {
                canvas.fill_rounded_rect(row, 5, t.popup_hover_bg);
            }
            canvas.fill_circle(
                row.x + 11,
                row.y + 13,
                3,
                if busy {
                    t.accent_blue
                } else if is_cur {
                    t.run_green
                } else {
                    t.text_muted
                },
            );
            canvas.draw_text_styled(
                row.x + 22,
                row.y + 4,
                &title,
                if is_cur { 0xFFFFFF } else { t.text_primary },
                row.w - 86,
                if is_cur { FontStyle::UiBold } else { FontStyle::UiRegular },
            );
            canvas.draw_text_styled(
                row.x + row.w - 62,
                row.y + 4,
                &format!("{msg_cnt}条"),
                t.text_muted,
                36,
                FontStyle::UiSmall,
            );
            self.hit_regions
                .push((row, HitAction::SelectAgentSession(idx)));

            let close_btn = Rect::new(row.x + row.w - 22, row.y + 4, 18, 18);
            let chov = close_btn.contains(hx, hy);
            canvas.draw_text_centered(
                close_btn,
                "×",
                if chov { t.breakpoint_red } else { t.text_muted },
                FontStyle::UiSmall,
            );
            self.hit_regions
                .push((close_btn, HitAction::CloseAgentSession(idx)));
            ry += 28;
        }

        let new_row = Rect::new(px + 8, py + ph - 30, pw - 16, 24);
        let nhov = new_row.contains(hx, hy);
        canvas.stroke_rounded_rect(
            new_row,
            5,
            t.accent_blue,
            if nhov {
                t.accent_blue
            } else {
                blend_rgb(t.popup_bg, t.accent_blue, 0.16)
            },
        );
        canvas.draw_text_centered(
            new_row,
            self.tr("+ 新建 AI 会话", "+ Create New Session"),
            0xFFFFFF,
            FontStyle::UiBold,
        );
        self.hit_regions.push((new_row, HitAction::NewAgentSession));
    }

    pub(crate) fn render_image_preview_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let Some(img_path) = self.active_image_preview.clone() else {
            return;
        };
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        canvas.fill_rect_alpha(Rect::new(0, 0, w, h), 0x090A0D, 180);

        let mw = ((w as f32 * 0.82).round() as i32).clamp(320, 960);
        let mh = ((h as f32 * 0.82).round() as i32).clamp(240, 720);
        let mx = (w - mw) / 2;
        let my = (h - mh) / 2;
        let modal = Rect::new(mx, my, mw, mh);
        self.active_overlay_rects.push(modal);
        self.hit_regions
            .push((Rect::new(0, 0, w, h), HitAction::CloseImagePreview));
        self.hit_regions
            .push((modal, HitAction::OpenImagePreview(img_path.clone())));

        canvas.fill_rounded_shadow(modal, 10);
        canvas.stroke_rounded_rect(modal, 10, t.accent_blue, t.popup_bg);

        let header = Rect::new(mx + 2, my + 2, mw - 4, 36);
        canvas.fill_rounded_rect(header, 8, t.panel_header_bg);

        let decoded = self.get_or_load_image_preview(&img_path);
        let fname = img_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image");
        let title = if let Some(ref im) = decoded {
            format!("🖼 {fname}  ({} × {} px)", im.orig_width, im.orig_height)
        } else {
            format!("🖼 {fname}")
        };
        canvas.draw_text_styled(
            mx + 14,
            my + 10,
            &title,
            t.text_primary,
            mw - 104,
            FontStyle::UiBold,
        );

        let close_btn = Rect::new(mx + mw - 78, my + 7, 66, 26);
        let chov = close_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            close_btn,
            5,
            if chov { t.accent_blue } else { t.border_color },
            if chov { t.popup_hover_bg } else { t.window_bg },
        );
        canvas.draw_text_centered(
            close_btn,
            self.tr("关闭 (Esc)", "Close (Esc)"),
            if chov { 0xFFFFFF } else { t.text_primary },
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((close_btn, HitAction::CloseImagePreview));

        let body_rect = Rect::new(mx + 14, my + 46, mw - 28, mh - 82);
        if let Some(im) = decoded {
            canvas.draw_rgba_image_fit(
                body_rect,
                6,
                im.width,
                im.height,
                &im.rgba,
                t.window_bg,
                t.border_color,
            );
        } else {
            canvas.stroke_rounded_rect(body_rect, 6, t.border_color, t.window_bg);
            canvas.draw_text_centered(
                body_rect,
                self.tr("无法解码该图片格式", "Unable to decode this image format"),
                t.warning_yellow,
                FontStyle::UiRegular,
            );
        }

        let path_txt = img_path.display().to_string();
        canvas.draw_text_styled(
            mx + 14,
            my + mh - 26,
            &path_txt,
            t.text_muted,
            mw - 28,
            FontStyle::CodeMono,
        );
    }

    pub(crate) fn render_drag_ghost_overlay(&self, canvas: &mut SurfaceCanvas) {
        let Some((ref path, (cx, cy))) = self.active_ide_drag else {
            return;
        };
        let t = self.theme;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        let over_agent = self.right_agent_open && self.right_pane_rect.contains(cx, cy);
        let label = if over_agent {
            format!("📎 附加 `{name}`")
        } else {
            format!("📄 {name}")
        };
        let (tw, _) = canvas.measure_text(&label, FontStyle::UiSmall);
        let pill = Rect::new(cx + 12, cy + 12, (tw + 20).clamp(72, 220), 24);
        canvas.fill_rounded_shadow(pill, 6);
        canvas.stroke_rounded_rect(
            pill,
            6,
            t.accent_blue,
            if over_agent { t.popup_hover_bg } else { t.popup_bg },
        );
        canvas.draw_text_centered(pill, &label, 0xFFFFFF, FontStyle::UiSmall);
    }

    pub(crate) fn render_agent_provider_popup(&mut self, canvas: &mut SurfaceCanvas) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let providers = self.agent_hub.providers.clone();
        let pw = 372.min((self.right_pane_rect.w - 16).max(260));
        let ph = 42 + (providers.len() as i32) * 28;
        let px = (self.right_pane_rect.x + 12).min(
            (self.right_pane_rect.x + self.right_pane_rect.w - pw - 10).max(self.right_pane_rect.x + 10),
        );
        let py = if self.agent_provider_pill_rect.h > 0 {
            (self.agent_provider_pill_rect.y - ph - 6).max(46)
        } else {
            (self.right_pane_rect.y + self.right_pane_rect.h - ph - 44).max(46)
        };
        let box_rect = Rect::new(px, py, pw, ph);
        self.active_overlay_rects.push(box_rect);
        canvas.fill_rounded_shadow(box_rect, 8);
        canvas.stroke_rounded_rect(
            box_rect,
            8,
            blend_rgb(t.border_color, t.accent_blue, 0.45),
            t.popup_bg,
        );
        canvas.draw_text_styled(
            px + 12,
            py + 9,
            self.tr(
                "切换本地 CLI 智能体 (自动获取模型)",
                "Select Local CLI Agent (Auto-models)",
            ),
            t.text_muted,
            pw - 134,
            FontStyle::ChatMeta,
        );
        let hub_pill = Rect::new(px + pw - 122, py + 6, 50, 20);
        let hub_hov = hub_pill.contains(hx, hy);
        canvas.stroke_rounded_rect(
            hub_pill,
            4,
            if hub_hov { t.accent_blue } else { t.border_color },
            t.window_bg,
        );
        canvas.draw_text_centered(
            hub_pill,
            self.tr("总览", "Hub"),
            if hub_hov { t.accent_blue } else { t.text_secondary },
            FontStyle::ChatMeta,
        );
        self.hit_regions
            .push((hub_pill, HitAction::OpenAiConfigModal));

        let sync_pill = Rect::new(px + pw - 68, py + 6, 58, 20);
        let sync_hov = sync_pill.contains(hx, hy);
        canvas.stroke_rounded_rect(
            sync_pill,
            4,
            if sync_hov { t.accent_blue } else { t.border_color },
            t.window_bg,
        );
        canvas.draw_text_centered(
            sync_pill,
            self.tr("↻ 刷新", "↻ Sync"),
            if sync_hov { t.accent_blue } else { t.text_secondary },
            FontStyle::ChatMeta,
        );
        self.hit_regions
            .push((sync_pill, HitAction::RefreshLocalCliAgents));

        let mut ry = py + 32;
        for reg in providers {
            let row = Rect::new(px + 6, ry, pw - 12, 26);
            let sel = reg.kind == self.agent_hub.active_provider;
            if sel || row.contains(hx, hy) {
                canvas.fill_rounded_rect(
                    row,
                    5,
                    if sel {
                        t.popup_hover_bg
                    } else {
                        blend_rgb(t.popup_bg, t.text_primary, 0.06)
                    },
                );
            }
            canvas.fill_circle(
                row.x + 10,
                row.y + 13,
                3,
                if reg.available {
                    t.run_green
                } else {
                    t.warning_yellow
                },
            );
            canvas.draw_text_styled(
                row.x + 20,
                row.y + 4,
                reg.kind.display_name(),
                if sel { 0xFFFFFF } else { t.text_primary },
                pw - 148,
                if sel { FontStyle::ChatBodyBold } else { FontStyle::ChatBody },
            );
            let badge_txt = if reg.available {
                reg.active_model
                    .rsplit('/')
                    .next()
                    .unwrap_or(&reg.active_model)
                    .to_string()
            } else {
                self.tr("未安装 CLI", "Missing CLI").to_string()
            };
            canvas.draw_text_styled(
                row.x + row.w - 122,
                row.y + 5,
                &badge_txt,
                if reg.available {
                    t.syn_function
                } else {
                    t.warning_yellow
                },
                116,
                FontStyle::ChatMeta,
            );
            self.hit_regions
                .push((row, HitAction::SelectAgentProvider(reg.kind)));
            ry += 28;
        }
    }

    pub(crate) fn render_agent_model_popup(&mut self, canvas: &mut SurfaceCanvas) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let models = self.agent_hub.available_models_for_active();
        let active_model = self.agent_hub.active_model();
        let resolved_model = self.agent_hub.active_resolved_cli_model();
        let cli_short = self.agent_hub.active_provider.short_name();
        let max_show = models.len().clamp(1, 12);
        let pw = 378.min((self.right_pane_rect.w - 16).max(260));
        let ph = 56 + (max_show as i32) * 25;
        let anchor_x = if self.agent_model_pill_rect.w > 0 {
            self.agent_model_pill_rect.x
        } else {
            self.right_pane_rect.x + 12
        };
        let min_px = self.right_pane_rect.x + 10;
        let max_px = (self.right_pane_rect.x + self.right_pane_rect.w - pw - 10).max(min_px);
        let px = anchor_x.clamp(min_px, max_px);
        let py = if self.agent_model_pill_rect.h > 0 {
            (self.agent_model_pill_rect.y - ph - 6).max(46)
        } else {
            (self.right_pane_rect.y + self.right_pane_rect.h - ph - 44).max(46)
        };
        let box_rect = Rect::new(px, py, pw, ph);
        self.active_overlay_rects.push(box_rect);
        canvas.fill_rounded_shadow(box_rect, 8);
        canvas.stroke_rounded_rect(
            box_rect,
            8,
            blend_rgb(t.border_color, t.accent_blue, 0.45),
            t.popup_bg,
        );
        canvas.draw_text_styled(
            px + 12,
            py + 7,
            &format!(
                "{} ({cli_short})",
                self.tr("本地 CLI 模型列表", "Local CLI Models")
            ),
            t.text_muted,
            pw - 86,
            FontStyle::ChatMeta,
        );
        canvas.draw_text_styled(
            px + 12,
            py + 24,
            &format!("{}: {resolved_model}", self.tr("实际调用 ID", "Resolved ID")),
            t.syn_function,
            pw - 24,
            FontStyle::ChatCode,
        );
        let sync_pill = Rect::new(px + pw - 68, py + 6, 58, 20);
        let sync_hov = sync_pill.contains(hx, hy);
        canvas.stroke_rounded_rect(
            sync_pill,
            4,
            if sync_hov { t.accent_blue } else { t.border_color },
            t.window_bg,
        );
        canvas.draw_text_centered(
            sync_pill,
            self.tr("↻ 同步", "↻ Sync"),
            if sync_hov { t.accent_blue } else { t.text_secondary },
            FontStyle::ChatMeta,
        );
        self.hit_regions
            .push((sync_pill, HitAction::RefreshLocalCliAgents));

        let mut ry = py + 44;
        for m in models.into_iter().take(max_show) {
            let row = Rect::new(px + 6, ry, pw - 12, 23);
            let sel = m.eq_ignore_ascii_case(&active_model);
            if sel || row.contains(hx, hy) {
                canvas.fill_rounded_rect(
                    row,
                    5,
                    if sel {
                        t.popup_hover_bg
                    } else {
                        blend_rgb(t.popup_bg, t.text_primary, 0.06)
                    },
                );
            }
            let prefix = if sel { "✓ " } else { "  " };
            canvas.draw_text_styled(
                row.x + 10,
                row.y + 3,
                &format!("{prefix}{m}"),
                if sel { 0xFFFFFF } else { t.text_primary },
                pw - 28,
                if sel { FontStyle::ChatCodeBold } else { FontStyle::ChatCode },
            );
            self.hit_regions
                .push((row, HitAction::SelectAgentModel(m)));
            ry += 25;
        }
    }

    pub(crate) fn render_agent_mode_popup(&mut self, canvas: &mut SurfaceCanvas) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let modes = self.agent_hub.supported_modes_for_active();
        let active_mode = self.agent_hub.normalize_mode_for_active(self.agent_mode);
        let cli_short = self.agent_hub.active_provider.short_name();

        let pw = 324.min((self.right_pane_rect.w - 16).max(240));
        let row_h = 46;
        let ph = 38 + (modes.len() as i32) * row_h + 6;
        let anchor_x = if self.agent_mode_pill_rect.w > 0 {
            self.agent_mode_pill_rect.x
        } else {
            self.right_pane_rect.x + self.right_pane_rect.w - pw - 12
        };
        let min_px = self.right_pane_rect.x + 10;
        let max_px = (self.right_pane_rect.x + self.right_pane_rect.w - pw - 10).max(min_px);
        let px = anchor_x.clamp(min_px, max_px);
        let anchor_y = if self.agent_mode_pill_rect.h > 0 {
            self.agent_mode_pill_rect.y - ph - 6
        } else {
            self.right_pane_rect.y + self.right_pane_rect.h - ph - 46
        };
        let py = anchor_y.max(46);

        let box_rect = Rect::new(px, py, pw, ph);
        self.active_overlay_rects.push(box_rect);
        canvas.fill_rounded_shadow(box_rect, 8);
        canvas.stroke_rounded_rect(
            box_rect,
            8,
            blend_rgb(t.border_color, t.accent_blue, 0.45),
            t.popup_bg,
        );

        canvas.draw_text_styled(
            px + 12,
            py + 9,
            &format!("{} · {cli_short}", self.tr("工作模式", "Mode")),
            t.text_muted,
            pw - 24,
            FontStyle::ChatMeta,
        );
        canvas.fill_rect(
            Rect::new(px + 8, py + 30, pw - 16, 1),
            blend_rgb(t.border_color, t.popup_bg, 0.5),
        );

        let is_zh = matches!(self.ui_lang, UiLanguage::ZhCn);
        let mut ry = py + 36;
        for &mode in modes {
            let row = Rect::new(px + 6, ry, pw - 12, row_h - 2);
            let sel = mode == active_mode;
            let hov = row.contains(hx, hy);
            if sel || hov {
                canvas.fill_rounded_rect(
                    row,
                    6,
                    if sel {
                        t.popup_hover_bg
                    } else {
                        blend_rgb(t.popup_bg, t.text_primary, 0.06)
                    },
                );
            }

            let (icon_kind, mode_color) = match mode {
                AgentWorkbenchMode::Agent => (0u8, t.accent_blue),
                AgentWorkbenchMode::Edit => (1u8, t.run_green),
                AgentWorkbenchMode::Ask => (2u8, t.syn_function),
            };
            canvas.draw_mode_icon(
                row.x + 10,
                row.y + 8,
                icon_kind,
                if sel { 0xFFFFFF } else { mode_color },
            );
            canvas.draw_text_styled(
                row.x + 27,
                row.y + 4,
                mode.label(),
                if sel { 0xFFFFFF } else { t.text_primary },
                80,
                FontStyle::ChatBodyBold,
            );

            let flag_txt = self.agent_hub.mode_cli_flag_for_active(mode);
            let (fw, _) = canvas.measure_text(flag_txt, FontStyle::ChatCode);
            let badge_w = (fw + 12).min((row.w - 130).max(52));
            let badge_x = row.x + row.w - badge_w - 26;
            let badge_rect = Rect::new(badge_x, row.y + 5, badge_w, 17);
            canvas.fill_rounded_rect(
                badge_rect,
                4,
                if sel {
                    blend_rgb(t.popup_hover_bg, t.window_bg, 0.35)
                } else {
                    blend_rgb(t.popup_bg, t.window_bg, 0.55)
                },
            );
            canvas.draw_text_centered(
                badge_rect,
                flag_txt,
                if sel { t.text_primary } else { t.text_secondary },
                FontStyle::ChatCode,
            );

            if sel {
                canvas.draw_check_icon(row.x + row.w - 18, row.y + 8, 0xFFFFFF);
            }

            let desc = self.agent_hub.mode_description_for_active(mode, is_zh);
            canvas.draw_text_styled(
                row.x + 27,
                row.y + 24,
                desc,
                if sel { t.text_secondary } else { t.text_muted },
                row.w - 36,
                FontStyle::ChatMeta,
            );

            self.hit_regions
                .push((row, HitAction::SelectAgentMode(mode)));
            ry += row_h;
        }
    }

    pub(crate) fn render_ai_config_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let mw = 620.min(w - 40);
        let mh = 436.min(h - 60);
        let mx = (w - mw) / 2;
        let my = (h - mh) / 4;
        let modal = Rect::new(mx, my, mw, mh);
        self.active_overlay_rects.push(modal);

        canvas.fill_rounded_shadow(modal, 8);
        canvas.stroke_rounded_rect(modal, 8, t.accent_blue, t.popup_bg);

        let header = Rect::new(mx + 2, my + 2, mw - 4, 34);
        canvas.fill_rounded_rect(header, 6, t.panel_header_bg);
        canvas.draw_ai_sparkle_icon(mx + 14, my + 10, t.accent_blue);
        canvas.draw_text_styled(
            mx + 34,
            my + 9,
            self.tr(
                "本地 CLI 智能体与模型中心  (自动获取 · 无需配置基础地址/Key)",
                "Local CLI Agents & Models Hub  (Zero-Config Auto Discovery)",
            ),
            t.text_primary,
            mw - 108,
            FontStyle::UiBold,
        );

        let close_btn = Rect::new(mx + mw - 66, my + 7, 54, 24);
        canvas.stroke_rounded_rect(
            close_btn,
            5,
            t.border_color,
            if close_btn.contains(hx, hy) {
                t.editor_bg
            } else {
                t.window_bg
            },
        );
        canvas.draw_text_centered(
            close_btn,
            self.tr("关闭", "Close"),
            t.text_primary,
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((close_btn, HitAction::CloseModalOrPopup));

        canvas.draw_text_styled(
            mx + 16,
            my + 44,
            self.tr(
                "1. 本机 CLI 引擎状态 (点击切换活动 CLI):",
                "1. Detected Local CLI Engines (Click to switch):",
            ),
            t.syn_function,
            mw - 130,
            FontStyle::UiBold,
        );
        let sync_btn = Rect::new(mx + mw - 110, my + 41, 94, 24);
        let sync_hov = sync_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            sync_btn,
            5,
            if sync_hov { t.accent_blue } else { t.border_color },
            t.window_bg,
        );
        canvas.draw_text_centered(
            sync_btn,
            self.tr("↻ 重新扫描", "↻ Rescan"),
            if sync_hov { t.accent_blue } else { t.text_primary },
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((sync_btn, HitAction::RefreshLocalCliAgents));

        let providers = self.agent_hub.providers.clone();
        let mut ry = my + 70;
        for reg in providers {
            let row = Rect::new(mx + 16, ry, mw - 32, 30);
            let sel = reg.kind == self.agent_hub.active_provider;
            let hov = row.contains(hx, hy);
            canvas.stroke_rounded_rect(
                row,
                6,
                if sel || hov { t.accent_blue } else { t.border_color },
                if sel { t.popup_hover_bg } else { t.editor_bg },
            );
            canvas.fill_circle(
                row.x + 12,
                row.y + 15,
                4,
                if reg.available {
                    t.run_green
                } else {
                    t.warning_yellow
                },
            );
            canvas.draw_text_styled(
                row.x + 24,
                row.y + 6,
                reg.kind.display_name(),
                if sel { 0xFFFFFF } else { t.text_primary },
                180,
                if sel { FontStyle::UiBold } else { FontStyle::UiRegular },
            );
            canvas.draw_text_styled(
                row.x + 210,
                row.y + 7,
                &reg.status_text,
                if sel { 0xE2E8F0 } else { t.text_secondary },
                row.w - 220,
                FontStyle::CodeMono,
            );
            self.hit_regions
                .push((row, HitAction::SelectAgentProvider(reg.kind)));
            ry += 34;
        }

        let active_short = self.agent_hub.active_provider.short_name();
        let active_model = self.agent_hub.active_model();
        let resolved = self.agent_hub.active_resolved_cli_model();
        let models = self.agent_hub.available_models_for_active();
        canvas.draw_text_styled(
            mx + 16,
            ry + 8,
            &format!(
                "{} ({active_short} -> {resolved}):",
                self.tr("2. 自动获取模型", "2. Auto-discovered Models")
            ),
            t.syn_function,
            mw - 32,
            FontStyle::UiBold,
        );

        let mut px = mx + 16;
        let mut py = ry + 32;
        for m in models.iter().take(12) {
            let (lw, _) = canvas.measure_text(m, FontStyle::CodeMono);
            let btn_w = (lw + 22).clamp(88, mw - 32);
            if px + btn_w > mx + mw - 16 {
                px = mx + 16;
                py += 29;
            }
            if py + 28 > my + mh - 52 {
                break;
            }
            let mbtn = Rect::new(px, py, btn_w, 24);
            let is_cur = m.eq_ignore_ascii_case(&active_model);
            let hov = mbtn.contains(hx, hy);
            canvas.stroke_rounded_rect(
                mbtn,
                5,
                if is_cur || hov {
                    t.accent_blue
                } else {
                    t.border_color
                },
                if is_cur {
                    t.popup_hover_bg
                } else {
                    t.editor_bg
                },
            );
            canvas.draw_text_centered(
                mbtn,
                m,
                if is_cur { 0xFFFFFF } else { t.text_primary },
                FontStyle::CodeMono,
            );
            self.hit_regions
                .push((mbtn, HitAction::SelectAgentModel(m.clone())));
            px += btn_w + 8;
        }

        let save_btn = Rect::new(mx + mw - 188, my + mh - 40, 172, 28);
        canvas.fill_rounded_rect(save_btn, 6, t.accent_blue);
        canvas.draw_text_centered(
            save_btn,
            self.tr("完成 (Enter)", "Done (Enter)"),
            0xFFFFFF,
            FontStyle::UiBold,
        );
        self.hit_regions
            .push((save_btn, HitAction::SaveAiConfigModal));
    }

    pub(crate) fn copy_agent_message_to_clipboard(&mut self, msg_idx: usize) {
        if let Some(msg) = self.agent_hub.messages.get(msg_idx) {
            let text = msg.content.clone();
            write_windows_clipboard_text(&text);
            self.status_notification = self
                .tr("✓ 已复制整条消息到剪贴板", "✓ Copied message to clipboard")
                .to_string();
        }
    }
}
