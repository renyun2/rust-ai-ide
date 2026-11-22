use ide_core::{IdeaTheme, LanguageId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenKind {
    Plain,
    Keyword,
    Function,
    Type,
    String,
    Number,
    Comment,
    MacroOrDecorator,
    FieldOrProperty,
}

impl TokenKind {
    pub fn color(&self, theme: &IdeaTheme) -> u32 {
        match self {
            Self::Plain => theme.text_primary,
            Self::Keyword => theme.syn_keyword,
            Self::Function => theme.syn_function,
            Self::Type => theme.syn_type,
            Self::String => theme.syn_string,
            Self::Number => theme.syn_number,
            Self::Comment => theme.syn_comment,
            Self::MacroOrDecorator => theme.syn_macro_or_decorator,
            Self::FieldOrProperty => theme.syn_field_or_property,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightSpan {
    pub text: String,
    pub kind: TokenKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Class,
    Interface,
    Constant,
}

impl SymbolKind {
    pub fn icon_badge(&self) -> &'static str {
        match self {
            Self::Function => "ƒ",
            Self::Struct => "S",
            Self::Enum => "E",
            Self::Trait => "T",
            Self::Impl => "I",
            Self::Class => "C",
            Self::Interface => "I",
            Self::Constant => "K",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructureSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnableGutterEntry {
    pub line: usize,
    pub label: String,
    pub command: String,
    pub args: Vec<String>,
}

const RUST_KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "pub", "struct", "enum", "impl", "trait", "use", "mod", "match", "if",
    "else", "for", "while", "loop", "return", "async", "await", "where", "const", "static", "type",
    "Self", "self", "super", "crate", "true", "false", "in", "as", "ref", "move", "unsafe", "dyn",
];

const TS_JS_KEYWORDS: &[&str] = &[
    "function", "const", "let", "var", "interface", "type", "class", "extends", "implements",
    "import", "export", "from", "return", "if", "else", "for", "while", "switch", "case", "async",
    "await", "new", "try", "catch", "finally", "true", "false", "null", "undefined", "public",
    "private", "readonly",
];

const PYTHON_KEYWORDS: &[&str] = &[
    "def", "class", "import", "from", "as", "return", "if", "elif", "else", "for", "while", "in",
    "try", "except", "finally", "with", "async", "await", "yield", "lambda", "pass", "break",
    "continue", "True", "False", "None", "and", "or", "not", "is", "self",
];

const JAVA_KEYWORDS: &[&str] = &[
    "abstract", "assert", "boolean", "break", "byte", "case", "catch", "char", "class", "const",
    "continue", "default", "do", "double", "else", "enum", "extends", "final", "finally", "float",
    "for", "goto", "if", "implements", "import", "instanceof", "int", "interface", "long",
    "native", "new", "package", "private", "protected", "public", "record", "return", "sealed",
    "non-sealed", "permits", "short", "static", "strictfp", "super", "switch", "synchronized",
    "this", "throw", "throws", "transient", "try", "var", "void", "volatile", "while", "yield",
    "true", "false", "null",
];

/// 对单行代码执行针对 Rust / TypeScript / JavaScript / Python / Java 的 IDEA Darcula 风格语法高亮切分
pub fn highlight_line(line: &str, lang: LanguageId) -> Vec<HighlightSpan> {
    let trimmed = line.trim_start();
    if (matches!(
        lang,
        LanguageId::Rust | LanguageId::TypeScript | LanguageId::JavaScript | LanguageId::Java
    ) && (trimmed.starts_with("//")
        || (lang == LanguageId::Java
            && (trimmed.starts_with("/**")
                || trimmed.starts_with("/*")
                || trimmed.starts_with("*/")
                || trimmed == "*"
                || trimmed.starts_with("* ")))))
        || (matches!(lang, LanguageId::Python | LanguageId::Toml) && trimmed.starts_with('#'))
    {
        return vec![HighlightSpan {
            text: line.to_string(),
            kind: TokenKind::Comment,
        }];
    }

    if lang == LanguageId::Rust && (trimmed.starts_with("#[") || trimmed.starts_with("#![")) {
        return vec![HighlightSpan {
            text: line.to_string(),
            kind: TokenKind::MacroOrDecorator,
        }];
    }

    if matches!(lang, LanguageId::Python | LanguageId::TypeScript) && trimmed.starts_with('@') {
        return vec![HighlightSpan {
            text: line.to_string(),
            kind: TokenKind::MacroOrDecorator,
        }];
    }

    let keywords = match lang {
        LanguageId::Rust => RUST_KEYWORDS,
        LanguageId::TypeScript | LanguageId::JavaScript => TS_JS_KEYWORDS,
        LanguageId::Python => PYTHON_KEYWORDS,
        LanguageId::Java => JAVA_KEYWORDS,
        _ => &[],
    };

    let mut spans = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // 行尾注释检测
        if c == '/'
            && i + 1 < chars.len()
            && chars[i + 1] == '/'
            && matches!(
                lang,
                LanguageId::Rust | LanguageId::TypeScript | LanguageId::JavaScript | LanguageId::Java
            )
        {
            let comment: String = chars[i..].iter().collect();
            spans.push(HighlightSpan {
                text: comment,
                kind: TokenKind::Comment,
            });
            break;
        }
        // 行内块注释 /* ... */
        if c == '/'
            && i + 1 < chars.len()
            && chars[i + 1] == '*'
            && matches!(
                lang,
                LanguageId::Rust | LanguageId::TypeScript | LanguageId::JavaScript | LanguageId::Java
            )
        {
            let start = i;
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '/' {
                i += 2;
            } else {
                i = chars.len();
            }
            let comment: String = chars[start..i].iter().collect();
            spans.push(HighlightSpan {
                text: comment,
                kind: TokenKind::Comment,
            });
            continue;
        }
        if c == '#' && matches!(lang, LanguageId::Python | LanguageId::Toml) {
            let comment: String = chars[i..].iter().collect();
            spans.push(HighlightSpan {
                text: comment,
                kind: TokenKind::Comment,
            });
            break;
        }

        // Java 注解 (@Override, @SpringBootApplication, @GetMapping("/api"), ...)
        if c == '@'
            && matches!(lang, LanguageId::Java | LanguageId::TypeScript | LanguageId::Python)
            && i + 1 < chars.len()
            && (chars[i + 1].is_ascii_alphabetic() || chars[i + 1] == '_')
        {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let anno: String = chars[start..i].iter().collect();
            spans.push(HighlightSpan {
                text: anno,
                kind: TokenKind::MacroOrDecorator,
            });
            continue;
        }

        // 字符串字面量
        if c == '"' || (c == '\'' && lang != LanguageId::Rust) {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let s: String = chars[start..i.min(chars.len())].iter().collect();
            spans.push(HighlightSpan {
                text: s,
                kind: TokenKind::String,
            });
            continue;
        }

        // 标识符或关键字
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            if lang == LanguageId::Rust && i < chars.len() && chars[i] == '!' {
                i += 1;
                let mac: String = chars[start..i].iter().collect();
                spans.push(HighlightSpan {
                    text: mac,
                    kind: TokenKind::MacroOrDecorator,
                });
                continue;
            }

            let word: String = chars[start..i].iter().collect();
            let kind = if keywords.contains(&word.as_str()) {
                TokenKind::Keyword
            } else if word
                .chars()
                .next()
                .map(|ch| ch.is_ascii_uppercase())
                .unwrap_or(false)
            {
                TokenKind::Type
            } else if i < chars.len() && chars[i] == '(' {
                TokenKind::Function
            } else {
                TokenKind::Plain
            };
            spans.push(HighlightSpan { text: word, kind });
            continue;
        }

        // 数字字面量
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_')
            {
                i += 1;
            }
            let num: String = chars[start..i].iter().collect();
            spans.push(HighlightSpan {
                text: num,
                kind: TokenKind::Number,
            });
            continue;
        }

        // 其他符号与空白合并
        let start = i;
        i += 1;
        while i < chars.len()
            && !chars[i].is_ascii_alphanumeric()
            && chars[i] != '_'
            && chars[i] != '"'
            && chars[i] != '\''
            && chars[i] != '/'
            && chars[i] != '#'
            && chars[i] != '@'
        {
            i += 1;
        }
        let plain: String = chars[start..i].iter().collect();
        spans.push(HighlightSpan {
            text: plain,
            kind: TokenKind::Plain,
        });
    }

    spans
}

/// 提取文件的 Structure 符号大纲（用于 IDEA 左侧 Structure 面板及顶部 Breadcrumbs 面包屑导航）
pub fn extract_structure_symbols(source: &str, lang: LanguageId) -> Vec<StructureSymbol> {
    let mut symbols = Vec::new();

    for (line_idx, raw_line) in source.lines().enumerate() {
        let line = raw_line.trim();
        match lang {
            LanguageId::Rust => {
                if let Some(rest) = line
                    .strip_prefix("pub fn ")
                    .or_else(|| line.strip_prefix("pub(crate) fn "))
                    .or_else(|| line.strip_prefix("pub(super) fn "))
                    .or_else(|| line.strip_prefix("pub async fn "))
                    .or_else(|| line.strip_prefix("pub(crate) async fn "))
                    .or_else(|| line.strip_prefix("pub const fn "))
                    .or_else(|| line.strip_prefix("const fn "))
                    .or_else(|| line.strip_prefix("async fn "))
                    .or_else(|| line.strip_prefix("fn "))
                {
                    if let Some(name) = rest.split(['(', '<', ' ']).next() {
                        if !name.trim().is_empty() {
                            symbols.push(StructureSymbol {
                                name: name.trim().to_string(),
                                kind: SymbolKind::Function,
                                line: line_idx,
                            });
                        }
                    }
                } else if let Some(rest) = line
                    .strip_prefix("pub struct ")
                    .or_else(|| line.strip_prefix("pub(crate) struct "))
                    .or_else(|| line.strip_prefix("struct "))
                {
                    if let Some(name) = rest.split(['{', '(', '<', ';', ' ']).next() {
                        if !name.trim().is_empty() {
                            symbols.push(StructureSymbol {
                                name: name.trim().to_string(),
                                kind: SymbolKind::Struct,
                                line: line_idx,
                            });
                        }
                    }
                } else if let Some(rest) = line
                    .strip_prefix("pub enum ")
                    .or_else(|| line.strip_prefix("pub(crate) enum "))
                    .or_else(|| line.strip_prefix("enum "))
                {
                    if let Some(name) = rest.split(['{', '<', ' ']).next() {
                        if !name.trim().is_empty() {
                            symbols.push(StructureSymbol {
                                name: name.trim().to_string(),
                                kind: SymbolKind::Enum,
                                line: line_idx,
                            });
                        }
                    }
                } else if let Some(rest) = line
                    .strip_prefix("pub trait ")
                    .or_else(|| line.strip_prefix("pub(crate) trait "))
                    .or_else(|| line.strip_prefix("trait "))
                {
                    if let Some(name) = rest.split(['{', '<', ':', ' ']).next() {
                        if !name.trim().is_empty() {
                            symbols.push(StructureSymbol {
                                name: name.trim().to_string(),
                                kind: SymbolKind::Trait,
                                line: line_idx,
                            });
                        }
                    }
                } else if let Some(rest) = line
                    .strip_prefix("pub const ")
                    .or_else(|| line.strip_prefix("pub(crate) const "))
                    .or_else(|| line.strip_prefix("const "))
                    .or_else(|| line.strip_prefix("pub static "))
                    .or_else(|| line.strip_prefix("static "))
                {
                    if let Some(name) = rest.split([':', '=', ' ']).next() {
                        let n = name.trim();
                        if !n.is_empty() && n != "fn" && n != "mut" {
                            symbols.push(StructureSymbol {
                                name: n.to_string(),
                                kind: SymbolKind::Constant,
                                line: line_idx,
                            });
                        }
                    }
                } else if let Some(rest) = line.strip_prefix("impl") {
                    if rest.starts_with(' ') || rest.starts_with('<') {
                        let title = rest.trim_end_matches('{').trim();
                        if !title.is_empty() {
                            symbols.push(StructureSymbol {
                                name: format!("impl {title}"),
                                kind: SymbolKind::Impl,
                                line: line_idx,
                            });
                        }
                    }
                }
            }
            LanguageId::TypeScript | LanguageId::JavaScript => {
                if let Some(rest) = line
                    .strip_prefix("export function ")
                    .or_else(|| line.strip_prefix("export default function "))
                    .or_else(|| line.strip_prefix("export async function "))
                    .or_else(|| line.strip_prefix("async function "))
                    .or_else(|| line.strip_prefix("function "))
                {
                    if let Some(name) = rest.split(['(', '<', ' ']).next() {
                        if !name.trim().is_empty() {
                            symbols.push(StructureSymbol {
                                name: name.trim().to_string(),
                                kind: SymbolKind::Function,
                                line: line_idx,
                            });
                        }
                    }
                } else if let Some(rest) = line
                    .strip_prefix("export class ")
                    .or_else(|| line.strip_prefix("export default class "))
                    .or_else(|| line.strip_prefix("class "))
                {
                    if let Some(name) = rest.split(['{', ' ', '<']).next() {
                        if !name.trim().is_empty() {
                            symbols.push(StructureSymbol {
                                name: name.trim().to_string(),
                                kind: SymbolKind::Class,
                                line: line_idx,
                            });
                        }
                    }
                } else if let Some(rest) = line
                    .strip_prefix("export interface ")
                    .or_else(|| line.strip_prefix("interface "))
                    .or_else(|| line.strip_prefix("export type "))
                    .or_else(|| line.strip_prefix("type "))
                {
                    if let Some(name) = rest.split(['{', ' ', '<', '=']).next() {
                        if !name.trim().is_empty() {
                            symbols.push(StructureSymbol {
                                name: name.trim().to_string(),
                                kind: SymbolKind::Interface,
                                line: line_idx,
                            });
                        }
                    }
                }
            }
            LanguageId::Python => {
                if let Some(rest) = line
                    .strip_prefix("def ")
                    .or_else(|| line.strip_prefix("async def "))
                {
                    if let Some(name) = rest.split('(').next() {
                        if !name.trim().is_empty() {
                            symbols.push(StructureSymbol {
                                name: name.trim().to_string(),
                                kind: SymbolKind::Function,
                                line: line_idx,
                            });
                        }
                    }
                } else if let Some(rest) = line.strip_prefix("class ") {
                    if let Some(name) = rest.split(['(', ':']).next() {
                        if !name.trim().is_empty() {
                            symbols.push(StructureSymbol {
                                name: name.trim().to_string(),
                                kind: SymbolKind::Class,
                                line: line_idx,
                            });
                        }
                    }
                }
            }
            LanguageId::Java => {
                if let Some(sym) = extract_java_structure_symbol(line, line_idx) {
                    symbols.push(sym);
                }
            }
            _ => {}
        }
    }

    symbols
}

/// 在源码中精确定位给定符号名 `symbol` 的声明/定义位置，返回 0-based `(line, col)`
pub fn find_symbol_definition(
    source: &str,
    lang: LanguageId,
    symbol: &str,
) -> Option<(usize, usize)> {
    let target = symbol.trim();
    if target.is_empty() {
        return None;
    }

    // 1. 优先匹配顶层与成员结构符号（fn / struct / enum / trait / const / class / interface / def）
    for sym in extract_structure_symbols(source, lang) {
        if sym.name == target {
            let col = source
                .lines()
                .nth(sym.line)
                .and_then(|l| l.find(target))
                .unwrap_or(0);
            return Some((sym.line, col));
        }
    }

    // 2. 匹配 type alias、mod、局部变量绑定 (let / let mut / const / var / 参数或赋值定义)
    let prefixes: &[&str] = match lang {
        LanguageId::Rust => &[
            "pub type ",
            "pub(crate) type ",
            "type ",
            "pub mod ",
            "mod ",
            "let mut ",
            "let ",
            "for ",
        ],
        LanguageId::TypeScript | LanguageId::JavaScript => &[
            "export const ",
            "export let ",
            "const ",
            "let ",
            "var ",
        ],
        LanguageId::Python => &[""],
        LanguageId::Java => &[""],
        _ => &[],
    };

    for (line_idx, raw_line) in source.lines().enumerate() {
        let trimmed = raw_line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('#') {
            continue;
        }
        if lang == LanguageId::Python {
            if let Some(rest) = trimmed.strip_prefix(target) {
                let after = rest.trim_start();
                if after.starts_with('=') && !after.starts_with("==") {
                    let col = raw_line.find(target).unwrap_or(0);
                    return Some((line_idx, col));
                }
            }
            continue;
        }
        if lang == LanguageId::Java {
            if let Some((lhs, _)) = trimmed.split_once(['=', ';']) {
                let lhs_clean = lhs.trim();
                let mut tokens = lhs_clean.split_whitespace();
                let last_tok = tokens.next_back().unwrap_or("");
                let has_type_prefix = tokens.next().is_some();
                if has_type_prefix && last_tok == target {
                    let col = raw_line.find(target).unwrap_or(0);
                    return Some((line_idx, col));
                }
            }
            continue;
        }
        for prefix in prefixes {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let ident: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if ident == target {
                    let col = raw_line.find(target).unwrap_or(0);
                    return Some((line_idx, col));
                }
            }
        }
    }

    None
}

/// 识别源码中的可直接运行入口（IDEA 编辑器行号旁的绿色三角 ▶ 按钮）
pub fn detect_runnable_gutter_entries(source: &str, lang: LanguageId) -> Vec<RunnableGutterEntry> {
    let mut entries = Vec::new();
    let lines: Vec<&str> = source.lines().collect();

    for (idx, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim();
        match lang {
            LanguageId::Rust => {
                if trimmed.starts_with("fn main(") || trimmed.starts_with("pub fn main(") {
                    entries.push(RunnableGutterEntry {
                        line: idx,
                        label: "Run 'main'".to_string(),
                        command: "cargo".to_string(),
                        args: vec!["run".to_string()],
                    });
                } else if trimmed == "#[test]" || trimmed == "#[tokio::test]" {
                    // 向下寻找紧邻的测试函数名
                    let test_fn_line = (idx + 1..=(idx + 3).min(lines.len().saturating_sub(1)))
                        .find(|&l| lines[l].contains("fn "))
                        .unwrap_or(idx);
                    entries.push(RunnableGutterEntry {
                        line: test_fn_line,
                        label: "Run Cargo Test".to_string(),
                        command: "cargo".to_string(),
                        args: vec!["test".to_string()],
                    });
                }
            }
            LanguageId::Python => {
                if trimmed.starts_with("if __name__") && trimmed.contains("__main__") {
                    entries.push(RunnableGutterEntry {
                        line: idx,
                        label: "Run Python Script".to_string(),
                        command: "python".to_string(),
                        args: vec![],
                    });
                } else if trimmed.starts_with("def test_") {
                    entries.push(RunnableGutterEntry {
                        line: idx,
                        label: "Run Pytest".to_string(),
                        command: "pytest".to_string(),
                        args: vec![],
                    });
                }
            }
            LanguageId::TypeScript | LanguageId::JavaScript => {
                if trimmed.starts_with("describe(") || trimmed.starts_with("test(") {
                    entries.push(RunnableGutterEntry {
                        line: idx,
                        label: "Run JS/TS Test".to_string(),
                        command: "npm".to_string(),
                        args: vec!["test".to_string()],
                    });
                }
            }
            LanguageId::Java => {
                if is_java_main_signature(trimmed) {
                    entries.push(RunnableGutterEntry {
                        line: idx,
                        label: "Run 'main()'".to_string(),
                        command: "java".to_string(),
                        args: vec![],
                    });
                } else if trimmed == "@Test"
                    || trimmed.starts_with("@Test(")
                    || trimmed == "@ParameterizedTest"
                    || trimmed.starts_with("@ParameterizedTest(")
                    || trimmed == "@RepeatedTest"
                    || trimmed.starts_with("@RepeatedTest(")
                    || trimmed == "@TestFactory"
                {
                    let (target_line, method_name) =
                        find_next_java_method_line(&lines, idx).unwrap_or((idx, None));
                    let label = match &method_name {
                        Some(m) => format!("Run '{m}()'"),
                        None => "Run JUnit Test".to_string(),
                    };
                    let args = match method_name {
                        Some(m) => vec!["test".to_string(), m],
                        None => vec!["test".to_string()],
                    };
                    entries.push(RunnableGutterEntry {
                        line: target_line,
                        label,
                        command: "mvn".to_string(),
                        args,
                    });
                } else if trimmed.starts_with("@SpringBootApplication") {
                    let class_line = (idx + 1..=(idx + 4).min(lines.len().saturating_sub(1)))
                        .find(|&l| lines[l].contains("class "))
                        .unwrap_or(idx);
                    entries.push(RunnableGutterEntry {
                        line: class_line,
                        label: "Run Spring Boot".to_string(),
                        command: "mvn".to_string(),
                        args: vec!["spring-boot:run".to_string()],
                    });
                }
            }
            _ => {}
        }
    }

    entries
}

fn is_valid_java_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn strip_leading_java_modifiers(mut s: &str) -> (&str, bool) {
    let mut had_visibility_or_static = false;
    loop {
        let trimmed = s.trim_start();
        let mut matched = false;
        for kw in [
            "public ",
            "protected ",
            "private ",
            "static ",
            "final ",
            "abstract ",
            "synchronized ",
            "native ",
            "strictfp ",
            "default ",
            "sealed ",
            "non-sealed ",
            "transient ",
            "volatile ",
        ] {
            if let Some(rest) = trimmed.strip_prefix(kw) {
                if matches!(
                    kw,
                    "public " | "protected " | "private " | "static " | "final "
                ) {
                    had_visibility_or_static = true;
                }
                s = rest;
                matched = true;
                break;
            }
        }
        if !matched {
            return (trimmed, had_visibility_or_static);
        }
    }
}

fn extract_java_structure_symbol(raw_line: &str, line_idx: usize) -> Option<StructureSymbol> {
    let no_comment = raw_line.split("//").next().unwrap_or("").trim();
    if no_comment.is_empty()
        || no_comment.starts_with("/*")
        || no_comment.starts_with('*')
        || no_comment.starts_with("package ")
        || no_comment.starts_with("import ")
        || no_comment.starts_with("return ")
        || no_comment.starts_with("throw ")
        || no_comment.starts_with("new ")
    {
        return None;
    }

    // Strip inline leading annotation like `@Test void foo() {}`
    let mut work = no_comment;
    while work.starts_with('@') && !work.starts_with("@interface ") {
        let after_at = &work[1..];
        let end_ident = after_at
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(after_at.len());
        let mut rest = &after_at[end_ident..];
        if rest.trim_start().starts_with('(') {
            if let Some(close) = rest.find(')') {
                rest = &rest[close + 1..];
            } else {
                return None;
            }
        }
        work = rest.trim_start();
        if work.is_empty() {
            return None;
        }
    }

    let (rest, had_modifier) = strip_leading_java_modifiers(work);

    if let Some(after) = rest.strip_prefix("class ") {
        let name = after.split(['{', '<', ' ']).next()?.trim();
        if is_valid_java_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Class,
                line: line_idx,
            });
        }
    } else if let Some(after) = rest
        .strip_prefix("interface ")
        .or_else(|| rest.strip_prefix("@interface "))
    {
        let name = after.split(['{', '<', ' ']).next()?.trim();
        if is_valid_java_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Interface,
                line: line_idx,
            });
        }
    } else if let Some(after) = rest.strip_prefix("enum ") {
        let name = after.split(['{', ' ']).next()?.trim();
        if is_valid_java_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Enum,
                line: line_idx,
            });
        }
    } else if let Some(after) = rest.strip_prefix("record ") {
        let name = after.split(['(', '<', ' ', '{']).next()?.trim();
        if is_valid_java_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Class,
                line: line_idx,
            });
        }
    } else if let Some((before_paren, _)) = rest.split_once('(') {
        if !before_paren.contains('=') && !before_paren.contains('.') {
            let name = before_paren.split_whitespace().last()?.trim();
            if is_valid_java_ident(name)
                && !matches!(
                    name,
                    "if" | "for"
                        | "while"
                        | "switch"
                        | "catch"
                        | "synchronized"
                        | "return"
                        | "new"
                        | "super"
                        | "this"
                        | "throw"
                        | "assert"
                        | "else"
                        | "try"
                )
            {
                return Some(StructureSymbol {
                    name: name.to_string(),
                    kind: SymbolKind::Function,
                    line: line_idx,
                });
            }
        }
    } else if had_modifier && (rest.ends_with(';') || rest.contains('=')) && !rest.contains('(') {
        let lhs = rest.split(['=', ';']).next()?.trim();
        let name = lhs.split_whitespace().last()?.trim();
        if is_valid_java_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Constant,
                line: line_idx,
            });
        }
    }

    None
}

