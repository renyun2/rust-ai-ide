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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MarkupSection {
    #[default]
    Markup,
    ScriptTs,
    ScriptJs,
    Style,
    AstroFrontmatter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OpenMarkupTagKind {
    #[default]
    None,
    Generic,
    ScriptJs,
    ScriptTs,
    Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MarkupAttrQuote {
    #[default]
    None,
    Double,
    Single,
    DirectiveDouble,
    DirectiveSingle,
}

/// 跨行语法高亮状态机（支持 Vue/Svelte/Astro SFC 区块、跨行标签/属性、多行注释与模板字符串）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HighlightState {
    pub section: MarkupSection,
    pub in_block_comment: bool,
    pub in_html_comment: bool,
    pub in_template_string: bool,
    pub astro_frontmatter_done: bool,
    pub seen_non_empty: bool,
    pub open_markup_tag: OpenMarkupTagKind,
    pub markup_attr_quote: MarkupAttrQuote,
    pub markup_last_attr_was_directive: bool,
    pub markup_pending_lang_attr: bool,
    pub in_mustache: bool,
    pub brace_expr_depth: u16,
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

    pub fn display_label(&self) -> &'static str {
        match self {
            Self::Function => "Function",
            Self::Struct => "Struct",
            Self::Enum => "Enum",
            Self::Trait => "Trait",
            Self::Impl => "Impl",
            Self::Class => "Class",
            Self::Interface => "Interface",
            Self::Constant => "Constant",
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
    "extern", "break", "continue", "box", "yield",
];

const RUST_BUILTIN_TYPES: &[&str] = &[
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32",
    "f64", "bool", "char", "str",
];

const TS_JS_KEYWORDS: &[&str] = &[
    "function", "const", "let", "var", "interface", "type", "class", "extends", "implements",
    "import", "export", "from", "return", "if", "else", "for", "while", "do", "switch", "case",
    "default", "break", "continue", "async", "await", "new", "try", "catch", "finally", "throw",
    "true", "false", "null", "undefined", "public", "private", "protected", "readonly", "static",
    "abstract", "override", "as", "typeof", "keyof", "in", "of", "instanceof", "void", "this",
    "super", "yield", "declare", "namespace", "enum", "satisfies", "get", "set",
];

const TS_JS_BUILTIN_TYPES: &[&str] = &[
    "string", "number", "boolean", "bigint", "symbol", "object", "any", "unknown", "never",
];

const VUE_SVELTE_MACROS: &[&str] = &[
    "defineProps", "defineEmits", "defineModel", "defineExpose", "defineOptions", "defineSlots",
    "withDefaults", "$state", "$derived", "$effect", "$props", "$bindable", "$inspect",
];

const PYTHON_KEYWORDS: &[&str] = &[
    "def", "class", "import", "from", "as", "return", "if", "elif", "else", "for", "while", "in",
    "try", "except", "finally", "raise", "with", "async", "await", "yield", "lambda", "pass",
    "break", "continue", "True", "False", "None", "and", "or", "not", "is", "self", "cls",
    "global", "nonlocal", "assert", "del", "match", "case", "type",
];

const PYTHON_BUILTIN_TYPES: &[&str] = &[
    "int", "str", "float", "bool", "list", "dict", "set", "tuple", "bytes", "object",
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

const GO_KEYWORDS: &[&str] = &[
    "package", "import", "func", "var", "const", "type", "struct", "interface", "map", "chan",
    "go", "defer", "return", "if", "else", "for", "range", "switch", "case", "default", "select",
    "break", "continue", "fallthrough", "goto", "true", "false", "nil", "iota",
];

const GO_BUILTIN_TYPES: &[&str] = &[
    "string", "bool", "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16",
    "uint32", "uint64", "uintptr", "byte", "rune", "float32", "float64", "complex64", "complex128",
    "error", "any", "comparable",
];

const KOTLIN_KEYWORDS: &[&str] = &[
    "package", "import", "fun", "val", "var", "class", "interface", "object", "data", "sealed",
    "enum", "annotation", "companion", "open", "abstract", "final", "override", "private",
    "protected", "public", "internal", "lateinit", "by", "lazy", "init", "constructor", "this",
    "super", "if", "else", "when", "for", "while", "do", "return", "break", "continue", "throw",
    "try", "catch", "finally", "in", "is", "as", "null", "true", "false", "suspend", "inline",
    "reified", "typealias", "where", "out",
];

const DART_KEYWORDS: &[&str] = &[
    "library", "import", "export", "part", "class", "mixin", "enum", "extension", "typedef",
    "abstract", "base", "final", "interface", "sealed", "implements", "extends", "with", "on",
    "var", "const", "late", "required", "static", "covariant", "dynamic", "void", "Function", "if",
    "else", "for", "while", "do", "switch", "case", "default", "break", "continue", "return",
    "try", "catch", "finally", "throw", "rethrow", "assert", "async", "await", "sync", "yield",
    "new", "this", "super", "true", "false", "null", "is", "as", "in", "get", "set", "operator",
    "factory", "external",
];

const DART_BUILTIN_TYPES: &[&str] = &[
    "int", "double", "num", "bool", "String", "List", "Map", "Set", "Future", "Stream", "Iterable",
    "Object", "Widget", "BuildContext", "State", "StatelessWidget", "StatefulWidget", "Key",
];

const C_CPP_KEYWORDS: &[&str] = &[
    "auto", "break", "case", "catch", "class", "const", "constexpr", "consteval", "constinit",
    "continue", "co_await", "co_return", "co_yield", "decltype", "default", "delete", "do", "else",
    "enum", "explicit", "export", "extern", "false", "final", "for", "friend", "goto", "if",
    "inline", "mutable", "namespace", "new", "noexcept", "nullptr", "NULL", "operator", "override",
    "private", "protected", "public", "register", "reinterpret_cast", "requires", "return",
    "sizeof", "static", "static_assert", "static_cast", "struct", "switch", "template", "this",
    "thread_local", "throw", "true", "try", "typedef", "typeid", "typename", "union", "using",
    "virtual", "volatile", "while",
];

const C_CPP_BUILTIN_TYPES: &[&str] = &[
    "void", "bool", "char", "short", "int", "long", "float", "double", "signed", "unsigned",
    "size_t", "ssize_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "int8_t", "int16_t",
    "int32_t", "int64_t", "uintptr_t", "ptrdiff_t", "wchar_t", "char8_t", "char16_t", "char32_t",
    "string", "vector", "map", "set", "unique_ptr", "shared_ptr", "optional",
];

const CSHARP_KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "base", "break", "case", "catch", "checked", "class",
    "const", "continue", "default", "delegate", "do", "else", "enum", "event", "explicit",
    "extern", "false", "finally", "fixed", "for", "foreach", "goto", "if", "implicit", "in",
    "interface", "internal", "is", "lock", "namespace", "new", "null", "operator", "out",
    "override", "params", "partial", "private", "protected", "public", "readonly", "record", "ref",
    "required", "return", "sealed", "sizeof", "stackalloc", "static", "struct", "switch", "this",
    "throw", "true", "try", "typeof", "unchecked", "unsafe", "using", "var", "virtual", "void",
    "volatile", "when", "where", "while", "yield", "get", "set", "init", "value",
];

const CSHARP_BUILTIN_TYPES: &[&str] = &[
    "bool", "byte", "sbyte", "char", "decimal", "double", "float", "int", "uint", "nint", "nuint",
    "long", "ulong", "short", "ushort", "object", "string", "dynamic", "Task", "List",
    "Dictionary", "IEnumerable",
];

const SQL_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "ON",
    "GROUP", "BY", "ORDER", "HAVING", "LIMIT", "OFFSET", "INSERT", "INTO", "VALUES", "UPDATE",
    "SET", "DELETE", "CREATE", "ALTER", "DROP", "TRUNCATE", "TABLE", "VIEW", "INDEX", "FUNCTION",
    "PROCEDURE", "TRIGGER", "DATABASE", "SCHEMA", "PRIMARY", "KEY", "FOREIGN", "REFERENCES", "NOT",
    "NULL", "DEFAULT", "UNIQUE", "CHECK", "CONSTRAINT", "AS", "AND", "OR", "IN", "EXISTS",
    "BETWEEN", "LIKE", "ILIKE", "IS", "DISTINCT", "UNION", "ALL", "CASE", "WHEN", "THEN", "ELSE",
    "END", "BEGIN", "COMMIT", "ROLLBACK", "TRANSACTION", "WITH", "RECURSIVE", "RETURNING", "ASC",
    "DESC", "TRUE", "FALSE", "IF", "CASCADE", "RESTRICT", "ADD", "COLUMN",
];

const SQL_TYPES: &[&str] = &[
    "INT", "INTEGER", "BIGINT", "SMALLINT", "TINYINT", "SERIAL", "BIGSERIAL", "TEXT", "VARCHAR",
    "CHAR", "NVARCHAR", "BOOLEAN", "BOOL", "DATE", "TIME", "TIMESTAMP", "TIMESTAMPTZ", "DATETIME",
    "NUMERIC", "DECIMAL", "REAL", "FLOAT", "DOUBLE", "JSON", "JSONB", "UUID", "BYTEA", "BLOB",
];

const SHELL_KEYWORDS: &[&str] = &[
    "if", "then", "elif", "else", "fi", "for", "in", "do", "done", "while", "until", "case",
    "esac", "function", "return", "exit", "export", "local", "readonly", "declare", "typeset",
    "set", "unset", "shift", "source", "alias", "echo", "printf", "cd", "pwd", "test", "true",
    "false", "break", "continue", "trap", "wait", "eval", "exec", "param", "foreach", "try",
    "catch", "finally", "FROM", "RUN", "CMD", "ENTRYPOINT", "COPY", "ADD", "ENV", "ARG", "WORKDIR",
    "EXPOSE", "VOLUME", "USER", "LABEL", "HEALTHCHECK", "SHELL", "STOPSIGNAL", "ONBUILD",
];

const CSS_KEYWORDS: &[&str] = &[
    "important", "solid", "dashed", "dotted", "none", "auto", "inherit", "initial", "unset",
    "revert", "flex", "grid", "block", "inline", "inline-block", "inline-flex", "hidden",
    "visible", "relative", "absolute", "fixed", "sticky", "static", "center", "left", "right",
    "top", "bottom", "bold", "normal", "italic", "pointer", "default", "transparent", "currentColor",
    " ease", "linear", "ease-in", "ease-out", "ease-in-out", "infinite", "row", "column", "wrap",
    "nowrap", "space-between", "space-around", "stretch",
];

fn language_keywords(lang: LanguageId) -> &'static [&'static str] {
    match lang {
        LanguageId::Rust => RUST_KEYWORDS,
        LanguageId::TypeScript | LanguageId::JavaScript => TS_JS_KEYWORDS,
        LanguageId::Python => PYTHON_KEYWORDS,
        LanguageId::Java => JAVA_KEYWORDS,
        LanguageId::Go => GO_KEYWORDS,
        LanguageId::Kotlin => KOTLIN_KEYWORDS,
        LanguageId::Dart => DART_KEYWORDS,
        LanguageId::C | LanguageId::Cpp => C_CPP_KEYWORDS,
        LanguageId::CSharp => CSHARP_KEYWORDS,
        LanguageId::Shell => SHELL_KEYWORDS,
        LanguageId::Css => CSS_KEYWORDS,
        _ => &[],
    }
}

fn language_builtin_types(lang: LanguageId) -> &'static [&'static str] {
    match lang {
        LanguageId::Rust => RUST_BUILTIN_TYPES,
        LanguageId::TypeScript | LanguageId::JavaScript => TS_JS_BUILTIN_TYPES,
        LanguageId::Python => PYTHON_BUILTIN_TYPES,
        LanguageId::Go => GO_BUILTIN_TYPES,
        LanguageId::Dart => DART_BUILTIN_TYPES,
        LanguageId::C | LanguageId::Cpp => C_CPP_BUILTIN_TYPES,
        LanguageId::CSharp => CSHARP_BUILTIN_TYPES,
        _ => &[],
    }
}

fn uses_slash_comments(lang: LanguageId) -> bool {
    matches!(
        lang,
        LanguageId::Rust
            | LanguageId::TypeScript
            | LanguageId::JavaScript
            | LanguageId::Java
            | LanguageId::Go
            | LanguageId::Kotlin
            | LanguageId::Dart
            | LanguageId::C
            | LanguageId::Cpp
            | LanguageId::CSharp
            | LanguageId::Css
            | LanguageId::Json
    )
}

fn uses_hash_comments(lang: LanguageId) -> bool {
    matches!(
        lang,
        LanguageId::Python | LanguageId::Toml | LanguageId::Yaml | LanguageId::Shell
    )
}

fn is_block_comment_continuation(trimmed: &str, lang: LanguageId) -> bool {
    uses_slash_comments(lang)
        && (trimmed.starts_with("/**")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*/")
            || trimmed == "*"
            || trimmed.starts_with("* "))
}

/// 对单行代码执行针对全部支持语言的 IDEA Islands Dark / Darcula 风格语法高亮切分
pub fn highlight_line(line: &str, lang: LanguageId) -> Vec<HighlightSpan> {
    let mut state = HighlightState::default();
    highlight_line_with_state(line, lang, &mut state)
}

/// 仅推进跨行高亮状态机（用于可视区域之前的行快速同步 `<script>`/`<style>`/多行注释状态）
pub fn advance_highlight_state(line: &str, lang: LanguageId, state: &mut HighlightState) {
    let _ = highlight_line_with_state(line, lang, state);
}

