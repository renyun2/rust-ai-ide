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

/// 对单行代码执行针对 Rust / TypeScript / JavaScript / Python 的 IDEA Darcula 风格语法高亮切分
pub fn highlight_line(line: &str, lang: LanguageId) -> Vec<HighlightSpan> {
    let trimmed = line.trim_start();
    if (matches!(
        lang,
        LanguageId::Rust | LanguageId::TypeScript | LanguageId::JavaScript
    ) && trimmed.starts_with("//"))
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

    if matches!(
        lang,
        LanguageId::Python | LanguageId::TypeScript
    ) && trimmed.starts_with('@')
    {
        return vec![HighlightSpan {
            text: line.to_string(),
            kind: TokenKind::MacroOrDecorator,
        }];
    }

    let keywords = match lang {
        LanguageId::Rust => RUST_KEYWORDS,
        LanguageId::TypeScript | LanguageId::JavaScript => TS_JS_KEYWORDS,
        LanguageId::Python => PYTHON_KEYWORDS,
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
                LanguageId::Rust | LanguageId::TypeScript | LanguageId::JavaScript
            )
        {
            let comment: String = chars[i..].iter().collect();
            spans.push(HighlightSpan {
                text: comment,
                kind: TokenKind::Comment,
            });
            break;
        }
        if c == '#' && matches!(lang, LanguageId::Python | LanguageId::Toml) {
            let comment: String = chars[i..].iter().collect();
            spans.push(HighlightSpan {
                text: comment,
                kind: TokenKind::Comment,
            });
            break;
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
                    .or_else(|| line.strip_prefix("pub async fn "))
                    .or_else(|| line.strip_prefix("async fn "))
                    .or_else(|| line.strip_prefix("fn "))
                {
                    if let Some(name) = rest.split(['(', '<']).next() {
                        symbols.push(StructureSymbol {
                            name: name.trim().to_string(),
                            kind: SymbolKind::Function,
                            line: line_idx,
                        });
                    }
                } else if let Some(rest) = line
                    .strip_prefix("pub struct ")
                    .or_else(|| line.strip_prefix("struct "))
                {
                    if let Some(name) = rest.split(['{', '(', '<', ' ']).next() {
                        symbols.push(StructureSymbol {
                            name: name.trim().to_string(),
                            kind: SymbolKind::Struct,
                            line: line_idx,
                        });
                    }
                } else if let Some(rest) = line
                    .strip_prefix("pub enum ")
                    .or_else(|| line.strip_prefix("enum "))
                {
                    if let Some(name) = rest.split(['{', '<', ' ']).next() {
                        symbols.push(StructureSymbol {
                            name: name.trim().to_string(),
                            kind: SymbolKind::Enum,
                            line: line_idx,
                        });
                    }
                } else if let Some(rest) = line
                    .strip_prefix("pub trait ")
                    .or_else(|| line.strip_prefix("trait "))
                {
                    if let Some(name) = rest.split(['{', '<', ':']).next() {
                        symbols.push(StructureSymbol {
                            name: name.trim().to_string(),
                            kind: SymbolKind::Trait,
                            line: line_idx,
                        });
                    }
                } else if let Some(rest) = line.strip_prefix("impl") {
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
            LanguageId::TypeScript | LanguageId::JavaScript => {
                if let Some(rest) = line
                    .strip_prefix("export function ")
                    .or_else(|| line.strip_prefix("export async function "))
                    .or_else(|| line.strip_prefix("async function "))
                    .or_else(|| line.strip_prefix("function "))
                {
                    if let Some(name) = rest.split(['(', '<']).next() {
                        symbols.push(StructureSymbol {
                            name: name.trim().to_string(),
                            kind: SymbolKind::Function,
                            line: line_idx,
                        });
                    }
                } else if let Some(rest) = line
                    .strip_prefix("export class ")
                    .or_else(|| line.strip_prefix("class "))
                {
                    if let Some(name) = rest.split(['{', ' ', '<']).next() {
                        symbols.push(StructureSymbol {
                            name: name.trim().to_string(),
                            kind: SymbolKind::Class,
                            line: line_idx,
                        });
                    }
                } else if let Some(rest) = line
                    .strip_prefix("export interface ")
                    .or_else(|| line.strip_prefix("interface "))
                {
                    if let Some(name) = rest.split(['{', ' ', '<']).next() {
                        symbols.push(StructureSymbol {
                            name: name.trim().to_string(),
                            kind: SymbolKind::Interface,
                            line: line_idx,
                        });
                    }
                }
            }
            LanguageId::Python => {
                if let Some(rest) = line
                    .strip_prefix("def ")
                    .or_else(|| line.strip_prefix("async def "))
                {
                    if let Some(name) = rest.split('(').next() {
                        symbols.push(StructureSymbol {
                            name: name.trim().to_string(),
                            kind: SymbolKind::Function,
                            line: line_idx,
                        });
                    }
                } else if let Some(rest) = line.strip_prefix("class ") {
                    if let Some(name) = rest.split(['(', ':']).next() {
                        symbols.push(StructureSymbol {
                            name: name.trim().to_string(),
                            kind: SymbolKind::Class,
                            line: line_idx,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    symbols
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
            _ => {}
        }
    }

    entries
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
}