fn is_java_main_signature(trimmed: &str) -> bool {
    trimmed.contains("void main(")
        && (trimmed.contains("public static")
            || trimmed.contains("static public")
            || trimmed.starts_with("void main(")
            || trimmed.starts_with("static void main("))
}

fn find_next_java_method_line(lines: &[&str], anno_line: usize) -> Option<(usize, Option<String>)> {
    if let Some(sym) = extract_java_structure_symbol(lines[anno_line], anno_line) {
        if sym.kind == SymbolKind::Function {
            return Some((anno_line, Some(sym.name)));
        }
    }
    let end = (anno_line + 5).min(lines.len().saturating_sub(1));
    for idx in (anno_line + 1)..=end {
        let t = lines[idx].trim();
        if t.is_empty() || t.starts_with('@') || t.starts_with("//") {
            continue;
        }
        if let Some(sym) = extract_java_structure_symbol(t, idx) {
            if sym.kind == SymbolKind::Function {
                return Some((idx, Some(sym.name)));
            }
        }
        break;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_rust_structure_and_runnables() {
        let src = "pub struct Engine {}\n\nfn main() {\n    println!(\"hi\");\n}\n\n#[test]\nfn it_works() {}\n";
        let syms = extract_structure_symbols(src, LanguageId::Rust);
        assert_eq!(syms.len(), 3);
        let runnables = detect_runnable_gutter_entries(src, LanguageId::Rust);
        assert_eq!(runnables.len(), 2);
    }

    #[test]
    fn test_java_highlight_structure_and_runnables() {
        let spans = highlight_line("@GetMapping(\"/users\") // endpoint", LanguageId::Java);
        assert!(spans
            .iter()
            .any(|s| s.text == "@GetMapping" && s.kind == TokenKind::MacroOrDecorator));
        assert!(spans
            .iter()
            .any(|s| s.text == "\"/users\"" && s.kind == TokenKind::String));
        assert!(spans
            .iter()
            .any(|s| s.text == "// endpoint" && s.kind == TokenKind::Comment));

        let src = "\
package com.example;

@SpringBootApplication
public class DemoApp {
    public static final int MAX_RETRIES = 3;

    public record UserDto(String name) {}

    public static void main(String[] args) {
        var greeting = \"hello\";
    }

    @Test
    void shouldLoadContext() {}
}
";
        let syms = extract_structure_symbols(src, LanguageId::Java);
        assert!(syms.iter().any(|s| s.name == "DemoApp" && s.kind == SymbolKind::Class));
        assert!(syms.iter().any(|s| s.name == "MAX_RETRIES" && s.kind == SymbolKind::Constant));
        assert!(syms.iter().any(|s| s.name == "UserDto" && s.kind == SymbolKind::Class));
        assert!(syms.iter().any(|s| s.name == "main" && s.kind == SymbolKind::Function));
        assert!(syms.iter().any(|s| s.name == "shouldLoadContext" && s.kind == SymbolKind::Function));

        assert!(find_symbol_definition(src, LanguageId::Java, "greeting").is_some());
        assert!(find_symbol_definition(src, LanguageId::Java, "DemoApp").is_some());

        let runnables = detect_runnable_gutter_entries(src, LanguageId::Java);
        assert!(runnables.iter().any(|r| r.label == "Run Spring Boot" && r.line == 3));
        assert!(runnables.iter().any(|r| r.label == "Run 'main()'" && r.line == 8));
        assert!(runnables.iter().any(|r| r.label == "Run 'shouldLoadContext()'" && r.line == 13));
    }
}