/// 带跨行状态的单行语法高亮（支持 Vue / Svelte / Astro / HTML 嵌入式区块与多行注释）
pub fn highlight_line_with_state(
    line: &str,
    lang: LanguageId,
    state: &mut HighlightState,
) -> Vec<HighlightSpan> {
    if line.is_empty() {
        return Vec::new();
    }

    match lang {
        LanguageId::Markdown => return highlight_markdown_line(line),
        LanguageId::Vue | LanguageId::Svelte | LanguageId::Astro | LanguageId::Html => {
            return highlight_sfc_or_html_line(line, lang, state);
        }
        LanguageId::Xml => return highlight_markup_line_with_state(line, lang, state),
        LanguageId::Json => return highlight_json_line_with_state(line, state),
        LanguageId::Toml => return highlight_toml_line(line),
        LanguageId::Yaml => return highlight_yaml_line(line),
        LanguageId::Css => return highlight_css_line_with_state(line, state),
        LanguageId::Sql => return highlight_sql_line(line),
        LanguageId::PlainText => {
            return vec![HighlightSpan {
                text: line.to_string(),
                kind: TokenKind::Plain,
            }];
        }
        _ => {}
    }

    highlight_code_line_with_state(line, lang, state)
}

fn highlight_code_line_with_state(
    line: &str,
    lang: LanguageId,
    state: &mut HighlightState,
) -> Vec<HighlightSpan> {
    if line.is_empty() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut rest = line;

    if state.in_block_comment && uses_slash_comments(lang) {
        if let Some(end_rel) = rest.find("*/") {
            let end_idx = end_rel + 2;
            spans.push(HighlightSpan {
                text: rest[..end_idx].to_string(),
                kind: TokenKind::Comment,
            });
            state.in_block_comment = false;
            rest = &rest[end_idx..];
            if rest.is_empty() {
                return spans;
            }
        } else {
            return vec![HighlightSpan {
                text: line.to_string(),
                kind: TokenKind::Comment,
            }];
        }
    }

    if state.in_template_string
        && matches!(lang, LanguageId::TypeScript | LanguageId::JavaScript | LanguageId::Go)
    {
        let chars: Vec<char> = rest.chars().collect();
        let (consumed, closed) = consume_template_string(&chars, 0, lang, &mut spans);
        state.in_template_string = !closed;
        let tail: String = chars[consumed..].iter().collect();
        if tail.is_empty() {
            return spans;
        }
        rest = &rest[rest.len() - tail.len()..];
    }

    let trimmed = rest.trim_start();
    if (uses_slash_comments(lang)
        && (trimmed.starts_with("//") || is_block_comment_continuation(trimmed, lang)))
        || (uses_hash_comments(lang) && trimmed.starts_with('#'))
    {
        if trimmed.starts_with("/*") && !trimmed.contains("*/") && uses_slash_comments(lang) {
            state.in_block_comment = true;
        }
        spans.push(HighlightSpan {
            text: rest.to_string(),
            kind: TokenKind::Comment,
        });
        return spans;
    }

    if lang == LanguageId::Rust && (trimmed.starts_with("#[") || trimmed.starts_with("#![")) {
        spans.push(HighlightSpan {
            text: rest.to_string(),
            kind: TokenKind::MacroOrDecorator,
        });
        return spans;
    }

    if matches!(lang, LanguageId::C | LanguageId::Cpp) && trimmed.starts_with('#') {
        spans.extend(highlight_c_preprocessor_line(rest));
        return spans;
    }

    if lang == LanguageId::CSharp
        && trimmed.starts_with('[')
        && trimmed.ends_with(']')
        && trimmed.len() > 2
    {
        spans.push(HighlightSpan {
            text: rest.to_string(),
            kind: TokenKind::MacroOrDecorator,
        });
        return spans;
    }

    if matches!(lang, LanguageId::Python | LanguageId::TypeScript)
        && trimmed.starts_with('@')
        && !trimmed.contains('(')
    {
        spans.push(HighlightSpan {
            text: rest.to_string(),
            kind: TokenKind::MacroOrDecorator,
        });
        return spans;
    }

    let keywords = language_keywords(lang);
    let builtin_types = language_builtin_types(lang);

    let chars: Vec<char> = rest.chars().collect();
    let mut i = 0;

    if matches!(lang, LanguageId::TypeScript | LanguageId::JavaScript)
        && state.open_markup_tag != OpenMarkupTagKind::None
    {
        let (next_i, _) = consume_markup_tag_body(&chars, 0, lang, state, &mut spans);
        i = next_i;
    }

    while i < chars.len() {
        let c = chars[i];

        // 行尾注释 //
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' && uses_slash_comments(lang) {
            let comment: String = chars[i..].iter().collect();
            spans.push(HighlightSpan {
                text: comment,
                kind: TokenKind::Comment,
            });
            break;
        }
        // 行内或跨行块注释 /* ... */
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' && uses_slash_comments(lang) {
            let start = i;
            i += 2;
            let mut closed = false;
            while i + 1 < chars.len() {
                if chars[i] == '*' && chars[i + 1] == '/' {
                    i += 2;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                i = chars.len();
                state.in_block_comment = true;
            }
            let comment: String = chars[start..i].iter().collect();
            spans.push(HighlightSpan {
                text: comment,
                kind: TokenKind::Comment,
            });
            continue;
        }
        // # 行尾注释
        if c == '#' && uses_hash_comments(lang) {
            let comment: String = chars[i..].iter().collect();
            spans.push(HighlightSpan {
                text: comment,
                kind: TokenKind::Comment,
            });
            break;
        }

        // JSX / TSX 标签检测（如 <MyComponent prop={val}>, <div>, </div>, <>）
        if c == '<'
            && matches!(lang, LanguageId::TypeScript | LanguageId::JavaScript)
            && is_jsx_tag_start(&chars, i)
        {
            let (next_i, _) = consume_markup_tag(&chars, i, lang, state, &mut spans);
            i = next_i;
            continue;
        }

        // 注解 / 装饰器 (@Override, @SpringBootApplication, @Composable, @Component)
        if c == '@'
            && matches!(
                lang,
                LanguageId::Java
                    | LanguageId::Kotlin
                    | LanguageId::Dart
                    | LanguageId::TypeScript
                    | LanguageId::JavaScript
                    | LanguageId::Python
            )
            && i + 1 < chars.len()
            && (chars[i + 1].is_ascii_alphabetic() || chars[i + 1] == '_')
        {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                i += 1;
            }
            let anno: String = chars[start..i].iter().collect();
            spans.push(HighlightSpan {
                text: anno,
                kind: TokenKind::MacroOrDecorator,
            });
            continue;
        }

        // Shell / PowerShell / Dart / Kotlin 变量引用 / Svelte runes ($state, $derived, $effect)
        if c == '$'
            && matches!(
                lang,
                LanguageId::Shell
                    | LanguageId::Kotlin
                    | LanguageId::Dart
                    | LanguageId::TypeScript
                    | LanguageId::JavaScript
            )
            && i + 1 < chars.len()
            && (chars[i + 1].is_ascii_alphanumeric()
                || matches!(chars[i + 1], '_' | '{' | '(' | '?' | '@' | '*' | ':'))
        {
            let start = i;
            if matches!(lang, LanguageId::TypeScript | LanguageId::JavaScript) {
                i += 1;
                if i < chars.len() && chars[i] == ':' {
                    i += 1;
                    spans.push(HighlightSpan {
                        text: "$:".to_string(),
                        kind: TokenKind::MacroOrDecorator,
                    });
                    continue;
                }
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let tok: String = chars[start..i].iter().collect();
                let kind = if VUE_SVELTE_MACROS.contains(&tok.as_str()) {
                    TokenKind::MacroOrDecorator
                } else if i < chars.len() && chars[i] == '(' {
                    TokenKind::Function
                } else {
                    TokenKind::Plain
                };
                spans.push(HighlightSpan { text: tok, kind });
                continue;
            }
            i += 1;
            if i < chars.len() && chars[i] == '{' {
                i += 1;
                while i < chars.len() && chars[i] != '}' {
                    i += 1;
                }
                if i < chars.len() {
                    i += 1;
                }
            } else {
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | ':'))
                {
                    i += 1;
                }
            }
            let var_tok: String = chars[start..i].iter().collect();
            spans.push(HighlightSpan {
                text: var_tok,
                kind: TokenKind::FieldOrProperty,
            });
            continue;
        }

        // Shell 命令行参数选项 (--flag / -f)
        if lang == LanguageId::Shell
            && c == '-'
            && (i == 0 || chars[i - 1].is_whitespace())
            && i + 1 < chars.len()
            && (chars[i + 1].is_ascii_alphabetic() || chars[i + 1] == '-')
        {
            let start = i;
            while i < chars.len()
                && !chars[i].is_whitespace()
                && !matches!(chars[i], '=' | ';' | '|' | ')' | '"' | '\'')
            {
                i += 1;
            }
            let flag_tok: String = chars[start..i].iter().collect();
            spans.push(HighlightSpan {
                text: flag_tok,
                kind: TokenKind::MacroOrDecorator,
            });
            continue;
        }

        // Rust 字符字面量 ('a', '\n') vs 生命周期 ('a, 'static)
        if c == '\'' && lang == LanguageId::Rust {
            let start = i;
            // 检测是否为字符字面量：'x' 或 '\x' / '\u{...}'
            let is_char_lit = if i + 2 < chars.len() && chars[i + 1] != '\\' && chars[i + 2] == '\''
            {
                i += 3;
                true
            } else if i + 3 < chars.len() && chars[i + 1] == '\\' {
                let mut j = i + 2;
                while j < chars.len() && j <= i + 10 && chars[j] != '\'' && !chars[j].is_whitespace()
                {
                    j += 1;
                }
                if j < chars.len() && chars[j] == '\'' {
                    i = j + 1;
                    true
                } else {
                    false
                }
            } else {
                false
            };

            if is_char_lit {
                let s: String = chars[start..i].iter().collect();
                spans.push(HighlightSpan {
                    text: s,
                    kind: TokenKind::String,
                });
                continue;
            } else if i + 1 < chars.len()
                && (chars[i + 1].is_ascii_alphabetic() || chars[i + 1] == '_')
            {
                i += 1;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let lt: String = chars[start..i].iter().collect();
                spans.push(HighlightSpan {
                    text: lt,
                    kind: TokenKind::MacroOrDecorator,
                });
                continue;
            }
        }

        // 模板字符串 `...${expr}...`
        if c == '`'
            && matches!(
                lang,
                LanguageId::TypeScript
                    | LanguageId::JavaScript
                    | LanguageId::Go
                    | LanguageId::Shell
            )
        {
            spans.push(HighlightSpan {
                text: "`".to_string(),
                kind: TokenKind::String,
            });
            let (next_i, closed) = consume_template_string(&chars, i + 1, lang, &mut spans);
            if !closed && matches!(lang, LanguageId::TypeScript | LanguageId::JavaScript | LanguageId::Go) {
                state.in_template_string = true;
            }
            i = next_i;
            continue;
        }

        // 字符串字面量 ("...", '...')
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

        // 标识符、关键字、类型、函数或字段属性
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric()
                    || chars[i] == '_'
                    || (lang == LanguageId::Shell
                        && chars[i] == '-'
                        && i + 1 < chars.len()
                        && (chars[i + 1].is_ascii_alphanumeric() || chars[i + 1] == '_')))
            {
                i += 1;
            }
            // Rust 宏调用 foo!
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
            let preceded_by_dot =
                start > 0 && chars[start - 1] == '.' && (start < 2 || chars[start - 2] != '.');
            let followed_by_paren = i < chars.len() && chars[i] == '(';
            let followed_by_single_colon = i < chars.len()
                && chars[i] == ':'
                && (i + 1 >= chars.len() || chars[i + 1] != ':');

            let kind = if matches!(lang, LanguageId::TypeScript | LanguageId::JavaScript)
                && VUE_SVELTE_MACROS.contains(&word.as_str())
            {
                TokenKind::MacroOrDecorator
            } else if keywords.contains(&word.as_str()) {
                TokenKind::Keyword
            } else if builtin_types.contains(&word.as_str())
                || word
                    .chars()
                    .next()
                    .map(|ch| ch.is_ascii_uppercase())
                    .unwrap_or(false)
            {
                TokenKind::Type
            } else if followed_by_paren {
                TokenKind::Function
            } else if preceded_by_dot || followed_by_single_colon {
                TokenKind::FieldOrProperty
            } else {
                TokenKind::Plain
            };
            spans.push(HighlightSpan { text: word, kind });
            continue;
        }

        // 数字字面量 (十进制 / 十六进制 0x / 二进制 0b / 浮点数)
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
            && chars[i] != '`'
            && chars[i] != '/'
            && chars[i] != '#'
            && chars[i] != '@'
            && chars[i] != '$'
            && !(matches!(lang, LanguageId::TypeScript | LanguageId::JavaScript)
                && chars[i] == '<')
            && !(lang == LanguageId::Shell && chars[i] == '-')
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

