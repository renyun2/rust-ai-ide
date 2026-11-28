use crate::canvas::{FontStyle, Rect, SurfaceCanvas};
use crate::ide_state::{
    blend_rgb, write_windows_clipboard_text, DockPane, FocusTarget, HitAction, IdeApplicationState,
    RightPanelTab, ScrollTrackInfo,
};
use crate::markdown::{
    draw_list_marker, draw_md_spans_text, heading_font_style, heading_row_height,
    hit_test_md_spans_col, layout_table_rows, list_marker_width, measure_md_spans_x,
    parse_markdown_blocks, project_md_spans_slice, render_prepared_table_row, table_row_height,
    wrap_md_line_segments, MdAlign, MdBlock, MdCalloutKind, MdListKind, PreparedTableRow,
};
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use crate::markdown::{MdSpan as ChatTextSpan, MdSpanKind as ChatSpanKind};
use ide_agent_hub::{
    AgentAttachedItem, AgentProviderKind, AgentWorkbenchMode, QueuedAgentPrompt,
    StreamingAgentProgress,
};
use ide_core::{LanguageId, ThemeKind, UiLanguage};
use ide_git::DiffLineKind;
use ide_syntax::{highlight_line_with_state, HighlightState, TokenKind};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) const AGENT_INPUT_FONT: FontStyle = FontStyle::ChatBody;
pub(crate) const AGENT_INPUT_LINE_H: i32 = 20;

/// 从 `start` 起能完整放进 `avail` 的标签数量。
fn session_tabs_fitting(start: usize, widths: &[i32], avail: i32) -> usize {
    if start >= widths.len() || avail <= 0 {
        return 0;
    }
    let mut used = 0;
    let mut count = 0;
    for width in widths.iter().skip(start) {
        let next = if count == 0 { *width } else { used + 4 + *width };
        if count > 0 && next > avail {
            break;
        }
        used = next;
        count += 1;
    }
    count
}

/// 能看到最后一个标签时的起始下标。再往右滚没有更多标签。
fn session_tab_scroll_including(target: usize, widths: &[i32], avail: i32) -> usize {
    if widths.is_empty() || avail <= 0 {
        return 0;
    }
    let target = target.min(widths.len() - 1);
    let mut start = target;
    let mut used = widths[target];
    while start > 0 {
        let prev = widths[start - 1];
        if used + 4 + prev > avail {
            break;
        }
        used += 4 + prev;
        start -= 1;
    }
    start
}

/// 只在目标标签不在当前窗口里时挪动滚动。已经看得见的标签保持原位，不会被挤到最右边。
fn session_tab_scroll_reveal(current: usize, target: usize, widths: &[i32], avail: i32) -> usize {
    if widths.is_empty() || avail <= 0 {
        return 0;
    }
    let last = widths.len() - 1;
    let target = target.min(last);
    let max_start = session_tab_scroll_including(last, widths, avail);
    let current = current.min(max_start);
    let count = session_tabs_fitting(current, widths, avail).max(1);
    if target >= current && target < current + count {
        return current;
    }
    if target < current {
        return target;
    }
    let mut start = current;
    while start < max_start {
        start += 1;
        let visible = session_tabs_fitting(start, widths, avail).max(1);
        if target < start + visible {
            return start;
        }
    }
    max_start
}

