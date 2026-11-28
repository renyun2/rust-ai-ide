use crate::canvas::{FontStyle, Rect, SurfaceCanvas};
use crate::ide_state::{blend_rgb, HitAction};
use ide_agent_hub::detect_workspace_file_references;
use ide_core::{IdeaTheme, LanguageId, ThemeKind, UiLanguage};
use ide_syntax::{highlight_line_with_state, HighlightState, TokenKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MdSpanKind {
    Plain,
    Bold,
    Italic,
    BoldItalic,
    Strikethrough,
    InlineCode,
    Link { target: String },
    FileLink { path: PathBuf, line: Option<usize> },
}

#[derive(Debug, Clone)]
pub(crate) struct MdSpan {
    pub start_char: usize,
    pub end_char: usize,
    pub kind: MdSpanKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MdAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MdCalloutKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl MdCalloutKind {
    pub(crate) fn from_tag(tag: &str) -> Option<(Self, &str)> {
        let trimmed = tag.trim_start();
        for (prefix, kind) in [
            ("[!NOTE]", Self::Note),
            ("[!TIP]", Self::Tip),
            ("[!IMPORTANT]", Self::Important),
            ("[!WARNING]", Self::Warning),
            ("[!CAUTION]", Self::Caution),
        ] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                    return Some((kind, rest.trim_start()));
                }
            }
        }
        None
    }

    pub(crate) fn label(&self, lang: UiLanguage) -> &'static str {
        match (self, lang) {
            (Self::Note, UiLanguage::ZhCn) => "提示 Note",
            (Self::Note, UiLanguage::EnUs) => "NOTE",
            (Self::Tip, UiLanguage::ZhCn) => "技巧 Tip",
            (Self::Tip, UiLanguage::EnUs) => "TIP",
            (Self::Important, UiLanguage::ZhCn) => "重要 Important",
            (Self::Important, UiLanguage::EnUs) => "IMPORTANT",
            (Self::Warning, UiLanguage::ZhCn) => "警告 Warning",
            (Self::Warning, UiLanguage::EnUs) => "WARNING",
            (Self::Caution, UiLanguage::ZhCn) => "注意 Caution",
            (Self::Caution, UiLanguage::EnUs) => "CAUTION",
        }
    }

    pub(crate) fn color(&self, t: &IdeaTheme) -> u32 {
        match self {
            Self::Note => t.accent_blue,
            Self::Tip => t.run_green,
            Self::Important => t.syn_field_or_property,
            Self::Warning => t.warning_yellow,
            Self::Caution => t.breakpoint_red,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MdListKind {
    Bullet { indent: usize },
    Ordered { indent: usize, number: usize },
    Task { indent: usize, checked: bool },
}

impl MdListKind {
    pub(crate) fn indent(&self) -> usize {
        match self {
            Self::Bullet { indent } | Self::Ordered { indent, .. } | Self::Task { indent, .. } => {
                *indent
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MdLineContent {
    pub text: String,
    pub spans: Vec<MdSpan>,
    pub pill_reserve: i32,
}

#[derive(Debug, Clone)]
pub(crate) enum MdBlock {
    Blank,
    Heading {
        level: u8,
        content: MdLineContent,
    },
    ParagraphLine {
        content: MdLineContent,
    },
    ListItem {
        kind: MdListKind,
        content: MdLineContent,
    },
    BlockquoteLine {
        depth: usize,
        callout: Option<MdCalloutKind>,
        is_callout_header: bool,
        content: MdLineContent,
    },
    HorizontalRule,
    CodeBlock {
        lang_tag: String,
        lang_id: LanguageId,
        full_code: Arc<str>,
        lines: Vec<String>,
        line_count: usize,
        preview: String,
    },
    Table {
        alignments: Vec<MdAlign>,
        headers: Vec<MdLineContent>,
        rows: Vec<Vec<MdLineContent>>,
    },
}

fn is_escapable_md_char(c: char) -> bool {
    matches!(
        c,
        '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.' | '!'
            | '|' | '~' | '>'
    )
}

fn is_url_char(c: char) -> bool {
    !c.is_whitespace() && !matches!(c, '<' | '>' | '"' | '\'' | '`' | '[' | ']')
}

fn find_matching_closing_seq(chars: &[char], from: usize, delim: char, count: usize) -> Option<usize> {
    let mut j = from;
    while j + count <= chars.len() {
        if chars[j] == '\\' && j + 1 < chars.len() {
            j += 2;
            continue;
        }
        if chars[j] == '`' {
            let ticks = chars[j..].iter().take_while(|&&c| c == '`').count();
            j += ticks;
            while j < chars.len() {
                if chars[j] == '`' {
                    let close_t = chars[j..].iter().take_while(|&&c| c == '`').count();
                    j += close_t;
                    if close_t == ticks {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            continue;
        }
        if (0..count).all(|k| chars[j + k] == delim) {
            // Avoid matching inside a longer run of the same delimiter
            let prev_same = j > 0 && chars[j - 1] == delim;
            let next_same = j + count < chars.len() && chars[j + count] == delim;
            if !prev_same && !next_same {
                return Some(j);
            }
        }
        j += 1;
    }
    None
}

fn strip_markdown_inline_text(raw: &str) -> String {
    let (clean, _) = parse_markdown_inline(raw);
    clean
}

/// Parses inline Markdown spans: escapes, multi-backtick code, images, links,
/// bold/italic (`**`, `__`, `*`, `_`, `***`, `___`), strikethrough (`~~`), and bare URLs.
pub(crate) fn parse_markdown_inline(raw: &str) -> (String, Vec<MdSpan>) {
    let chars: Vec<char> = raw.chars().collect();
    let mut clean = String::with_capacity(raw.len());
    let mut spans = Vec::new();
    let mut clean_len = 0usize;
    let mut i = 0usize;

    while i < chars.len() {
        // Backslash escape
        if chars[i] == '\\' && i + 1 < chars.len() && is_escapable_md_char(chars[i + 1]) {
            clean.push(chars[i + 1]);
            clean_len += 1;
            i += 2;
            continue;
        }

        // Inline code: one or more backticks
        if chars[i] == '`' {
            let ticks = chars[i..].iter().take_while(|&&c| c == '`').count();
            let mut j = i + ticks;
            let mut found = None;
            while j + ticks <= chars.len() {
                if chars[j] == '`' {
                    let close_t = chars[j..].iter().take_while(|&&c| c == '`').count();
                    if close_t == ticks {
                        found = Some(j);
                        break;
                    }
                    j += close_t;
                } else {
                    j += 1;
                }
            }
            if let Some(end_idx) = found {
                let mut inner: Vec<char> = chars[i + ticks..end_idx].to_vec();
                if inner.len() >= 2
                    && inner.first() == Some(&' ')
                    && inner.last() == Some(&' ')
                    && inner.iter().any(|&c| c != ' ')
                {
                    inner.remove(0);
                    inner.pop();
                }
                if !inner.is_empty() {
                    let start = clean_len;
                    for c in inner {
                        clean.push(c);
                        clean_len += 1;
                    }
                    spans.push(MdSpan {
                        start_char: start,
                        end_char: clean_len,
                        kind: MdSpanKind::InlineCode,
                    });
                    i = end_idx + ticks;
                    continue;
                }
            }
        }

        // Image ![alt](url) or Link [label](url)
        let is_image = chars[i] == '!' && i + 1 < chars.len() && chars[i + 1] == '[';
        if is_image || chars[i] == '[' {
            let bracket_start = if is_image { i + 1 } else { i };
            let mut b_depth = 1usize;
            let mut j = bracket_start + 1;
            let mut close_bracket = None;
            while j < chars.len() {
                if chars[j] == '\\' && j + 1 < chars.len() {
                    j += 2;
                    continue;
                }
                if chars[j] == '[' {
                    b_depth += 1;
                } else if chars[j] == ']' {
                    b_depth -= 1;
                    if b_depth == 0 {
                        close_bracket = Some(j);
                        break;
                    }
                }
                j += 1;
            }
            if let Some(cb) = close_bracket {
                if cb + 1 < chars.len() && chars[cb + 1] == '(' {
                    let mut p_depth = 1usize;
                    let mut k = cb + 2;
                    let mut close_paren = None;
                    while k < chars.len() {
                        if chars[k] == '\\' && k + 1 < chars.len() {
                            k += 2;
                            continue;
                        }
                        if chars[k] == '(' {
                            p_depth += 1;
                        } else if chars[k] == ')' {
                            p_depth -= 1;
                            if p_depth == 0 {
                                close_paren = Some(k);
                                break;
                            }
                        }
                        k += 1;
                    }
                    if let Some(cp) = close_paren {
                        let raw_label: String = chars[bracket_start + 1..cb].iter().collect();
                        let raw_dest: String = chars[cb + 2..cp].iter().collect();
                        let target = parse_link_destination(&raw_dest);
                        if !target.is_empty() {
                            let label_clean = if is_image {
                                let alt = strip_markdown_inline_text(&raw_label);
                                if alt.trim().is_empty() {
                                    "image".to_string()
                                } else {
                                    alt
                                }
                            } else {
                                let stripped = strip_markdown_inline_text(&raw_label);
                                if stripped.is_empty() {
                                    target.clone()
                                } else {
                                    stripped
                                }
                            };
                            let start = clean_len;
                            for ch in label_clean.chars() {
                                clean.push(ch);
                                clean_len += 1;
                            }
                            spans.push(MdSpan {
                                start_char: start,
                                end_char: clean_len,
                                kind: MdSpanKind::Link { target },
                            });
                            i = cp + 1;
                            continue;
                        }
                    }
                }
            }
        }

        // Strikethrough ~~...~~
        if i + 1 < chars.len() && chars[i] == '~' && chars[i + 1] == '~' {
            if let Some(end_idx) = find_matching_closing_seq(&chars, i + 2, '~', 2) {
                if end_idx > i + 2 {
                    let inner_str: String = chars[i + 2..end_idx].iter().collect();
                    let inner_clean = strip_markdown_inline_text(&inner_str);
                    if !inner_clean.is_empty() {
                        let start = clean_len;
                        for c in inner_clean.chars() {
                            clean.push(c);
                            clean_len += 1;
                        }
                        spans.push(MdSpan {
                            start_char: start,
                            end_char: clean_len,
                            kind: MdSpanKind::Strikethrough,
                        });
                        i = end_idx + 2;
                        continue;
                    }
                }
            }
        }

        // Bold+Italic ***...*** or ___...___
        if (chars[i] == '*' || chars[i] == '_')
            && i + 2 < chars.len()
            && chars[i + 1] == chars[i]
            && chars[i + 2] == chars[i]
        {
            let delim = chars[i];
            let prev_word = delim == '_' && i > 0 && chars[i - 1].is_alphanumeric();
            if !prev_word {
                if let Some(end_idx) = find_matching_closing_seq(&chars, i + 3, delim, 3) {
                    let next_word = delim == '_'
                        && end_idx + 3 < chars.len()
                        && chars[end_idx + 3].is_alphanumeric();
                    if !next_word && end_idx > i + 3 {
                        let inner_str: String = chars[i + 3..end_idx].iter().collect();
                        let inner_clean = strip_markdown_inline_text(&inner_str);
                        if !inner_clean.is_empty() {
                            let start = clean_len;
                            for c in inner_clean.chars() {
                                clean.push(c);
                                clean_len += 1;
                            }
                            spans.push(MdSpan {
                                start_char: start,
                                end_char: clean_len,
                                kind: MdSpanKind::BoldItalic,
                            });
                            i = end_idx + 3;
                            continue;
                        }
                    }
                }
            }
        }

        // Bold **...** or __...__
        if (chars[i] == '*' || chars[i] == '_') && i + 1 < chars.len() && chars[i + 1] == chars[i] {
            let delim = chars[i];
            let prev_word = delim == '_' && i > 0 && chars[i - 1].is_alphanumeric();
            if !prev_word {
                if let Some(end_idx) = find_matching_closing_seq(&chars, i + 2, delim, 2) {
                    let next_word = delim == '_'
                        && end_idx + 2 < chars.len()
                        && chars[end_idx + 2].is_alphanumeric();
                    if !next_word && end_idx > i + 2 {
                        let inner_str: String = chars[i + 2..end_idx].iter().collect();
                        let inner_clean = strip_markdown_inline_text(&inner_str);
                        if !inner_clean.is_empty() {
                            let start = clean_len;
                            for c in inner_clean.chars() {
                                clean.push(c);
                                clean_len += 1;
                            }
                            spans.push(MdSpan {
                                start_char: start,
                                end_char: clean_len,
                                kind: MdSpanKind::Bold,
                            });
                            i = end_idx + 2;
                            continue;
                        }
                    }
                }
            }
        }

        // Italic *...* or _..._
        if chars[i] == '*' || chars[i] == '_' {
            let delim = chars[i];
            let prev_word = delim == '_' && i > 0 && chars[i - 1].is_alphanumeric();
            let next_space = i + 1 >= chars.len() || chars[i + 1].is_whitespace();
            if !prev_word && !next_space {
                if let Some(end_idx) = find_matching_closing_seq(&chars, i + 1, delim, 1) {
                    let close_prev_space = chars[end_idx - 1].is_whitespace();
                    let next_word = delim == '_'
                        && end_idx + 1 < chars.len()
                        && chars[end_idx + 1].is_alphanumeric();
                    if !close_prev_space && !next_word && end_idx > i + 1 {
                        let inner_str: String = chars[i + 1..end_idx].iter().collect();
                        let inner_clean = strip_markdown_inline_text(&inner_str);
                        if !inner_clean.is_empty() {
                            let start = clean_len;
                            for c in inner_clean.chars() {
                                clean.push(c);
                                clean_len += 1;
                            }
                            spans.push(MdSpan {
                                start_char: start,
                                end_char: clean_len,
                                kind: MdSpanKind::Italic,
                            });
                            i = end_idx + 1;
                            continue;
                        }
                    }
                }
            }
        }

        // Bare URL http:// or https://
        if chars[i] == 'h' {
            let rem: String = chars[i..].iter().take(8).collect();
            if rem.starts_with("https://") || rem.starts_with("http://") {
                let prev_ok = i == 0 || !chars[i - 1].is_alphanumeric();
                if prev_ok {
                    let mut end = i;
                    while end < chars.len() && is_url_char(chars[end]) {
                        end += 1;
                    }
                    while end > i + 8 && matches!(chars[end - 1], '.' | ',' | ';' | ':' | ')' | '!') {
                        end -= 1;
                    }
                    if end > i + 8 {
                        let url: String = chars[i..end].iter().collect();
                        let start = clean_len;
                        for c in url.chars() {
                            clean.push(c);
                            clean_len += 1;
                        }
                        spans.push(MdSpan {
                            start_char: start,
                            end_char: clean_len,
                            kind: MdSpanKind::Link { target: url },
                        });
                        i = end;
                        continue;
                    }
                }
            }
        }

        clean.push(chars[i]);
        clean_len += 1;
        i += 1;
    }

    (clean, spans)
}

fn parse_link_destination(raw_dest: &str) -> String {
    let trimmed = raw_dest.trim();
    if let Some(rest) = trimmed.strip_prefix('<') {
        if let Some(end) = rest.find('>') {
            return rest[..end].trim().to_string();
        }
    }
    let mut url = String::new();
    for c in trimmed.chars() {
        if c == '"' || c.is_whitespace() {
            break;
        }
        url.push(c);
    }
    url
}

fn resolve_target_file(
    workspace_root: &Path,
    base_dir: Option<&Path>,
    target: &str,
) -> Option<(PathBuf, Option<usize>)> {
    if target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with("mailto:")
        || target.starts_with('#')
    {
        return None;
    }
    let (raw_path, line_opt) = if let Some((p, frag)) = target.split_once("#L") {
        let ln = frag
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|s| s.parse::<usize>().ok());
        (p, ln)
    } else if let Some((p, frag)) = target.split_once('#') {
        let ln = frag
            .strip_prefix('L')
            .and_then(|s| s.parse::<usize>().ok());
        (p, ln)
    } else if let Some((p, tail)) = target.rsplit_once(':') {
        if !p.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            (p, tail.parse::<usize>().ok())
        } else {
            (target, None)
        }
    } else {
        (target, None)
    };
    let clean_rel = raw_path.trim().trim_start_matches("./");
    if clean_rel.is_empty() {
        return None;
    }
    let rel_p = Path::new(clean_rel);
    if rel_p.is_absolute() && rel_p.exists() {
        return Some((rel_p.to_path_buf(), line_opt));
    }
    if let Some(dir) = base_dir {
        let cand = dir.join(rel_p);
        if cand.exists() {
            return Some((cand, line_opt));
        }
    }
    let ws_cand = workspace_root.join(rel_p);
    if ws_cand.exists() {
        return Some((ws_cand, line_opt));
    }
    None
}

pub(crate) fn merge_file_references(
    workspace_root: &Path,
    base_dir: Option<&Path>,
    clean: &str,
    raw_spans: &[MdSpan],
) -> Vec<MdSpan> {
    let seg_len = clean.chars().count();
    let mut spans: Vec<MdSpan> = raw_spans
        .iter()
        .map(|s| {
            let mut sp = s.clone();
            if let MdSpanKind::Link { ref target } = s.kind {
                if let Some((path, line)) = resolve_target_file(workspace_root, base_dir, target) {
                    sp.kind = MdSpanKind::FileLink { path, line };
                }
            }
            sp
        })
        .collect();

    let refs = detect_workspace_file_references(workspace_root, clean);
    for (tok, path, line_opt) in refs {
        if let Some(byte_pos) = clean.find(&tok) {
            let start_char = clean[..byte_pos].chars().count();
            let end_char = (start_char + tok.chars().count()).min(seg_len);
            if end_char <= start_char {
                continue;
            }
            // Do not override explicit markdown links
            let hits_explicit_link = spans.iter().any(|s| {
                matches!(s.kind, MdSpanKind::Link { .. })
                    && s.end_char > start_char
                    && s.start_char < end_char
            });
            if hits_explicit_link {
                continue;
            }
            spans.retain(|s| s.end_char <= start_char || s.start_char >= end_char);
            spans.push(MdSpan {
                start_char,
                end_char,
                kind: MdSpanKind::FileLink {
                    path,
                    line: line_opt,
                },
            });
        }
    }
    spans.sort_by_key(|s| s.start_char);
    spans
}

pub(crate) fn inline_pill_reserve(spans: &[MdSpan]) -> i32 {
    spans
        .iter()
        .map(|s| match s.kind {
            MdSpanKind::InlineCode => 12,
            MdSpanKind::FileLink { .. } => 8,
            _ => 0,
        })
        .sum::<i32>()
        .min(64)
}

fn make_line_content(
    workspace_root: &Path,
    base_dir: Option<&Path>,
    raw_line: &str,
) -> MdLineContent {
    let (clean, raw_spans) = parse_markdown_inline(raw_line);
    let spans = merge_file_references(workspace_root, base_dir, &clean, &raw_spans);
    let pill_reserve = inline_pill_reserve(&spans);
    MdLineContent {
        text: clean,
        spans,
        pill_reserve,
    }
}

fn fence_opener(trimmed: &str) -> Option<(char, usize, String)> {
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
        Some((ch, count, rest.trim().to_string()))
    } else {
        None
    }
}

fn parse_atx_heading(trimmed: &str) -> Option<(u8, String)> {
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let after = &trimmed[hashes..];
    if !after.starts_with(char::is_whitespace) {
        return None;
    }
    let cleaned = after
        .trim()
        .trim_end_matches(|c: char| c == '#' || c == ' ' || c == '\t')
        .trim();
    if cleaned.is_empty() {
        return None;
    }
    Some((hashes as u8, cleaned.to_string()))
}

fn is_horizontal_rule(trimmed: &str) -> bool {
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

fn split_table_cells(line: &str) -> Vec<String> {
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
                    cur.push('|');
                    chars.next();
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

fn parse_table_separator(line: &str) -> Option<Vec<MdAlign>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let cells = split_table_cells(trimmed);
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
            (true, true) => MdAlign::Center,
            (false, true) => MdAlign::Right,
            _ => MdAlign::Left,
        });
    }
    Some(aligns)
}

fn parse_list_item(line: &str) -> Option<(MdListKind, &str)> {
    let indent_bytes = line.len() - line.trim_start_matches([' ', '\t']).len();
    let indent_spaces = line[..indent_bytes]
        .chars()
        .map(|c| if c == '\t' { 2 } else { 1 })
        .sum::<usize>();
    let indent = (indent_spaces / 2).min(6);
    let rest = &line[indent_bytes..];

    for marker in ["- ", "* ", "+ "] {
        if let Some(after) = rest.strip_prefix(marker) {
            let trimmed_after = after.trim_start();
            if let Some(task_rest) = trimmed_after
                .strip_prefix("[ ]")
                .or_else(|| trimmed_after.strip_prefix("[x]"))
                .or_else(|| trimmed_after.strip_prefix("[X]"))
            {
                if task_rest.is_empty() || task_rest.starts_with(char::is_whitespace) {
                    let checked = !trimmed_after.starts_with("[ ]");
                    return Some((
                        MdListKind::Task { indent, checked },
                        task_rest.trim_start(),
                    ));
                }
            }
            return Some((MdListKind::Bullet { indent }, trimmed_after));
        }
    }

    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits <= 6 {
        let after_digits = &rest[digits..];
        if let Some(after_marker) = after_digits
            .strip_prefix(". ")
            .or_else(|| after_digits.strip_prefix(") "))
        {
            if let Ok(number) = rest[..digits].parse::<usize>() {
                return Some((
                    MdListKind::Ordered { indent, number },
                    after_marker.trim_start(),
                ));
            }
        }
    }
    None
}

fn parse_blockquote_line(trimmed: &str) -> Option<(usize, &str)> {
    if !trimmed.starts_with('>') {
        return None;
    }
    let mut depth = 0usize;
    let mut rest = trimmed;
    while let Some(after) = rest.strip_prefix('>') {
        depth += 1;
        rest = after.trim_start();
    }
    Some((depth.min(4), rest))
}

/// Parses a full Markdown document or AI message into structured `MdBlock`s.
pub(crate) fn parse_markdown_blocks(
    workspace_root: &Path,
    base_dir: Option<&Path>,
    markdown: &str,
) -> Vec<MdBlock> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut blocks: Vec<MdBlock> = Vec::new();
    let mut i = 0usize;
    let mut active_callout: Option<MdCalloutKind> = None;

    let push_code = |tag: String, code_lines: Vec<String>, out: &mut Vec<MdBlock>| {
        let full_code: Arc<str> = Arc::from(code_lines.join("\n"));
        let line_count = code_lines.len().max(1);
        let preview = code_lines
            .iter()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("...")
            .to_string();
        let lang_id = LanguageId::from_fence_tag(&tag);
        let lang_tag = if tag.is_empty() {
            "code".to_string()
        } else {
            tag
        };
        out.push(MdBlock::CodeBlock {
            lang_tag,
            lang_id,
            full_code,
            lines: code_lines,
            line_count,
            preview,
        });
    };

    while i < lines.len() {
        let raw = lines[i];
        let trimmed = raw.trim_start();

        // Fenced code block (``` or ~~~)
        if let Some((fch, flen, tag)) = fence_opener(trimmed) {
            active_callout = None;
            let mut code_lines = Vec::new();
            i += 1;
            while i < lines.len() {
                let inner_trimmed = lines[i].trim_start();
                let close_cnt = inner_trimmed.chars().take_while(|&c| c == fch).count();
                if close_cnt >= flen && inner_trimmed[close_cnt..].trim().is_empty() {
                    i += 1;
                    break;
                }
                code_lines.push(lines[i].to_string());
                i += 1;
            }
            push_code(tag, code_lines, &mut blocks);
            continue;
        }

        if trimmed.is_empty() {
            active_callout = None;
            if !matches!(blocks.last(), Some(MdBlock::Blank) | None) {
                blocks.push(MdBlock::Blank);
            }
            i += 1;
            continue;
        }

        // GFM Table
        if raw.contains('|') && i + 1 < lines.len() {
            if let Some(mut alignments) = parse_table_separator(lines[i + 1]) {
                active_callout = None;
                let header_raw = split_table_cells(raw);
                let cols = alignments.len().max(header_raw.len());
                while alignments.len() < cols {
                    alignments.push(MdAlign::Left);
                }
                let mut headers: Vec<MdLineContent> = header_raw
                    .into_iter()
                    .map(|c| make_line_content(workspace_root, base_dir, &c))
                    .collect();
                while headers.len() < cols {
                    headers.push(make_line_content(workspace_root, base_dir, ""));
                }

                let mut rows = Vec::new();
                let mut j = i + 2;
                while j < lines.len() {
                    let row_raw = lines[j];
                    let row_tr = row_raw.trim();
                    if row_tr.is_empty() || !row_tr.contains('|') {
                        break;
                    }
                    if fence_opener(row_tr).is_some()
                        || parse_atx_heading(row_tr).is_some()
                        || is_horizontal_rule(row_tr)
                    {
                        break;
                    }
                    let mut cells: Vec<MdLineContent> = split_table_cells(row_raw)
                        .into_iter()
                        .map(|c| make_line_content(workspace_root, base_dir, &c))
                        .collect();
                    while cells.len() < cols {
                        cells.push(make_line_content(workspace_root, base_dir, ""));
                    }
                    cells.truncate(cols);
                    rows.push(cells);
                    j += 1;
                }

                blocks.push(MdBlock::Table {
                    alignments,
                    headers,
                    rows,
                });
                i = j;
                continue;
            }
        }

        // Setext heading: single paragraph line followed by === or ---
        if i + 1 < lines.len() {
            let next_tr = lines[i + 1].trim();
            let setext_level = if next_tr.len() >= 3 && next_tr.chars().all(|c| c == '=') {
                Some(1u8)
            } else if next_tr.len() >= 3 && next_tr.chars().all(|c| c == '-') {
                Some(2u8)
            } else {
                None
            };
            if let Some(level) = setext_level {
                if !trimmed.starts_with('>')
                    && parse_atx_heading(trimmed).is_none()
                    && parse_list_item(raw).is_none()
                {
                    active_callout = None;
                    blocks.push(MdBlock::Heading {
                        level,
                        content: make_line_content(workspace_root, base_dir, trimmed.trim()),
                    });
                    i += 2;
                    continue;
                }
            }
        }

        // ATX Heading # .. ######
        if let Some((level, title)) = parse_atx_heading(trimmed) {
            active_callout = None;
            blocks.push(MdBlock::Heading {
                level,
                content: make_line_content(workspace_root, base_dir, &title),
            });
            i += 1;
            continue;
        }

        // Horizontal rule
        if is_horizontal_rule(trimmed) {
            active_callout = None;
            blocks.push(MdBlock::HorizontalRule);
            i += 1;
            continue;
        }

        // Blockquote / Callout
        if let Some((depth, body)) = parse_blockquote_line(trimmed) {
            let mut is_callout_header = false;
            let mut quote_text = body;
            if active_callout.is_none() {
                if let Some((kind, rest)) = MdCalloutKind::from_tag(body) {
                    active_callout = Some(kind);
                    is_callout_header = true;
                    quote_text = rest;
                }
            }
            blocks.push(MdBlock::BlockquoteLine {
                depth,
                callout: active_callout,
                is_callout_header,
                content: make_line_content(workspace_root, base_dir, quote_text),
            });
            i += 1;
            continue;
        }
        active_callout = None;

        // List item (Task / Unordered / Ordered)
        if let Some((kind, body)) = parse_list_item(raw) {
            blocks.push(MdBlock::ListItem {
                kind,
                content: make_line_content(workspace_root, base_dir, body),
            });
            i += 1;
            continue;
        }

        // Regular prose line
        blocks.push(MdBlock::ParagraphLine {
            content: make_line_content(workspace_root, base_dir, raw),
        });
        i += 1;
    }

    blocks
}

pub(crate) fn heading_font_style(level: u8) -> FontStyle {
    match level {
        1 => FontStyle::ChatH1,
        2 => FontStyle::ChatH2,
        3 => FontStyle::ChatH3,
        _ => FontStyle::ChatBodyBold,
    }
}

pub(crate) fn heading_row_height(level: u8) -> i32 {
    match level {
        1 => 34,
        2 => 30,
        3 => 26,
        _ => 24,
    }
}

pub(crate) fn span_visual_metrics(kind: &MdSpanKind, base_style: FontStyle) -> (FontStyle, i32, i32) {
    let bold_style = match base_style {
        FontStyle::ChatH1 | FontStyle::ChatH2 | FontStyle::ChatH3 => base_style,
        _ => FontStyle::ChatBodyBold,
    };
    let italic_style = match base_style {
        FontStyle::ChatBodyBold | FontStyle::ChatH1 | FontStyle::ChatH2 | FontStyle::ChatH3 => {
            FontStyle::ChatBodyBoldItalic
        }
        _ => FontStyle::ChatBodyItalic,
    };
    match kind {
        MdSpanKind::Plain | MdSpanKind::Strikethrough => (base_style, 0, 0),
        MdSpanKind::Bold => (bold_style, 0, 0),
        MdSpanKind::Italic => (italic_style, 0, 0),
        MdSpanKind::BoldItalic => (FontStyle::ChatBodyBoldItalic, 0, 0),
        MdSpanKind::InlineCode => (FontStyle::ChatCode, 6, 6),
        MdSpanKind::Link { .. } => (base_style, 1, 1),
        MdSpanKind::FileLink { .. } => (FontStyle::ChatCode, 4, 4),
    }
}

pub(crate) fn build_normalized_segments(
    char_len: usize,
    spans: &[MdSpan],
) -> Vec<(usize, usize, MdSpanKind)> {
    let mut out = Vec::new();
    let mut cur = 0usize;
    for sp in spans {
        let s = sp.start_char.min(char_len);
        let e = sp.end_char.min(char_len);
        if s > cur {
            out.push((cur, s, MdSpanKind::Plain));
        }
        if e > s {
            out.push((s, e, sp.kind.clone()));
            cur = e;
        }
    }
    if cur < char_len {
        out.push((cur, char_len, MdSpanKind::Plain));
    }
    out
}

pub(crate) fn project_md_spans_slice(
    seg_start_char: usize,
    seg_end_char: usize,
    full_spans: &[MdSpan],
) -> Vec<MdSpan> {
    if full_spans.is_empty() || seg_end_char <= seg_start_char {
        return Vec::new();
    }
    let mut spans = Vec::new();
    for sp in full_spans {
        let ov_s = sp.start_char.max(seg_start_char);
        let ov_e = sp.end_char.min(seg_end_char);
        if ov_e > ov_s {
            spans.push(MdSpan {
                start_char: ov_s - seg_start_char,
                end_char: ov_e - seg_start_char,
                kind: sp.kind.clone(),
            });
        }
    }
    spans
}

pub(crate) fn measure_md_spans_x(
    canvas: &SurfaceCanvas,
    text: &str,
    spans: &[MdSpan],
    base_style: FontStyle,
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
        let (style, pad_l, pad_r) = span_visual_metrics(&kind, base_style);
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

pub(crate) fn hit_test_md_spans_col(
    canvas: &SurfaceCanvas,
    text: &str,
    spans: &[MdSpan],
    base_style: FontStyle,
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
        let (style, pad_l, pad_r) = span_visual_metrics(&kind, base_style);
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

/// Wraps text by pixel width, preferring spaces/word boundaries, returning `(start_char, end_char, segment)`.
pub(crate) fn wrap_md_line_segments(
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_md_spans_text(
    canvas: &mut SurfaceCanvas,
    t: &IdeaTheme,
    hover_pos: (i32, i32),
    hit_regions: &mut Vec<(Rect, HitAction)>,
    row_rect: Rect,
    text_x: i32,
    text_y: i32,
    text: &str,
    spans: &[MdSpan],
    base_color: u32,
    max_w: i32,
    base_style: FontStyle,
    align: MdAlign,
) {
    let drawn_x = if align == MdAlign::Left {
        text_x
    } else {
        let total_w = if spans.is_empty() {
            canvas.measure_text(text, base_style).0
        } else {
            measure_md_spans_x(canvas, text, spans, base_style, usize::MAX)
        };
        let free = (max_w - total_w).max(0);
        match align {
            MdAlign::Left => text_x,
            MdAlign::Center => text_x + free / 2,
            MdAlign::Right => text_x + free,
        }
    };

    if spans.is_empty() {
        canvas.draw_text_styled(drawn_x, text_y, text, base_color, max_w, base_style);
        return;
    }

    let (hx, hy) = hover_pos;
    let mut cur_x = drawn_x;
    let right_edge = text_x + max_w;
    let (_, body_h) = canvas.measure_text("Ag中文", base_style);
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
    let link_fg = if t.kind == ThemeKind::IslandsLight {
        t.accent_blue
    } else {
        blend_rgb(t.accent_blue, 0xFFFFFF, 0.28)
    };

    let mut char_cursor = 0usize;
    let mut byte_cursor = 0usize;
    let mut ci = text.char_indices();
    let mut advance_to = |target_char: usize| -> usize {
        while char_cursor < target_char {
            if let Some((b, ch)) = ci.next() {
                byte_cursor = b + ch.len_utf8();
                char_cursor += 1;
            } else {
                byte_cursor = text.len();
                break;
            }
        }
        byte_cursor
    };

    let mut emit_segment = |cur_x: &mut i32, chunk: &str, kind: &MdSpanKind| -> bool {
        if *cur_x >= right_edge {
            return false;
        }
        if chunk.is_empty() {
            return true;
        }
        match kind {
            MdSpanKind::Plain => {
                *cur_x = canvas.draw_text_styled(
                    *cur_x,
                    text_y,
                    chunk,
                    base_color,
                    right_edge - *cur_x,
                    base_style,
                );
            }
            MdSpanKind::Bold => {
                let st = if matches!(base_style, FontStyle::ChatH1 | FontStyle::ChatH2 | FontStyle::ChatH3) {
                    base_style
                } else {
                    FontStyle::ChatBodyBold
                };
                *cur_x = canvas.draw_text_styled(
                    *cur_x,
                    text_y,
                    chunk,
                    base_color,
                    right_edge - *cur_x,
                    st,
                );
            }
            MdSpanKind::Italic => {
                let st = if matches!(base_style, FontStyle::ChatBodyBold | FontStyle::ChatH1 | FontStyle::ChatH2 | FontStyle::ChatH3) {
                    FontStyle::ChatBodyBoldItalic
                } else {
                    FontStyle::ChatBodyItalic
                };
                *cur_x = canvas.draw_text_styled(
                    *cur_x,
                    text_y,
                    chunk,
                    blend_rgb(base_color, t.text_secondary, 0.12),
                    right_edge - *cur_x,
                    st,
                );
            }
            MdSpanKind::BoldItalic => {
                *cur_x = canvas.draw_text_styled(
                    *cur_x,
                    text_y,
                    chunk,
                    base_color,
                    right_edge - *cur_x,
                    FontStyle::ChatBodyBoldItalic,
                );
            }
            MdSpanKind::Strikethrough => {
                let sx = *cur_x;
                let ex = canvas.draw_text_styled(
                    sx,
                    text_y,
                    chunk,
                    t.text_muted,
                    right_edge - sx,
                    base_style,
                );
                let strike_w = (ex - sx).max(2);
                let strike_y = text_y + (body_h / 2).max(6);
                canvas.fill_rect(Rect::new(sx, strike_y, strike_w, 1), t.text_muted);
                *cur_x = ex;
            }
            MdSpanKind::InlineCode => {
                if right_edge - *cur_x < 12 {
                    return false;
                }
                let (cw, _) = canvas.measure_text(chunk, FontStyle::ChatCode);
                let pill_w = (cw + 8).min((right_edge - *cur_x - 2).max(10));
                let pill_h = (code_h + 3).min(row_rect.h - 2);
                let pill_rect = Rect::new(*cur_x + 2, text_y + code_y_off - 1, pill_w, pill_h);
                canvas.stroke_rounded_rect(pill_rect, 4, inline_pill_border, inline_pill_bg);
                canvas.draw_text_styled(
                    pill_rect.x + 4,
                    text_y + code_y_off,
                    chunk,
                    inline_code_fg,
                    (pill_w - 6).max(4),
                    FontStyle::ChatCode,
                );
                *cur_x += cw + 12;
            }
            MdSpanKind::Link { target } => {
                let (lw, _) = canvas.measure_text(chunk, base_style);
                let draw_w = lw.min((right_edge - *cur_x - 2).max(4));
                let link_rect = Rect::new(*cur_x, text_y, draw_w + 2, (body_h + 2).min(row_rect.h));
                let hovered = link_rect.contains(hx, hy);
                let sx = *cur_x + 1;
                let ex = canvas.draw_text_styled(
                    sx,
                    text_y,
                    chunk,
                    if hovered { 0xFFFFFF } else { link_fg },
                    (right_edge - sx).max(4),
                    base_style,
                );
                canvas.fill_rect(
                    Rect::new(sx, text_y + body_h - 1, (ex - sx).max(2), 1),
                    if hovered { 0xFFFFFF } else { link_fg },
                );
                if target.starts_with("http://") || target.starts_with("https://") {
                    hit_regions.push((link_rect, HitAction::OpenExternalUrl(target.clone())));
                }
                *cur_x = ex + 1;
            }
            MdSpanKind::FileLink { path, line } => {
                if right_edge - *cur_x < 12 {
                    return false;
                }
                let (lw, _) = canvas.measure_text(chunk, FontStyle::ChatCode);
                let pill_w = (lw + 6).min((right_edge - *cur_x - 1).max(8));
                let link_rect = Rect::new(
                    *cur_x + 1,
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
                    chunk,
                    if hovered { 0xFFFFFF } else { t.syn_function },
                    (pill_w - 4).max(4),
                    FontStyle::ChatCode,
                );
                hit_regions.push((link_rect, HitAction::OpenFileAtLine(path.clone(), *line)));
                *cur_x += lw + 8;
            }
        }
        true
    };

    let mut cur_c = 0usize;
    for sp in spans {
        if cur_x >= right_edge {
            break;
        }
        if sp.start_char > cur_c {
            let b0 = advance_to(cur_c);
            let b1 = advance_to(sp.start_char);
            if !emit_segment(&mut cur_x, &text[b0..b1], &MdSpanKind::Plain) {
                return;
            }
            cur_c = sp.start_char;
        }
        let s = sp.start_char.max(cur_c);
        if sp.end_char > s {
            let b0 = advance_to(s);
            let b1 = advance_to(sp.end_char);
            if !emit_segment(&mut cur_x, &text[b0..b1], &sp.kind) {
                return;
            }
            cur_c = sp.end_char;
        }
    }
    if cur_x < right_edge {
        let b0 = advance_to(cur_c);
        if b0 < text.len() {
            let _ = emit_segment(&mut cur_x, &text[b0..], &MdSpanKind::Plain);
        }
    }
}

pub(crate) type WrappedTableCell = Vec<(String, Vec<MdSpan>)>;

#[derive(Debug, Clone)]
pub(crate) struct PreparedTableRow {
    pub is_header: bool,
    pub row_idx: usize,
    pub cells: Vec<WrappedTableCell>,
    pub col_widths: Vec<i32>,
    pub alignments: Vec<MdAlign>,
    pub line_count: usize,
}

pub(crate) fn layout_table_rows(
    canvas: &SurfaceCanvas,
    alignments: &[MdAlign],
    headers: &[MdLineContent],
    rows: &[Vec<MdLineContent>],
    avail_w: i32,
) -> Vec<PreparedTableRow> {
    let cols = alignments.len().max(headers.len()).max(1);
    let table_w = avail_w.max(cols as i32 * 56);
    let mut desired = vec![56i32; cols];

    let measure_cell = |c: &MdLineContent, style: FontStyle| -> i32 {
        let w = if c.spans.is_empty() {
            canvas.measure_text(&c.text, style).0
        } else {
            measure_md_spans_x(canvas, &c.text, &c.spans, style, usize::MAX)
        };
        (w + 18).max(52)
    };

    for (ci, hc) in headers.iter().enumerate() {
        desired[ci] = desired[ci].max(measure_cell(hc, FontStyle::ChatBodyBold));
    }
    for row in rows {
        for (ci, cell) in row.iter().enumerate() {
            if ci < cols {
                desired[ci] = desired[ci].max(measure_cell(cell, FontStyle::ChatBody));
            }
        }
    }

    let sum_desired: i32 = desired.iter().sum();
    let mut col_widths = vec![0i32; cols];
    if sum_desired <= table_w {
        let extra = (table_w - sum_desired) / cols as i32;
        for ci in 0..cols {
            col_widths[ci] = desired[ci] + extra;
        }
    } else {
        let min_col = 48.min((table_w / cols as i32).max(28));
        let mut used = 0i32;
        for ci in 0..cols {
            let scaled = ((desired[ci] as i64 * table_w as i64) / sum_desired.max(1) as i64) as i32;
            col_widths[ci] = scaled.max(min_col);
            used += col_widths[ci];
        }
        if used > table_w {
            let excess = used - table_w;
            let widest = col_widths
                .iter()
                .enumerate()
                .max_by_key(|(_, w)| *w)
                .map(|(i, _)| i)
                .unwrap_or(0);
            col_widths[widest] = (col_widths[widest] - excess).max(28);
        }
    }
    // Fix rounding remainder on last column
    let total_cw: i32 = col_widths.iter().sum();
    if let Some(last) = col_widths.last_mut() {
        *last = (*last + (table_w - total_cw)).max(28);
    }

    let wrap_row = |is_header: bool, row_idx: usize, row_cells: &[MdLineContent]| -> PreparedTableRow {
        let style = if is_header {
            FontStyle::ChatBodyBold
        } else {
            FontStyle::ChatBody
        };
        let mut wrapped_cells = Vec::with_capacity(cols);
        let mut max_lines = 1usize;
        for ci in 0..cols {
            let cell = row_cells.get(ci);
            let cell_max_w = (col_widths[ci] - 14 - cell.map(|c| c.pill_reserve).unwrap_or(0)).max(24);
            let lines: WrappedTableCell = if let Some(c) = cell {
                if c.text.is_empty() {
                    vec![(String::new(), Vec::new())]
                } else {
                    wrap_md_line_segments(canvas, &c.text, cell_max_w, style)
                        .into_iter()
                        .map(|(s, e, seg)| {
                            let sp = project_md_spans_slice(s, e, &c.spans);
                            (seg, sp)
                        })
                        .collect()
                }
            } else {
                vec![(String::new(), Vec::new())]
            };
            max_lines = max_lines.max(lines.len());
            wrapped_cells.push(lines);
        }
        PreparedTableRow {
            is_header,
            row_idx,
            cells: wrapped_cells,
            col_widths: col_widths.clone(),
            alignments: alignments.to_vec(),
            line_count: max_lines,
        }
    };

    let mut out = Vec::with_capacity(rows.len() + 1);
    out.push(wrap_row(true, 0, headers));
    for (r_idx, r) in rows.iter().enumerate() {
        out.push(wrap_row(false, r_idx + 1, r));
    }
    out
}

pub(crate) fn table_row_height(line_count: usize) -> i32 {
    (line_count.max(1) as i32) * 21 + 8
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_prepared_table_row(
    canvas: &mut SurfaceCanvas,
    t: &IdeaTheme,
    hover_pos: (i32, i32),
    hit_regions: &mut Vec<(Rect, HitAction)>,
    row_rect: Rect,
    prepared: &PreparedTableRow,
    prose_fg: u32,
) {
    let header_bg = if t.kind == ThemeKind::IslandsLight {
        blend_rgb(t.panel_bg, t.accent_blue, 0.08)
    } else {
        blend_rgb(t.panel_bg, t.editor_bg, 0.42)
    };
    let even_bg = if t.kind == ThemeKind::IslandsLight {
        t.editor_bg
    } else {
        blend_rgb(t.window_bg, t.editor_bg, 0.65)
    };
    let odd_bg = if t.kind == ThemeKind::IslandsLight {
        blend_rgb(t.editor_bg, t.panel_bg, 0.55)
    } else {
        blend_rgb(t.panel_bg, t.window_bg, 0.40)
    };
    let bg = if prepared.is_header {
        header_bg
    } else if prepared.row_idx % 2 == 1 {
        even_bg
    } else {
        odd_bg
    };
    let brd = blend_rgb(t.border_color, t.panel_bg, 0.35);

    canvas.fill_rect(row_rect, bg);
    // Outer left/right & bottom border (+ top border for header)
    if prepared.is_header {
        canvas.fill_rect(Rect::new(row_rect.x, row_rect.y, row_rect.w, 1), brd);
    }
    canvas.fill_rect(
        Rect::new(row_rect.x, row_rect.y + row_rect.h - 1, row_rect.w, 1),
        brd,
    );
    canvas.fill_rect(Rect::new(row_rect.x, row_rect.y, 1, row_rect.h), brd);
    canvas.fill_rect(
        Rect::new(row_rect.x + row_rect.w - 1, row_rect.y, 1, row_rect.h),
        brd,
    );

    let style = if prepared.is_header {
        FontStyle::ChatBodyBold
    } else {
        FontStyle::ChatBody
    };
    let mut cx = row_rect.x;
    for (ci, cw) in prepared.col_widths.iter().enumerate() {
        let actual_w = (*cw).min((row_rect.x + row_rect.w - cx).max(0));
        if actual_w <= 0 {
            break;
        }
        if ci > 0 {
            canvas.fill_rect(Rect::new(cx, row_rect.y, 1, row_rect.h), brd);
        }
        let align = prepared
            .alignments
            .get(ci)
            .copied()
            .unwrap_or(MdAlign::Left);
        if let Some(cell_lines) = prepared.cells.get(ci) {
            for (li, (line_txt, line_spans)) in cell_lines.iter().enumerate() {
                let ly = row_rect.y + 4 + (li as i32) * 21;
                let cell_rect = Rect::new(cx + 7, ly, (actual_w - 14).max(8), 21);
                draw_md_spans_text(
                    canvas,
                    t,
                    hover_pos,
                    hit_regions,
                    cell_rect,
                    cell_rect.x,
                    ly,
                    line_txt,
                    line_spans,
                    if prepared.is_header {
                        t.text_primary
                    } else {
                        prose_fg
                    },
                    cell_rect.w,
                    style,
                    align,
                );
            }
        }
        cx += actual_w;
    }
}

pub(crate) fn draw_list_marker(
    canvas: &mut SurfaceCanvas,
    t: &IdeaTheme,
    x: i32,
    y: i32,
    kind: MdListKind,
    color: u32,
) {
    match kind {
        MdListKind::Bullet { indent } => {
            let glyph = match indent % 3 {
                0 => "•",
                1 => "◦",
                _ => "▪",
            };
            canvas.draw_text_styled(x + 2, y + 1, glyph, t.accent_blue, 14, FontStyle::ChatBodyBold);
        }
        MdListKind::Ordered { number, .. } => {
            let lbl = format!("{number}.");
            canvas.draw_text_styled(x, y + 1, &lbl, t.syn_number, 32, FontStyle::ChatBodyBold);
        }
        MdListKind::Task { checked, .. } => {
            let box_rect = Rect::new(x + 1, y + 5, 13, 13);
            if checked {
                canvas.fill_rounded_rect(box_rect, 3, t.run_green);
                canvas.draw_check_icon(box_rect.x + 1, box_rect.y + 1, 0xFFFFFF);
            } else {
                canvas.stroke_rounded_rect(box_rect, 3, t.border_color, t.editor_bg);
            }
            let _ = color;
        }
    }
}

pub(crate) fn list_marker_width(canvas: &SurfaceCanvas, kind: MdListKind) -> i32 {
    match kind {
        MdListKind::Bullet { .. } => 16,
        MdListKind::Task { .. } => 20,
        MdListKind::Ordered { number, .. } => {
            let lbl = format!("{number}. ");
            canvas.measure_text(&lbl, FontStyle::ChatBodyBold).0.max(20)
        }
    }
}

// =============================================================================
// Central Editor Formatted Markdown Preview Cache & Renderer
// =============================================================================

#[derive(Debug, Clone)]
pub(crate) enum MdPreviewRow {
    Spacer {
        height: i32,
    },
    Heading {
        level: u8,
        text: String,
        spans: Vec<MdSpan>,
        has_bottom_rule: bool,
    },
    ProseLine {
        text: String,
        spans: Vec<MdSpan>,
    },
    ListItemLine {
        kind: MdListKind,
        text: String,
        spans: Vec<MdSpan>,
        is_continuation: bool,
        marker_w: i32,
    },
    BlockquoteLine {
        depth: usize,
        callout: Option<MdCalloutKind>,
        is_callout_header: bool,
        text: String,
        spans: Vec<MdSpan>,
    },
    HorizontalRule,
    CodeBlockHeader {
        lang: String,
        code: Arc<str>,
        line_count: usize,
    },
    CodeBlockLine {
        spans: Arc<Vec<ide_syntax::HighlightSpan>>,
    },
    TableRow(PreparedTableRow),
}

impl MdPreviewRow {
    pub(crate) fn row_height(&self) -> i32 {
        match self {
            Self::Spacer { height } => *height,
            Self::Heading { level, .. } => heading_row_height(*level) + 4,
            Self::ProseLine { .. } | Self::ListItemLine { .. } => 24,
            Self::BlockquoteLine { .. } => 23,
            Self::HorizontalRule => 18,
            Self::CodeBlockHeader { .. } => 28,
            Self::CodeBlockLine { .. } => 20,
            Self::TableRow(tr) => table_row_height(tr.line_count),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MarkdownPreviewCache {
    pub path: PathBuf,
    pub revision: usize,
    pub wrap_w: i32,
    pub scale_bits: u32,
    pub rows: Vec<MdPreviewRow>,
    pub row_tops: Vec<i32>,
    pub total_height: i32,
}

impl MarkdownPreviewCache {
    pub(crate) fn ensure_built(
        &mut self,
        canvas: &SurfaceCanvas,
        workspace_root: &Path,
        path: &Path,
        revision: usize,
        markdown_text: &str,
        wrap_w: i32,
        scale_bits: u32,
    ) {
        if self.path == path
            && self.revision == revision
            && self.wrap_w == wrap_w
            && self.scale_bits == scale_bits
        {
            return;
        }
        let base_dir = path.parent();
        let blocks = parse_markdown_blocks(workspace_root, base_dir, markdown_text);
        let mut rows = Vec::with_capacity(blocks.len() * 2);

        for block in &blocks {
            match block {
                MdBlock::Blank => {
                    if !rows.is_empty() {
                        rows.push(MdPreviewRow::Spacer { height: 10 });
                    }
                }
                MdBlock::Heading { level, content } => {
                    if !rows.is_empty() {
                        rows.push(MdPreviewRow::Spacer {
                            height: if *level <= 2 { 10 } else { 6 },
                        });
                    }
                    let style = heading_font_style(*level);
                    let eff_w = (wrap_w - content.pill_reserve).max(80);
                    let segs = wrap_md_line_segments(canvas, &content.text, eff_w, style);
                    let last_idx = segs.len().saturating_sub(1);
                    for (idx, (s, e, seg)) in segs.into_iter().enumerate() {
                        rows.push(MdPreviewRow::Heading {
                            level: *level,
                            text: seg,
                            spans: project_md_spans_slice(s, e, &content.spans),
                            has_bottom_rule: *level <= 2 && idx == last_idx,
                        });
                    }
                }
                MdBlock::ParagraphLine { content } => {
                    let eff_w = (wrap_w - content.pill_reserve).max(80);
                    for (s, e, seg) in
                        wrap_md_line_segments(canvas, &content.text, eff_w, FontStyle::ChatBody)
                    {
                        rows.push(MdPreviewRow::ProseLine {
                            text: seg,
                            spans: project_md_spans_slice(s, e, &content.spans),
                        });
                    }
                }
                MdBlock::ListItem { kind, content } => {
                    let indent_px = (kind.indent() as i32) * 18;
                    let marker_w = list_marker_width(canvas, *kind);
                    let eff_w = (wrap_w - indent_px - marker_w - content.pill_reserve).max(80);
                    for (idx, (s, e, seg)) in
                        wrap_md_line_segments(canvas, &content.text, eff_w, FontStyle::ChatBody)
                            .into_iter()
                            .enumerate()
                    {
                        rows.push(MdPreviewRow::ListItemLine {
                            kind: *kind,
                            text: seg,
                            spans: project_md_spans_slice(s, e, &content.spans),
                            is_continuation: idx > 0,
                            marker_w,
                        });
                    }
                }
                MdBlock::BlockquoteLine {
                    depth,
                    callout,
                    is_callout_header,
                    content,
                } => {
                    let indent_px = (*depth as i32) * 14 + 12;
                    let eff_w = (wrap_w - indent_px - content.pill_reserve).max(80);
                    if *is_callout_header {
                        rows.push(MdPreviewRow::BlockquoteLine {
                            depth: *depth,
                            callout: *callout,
                            is_callout_header: true,
                            text: String::new(),
                            spans: Vec::new(),
                        });
                    }
                    if !content.text.is_empty() || !*is_callout_header {
                        for (s, e, seg) in
                            wrap_md_line_segments(canvas, &content.text, eff_w, FontStyle::ChatBody)
                        {
                            rows.push(MdPreviewRow::BlockquoteLine {
                                depth: *depth,
                                callout: *callout,
                                is_callout_header: false,
                                text: seg,
                                spans: project_md_spans_slice(s, e, &content.spans),
                            });
                        }
                    }
                }
                MdBlock::HorizontalRule => {
                    rows.push(MdPreviewRow::HorizontalRule);
                }
                MdBlock::CodeBlock {
                    lang_tag,
                    lang_id,
                    full_code,
                    lines,
                    line_count,
                    ..
                } => {
                    rows.push(MdPreviewRow::CodeBlockHeader {
                        lang: lang_tag.clone(),
                        code: Arc::clone(full_code),
                        line_count: *line_count,
                    });
                    let code_w = (wrap_w - 24).max(80);
                    let mut hl_state = HighlightState::default();
                    for cl in lines {
                        for (_, _, wl) in wrap_md_line_segments(canvas, cl, code_w, FontStyle::ChatCode)
                        {
                            rows.push(MdPreviewRow::CodeBlockLine {
                                spans: Arc::new(highlight_line_with_state(&wl, *lang_id, &mut hl_state)),
                            });
                        }
                    }
                    rows.push(MdPreviewRow::Spacer { height: 6 });
                }
                MdBlock::Table {
                    alignments,
                    headers,
                    rows: tbl_rows,
                } => {
                    let prepared = layout_table_rows(canvas, alignments, headers, tbl_rows, wrap_w);
                    for pr in prepared {
                        rows.push(MdPreviewRow::TableRow(pr));
                    }
                    rows.push(MdPreviewRow::Spacer { height: 6 });
                }
            }
        }

        self.row_tops.clear();
        self.row_tops.reserve(rows.len());
        let mut y = 0i32;
        for r in &rows {
            self.row_tops.push(y);
            y += r.row_height();
        }
        self.path = path.to_path_buf();
        self.revision = revision;
        self.wrap_w = wrap_w;
        self.scale_bits = scale_bits;
        self.rows = rows;
        self.total_height = y;
    }

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

    pub(crate) fn visible_rows_from(&self, start_row: usize, viewport_h: i32) -> usize {
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_markdown_preview_body(
    canvas: &mut SurfaceCanvas,
    t: &IdeaTheme,
    ui_lang: UiLanguage,
    hover_pos: (i32, i32),
    hit_regions: &mut Vec<(Rect, HitAction)>,
    cache: &MarkdownPreviewCache,
    preview_rect: Rect,
    scroll_row: &mut usize,
) -> (usize, usize, Rect) {
    let content_pad_x = if preview_rect.w >= 760 { 28 } else { 16 };
    let content_x = preview_rect.x + content_pad_x;
    let content_w = (preview_rect.w - content_pad_x * 2 - 12).max(120);
    let view_h = (preview_rect.h - 12).max(24);
    let max_s = cache.max_scroll_row_for_height(view_h);
    if *scroll_row > max_s {
        *scroll_row = max_s;
    }
    let vis_count = cache.visible_rows_from(*scroll_row, view_h);
    let prose_fg = if t.kind == ThemeKind::IslandsLight {
        t.text_primary
    } else {
        blend_rgb(t.text_primary, 0xFFFFFF, 0.14)
    };
    let code_block_bg = if t.kind == ThemeKind::IslandsLight {
        t.panel_bg
    } else {
        blend_rgb(t.window_bg, t.editor_bg, 0.45)
    };
    let (hx, hy) = hover_pos;
    let bottom = preview_rect.y + preview_rect.h - 6;
    let mut cy = preview_rect.y + 8;

    for row in cache.rows.iter().skip(*scroll_row).take(vis_count + 1) {
        let rh = row.row_height();
        if cy + rh > bottom {
            break;
        }
        let row_rect = Rect::new(content_x, cy, content_w, rh);
        match row {
            MdPreviewRow::Spacer { .. } => {}
            MdPreviewRow::Heading {
                level,
                text,
                spans,
                has_bottom_rule,
            } => {
                let style = heading_font_style(*level);
                let color = match level {
                    1 => t.text_primary,
                    2 => blend_rgb(t.text_primary, t.accent_blue, 0.18),
                    3 => blend_rgb(t.text_primary, t.syn_function, 0.18),
                    _ => t.text_secondary,
                };
                draw_md_spans_text(
                    canvas,
                    t,
                    hover_pos,
                    hit_regions,
                    row_rect,
                    row_rect.x,
                    cy + 3,
                    text,
                    spans,
                    color,
                    row_rect.w,
                    style,
                    MdAlign::Left,
                );
                if *has_bottom_rule {
                    canvas.fill_rect(
                        Rect::new(
                            row_rect.x,
                            cy + rh - 3,
                            row_rect.w,
                            1,
                        ),
                        blend_rgb(t.border_color, t.editor_bg, 0.35),
                    );
                }
            }
            MdPreviewRow::ProseLine { text, spans } => {
                draw_md_spans_text(
                    canvas,
                    t,
                    hover_pos,
                    hit_regions,
                    row_rect,
                    row_rect.x,
                    cy + 2,
                    text,
                    spans,
                    prose_fg,
                    row_rect.w,
                    FontStyle::ChatBody,
                    MdAlign::Left,
                );
            }
            MdPreviewRow::ListItemLine {
                kind,
                text,
                spans,
                is_continuation,
                marker_w,
            } => {
                let indent_px = (kind.indent() as i32) * 18;
                if !*is_continuation {
                    draw_list_marker(canvas, t, row_rect.x + indent_px, cy + 2, *kind, prose_fg);
                }
                let tx = row_rect.x + indent_px + *marker_w;
                draw_md_spans_text(
                    canvas,
                    t,
                    hover_pos,
                    hit_regions,
                    row_rect,
                    tx,
                    cy + 2,
                    text,
                    spans,
                    prose_fg,
                    (row_rect.x + row_rect.w - tx).max(40),
                    FontStyle::ChatBody,
                    MdAlign::Left,
                );
            }
            MdPreviewRow::BlockquoteLine {
                depth,
                callout,
                is_callout_header,
                text,
                spans,
            } => {
                let accent = callout
                    .as_ref()
                    .map(|c| c.color(t))
                    .unwrap_or_else(|| blend_rgb(t.accent_blue, t.border_color, 0.35));
                let bg = blend_rgb(t.editor_bg, accent, if callout.is_some() { 0.08 } else { 0.04 });
                canvas.fill_rect(row_rect, bg);
                for d in 0..(*depth).max(1) {
                    canvas.fill_rect(
                        Rect::new(row_rect.x + 2 + (d as i32) * 10, cy, 3, rh),
                        accent,
                    );
                }
                let tx = row_rect.x + (*depth as i32) * 10 + 12;
                if *is_callout_header {
                    if let Some(c) = callout {
                        canvas.draw_text_styled(
                            tx,
                            cy + 3,
                            c.label(ui_lang),
                            accent,
                            (row_rect.x + row_rect.w - tx).max(40),
                            FontStyle::ChatMetaBold,
                        );
                    }
                } else {
                    draw_md_spans_text(
                        canvas,
                        t,
                        hover_pos,
                        hit_regions,
                        row_rect,
                        tx,
                        cy + 2,
                        text,
                        spans,
                        t.text_secondary,
                        (row_rect.x + row_rect.w - tx).max(40),
                        FontStyle::ChatBody,
                        MdAlign::Left,
                    );
                }
            }
            MdPreviewRow::HorizontalRule => {
                canvas.fill_rect(
                    Rect::new(row_rect.x, cy + rh / 2, row_rect.w, 1),
                    blend_rgb(t.border_color, t.editor_bg, 0.25),
                );
            }
            MdPreviewRow::CodeBlockHeader {
                lang,
                code,
                line_count,
            } => {
                let hdr = Rect::new(row_rect.x, cy + 2, row_rect.w, rh - 2);
                canvas.stroke_rounded_rect(
                    hdr,
                    6,
                    blend_rgb(t.border_color, t.panel_bg, 0.4),
                    blend_rgb(code_block_bg, t.panel_bg, 0.35),
                );
                let lines_lbl = match ui_lang {
                    UiLanguage::ZhCn => "行",
                    UiLanguage::EnUs => "lines",
                };
                canvas.draw_text_styled(
                    hdr.x + 10,
                    hdr.y + 5,
                    &format!("{lang} · {line_count} {lines_lbl}"),
                    t.text_secondary,
                    (hdr.w - 76).max(32),
                    FontStyle::ChatCodeBold,
                );
                let copy_btn = Rect::new(hdr.x + hdr.w - 60, hdr.y + 4, 54, 18);
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
                    match ui_lang {
                        UiLanguage::ZhCn => "复制",
                        UiLanguage::EnUs => "Copy",
                    },
                    if chov { t.text_primary } else { t.text_muted },
                    FontStyle::ChatMeta,
                );
                hit_regions.push((copy_btn, HitAction::CopyAgentCodeBlock(code.clone())));
            }
            MdPreviewRow::CodeBlockLine { spans } => {
                let brd = blend_rgb(t.border_color, t.panel_bg, 0.4);
                canvas.fill_rect(row_rect, code_block_bg);
                canvas.fill_rect(Rect::new(row_rect.x, row_rect.y, 1, rh), brd);
                canvas.fill_rect(
                    Rect::new(row_rect.x + row_rect.w - 1, row_rect.y, 1, rh),
                    brd,
                );
                let mut cur_x = row_rect.x + 12;
                let right_edge = row_rect.x + row_rect.w - 10;
                for span in spans.iter() {
                    if cur_x >= right_edge {
                        break;
                    }
                    let slice = span.text.as_str();
                    if slice.is_empty() {
                        continue;
                    }
                    let fst = if span.kind == TokenKind::Keyword {
                        FontStyle::ChatCodeBold
                    } else {
                        FontStyle::ChatCode
                    };
                    cur_x = canvas.draw_text_styled(
                        cur_x,
                        cy + 2,
                        slice,
                        span.kind.color(t),
                        right_edge - cur_x,
                        fst,
                    );
                }
            }
            MdPreviewRow::TableRow(tr) => {
                render_prepared_table_row(
                    canvas,
                    t,
                    hover_pos,
                    hit_regions,
                    row_rect,
                    tr,
                    prose_fg,
                );
            }
        }
        cy += rh;
    }

    let track = Rect::new(
        preview_rect.x + preview_rect.w - 9,
        preview_rect.y + 4,
        6,
        (preview_rect.h - 8).max(16),
    );
    let scroll_px = cache
        .row_tops
        .get(*scroll_row)
        .copied()
        .unwrap_or(0)
        .max(0) as usize;
    let max_scroll_px = cache
        .row_tops
        .get(max_s)
        .copied()
        .unwrap_or(0)
        .max(0) as usize;
    let vis_px = view_h.max(1) as usize;
    let eff_total_px = if max_s > 0 {
        max_scroll_px.saturating_add(vis_px)
    } else {
        cache.total_height.max(0) as usize
    };
    canvas.draw_vertical_scrollbar(
        track,
        scroll_px,
        vis_px,
        eff_total_px,
        blend_rgb(t.border_color, t.editor_bg, 0.25),
    );
    (vis_count, cache.rows.len(), track)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_parser_handles_bold_italic_strike_code_links_and_snake_case() {
        let (clean, spans) = parse_markdown_inline(
            "Hello **bold** *italic* ***both*** ~~old~~ `my_var` snake_case_id [Docs](https://example.com) and \\*escaped\\*",
        );
        assert!(clean.contains("bold italic both old my_var snake_case_id Docs and *escaped*"));
        assert!(!clean.contains("https://example.com"));
        assert!(spans.iter().any(|s| s.kind == MdSpanKind::Bold));
        assert!(spans.iter().any(|s| s.kind == MdSpanKind::Italic));
        assert!(spans.iter().any(|s| s.kind == MdSpanKind::BoldItalic));
        assert!(spans.iter().any(|s| s.kind == MdSpanKind::Strikethrough));
        assert!(spans.iter().any(|s| s.kind == MdSpanKind::InlineCode));
        assert!(spans
            .iter()
            .any(|s| matches!(&s.kind, MdSpanKind::Link { target } if target == "https://example.com")));
    }

    #[test]
    fn block_parser_handles_headings_tables_quotes_lists_hr_and_fences() {
        let md = "\
# Main Title
## Sub Title

> [!NOTE]
> Callout body with **emphasis**

- [x] Done task
- [ ] Pending task
1. First item

| Name | Score |
| :--- | ---: |
| Alice | 95 |
| Bob | 88 |

---

~~~rust
fn demo() {}
~~~
";
        let blocks = parse_markdown_blocks(Path::new("."), None, md);
        assert!(blocks
            .iter()
            .any(|b| matches!(b, MdBlock::Heading { level: 1, content } if content.text == "Main Title")));
        assert!(blocks
            .iter()
            .any(|b| matches!(b, MdBlock::Heading { level: 2, content } if content.text == "Sub Title")));
        assert!(blocks.iter().any(|b| matches!(
            b,
            MdBlock::BlockquoteLine {
                callout: Some(MdCalloutKind::Note),
                is_callout_header: true,
                ..
            }
        )));
        assert!(blocks.iter().any(|b| matches!(
            b,
            MdBlock::ListItem {
                kind: MdListKind::Task { checked: true, .. },
                ..
            }
        )));
        assert!(blocks.iter().any(|b| matches!(
            b,
            MdBlock::ListItem {
                kind: MdListKind::Ordered { number: 1, .. },
                ..
            }
        )));
        assert!(blocks.iter().any(|b| matches!(
            b,
            MdBlock::Table { alignments, headers, rows }
                if alignments == &[MdAlign::Left, MdAlign::Right]
                && headers.len() == 2
                && rows.len() == 2
        )));
        assert!(blocks.iter().any(|b| matches!(b, MdBlock::HorizontalRule)));
        assert!(blocks.iter().any(|b| matches!(
            b,
            MdBlock::CodeBlock { lang_id: LanguageId::Rust, .. }
        )));
    }
}