fn consume_template_string(
    chars: &[char],
    mut i: usize,
    lang: LanguageId,
    spans: &mut Vec<HighlightSpan>,
) -> (usize, bool) {
    let mut seg_start = i;
    while i < chars.len() {
        if chars[i] == '\\' {
            i = (i + 2).min(chars.len());
            continue;
        }
        if matches!(lang, LanguageId::TypeScript | LanguageId::JavaScript)
            && chars[i] == '$'
            && i + 1 < chars.len()
            && chars[i + 1] == '{'
        {
            if i > seg_start {
                spans.push(HighlightSpan {
                    text: chars[seg_start..i].iter().collect(),
                    kind: TokenKind::String,
                });
            }
            spans.push(HighlightSpan {
                text: "${".to_string(),
                kind: TokenKind::MacroOrDecorator,
            });
            i += 2;
            let expr_start = i;
            let mut depth = 1usize;
            while i < chars.len() && depth > 0 {
                if chars[i] == '{' {
                    depth += 1;
                } else if chars[i] == '}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                i += 1;
            }
            let expr: String = chars[expr_start..i].iter().collect();
            if !expr.is_empty() {
                spans.extend(highlight_inline_js_expr(&expr));
            }
            if i < chars.len() && chars[i] == '}' {
                spans.push(HighlightSpan {
                    text: "}".to_string(),
                    kind: TokenKind::MacroOrDecorator,
                });
                i += 1;
            }
            seg_start = i;
            continue;
        }
        if chars[i] == '`' {
            if i > seg_start {
                spans.push(HighlightSpan {
                    text: chars[seg_start..i].iter().collect(),
                    kind: TokenKind::String,
                });
            }
            spans.push(HighlightSpan {
                text: "`".to_string(),
                kind: TokenKind::String,
            });
            return (i + 1, true);
        }
        i += 1;
    }
    if i > seg_start {
        spans.push(HighlightSpan {
            text: chars[seg_start..i].iter().collect(),
            kind: TokenKind::String,
        });
    }
    (i, false)
}

fn highlight_inline_js_expr(expr: &str) -> Vec<HighlightSpan> {
    let mut local_state = HighlightState::default();
    highlight_code_line_with_state(expr, LanguageId::TypeScript, &mut local_state)
}

fn is_jsx_tag_start(chars: &[char], idx: usize) -> bool {
    if idx + 1 >= chars.len() {
        return false;
    }
    let next = chars[idx + 1];
    // Fragment `<>` or closing tag `</tag>` / `</>`
    if next == '>' || next == '/' {
        return true;
    }
    if !next.is_ascii_alphabetic() && next != '_' {
        return false;
    }
    // Check character immediately before `<` (skipping whitespace)
    let mut prev_pos = idx;
    let mut prev_non_ws = None;
    while prev_pos > 0 {
        prev_pos -= 1;
        if !chars[prev_pos].is_whitespace() {
            prev_non_ws = Some(prev_pos);
            break;
        }
    }
    let allowed_context = match prev_non_ws {
        None => true,
        Some(p) => {
            let ch = chars[p];
            if matches!(ch, '=' | '(' | '[' | '{' | ',' | ':' | '?' | '!' | '&' | '|' | '>' | ';') {
                true
            } else if ch.is_ascii_alphabetic() {
                let mut w_start = p;
                while w_start > 0 && (chars[w_start - 1].is_ascii_alphanumeric() || chars[w_start - 1] == '_') {
                    w_start -= 1;
                }
                let prev_word: String = chars[w_start..=p].iter().collect();
                matches!(prev_word.as_str(), "return" | "yield" | "await" | "case" | "default")
            } else {
                false
            }
        }
    };
    if !allowed_context {
        return false;
    }
    // Distinguish `<T>(x: T) => ...` generic arrow functions from `<Tag ...>` JSX tags
    let mut j = idx + 1;
    while j < chars.len()
        && (chars[j].is_ascii_alphanumeric() || matches!(chars[j], '_' | '.' | '-' | ':'))
    {
        j += 1;
    }
    if j < chars.len() && chars[j] == '>' && j + 1 < chars.len() && chars[j + 1] == '(' {
        return false;
    }
    j >= chars.len() || chars[j].is_whitespace() || matches!(chars[j], '>' | '/')
}

fn highlight_c_preprocessor_line(line: &str) -> Vec<HighlightSpan> {
    if let Some(lt_idx) = line.find('<') {
        if let Some(gt_rel) = line[lt_idx..].find('>') {
            let gt_idx = lt_idx + gt_rel + 1;
            let mut spans = vec![
                HighlightSpan {
                    text: line[..lt_idx].to_string(),
                    kind: TokenKind::MacroOrDecorator,
                },
                HighlightSpan {
                    text: line[lt_idx..gt_idx].to_string(),
                    kind: TokenKind::String,
                },
            ];
            if gt_idx < line.len() {
                spans.push(HighlightSpan {
                    text: line[gt_idx..].to_string(),
                    kind: TokenKind::Comment,
                });
            }
            return spans;
        }
    }
    vec![HighlightSpan {
        text: line.to_string(),
        kind: TokenKind::MacroOrDecorator,
    }]
}

fn highlight_json_line_with_state(line: &str, state: &mut HighlightState) -> Vec<HighlightSpan> {
    let mut spans = Vec::new();
    let mut rest = line;
    if state.in_block_comment {
        if let Some(end_rel) = rest.find("*/") {
            let end_idx = end_rel + 2;
            spans.push(HighlightSpan {
                text: rest[..end_idx].to_string(),
                kind: TokenKind::Comment,
            });
            state.in_block_comment = false;
            rest = &rest[end_idx..];
            if rest.is_empty() {
                return spans;
            }
        } else {
            return vec![HighlightSpan {
                text: line.to_string(),
                kind: TokenKind::Comment,
            }];
        }
    }
    let trimmed = rest.trim_start();
    if trimmed.starts_with("//") || is_block_comment_continuation(trimmed, LanguageId::Json) {
        if trimmed.starts_with("/*") && !trimmed.contains("*/") {
            state.in_block_comment = true;
        }
        spans.push(HighlightSpan {
            text: rest.to_string(),
            kind: TokenKind::Comment,
        });
        return spans;
    }
    let chars: Vec<char> = rest.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            spans.push(HighlightSpan {
                text: chars[i..].iter().collect(),
                kind: TokenKind::Comment,
            });
            break;
        }
        if c == '"' {
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let s: String = chars[start..i.min(chars.len())].iter().collect();
            let mut look = i;
            while look < chars.len() && chars[look].is_whitespace() {
                look += 1;
            }
            let is_key = look < chars.len() && chars[look] == ':';
            spans.push(HighlightSpan {
                text: s,
                kind: if is_key {
                    TokenKind::FieldOrProperty
                } else {
                    TokenKind::String
                },
            });
            continue;
        }
        if c.is_ascii_digit() || (c == '-' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit())
        {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || matches!(chars[i], '.' | 'e' | 'E' | '+' | '-'))
            {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::Number,
            });
            continue;
        }
        if c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let kind = if matches!(word.as_str(), "true" | "false" | "null") {
                TokenKind::Keyword
            } else {
                TokenKind::Plain
            };
            spans.push(HighlightSpan { text: word, kind });
            continue;
        }
        let start = i;
        i += 1;
        while i < chars.len()
            && !chars[i].is_ascii_alphanumeric()
            && chars[i] != '"'
            && chars[i] != '-'
            && chars[i] != '/'
        {
            i += 1;
        }
        spans.push(HighlightSpan {
            text: chars[start..i].iter().collect(),
            kind: TokenKind::Plain,
        });
    }
    spans
}

fn highlight_toml_line(line: &str) -> Vec<HighlightSpan> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return vec![HighlightSpan {
            text: line.to_string(),
            kind: TokenKind::Comment,
        }];
    }
    if trimmed.starts_with('[') {
        if let Some(comment_idx) = line.find('#') {
            return vec![
                HighlightSpan {
                    text: line[..comment_idx].to_string(),
                    kind: TokenKind::MacroOrDecorator,
                },
                HighlightSpan {
                    text: line[comment_idx..].to_string(),
                    kind: TokenKind::Comment,
                },
            ];
        }
        return vec![HighlightSpan {
            text: line.to_string(),
            kind: TokenKind::MacroOrDecorator,
        }];
    }

    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    let mut seen_eq = false;

    while i < chars.len() {
        let c = chars[i];
        if c == '#' {
            spans.push(HighlightSpan {
                text: chars[i..].iter().collect(),
                kind: TokenKind::Comment,
            });
            break;
        }
        if c == '=' {
            seen_eq = true;
            spans.push(HighlightSpan {
                text: "=".to_string(),
                kind: TokenKind::Plain,
            });
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if quote == '"' && chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i.min(chars.len())].iter().collect(),
                kind: if !seen_eq {
                    TokenKind::FieldOrProperty
                } else {
                    TokenKind::String
                },
            });
            continue;
        }
        if !seen_eq && (c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '-' | '.'))
            {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::FieldOrProperty,
            });
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric()
                    || matches!(chars[i], '.' | '_' | '-' | ':' | '+'))
            {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::Number,
            });
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '-'))
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let kind = if matches!(word.as_str(), "true" | "false") {
                TokenKind::Keyword
            } else {
                TokenKind::FieldOrProperty
            };
            spans.push(HighlightSpan { text: word, kind });
            continue;
        }
        let start = i;
        i += 1;
        while i < chars.len()
            && !chars[i].is_ascii_alphanumeric()
            && !matches!(chars[i], '#' | '=' | '"' | '\'' | '_')
        {
            i += 1;
        }
        spans.push(HighlightSpan {
            text: chars[start..i].iter().collect(),
            kind: TokenKind::Plain,
        });
    }
    spans
}

fn highlight_yaml_line(line: &str) -> Vec<HighlightSpan> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return vec![HighlightSpan {
            text: line.to_string(),
            kind: TokenKind::Comment,
        }];
    }
    if trimmed == "---" || trimmed == "..." {
        return vec![HighlightSpan {
            text: line.to_string(),
            kind: TokenKind::Keyword,
        }];
    }

    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    let mut seen_colon = false;

    while i < chars.len() {
        let c = chars[i];
        if c == '#' && (i == 0 || chars[i - 1].is_whitespace()) {
            spans.push(HighlightSpan {
                text: chars[i..].iter().collect(),
                kind: TokenKind::Comment,
            });
            break;
        }
        if c == '"' || c == '\'' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if quote == '"' && chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let mut look = i;
            while look < chars.len() && chars[look].is_whitespace() {
                look += 1;
            }
            let is_key = !seen_colon && look < chars.len() && chars[look] == ':';
            spans.push(HighlightSpan {
                text: chars[start..i.min(chars.len())].iter().collect(),
                kind: if is_key {
                    TokenKind::FieldOrProperty
                } else {
                    TokenKind::String
                },
            });
            continue;
        }
        if (c == '&' || c == '*')
            && i + 1 < chars.len()
            && (chars[i + 1].is_ascii_alphanumeric() || chars[i + 1] == '_')
        {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '-'))
            {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::MacroOrDecorator,
            });
            continue;
        }
        if c == ':' && (i + 1 >= chars.len() || chars[i + 1].is_whitespace()) {
            seen_colon = true;
            spans.push(HighlightSpan {
                text: ":".to_string(),
                kind: TokenKind::Plain,
            });
            i += 1;
            continue;
        }
        if !seen_colon && (c.is_ascii_alphanumeric() || matches!(c, '_' | '.')) {
            let start = i;
            while i < chars.len()
                && chars[i] != ':'
                && chars[i] != '#'
                && !chars[i].is_control()
            {
                if chars[i] == ':' && (i + 1 >= chars.len() || chars[i + 1].is_whitespace()) {
                    break;
                }
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            if i < chars.len() && chars[i] == ':' {
                spans.push(HighlightSpan {
                    text: token,
                    kind: TokenKind::FieldOrProperty,
                });
                continue;
            } else {
                let kind = if matches!(
                    token.trim(),
                    "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "True" | "False" | "None"
                ) {
                    TokenKind::Keyword
                } else {
                    TokenKind::String
                };
                spans.push(HighlightSpan { text: token, kind });
                continue;
            }
        }
        if seen_colon && (c.is_ascii_digit() || (c == '-' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit())) {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || matches!(chars[i], '.' | '_')) {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::Number,
            });
            continue;
        }
        if seen_colon && (c.is_ascii_alphabetic() || c == '_') {
            let start = i;
            while i < chars.len()
                && !chars[i].is_whitespace()
                && !matches!(chars[i], '#' | ',' | ']' | '}')
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let kind = if matches!(
                word.as_str(),
                "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "True" | "False" | "None"
            ) {
                TokenKind::Keyword
            } else {
                TokenKind::String
            };
            spans.push(HighlightSpan { text: word, kind });
            continue;
        }
        spans.push(HighlightSpan {
            text: c.to_string(),
            kind: if c == '-' && (i + 1 >= chars.len() || chars[i + 1].is_whitespace()) {
                TokenKind::Keyword
            } else {
                TokenKind::Plain
            },
        });
        i += 1;
    }
    spans
}

fn find_ascii_ci(haystack: &str, needle_lower: &str) -> Option<usize> {
    let h_bytes = haystack.as_bytes();
    let n_bytes = needle_lower.as_bytes();
    if n_bytes.is_empty() || h_bytes.len() < n_bytes.len() {
        return None;
    }
    h_bytes
        .windows(n_bytes.len())
        .position(|w| w.eq_ignore_ascii_case(n_bytes))
}