fn stream_progress_signature(prog: Option<&StreamingAgentProgress>) -> u64 {
    let Some(p) = prog else {
        return 0;
    };
    let mut h = DefaultHasher::new();
    p.phase.hash(&mut h);
    p.thought_text.hash(&mut h);
    p.execution_steps.hash(&mut h);
    p.reply_text.hash(&mut h);
    h.finish()
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
    HeadingLine {
        level: u8,
        text: String,
        spans: Vec<ChatTextSpan>,
        has_bottom_rule: bool,
        is_user: bool,
        bubble_w: i32,
    },
    ListItemLine {
        kind: MdListKind,
        text: String,
        spans: Vec<ChatTextSpan>,
        is_continuation: bool,
        marker_w: i32,
        is_user: bool,
        bubble_w: i32,
    },
    BlockquoteLine {
        depth: usize,
        callout: Option<MdCalloutKind>,
        is_callout_header: bool,
        text: String,
        spans: Vec<ChatTextSpan>,
        is_user: bool,
        bubble_w: i32,
    },
    HorizontalRule {
        is_user: bool,
        bubble_w: i32,
    },
    TableRow {
        prepared: PreparedTableRow,
        is_user: bool,
        bubble_w: i32,
    },
    ParaSpacer {
        is_user: bool,
        bubble_w: i32,
    },
    CodeBlockHeader {
        msg_idx: usize,
        blk_idx: usize,
        lang: String,
        code: Arc<str>,
        line_count: usize,
        collapsed: bool,
        user_bubble_w: Option<i32>,
    },
    CodeBlockCollapsedSummary {
        msg_idx: usize,
        blk_idx: usize,
        preview: String,
        line_count: usize,
        user_bubble_w: Option<i32>,
    },
    CodeBlockLine {
        text: String,
        spans: Arc<Vec<ide_syntax::HighlightSpan>>,
        user_bubble_w: Option<i32>,
    },
    TurnFilesToggle {
        msg_idx: usize,
        file_count: usize,
        added_lines: usize,
        removed_lines: usize,
        expanded: bool,
    },
    TurnChangedFileRow {
        msg_idx: usize,
        path: PathBuf,
        display: String,
        added_lines: usize,
        removed_lines: usize,
    },
    LiveThoughtLine(String),
    LiveStepLine(String),
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
            Self::UserBubbleTop { .. } | Self::UserBubbleBottom { .. } => 9,
            Self::UserFilesRow { .. } => 26,
            Self::UserImageCard { .. } => 86,
            Self::ThoughtToggle { .. } => 22,
            Self::ThoughtLine(_) | Self::LiveThoughtLine(_) | Self::LiveStepLine(_) => 19,
            Self::CodeBlockHeader { .. } => 26,
            Self::CodeBlockCollapsedSummary { .. } => 24,
            Self::CodeBlockLine { .. } => 19,
            Self::TurnFilesToggle { .. } | Self::TurnChangedFileRow { .. } => 21,
            Self::PatchHeader { .. } | Self::PatchButtons { .. } => 24,
            Self::PatchDiffLine { .. } => 20,
            Self::BusyIndicator => 26,
            Self::ParaSpacer { .. } => 9,
            Self::Spacer => 14,
            Self::UserLine { .. } => 24,
            Self::AgentLine { .. } | Self::StreamingLine { .. } => 23,
            Self::HeadingLine { level, .. } => heading_row_height(*level),
            Self::ListItemLine { is_user, .. } => if *is_user { 24 } else { 23 },
            Self::BlockquoteLine { .. } => 22,
            Self::HorizontalRule { .. } => 14,
            Self::TableRow { prepared, .. } => table_row_height(prepared.line_count),
        }
    }

    pub(crate) fn copyable_text(&self) -> Option<&str> {
        match self {
            Self::UserLine { text, .. }
            | Self::AgentLine { text, .. }
            | Self::HeadingLine { text, .. }
            | Self::ListItemLine { text, .. }
            | Self::BlockquoteLine { text, .. }
            | Self::ThoughtLine(text)
            | Self::LiveThoughtLine(text)
            | Self::LiveStepLine(text)
            | Self::StreamingLine { text, .. }
            | Self::CodeBlockLine { text, .. } => Some(text.as_str()),
            Self::PatchDiffLine { text, .. } => Some(text.as_str()),
            Self::TurnChangedFileRow { display, .. } => Some(display.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedChatMessage {
    pub blocks: Vec<MdBlock>,
    pub has_code_fence: bool,
    pub has_table: bool,
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
    pub parsed_session_id: String,
    pub parsed_msg_count: usize,
    pub parsed_content_sig: u64,
    pub parsed_workspace_root: PathBuf,
    pub parsed_msgs: Vec<ParsedChatMessage>,
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
            parsed_session_id: String::new(),
            parsed_msg_count: usize::MAX,
            parsed_content_sig: 0,
            parsed_workspace_root: PathBuf::new(),
            parsed_msgs: Vec::new(),
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
    pub(crate) fn max_scroll_row_for_height(&self, viewport_h: i32) -> usize {
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
            if acc > 0 && acc + rh > viewport_h {
                break;
            }
            acc += rh;
            cnt += 1;
        }
        cnt.max(1)
    }
}

/// Width-independent pre-parsing of a chat message body using the full Markdown block/inline parser.
/// Cached across `wrap_w` changes so splitter dragging never re-runs filesystem file-ref resolution.
fn parse_message_body(root: &std::path::Path, content: &str) -> ParsedChatMessage {
    let blocks = parse_markdown_blocks(root, None, content);
    let has_code_fence = blocks
        .iter()
        .any(|b| matches!(b, MdBlock::CodeBlock { .. }));
    let has_table = blocks.iter().any(|b| matches!(b, MdBlock::Table { .. }));
    ParsedChatMessage {
        blocks,
        has_code_fence,
        has_table,
    }
}

/// 将预解析后的聊天消息按当前 `wrap_w` 折行并输出至 `out`
#[allow(clippy::too_many_arguments)]
fn append_message_body_rows_from_parsed(
    canvas: &SurfaceCanvas,
    msg_idx: usize,
    parsed: &ParsedChatMessage,
    wrap_w: i32,
    is_user: bool,
    is_streaming: bool,
    bubble_w: i32,
    collapse_overrides: &std::collections::HashMap<(usize, usize), bool>,
    out: &mut Vec<CachedChatRow>,
) {
    let mut blk_idx = 0usize;
    let user_bubble_w = if is_user { Some(bubble_w) } else { None };
    let code_wrap_w = if is_user {
        (bubble_w - 42).max(40)
    } else {
        (wrap_w - 20).max(40)
    };

    for block in &parsed.blocks {
        match block {
            MdBlock::Blank => {
                if !out.is_empty() {
                    out.push(CachedChatRow::ParaSpacer { is_user, bubble_w });
                }
            }
            MdBlock::Heading { level, content } => {
                let style = heading_font_style(*level);
                let eff_wrap_w = (wrap_w - content.pill_reserve).max(60);
                let segs = wrap_md_line_segments(canvas, &content.text, eff_wrap_w, style);
                let last_idx = segs.len().saturating_sub(1);
                for (idx, (seg_s, seg_e, seg_txt)) in segs.into_iter().enumerate() {
                    out.push(CachedChatRow::HeadingLine {
                        level: *level,
                        text: seg_txt,
                        spans: project_md_spans_slice(seg_s, seg_e, &content.spans),
                        has_bottom_rule: *level <= 2 && idx == last_idx && !is_user,
                        is_user,
                        bubble_w,
                    });
                }
            }
            MdBlock::ParagraphLine { content } => {
                let eff_wrap_w = (wrap_w - content.pill_reserve).max(60);
                for (seg_s, seg_e, seg_txt) in
                    wrap_md_line_segments(canvas, &content.text, eff_wrap_w, FontStyle::ChatBody)
                {
                    let spans = project_md_spans_slice(seg_s, seg_e, &content.spans);
                    if is_user {
                        out.push(CachedChatRow::UserLine {
                            text: seg_txt,
                            spans,
                            default_bold: false,
                            bubble_w,
                        });
                    } else if is_streaming {
                        out.push(CachedChatRow::StreamingLine {
                            text: seg_txt,
                            spans,
                        });
                    } else {
                        out.push(CachedChatRow::AgentLine {
                            text: seg_txt,
                            spans,
                            default_bold: false,
                        });
                    }
                }
            }
            MdBlock::ListItem { kind, content } => {
                let indent_px = (kind.indent() as i32) * 14;
                let marker_w = list_marker_width(canvas, *kind);
                let eff_wrap_w = (wrap_w - indent_px - marker_w - content.pill_reserve).max(60);
                for (idx, (seg_s, seg_e, seg_txt)) in
                    wrap_md_line_segments(canvas, &content.text, eff_wrap_w, FontStyle::ChatBody)
                        .into_iter()
                        .enumerate()
                {
                    out.push(CachedChatRow::ListItemLine {
                        kind: *kind,
                        text: seg_txt,
                        spans: project_md_spans_slice(seg_s, seg_e, &content.spans),
                        is_continuation: idx > 0,
                        marker_w,
                        is_user,
                        bubble_w,
                    });
                }
            }
            MdBlock::BlockquoteLine {
                depth,
                callout,
                is_callout_header,
                content,
            } => {
                let indent_px = (*depth as i32) * 10 + 14;
                let eff_wrap_w = (wrap_w - indent_px - content.pill_reserve).max(60);
                if *is_callout_header {
                    out.push(CachedChatRow::BlockquoteLine {
                        depth: *depth,
                        callout: *callout,
                        is_callout_header: true,
                        text: String::new(),
                        spans: Vec::new(),
                        is_user,
                        bubble_w,
                    });
                }
                if !content.text.is_empty() || !*is_callout_header {
                    for (seg_s, seg_e, seg_txt) in wrap_md_line_segments(
                        canvas,
                        &content.text,
                        eff_wrap_w,
                        FontStyle::ChatBody,
                    ) {
                        out.push(CachedChatRow::BlockquoteLine {
                            depth: *depth,
                            callout: *callout,
                            is_callout_header: false,
                            text: seg_txt,
                            spans: project_md_spans_slice(seg_s, seg_e, &content.spans),
                            is_user,
                            bubble_w,
                        });
                    }
                }
            }
            MdBlock::HorizontalRule => {
                out.push(CachedChatRow::HorizontalRule { is_user, bubble_w });
            }
            MdBlock::Table {
                alignments,
                headers,
                rows,
            } => {
                let tbl_w = if is_user {
                    (bubble_w - 26).max(120)
                } else {
                    (wrap_w - 8).max(120)
                };
                for prepared in layout_table_rows(canvas, alignments, headers, rows, tbl_w) {
                    out.push(CachedChatRow::TableRow {
                        prepared,
                        is_user,
                        bubble_w,
                    });
                }
            }
            MdBlock::CodeBlock {
                lang_tag,
                lang_id,
                full_code,
                lines,
                line_count,
                preview,
            } => {
                let default_collapsed = is_user || *line_count >= 28;
                let collapsed = if is_streaming {
                    false
                } else {
                    collapse_overrides
                        .get(&(msg_idx, blk_idx))
                        .copied()
                        .unwrap_or(default_collapsed)
                };
                out.push(CachedChatRow::CodeBlockHeader {
                    msg_idx,
                    blk_idx,
                    lang: lang_tag.clone(),
                    code: Arc::clone(full_code),
                    line_count: *line_count,
                    collapsed,
                    user_bubble_w,
                });
                if collapsed {
                    out.push(CachedChatRow::CodeBlockCollapsedSummary {
                        msg_idx,
                        blk_idx,
                        preview: preview.clone(),
                        line_count: *line_count,
                        user_bubble_w,
                    });
                } else {
                    let mut hl_state = HighlightState::default();
                    for cl in lines {
                        for (_, _, wl) in
                            wrap_md_line_segments(canvas, cl, code_wrap_w, FontStyle::ChatCode)
                        {
                            let spans = Arc::new(highlight_line_with_state(&wl, *lang_id, &mut hl_state));
                            out.push(CachedChatRow::CodeBlockLine {
                                text: wl,
                                spans,
                                user_bubble_w,
                            });
                        }
                    }
                }
                blk_idx += 1;
            }
        }
    }
}

#[inline]
fn user_bubble_rect(row_rect: Rect, right_edge_x: i32, bubble_w: i32) -> Rect {
    let bx = (right_edge_x - bubble_w).max(row_rect.x);
    let bw = bubble_w.min(right_edge_x - bx);
    Rect::new(bx, row_rect.y, bw, row_rect.h)
}

#[inline]
fn code_block_row_bounds(
    canvas: &mut SurfaceCanvas,
    row_rect: Rect,
    right_edge_x: i32,
    user_bubble_w: Option<i32>,
    user_bubble_bg: u32,
) -> (i32, i32) {
    if let Some(ubw) = user_bubble_w {
        let brect = user_bubble_rect(row_rect, right_edge_x, ubw);
        canvas.fill_rect(brect, user_bubble_bg);
        (brect.x + 10, (brect.w - 20).max(40))
    } else {
        (row_rect.x + 2, (row_rect.w - 4).max(40))
    }
}

fn measure_user_bubble_width_from_parsed(
    canvas: &SurfaceCanvas,
    msg: &ide_agent_hub::AgentChatMessage,
    parsed: &ParsedChatMessage,
    prose_max_content_w: i32,
    code_max_content_w: i32,
) -> i32 {
    let wide_block = parsed.has_code_fence || parsed.has_table;
    let max_content_w = if wide_block {
        code_max_content_w
    } else {
        prose_max_content_w
    };
    let mut max_w = if wide_block { code_max_content_w } else { 0i32 };
    for block in &parsed.blocks {
        let (content, base_style, extra_prefix) = match block {
            MdBlock::ParagraphLine { content } => (content, FontStyle::ChatBody, 0),
            MdBlock::Heading { level, content } => (content, heading_font_style(*level), 0),
            MdBlock::ListItem { kind, content } => (
                content,
                FontStyle::ChatBody,
                (kind.indent() as i32) * 14 + list_marker_width(canvas, *kind),
            ),
            MdBlock::BlockquoteLine { depth, content, .. } => {
                (content, FontStyle::ChatBody, (*depth as i32) * 10 + 14)
            }
            _ => continue,
        };
        let eff_wrap = (max_content_w - extra_prefix - content.pill_reserve).max(60);
        let (full_w, _) = canvas.measure_text(&content.text, base_style);
        if full_w <= eff_wrap {
            let w = if content.spans.is_empty() {
                full_w
            } else {
                measure_md_spans_x(canvas, &content.text, &content.spans, base_style, usize::MAX)
            };
            max_w = max_w.max(w + extra_prefix);
        } else {
            for (seg_s, seg_e, seg_txt) in
                wrap_md_line_segments(canvas, &content.text, eff_wrap, base_style)
            {
                let spans = project_md_spans_slice(seg_s, seg_e, &content.spans);
                let w = measure_md_spans_x(canvas, &seg_txt, &spans, base_style, usize::MAX);
                max_w = max_w.max(w + extra_prefix);
            }
        }
    }
    if !msg.attachments.is_empty() {
        max_w = max_w.max(240.min(max_content_w));
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
            }) => {
                let st = if *default_bold {
                    FontStyle::ChatBodyBold
                } else {
                    FontStyle::ChatBody
                };
                hit_test_md_spans_col(canvas, text, spans, st, rel_x)
            }
            Some(CachedChatRow::HeadingLine {
                level, text, spans, ..
            }) => hit_test_md_spans_col(canvas, text, spans, heading_font_style(*level), rel_x),
            Some(CachedChatRow::ListItemLine { text, spans, .. })
            | Some(CachedChatRow::BlockquoteLine { text, spans, .. })
            | Some(CachedChatRow::StreamingLine { text, spans }) => {
                hit_test_md_spans_col(canvas, text, spans, FontStyle::ChatBody, rel_x)
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
            let eff_style = if default_bold && matches!(style, FontStyle::ChatBody) {
                FontStyle::ChatBodyBold
            } else {
                style
            };
            let (w0, w1) = if !spans.is_empty()
                || matches!(
                    eff_style,
                    FontStyle::ChatBody
                        | FontStyle::ChatBodyBold
                        | FontStyle::ChatBodyItalic
                        | FontStyle::ChatBodyBoldItalic
                        | FontStyle::ChatH1
                        | FontStyle::ChatH2
                        | FontStyle::ChatH3
                ) {
                (
                    measure_md_spans_x(canvas, text, spans, eff_style, c0),
                    measure_md_spans_x(canvas, text, spans, eff_style, c1),
                )
            } else {
                let chars: Vec<char> = text.chars().collect();
                let p0: String = chars[..c0].iter().collect();
                let p1: String = chars[..c1].iter().collect();
                (
                    canvas.measure_text(&p0, eff_style).0,
                    canvas.measure_text(&p1, eff_style).0,
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

        let t = self.theme;
        draw_md_spans_text(
            canvas,
            &t,
            self.hover_pos,
            &mut self.hit_regions,
            row_rect,
            text_x,
            text_y,
            text,
            spans,
            base_color,
            max_w,
            base_style,
            MdAlign::Left,
        );
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
        spans: &[ide_syntax::HighlightSpan],
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
        self.arm_pane_header_drag(canvas, rect, DockPane::Agent);
        self.agent_chat_line_rects.clear();
        self.agent_session_tab_strip = Rect::new(0, 0, 0, 0);

        // 顶部简洁头部底带
        let top_hdr = Rect::new(rect.x + 1, rect.y + 1, rect.w - 2, 32);
        canvas.fill_rounded_rect(top_hdr, 7, blend_rgb(panel_surface, t.panel_bg, 0.55));

        let is_chat = self.right_panel_tab == RightPanelTab::Chat;
        let chg_cnt = self.git_engine.changed_files.len();

        // 分段 Tab：[✦ AI 助手] [± 更改 (N)]
        let seg_bg = Rect::new(rect.x + 22, rect.y + 4, 192, 24);
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
            if is_chat {
                t.text_primary
            } else {
                t.text_muted
            },
            58,
            if is_chat {
                FontStyle::ChatMetaBold
            } else {
                FontStyle::ChatMeta
            },
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
            if !is_chat {
                t.text_primary
            } else {
                t.text_muted
            },
        );
        canvas.draw_text_styled(
            tab2.x + 24,
            tab2.y + 2,
            &format!("{} ({chg_cnt})", self.tr("更改", "Changes")),
            if !is_chat {
                t.text_primary
            } else {
                t.text_muted
            },
            76,
            if !is_chat {
                FontStyle::ChatMetaBold
            } else {
                FontStyle::ChatMeta
            },
        );
        self.hit_regions
            .push((tab2, HitAction::SwitchRightPanelTab(RightPanelTab::Changes)));

        // 右上角克制扁平动作按钮（默认无边框，悬停亮起）：[✦ 改写] [↻] [清空] [↶]
        let btn_hover_bg = blend_rgb(t.panel_bg, t.text_primary, 0.08);
        let rb_btn = Rect::new(rect.x + rect.w - 30, rect.y + 5, 23, 22);
        if rb_btn.contains(hx, hy) {
            canvas.fill_rounded_rect(rb_btn, 5, btn_hover_bg);
        }
        canvas.draw_rollback_icon(
            rb_btn.x + 4,
            rb_btn.y + 4,
            if rb_btn.contains(hx, hy) {
                t.warning_yellow
            } else {
                t.text_muted
            },
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
            if clr_btn.contains(hx, hy) {
                t.text_primary
            } else {
                t.text_muted
            },
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
        if self.git_engine.has_loaded_snapshot && !self.git_engine.git_installed {
            let card = Rect::new(rect.x + 10, rect.y + 44, (rect.w - 20).max(100), 156);
            self.render_git_missing_card(canvas, card);
            return;
        }
        if self.git_engine.has_loaded_snapshot && !self.git_engine.is_git_repo {
            let card_h = (rect.h - 56).clamp(150, 210);
            let card = Rect::new(rect.x + 10, rect.y + 44, (rect.w - 20).max(100), card_h);
            self.render_git_not_initialized_card(canvas, card);
            return;
        }
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
        self.hit_regions
            .push((ref_btn, HitAction::RefreshGitStatus));

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
            canvas.draw_path_file_icon(row.x + 4, y + 4, &ch.path);
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
            canvas.draw_text_ellipsis(
                row.x + 22,
                y + 3,
                &label,
                status_color,
                row.w - 28,
                FontStyle::UiSmall,
            );
            self.hit_regions
                .push((row, HitAction::InspectGitWorkingFileDiff(ch.path.clone())));

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
        let new_label = self.tr("+ 新会话", "+ New").to_string();
        let (new_tw, _) = canvas.measure_text(&new_label, FontStyle::ChatMetaBold);
        let new_btn_w = (new_tw + 16).clamp(72, 148);
        let new_btn = Rect::new(
            sess_strip.x + sess_strip.w - new_btn_w,
            sess_bar_y + 2,
            new_btn_w,
            22,
        );
        let all_btn = Rect::new(new_btn.x - all_btn_w - 4, sess_bar_y + 2, all_btn_w, 22);

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
            &new_label,
            if new_hov {
                t.text_primary
            } else {
                t.accent_blue
            },
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
            if all_hov {
                t.text_primary
            } else {
                t.text_muted
            },
            FontStyle::ChatMeta,
        );
        self.hit_regions
            .push((all_btn, HitAction::ToggleAgentSessionsPopup));

        // 水平渲染会话 Tabs。标签在「新会话」左侧，滚轮可横向翻页，最新会话排在最后。
        let tabs_avail_w = (all_btn.x - sess_strip.x - 6).max(40);
        let tabs_fit_w = (all_btn.x - 2 - sess_strip.x).max(40);
        self.agent_session_tab_strip = Rect::new(sess_strip.x, sess_bar_y, tabs_avail_w, sess_bar_h);
        let tab_widths: Vec<i32> = self
            .agent_hub
            .sessions
            .iter()
            .map(|session| {
                let (tw, _) = canvas.measure_text(&session.title, FontStyle::ChatMeta);
                (tw + 32).clamp(64, 138)
            })
            .collect();
        if let Some(reveal) = self.agent_session_tab_reveal.take() {
            self.agent_session_tab_scroll = session_tab_scroll_reveal(
                self.agent_session_tab_scroll,
                reveal,
                &tab_widths,
                tabs_fit_w,
            );
        }
        let max_scroll = session_tab_scroll_including(
            tab_widths.len().saturating_sub(1),
            &tab_widths,
            tabs_fit_w,
        );
        self.agent_session_tab_max_scroll = max_scroll;
        if self.agent_session_tab_scroll > max_scroll {
            self.agent_session_tab_scroll = max_scroll;
        }
        let fit_count = session_tabs_fitting(self.agent_session_tab_scroll, &tab_widths, tabs_fit_w);
        let sess_meta: Vec<(usize, String, bool, i32)> = self
            .agent_hub
            .sessions
            .iter()
            .enumerate()
            .skip(self.agent_session_tab_scroll)
            .take(fit_count)
            .map(|(i, s)| (i, s.title.clone(), s.is_busy, tab_widths[i]))
            .collect();
        let mut used = 0;
        for (i, (_, _, _, tab_w)) in sess_meta.iter().enumerate() {
            used += if i == 0 { *tab_w } else { 4 + *tab_w };
        }
        let mut sx = (all_btn.x - 6 - used).max(sess_strip.x);
        for (idx, title, sess_busy, tab_w) in sess_meta {
            let is_cur = idx == active_sess_idx;
            if sx + tab_w > all_btn.x - 2 {
                break;
            }
            let tab_rect = Rect::new(sx, sess_bar_y + 2, tab_w, 22);
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
                if is_cur {
                    FontStyle::ChatMetaBold
                } else {
                    FontStyle::ChatMeta
                },
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
        let pasted_chips: Vec<(String, String, usize)> = self
            .agent_hub
            .active_session()
            .pasted_snippets
            .iter()
            .map(|s| (s.id.clone(), s.language.clone(), s.line_count))
            .collect();
        let has_images_in_tray = attached_items.iter().any(|it| it.is_image);
        let has_tray = !attached_items.is_empty() || !pasted_chips.is_empty();
        let tray_h = if has_images_in_tray {
            58
        } else if has_tray {
            28
        } else {
            0
        };
        let expanded_snip = self.expanded_composer_snippet_id.as_ref().and_then(|eid| {
            self.agent_hub
                .active_session()
                .pasted_snippets
                .iter()
                .find(|s| &s.id == eid)
                .cloned()
        });
        let drawer_h = if let Some(ref snip) = expanded_snip {
            let vis = snip.line_count.clamp(2, 5) as i32;
            24 + vis * 16 + 6
        } else {
            0
        };
        let edit_banner_h = if self.editing_sent_msg_idx.is_some() {
            24
        } else {
            0
        };
        let composer_h = 125 + tray_h + drawer_h + edit_banner_h;
        let queued_prompts = self.agent_hub.active_session().queued_prompts.clone();
        let queue_rows = queued_prompts.len().min(4) as i32;
        let queue_h = if queued_prompts.is_empty() {
            0
        } else {
            24 + queue_rows * 46
        };
        let composer_rect = Rect::new(
            rect.x + 10,
            rect.y + rect.h - composer_h - 10,
            rect.w - 20,
            composer_h,
        );
        let queue_rect = Rect::new(
            composer_rect.x,
            composer_rect.y - queue_h - if queue_h > 0 { 6 } else { 0 },
            composer_rect.w,
            queue_h,
        );

        let supports_rewind = self.agent_hub.active_cli_supports_checkpoint_rewind();
        let session_changed_files: Vec<(PathBuf, usize, usize)> = self
            .agent_hub
            .active_session_changed_files()
            .iter()
            .map(|c| (c.path.clone(), c.added_lines, c.removed_lines))
            .collect();
        let files_drawer_h = if !session_changed_files.is_empty() {
            if self.agent_changed_files_expanded {
                let vis_rows = session_changed_files.len().min(5) as i32;
                28 + vis_rows * 22 + 4
            } else {
                26
            }
        } else {
            0
        };
        let stack_top = if queue_h > 0 {
            queue_rect.y
        } else {
            composer_rect.y
        };
        let chat_bottom_limit = if files_drawer_h > 0 {
            stack_top - files_drawer_h - 6
        } else {
            stack_top
        };

        // 2. 中部可滚动对话区（去掉多余常驻快捷方块，保持充足阅读空间）
        let chat_top = sess_bar_y + sess_bar_h + 6;
        let chat_h = (chat_bottom_limit - chat_top - 8).max(40);
        let chat_area = Rect::new(rect.x + 8, chat_top, rect.w - 16, chat_h);
        self.agent_chat_area_rect = chat_area;
        let raw_wrap_w = (chat_area.w - 28).max(80);
        let wrap_w = if self.active_splitter_drag.is_some() {
            ((raw_wrap_w / 12) * 12).max(80)
        } else {
            raw_wrap_w
        };

        self.hit_regions
            .push((chat_area, HitAction::FocusAgentChatStream));

        let is_busy = self.agent_hub.is_active_session_busy();
        let stream_progress = self.agent_hub.streaming_progress();
        let stream_sig = stream_progress_signature(stream_progress.as_ref());
        let msg_count = self.agent_hub.messages.len();
        let session_id = self.agent_hub.active_session().id.clone();

        // 若消息包含图片，确保在缓存构建/渲染前加载图像解码（已缓存时零分配跳过）
        let has_uncached_img = self.agent_hub.messages.iter().any(|m| {
            m.attachments
                .iter()
                .any(|a| a.is_image && !self.image_preview_cache.contains_key(&a.path))
        });
        if has_uncached_img {
            let image_paths: Vec<PathBuf> = self
                .agent_hub
                .messages
                .iter()
                .flat_map(|m| {
                    m.attachments
                        .iter()
                        .filter(|a| a.is_image && !self.image_preview_cache.contains_key(&a.path))
                        .map(|a| a.path.clone())
                })
                .collect();
            for img_p in image_paths {
                let _ = self.get_or_load_image_preview(&img_p);
            }
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
                let mf_bit = (m.modified_files.len() as u64).wrapping_mul(131)
                    ^ (m.file_changes.len() as u64).wrapping_mul(523)
                    ^ m.file_changes
                        .iter()
                        .fold(0u64, |a, fc| a.wrapping_add((fc.added_lines as u64) * 17 + (fc.removed_lines as u64) * 31));
                let att_bit = (m.attachments.len() as u64).wrapping_mul(977);
                p_bit ^ mf_bit ^ att_bit
            })
            .fold(
                self.agent_chat_signature_salt
                    ^ (u64::from(supports_rewind) << 40)
                    ^ (self.editing_sent_msg_idx.unwrap_or(usize::MAX) as u64)
                        .wrapping_mul(2654435761),
                |acc, x| acc.wrapping_add(x),
            );
        let scale_bits = canvas.scale_factor.to_bits();

        if msg_count == 0 && !is_busy {
            self.cached_chat_rows.session_id = session_id;
            self.cached_chat_rows.msg_count = 0;
            self.cached_chat_rows.signature = applied_sig;
            self.cached_chat_rows.wrap_w = wrap_w;
            self.cached_chat_rows.scale_bits = scale_bits;
            self.cached_chat_rows.is_busy = false;
            self.cached_chat_rows.rows.clear();
            self.cached_chat_rows.row_tops.clear();
            self.cached_chat_rows.total_height = 0;
            self.cached_chat_rows.parsed_msg_count = 0;
            self.cached_chat_rows.parsed_msgs.clear();
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
                stream_progress,
                stream_sig,
            );
        }

        if files_drawer_h > 0 {
            let drawer_rect = Rect::new(
                composer_rect.x,
                stack_top - files_drawer_h - 6,
                composer_rect.w,
                files_drawer_h,
            );
            self.render_agent_changed_files_bar(
                canvas,
                drawer_rect,
                &session_changed_files,
                supports_rewind,
            );
        }

        if queue_h > 0 {
            self.render_agent_wait_queue(canvas, queue_rect, &queued_prompts);
        }

        // 3. 绘制美观的底部 Composer 卡片（含附件托盘、图片直接缩略图预览、可折叠粘贴代码预览）
        self.render_agent_composer(
            canvas,
            composer_rect,
            &attached_items,
            &pasted_chips,
            expanded_snip.as_ref(),
            tray_h,
            drawer_h,
            edit_banner_h,
            is_busy,
        );

        // 4. 若外部资源管理器或内部/跨窗口文件树正在向 AI 面板拖拽文件/图片，渲染高质感虚线投放指引
        let drag_hint = self
            .active_ide_drag
            .as_ref()
            .filter(|(_, (dx, dy))| rect.contains(*dx, *dy))
            .map(|(p, _)| p.clone())
            .or_else(|| {
                self.external_drag_hover
                    .as_ref()
                    .filter(|(_, (dx, dy))| rect.contains(*dx, *dy))
                    .map(|(p, _)| p.clone())
            });
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
        let active_avail = self
            .agent_hub
            .providers
            .iter()
            .find(|p| p.kind == self.agent_hub.active_provider)
            .map(|p| p.available)
            .unwrap_or(false);
        canvas.draw_text_centered(
            Rect::new(card.x + 12, card.y + 76, card.w - 24, 18),
            &format!("{cli_short}  ·  {model}  ·  {}", self.agent_mode.label()),
            t.text_muted,
            FontStyle::ChatMeta,
        );

        let mut grid_y = card.y + 104;
        if !active_avail {
            let inst_w = (card.w - 36).min(230);
            let inst_btn = Rect::new(card.x + (card.w - inst_w) / 2, card.y + 98, inst_w, 26);
            let ihov = inst_btn.contains(hx, hy);
            canvas.stroke_rounded_rect(
                inst_btn,
                6,
                t.warning_yellow,
                if ihov {
                    t.accent_blue
                } else {
                    blend_rgb(t.panel_bg, t.warning_yellow, 0.16)
                },
            );
            let lbl = if self.cli_install_in_progress == Some(self.agent_hub.active_provider) {
                self.tr("⏳ 正在安装 CLI...", "⏳ Installing CLI...")
            } else if self.agent_hub.active_provider == AgentProviderKind::GoogleAntigravityAcp {
                self.tr("⬇ 前往官网下载 Antigravity CLI", "⬇ Get Antigravity CLI")
            } else {
                self.tr("⬇ 一键安装当前智能体 CLI", "⬇ Install Active Agent CLI")
            };
            canvas.draw_text_centered(
                inst_btn,
                lbl,
                if ihov { 0xFFFFFF } else { t.warning_yellow },
                FontStyle::ChatMetaBold,
            );
            self.hit_regions.push((
                inst_btn,
                HitAction::InstallAgentCli(self.agent_hub.active_provider),
            ));
            grid_y = card.y + 132;
        }

        let starters: [(&'static str, &'static str, &'static str); 4] = match self.ui_lang {
            UiLanguage::ZhCn => [
                (
                    "✦ 解释当前代码",
                    "分析核心流程",
                    "请解释当前光标所在代码块的功能与关键逻辑",
                ),
                (
                    "⚡ 生成单元测试",
                    "覆盖边界条件",
                    "请为当前函数生成 #[test] 单元测试补丁",
                ),
                (
                    "🛡 安全错误处理",
                    "清理 unwrap()",
                    "请将当前代码中的 unwrap() 替换为安全的 expect()/错误处理",
                ),
                ("🛠 检查工作区编译", "cargo check", "/check"),
            ],
            UiLanguage::EnUs => [
                (
                    "✦ Explain Code",
                    "Trace logic",
                    "Please explain the functionality and key logic of the code around cursor",
                ),
                (
                    "⚡ Unit Tests",
                    "Add #[test]",
                    "Please generate a #[test] unit test patch for the current function",
                ),
                (
                    "🛡 Safe Errors",
                    "Fix unwrap()",
                    "Please replace unwrap() calls with safe expect()/error handling",
                ),
                ("🛠 Workspace Check", "cargo check", "/check"),
            ],
        };

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
                    if hov {
                        t.accent_blue
                    } else {
                        blend_rgb(t.border_color, t.panel_bg, 0.45)
                    },
                    if hov {
                        t.popup_hover_bg
                    } else {
                        blend_rgb(t.panel_bg, t.editor_bg, 0.4)
                    },
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
        stream_progress: Option<StreamingAgentProgress>,
        stream_sig: u64,
    ) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let combined_sig = applied_sig ^ stream_sig.wrapping_mul(104_729);

        let cache_key_match = self.cached_chat_rows.session_id == session_id
            && self.cached_chat_rows.msg_count == msg_count
            && self.cached_chat_rows.signature == combined_sig
            && self.cached_chat_rows.wrap_w == wrap_w
            && self.cached_chat_rows.scale_bits == scale_bits
            && self.cached_chat_rows.is_busy == is_busy;

        if !cache_key_match {
            let mut ph = DefaultHasher::new();
            for m in &self.agent_hub.messages {
                m.role.hash(&mut ph);
                m.content.hash(&mut ph);
            }
            let parsed_sig = ph.finish();
            let parsed_match = self.cached_chat_rows.parsed_session_id == session_id
                && self.cached_chat_rows.parsed_msg_count == msg_count
                && self.cached_chat_rows.parsed_content_sig == parsed_sig
                && self.cached_chat_rows.parsed_workspace_root == self.workspace_root
                && self.cached_chat_rows.parsed_msgs.len() == msg_count;
            if !parsed_match {
                let parsed: Vec<ParsedChatMessage> = self
                    .agent_hub
                    .messages
                    .iter()
                    .map(|m| parse_message_body(&self.workspace_root, &m.content))
                    .collect();
                self.cached_chat_rows.parsed_session_id = session_id.to_string();
                self.cached_chat_rows.parsed_msg_count = msg_count;
                self.cached_chat_rows.parsed_content_sig = parsed_sig;
                self.cached_chat_rows.parsed_workspace_root = self.workspace_root.clone();
                self.cached_chat_rows.parsed_msgs = parsed;
            }

            let row_avail_w = (chat_area.w - 20).max(136);
            let user_code_max_content_w = (row_avail_w - 26).max(110);
            let user_prose_max_content_w = (((row_avail_w as f32) * 0.92) as i32 - 26)
                .clamp(110, user_code_max_content_w);
            let mut rebuilt: Vec<CachedChatRow> = Vec::with_capacity(msg_count * 7 + 6);
            for (msg_idx, msg) in self.agent_hub.messages.iter().enumerate() {
                let parsed_msg = &self.cached_chat_rows.parsed_msgs[msg_idx];
                if msg.role == "user" {
                    let bubble_w = measure_user_bubble_width_from_parsed(
                        canvas,
                        msg,
                        parsed_msg,
                        user_prose_max_content_w,
                        user_code_max_content_w,
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
                    append_message_body_rows_from_parsed(
                        canvas,
                        msg_idx,
                        parsed_msg,
                        (bubble_w - 26).max(60),
                        true,
                        false,
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
                            let one_line: String = thought
                                .lines()
                                .next()
                                .unwrap_or("")
                                .chars()
                                .take(48)
                                .collect();
                            rebuilt.push(CachedChatRow::ThoughtToggle {
                                msg_idx,
                                preview: one_line,
                                expanded,
                            });
                            if expanded {
                                for raw_tl in thought.lines() {
                                    for (_, _, tl) in wrap_md_line_segments(
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
                    append_message_body_rows_from_parsed(
                        canvas,
                        msg_idx,
                        parsed_msg,
                        (wrap_w - 8).max(60),
                        false,
                        false,
                        0,
                        &self.collapsed_code_blocks,
                        &mut rebuilt,
                    );
                    if msg.role == "assistant" {
                        let turn_changes: Vec<(PathBuf, usize, usize)> =
                            if !msg.file_changes.is_empty() {
                                msg.file_changes
                                    .iter()
                                    .map(|fc| (fc.path.clone(), fc.added_lines, fc.removed_lines))
                                    .collect()
                            } else {
                                msg.modified_files
                                    .iter()
                                    .map(|p| {
                                        let stats = self
                                            .agent_hub
                                            .active_session_changed_files()
                                            .iter()
                                            .find(|sc| {
                                                sc.path == *p
                                                    || sc
                                                        .path
                                                        .to_string_lossy()
                                                        .replace('/', "\\")
                                                        .eq_ignore_ascii_case(
                                                            &p.to_string_lossy().replace('/', "\\"),
                                                        )
                                            })
                                            .map(|sc| (sc.added_lines, sc.removed_lines))
                                            .unwrap_or((0, 0));
                                        (p.clone(), stats.0, stats.1)
                                    })
                                    .collect()
                            };
                        if !turn_changes.is_empty() {
                            let total_add: usize = turn_changes.iter().map(|c| c.1).sum();
                            let total_del: usize = turn_changes.iter().map(|c| c.2).sum();
                            let expanded = self
                                .expanded_message_file_changes
                                .get(&msg_idx)
                                .copied()
                                .unwrap_or(false);
                            rebuilt.push(CachedChatRow::TurnFilesToggle {
                                msg_idx,
                                file_count: turn_changes.len(),
                                added_lines: total_add,
                                removed_lines: total_del,
                                expanded,
                            });
                            if expanded {
                                for (path, added_lines, removed_lines) in turn_changes {
                                    let rel = path
                                        .strip_prefix(&self.workspace_root)
                                        .unwrap_or(&path)
                                        .to_string_lossy()
                                        .replace('\\', "/");
                                    rebuilt.push(CachedChatRow::TurnChangedFileRow {
                                        msg_idx,
                                        path,
                                        display: rel,
                                        added_lines,
                                        removed_lines,
                                    });
                                }
                            }
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

            if is_busy || stream_progress.is_some() {
                rebuilt.push(CachedChatRow::AgentHeader {
                    msg_idx: usize::MAX,
                    agent_name: self.agent_hub.active_provider.compact_name().to_string(),
                });
                if let Some(prog) = stream_progress {
                    if !prog.thought_text.trim().is_empty() {
                        let mut thought_lines = Vec::new();
                        for raw_tl in prog.thought_text.trim().lines() {
                            if raw_tl.trim().is_empty() {
                                continue;
                            }
                            for (_, _, tl) in wrap_md_line_segments(
                                canvas,
                                raw_tl.trim(),
                                (wrap_w - 22).max(60),
                                FontStyle::ChatMeta,
                            ) {
                                thought_lines.push(tl);
                            }
                        }
                        let start = thought_lines.len().saturating_sub(6);
                        for tl in thought_lines.into_iter().skip(start) {
                            rebuilt.push(CachedChatRow::LiveThoughtLine(tl));
                        }
                    }

                    if !prog.reply_text.trim().is_empty() {
                        let parsed_stream = parse_message_body(&self.workspace_root, &prog.reply_text);
                        append_message_body_rows_from_parsed(
                            canvas,
                            usize::MAX,
                            &parsed_stream,
                            (wrap_w - 14).max(60),
                            false,
                            true,
                            0,
                            &self.collapsed_code_blocks,
                            &mut rebuilt,
                        );
                    }

                    if !prog.execution_steps.is_empty() {
                        let skip_steps = prog.execution_steps.len().saturating_sub(6);
                        for step in prog.execution_steps.iter().skip(skip_steps) {
                            for (_, _, seg) in wrap_md_line_segments(
                                canvas,
                                step,
                                (wrap_w - 22).max(60),
                                FontStyle::ChatMeta,
                            ) {
                                rebuilt.push(CachedChatRow::LiveStepLine(seg));
                            }
                        }
                    } else if prog.thought_text.trim().is_empty()
                        && prog.reply_text.trim().is_empty()
                        && !prog.phase.trim().is_empty()
                    {
                        for (_, _, seg) in wrap_md_line_segments(
                            canvas,
                            prog.phase.trim(),
                            (wrap_w - 22).max(60),
                            FontStyle::ChatMeta,
                        ) {
                            rebuilt.push(CachedChatRow::LiveStepLine(seg));
                        }
                    }
                }
            }
            if is_busy {
                rebuilt.push(CachedChatRow::BusyIndicator);
            }

            let prev_max_s = self
                .cached_chat_rows
                .max_scroll_row_for_height(chat_area.h - 4);
            let was_at_bottom = self.agent_scroll == usize::MAX
                || (!self.cached_chat_rows.rows.is_empty() && self.agent_scroll >= prev_max_s)
                || (self.cached_chat_rows.msg_count == 0 && self.agent_scroll == 0);
            self.cached_chat_rows.session_id = session_id.to_string();
            self.cached_chat_rows.msg_count = msg_count;
            self.cached_chat_rows.signature = combined_sig;
            self.cached_chat_rows.wrap_w = wrap_w;
            self.cached_chat_rows.scale_bits = scale_bits;
            self.cached_chat_rows.is_busy = is_busy;
            self.cached_chat_rows.rows = rebuilt;
            self.cached_chat_rows.recompute_offsets();
            if was_at_bottom {
                self.agent_scroll = usize::MAX;
            }
        }

        let total_rows = self.cached_chat_rows.rows.len();
        let max_s = self
            .cached_chat_rows
            .max_scroll_row_for_height(chat_area.h - 4);
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
                    let show_edit = self.agent_hub.active_cli_supports_checkpoint_rewind()
                        && !self.agent_hub.is_active_session_busy();
                    let is_editing_this = show_edit && self.editing_sent_msg_idx == Some(*msg_idx);
                    let edit_w = if is_editing_this { 46 } else { 34 };
                    let total_btn_w = if show_edit { 34 + 4 + edit_w } else { 34 };
                    let bubble_left = (right_edge_x - *bubble_w).max(row_rect.x);
                    let btns_x = bubble_left.min(lbl_x - total_btn_w - 6).max(row_rect.x);

                    let copy_btn = Rect::new(btns_x, cy + 2, 34, 17);
                    let chov = copy_btn.contains(hx, hy);
                    if chov {
                        canvas.fill_rounded_rect(
                            copy_btn,
                            4,
                            blend_rgb(t.panel_bg, t.text_primary, 0.08),
                        );
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

                    if show_edit {
                        let edit_btn = Rect::new(copy_btn.x + 38, cy + 2, edit_w, 17);
                        if edit_btn.x + edit_btn.w <= lbl_x - 4 {
                            let ehov = edit_btn.contains(hx, hy);
                            if is_editing_this {
                                canvas.fill_rounded_rect(
                                    edit_btn,
                                    4,
                                    blend_rgb(t.panel_bg, t.accent_blue, 0.22),
                                );
                            } else if ehov {
                                canvas.fill_rounded_rect(
                                    edit_btn,
                                    4,
                                    blend_rgb(t.panel_bg, t.text_primary, 0.08),
                                );
                            }
                            canvas.draw_text_centered(
                                edit_btn,
                                if is_editing_this {
                                    self.tr("编辑中", "Editing")
                                } else {
                                    self.tr("编辑", "Edit")
                                },
                                if is_editing_this {
                                    t.accent_blue
                                } else if ehov {
                                    t.text_primary
                                } else {
                                    blend_rgb(t.editor_bg, t.text_muted, 0.55)
                                },
                                FontStyle::ChatMeta,
                            );
                            if !is_editing_this {
                                self.hit_regions.push((
                                    edit_btn,
                                    HitAction::StartEditSentUserMessage(*msg_idx),
                                ));
                            }
                        }
                    }
                }
                CachedChatRow::UserBubbleTop { bubble_w } => {
                    let brect = user_bubble_rect(row_rect, right_edge_x, *bubble_w);
                    canvas.fill_top_rounded_rect(
                        Rect::new(brect.x, cy, brect.w, draw_h),
                        9,
                        user_bubble_bg,
                    );
                }
                CachedChatRow::UserBubbleBottom { bubble_w } => {
                    let brect = user_bubble_rect(row_rect, right_edge_x, *bubble_w);
                    canvas.fill_bottom_rounded_rect(
                        Rect::new(brect.x, cy, brect.w, draw_h),
                        9,
                        user_bubble_bg,
                    );
                }
                CachedChatRow::UserLine {
                    text,
                    spans,
                    default_bold,
                    bubble_w,
                } => {
                    let brect = user_bubble_rect(row_rect, right_edge_x, *bubble_w);
                    canvas.fill_rect(brect, user_bubble_bg);
                    self.draw_chat_line_with_spans_and_selection(
                        canvas,
                        brect,
                        brect.x + 13,
                        cy + 2,
                        *global_row_idx,
                        text,
                        spans,
                        *default_bold,
                        prose_fg,
                        (brect.w - 24).max(40),
                        FontStyle::ChatBody,
                    );
                }
                CachedChatRow::UserFilesRow { files, bubble_w } => {
                    let brect = user_bubble_rect(row_rect, right_edge_x, *bubble_w);
                    canvas.fill_rect(brect, user_bubble_bg);
                    let mut fx = brect.x + 12;
                    let max_fx = brect.x + brect.w - 10;
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
                            if hov {
                                t.accent_blue
                            } else {
                                user_bubble_border
                            },
                            t.editor_bg,
                        );
                        canvas.draw_path_file_icon(chip.x + 5, chip.y + 3, path);
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
                    let brect = user_bubble_rect(row_rect, right_edge_x, *bubble_w);
                    canvas.fill_rect(brect, user_bubble_bg);
                    if cy + row_h <= chat_bottom + 8 {
                        let card = Rect::new(brect.x + 12, cy + 4, (brect.w - 24).max(60), row_h - 8);
                        let hov = card.contains(hx, hy);
                        canvas.stroke_rounded_rect(
                            card,
                            6,
                            if hov {
                                t.accent_blue
                            } else {
                                user_bubble_border
                            },
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
                            canvas.draw_text_centered(
                                thumb_rect,
                                "IMG",
                                t.text_muted,
                                FontStyle::ChatMeta,
                            );
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
                        let brect = user_bubble_rect(row_rect, right_edge_x, *bubble_w);
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
                            canvas.fill_rounded_rect(
                                copy_btn,
                                4,
                                blend_rgb(t.panel_bg, t.text_primary, 0.08),
                            );
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
                        canvas.fill_rounded_rect(
                            pill,
                            4,
                            blend_rgb(t.panel_bg, t.text_primary, 0.05),
                        );
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
                CachedChatRow::HeadingLine {
                    level,
                    text,
                    spans,
                    has_bottom_rule,
                    is_user,
                    bubble_w,
                } => {
                    let style = heading_font_style(*level);
                    let (tx, max_tw, container_rect) = if *is_user {
                        let brect = user_bubble_rect(row_rect, right_edge_x, *bubble_w);
                        canvas.fill_rect(brect, user_bubble_bg);
                        (brect.x + 13, (brect.w - 24).max(40), brect)
                    } else {
                        (row_rect.x + 4, wrap_w - 4, row_rect)
                    };
                    let color = match level {
                        1 => t.text_primary,
                        2 => blend_rgb(t.text_primary, t.accent_blue, 0.20),
                        3 => blend_rgb(t.text_primary, t.syn_function, 0.20),
                        _ => prose_fg,
                    };
                    self.draw_chat_line_with_spans_and_selection(
                        canvas,
                        container_rect,
                        tx,
                        cy + 3,
                        *global_row_idx,
                        text,
                        spans,
                        false,
                        color,
                        max_tw,
                        style,
                    );
                    if *has_bottom_rule {
                        canvas.fill_rect(
                            Rect::new(tx, cy + row_h - 3, max_tw, 1),
                            blend_rgb(t.border_color, t.panel_bg, 0.45),
                        );
                    }
                }
                CachedChatRow::ListItemLine {
                    kind,
                    text,
                    spans,
                    is_continuation,
                    marker_w,
                    is_user,
                    bubble_w,
                } => {
                    let indent_px = (kind.indent() as i32) * 14;
                    let (base_x, right_lim, container_rect) = if *is_user {
                        let brect = user_bubble_rect(row_rect, right_edge_x, *bubble_w);
                        canvas.fill_rect(brect, user_bubble_bg);
                        (brect.x + 13, brect.x + brect.w - 11, brect)
                    } else {
                        (row_rect.x + 4, row_rect.x + wrap_w, row_rect)
                    };
                    if !*is_continuation {
                        draw_list_marker(canvas, &t, base_x + indent_px, cy + 2, *kind, prose_fg);
                    }
                    let tx = base_x + indent_px + *marker_w;
                    self.draw_chat_line_with_spans_and_selection(
                        canvas,
                        container_rect,
                        tx,
                        cy + 2,
                        *global_row_idx,
                        text,
                        spans,
                        false,
                        prose_fg,
                        (right_lim - tx).max(36),
                        FontStyle::ChatBody,
                    );
                }
                CachedChatRow::BlockquoteLine {
                    depth,
                    callout,
                    is_callout_header,
                    text,
                    spans,
                    is_user,
                    bubble_w,
                } => {
                    let (bx, bw, container_rect) = if *is_user {
                        let brect = user_bubble_rect(row_rect, right_edge_x, *bubble_w);
                        canvas.fill_rect(brect, user_bubble_bg);
                        (brect.x + 11, (brect.w - 22).max(40), brect)
                    } else {
                        (row_rect.x + 2, (row_rect.w - 4).max(40), row_rect)
                    };
                    let accent = callout
                        .as_ref()
                        .map(|c| c.color(&t))
                        .unwrap_or_else(|| blend_rgb(t.accent_blue, t.border_color, 0.35));
                    let qrect = Rect::new(bx, cy, bw, row_h);
                    let qbg = blend_rgb(
                        t.panel_bg,
                        accent,
                        if callout.is_some() { 0.09 } else { 0.05 },
                    );
                    canvas.fill_rect(qrect, qbg);
                    for d in 0..(*depth).max(1) {
                        canvas.fill_rect(Rect::new(bx + 2 + (d as i32) * 8, cy, 3, row_h), accent);
                    }
                    let tx = bx + (*depth as i32) * 8 + 10;
                    if *is_callout_header {
                        if let Some(c) = callout {
                            canvas.draw_text_styled(
                                tx,
                                cy + 3,
                                c.label(self.ui_lang),
                                accent,
                                (bx + bw - tx).max(32),
                                FontStyle::ChatMetaBold,
                            );
                        }
                    } else {
                        self.draw_chat_line_with_spans_and_selection(
                            canvas,
                            container_rect,
                            tx,
                            cy + 2,
                            *global_row_idx,
                            text,
                            spans,
                            false,
                            t.text_secondary,
                            (bx + bw - tx).max(36),
                            FontStyle::ChatBody,
                        );
                    }
                }
                CachedChatRow::HorizontalRule { is_user, bubble_w } => {
                    let (lx, lw) = if *is_user {
                        let brect = user_bubble_rect(row_rect, right_edge_x, *bubble_w);
                        canvas.fill_rect(brect, user_bubble_bg);
                        (brect.x + 12, (brect.w - 24).max(32))
                    } else {
                        (row_rect.x + 4, (row_rect.w - 8).max(32))
                    };
                    canvas.fill_rect(
                        Rect::new(lx, cy + row_h / 2, lw, 1),
                        blend_rgb(t.border_color, t.panel_bg, 0.30),
                    );
                }
                CachedChatRow::TableRow {
                    prepared,
                    is_user,
                    bubble_w,
                } => {
                    let trect = if *is_user {
                        let brect = user_bubble_rect(row_rect, right_edge_x, *bubble_w);
                        canvas.fill_rect(brect, user_bubble_bg);
                        Rect::new(brect.x + 12, cy, (brect.w - 24).max(80), row_h)
                    } else {
                        Rect::new(row_rect.x + 2, cy, (row_rect.w - 4).max(80), row_h)
                    };
                    render_prepared_table_row(
                        canvas,
                        &t,
                        self.hover_pos,
                        &mut self.hit_regions,
                        trect,
                        prepared,
                        prose_fg,
                    );
                }
                CachedChatRow::CodeBlockHeader {
                    msg_idx,
                    blk_idx,
                    lang,
                    code,
                    line_count,
                    collapsed,
                    user_bubble_w,
                } => {
                    let (cx, cw) = code_block_row_bounds(
                        canvas,
                        row_rect,
                        right_edge_x,
                        *user_bubble_w,
                        user_bubble_bg,
                    );
                    let hdr = Rect::new(cx, cy + 1, cw, row_h - 1);
                    canvas.stroke_rounded_rect(
                        hdr,
                        6,
                        blend_rgb(t.border_color, t.panel_bg, 0.45),
                        blend_rgb(code_block_bg, t.panel_bg, 0.35),
                    );
                    let arrow = if *collapsed { "▸" } else { "▾" };
                    let show_fold_word = hdr.w >= 165;
                    let title_reserve = if show_fold_word { 116 } else { 72 };
                    let collapse_hit = Rect::new(hdr.x, hdr.y, (hdr.w - 64).max(20), hdr.h);
                    let col_hov = collapse_hit.contains(hx, hy);
                    canvas.draw_text_styled(
                        hdr.x + 10,
                        hdr.y + 4,
                        &format!("{arrow}  {lang} · {line_count} {}", self.tr("行", "lines")),
                        if col_hov {
                            t.text_primary
                        } else {
                            t.text_secondary
                        },
                        (hdr.w - title_reserve).max(24),
                        FontStyle::ChatCodeBold,
                    );
                    if show_fold_word {
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
                    }
                    self.hit_regions.push((
                        collapse_hit,
                        HitAction::ToggleCodeBlockCollapse(*msg_idx, *blk_idx),
                    ));

                    let copy_btn = Rect::new(hdr.x + hdr.w - 60, hdr.y + 3, 54, 18);
                    let chov = copy_btn.contains(hx, hy);
                    if chov {
                        canvas.fill_rounded_rect(
                            copy_btn,
                            4,
                            blend_rgb(t.panel_bg, t.text_primary, 0.1),
                        );
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
                    user_bubble_w,
                } => {
                    let (cx, cw) = code_block_row_bounds(
                        canvas,
                        row_rect,
                        right_edge_x,
                        *user_bubble_w,
                        user_bubble_bg,
                    );
                    let srect = Rect::new(cx, cy, cw, row_h - 1);
                    let hov = srect.contains(hx, hy);
                    canvas.stroke_rounded_rect(
                        srect,
                        5,
                        if hov {
                            t.accent_blue
                        } else {
                            blend_rgb(t.border_color, t.panel_bg, 0.45)
                        },
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
                        (srect.w - 18).max(20),
                        FontStyle::ChatCode,
                    );
                    self.hit_regions.push((
                        srect,
                        HitAction::ToggleCodeBlockCollapse(*msg_idx, *blk_idx),
                    ));
                }
                CachedChatRow::CodeBlockLine {
                    text,
                    spans,
                    user_bubble_w,
                } => {
                    let (cx, cw) = code_block_row_bounds(
                        canvas,
                        row_rect,
                        right_edge_x,
                        *user_bubble_w,
                        user_bubble_bg,
                    );
                    let crect = Rect::new(cx, cy, cw, row_h);
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
                        spans,
                        (crect.w - 20).max(20),
                    );
                }
                CachedChatRow::TurnFilesToggle {
                    msg_idx,
                    file_count,
                    added_lines,
                    removed_lines,
                    expanded,
                } => {
                    let pill = Rect::new(row_rect.x + 2, cy + 1, row_rect.w - 4, row_h - 2);
                    let hov = pill.contains(hx, hy);
                    if hov {
                        canvas.fill_rounded_rect(
                            pill,
                            4,
                            blend_rgb(t.panel_bg, t.text_primary, 0.05),
                        );
                    }
                    let glyph = if *expanded { "▾" } else { "▸" };
                    let lbl = match self.ui_lang {
                        UiLanguage::ZhCn => format!("{glyph}  本轮已更改 {file_count} 个文件"),
                        UiLanguage::EnUs => format!("{glyph}  Changed {file_count} files"),
                    };
                    let (lw, _) = canvas.measure_text(&lbl, FontStyle::ChatMeta);
                    canvas.draw_text_styled(
                        pill.x + 4,
                        pill.y + 2,
                        &lbl,
                        if hov { t.text_secondary } else { t.text_muted },
                        (pill.w - 70).max(40),
                        FontStyle::ChatMeta,
                    );
                    let mut sx = pill.x + 4 + lw + 8;
                    if *added_lines > 0 && sx + 26 < pill.x + pill.w {
                        let add_s = format!("+{added_lines}");
                        let (aw, _) = canvas.measure_text(&add_s, FontStyle::ChatMeta);
                        canvas.draw_text_styled(
                            sx,
                            pill.y + 2,
                            &add_s,
                            t.run_green,
                            aw + 2,
                            FontStyle::ChatMeta,
                        );
                        sx += aw + 5;
                    }
                    if *removed_lines > 0 && sx + 26 < pill.x + pill.w {
                        let del_s = format!("-{removed_lines}");
                        let (dw, _) = canvas.measure_text(&del_s, FontStyle::ChatMeta);
                        canvas.draw_text_styled(
                            sx,
                            pill.y + 2,
                            &del_s,
                            t.breakpoint_red,
                            dw + 2,
                            FontStyle::ChatMeta,
                        );
                    }
                    self.hit_regions
                        .push((pill, HitAction::ToggleMessageFileChangesCollapse(*msg_idx)));
                }
                CachedChatRow::TurnChangedFileRow {
                    msg_idx,
                    path,
                    display,
                    added_lines,
                    removed_lines,
                } => {
                    canvas.fill_rect(
                        Rect::new(row_rect.x + 8, cy, 1, row_h),
                        blend_rgb(t.border_color, t.panel_bg, 0.45),
                    );
                    let frow = Rect::new(row_rect.x + 14, cy + 1, row_rect.w - 18, row_h - 2);
                    let hov = frow.contains(hx, hy);
                    if hov {
                        canvas.fill_rounded_rect(
                            frow,
                            4,
                            blend_rgb(t.panel_bg, t.text_primary, 0.06),
                        );
                    }
                    canvas.draw_path_file_icon(frow.x + 4, frow.y + 2, path);
                    let stats_w = Self::draw_inline_diff_stats(
                        canvas,
                        frow.x + frow.w - 6,
                        frow.y + 2,
                        frow.x + 28,
                        *added_lines,
                        *removed_lines,
                        t,
                    );
                    let name_max_w = (frow.w - stats_w - 30).max(32);
                    let shown_display =
                        canvas.ellipsize_filename(display, name_max_w, FontStyle::ChatMeta);
                    canvas.draw_text_styled(
                        frow.x + 22,
                        frow.y + 2,
                        &shown_display,
                        if hov { t.text_primary } else { t.syn_function },
                        name_max_w,
                        FontStyle::ChatMeta,
                    );
                    self.hit_regions.push((
                        frow,
                        HitAction::InspectAgentFileDiff {
                            path: path.clone(),
                            msg_idx: Some(*msg_idx),
                        },
                    ));
                }
                CachedChatRow::LiveThoughtLine(txt) => {
                    canvas.fill_rect(
                        Rect::new(row_rect.x + 8, cy, 1, row_h),
                        blend_rgb(t.border_color, t.panel_bg, 0.40),
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
                CachedChatRow::LiveStepLine(txt) => {
                    canvas.draw_text_styled(
                        row_rect.x + 4,
                        cy + 1,
                        "›",
                        t.text_muted,
                        10,
                        FontStyle::ChatMeta,
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
                        blend_rgb(t.text_muted, t.text_secondary, 0.35),
                        wrap_w - 16,
                        FontStyle::ChatMeta,
                    );
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
                        .agent_hub
                        .active_session_busy_started_at()
                        .map(|st| st.elapsed().as_secs())
                        .unwrap_or(0);
                    let elapsed_str = if elapsed_secs >= 60 {
                        format!("{}m {}s", elapsed_secs / 60, elapsed_secs % 60)
                    } else {
                        format!("{elapsed_secs}s")
                    };
                    let busy_lbl =
                        format!("{}  {elapsed_str}", self.tr("处理中...", "Processing..."));
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
        let scroll_px = self
            .cached_chat_rows
            .row_tops
            .get(self.agent_scroll)
            .copied()
            .unwrap_or(0)
            .max(0) as usize;
        let max_scroll_px = self
            .cached_chat_rows
            .row_tops
            .get(max_s)
            .copied()
            .unwrap_or(0)
            .max(0) as usize;
        let vis_px = (chat_area.h - 4).max(1) as usize;
        let eff_total_px = if max_s > 0 {
            max_scroll_px.saturating_add(vis_px)
        } else {
            self.cached_chat_rows.total_height.max(0) as usize
        };
        canvas.draw_vertical_scrollbar(
            track,
            scroll_px,
            vis_px,
            eff_total_px,
            blend_rgb(t.border_color, t.panel_bg, 0.35),
        );
    }

    /// 在文件行右侧绘制单文件 `+N` / `-M` 变更统计，并返回占用的总宽度（自动隐藏无意义的 0 值）
    fn draw_inline_diff_stats(
        canvas: &mut SurfaceCanvas,
        right_edge_x: i32,
        y: i32,
        min_left_x: i32,
        added_lines: usize,
        removed_lines: usize,
        t: ide_core::IdeaTheme,
    ) -> i32 {
        let add_s = (added_lines > 0 || removed_lines == 0).then(|| format!("+{added_lines}"));
        let del_s = (removed_lines > 0).then(|| format!("-{removed_lines}"));
        let dw = del_s
            .as_ref()
            .map(|s| canvas.measure_text(s, FontStyle::ChatMeta).0)
            .unwrap_or(0);
        let aw = add_s
            .as_ref()
            .map(|s| canvas.measure_text(s, FontStyle::ChatMeta).0)
            .unwrap_or(0);
        let gap = if add_s.is_some() && del_s.is_some() { 5 } else { 0 };
        let total_w = aw + gap + dw;
        let start_x = right_edge_x - total_w;
        if start_x <= min_left_x {
            return total_w;
        }
        let mut cx = start_x;
        if let Some(ref s) = add_s {
            let color = if added_lines > 0 { t.run_green } else { t.text_muted };
            canvas.draw_text_styled(cx, y, s, color, aw + 2, FontStyle::ChatMeta);
            cx += aw + gap;
        }
        if let Some(ref s) = del_s {
            canvas.draw_text_styled(cx, y, s, t.breakpoint_red, dw + 2, FontStyle::ChatMeta);
        }
        total_w
    }

    fn render_agent_changed_files_bar(
        &mut self,
        canvas: &mut SurfaceCanvas,
        rect: Rect,
        changed_files: &[(PathBuf, usize, usize)],
        supports_rewind: bool,
    ) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let bar_bg = if t.kind == ThemeKind::IslandsLight {
            blend_rgb(t.panel_bg, t.editor_bg, 0.45)
        } else {
            blend_rgb(t.editor_bg, t.panel_bg, 0.30)
        };
        let border = blend_rgb(t.border_color, t.panel_bg, 0.45);
        canvas.stroke_rounded_rect(rect, 7, border, bar_bg);

        let total_add: usize = changed_files.iter().map(|f| f.1).sum();
        let total_del: usize = changed_files.iter().map(|f| f.2).sum();
        let arrow = if self.agent_changed_files_expanded {
            "▾"
        } else {
            "▸"
        };
        let title = match self.ui_lang {
            UiLanguage::ZhCn => format!("{arrow}  已更改 {} 个文件", changed_files.len()),
            UiLanguage::EnUs => format!("{arrow}  {} files changed", changed_files.len()),
        };

        let hdr_rect = Rect::new(rect.x, rect.y, rect.w, 26);
        let revert_btn = Rect::new(rect.x + rect.w - 50, rect.y + 4, 44, 18);
        let keep_btn = if supports_rewind {
            Rect::new(revert_btn.x - 46, rect.y + 4, 42, 18)
        } else {
            Rect::new(rect.x + rect.w - 50, rect.y + 4, 44, 18)
        };
        let toggle_rect = Rect::new(
            hdr_rect.x + 2,
            hdr_rect.y + 1,
            (keep_btn.x - hdr_rect.x - 6).max(40),
            24,
        );
        if toggle_rect.contains(hx, hy) {
            canvas.fill_rounded_rect(toggle_rect, 5, blend_rgb(bar_bg, t.text_primary, 0.05));
        }
        let (tw, _) = canvas.measure_text(&title, FontStyle::ChatMetaBold);
        canvas.draw_text_styled(
            toggle_rect.x + 8,
            toggle_rect.y + 4,
            &title,
            t.text_primary,
            (toggle_rect.w - 12).max(24),
            FontStyle::ChatMetaBold,
        );
        let mut sx = toggle_rect.x + 8 + tw + 8;
        if total_add > 0 && sx + 28 < keep_btn.x {
            let add_s = format!("+{total_add}");
            let (aw, _) = canvas.measure_text(&add_s, FontStyle::ChatMeta);
            canvas.draw_text_styled(
                sx,
                toggle_rect.y + 4,
                &add_s,
                t.run_green,
                aw + 4,
                FontStyle::ChatMeta,
            );
            sx += aw + 6;
        }
        if total_del > 0 && sx + 28 < keep_btn.x {
            let del_s = format!("-{total_del}");
            let (dw, _) = canvas.measure_text(&del_s, FontStyle::ChatMeta);
            canvas.draw_text_styled(
                sx,
                toggle_rect.y + 4,
                &del_s,
                t.breakpoint_red,
                dw + 4,
                FontStyle::ChatMeta,
            );
        }
        self.hit_regions
            .push((toggle_rect, HitAction::ToggleAgentChangedFilesDrawer));

        let keep_hov = keep_btn.contains(hx, hy);
        canvas.fill_rounded_rect(
            keep_btn,
            4,
            if keep_hov {
                blend_rgb(bar_bg, t.text_primary, 0.09)
            } else {
                blend_rgb(bar_bg, t.text_primary, 0.04)
            },
        );
        canvas.draw_text_centered(
            keep_btn,
            if supports_rewind {
                self.tr("保留", "Keep")
            } else {
                self.tr("清除", "Clear")
            },
            if keep_hov {
                t.text_primary
            } else {
                t.text_secondary
            },
            FontStyle::ChatMeta,
        );
        self.hit_regions
            .push((keep_btn, HitAction::KeepAllAgentChangedFiles));

        if supports_rewind {
            let rev_hov = revert_btn.contains(hx, hy);
            canvas.fill_rounded_rect(
                revert_btn,
                4,
                if rev_hov {
                    blend_rgb(bar_bg, t.breakpoint_red, 0.18)
                } else {
                    blend_rgb(bar_bg, t.text_primary, 0.04)
                },
            );
            canvas.draw_text_centered(
                revert_btn,
                self.tr("回退", "Revert"),
                if rev_hov {
                    t.breakpoint_red
                } else {
                    t.text_muted
                },
                FontStyle::ChatMeta,
            );
            self.hit_regions
                .push((revert_btn, HitAction::RevertAllAgentChangedFiles));
        }

        if !self.agent_changed_files_expanded {
            return;
        }

        canvas.fill_rect(
            Rect::new(rect.x + 8, rect.y + 27, rect.w - 16, 1),
            blend_rgb(border, bar_bg, 0.55),
        );
        let mut ry = rect.y + 29;
        for (ch_path, added_lines, removed_lines) in changed_files.iter().take(5) {
            if ry + 20 > rect.y + rect.h - 2 {
                break;
            }
            let row_r = Rect::new(rect.x + 6, ry, rect.w - 12, 20);
            let rev_one = Rect::new(row_r.x + row_r.w - 22, ry + 1, 20, 18);
            let open_w = if supports_rewind {
                (rev_one.x - row_r.x - 4).max(40)
            } else {
                row_r.w.max(40)
            };
            let open_r = Rect::new(row_r.x, ry, open_w, 20);
            if open_r.contains(hx, hy) {
                canvas.fill_rounded_rect(open_r, 4, blend_rgb(bar_bg, t.text_primary, 0.06));
            }
            canvas.draw_path_file_icon(open_r.x + 4, ry + 3, ch_path);
            let rel = ch_path
                .strip_prefix(&self.workspace_root)
                .unwrap_or(ch_path)
                .to_string_lossy()
                .replace('\\', "/");
            let stats_w = Self::draw_inline_diff_stats(
                canvas,
                open_r.x + open_r.w - 6,
                ry + 2,
                open_r.x + 30,
                *added_lines,
                *removed_lines,
                t,
            );
            let name_max_w = (open_r.w - stats_w - 30).max(32);
            let shown_rel = canvas.ellipsize_filename(&rel, name_max_w, FontStyle::ChatMeta);
            canvas.draw_text_styled(
                open_r.x + 22,
                ry + 2,
                &shown_rel,
                t.text_secondary,
                name_max_w,
                FontStyle::ChatMeta,
            );
            self.hit_regions.push((
                open_r,
                HitAction::InspectAgentFileDiff {
                    path: ch_path.clone(),
                    msg_idx: None,
                },
            ));

            if supports_rewind {
                let rhov = rev_one.contains(hx, hy);
                if rhov {
                    canvas.fill_rounded_rect(rev_one, 4, blend_rgb(bar_bg, t.breakpoint_red, 0.16));
                }
                canvas.draw_text_centered(
                    rev_one,
                    "↶",
                    if rhov { t.breakpoint_red } else { t.text_muted },
                    FontStyle::ChatMeta,
                );
                self.hit_regions.push((
                    rev_one,
                    HitAction::RevertSingleAgentChangedFile(ch_path.clone()),
                ));
            }
            ry += 22;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_agent_composer(
        &mut self,
        canvas: &mut SurfaceCanvas,
        composer_rect: Rect,
        attached_items: &[AgentAttachedItem],
        pasted_chips: &[(String, String, usize)],
        expanded_snip: Option<&ide_agent_hub::PastedCodeSnippet>,
        tray_h: i32,
        drawer_h: i32,
        edit_banner_h: i32,
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
        let composer_border = if self.editing_sent_msg_idx.is_some() || is_focused {
            blend_rgb(t.border_color, t.accent_blue, 0.65)
        } else {
            blend_rgb(t.border_color, t.panel_bg, 0.35)
        };
        let hover_pill_bg = blend_rgb(composer_bg, t.text_primary, 0.08);
        canvas.stroke_rounded_rect(composer_rect, 10, composer_border, composer_bg);

        if let Some(edit_idx) = self.editing_sent_msg_idx {
            let banner = Rect::new(
                composer_rect.x + 6,
                composer_rect.y + 4,
                composer_rect.w - 12,
                20,
            );
            canvas.fill_rounded_rect(banner, 5, blend_rgb(composer_bg, t.accent_blue, 0.16));
            let banner_txt = match self.ui_lang {
                UiLanguage::ZhCn => format!(
                    "✎ 正在编辑已发送消息 #{} · Enter 确认修改，Esc 取消",
                    edit_idx + 1
                ),
                UiLanguage::EnUs => format!(
                    "✎ Editing sent message #{} · Enter to review rewind, Esc to cancel",
                    edit_idx + 1
                ),
            };
            canvas.draw_text_styled(
                banner.x + 8,
                banner.y + 2,
                &banner_txt,
                t.accent_blue,
                (banner.w - 54).max(40),
                FontStyle::ChatMeta,
            );
            let cancel_r = Rect::new(banner.x + banner.w - 42, banner.y + 1, 38, 18);
            let chov = cancel_r.contains(hx, hy);
            if chov {
                canvas.fill_rounded_rect(cancel_r, 4, hover_pill_bg);
            }
            canvas.draw_text_centered(
                cancel_r,
                self.tr("取消", "Cancel"),
                if chov {
                    t.text_primary
                } else {
                    t.text_secondary
                },
                FontStyle::ChatMeta,
            );
            self.hit_regions
                .push((cancel_r, HitAction::CancelEditSentUserMessage));
        }
        let inner_top = composer_rect.y + edit_banner_h;

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
        let ctx_pill = Rect::new(composer_rect.x + 10, inner_top + 6, cw + 12, 18);
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
            let eff_pill = Rect::new(opt_rx - ew_pill, inner_top + 6, ew_pill, 18);
            if eff_pill.x > ctx_pill.x + ctx_pill.w + 6 {
                let ehov = eff_pill.contains(hx, hy);
                if ehov {
                    canvas.fill_rounded_rect(eff_pill, 4, hover_pill_bg);
                }
                canvas.draw_text_centered(
                    eff_pill,
                    eff_txt,
                    if ehov {
                        t.text_primary
                    } else {
                        t.text_secondary
                    },
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
            let fast_pill = Rect::new(opt_rx - fw_pill, inner_top + 6, fw_pill, 18);
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
        let mut cursor_y = inner_top + 30;
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
                self.hit_regions.push((
                    rm_btn,
                    HitAction::RemoveComposerAttachment(item.path.clone()),
                ));

                tx += card_w + 6;
            }

            let chip_y = if tray_h > 30 {
                cursor_y + 14
            } else {
                cursor_y + 2
            };
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
                canvas.draw_path_file_icon(chip.x + 5, chip.y + 4, &item.path);
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
                    if rm.contains(hx, hy) {
                        t.breakpoint_red
                    } else {
                        t.text_muted
                    },
                    FontStyle::ChatMeta,
                );
                self.hit_regions
                    .push((rm, HitAction::RemoveComposerAttachment(item.path.clone())));
                tx += cw_chip + 5;
            }

            for (snip_id, snip_lang, snip_line_count) in pasted_chips {
                let is_exp = self.expanded_composer_snippet_id.as_deref() == Some(snip_id.as_str());
                let arrow = if is_exp { "▾" } else { "▸" };
                let lbl = format!("{arrow} </> {snip_lang} ({snip_line_count}行)");
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
                self.hit_regions.push((
                    chip,
                    HitAction::ToggleComposerPastedSnippet(snip_id.clone()),
                ));
                let rm = Rect::new(chip.x + chip.w - 16, chip.y + 3, 14, 16);
                canvas.draw_text_centered(
                    rm,
                    "×",
                    if rm.contains(hx, hy) {
                        t.breakpoint_red
                    } else {
                        t.text_muted
                    },
                    FontStyle::ChatMeta,
                );
                self.hit_regions
                    .push((rm, HitAction::RemoveComposerPastedSnippet(snip_id.clone())));
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
                &format!(
                    "</> {} · {} 行（发送时默认折叠展示）",
                    snip.title, snip.line_count
                ),
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
            let lang_id = LanguageId::from_fence_tag(&snip.language);
            let mut hl_state = HighlightState::default();
            for line in snip.content.lines().take(5) {
                let mut cur_x = drect.x + 8;
                let max_x = drect.x + drect.w - 8;
                for sp in highlight_line_with_state(line, lang_id, &mut hl_state) {
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
            if agent_hov {
                t.text_primary
            } else {
                t.text_secondary
            },
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
                if model_hov {
                    t.text_primary
                } else {
                    t.text_secondary
                },
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
            if mode_hov {
                t.text_primary
            } else {
                mode_accent
            },
        );
        canvas.draw_text_styled(
            mode_pill.x + 20,
            mode_pill.y + 3,
            mode_label,
            if mode_hov {
                t.text_primary
            } else {
                mode_accent
            },
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
                || !pasted_chips.is_empty();
            canvas.fill_rounded_rect(
                send_btn,
                6,
                if has_text {
                    if send_hov {
                        blend_rgb(t.accent_blue, 0xFFFFFF, 0.15)
                    } else {
                        t.accent_blue
                    }
                } else {
                    blend_rgb(
                        composer_bg,
                        t.text_primary,
                        if send_hov { 0.16 } else { 0.10 },
                    )
                },
            );
            canvas.draw_send_up_icon(
                send_btn.x + 5,
                send_btn.y + 5,
                if has_text || send_hov {
                    0xFFFFFF
                } else {
                    t.text_secondary
                },
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
            if attach_hov {
                t.text_primary
            } else {
                t.text_muted
            },
        );
        self.hit_regions
            .push((attach_btn, HitAction::AttachActiveFileToAgent));
    }

    fn render_agent_wait_queue(
        &mut self,
        canvas: &mut SurfaceCanvas,
        queue_rect: Rect,
        queued: &[QueuedAgentPrompt],
    ) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        canvas.fill_rounded_rect(queue_rect, 8, blend_rgb(t.editor_bg, t.panel_bg, 0.35));
        canvas.stroke_rounded_rect(queue_rect, 8, t.border_color, t.editor_bg);
        let extra = queued.len().saturating_sub(4);
        let title = if extra > 0 {
            format!(
                "{} · {}  (+{})",
                self.tr("等待区", "Queue"),
                queued.len(),
                extra
            )
        } else {
            format!("{} · {}", self.tr("等待区", "Queue"), queued.len())
        };
        canvas.draw_text_styled(
            queue_rect.x + 10,
            queue_rect.y + 4,
            &title,
            t.text_muted,
            queue_rect.w - 20,
            FontStyle::UiSmall,
        );
        #[derive(Clone, Copy)]
        enum QueueGlyph {
            Undo,
            Edit,
            Close,
            Send,
        }
        let mut hover_tip: Option<(Rect, String)> = None;
        let mut qy = queue_rect.y + 22;
        for item in queued.iter().take(4) {
            let row = Rect::new(queue_rect.x + 6, qy, queue_rect.w - 12, 44);
            if row.contains(hx, hy) {
                canvas.fill_rounded_rect(row, 5, blend_rgb(t.editor_bg, t.text_primary, 0.05));
            }
            let preview = item.text.replace('\n', " ");
            let mode = item.mode.label();
            canvas.draw_text_styled(
                row.x + 6,
                row.y + 2,
                &format!("{mode}  {preview}"),
                t.text_primary,
                (row.w - 12).max(20),
                FontStyle::ChatMeta,
            );
            let actions = [
                (
                    QueueGlyph::Undo,
                    self.tr("撤销", "Undo").to_string(),
                    HitAction::UndoQueuedAgentPrompt(item.id.clone()),
                ),
                (
                    QueueGlyph::Edit,
                    self.tr("退回编辑", "Return to edit").to_string(),
                    HitAction::EditQueuedAgentPrompt(item.id.clone()),
                ),
                (
                    QueueGlyph::Close,
                    self.tr("关闭", "Close").to_string(),
                    HitAction::CloseQueuedAgentPrompt(item.id.clone()),
                ),
                (
                    QueueGlyph::Send,
                    self.tr("立即发送", "Send now").to_string(),
                    HitAction::SendQueuedAgentPromptNow(item.id.clone()),
                ),
            ];
            let btn_size = 18;
            let gap = 3;
            let total = actions.len() as i32 * btn_size + (actions.len() as i32 - 1) * gap;
            let mut bx = row.x + row.w - 2 - total;
            if bx < row.x + 2 {
                bx = row.x + 2;
            }
            let by = row.y + 22;
            for (glyph, label, action) in actions {
                let btn = Rect::new(bx, by, btn_size, btn_size);
                if btn.x + btn.w > row.x + row.w - 2 {
                    break;
                }
                let hov = btn.contains(hx, hy);
                let icon_color = match glyph {
                    QueueGlyph::Send => {
                        if hov {
                            0xFFFFFF
                        } else {
                            t.accent_blue
                        }
                    }
                    QueueGlyph::Close => {
                        if hov {
                            t.breakpoint_red
                        } else {
                            t.text_muted
                        }
                    }
                    QueueGlyph::Undo | QueueGlyph::Edit => {
                        if hov {
                            t.text_primary
                        } else {
                            t.text_muted
                        }
                    }
                };
                if hov || matches!(glyph, QueueGlyph::Send) {
                    let fill = match glyph {
                        QueueGlyph::Send => {
                            if hov {
                                t.accent_blue
                            } else {
                                blend_rgb(t.editor_bg, t.accent_blue, 0.22)
                            }
                        }
                        QueueGlyph::Close => blend_rgb(t.editor_bg, t.breakpoint_red, 0.14),
                        QueueGlyph::Undo | QueueGlyph::Edit => {
                            blend_rgb(t.editor_bg, t.text_primary, 0.10)
                        }
                    };
                    canvas.fill_rounded_rect(btn, 4, fill);
                }
                match glyph {
                    QueueGlyph::Undo => {
                        canvas.draw_rollback_icon(btn.x + 2, btn.y + 2, icon_color);
                    }
                    QueueGlyph::Edit => {
                        canvas.draw_pencil_icon(btn.x + 2, btn.y + 2, icon_color);
                    }
                    QueueGlyph::Close => {
                        canvas.draw_window_close_icon(btn.x + 9, btn.y + 9, icon_color);
                    }
                    QueueGlyph::Send => {
                        canvas.draw_send_up_icon(btn.x + 3, btn.y + 3, icon_color);
                    }
                }
                if hov {
                    hover_tip = Some((btn, label));
                }
                self.hit_regions.push((btn, action));
                bx += btn_size + gap;
            }
            qy += 46;
        }
        if let Some((anchor, label)) = hover_tip {
            let (tw, th) = canvas.measure_text(&label, FontStyle::UiSmall);
            let bw = tw + 16;
            let bh = (th + 8).max(20);
            let mut tx = anchor.x + anchor.w / 2 - bw / 2;
            let mut ty = anchor.y - bh - 6;
            if ty < 4 {
                ty = anchor.y + anchor.h + 4;
            }
            let min_x = queue_rect.x + 4;
            let max_x = queue_rect.x + queue_rect.w - bw - 4;
            if max_x >= min_x {
                tx = tx.clamp(min_x, max_x);
            }
            let tip = Rect::new(tx, ty, bw, bh);
            canvas.fill_rounded_shadow(tip, 4);
            canvas.stroke_rounded_rect(tip, 4, t.border_color, t.popup_bg);
            canvas.draw_text_centered(tip, &label, t.text_primary, FontStyle::UiSmall);
        }
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
        let visible_start = sessions.len().saturating_sub(max_show);
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
        for (idx, title, msg_cnt, busy) in sessions.into_iter().skip(visible_start) {
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
                if is_cur {
                    FontStyle::UiBold
                } else {
                    FontStyle::UiRegular
                },
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

    pub(crate) fn render_image_preview_modal(
        &mut self,
        canvas: &mut SurfaceCanvas,
        w: i32,
        h: i32,
    ) {
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
        canvas.draw_text_ellipsis(
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
        canvas.draw_text_ellipsis(
            mx + 14,
            my + mh - 26,
            &path_txt,
            t.text_muted,
            mw - 28,
            FontStyle::CodeMono,
        );
    }

    pub(crate) fn render_drag_ghost_overlay(&self, canvas: &mut SurfaceCanvas) {
        let Some((path, (cx, cy), is_cross)) = self.active_drag_hover() else {
            return;
        };
        let t = self.theme;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let drop_target = self.resolve_drop_target(path, cx, cy, is_cross);

        // Draw target highlight for non-AI zones (AI panel renders its own full drop overlay)
        match &drop_target {
            Some(crate::ide_state::IdeDropTarget::ProjectTreeCopy { highlight_rect, .. }) => {
                canvas.fill_rounded_rect_alpha(
                    *highlight_rect,
                    5,
                    blend_rgb(t.panel_bg, t.run_green, 0.28),
                    155,
                );
                canvas.draw_dashed_rounded_border(*highlight_rect, 5, t.run_green);
            }
            Some(crate::ide_state::IdeDropTarget::TextInput { rect, .. })
            | Some(crate::ide_state::IdeDropTarget::Terminal(rect)) => {
                canvas.fill_rounded_rect_alpha(
                    *rect,
                    5,
                    blend_rgb(t.editor_bg, t.accent_blue, 0.22),
                    135,
                );
                canvas.draw_dashed_rounded_border(*rect, 5, t.accent_blue);
            }
            _ => {}
        }

        let (label, border_col, bg_col) = match &drop_target {
            Some(crate::ide_state::IdeDropTarget::ProjectTreeCopy { target_dir, .. }) => {
                let dir_name = target_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("root");
                let text = match self.ui_lang {
                    UiLanguage::ZhCn => format!("📋 复制 `{name}` → `{dir_name}/`"),
                    UiLanguage::EnUs => format!("📋 Copy `{name}` → `{dir_name}/`"),
                };
                (text, t.run_green, t.popup_hover_bg)
            }
            Some(crate::ide_state::IdeDropTarget::AgentAssistant) => {
                let text = if crate::image_preview::is_image_file_path(path) {
                    match self.ui_lang {
                        UiLanguage::ZhCn => format!("🖼 附加图片 `{name}`"),
                        UiLanguage::EnUs => format!("🖼 Attach image `{name}`"),
                    }
                } else {
                    match self.ui_lang {
                        UiLanguage::ZhCn => format!("📎 附加到 AI: `{name}`"),
                        UiLanguage::EnUs => format!("📎 Attach to AI: `{name}`"),
                    }
                };
                (text, t.accent_blue, t.popup_hover_bg)
            }
            Some(crate::ide_state::IdeDropTarget::TextInput { .. }) => {
                let text = match self.ui_lang {
                    UiLanguage::ZhCn => format!("⤵ 插入路径到输入框: `{name}`"),
                    UiLanguage::EnUs => format!("⤵ Insert path: `{name}`"),
                };
                (text, t.accent_blue, t.popup_hover_bg)
            }
            Some(crate::ide_state::IdeDropTarget::Terminal(_)) => {
                let text = match self.ui_lang {
                    UiLanguage::ZhCn => format!("⤵ 插入路径到终端: `{name}`"),
                    UiLanguage::EnUs => format!("⤵ Paste path in terminal: `{name}`"),
                };
                (text, t.accent_blue, t.popup_hover_bg)
            }
            Some(crate::ide_state::IdeDropTarget::EditorOpen) => {
                let text = match self.ui_lang {
                    UiLanguage::ZhCn => format!("📄 打开 `{name}`"),
                    UiLanguage::EnUs => format!("📄 Open `{name}`"),
                };
                (text, t.accent_blue, t.popup_bg)
            }
            None => (format!("📄 {name}"), t.border_color, t.popup_bg),
        };

        let (tw, _) = canvas.measure_text(&label, FontStyle::UiSmall);
        let pill_w = (tw + 22).clamp(80, 320);
        let max_x = (canvas.width as i32 - pill_w - 6).max(6);
        let max_y = (canvas.height as i32 - 30).max(6);
        let pill = Rect::new((cx + 12).clamp(6, max_x), (cy + 12).clamp(6, max_y), pill_w, 24);
        canvas.fill_rounded_shadow(pill, 6);
        canvas.stroke_rounded_rect(pill, 6, border_col, bg_col);
        canvas.draw_text_centered(pill, &label, 0xFFFFFF, FontStyle::UiSmall);
    }

    pub(crate) fn render_agent_provider_popup(&mut self, canvas: &mut SurfaceCanvas) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let providers = self.agent_hub.providers.clone();
        let pw = 372.min((self.right_pane_rect.w - 16).max(260));
        let ph = 42 + (providers.len() as i32) * 28;
        let px = (self.right_pane_rect.x + 12).min(
            (self.right_pane_rect.x + self.right_pane_rect.w - pw - 10)
                .max(self.right_pane_rect.x + 10),
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
            if hub_hov {
                t.accent_blue
            } else {
                t.border_color
            },
            t.window_bg,
        );
        canvas.draw_text_centered(
            hub_pill,
            self.tr("总览", "Hub"),
            if hub_hov {
                t.accent_blue
            } else {
                t.text_secondary
            },
            FontStyle::ChatMeta,
        );
        self.hit_regions
            .push((hub_pill, HitAction::OpenAiConfigModal));

        let sync_pill = Rect::new(px + pw - 68, py + 6, 58, 20);
        let sync_hov = sync_pill.contains(hx, hy);
        canvas.stroke_rounded_rect(
            sync_pill,
            4,
            if sync_hov {
                t.accent_blue
            } else {
                t.border_color
            },
            t.window_bg,
        );
        canvas.draw_text_centered(
            sync_pill,
            self.tr("↻ 刷新", "↻ Sync"),
            if sync_hov {
                t.accent_blue
            } else {
                t.text_secondary
            },
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
                if sel {
                    FontStyle::ChatBodyBold
                } else {
                    FontStyle::ChatBody
                },
            );
            if reg.available {
                let badge_txt = reg
                    .active_model
                    .rsplit('/')
                    .next()
                    .unwrap_or(&reg.active_model);
                canvas.draw_text_styled(
                    row.x + row.w - 122,
                    row.y + 5,
                    badge_txt,
                    t.syn_function,
                    116,
                    FontStyle::ChatMeta,
                );
                self.hit_regions
                    .push((row, HitAction::SelectAgentProvider(reg.kind)));
            } else {
                self.hit_regions
                    .push((row, HitAction::SelectAgentProvider(reg.kind)));
                let inst_btn = Rect::new(row.x + row.w - 104, row.y + 3, 98, 20);
                let ihov = inst_btn.contains(hx, hy);
                canvas.stroke_rounded_rect(
                    inst_btn,
                    4,
                    if ihov { t.accent_blue } else { t.warning_yellow },
                    if ihov {
                        t.accent_blue
                    } else {
                        blend_rgb(t.popup_bg, t.warning_yellow, 0.14)
                    },
                );
                let lbl = if self.cli_install_in_progress == Some(reg.kind) {
                    self.tr("安装中…", "Installing…")
                } else if reg.kind == AgentProviderKind::GoogleAntigravityAcp {
                    self.tr("官网下载", "Get CLI")
                } else {
                    self.tr("⬇ 安装 CLI", "⬇ Install")
                };
                canvas.draw_text_centered(
                    inst_btn,
                    lbl,
                    if ihov { 0xFFFFFF } else { t.warning_yellow },
                    FontStyle::ChatMeta,
                );
                self.hit_regions
                    .push((inst_btn, HitAction::InstallAgentCli(reg.kind)));
            }
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
            &format!(
                "{}: {resolved_model}",
                self.tr("实际调用 ID", "Resolved ID")
            ),
            t.syn_function,
            pw - 24,
            FontStyle::ChatCode,
        );
        let sync_pill = Rect::new(px + pw - 68, py + 6, 58, 20);
        let sync_hov = sync_pill.contains(hx, hy);
        canvas.stroke_rounded_rect(
            sync_pill,
            4,
            if sync_hov {
                t.accent_blue
            } else {
                t.border_color
            },
            t.window_bg,
        );
        canvas.draw_text_centered(
            sync_pill,
            self.tr("↻ 同步", "↻ Sync"),
            if sync_hov {
                t.accent_blue
            } else {
                t.text_secondary
            },
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
                if sel {
                    FontStyle::ChatCodeBold
                } else {
                    FontStyle::ChatCode
                },
            );
            self.hit_regions.push((row, HitAction::SelectAgentModel(m)));
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
                if sel {
                    t.text_primary
                } else {
                    t.text_secondary
                },
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
        let mw = 640.min(w - 40);
        let mh = 468.min(h - 60);
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
                "本地 CLI 智能体与模型中心  (自动获取 · 支持一键安装)",
                "Local CLI Agents & Models Hub  (Auto Discovery & Install)",
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

        // Node.js 运行环境检测提示条
        let node_bar = Rect::new(mx + 16, my + 40, mw - 32, 26);
        let node_ready = self.node_env.is_ready();
        canvas.stroke_rounded_rect(
            node_bar,
            5,
            if node_ready {
                blend_rgb(t.border_color, t.run_green, 0.45)
            } else {
                t.warning_yellow
            },
            if node_ready {
                t.editor_bg
            } else {
                blend_rgb(t.popup_bg, t.warning_yellow, 0.12)
            },
        );
        let node_status_msg = if node_ready {
            let ver = self.node_env.node_version.as_deref().unwrap_or("ready");
            match self.ui_lang {
                UiLanguage::ZhCn => {
                    format!("✓ Node.js 运行环境就绪 ({ver} · npm 可用，支持直接安装 Claude / Codex CLI)")
                }
                UiLanguage::EnUs => {
                    format!("✓ Node.js ready ({ver} · npm available for CLI installation)")
                }
            }
        } else {
            self.tr(
                "⚠ 未检测到 Node.js (安装 Claude / Codex 等 CLI 需要 Node.js)",
                "⚠ Node.js not found (Required to install Claude / Codex CLI)",
            )
            .to_string()
        };
        canvas.draw_text_ellipsis(
            node_bar.x + 10,
            node_bar.y + 5,
            &node_status_msg,
            if node_ready {
                t.run_green
            } else {
                t.warning_yellow
            },
            node_bar.w - if node_ready { 20 } else { 154 },
            FontStyle::UiSmall,
        );
        if !node_ready {
            let dl_node_btn = Rect::new(node_bar.x + node_bar.w - 138, node_bar.y + 3, 132, 20);
            let nhov = dl_node_btn.contains(hx, hy);
            canvas.fill_rounded_rect(
                dl_node_btn,
                4,
                if nhov {
                    blend_rgb(t.accent_blue, 0xFFFFFF, 0.12)
                } else {
                    t.accent_blue
                },
            );
            canvas.draw_text_centered(
                dl_node_btn,
                self.tr("前往官网下载 Node.js", "Download Node.js"),
                0xFFFFFF,
                FontStyle::ChatMeta,
            );
            self.hit_regions
                .push((dl_node_btn, HitAction::OpenNodeJsDownloadPage));
        }

        canvas.draw_text_styled(
            mx + 16,
            my + 74,
            self.tr(
                "1. 本机 CLI 引擎状态 (点击切换活动 CLI):",
                "1. Detected Local CLI Engines (Click to switch):",
            ),
            t.syn_function,
            mw - 130,
            FontStyle::UiBold,
        );
        let sync_btn = Rect::new(mx + mw - 110, my + 71, 94, 24);
        let sync_hov = sync_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            sync_btn,
            5,
            if sync_hov {
                t.accent_blue
            } else {
                t.border_color
            },
            t.window_bg,
        );
        canvas.draw_text_centered(
            sync_btn,
            self.tr("↻ 重新扫描", "↻ Rescan"),
            if sync_hov {
                t.accent_blue
            } else {
                t.text_primary
            },
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((sync_btn, HitAction::RefreshLocalCliAgents));

        let providers = self.agent_hub.providers.clone();
        let mut ry = my + 100;
        for reg in providers {
            let row = Rect::new(mx + 16, ry, mw - 32, 30);
            let sel = reg.kind == self.agent_hub.active_provider;
            let hov = row.contains(hx, hy);
            canvas.stroke_rounded_rect(
                row,
                6,
                if sel || hov {
                    t.accent_blue
                } else {
                    t.border_color
                },
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
                174,
                if sel {
                    FontStyle::UiBold
                } else {
                    FontStyle::UiRegular
                },
            );
            let status_w = if reg.available {
                row.w - 220
            } else {
                (row.w - 332).max(80)
            };
            canvas.draw_text_ellipsis(
                row.x + 204,
                row.y + 7,
                &reg.status_text,
                if sel { 0xE2E8F0 } else { t.text_secondary },
                status_w,
                FontStyle::CodeMono,
            );
            self.hit_regions
                .push((row, HitAction::SelectAgentProvider(reg.kind)));

            if !reg.available {
                let inst_btn = Rect::new(row.x + row.w - 110, row.y + 4, 102, 22);
                let ihov = inst_btn.contains(hx, hy);
                canvas.fill_rounded_rect(
                    inst_btn,
                    5,
                    if self.cli_install_in_progress == Some(reg.kind) {
                        blend_rgb(t.accent_blue, t.panel_bg, 0.35)
                    } else if ihov {
                        blend_rgb(t.accent_blue, 0xFFFFFF, 0.12)
                    } else {
                        t.accent_blue
                    },
                );
                let inst_lbl = if self.cli_install_in_progress == Some(reg.kind) {
                    self.tr("⏳ 安装中...", "⏳ Installing...")
                } else if reg.kind == AgentProviderKind::GoogleAntigravityAcp {
                    self.tr("官网下载", "Get CLI")
                } else {
                    self.tr("⬇ 一键安装", "⬇ Install")
                };
                canvas.draw_text_centered(inst_btn, inst_lbl, 0xFFFFFF, FontStyle::UiSmall);
                self.hit_regions
                    .push((inst_btn, HitAction::InstallAgentCli(reg.kind)));
            }
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

#[cfg(test)]
mod session_tab_scroll_tests {
    use super::{session_tab_scroll_including, session_tab_scroll_reveal, session_tabs_fitting};

    #[test]
    fn clicking_a_visible_tab_does_not_move_the_strip() {
        let widths = vec![80, 80, 80, 80, 80, 80];
        let avail = 80 + 4 + 80 + 4 + 80;
        assert_eq!(session_tabs_fitting(1, &widths, avail), 3);
        assert_eq!(session_tab_scroll_reveal(1, 2, &widths, avail), 1);
        assert_eq!(session_tab_scroll_reveal(1, 3, &widths, avail), 1);
        let shown = session_tab_scroll_reveal(1, 5, &widths, avail);
        assert_eq!(shown, 3);
        assert_eq!(session_tab_scroll_including(5, &widths, avail), 3);
        assert_eq!(session_tab_scroll_reveal(1, 0, &widths, avail), 0);
    }
}