fn is_ts_lang_value(val: &str) -> bool {
    matches!(
        val.trim().to_ascii_lowercase().as_str(),
        "ts" | "tsx" | "typescript"
    )
}

fn highlight_sfc_or_html_line(
    line: &str,
    lang: LanguageId,
    state: &mut HighlightState,
) -> Vec<HighlightSpan> {
    let trimmed = line.trim();

    if lang == LanguageId::Astro
        && !state.astro_frontmatter_done
        && state.section == MarkupSection::Markup
        && !state.seen_non_empty
        && trimmed == "---"
    {
        state.seen_non_empty = true;
        state.section = MarkupSection::AstroFrontmatter;
        return vec![HighlightSpan {
            text: line.to_string(),
            kind: TokenKind::MacroOrDecorator,
        }];
    }

    if state.section == MarkupSection::AstroFrontmatter {
        if !state.in_block_comment && !state.in_template_string && trimmed == "---" {
            state.section = MarkupSection::Markup;
            state.astro_frontmatter_done = true;
            return vec![HighlightSpan {
                text: line.to_string(),
                kind: TokenKind::MacroOrDecorator,
            }];
        }
        return highlight_code_line_with_state(line, LanguageId::TypeScript, state);
    }

    if !trimmed.is_empty() {
        state.seen_non_empty = true;
    }

    if matches!(
        state.section,
        MarkupSection::ScriptTs | MarkupSection::ScriptJs
    ) {
        let script_lang = if state.section == MarkupSection::ScriptTs {
            LanguageId::TypeScript
        } else {
            LanguageId::JavaScript
        };
        if !state.in_block_comment && !state.in_template_string {
            if let Some(close_pos) = find_ascii_ci(line, "</script") {
                let mut spans = highlight_code_line_with_state(&line[..close_pos], script_lang, state);
                state.section = MarkupSection::Markup;
                state.in_block_comment = false;
                state.in_template_string = false;
                state.open_markup_tag = OpenMarkupTagKind::None;
                state.markup_attr_quote = MarkupAttrQuote::None;
                state.markup_last_attr_was_directive = false;
                state.markup_pending_lang_attr = false;
                spans.extend(highlight_sfc_or_html_line(&line[close_pos..], lang, state));
                return spans;
            }
        }
        return highlight_code_line_with_state(line, script_lang, state);
    }

    if state.section == MarkupSection::Style {
        if !state.in_block_comment {
            if let Some(close_pos) = find_ascii_ci(line, "</style") {
                let mut spans = highlight_css_line_with_state(&line[..close_pos], state);
                state.section = MarkupSection::Markup;
                state.in_block_comment = false;
                spans.extend(highlight_sfc_or_html_line(&line[close_pos..], lang, state));
                return spans;
            }
        }
        return highlight_css_line_with_state(line, state);
    }

    highlight_markup_line_with_state(line, lang, state)
}

fn highlight_markup_line_with_state(
    line: &str,
    lang: LanguageId,
    state: &mut HighlightState,
) -> Vec<HighlightSpan> {
    let mut spans = Vec::new();
    let mut rest = line;

    if state.in_html_comment {
        if let Some(end_rel) = rest.find("-->") {
            let end_idx = end_rel + 3;
            spans.push(HighlightSpan {
                text: rest[..end_idx].to_string(),
                kind: TokenKind::Comment,
            });
            state.in_html_comment = false;
            rest = &rest[end_idx..];
            if rest.is_empty() {
                return spans;
            }
        } else {
            return vec![HighlightSpan {
                text: line.to_string(),
                kind: TokenKind::Comment,
            }];
        }
    }

    let chars: Vec<char> = rest.chars().collect();
    let mut i = 0;

    if state.open_markup_tag != OpenMarkupTagKind::None {
        let (next_i, entered_section) =
            consume_markup_tag_body(&chars, 0, lang, state, &mut spans);
        i = next_i;
        if entered_section && i < chars.len() {
            let tail: String = chars[i..].iter().collect();
            spans.extend(highlight_sfc_or_html_line(&tail, lang, state));
            return spans;
        }
    }

    if state.in_mustache && matches!(lang, LanguageId::Vue | LanguageId::Html) {
        let expr_start = i;
        while i + 1 < chars.len() && !(chars[i] == '}' && chars[i + 1] == '}') {
            i += 1;
        }
        if i + 1 < chars.len() && chars[i] == '}' && chars[i + 1] == '}' {
            let expr: String = chars[expr_start..i].iter().collect();
            if !expr.is_empty() {
                spans.extend(highlight_inline_js_expr(&expr));
            }
            spans.push(HighlightSpan {
                text: "}}".to_string(),
                kind: TokenKind::MacroOrDecorator,
            });
            state.in_mustache = false;
            i += 2;
        } else {
            let expr: String = chars[expr_start..].iter().collect();
            if !expr.is_empty() {
                spans.extend(highlight_inline_js_expr(&expr));
            }
            return spans;
        }
    }

    if state.brace_expr_depth > 0 && matches!(lang, LanguageId::Svelte | LanguageId::Astro) {
        let expr_start = i;
        let mut depth = state.brace_expr_depth as usize;
        while i < chars.len() && depth > 0 {
            if chars[i] == '{' {
                depth += 1;
            } else if chars[i] == '}' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            i += 1;
        }
        let expr: String = chars[expr_start..i].iter().collect();
        if !expr.is_empty() {
            spans.extend(highlight_inline_js_expr(&expr));
        }
        if depth == 0 && i < chars.len() && chars[i] == '}' {
            spans.push(HighlightSpan {
                text: "}".to_string(),
                kind: TokenKind::MacroOrDecorator,
            });
            state.brace_expr_depth = 0;
            i += 1;
        } else {
            state.brace_expr_depth = depth as u16;
            return spans;
        }
    }

    while i < chars.len() {
        // HTML/XML comment <!-- ... -->
        if i + 3 < chars.len()
            && chars[i] == '<'
            && chars[i + 1] == '!'
            && chars[i + 2] == '-'
            && chars[i + 3] == '-'
        {
            let start = i;
            i += 4;
            let mut closed = false;
            while i + 2 < chars.len() {
                if chars[i] == '-' && chars[i + 1] == '-' && chars[i + 2] == '>' {
                    i += 3;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                i = chars.len();
                state.in_html_comment = true;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::Comment,
            });
            continue;
        }
        // <!DOCTYPE ...> or <?xml ...?>
        if i + 1 < chars.len() && chars[i] == '<' && (chars[i + 1] == '!' || chars[i + 1] == '?')
        {
            let start = i;
            while i < chars.len() && chars[i] != '>' {
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::MacroOrDecorator,
            });
            continue;
        }
        // Tag `<tag attr="val">` or `</tag>`
        if chars[i] == '<'
            && i + 1 < chars.len()
            && (chars[i + 1].is_ascii_alphabetic() || chars[i + 1] == '/' || chars[i + 1] == '>')
        {
            let (next_i, entered_section) =
                consume_markup_tag(&chars, i, lang, state, &mut spans);
            i = next_i;
            if entered_section && i < chars.len() {
                let tail: String = chars[i..].iter().collect();
                spans.extend(highlight_sfc_or_html_line(&tail, lang, state));
                break;
            }
            continue;
        }

        // Vue / HTML mustache interpolation `{{ expr }}`
        if i + 1 < chars.len()
            && chars[i] == '{'
            && chars[i + 1] == '{'
            && matches!(lang, LanguageId::Vue | LanguageId::Html)
        {
            spans.push(HighlightSpan {
                text: "{{".to_string(),
                kind: TokenKind::MacroOrDecorator,
            });
            i += 2;
            let expr_start = i;
            while i + 1 < chars.len() && !(chars[i] == '}' && chars[i + 1] == '}') {
                i += 1;
            }
            if i + 1 < chars.len() && chars[i] == '}' && chars[i + 1] == '}' {
                let expr: String = chars[expr_start..i].iter().collect();
                if !expr.is_empty() {
                    spans.extend(highlight_inline_js_expr(&expr));
                }
                spans.push(HighlightSpan {
                    text: "}}".to_string(),
                    kind: TokenKind::MacroOrDecorator,
                });
                i += 2;
            } else {
                let expr: String = chars[expr_start..].iter().collect();
                if !expr.is_empty() {
                    spans.extend(highlight_inline_js_expr(&expr));
                }
                state.in_mustache = true;
                break;
            }
            continue;
        }

        // Svelte / Astro template interpolation & control blocks `{#if ...}`, `{:else}`, `{/if}`, `{@html ...}`, `{expr}`
        if chars[i] == '{' && matches!(lang, LanguageId::Svelte | LanguageId::Astro) {
            spans.push(HighlightSpan {
                text: "{".to_string(),
                kind: TokenKind::MacroOrDecorator,
            });
            i += 1;
            if lang == LanguageId::Svelte
                && i < chars.len()
                && matches!(chars[i], '#' | ':' | '/' | '@')
            {
                let tag_start = i;
                i += 1;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                spans.push(HighlightSpan {
                    text: chars[tag_start..i].iter().collect(),
                    kind: TokenKind::Keyword,
                });
            }
            let expr_start = i;
            let mut depth = 1usize;
            while i < chars.len() && depth > 0 {
                if chars[i] == '{' {
                    depth += 1;
                } else if chars[i] == '}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                i += 1;
            }
            let expr: String = chars[expr_start..i].iter().collect();
            if !expr.is_empty() {
                spans.extend(highlight_inline_js_expr(&expr));
            }
            if depth == 0 && i < chars.len() && chars[i] == '}' {
                spans.push(HighlightSpan {
                    text: "}".to_string(),
                    kind: TokenKind::MacroOrDecorator,
                });
                i += 1;
            } else {
                state.brace_expr_depth = depth as u16;
                break;
            }
            continue;
        }

        let start = i;
        i += 1;
        while i < chars.len()
            && chars[i] != '<'
            && !(chars[i] == '{' && matches!(lang, LanguageId::Vue | LanguageId::Svelte | LanguageId::Astro | LanguageId::Html))
        {
            i += 1;
        }
        spans.push(HighlightSpan {
            text: chars[start..i].iter().collect(),
            kind: TokenKind::Plain,
        });
    }
    spans
}

fn is_framework_directive_attr(attr: &str) -> bool {
    attr.starts_with('@')
        || attr.starts_with(':')
        || attr.starts_with('#')
        || attr.starts_with("v-")
        || attr.starts_with("on:")
        || attr.starts_with("bind:")
        || attr.starts_with("class:")
        || attr.starts_with("style:")
        || attr.starts_with("use:")
        || attr.starts_with("transition:")
        || attr.starts_with("in:")
        || attr.starts_with("out:")
        || attr.starts_with("animate:")
        || attr.starts_with("let:")
        || attr.starts_with("client:")
        || attr.starts_with("set:")
        || attr.starts_with("is:")
        || attr.starts_with("define:")
}

fn consume_markup_tag(
    chars: &[char],
    mut i: usize,
    lang: LanguageId,
    state: &mut HighlightState,
    spans: &mut Vec<HighlightSpan>,
) -> (usize, bool) {
    let open_start = i;
    i += 1;
    let is_closing = i < chars.len() && chars[i] == '/';
    if is_closing {
        i += 1;
    }
    spans.push(HighlightSpan {
        text: chars[open_start..i].iter().collect(),
        kind: TokenKind::Keyword,
    });
    let tag_start = i;
    while i < chars.len()
        && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '-' | '_' | ':' | '.'))
    {
        i += 1;
    }
    let mut open_kind = OpenMarkupTagKind::Generic;
    if i > tag_start {
        let tag_name: String = chars[tag_start..i].iter().collect();
        if !is_closing && matches!(lang, LanguageId::Vue | LanguageId::Svelte | LanguageId::Astro | LanguageId::Html) {
            if tag_name.eq_ignore_ascii_case("script") {
                open_kind = OpenMarkupTagKind::ScriptJs;
            } else if tag_name.eq_ignore_ascii_case("style") {
                open_kind = OpenMarkupTagKind::Style;
            }
        }
        let is_component = tag_name
            .chars()
            .next()
            .map(|ch| ch.is_ascii_uppercase())
            .unwrap_or(false)
            || tag_name.contains('.')
            || tag_name.contains('-')
            || tag_name.starts_with("svelte:")
            || matches!(
                tag_name.as_str(),
                "slot" | "component" | "transition" | "transition-group" | "keep-alive" | "teleport" | "suspense" | "Fragment"
            );
        spans.push(HighlightSpan {
            text: tag_name,
            kind: if is_component {
                TokenKind::Type
            } else {
                TokenKind::Keyword
            },
        });
    }
    state.open_markup_tag = open_kind;
    state.markup_attr_quote = MarkupAttrQuote::None;
    state.markup_last_attr_was_directive = false;
    state.markup_pending_lang_attr = false;
    consume_markup_tag_body(chars, i, lang, state, spans)
}

fn consume_markup_quoted_attr(
    chars: &[char],
    mut i: usize,
    quote: char,
    include_open_quote: bool,
    is_directive: bool,
    state: &mut HighlightState,
    spans: &mut Vec<HighlightSpan>,
) -> usize {
    let open_idx = i;
    if include_open_quote {
        i += 1;
    }
    let inner_start = i;
    while i < chars.len() && chars[i] != quote {
        if chars[i] == '\\' {
            i += 1;
        }
        if i < chars.len() {
            i += 1;
        }
    }
    let inner_end = i;
    let closed = i < chars.len() && chars[i] == quote;
    if closed {
        i += 1;
    }
    let inner: String = chars[inner_start..inner_end].iter().collect();
    if state.markup_pending_lang_attr
        && state.open_markup_tag == OpenMarkupTagKind::ScriptJs
        && is_ts_lang_value(&inner)
    {
        state.open_markup_tag = OpenMarkupTagKind::ScriptTs;
    }

    if is_directive {
        if include_open_quote {
            spans.push(HighlightSpan {
                text: quote.to_string(),
                kind: TokenKind::String,
            });
        }
        if !inner.is_empty() {
            spans.extend(highlight_inline_js_expr(&inner));
        }
        if closed {
            spans.push(HighlightSpan {
                text: quote.to_string(),
                kind: TokenKind::String,
            });
        }
    } else if i > open_idx {
        spans.push(HighlightSpan {
            text: chars[open_idx..i].iter().collect(),
            kind: TokenKind::String,
        });
    }

    if closed {
        state.markup_attr_quote = MarkupAttrQuote::None;
        state.markup_last_attr_was_directive = false;
        state.markup_pending_lang_attr = false;
    } else {
        state.markup_attr_quote = match (quote, is_directive) {
            ('"', true) => MarkupAttrQuote::DirectiveDouble,
            ('\'', true) => MarkupAttrQuote::DirectiveSingle,
            ('"', false) => MarkupAttrQuote::Double,
            _ => MarkupAttrQuote::Single,
        };
    }
    i
}

fn consume_markup_tag_body(
    chars: &[char],
    mut i: usize,
    lang: LanguageId,
    state: &mut HighlightState,
    spans: &mut Vec<HighlightSpan>,
) -> (usize, bool) {
    if state.markup_attr_quote != MarkupAttrQuote::None {
        let (quote, is_directive) = match state.markup_attr_quote {
            MarkupAttrQuote::Double => ('"', false),
            MarkupAttrQuote::Single => ('\'', false),
            MarkupAttrQuote::DirectiveDouble => ('"', true),
            MarkupAttrQuote::DirectiveSingle => ('\'', true),
            MarkupAttrQuote::None => ('"', false),
        };
        i = consume_markup_quoted_attr(chars, i, quote, false, is_directive, state, spans);
        if state.markup_attr_quote != MarkupAttrQuote::None {
            return (i, false);
        }
    }

    let mut last_non_ws_was_slash = false;
    while i < chars.len() && chars[i] != '>' {
        if chars[i].is_whitespace() {
            let ws_start = i;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[ws_start..i].iter().collect(),
                kind: TokenKind::Plain,
            });
            continue;
        }
        // JSX / Svelte / Astro attribute expression `{...}`
        if chars[i] == '{' {
            spans.push(HighlightSpan {
                text: "{".to_string(),
                kind: TokenKind::MacroOrDecorator,
            });
            i += 1;
            let expr_start = i;
            let mut depth = 1usize;
            while i < chars.len() && depth > 0 {
                if chars[i] == '{' {
                    depth += 1;
                } else if chars[i] == '}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                i += 1;
            }
            let expr: String = chars[expr_start..i].iter().collect();
            if !expr.is_empty() {
                spans.extend(highlight_inline_js_expr(&expr));
            }
            if i < chars.len() && chars[i] == '}' {
                spans.push(HighlightSpan {
                    text: "}".to_string(),
                    kind: TokenKind::MacroOrDecorator,
                });
                i += 1;
            }
            state.markup_last_attr_was_directive = false;
            state.markup_pending_lang_attr = false;
            last_non_ws_was_slash = false;
            continue;
        }
        if chars[i] == '"' || chars[i] == '\'' {
            let quote = chars[i];
            let is_dir = state.markup_last_attr_was_directive
                && matches!(
                    lang,
                    LanguageId::Vue
                        | LanguageId::Svelte
                        | LanguageId::Astro
                        | LanguageId::TypeScript
                        | LanguageId::JavaScript
                );
            i = consume_markup_quoted_attr(chars, i, quote, true, is_dir, state, spans);
            last_non_ws_was_slash = false;
            if state.markup_attr_quote != MarkupAttrQuote::None {
                return (i, false);
            }
            continue;
        }
        if chars[i].is_ascii_alphabetic() || matches!(chars[i], '@' | ':' | '#' | '_' | '$') {
            let a_start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric()
                    || matches!(chars[i], '-' | '_' | ':' | '@' | '#' | '.' | '[' | ']'))
            {
                i += 1;
            }
            let attr: String = chars[a_start..i].iter().collect();
            let is_dir = is_framework_directive_attr(&attr);
            state.markup_last_attr_was_directive = is_dir;
            state.markup_pending_lang_attr = attr.eq_ignore_ascii_case("lang");
            last_non_ws_was_slash = false;
            spans.push(HighlightSpan {
                text: attr,
                kind: if is_dir {
                    TokenKind::MacroOrDecorator
                } else {
                    TokenKind::FieldOrProperty
                },
            });
            continue;
        }
        if chars[i] != '=' {
            state.markup_last_attr_was_directive = false;
            state.markup_pending_lang_attr = false;
        }
        last_non_ws_was_slash = chars[i] == '/';
        spans.push(HighlightSpan {
            text: chars[i].to_string(),
            kind: if chars[i] == '/' {
                TokenKind::Keyword
            } else {
                TokenKind::Plain
            },
        });
        i += 1;
    }

    let mut entered_section = false;
    if i < chars.len() && chars[i] == '>' {
        spans.push(HighlightSpan {
            text: ">".to_string(),
            kind: TokenKind::Keyword,
        });
        i += 1;
        if !last_non_ws_was_slash && lang != LanguageId::Xml {
            match state.open_markup_tag {
                OpenMarkupTagKind::ScriptTs => {
                    state.section = MarkupSection::ScriptTs;
                    entered_section = true;
                }
                OpenMarkupTagKind::ScriptJs => {
                    state.section = MarkupSection::ScriptJs;
                    entered_section = true;
                }
                OpenMarkupTagKind::Style => {
                    state.section = MarkupSection::Style;
                    entered_section = true;
                }
                OpenMarkupTagKind::Generic | OpenMarkupTagKind::None => {}
            }
        }
        state.open_markup_tag = OpenMarkupTagKind::None;
        state.markup_last_attr_was_directive = false;
        state.markup_pending_lang_attr = false;
    }
    (i, entered_section)
}

fn highlight_css_line_with_state(line: &str, state: &mut HighlightState) -> Vec<HighlightSpan> {
    let mut spans = Vec::new();
    let mut rest = line;
    if state.in_block_comment {
        if let Some(end_rel) = rest.find("*/") {
            let end_idx = end_rel + 2;
            spans.push(HighlightSpan {
                text: rest[..end_idx].to_string(),
                kind: TokenKind::Comment,
            });
            state.in_block_comment = false;
            rest = &rest[end_idx..];
            if rest.is_empty() {
                return spans;
            }
        } else {
            return vec![HighlightSpan {
                text: line.to_string(),
                kind: TokenKind::Comment,
            }];
        }
    }

    let trimmed = rest.trim_start();
    if trimmed.starts_with("//") || is_block_comment_continuation(trimmed, LanguageId::Css) {
        if trimmed.starts_with("/*") && !trimmed.contains("*/") {
            state.in_block_comment = true;
        }
        spans.push(HighlightSpan {
            text: rest.to_string(),
            kind: TokenKind::Comment,
        });
        return spans;
    }
    let chars: Vec<char> = rest.chars().collect();
    let mut i = 0;
    let has_colon_no_brace = rest.contains(':') && !rest.contains('{');
    let mut seen_colon = false;

    while i < chars.len() {
        let c = chars[i];
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            let mut closed = false;
            while i + 1 < chars.len() {
                if chars[i] == '*' && chars[i + 1] == '/' {
                    i += 2;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                i = chars.len();
                state.in_block_comment = true;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::Comment,
            });
            continue;
        }
        if (c == '@' || c == '$')
            && i + 1 < chars.len()
            && (chars[i + 1].is_ascii_alphabetic() || chars[i + 1] == '-')
        {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '-' | '_')) {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::MacroOrDecorator,
            });
            continue;
        }
        if c == '"' || c == '\'' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::String,
            });
            continue;
        }
        if c == ':' {
            // Vue/CSS pseudo-selectors like :deep(), :slotted(), :global(), :hover
            if i + 1 < chars.len() && chars[i + 1].is_ascii_alphabetic() && (!seen_colon || !has_colon_no_brace) {
                let start = i;
                i += 1;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '-' | '_')) {
                    i += 1;
                }
                let pseudo: String = chars[start..i].iter().collect();
                let kind = if matches!(pseudo.as_str(), ":deep" | ":slotted" | ":global") {
                    TokenKind::MacroOrDecorator
                } else {
                    TokenKind::Keyword
                };
                spans.push(HighlightSpan { text: pseudo, kind });
                continue;
            }
            seen_colon = true;
            spans.push(HighlightSpan {
                text: ":".to_string(),
                kind: TokenKind::Plain,
            });
            i += 1;
            continue;
        }
        // Hex color `#1E1F22` vs ID selector `#app`
        if c == '#' && i + 1 < chars.len() && chars[i + 1].is_ascii_alphanumeric() {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '-' | '_')) {
                i += 1;
            }
            let tok: String = chars[start..i].iter().collect();
            let is_hex = seen_colon && tok[1..].chars().all(|ch| ch.is_ascii_hexdigit());
            spans.push(HighlightSpan {
                text: tok,
                kind: if is_hex {
                    TokenKind::Number
                } else {
                    TokenKind::Function
                },
            });
            continue;
        }
        if c == '.' && !seen_colon && i + 1 < chars.len() && chars[i + 1].is_ascii_alphabetic() {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '-' | '_')) {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::Type,
            });
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '.' | '%'))
            {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::Number,
            });
            continue;
        }
        if c.is_ascii_alphabetic() || c == '-' || c == '!' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '-' | '_')) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let kind = if word == "v-bind" {
                TokenKind::MacroOrDecorator
            } else if !seen_colon && has_colon_no_brace {
                TokenKind::FieldOrProperty
            } else if word == "!important" || CSS_KEYWORDS.contains(&word.as_str()) {
                TokenKind::Keyword
            } else if i < chars.len() && chars[i] == '(' {
                TokenKind::Function
            } else if !seen_colon {
                TokenKind::Keyword
            } else {
                TokenKind::Plain
            };
            spans.push(HighlightSpan { text: word, kind });
            continue;
        }
        spans.push(HighlightSpan {
            text: c.to_string(),
            kind: TokenKind::Plain,
        });
        i += 1;
    }
    spans
}

fn highlight_sql_line(line: &str) -> Vec<HighlightSpan> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("--") {
        return vec![HighlightSpan {
            text: line.to_string(),
            kind: TokenKind::Comment,
        }];
    }
    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c == '-' && i + 1 < chars.len() && chars[i + 1] == '-' {
            spans.push(HighlightSpan {
                text: chars[i..].iter().collect(),
                kind: TokenKind::Comment,
            });
            break;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            if i + 1 < chars.len() {
                i += 2;
            } else {
                i = chars.len();
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::Comment,
            });
            continue;
        }
        if c == '\'' || c == '"' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: if quote == '\'' {
                    TokenKind::String
                } else {
                    TokenKind::FieldOrProperty
                },
            });
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let upper = word.to_ascii_uppercase();
            let kind = if SQL_KEYWORDS.contains(&upper.as_str()) {
                TokenKind::Keyword
            } else if SQL_TYPES.contains(&upper.as_str()) {
                TokenKind::Type
            } else if i < chars.len() && chars[i] == '(' {
                TokenKind::Function
            } else {
                TokenKind::Plain
            };
            spans.push(HighlightSpan { text: word, kind });
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::Number,
            });
            continue;
        }
        spans.push(HighlightSpan {
            text: c.to_string(),
            kind: TokenKind::Plain,
        });
        i += 1;
    }
    spans
}

fn highlight_markdown_line(line: &str) -> Vec<HighlightSpan> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return vec![HighlightSpan {
            text: line.to_string(),
            kind: TokenKind::Keyword,
        }];
    }
    if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
        return vec![HighlightSpan {
            text: line.to_string(),
            kind: TokenKind::MacroOrDecorator,
        }];
    }
    if trimmed.starts_with('>') {
        return vec![HighlightSpan {
            text: line.to_string(),
            kind: TokenKind::Comment,
        }];
    }

    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        // Inline code `...`
        if c == '`' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != '`' {
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::String,
            });
            continue;
        }
        // Bold **...**
        if c == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '*') {
                i += 1;
            }
            if i + 1 < chars.len() {
                i += 2;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::Type,
            });
            continue;
        }
        // Markdown link [label](url)
        if c == '[' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != ']' {
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            spans.push(HighlightSpan {
                text: chars[start..i].iter().collect(),
                kind: TokenKind::Function,
            });
            if i < chars.len() && chars[i] == '(' {
                let u_start = i;
                i += 1;
                while i < chars.len() && chars[i] != ')' {
                    i += 1;
                }
                if i < chars.len() {
                    i += 1;
                }
                spans.push(HighlightSpan {
                    text: chars[u_start..i].iter().collect(),
                    kind: TokenKind::String,
                });
            }
            continue;
        }
        let start = i;
        i += 1;
        while i < chars.len() && !matches!(chars[i], '`' | '*' | '[') {
            i += 1;
        }
        spans.push(HighlightSpan {
            text: chars[start..i].iter().collect(),
            kind: TokenKind::Plain,
        });
    }
    spans
}

/// 提取文件的 Structure 符号大纲（用于 IDEA 左侧 Structure 面板、顶部 Breadcrumbs 面包屑导航及全局符号索引）
pub fn extract_structure_symbols(source: &str, lang: LanguageId) -> Vec<StructureSymbol> {
    let mut symbols = Vec::new();
    let mut in_sfc_script = false;
    let mut in_open_script_tag = false;
    let mut open_script_sym_idx: Option<usize> = None;
    let mut in_astro_frontmatter = false;
    let mut astro_fm_done = false;
    let mut seen_non_empty = false;

    for (line_idx, raw_line) in source.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if lang == LanguageId::Astro && !astro_fm_done {
            if !seen_non_empty && line == "---" {
                seen_non_empty = true;
                in_astro_frontmatter = true;
                continue;
            }
            if in_astro_frontmatter {
                if line == "---" {
                    in_astro_frontmatter = false;
                    astro_fm_done = true;
                } else if let Some(sym) = extract_ts_js_structure_symbol(line, line_idx) {
                    symbols.push(sym);
                }
                continue;
            }
        }
        seen_non_empty = true;
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
                if let Some(sym) = extract_ts_js_structure_symbol(line, line_idx) {
                    symbols.push(sym);
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
                } else if !raw_line.starts_with(' ') && !raw_line.starts_with('\t') {
                    if let Some((lhs, _)) = line.split_once('=') {
                        let name = lhs.split(':').next().unwrap_or("").trim();
                        if !name.is_empty()
                            && name
                                .chars()
                                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                        {
                            symbols.push(StructureSymbol {
                                name: name.to_string(),
                                kind: SymbolKind::Constant,
                                line: line_idx,
                            });
                        }
                    }
                }
            }
            LanguageId::Java | LanguageId::CSharp => {
                if let Some(sym) = extract_java_or_csharp_structure_symbol(line, line_idx) {
                    symbols.push(sym);
                }
            }
            LanguageId::Go => {
                if let Some(sym) = extract_go_structure_symbol(line, line_idx) {
                    symbols.push(sym);
                }
            }
            LanguageId::Kotlin => {
                if let Some(sym) = extract_kotlin_structure_symbol(line, line_idx) {
                    symbols.push(sym);
                }
            }
            LanguageId::Dart => {
                if let Some(sym) = extract_dart_structure_symbol(line, line_idx) {
                    symbols.push(sym);
                }
            }
            LanguageId::C | LanguageId::Cpp => {
                if let Some(sym) = extract_c_cpp_structure_symbol(line, line_idx) {
                    symbols.push(sym);
                }
            }
            LanguageId::Markdown => {
                if line.starts_with('#') {
                    let title = line.trim_start_matches('#').trim();
                    if !title.is_empty() {
                        symbols.push(StructureSymbol {
                            name: title.to_string(),
                            kind: SymbolKind::Struct,
                            line: line_idx,
                        });
                    }
                }
            }
            LanguageId::Toml => {
                if line.starts_with('[') && line.ends_with(']') {
                    let section = line.trim_matches(['[', ']']).trim();
                    if !section.is_empty() {
                        symbols.push(StructureSymbol {
                            name: section.to_string(),
                            kind: SymbolKind::Struct,
                            line: line_idx,
                        });
                    }
                } else if !raw_line.starts_with(' ') && !raw_line.starts_with('\t') {
                    if let Some((k, _)) = line.split_once('=') {
                        let key = k.trim().trim_matches('"');
                        if !key.is_empty() && !key.starts_with('#') {
                            symbols.push(StructureSymbol {
                                name: key.to_string(),
                                kind: SymbolKind::Constant,
                                line: line_idx,
                            });
                        }
                    }
                }
            }
            LanguageId::Yaml => {
                if !line.starts_with('#') && !line.starts_with('-') {
                    if let Some((k, _)) = line.split_once(':') {
                        let indent = raw_line.len() - raw_line.trim_start().len();
                        let key = k.trim().trim_matches(['"', '\'']);
                        if indent <= 2 && !key.is_empty() && !key.contains(' ') {
                            symbols.push(StructureSymbol {
                                name: key.to_string(),
                                kind: if indent == 0 {
                                    SymbolKind::Struct
                                } else {
                                    SymbolKind::Constant
                                },
                                line: line_idx,
                            });
                        }
                    }
                }
            }
            LanguageId::Json => {
                if line.starts_with('"') {
                    if let Some((k, _)) = line.split_once(':') {
                        let indent = raw_line.len() - raw_line.trim_start().len();
                        let key = k.trim().trim_matches('"');
                        if indent <= 4 && !key.is_empty() {
                            symbols.push(StructureSymbol {
                                name: key.to_string(),
                                kind: SymbolKind::Constant,
                                line: line_idx,
                            });
                        }
                    }
                }
            }
            LanguageId::Sql => {
                let upper = line.to_ascii_uppercase();
                for (prefix, kind) in [
                    ("CREATE TABLE ", SymbolKind::Struct),
                    ("CREATE TEMP TABLE ", SymbolKind::Struct),
                    ("CREATE VIEW ", SymbolKind::Interface),
                    ("CREATE OR REPLACE VIEW ", SymbolKind::Interface),
                    ("CREATE FUNCTION ", SymbolKind::Function),
                    ("CREATE OR REPLACE FUNCTION ", SymbolKind::Function),
                    ("CREATE PROCEDURE ", SymbolKind::Function),
                    ("CREATE INDEX ", SymbolKind::Constant),
                    ("CREATE UNIQUE INDEX ", SymbolKind::Constant),
                ] {
                    if let Some(after) = upper.strip_prefix(prefix) {
                        let offset = prefix.len();
                        let orig_after = &line[offset..];
                        let clean = if after.starts_with("IF NOT EXISTS ") {
                            &orig_after["IF NOT EXISTS ".len()..]
                        } else {
                            orig_after
                        };
                        if let Some(name) = clean.split(['(', ' ', ';']).next() {
                            let n = name.trim().trim_matches(['"', '`', '[', ']']);
                            if !n.is_empty() {
                                symbols.push(StructureSymbol {
                                    name: n.to_string(),
                                    kind,
                                    line: line_idx,
                                });
                            }
                        }
                        break;
                    }
                }
            }
            LanguageId::Shell => {
                if let Some(rest) = line.strip_prefix("function ") {
                    if let Some(name) = rest.split(['(', '{', ' ']).next() {
                        if !name.trim().is_empty() {
                            symbols.push(StructureSymbol {
                                name: name.trim().to_string(),
                                kind: SymbolKind::Function,
                                line: line_idx,
                            });
                        }
                    }
                } else if let Some((before_paren, _)) = line.split_once("()") {
                    let name = before_paren.trim();
                    if !name.is_empty()
                        && name
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                    {
                        symbols.push(StructureSymbol {
                            name: name.to_string(),
                            kind: SymbolKind::Function,
                            line: line_idx,
                        });
                    }
                }
            }
            LanguageId::Css => {
                if line.ends_with('{') && !line.starts_with("//") && !line.starts_with("/*") {
                    let sel = line.trim_end_matches('{').trim();
                    if !sel.is_empty() {
                        symbols.push(StructureSymbol {
                            name: sel.to_string(),
                            kind: if sel.starts_with('@') {
                                SymbolKind::Interface
                            } else {
                                SymbolKind::Class
                            },
                            line: line_idx,
                        });
                    }
                }
            }
            LanguageId::Vue | LanguageId::Svelte | LanguageId::Astro | LanguageId::Html | LanguageId::Xml => {
                if in_open_script_tag {
                    let lower = line.to_ascii_lowercase();
                    if lower.contains("setup") {
                        if let Some(idx) = open_script_sym_idx {
                            symbols[idx].name = "script setup".to_string();
                        }
                    }
                    if lower.contains('>') {
                        in_open_script_tag = false;
                        open_script_sym_idx = None;
                        if !lower.contains("</script") && !lower.trim_end().ends_with("/>") {
                            in_sfc_script = true;
                        }
                    }
                    continue;
                }
                if in_sfc_script {
                    if find_ascii_ci(line, "</script").is_some() {
                        in_sfc_script = false;
                    } else if let Some(sym) = extract_ts_js_structure_symbol(line, line_idx) {
                        symbols.push(sym);
                    }
                    continue;
                }
                let lower = line.to_ascii_lowercase();
                if lower == "<script"
                    || (lower.starts_with("<script")
                        && lower
                            .as_bytes()
                            .get(7)
                            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>'))
                {
                    let label = if lower.contains("setup") {
                        "script setup"
                    } else {
                        "script"
                    };
                    let sym_idx = symbols.len();
                    symbols.push(StructureSymbol {
                        name: label.to_string(),
                        kind: SymbolKind::Struct,
                        line: line_idx,
                    });
                    if !lower.contains('>') {
                        in_open_script_tag = true;
                        open_script_sym_idx = Some(sym_idx);
                    } else if !lower.contains("</script") && !lower.trim_end().ends_with("/>") {
                        in_sfc_script = true;
                    }
                } else {
                    for tag in ["<template", "<style"] {
                        if lower == tag
                            || (lower.starts_with(tag)
                                && lower
                                    .as_bytes()
                                    .get(tag.len())
                                    .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>'))
                        {
                            symbols.push(StructureSymbol {
                                name: tag.trim_start_matches('<').to_string(),
                                kind: SymbolKind::Struct,
                                line: line_idx,
                            });
                        }
                    }
                }
                if let Some(id_pos) = line.find("id=\"") {
                    let after = &line[id_pos + 4..];
                    if let Some(end_q) = after.find('"') {
                        let id_val = &after[..end_q];
                        if !id_val.is_empty() {
                            symbols.push(StructureSymbol {
                                name: format!("#{id_val}"),
                                kind: SymbolKind::Constant,
                                line: line_idx,
                            });
                        }
                    }
                }
            }
            LanguageId::PlainText => {}
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

    // 1. 优先匹配顶层与成员结构符号（fn / struct / enum / trait / const / class / interface / def / func / fun）
    for sym in extract_structure_symbols(source, lang) {
        if sym.name == target || sym.name.trim_start_matches('#') == target {
            let col = source
                .lines()
                .nth(sym.line)
                .and_then(|l| l.find(target).map(|b| l[..b].chars().count()))
                .unwrap_or(0);
            return Some((sym.line, col));
        }
    }

    // 2. 匹配 type alias、mod、局部变量绑定 (let / const / var / val / := / 显式类型变量定义)
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
        LanguageId::TypeScript
        | LanguageId::JavaScript
        | LanguageId::Vue
        | LanguageId::Svelte
        | LanguageId::Astro => &[
            "export const ",
            "export let ",
            "const ",
            "let ",
            "var ",
        ],
        LanguageId::Go => &["var ", "const ", "type "],
        LanguageId::Kotlin => &["private val ", "private var ", "val ", "var ", "const val "],
        LanguageId::Dart => &["final ", "const ", "var ", "late final ", "late var "],
        _ => &[],
    };

    for (line_idx, raw_line) in source.lines().enumerate() {
        let trimmed = raw_line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with("--") {
            continue;
        }
        if lang == LanguageId::Python {
            if let Some(rest) = trimmed.strip_prefix(target) {
                let after = rest.trim_start();
                if (after.starts_with('=') && !after.starts_with("==")) || after.starts_with(':') {
                    let col = raw_line.find(target).map(|b| raw_line[..b].chars().count()).unwrap_or(0);
                    return Some((line_idx, col));
                }
            }
            continue;
        }
        if lang == LanguageId::Go {
            if let Some(rest) = trimmed.strip_prefix(target) {
                if rest.trim_start().starts_with(":=") {
                    let col = raw_line.find(target).map(|b| raw_line[..b].chars().count()).unwrap_or(0);
                    return Some((line_idx, col));
                }
            }
        }
        if matches!(
            lang,
            LanguageId::Java
                | LanguageId::CSharp
                | LanguageId::C
                | LanguageId::Cpp
                | LanguageId::Dart
        ) {
            if let Some((lhs, _)) = trimmed.split_once(['=', ';']) {
                let lhs_clean = lhs.trim();
                let mut tokens = lhs_clean.split_whitespace();
                let last_tok = tokens
                    .next_back()
                    .unwrap_or("")
                    .trim_start_matches(['*', '&']);
                let has_type_prefix = tokens.next().is_some();
                if has_type_prefix && last_tok == target {
                    let col = raw_line.find(target).map(|b| raw_line[..b].chars().count()).unwrap_or(0);
                    return Some((line_idx, col));
                }
            }
        }
        for prefix in prefixes {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let ident: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if ident == target {
                    let col = raw_line.find(target).map(|b| raw_line[..b].chars().count()).unwrap_or(0);
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
                if trimmed.starts_with("fn main(")
                    || trimmed.starts_with("pub fn main(")
                    || trimmed.starts_with("async fn main(")
                {
                    entries.push(RunnableGutterEntry {
                        line: idx,
                        label: "Run 'main'".to_string(),
                        command: "cargo".to_string(),
                        args: vec!["run".to_string()],
                    });
                } else if trimmed == "#[test]" || trimmed == "#[tokio::test]" {
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
                } else if trimmed.starts_with("def test_") || trimmed.starts_with("async def test_")
                {
                    entries.push(RunnableGutterEntry {
                        line: idx,
                        label: "Run Pytest".to_string(),
                        command: "pytest".to_string(),
                        args: vec![],
                    });
                }
            }
            LanguageId::TypeScript | LanguageId::JavaScript => {
                if trimmed.starts_with("describe(")
                    || trimmed.starts_with("test(")
                    || trimmed.starts_with("it(")
                {
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
            LanguageId::Go => {
                if trimmed.starts_with("func main(") {
                    entries.push(RunnableGutterEntry {
                        line: idx,
                        label: "Run 'go run'".to_string(),
                        command: "go".to_string(),
                        args: vec!["run".to_string(), ".".to_string()],
                    });
                } else if let Some(rest) = trimmed.strip_prefix("func Test") {
                    if let Some(fn_suffix) = rest.split('(').next() {
                        let full_name = format!("Test{}", fn_suffix.trim());
                        entries.push(RunnableGutterEntry {
                            line: idx,
                            label: format!("Run '{full_name}'"),
                            command: "go".to_string(),
                            args: vec![
                                "test".to_string(),
                                "-v".to_string(),
                                "-run".to_string(),
                                format!("^{full_name}$"),
                            ],
                        });
                    }
                }
            }
            LanguageId::Kotlin => {
                if trimmed.starts_with("fun main(") || trimmed.starts_with("suspend fun main(") {
                    entries.push(RunnableGutterEntry {
                        line: idx,
                        label: "Run 'main()'".to_string(),
                        command: "gradle".to_string(),
                        args: vec!["run".to_string()],
                    });
                } else if trimmed == "@Test" || trimmed.starts_with("@Test(") {
                    entries.push(RunnableGutterEntry {
                        line: (idx + 1).min(lines.len().saturating_sub(1)),
                        label: "Run Kotlin Test".to_string(),
                        command: "gradle".to_string(),
                        args: vec!["test".to_string()],
                    });
                }
            }
            LanguageId::Dart => {
                if trimmed.starts_with("void main(") || trimmed.starts_with("Future<void> main(") {
                    entries.push(RunnableGutterEntry {
                        line: idx,
                        label: "Run 'main()'".to_string(),
                        command: "dart".to_string(),
                        args: vec!["run".to_string()],
                    });
                } else if trimmed.starts_with("test(") || trimmed.starts_with("testWidgets(") {
                    entries.push(RunnableGutterEntry {
                        line: idx,
                        label: "Run Flutter/Dart Test".to_string(),
                        command: "flutter".to_string(),
                        args: vec!["test".to_string()],
                    });
                }
            }
            LanguageId::C | LanguageId::Cpp => {
                if trimmed.starts_with("int main(") || trimmed.starts_with("auto main(") {
                    entries.push(RunnableGutterEntry {
                        line: idx,
                        label: "Run 'main'".to_string(),
                        command: "cmake".to_string(),
                        args: vec!["--build".to_string(), "build".to_string()],
                    });
                }
            }
            LanguageId::CSharp => {
                if trimmed.contains("static void Main(")
                    || trimmed.contains("static async Task Main(")
                {
                    entries.push(RunnableGutterEntry {
                        line: idx,
                        label: "Run 'dotnet run'".to_string(),
                        command: "dotnet".to_string(),
                        args: vec!["run".to_string()],
                    });
                } else if matches!(trimmed, "[Fact]" | "[Theory]" | "[Test]") {
                    entries.push(RunnableGutterEntry {
                        line: (idx + 1).min(lines.len().saturating_sub(1)),
                        label: "Run 'dotnet test'".to_string(),
                        command: "dotnet".to_string(),
                        args: vec!["test".to_string()],
                    });
                }
            }
            _ => {}
        }
    }

    entries
}

fn extract_ts_js_structure_symbol(line: &str, line_idx: usize) -> Option<StructureSymbol> {
    if let Some(rest) = line
        .strip_prefix("export function ")
        .or_else(|| line.strip_prefix("export default function "))
        .or_else(|| line.strip_prefix("export async function "))
        .or_else(|| line.strip_prefix("async function "))
        .or_else(|| line.strip_prefix("function "))
    {
        if let Some(name) = rest.split(['(', '<', ' ']).next() {
            if !name.trim().is_empty() {
                return Some(StructureSymbol {
                    name: name.trim().to_string(),
                    kind: SymbolKind::Function,
                    line: line_idx,
                });
            }
        }
    } else if let Some(rest) = line
        .strip_prefix("export class ")
        .or_else(|| line.strip_prefix("export default class "))
        .or_else(|| line.strip_prefix("export abstract class "))
        .or_else(|| line.strip_prefix("abstract class "))
        .or_else(|| line.strip_prefix("class "))
    {
        if let Some(name) = rest.split(['{', ' ', '<']).next() {
            if !name.trim().is_empty() {
                return Some(StructureSymbol {
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
                return Some(StructureSymbol {
                    name: name.trim().to_string(),
                    kind: SymbolKind::Interface,
                    line: line_idx,
                });
            }
        }
    } else if let Some(rest) = line
        .strip_prefix("export enum ")
        .or_else(|| line.strip_prefix("export const enum "))
        .or_else(|| line.strip_prefix("enum "))
    {
        if let Some(name) = rest.split(['{', ' ']).next() {
            if !name.trim().is_empty() {
                return Some(StructureSymbol {
                    name: name.trim().to_string(),
                    kind: SymbolKind::Enum,
                    line: line_idx,
                });
            }
        }
    } else if let Some(rest) = line
        .strip_prefix("export const ")
        .or_else(|| line.strip_prefix("const "))
        .or_else(|| line.strip_prefix("export let "))
        .or_else(|| line.strip_prefix("let "))
    {
        if let Some((lhs, rhs)) = rest.split_once('=') {
            let name = lhs.split([':', ' ']).next().unwrap_or("").trim();
            let rhs_t = rhs.trim();
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                let kind = if rhs_t.contains("=>") || rhs_t.starts_with("function") {
                    SymbolKind::Function
                } else {
                    SymbolKind::Constant
                };
                return Some(StructureSymbol {
                    name: name.to_string(),
                    kind,
                    line: line_idx,
                });
            }
        }
    }
    None
}

fn is_valid_ident(s: &str) -> bool {
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
            "internal ",
            "static ",
            "final ",
            "readonly ",
            "abstract ",
            "virtual ",
            "override ",
            "async ",
            "partial ",
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
                    "public "
                        | "protected "
                        | "private "
                        | "internal "
                        | "static "
                        | "final "
                        | "readonly "
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

fn extract_java_or_csharp_structure_symbol(
    raw_line: &str,
    line_idx: usize,
) -> Option<StructureSymbol> {
    let no_comment = raw_line.split("//").next().unwrap_or("").trim();
    if no_comment.is_empty()
        || no_comment.starts_with("/*")
        || no_comment.starts_with('*')
        || no_comment.starts_with("package ")
        || no_comment.starts_with("import ")
        || no_comment.starts_with("using ")
        || no_comment.starts_with("return ")
        || no_comment.starts_with("throw ")
        || no_comment.starts_with("new ")
    {
        return None;
    }

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
        let name = after.split(['{', '<', ':', ' ']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Class,
                line: line_idx,
            });
        }
    } else if let Some(after) = rest.strip_prefix("struct ") {
        let name = after.split(['{', '<', ':', ' ']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Struct,
                line: line_idx,
            });
        }
    } else if let Some(after) = rest
        .strip_prefix("interface ")
        .or_else(|| rest.strip_prefix("@interface "))
    {
        let name = after.split(['{', '<', ':', ' ']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Interface,
                line: line_idx,
            });
        }
    } else if let Some(after) = rest.strip_prefix("enum ") {
        let name = after.split(['{', ':', ' ']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Enum,
                line: line_idx,
            });
        }
    } else if let Some(after) = rest.strip_prefix("record ") {
        let name = after.split(['(', '<', ' ', '{']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Class,
                line: line_idx,
            });
        }
    } else if let Some((before_paren, _)) = rest.split_once('(') {
        if !before_paren.contains('=') && !before_paren.contains('.') {
            let name = before_paren.split_whitespace().last()?.trim();
            if is_valid_ident(name)
                && !matches!(
                    name,
                    "if" | "for"
                        | "foreach"
                        | "while"
                        | "switch"
                        | "catch"
                        | "synchronized"
                        | "lock"
                        | "using"
                        | "return"
                        | "new"
                        | "super"
                        | "base"
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
        let lhs = rest.split(['=', ';', '{']).next()?.trim();
        let name = lhs.split_whitespace().last()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Constant,
                line: line_idx,
            });
        }
    }

    None
}

fn extract_go_structure_symbol(line: &str, line_idx: usize) -> Option<StructureSymbol> {
    let clean = line.split("//").next().unwrap_or("").trim();
    if let Some(after) = clean.strip_prefix("func ") {
        let rest = if after.starts_with('(') {
            let close = after.find(')')?;
            after[close + 1..].trim_start()
        } else {
            after
        };
        let name = rest.split(['(', '[']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Function,
                line: line_idx,
            });
        }
    } else if let Some(after) = clean.strip_prefix("type ") {
        let mut parts = after.split_whitespace();
        let name = parts.next()?.split('[').next()?.trim();
        let kind_tok = parts.next().unwrap_or("");
        if is_valid_ident(name) {
            let kind = match kind_tok {
                "interface" | "interface{" => SymbolKind::Interface,
                _ => SymbolKind::Struct,
            };
            return Some(StructureSymbol {
                name: name.to_string(),
                kind,
                line: line_idx,
            });
        }
    } else if let Some(after) = clean
        .strip_prefix("const ")
        .or_else(|| clean.strip_prefix("var "))
    {
        if !after.starts_with('(') {
            let name = after.split(['=', ' ', '\t']).next()?.trim();
            if is_valid_ident(name) {
                return Some(StructureSymbol {
                    name: name.to_string(),
                    kind: SymbolKind::Constant,
                    line: line_idx,
                });
            }
        }
    }
    None
}

fn extract_kotlin_structure_symbol(line: &str, line_idx: usize) -> Option<StructureSymbol> {
    let mut work = line.split("//").next().unwrap_or("").trim();
    for kw in [
        "public ",
        "private ",
        "protected ",
        "internal ",
        "open ",
        "abstract ",
        "final ",
        "override ",
        "data ",
        "sealed ",
        "suspend ",
        "inline ",
        "companion ",
    ] {
        if let Some(r) = work.strip_prefix(kw) {
            work = r.trim_start();
        }
    }
    if let Some(after) = work.strip_prefix("fun ") {
        let rest = if after.starts_with('<') {
            let close = after.find('>')?;
            after[close + 1..].trim_start()
        } else {
            after
        };
        let fn_part = rest.split('(').next()?.trim();
        let name = fn_part.rsplit('.').next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Function,
                line: line_idx,
            });
        }
    } else if let Some(after) = work.strip_prefix("enum class ") {
        let name = after.split(['(', '{', ':', '<', ' ']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Enum,
                line: line_idx,
            });
        }
    } else if let Some(after) = work
        .strip_prefix("class ")
        .or_else(|| work.strip_prefix("object "))
    {
        let name = after.split(['(', '{', ':', '<', ' ']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Class,
                line: line_idx,
            });
        }
    } else if let Some(after) = work
        .strip_prefix("interface ")
        .or_else(|| work.strip_prefix("typealias "))
    {
        let name = after.split(['{', ':', '<', '=', ' ']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Interface,
                line: line_idx,
            });
        }
    } else if let Some(after) = work
        .strip_prefix("const val ")
        .or_else(|| work.strip_prefix("val "))
    {
        let name = after.split([':', '=', ' ']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Constant,
                line: line_idx,
            });
        }
    }
    None
}

fn extract_dart_structure_symbol(line: &str, line_idx: usize) -> Option<StructureSymbol> {
    let mut work = line.split("//").next().unwrap_or("").trim();
    for kw in ["abstract ", "base ", "final ", "interface ", "sealed "] {
        if let Some(r) = work.strip_prefix(kw) {
            work = r.trim_start();
        }
    }
    if let Some(after) = work
        .strip_prefix("class ")
        .or_else(|| work.strip_prefix("mixin "))
        .or_else(|| work.strip_prefix("extension "))
    {
        let name = after.split(['{', '<', ' ']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Class,
                line: line_idx,
            });
        }
    } else if let Some(after) = work.strip_prefix("enum ") {
        let name = after.split(['{', '<', ' ']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Enum,
                line: line_idx,
            });
        }
    } else if let Some((before_paren, _)) = work.split_once('(') {
        if !before_paren.contains('=') && !before_paren.contains('.') {
            let tokens: Vec<&str> = before_paren.split_whitespace().collect();
            if tokens.len() >= 2 {
                let name = tokens.last()?.trim();
                if is_valid_ident(name)
                    && !matches!(
                        name,
                        "if" | "for" | "while" | "switch" | "catch" | "return" | "new" | "super"
                    )
                {
                    return Some(StructureSymbol {
                        name: name.to_string(),
                        kind: SymbolKind::Function,
                        line: line_idx,
                    });
                }
            }
        }
    }
    None
}

fn extract_c_cpp_structure_symbol(line: &str, line_idx: usize) -> Option<StructureSymbol> {
    let clean = line.split("//").next().unwrap_or("").trim();
    if clean.is_empty() || clean.starts_with("/*") || clean.starts_with('*') {
        return None;
    }
    if let Some(after) = clean.strip_prefix("#define ") {
        let name = after.split(['(', ' ', '\t']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Constant,
                line: line_idx,
            });
        }
        return None;
    }
    if let Some(after) = clean.strip_prefix("class ") {
        let name = after.split(['{', ':', ';', ' ']).next()?.trim();
        if is_valid_ident(name) && !clean.ends_with(';') {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Class,
                line: line_idx,
            });
        }
    } else if let Some(after) = clean
        .strip_prefix("struct ")
        .or_else(|| clean.strip_prefix("typedef struct "))
    {
        let name = after.split(['{', ':', ';', ' ']).next()?.trim();
        if is_valid_ident(name) && !clean.ends_with(';') {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Struct,
                line: line_idx,
            });
        }
    } else if let Some(after) = clean
        .strip_prefix("enum class ")
        .or_else(|| clean.strip_prefix("enum "))
    {
        let name = after.split(['{', ':', ';', ' ']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Enum,
                line: line_idx,
            });
        }
    } else if let Some(after) = clean.strip_prefix("namespace ") {
        let name = after.split(['{', ' ']).next()?.trim();
        if is_valid_ident(name) {
            return Some(StructureSymbol {
                name: name.to_string(),
                kind: SymbolKind::Interface,
                line: line_idx,
            });
        }
    } else if (clean.ends_with('{') || clean.ends_with(')')) && !clean.contains('=') {
        if let Some((before_paren, _)) = clean.split_once('(') {
            let tokens: Vec<&str> = before_paren.split_whitespace().collect();
            if tokens.len() >= 2 {
                let raw_name = tokens.last()?.trim_start_matches(['*', '&']);
                let name = raw_name.rsplit("::").next().unwrap_or(raw_name);
                if is_valid_ident(name)
                    && !matches!(
                        name,
                        "if" | "for" | "while" | "switch" | "catch" | "return" | "sizeof"
                    )
                {
                    return Some(StructureSymbol {
                        name: name.to_string(),
                        kind: SymbolKind::Function,
                        line: line_idx,
                    });
                }
            }
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
    if let Some(sym) = extract_java_or_csharp_structure_symbol(lines[anno_line], anno_line) {
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
        if let Some(sym) = extract_java_or_csharp_structure_symbol(t, idx) {
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

        let rust_spans = highlight_line("let count: u32 = self.total_items;", LanguageId::Rust);
        assert!(rust_spans
            .iter()
            .any(|s| s.text == "u32" && s.kind == TokenKind::Type));
        assert!(rust_spans
            .iter()
            .any(|s| s.text == "total_items" && s.kind == TokenKind::FieldOrProperty));
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

    #[test]
    fn test_multi_language_highlighting_and_symbol_extraction() {
        // Go
        let go_src = "package main\n\ntype Server struct {}\n\nfunc (s *Server) Start() error {\n    port := 8080\n    return nil\n}\n\nfunc main() {}\n";
        let go_syms = extract_structure_symbols(go_src, LanguageId::Go);
        assert!(go_syms.iter().any(|s| s.name == "Server" && s.kind == SymbolKind::Struct));
        assert!(go_syms.iter().any(|s| s.name == "Start" && s.kind == SymbolKind::Function));
        assert!(go_syms.iter().any(|s| s.name == "main" && s.kind == SymbolKind::Function));
        assert!(find_symbol_definition(go_src, LanguageId::Go, "port").is_some());

        // JSON / TOML / YAML / SQL / Markdown
        let json_spans = highlight_line("  \"version\": \"0.4.0\",", LanguageId::Json);
        assert!(json_spans
            .iter()
            .any(|s| s.text == "\"version\"" && s.kind == TokenKind::FieldOrProperty));
        assert!(json_spans
            .iter()
            .any(|s| s.text == "\"0.4.0\"" && s.kind == TokenKind::String));

        let toml_spans = highlight_line("edition = \"2021\" # workspace", LanguageId::Toml);
        assert!(toml_spans
            .iter()
            .any(|s| s.text == "edition" && s.kind == TokenKind::FieldOrProperty));
        assert!(toml_spans
            .iter()
            .any(|s| s.text == "\"2021\"" && s.kind == TokenKind::String));

        let sql_spans = highlight_line("SELECT id FROM users WHERE active = TRUE;", LanguageId::Sql);
        assert!(sql_spans
            .iter()
            .any(|s| s.text == "SELECT" && s.kind == TokenKind::Keyword));

        let md_syms = extract_structure_symbols("# Architecture\n## Indexing\n", LanguageId::Markdown);
        assert_eq!(md_syms.len(), 2);
        assert_eq!(md_syms[0].name, "Architecture");
    }

    #[test]
    fn test_vue_svelte_astro_and_jsx_highlighting_and_symbols() {
        let vue_src = "\
<script setup lang=\"ts\">
import { ref } from 'vue'
const count = ref<number>(0)
function increment() {
  count.value++
}
</script>

<template>
  <MyButton v-if=\"count >= 0\" @click=\"increment\" :disabled=\"false\" #default>
    <span>{{ count + 1 }}</span>
  </MyButton>
</template>

<style scoped lang=\"scss\">
:deep(.btn) {
  color: #3574F0;
}
</style>
";
        let mut state = HighlightState::default();
        let line_spans: Vec<Vec<HighlightSpan>> = vue_src
            .lines()
            .map(|l| highlight_line_with_state(l, LanguageId::Vue, &mut state))
            .collect();

        // <script setup lang="ts"> line
        assert!(line_spans[0]
            .iter()
            .any(|s| s.text == "script" && s.kind == TokenKind::Keyword));
        // Inside <script setup lang="ts">: TS keywords & generics don't break into HTML tags
        assert!(line_spans[2]
            .iter()
            .any(|s| s.text == "const" && s.kind == TokenKind::Keyword));
        assert!(line_spans[2]
            .iter()
            .any(|s| s.text == "number" && s.kind == TokenKind::Type));
        assert!(line_spans[3]
            .iter()
            .any(|s| s.text == "increment" && s.kind == TokenKind::Function));
        // Inside <template>: PascalCase component, directives, and {{ }} interpolation
        assert!(line_spans[9]
            .iter()
            .any(|s| s.text == "MyButton" && s.kind == TokenKind::Type));
        assert!(line_spans[9]
            .iter()
            .any(|s| s.text == "v-if" && s.kind == TokenKind::MacroOrDecorator));
        assert!(line_spans[9]
            .iter()
            .any(|s| s.text == "@click" && s.kind == TokenKind::MacroOrDecorator));
        assert!(line_spans[9]
            .iter()
            .any(|s| s.text == ":disabled" && s.kind == TokenKind::MacroOrDecorator));
        assert!(line_spans[9]
            .iter()
            .any(|s| s.text == "#default" && s.kind == TokenKind::MacroOrDecorator));
        assert!(line_spans[10]
            .iter()
            .any(|s| s.text == "{{" && s.kind == TokenKind::MacroOrDecorator));
        // Inside <style scoped>: CSS/SCSS & :deep()
        assert!(line_spans[15]
            .iter()
            .any(|s| s.text == ":deep" && s.kind == TokenKind::MacroOrDecorator));
        assert!(line_spans[16]
            .iter()
            .any(|s| s.text == "#3574F0" && s.kind == TokenKind::Number));

        let vue_syms = extract_structure_symbols(vue_src, LanguageId::Vue);
        assert!(vue_syms.iter().any(|s| s.name == "script setup"));
        assert!(vue_syms.iter().any(|s| s.name == "template"));
        assert!(vue_syms.iter().any(|s| s.name == "style"));
        assert!(vue_syms.iter().any(|s| s.name == "increment" && s.kind == SymbolKind::Function));
        assert!(find_symbol_definition(vue_src, LanguageId::Vue, "count").is_some());

        // Svelte
        let svelte_src = "<script lang=\"ts\">\nlet n = $state(0)\n</script>\n{#if n > 0}<Card on:click={inc}>{n}</Card>{/if}\n";
        let mut s_state = HighlightState::default();
        let s_spans: Vec<Vec<HighlightSpan>> = svelte_src
            .lines()
            .map(|l| highlight_line_with_state(l, LanguageId::Svelte, &mut s_state))
            .collect();
        assert!(s_spans[1]
            .iter()
            .any(|s| s.text == "$state" && s.kind == TokenKind::MacroOrDecorator));
        assert!(s_spans[3]
            .iter()
            .any(|s| s.text == "#if" && s.kind == TokenKind::Keyword));
        assert!(s_spans[3]
            .iter()
            .any(|s| s.text == "on:click" && s.kind == TokenKind::MacroOrDecorator));

        // Astro frontmatter
        let astro_src = "---\nconst title = 'Hello'\n---\n<Layout client:load>{title}</Layout>\n";
        let mut a_state = HighlightState::default();
        let a_spans: Vec<Vec<HighlightSpan>> = astro_src
            .lines()
            .map(|l| highlight_line_with_state(l, LanguageId::Astro, &mut a_state))
            .collect();
        assert!(a_spans[1]
            .iter()
            .any(|s| s.text == "const" && s.kind == TokenKind::Keyword));
        assert!(a_spans[3]
            .iter()
            .any(|s| s.text == "Layout" && s.kind == TokenKind::Type));
        assert!(a_spans[3]
            .iter()
            .any(|s| s.text == "client:load" && s.kind == TokenKind::MacroOrDecorator));

        // TSX JSX tag highlighting
        let tsx_spans = highlight_line("return <MyCard title=\"ok\" count={1} />", LanguageId::TypeScript);
        assert!(tsx_spans
            .iter()
            .any(|s| s.text == "MyCard" && s.kind == TokenKind::Type));
        assert!(tsx_spans
            .iter()
            .any(|s| s.text == "title" && s.kind == TokenKind::FieldOrProperty));

        // Multi-line Vue component tag, multi-line <script setup lang="ts">, multi-line attr & {{ }}
        let multiline_vue = "\
<script
  setup
  lang=\"ts\"
>
const count = ref<number>(0)
</script>

<template>
  <EstimateModalThFilter
    v-model=\"modalFilterRemarks\"
    col-key=\"modal.inst.remarks\"
    :default-width=\"80\"
    title=\"备注用途\"
    :options=\"modalRemarksOptions\"
    cell-class=\"px-1 py-1 align-top\"
  />
  <div>
    {{
      count + 1
    }}
  </div>
</template>
";
        let mut mv_state = HighlightState::default();
        let mv_spans: Vec<Vec<HighlightSpan>> = multiline_vue
            .lines()
            .map(|l| highlight_line_with_state(l, LanguageId::Vue, &mut mv_state))
            .collect();
        assert!(mv_spans[4]
            .iter()
            .any(|s| s.text == "const" && s.kind == TokenKind::Keyword));
        assert!(mv_spans[4]
            .iter()
            .any(|s| s.text == "number" && s.kind == TokenKind::Type));
        assert!(mv_spans[8]
            .iter()
            .any(|s| s.text == "EstimateModalThFilter" && s.kind == TokenKind::Type));
        assert!(mv_spans[9]
            .iter()
            .any(|s| s.text == "v-model" && s.kind == TokenKind::MacroOrDecorator));
        assert!(mv_spans[9]
            .iter()
            .any(|s| s.text == "\"" && s.kind == TokenKind::String));
        assert!(mv_spans[10]
            .iter()
            .any(|s| s.text == "col-key" && s.kind == TokenKind::FieldOrProperty));
        assert!(mv_spans[10]
            .iter()
            .any(|s| s.text == "\"modal.inst.remarks\"" && s.kind == TokenKind::String));
        assert!(mv_spans[11]
            .iter()
            .any(|s| s.text == ":default-width" && s.kind == TokenKind::MacroOrDecorator));
        assert!(mv_spans[11]
            .iter()
            .any(|s| s.text == "80" && s.kind == TokenKind::Number));
        assert!(mv_spans[12]
            .iter()
            .any(|s| s.text == "title" && s.kind == TokenKind::FieldOrProperty));
        assert!(mv_spans[13]
            .iter()
            .any(|s| s.text == ":options" && s.kind == TokenKind::MacroOrDecorator));
        assert!(mv_spans[14]
            .iter()
            .any(|s| s.text == "cell-class" && s.kind == TokenKind::FieldOrProperty));
        assert!(mv_spans[15]
            .iter()
            .any(|s| s.text == ">" && s.kind == TokenKind::Keyword));
        assert!(mv_spans[18]
            .iter()
            .any(|s| s.text == "1" && s.kind == TokenKind::Number));

        let mv_syms = extract_structure_symbols(multiline_vue, LanguageId::Vue);
        assert!(mv_syms.iter().any(|s| s.name == "script setup"));
        assert!(mv_syms.iter().any(|s| s.name == "count"));

        // Multi-line TSX JSX tag
        let multiline_tsx = "return (\n  <MyCard\n    title=\"ok\"\n  />\n)";
        let mut tsx_state = HighlightState::default();
        let tsx_ml_spans: Vec<Vec<HighlightSpan>> = multiline_tsx
            .lines()
            .map(|l| highlight_line_with_state(l, LanguageId::TypeScript, &mut tsx_state))
            .collect();
        assert!(tsx_ml_spans[2]
            .iter()
            .any(|s| s.text == "title" && s.kind == TokenKind::FieldOrProperty));
        assert!(tsx_ml_spans[2]
            .iter()
            .any(|s| s.text == "\"ok\"" && s.kind == TokenKind::String));
    }
}
