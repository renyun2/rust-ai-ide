use serde::{Deserialize, Serialize};
use std::path::Path;

mod mobile;
mod run_config;
pub use mobile::*;
pub use run_config::{parse_shell_command_parts, RunConfigSource, RunConfiguration};

/// 支持三大官方主题切换（可通过 `Ctrl+Alt+S` 设置或 `Ctrl+\`` 快速切换）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeKind {
    IslandsDark,
    DarculaClassic,
    IslandsLight,
}

impl ThemeKind {
    pub fn all() -> &'static [Self] {
        &[Self::IslandsDark, Self::DarculaClassic, Self::IslandsLight]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::IslandsDark => "JetBrains Dark (Islands)",
            Self::DarculaClassic => "Darcula Classic",
            Self::IslandsLight => "IntelliJ Light (Islands)",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::IslandsDark => Self::DarculaClassic,
            Self::DarculaClassic => Self::IslandsLight,
            Self::IslandsLight => Self::IslandsDark,
        }
    }
}

/// JetBrains IntelliJ IDEA 2025/2026 Islands New UI 像素级配色方案 (0x00RRGGBB)
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct IdeaTheme {
    pub kind: ThemeKind,
    pub window_bg: u32,
    pub main_toolbar_bg: u32,
    pub tool_stripe_bg: u32,
    pub panel_bg: u32,
    pub panel_header_bg: u32,
    pub editor_bg: u32,
    pub editor_gutter_bg: u32,
    pub editor_current_line_bg: u32,
    pub editor_selection_bg: u32,
    pub tab_active_bg: u32,
    pub tab_inactive_bg: u32,
    pub tab_underline_accent: u32,
    pub border_color: u32,
    pub splitter_hover_color: u32,
    pub status_bar_bg: u32,
    pub popup_bg: u32,
    pub popup_hover_bg: u32,
    pub text_primary: u32,
    pub text_secondary: u32,
    pub text_muted: u32,
    pub accent_blue: u32,
    pub run_green: u32,
    pub debug_orange: u32,
    pub breakpoint_red: u32,
    pub warning_yellow: u32,
    pub diff_added_bg: u32,
    pub diff_added_fg: u32,
    pub diff_removed_bg: u32,
    pub diff_removed_fg: u32,
    // IDEA 经典语法高亮配色
    pub syn_keyword: u32,
    pub syn_function: u32,
    pub syn_type: u32,
    pub syn_string: u32,
    pub syn_number: u32,
    pub syn_comment: u32,
    pub syn_macro_or_decorator: u32,
    pub syn_field_or_property: u32,
}

impl Default for IdeaTheme {
    fn default() -> Self {
        Self::from_kind(ThemeKind::IslandsDark)
    }
}

impl IdeaTheme {
    pub const fn from_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::IslandsDark => Self::jetbrains_islands_dark(),
            ThemeKind::DarculaClassic => Self::darcula_classic(),
            ThemeKind::IslandsLight => Self::jetbrains_islands_light(),
        }
    }

    pub const fn jetbrains_islands_dark() -> Self {
        Self {
            kind: ThemeKind::IslandsDark,
            window_bg: 0x1E1F22,
            main_toolbar_bg: 0x1E1F22,
            tool_stripe_bg: 0x1E1F22,
            panel_bg: 0x2B2D30,
            panel_header_bg: 0x2B2D30,
            editor_bg: 0x1E1F22,
            editor_gutter_bg: 0x1E1F22,
            editor_current_line_bg: 0x26282E,
            editor_selection_bg: 0x214283,
            tab_active_bg: 0x1E1F22,
            tab_inactive_bg: 0x2B2D30,
            tab_underline_accent: 0x3574F0,
            border_color: 0x393B40,
            splitter_hover_color: 0x3574F0,
            status_bar_bg: 0x1E1F22,
            popup_bg: 0x2B2D30,
            popup_hover_bg: 0x2E436E,
            text_primary: 0xDFE1E5,
            text_secondary: 0xB4B8BF,
            text_muted: 0x6F737A,
            accent_blue: 0x3574F0,
            run_green: 0x59A869,
            debug_orange: 0xE08855,
            breakpoint_red: 0xDB5860,
            warning_yellow: 0xF2C55C,
            diff_added_bg: 0x1E3826,
            diff_added_fg: 0x73C991,
            diff_removed_bg: 0x422024,
            diff_removed_fg: 0xF48771,
            syn_keyword: 0xCF8E6D,
            syn_function: 0x56A8F5,
            syn_type: 0x6FAFBD,
            syn_string: 0x6AAB73,
            syn_number: 0x2AACB8,
            syn_comment: 0x7A7E85,
            syn_macro_or_decorator: 0xBBB529,
            syn_field_or_property: 0xC77DBB,
        }
    }

    pub const fn darcula_classic() -> Self {
        Self {
            kind: ThemeKind::DarculaClassic,
            window_bg: 0x3C3F41,
            main_toolbar_bg: 0x3C3F41,
            tool_stripe_bg: 0x3C3F41,
            panel_bg: 0x313335,
            panel_header_bg: 0x3C3F41,
            editor_bg: 0x2B2B2B,
            editor_gutter_bg: 0x313335,
            editor_current_line_bg: 0x323232,
            editor_selection_bg: 0x214283,
            tab_active_bg: 0x2B2B2B,
            tab_inactive_bg: 0x3C3F41,
            tab_underline_accent: 0x4A88C7,
            border_color: 0x515151,
            splitter_hover_color: 0x4A88C7,
            status_bar_bg: 0x3C3F41,
            popup_bg: 0x3C3F41,
            popup_hover_bg: 0x4B6EAF,
            text_primary: 0xA9B7C6,
            text_secondary: 0x8C929A,
            text_muted: 0x606366,
            accent_blue: 0x4A88C7,
            run_green: 0x6A8759,
            debug_orange: 0xCC7832,
            breakpoint_red: 0xC75450,
            warning_yellow: 0xBBB529,
            diff_added_bg: 0x294436,
            diff_added_fg: 0x6A8759,
            diff_removed_bg: 0x482A2A,
            diff_removed_fg: 0xCC7832,
            syn_keyword: 0xCC7832,
            syn_function: 0xFFC66D,
            syn_type: 0xA9B7C6,
            syn_string: 0x6A8759,
            syn_number: 0x6897BB,
            syn_comment: 0x808080,
            syn_macro_or_decorator: 0xBBB529,
            syn_field_or_property: 0x9876AA,
        }
    }

    pub const fn jetbrains_islands_light() -> Self {
        Self {
            kind: ThemeKind::IslandsLight,
            window_bg: 0xEBECF0,
            main_toolbar_bg: 0xEBECF0,
            tool_stripe_bg: 0xEBECF0,
            panel_bg: 0xF7F8FA,
            panel_header_bg: 0xF2F3F5,
            editor_bg: 0xFFFFFF,
            editor_gutter_bg: 0xF7F8FA,
            editor_current_line_bg: 0xF5F8FE,
            editor_selection_bg: 0xD4E2FF,
            tab_active_bg: 0xFFFFFF,
            tab_inactive_bg: 0xEBECF0,
            tab_underline_accent: 0x3574F0,
            border_color: 0xD3D5DB,
            splitter_hover_color: 0x3574F0,
            status_bar_bg: 0xEBECF0,
            popup_bg: 0xFFFFFF,
            popup_hover_bg: 0xDFE8FF,
            text_primary: 0x1E1F22,
            text_secondary: 0x494B57,
            text_muted: 0x818594,
            accent_blue: 0x3574F0,
            run_green: 0x369650,
            debug_orange: 0xD66D27,
            breakpoint_red: 0xDB3B4B,
            warning_yellow: 0xC27D0E,
            diff_added_bg: 0xDCF5E1,
            diff_added_fg: 0x1E6B34,
            diff_removed_bg: 0xFFDCE0,
            diff_removed_fg: 0xA82020,
            syn_keyword: 0x0033B3,
            syn_function: 0x00627A,
            syn_type: 0x20799E,
            syn_string: 0x067D17,
            syn_number: 0x1750EB,
            syn_comment: 0x8C8C8C,
            syn_macro_or_decorator: 0x9E880D,
            syn_field_or_property: 0x871094,
        }
    }
}

/// 支持的编程语言、标记语言与配置文件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LanguageId {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Java,
    Go,
    Kotlin,
    Dart,
    C,
    Cpp,
    CSharp,
    Vue,
    Svelte,
    Astro,
    Html,
    Css,
    Yaml,
    Xml,
    Sql,
    Shell,
    Json,
    Toml,
    Markdown,
    PlainText,
}

impl LanguageId {
    pub fn from_path(path: &Path) -> Self {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match file_name.as_str() {
            "dockerfile"
            | "makefile"
            | "gnumakefile"
            | "justfile"
            | ".bashrc"
            | ".zshrc"
            | ".profile"
            | ".bash_profile"
            | "jenkinsfile" => return Self::Shell,
            "cmakelists.txt" => return Self::Shell,
            "go.mod" | "go.sum" | "go.work" => return Self::Go,
            "cargo.lock" => return Self::Toml,
            _ => {}
        }
        if file_name.starts_with("dockerfile.") || file_name.starts_with(".env") {
            return Self::Shell;
        }

        match path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "rs" => Self::Rust,
            "ts" | "tsx" | "mts" | "cts" => Self::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "py" | "pyi" | "pyw" => Self::Python,
            "java" => Self::Java,
            "go" => Self::Go,
            "kt" | "kts" => Self::Kotlin,
            "dart" => Self::Dart,
            "c" | "h" => Self::C,
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "ino" => Self::Cpp,
            "cs" => Self::CSharp,
            "vue" => Self::Vue,
            "svelte" => Self::Svelte,
            "astro" => Self::Astro,
            "html" | "htm" => Self::Html,
            "css" | "scss" | "sass" | "less" => Self::Css,
            "yaml" | "yml" => Self::Yaml,
            "xml" | "svg" | "plist" | "csproj" | "vcxproj" | "props" | "targets" | "pom"
            | "xaml" => Self::Xml,
            "sql" => Self::Sql,
            "sh" | "bash" | "zsh" | "fish" | "ps1" | "psm1" | "psd1" | "bat" | "cmd" => {
                Self::Shell
            }
            "json" | "jsonc" | "json5" | "arb" => Self::Json,
            "toml" => Self::Toml,
            "md" | "markdown" | "mdx" => Self::Markdown,
            _ => Self::PlainText,
        }
    }

    pub fn from_fence_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "rs" | "rust" => Self::Rust,
            "ts" | "tsx" | "mts" | "cts" | "typescript" => Self::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" | "javascript" => Self::JavaScript,
            "py" | "python" | "python3" => Self::Python,
            "java" => Self::Java,
            "go" | "golang" => Self::Go,
            "kt" | "kts" | "kotlin" => Self::Kotlin,
            "dart" | "flutter" => Self::Dart,
            "c" => Self::C,
            "cpp" | "c++" | "cc" | "cxx" | "hpp" => Self::Cpp,
            "cs" | "csharp" | "c#" => Self::CSharp,
            "vue" => Self::Vue,
            "svelte" => Self::Svelte,
            "astro" => Self::Astro,
            "html" | "htm" => Self::Html,
            "css" | "scss" | "sass" | "less" => Self::Css,
            "yaml" | "yml" => Self::Yaml,
            "xml" | "svg" | "xaml" => Self::Xml,
            "sql" | "mysql" | "postgresql" | "postgres" | "sqlite" => Self::Sql,
            "sh" | "bash" | "zsh" | "shell" | "console" | "ps1" | "pwsh" | "powershell"
            | "bat" | "cmd" | "dockerfile" => Self::Shell,
            "json" | "jsonc" | "json5" => Self::Json,
            "toml" | "ini" => Self::Toml,
            "md" | "markdown" | "mdx" => Self::Markdown,
            _ => Self::PlainText,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::TypeScript => "TypeScript",
            Self::JavaScript => "JavaScript",
            Self::Python => "Python",
            Self::Java => "Java",
            Self::Go => "Go",
            Self::Kotlin => "Kotlin",
            Self::Dart => "Dart",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::CSharp => "C#",
            Self::Vue => "Vue",
            Self::Svelte => "Svelte",
            Self::Astro => "Astro",
            Self::Html => "HTML",
            Self::Css => "CSS / SCSS",
            Self::Yaml => "YAML",
            Self::Xml => "XML",
            Self::Sql => "SQL",
            Self::Shell => "Shell",
            Self::Json => "JSON",
            Self::Toml => "TOML",
            Self::Markdown => "Markdown",
            Self::PlainText => "Plain Text",
        }
    }

    pub fn lsp_language_id(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Java => "java",
            Self::Go => "go",
            Self::Kotlin => "kotlin",
            Self::Dart => "dart",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::CSharp => "csharp",
            Self::Vue => "vue",
            Self::Svelte => "svelte",
            Self::Astro => "astro",
            Self::Html => "html",
            Self::Css => "css",
            Self::Yaml => "yaml",
            Self::Xml => "xml",
            Self::Sql => "sql",
            Self::Shell => "shellscript",
            Self::Json => "json",
            Self::Toml => "toml",
            Self::Markdown => "markdown",
            Self::PlainText => "plaintext",
        }
    }

    pub fn default_lsp_binary(&self) -> Option<&'static str> {
        match self {
            Self::Rust => Some("rust-analyzer"),
            Self::TypeScript | Self::JavaScript => Some("vtsls"),
            Self::Python => Some("pyright-langserver"),
            Self::Java => Some("jdtls"),
            Self::Go => Some("gopls"),
            Self::Kotlin => Some("kotlin-language-server"),
            Self::Dart => Some("dart"),
            Self::C | Self::Cpp => Some("clangd"),
            Self::CSharp => Some("csharp-ls"),
            Self::Vue => Some("vue-language-server"),
            Self::Svelte => Some("svelteserver"),
            Self::Astro => Some("astro-ls"),
            _ => None,
        }
    }

    pub fn default_dap_adapter(&self) -> Option<&'static str> {
        match self {
            Self::Rust | Self::C | Self::Cpp => Some("codelldb"),
            Self::TypeScript | Self::JavaScript | Self::Vue | Self::Svelte | Self::Astro => {
                Some("js-debug")
            }
            Self::Python => Some("debugpy"),
            Self::Java | Self::Kotlin => Some("java-debug"),
            Self::Go => Some("dlv"),
            Self::Dart => Some("flutter-dap"),
            Self::CSharp => Some("netcoredbg"),
            _ => None,
        }
    }

    pub fn line_comment_prefix(&self) -> &'static str {
        match self {
            Self::Python | Self::Toml | Self::Yaml | Self::Shell => "# ",
            Self::Sql => "-- ",
            _ => "// ",
        }
    }

    pub fn supports_symbol_indexing(&self) -> bool {
        !matches!(self, Self::PlainText)
    }

    pub fn is_sfc_or_html(&self) -> bool {
        matches!(self, Self::Vue | Self::Svelte | Self::Astro | Self::Html)
    }
}

/// 快捷键映射方案（支持 IntelliJ IDEA 默认方案与 Eclipse 经典方案）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeymapScheme {
    IntelliJIdea,
    Eclipse,
}

impl Default for KeymapScheme {
    fn default() -> Self {
        Self::IntelliJIdea
    }
}

impl KeymapScheme {
    pub fn all() -> &'static [Self] {
        &[Self::IntelliJIdea, Self::Eclipse]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::IntelliJIdea => "IntelliJ IDEA",
            Self::Eclipse => "Eclipse",
        }
    }

    pub fn display_name_i18n(&self, lang: UiLanguage) -> &'static str {
        match (self, lang) {
            (Self::IntelliJIdea, UiLanguage::ZhCn) => "IntelliJ IDEA（默认）",
            (Self::IntelliJIdea, UiLanguage::EnUs) => "IntelliJ IDEA (Default)",
            (Self::Eclipse, UiLanguage::ZhCn) => "Eclipse（经典快捷键）",
            (Self::Eclipse, UiLanguage::EnUs) => "Eclipse (Classic)",
        }
    }

    pub fn short_label(&self) -> &'static str {
        match self {
            Self::IntelliJIdea => "IDEA",
            Self::Eclipse => "Eclipse",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::IntelliJIdea => Self::Eclipse,
            Self::Eclipse => Self::IntelliJIdea,
        }
    }

    pub fn editor_find_label(&self) -> &'static str {
        "Ctrl+F"
    }

    pub fn editor_replace_label(&self) -> &'static str {
        match self {
            Self::IntelliJIdea => "Ctrl+R",
            Self::Eclipse => "Ctrl+F / Ctrl+R",
        }
    }

    pub fn go_to_line_label(&self) -> &'static str {
        match self {
            Self::IntelliJIdea => "Ctrl+G",
            Self::Eclipse => "Ctrl+L",
        }
    }

    pub fn debug_step_into_label(&self) -> &'static str {
        match self {
            Self::IntelliJIdea => "F7",
            Self::Eclipse => "F5",
        }
    }

    pub fn debug_step_over_label(&self) -> &'static str {
        match self {
            Self::IntelliJIdea => "F8",
            Self::Eclipse => "F6",
        }
    }

    pub fn debug_step_out_label(&self) -> &'static str {
        match self {
            Self::IntelliJIdea => "Shift+F8",
            Self::Eclipse => "F7",
        }
    }

    pub fn debug_continue_label(&self) -> &'static str {
        match self {
            Self::IntelliJIdea => "F9",
            Self::Eclipse => "F8",
        }
    }
}

/// IDEA 风格左右底部停靠工具窗口标识
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolWindowId {
    Project,
    Commit,
    Structure,
    AgentHub,
    Terminal,
    Problems,
    Debug,
    GitLog,
}

/// UI 界面语言切换（支持简体中文 / English 实时热切换）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UiLanguage {
    ZhCn,
    EnUs,
}

impl Default for UiLanguage {
    fn default() -> Self {
        Self::ZhCn
    }
}

impl UiLanguage {
    pub fn all() -> &'static [Self] {
        &[Self::ZhCn, Self::EnUs]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::ZhCn => "简体中文 (Chinese)",
            Self::EnUs => "English",
        }
    }

    pub fn short_label(&self) -> &'static str {
        match self {
            Self::ZhCn => "中文",
            Self::EnUs => "EN",
        }
    }

    pub fn toggle(&self) -> Self {
        match self {
            Self::ZhCn => Self::EnUs,
            Self::EnUs => Self::ZhCn,
        }
    }
}

/// 顶部主菜单分类（对齐 IntelliJ IDEA Main Menu，支持中英文国际化）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MainMenuCategory {
    File,
    Edit,
    View,
    Navigate,
    Code,
    Refactor,
    Run,
    Git,
    Tools,
    Window,
    Help,
}

impl MainMenuCategory {
    pub fn all() -> &'static [Self] {
        &[
            Self::File,
            Self::Edit,
            Self::View,
            Self::Navigate,
            Self::Code,
            Self::Refactor,
            Self::Run,
            Self::Git,
            Self::Tools,
            Self::Window,
            Self::Help,
        ]
    }

    pub fn label(&self) -> &'static str {
        self.label_i18n(UiLanguage::EnUs)
    }

    pub fn label_i18n(&self, lang: UiLanguage) -> &'static str {
        match (self, lang) {
            (Self::File, UiLanguage::ZhCn) => "文件",
            (Self::File, UiLanguage::EnUs) => "File",
            (Self::Edit, UiLanguage::ZhCn) => "编辑",
            (Self::Edit, UiLanguage::EnUs) => "Edit",
            (Self::View, UiLanguage::ZhCn) => "视图",
            (Self::View, UiLanguage::EnUs) => "View",
            (Self::Navigate, UiLanguage::ZhCn) => "导航",
            (Self::Navigate, UiLanguage::EnUs) => "Navigate",
            (Self::Code, UiLanguage::ZhCn) => "代码",
            (Self::Code, UiLanguage::EnUs) => "Code",
            (Self::Refactor, UiLanguage::ZhCn) => "重构",
            (Self::Refactor, UiLanguage::EnUs) => "Refactor",
            (Self::Run, UiLanguage::ZhCn) => "运行",
            (Self::Run, UiLanguage::EnUs) => "Run",
            (Self::Git, UiLanguage::ZhCn) => "Git",
            (Self::Git, UiLanguage::EnUs) => "Git",
            (Self::Tools, UiLanguage::ZhCn) => "工具",
            (Self::Tools, UiLanguage::EnUs) => "Tools",
            (Self::Window, UiLanguage::ZhCn) => "窗口",
            (Self::Window, UiLanguage::EnUs) => "Window",
            (Self::Help, UiLanguage::ZhCn) => "帮助",
            (Self::Help, UiLanguage::EnUs) => "Help",
        }
    }

    pub fn items(&self) -> Vec<IdeAction> {
        self.entries()
            .into_iter()
            .filter_map(|e| match e {
                MenuEntry::Action(a) => Some(a),
                MenuEntry::Separator => None,
            })
            .collect()
    }

    pub fn entries(&self) -> Vec<MenuEntry> {
        match self {
            Self::File => vec![
                MenuEntry::Action(IdeAction::OpenFolder),
                MenuEntry::Action(IdeAction::OpenFromGitRepo),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::NewFileInProject),
                MenuEntry::Action(IdeAction::NewFolderInProject),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::SaveActiveFile),
                MenuEntry::Action(IdeAction::CloseActiveTab),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::OpenSettingsModal),
            ],
            Self::Edit => vec![
                MenuEntry::Action(IdeAction::UndoEdit),
                MenuEntry::Action(IdeAction::RedoEdit),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::DuplicateLine),
                MenuEntry::Action(IdeAction::DeleteLine),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::FindInFiles),
            ],
            Self::View => vec![
                MenuEntry::Action(IdeAction::ToggleProjectWindow),
                MenuEntry::Action(IdeAction::ToggleCommitWindow),
                MenuEntry::Action(IdeAction::ToggleStructureWindow),
                MenuEntry::Action(IdeAction::ToggleAgentHubWindow),
                MenuEntry::Action(IdeAction::ToggleTerminalWindow),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::ZoomInUi),
                MenuEntry::Action(IdeAction::ZoomOutUi),
                MenuEntry::Action(IdeAction::ResetUiZoom),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::SwitchTheme),
                MenuEntry::Action(IdeAction::SwitchUiLanguage),
            ],
            Self::Navigate => vec![
                MenuEntry::Action(IdeAction::SearchEverywhere),
                MenuEntry::Action(IdeAction::GoToFile),
                MenuEntry::Action(IdeAction::FindAction),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::GoToDeclaration),
            ],
            Self::Code => vec![
                MenuEntry::Action(IdeAction::ToggleLineComment),
                MenuEntry::Action(IdeAction::ReformatCode),
            ],
            Self::Refactor => vec![MenuEntry::Action(IdeAction::RenameSymbol)],
            Self::Run => vec![
                MenuEntry::Action(IdeAction::RunActiveConfig),
                MenuEntry::Action(IdeAction::RunCurrentFile),
                MenuEntry::Action(IdeAction::DebugActiveConfig),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::ToggleBreakpoint),
            ],
            Self::Git => vec![
                MenuEntry::Action(IdeAction::GitCommitPopup),
                MenuEntry::Action(IdeAction::GitPush),
                MenuEntry::Action(IdeAction::GitPull),
                MenuEntry::Action(IdeAction::GitFetch),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::GitMerge),
                MenuEntry::Action(IdeAction::GitRebase),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::GitBranches),
                MenuEntry::Action(IdeAction::GitNewBranch),
                MenuEntry::Action(IdeAction::GitNewTag),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::GitStashChanges),
                MenuEntry::Action(IdeAction::GitUnstashChanges),
                MenuEntry::Action(IdeAction::GitResetHead),
                MenuEntry::Action(IdeAction::GitRollback),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::GitShowDiff),
                MenuEntry::Action(IdeAction::GitAnnotateBlame),
                MenuEntry::Action(IdeAction::GitFileHistory),
                MenuEntry::Action(IdeAction::GitRollbackCurrentFile),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::GitShowLog),
                MenuEntry::Action(IdeAction::GitManageRemotes),
            ],
            Self::Tools => vec![
                MenuEntry::Action(IdeAction::CheckMobileDevicesAndSdk),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::SwitchToAntigravity),
                MenuEntry::Action(IdeAction::SwitchToClaudeCode),
                MenuEntry::Action(IdeAction::SwitchToCodexCli),
            ],
            Self::Window => vec![
                MenuEntry::Action(IdeAction::WidenLeftPanel),
                MenuEntry::Action(IdeAction::NarrowLeftPanel),
                MenuEntry::Action(IdeAction::WidenRightPanel),
                MenuEntry::Action(IdeAction::RaiseBottomPanel),
            ],
            Self::Help => vec![
                MenuEntry::Action(IdeAction::FindAction),
                MenuEntry::Separator,
                MenuEntry::Action(IdeAction::SwitchUiLanguage),
                MenuEntry::Action(IdeAction::OpenSettingsModal),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuEntry {
    Action(IdeAction),
    Separator,
}

/// IDE 全局快捷键与 Action 命令定义（100% 对齐 IntelliJ IDEA + Cursor）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IdeAction {
    // 导航与全局搜索
    SearchEverywhere,        // Double Shift
    FindAction,              // Ctrl+Shift+A
    GoToFile,                // Ctrl+Shift+N / Ctrl+P
    FindInFiles,             // Ctrl+Shift+F
    GoToDeclaration,         // Ctrl+B
    // 文件与工作区/项目树操作
    OpenFolder,              // Ctrl+O
    OpenFromGitRepo,
    NewFileInProject,        // Alt+Insert / Ctrl+N
    NewFolderInProject,      // Ctrl+Alt+N
    DeleteSelectedPath,      // Delete
    SaveActiveFile,          // Ctrl+S
    CloseActiveTab,          // Ctrl+F4 / Ctrl+W
    // 编辑器经典快捷键
    UndoEdit,                // Ctrl+Z
    RedoEdit,                // Ctrl+Shift+Z / Ctrl+Y
    DuplicateLine,           // Ctrl+D (IDEA 经典整行复制)
    DeleteLine,              // Ctrl+Y (IDEA 经典整行删除)
    ToggleLineComment,       // Ctrl+/
    ReformatCode,            // Ctrl+Alt+L (IDEA 格式化代码)
    RenameSymbol,            // Shift+F6
    ShowIntentionActions,    // Alt+Enter
    // AI / Multi-Agent 快捷键
    InlineAiEdit,            // Ctrl+K
    SendSelectionToAgent,    // Alt+L
    AcceptInlineDiff,        // Tab
    RejectInlineDiff,        // Esc
    RevertLastCheckpoint,    // Ctrl+Alt+Z
    SwitchToAntigravity,
    SwitchToClaudeCode,
    SwitchToCodexCli,
    // 运行与调试 (Run / Debug)
    RunActiveConfig,         // Shift+F10
    RunCurrentFile,          // Ctrl+Shift+F10
    DebugActiveConfig,       // Shift+F9
    ToggleBreakpoint,        // Ctrl+F8
    CheckMobileDevicesAndSdk,
    // Git 操作 (VCS — 对齐 IntelliJ IDEA 全套 Git 菜单)
    GitCommitPopup,          // Alt+0
    GitPull,                 // Ctrl+T
    GitPush,                 // Ctrl+Shift+K
    GitFetch,
    GitMerge,
    GitRebase,
    GitBranches,             // Ctrl+Shift+G
    GitNewBranch,            // Ctrl+Alt+B
    GitNewTag,
    GitStashChanges,
    GitUnstashChanges,
    GitResetHead,
    GitRollback,
    GitShowDiff,
    GitAnnotateBlame,
    GitFileHistory,
    GitRollbackCurrentFile,
    GitShowLog,              // Alt+9
    GitManageRemotes,
    // 工具窗口与设置
    ToggleProjectWindow,     // Alt+1
    ToggleCommitWindow,      // Alt+0
    ToggleStructureWindow,   // Alt+7
    ToggleAgentHubWindow,    // Alt+8
    ToggleTerminalWindow,    // Alt+F12
    OpenSettingsModal,       // Ctrl+Alt+S
    SwitchTheme,             // Ctrl+`
    SwitchUiLanguage,        // Ctrl+Alt+U
    ZoomInUi,                // Ctrl+=
    ZoomOutUi,               // Ctrl+-
    ResetUiZoom,             // Ctrl+0
    WidenLeftPanel,
    NarrowLeftPanel,
    WidenRightPanel,
    RaiseBottomPanel,
}

impl IdeAction {
    pub fn all() -> &'static [Self] {
        &[
            Self::OpenFolder,
            Self::OpenFromGitRepo,
            Self::SearchEverywhere,
            Self::FindAction,
            Self::GoToFile,
            Self::FindInFiles,
            Self::GoToDeclaration,
            Self::NewFileInProject,
            Self::NewFolderInProject,
            Self::SaveActiveFile,
            Self::CloseActiveTab,
            Self::UndoEdit,
            Self::RedoEdit,
            Self::DuplicateLine,
            Self::DeleteLine,
            Self::ToggleLineComment,
            Self::ReformatCode,
            Self::RenameSymbol,
            Self::ShowIntentionActions,
            Self::InlineAiEdit,
            Self::SendSelectionToAgent,
            Self::AcceptInlineDiff,
            Self::RejectInlineDiff,
            Self::RevertLastCheckpoint,
            Self::SwitchToAntigravity,
            Self::SwitchToClaudeCode,
            Self::SwitchToCodexCli,
            Self::RunActiveConfig,
            Self::RunCurrentFile,
            Self::DebugActiveConfig,
            Self::ToggleBreakpoint,
            Self::CheckMobileDevicesAndSdk,
            Self::GitCommitPopup,
            Self::GitPull,
            Self::GitPush,
            Self::GitFetch,
            Self::GitMerge,
            Self::GitRebase,
            Self::GitBranches,
            Self::GitNewBranch,
            Self::GitNewTag,
            Self::GitStashChanges,
            Self::GitUnstashChanges,
            Self::GitResetHead,
            Self::GitRollback,
            Self::GitShowDiff,
            Self::GitAnnotateBlame,
            Self::GitFileHistory,
            Self::GitRollbackCurrentFile,
            Self::GitShowLog,
            Self::GitManageRemotes,
            Self::ToggleProjectWindow,
            Self::ToggleCommitWindow,
            Self::ToggleStructureWindow,
            Self::ToggleAgentHubWindow,
            Self::ToggleTerminalWindow,
            Self::OpenSettingsModal,
            Self::SwitchTheme,
            Self::SwitchUiLanguage,
            Self::ZoomInUi,
            Self::ZoomOutUi,
            Self::ResetUiZoom,
        ]
    }

    pub fn shortcut_label(&self) -> &'static str {
        self.shortcut_label_for(KeymapScheme::IntelliJIdea)
    }

    pub fn shortcut_label_for(&self, scheme: KeymapScheme) -> &'static str {
        match scheme {
            KeymapScheme::IntelliJIdea => match self {
                Self::SearchEverywhere => "Double Shift",
                Self::FindAction => "Ctrl+Shift+A",
                Self::GoToFile => "Ctrl+Shift+N",
                Self::FindInFiles => "Ctrl+Shift+F",
                Self::GoToDeclaration => "Ctrl+B",
                Self::OpenFolder => "Ctrl+O",
                Self::OpenFromGitRepo => "",
                Self::NewFileInProject => "Alt+Insert",
                Self::NewFolderInProject => "Ctrl+Alt+N",
                Self::DeleteSelectedPath => "Delete",
                Self::SaveActiveFile => "Ctrl+S",
                Self::CloseActiveTab => "Ctrl+F4",
                Self::UndoEdit => "Ctrl+Z",
                Self::RedoEdit => "Ctrl+Shift+Z",
                Self::DuplicateLine => "Ctrl+D",
                Self::DeleteLine => "Ctrl+Y",
                Self::ToggleLineComment => "Ctrl+/",
                Self::ReformatCode => "Ctrl+Alt+L",
                Self::RenameSymbol => "Shift+F6",
                Self::ShowIntentionActions => "Alt+Enter",
                Self::InlineAiEdit => "Ctrl+K",
                Self::SendSelectionToAgent => "Alt+L",
                Self::AcceptInlineDiff => "Tab",
                Self::RejectInlineDiff => "Esc",
                Self::RevertLastCheckpoint => "Ctrl+Alt+Z",
                Self::SwitchToAntigravity => "Alt+G",
                Self::SwitchToClaudeCode => "Alt+C",
                Self::SwitchToCodexCli => "Alt+X",
                Self::RunActiveConfig => "Shift+F10",
                Self::RunCurrentFile => "Ctrl+Shift+F10",
                Self::DebugActiveConfig => "Shift+F9",
                Self::ToggleBreakpoint => "Ctrl+F8",
                Self::CheckMobileDevicesAndSdk => "",
                Self::GitCommitPopup => "Alt+0",
                Self::GitPull => "Ctrl+T",
                Self::GitPush => "Ctrl+Shift+K",
                Self::GitFetch => "",
                Self::GitMerge => "",
                Self::GitRebase => "",
                Self::GitBranches => "Ctrl+Shift+G",
                Self::GitNewBranch => "Ctrl+Alt+B",
                Self::GitNewTag => "",
                Self::GitStashChanges => "",
                Self::GitUnstashChanges => "",
                Self::GitResetHead => "",
                Self::GitRollback => "",
                Self::GitShowDiff => "",
                Self::GitAnnotateBlame => "",
                Self::GitFileHistory => "",
                Self::GitRollbackCurrentFile => "",
                Self::GitShowLog => "Alt+9",
                Self::GitManageRemotes => "",
                Self::ToggleProjectWindow => "Alt+1",
                Self::ToggleCommitWindow => "Alt+0",
                Self::ToggleStructureWindow => "Alt+7",
                Self::ToggleAgentHubWindow => "Alt+8",
                Self::ToggleTerminalWindow => "Alt+F12",
                Self::OpenSettingsModal => "Ctrl+Alt+S",
                Self::SwitchTheme => "Ctrl+`",
                Self::SwitchUiLanguage => "Ctrl+Alt+U",
                Self::ZoomInUi => "Ctrl+=",
                Self::ZoomOutUi => "Ctrl+-",
                Self::ResetUiZoom => "Ctrl+0",
                Self::WidenLeftPanel => "Ctrl+Shift+Right",
                Self::NarrowLeftPanel => "Ctrl+Shift+Left",
                Self::WidenRightPanel => "Ctrl+Alt+Left",
                Self::RaiseBottomPanel => "Ctrl+Shift+Up",
            },
            KeymapScheme::Eclipse => match self {
                Self::SearchEverywhere => "Ctrl+3",
                Self::FindAction => "Ctrl+Shift+L",
                Self::GoToFile => "Ctrl+Shift+R",
                Self::FindInFiles => "Ctrl+H",
                Self::GoToDeclaration => "F3",
                Self::OpenFolder => "Ctrl+O",
                Self::OpenFromGitRepo => "",
                Self::NewFileInProject => "Ctrl+N",
                Self::NewFolderInProject => "Alt+Shift+N",
                Self::DeleteSelectedPath => "Delete",
                Self::SaveActiveFile => "Ctrl+S",
                Self::CloseActiveTab => "Ctrl+W",
                Self::UndoEdit => "Ctrl+Z",
                Self::RedoEdit => "Ctrl+Y",
                Self::DuplicateLine => "Ctrl+Alt+Down",
                Self::DeleteLine => "Ctrl+D",
                Self::ToggleLineComment => "Ctrl+/",
                Self::ReformatCode => "Ctrl+Shift+F",
                Self::RenameSymbol => "Alt+Shift+R",
                Self::ShowIntentionActions => "Ctrl+1",
                Self::InlineAiEdit => "Ctrl+K",
                Self::SendSelectionToAgent => "Alt+L",
                Self::AcceptInlineDiff => "Tab",
                Self::RejectInlineDiff => "Esc",
                Self::RevertLastCheckpoint => "Ctrl+Alt+Z",
                Self::SwitchToAntigravity => "Alt+G",
                Self::SwitchToClaudeCode => "Alt+C",
                Self::SwitchToCodexCli => "Alt+X",
                Self::RunActiveConfig => "Ctrl+F11",
                Self::RunCurrentFile => "Alt+Shift+X",
                Self::DebugActiveConfig => "F11",
                Self::ToggleBreakpoint => "Ctrl+Shift+B",
                Self::CheckMobileDevicesAndSdk => "",
                Self::GitCommitPopup => "Alt+0",
                Self::GitPull => "Ctrl+T",
                Self::GitPush => "Ctrl+Shift+K",
                Self::GitFetch => "",
                Self::GitMerge => "",
                Self::GitRebase => "",
                Self::GitBranches => "Ctrl+Shift+G",
                Self::GitNewBranch => "Ctrl+Alt+B",
                Self::GitNewTag => "",
                Self::GitStashChanges => "",
                Self::GitUnstashChanges => "",
                Self::GitResetHead => "",
                Self::GitRollback => "",
                Self::GitShowDiff => "",
                Self::GitAnnotateBlame => "",
                Self::GitFileHistory => "",
                Self::GitRollbackCurrentFile => "",
                Self::GitShowLog => "Alt+9",
                Self::GitManageRemotes => "",
                Self::ToggleProjectWindow => "Alt+1",
                Self::ToggleCommitWindow => "Alt+0",
                Self::ToggleStructureWindow => "Alt+7",
                Self::ToggleAgentHubWindow => "Alt+8",
                Self::ToggleTerminalWindow => "Alt+F12",
                Self::OpenSettingsModal => "Ctrl+Alt+S",
                Self::SwitchTheme => "Ctrl+`",
                Self::SwitchUiLanguage => "Ctrl+Alt+U",
                Self::ZoomInUi => "Ctrl+=",
                Self::ZoomOutUi => "Ctrl+-",
                Self::ResetUiZoom => "Ctrl+0",
                Self::WidenLeftPanel => "Ctrl+Shift+Right",
                Self::NarrowLeftPanel => "Ctrl+Shift+Left",
                Self::WidenRightPanel => "Ctrl+Alt+Left",
                Self::RaiseBottomPanel => "Ctrl+Shift+Up",
            },
        }
    }

    pub fn title(&self) -> &'static str {
        self.title_i18n(UiLanguage::ZhCn)
    }

    pub fn title_i18n(&self, lang: UiLanguage) -> &'static str {
        match (self, lang) {
            (Self::OpenFolder, UiLanguage::ZhCn) => "打开文件夹... (切换并加载其他本地项目工作区)",
            (Self::OpenFolder, UiLanguage::EnUs) => "Open Folder... (Switch to another project workspace)",
            (Self::OpenFromGitRepo, UiLanguage::ZhCn) => "从 Git 仓库打开... (通过远程 Git 地址克隆并打开)",
            (Self::OpenFromGitRepo, UiLanguage::EnUs) => "Open from Git Repository... (Clone from remote URL)",
            (Self::SearchEverywhere, UiLanguage::ZhCn) => "随处搜索 Search Everywhere (全局搜索文件/符号/命令)",
            (Self::SearchEverywhere, UiLanguage::EnUs) => "Search Everywhere (Files / Symbols / Actions)",
            (Self::FindAction, UiLanguage::ZhCn) => "查找动作 Find Action... (查找并执行 IDE 动作)",
            (Self::FindAction, UiLanguage::EnUs) => "Find Action... (Search & Run IDE Commands)",
            (Self::GoToFile, UiLanguage::ZhCn) => "转到文件 Go to File... (按文件名快速跳转)",
            (Self::GoToFile, UiLanguage::EnUs) => "Go to File... (Quick Jump by File Name)",
            (Self::FindInFiles, UiLanguage::ZhCn) => "在文件中查找 Find in Files... (Ripgrep 全局文本搜索)",
            (Self::FindInFiles, UiLanguage::EnUs) => "Find in Files... (Ripgrep Workspace Text Search)",
            (Self::GoToDeclaration, UiLanguage::ZhCn) => "转到声明或用法 Go to Declaration",
            (Self::GoToDeclaration, UiLanguage::EnUs) => "Go to Declaration or Usages",
            (Self::NewFileInProject, UiLanguage::ZhCn) => "新建文件 New File... (在当前目录下新建源码文件)",
            (Self::NewFileInProject, UiLanguage::EnUs) => "New File... (Create File in Active Directory)",
            (Self::NewFolderInProject, UiLanguage::ZhCn) => "新建目录 New Directory... (在当前目录下新建文件夹)",
            (Self::NewFolderInProject, UiLanguage::EnUs) => "New Directory... (Create Folder in Active Directory)",
            (Self::DeleteSelectedPath, UiLanguage::ZhCn) => "删除文件/目录 Delete",
            (Self::DeleteSelectedPath, UiLanguage::EnUs) => "Delete File/Directory",
            (Self::SaveActiveFile, UiLanguage::ZhCn) => "全部保存 Save All (保存当前及所有修改)",
            (Self::SaveActiveFile, UiLanguage::EnUs) => "Save All Documents",
            (Self::CloseActiveTab, UiLanguage::ZhCn) => "关闭当前标签页 Close Tab",
            (Self::CloseActiveTab, UiLanguage::EnUs) => "Close Active Editor Tab",
            (Self::UndoEdit, UiLanguage::ZhCn) => "撤销 Undo (撤销上一步编辑)",
            (Self::UndoEdit, UiLanguage::EnUs) => "Undo Last Edit",
            (Self::RedoEdit, UiLanguage::ZhCn) => "重做 Redo (重做编辑)",
            (Self::RedoEdit, UiLanguage::EnUs) => "Redo Edit",
            (Self::DuplicateLine, UiLanguage::ZhCn) => "复制当前行 Duplicate Line",
            (Self::DuplicateLine, UiLanguage::EnUs) => "Duplicate Line or Selection",
            (Self::DeleteLine, UiLanguage::ZhCn) => "删除当前行 Delete Line",
            (Self::DeleteLine, UiLanguage::EnUs) => "Delete Line at Caret",
            (Self::ToggleLineComment, UiLanguage::ZhCn) => "切换行注释 Line Comment",
            (Self::ToggleLineComment, UiLanguage::EnUs) => "Comment with Line Comment",
            (Self::ReformatCode, UiLanguage::ZhCn) => "格式化代码 Reformat Code",
            (Self::ReformatCode, UiLanguage::EnUs) => "Reformat Code",
            (Self::RenameSymbol, UiLanguage::ZhCn) => "重命名符号 Rename...",
            (Self::RenameSymbol, UiLanguage::EnUs) => "Rename Symbol...",
            (Self::ShowIntentionActions, UiLanguage::ZhCn) => "显示意图动作与 AI 修复",
            (Self::ShowIntentionActions, UiLanguage::EnUs) => "Show Context Actions & AI Fix",
            (Self::InlineAiEdit, UiLanguage::ZhCn) => "AI 行内改写 Inline AI Edit (Ctrl+K Diff)",
            (Self::InlineAiEdit, UiLanguage::EnUs) => "Inline AI Edit (Ctrl+K Diff)",
            (Self::SendSelectionToAgent, UiLanguage::ZhCn) => "发送选中代码至 AI 助手",
            (Self::SendSelectionToAgent, UiLanguage::EnUs) => "Send Selection to Active Agent",
            (Self::AcceptInlineDiff, UiLanguage::ZhCn) => "接受 AI 行内修改",
            (Self::AcceptInlineDiff, UiLanguage::EnUs) => "Accept AI Proposed Hunk Diff",
            (Self::RejectInlineDiff, UiLanguage::ZhCn) => "拒绝 AI 行内修改",
            (Self::RejectInlineDiff, UiLanguage::EnUs) => "Reject AI Proposed Hunk Diff",
            (Self::RevertLastCheckpoint, UiLanguage::ZhCn) => "回滚到上一个快照 Checkpoint",
            (Self::RevertLastCheckpoint, UiLanguage::EnUs) => "Revert to Last Checkpoint",
            (Self::SwitchToAntigravity, UiLanguage::ZhCn) => "切换智能体: Google Antigravity (ACP)",
            (Self::SwitchToAntigravity, UiLanguage::EnUs) => "Switch Agent: Google Antigravity (ACP)",
            (Self::SwitchToClaudeCode, UiLanguage::ZhCn) => "切换智能体: Claude Code CLI",
            (Self::SwitchToClaudeCode, UiLanguage::EnUs) => "Switch Agent: Claude Code CLI",
            (Self::SwitchToCodexCli, UiLanguage::ZhCn) => "切换智能体: OpenAI Codex CLI",
            (Self::SwitchToCodexCli, UiLanguage::EnUs) => "Switch Agent: OpenAI Codex CLI",
            (Self::RunActiveConfig, UiLanguage::ZhCn) => "运行当前配置 Run",
            (Self::RunActiveConfig, UiLanguage::EnUs) => "Run Active Configuration",
            (Self::RunCurrentFile, UiLanguage::ZhCn) => "运行当前文件 Run File",
            (Self::RunCurrentFile, UiLanguage::EnUs) => "Run Current File",
            (Self::DebugActiveConfig, UiLanguage::ZhCn) => "调试当前配置 Debug (DAP)",
            (Self::DebugActiveConfig, UiLanguage::EnUs) => "Debug Active Configuration (DAP)",
            (Self::ToggleBreakpoint, UiLanguage::ZhCn) => "切换行断点 Toggle Breakpoint",
            (Self::ToggleBreakpoint, UiLanguage::EnUs) => "Toggle Line Breakpoint",
            (Self::CheckMobileDevicesAndSdk, UiLanguage::ZhCn) => "移动端设备与 SDK 检测 (ADB / Flutter)...",
            (Self::CheckMobileDevicesAndSdk, UiLanguage::EnUs) => "Mobile Devices & SDK Check (ADB / Flutter)...",
            (Self::GitCommitPopup, UiLanguage::ZhCn) => "Git 提交 Commit...",
            (Self::GitCommitPopup, UiLanguage::EnUs) => "Commit...",
            (Self::GitPull, UiLanguage::ZhCn) => "更新项目 Update Project (Git Pull)",
            (Self::GitPull, UiLanguage::EnUs) => "Update Project (Git Pull)",
            (Self::GitPush, UiLanguage::ZhCn) => "推送代码 Git Push...",
            (Self::GitPush, UiLanguage::EnUs) => "Git Push...",
            (Self::GitFetch, UiLanguage::ZhCn) => "获取全部远程 Git Fetch",
            (Self::GitFetch, UiLanguage::EnUs) => "Git Fetch All Remotes",
            (Self::GitMerge, UiLanguage::ZhCn) => "合并分支 Git Merge...",
            (Self::GitMerge, UiLanguage::EnUs) => "Git Merge Branches...",
            (Self::GitRebase, UiLanguage::ZhCn) => "变基 Git Rebase...",
            (Self::GitRebase, UiLanguage::EnUs) => "Git Rebase...",
            (Self::GitBranches, UiLanguage::ZhCn) => "管理分支 Git Branches...",
            (Self::GitBranches, UiLanguage::EnUs) => "Git Branches...",
            (Self::GitNewBranch, UiLanguage::ZhCn) => "新建分支 Git New Branch...",
            (Self::GitNewBranch, UiLanguage::EnUs) => "Git New Branch...",
            (Self::GitNewTag, UiLanguage::ZhCn) => "新建标签 Git New Tag...",
            (Self::GitNewTag, UiLanguage::EnUs) => "Git New Tag...",
            (Self::GitStashChanges, UiLanguage::ZhCn) => "暂存工作区更改 Stash Changes...",
            (Self::GitStashChanges, UiLanguage::EnUs) => "Stash Changes...",
            (Self::GitUnstashChanges, UiLanguage::ZhCn) => "恢复暂存更改 Unstash Changes...",
            (Self::GitUnstashChanges, UiLanguage::EnUs) => "Unstash Changes...",
            (Self::GitResetHead, UiLanguage::ZhCn) => "重置提交指针 Reset HEAD...",
            (Self::GitResetHead, UiLanguage::EnUs) => "Reset HEAD...",
            (Self::GitRollback, UiLanguage::ZhCn) => "回滚未提交变更 Rollback...",
            (Self::GitRollback, UiLanguage::EnUs) => "Rollback Uncommitted Changes...",
            (Self::GitShowDiff, UiLanguage::ZhCn) => "对比当前文件与版本库 Diff",
            (Self::GitShowDiff, UiLanguage::EnUs) => "Compare Active File with HEAD",
            (Self::GitAnnotateBlame, UiLanguage::ZhCn) => "切换逐行追溯 Git Blame",
            (Self::GitAnnotateBlame, UiLanguage::EnUs) => "Annotate with Git Blame",
            (Self::GitFileHistory, UiLanguage::ZhCn) => "查看当前文件提交历史 File History",
            (Self::GitFileHistory, UiLanguage::EnUs) => "Show History for Active File",
            (Self::GitRollbackCurrentFile, UiLanguage::ZhCn) => "回滚当前文件至 HEAD",
            (Self::GitRollbackCurrentFile, UiLanguage::EnUs) => "Rollback Active File to HEAD",
            (Self::GitShowLog, UiLanguage::ZhCn) => "显示 Git 历史库 Tool Window",
            (Self::GitShowLog, UiLanguage::EnUs) => "Show Git Log Tool Window",
            (Self::GitManageRemotes, UiLanguage::ZhCn) => "管理远程仓库 Remotes...",
            (Self::GitManageRemotes, UiLanguage::EnUs) => "Manage Git Remotes...",
            (Self::ToggleProjectWindow, UiLanguage::ZhCn) => "项目工具窗口 Project",
            (Self::ToggleProjectWindow, UiLanguage::EnUs) => "Project Tool Window",
            (Self::ToggleCommitWindow, UiLanguage::ZhCn) => "Git 提交工具窗口 Commit",
            (Self::ToggleCommitWindow, UiLanguage::EnUs) => "Commit Tool Window",
            (Self::ToggleStructureWindow, UiLanguage::ZhCn) => "代码结构工具窗口 Structure",
            (Self::ToggleStructureWindow, UiLanguage::EnUs) => "Structure Tool Window",
            (Self::ToggleAgentHubWindow, UiLanguage::ZhCn) => "AI 助手工具窗口 AI Assistant",
            (Self::ToggleAgentHubWindow, UiLanguage::EnUs) => "AI Assistant Tool Window",
            (Self::ToggleTerminalWindow, UiLanguage::ZhCn) => "终端工具窗口 Terminal",
            (Self::ToggleTerminalWindow, UiLanguage::EnUs) => "Terminal Tool Window",
            (Self::OpenSettingsModal, UiLanguage::ZhCn) => "设置 Settings... (语言、主题与快捷键)",
            (Self::OpenSettingsModal, UiLanguage::EnUs) => "Settings... (Language, Theme & Keymap)",
            (Self::SwitchTheme, UiLanguage::ZhCn) => "快速切换主题 Switch Theme",
            (Self::SwitchTheme, UiLanguage::EnUs) => "Quick Switch Theme",
            (Self::SwitchUiLanguage, UiLanguage::ZhCn) => "切换界面语言 (中文 / English)",
            (Self::SwitchUiLanguage, UiLanguage::EnUs) => "Switch UI Language (English / 中文)",
            (Self::ZoomInUi, UiLanguage::ZhCn) => "放大界面与字体 Zoom In UI (+10%)",
            (Self::ZoomInUi, UiLanguage::EnUs) => "Zoom In UI & Fonts (+10%)",
            (Self::ZoomOutUi, UiLanguage::ZhCn) => "缩小界面与字体 Zoom Out UI (-10%)",
            (Self::ZoomOutUi, UiLanguage::EnUs) => "Zoom Out UI & Fonts (-10%)",
            (Self::ResetUiZoom, UiLanguage::ZhCn) => "重置界面缩放 Reset UI Zoom (100% 跟随系统 DPI)",
            (Self::ResetUiZoom, UiLanguage::EnUs) => "Reset UI Zoom (100% System DPI)",
            (Self::WidenLeftPanel, UiLanguage::ZhCn) => "加宽左侧面板",
            (Self::WidenLeftPanel, UiLanguage::EnUs) => "Stretch Left Panel Right",
            (Self::NarrowLeftPanel, UiLanguage::ZhCn) => "缩窄左侧面板",
            (Self::NarrowLeftPanel, UiLanguage::EnUs) => "Stretch Left Panel Left",
            (Self::WidenRightPanel, UiLanguage::ZhCn) => "加宽右侧 AI 面板",
            (Self::WidenRightPanel, UiLanguage::EnUs) => "Stretch Right Panel Left",
            (Self::RaiseBottomPanel, UiLanguage::ZhCn) => "加高底部工具面板",
            (Self::RaiseBottomPanel, UiLanguage::EnUs) => "Stretch Bottom Panel Up",
        }
    }

    pub fn menu_title(&self) -> &'static str {
        self.menu_title_i18n(UiLanguage::EnUs)
    }

    pub fn menu_title_i18n(&self, lang: UiLanguage) -> &'static str {
        match (self, lang) {
            (Self::OpenFolder, UiLanguage::ZhCn) => "打开文件夹...",
            (Self::OpenFolder, UiLanguage::EnUs) => "Open Folder...",
            (Self::OpenFromGitRepo, UiLanguage::ZhCn) => "从 Git 仓库打开...",
            (Self::OpenFromGitRepo, UiLanguage::EnUs) => "Open from Git Repository...",
            (Self::SearchEverywhere, UiLanguage::ZhCn) => "随处搜索...",
            (Self::SearchEverywhere, UiLanguage::EnUs) => "Search Everywhere...",
            (Self::FindAction, UiLanguage::ZhCn) => "查找动作...",
            (Self::FindAction, UiLanguage::EnUs) => "Find Action...",
            (Self::GoToFile, UiLanguage::ZhCn) => "转到文件...",
            (Self::GoToFile, UiLanguage::EnUs) => "Go to File...",
            (Self::FindInFiles, UiLanguage::ZhCn) => "在文件中查找...",
            (Self::FindInFiles, UiLanguage::EnUs) => "Find in Files...",
            (Self::GoToDeclaration, UiLanguage::ZhCn) => "转到声明或用法",
            (Self::GoToDeclaration, UiLanguage::EnUs) => "Go to Declaration or Usages",
            (Self::NewFileInProject, UiLanguage::ZhCn) => "新建文件...",
            (Self::NewFileInProject, UiLanguage::EnUs) => "New File...",
            (Self::NewFolderInProject, UiLanguage::ZhCn) => "新建文件夹...",
            (Self::NewFolderInProject, UiLanguage::EnUs) => "New Directory...",
            (Self::DeleteSelectedPath, UiLanguage::ZhCn) => "删除...",
            (Self::DeleteSelectedPath, UiLanguage::EnUs) => "Delete...",
            (Self::SaveActiveFile, UiLanguage::ZhCn) => "全部保存",
            (Self::SaveActiveFile, UiLanguage::EnUs) => "Save All",
            (Self::CloseActiveTab, UiLanguage::ZhCn) => "关闭标签页",
            (Self::CloseActiveTab, UiLanguage::EnUs) => "Close Tab",
            (Self::UndoEdit, UiLanguage::ZhCn) => "撤销",
            (Self::UndoEdit, UiLanguage::EnUs) => "Undo",
            (Self::RedoEdit, UiLanguage::ZhCn) => "重做",
            (Self::RedoEdit, UiLanguage::EnUs) => "Redo",
            (Self::DuplicateLine, UiLanguage::ZhCn) => "复制当前行或选区",
            (Self::DuplicateLine, UiLanguage::EnUs) => "Duplicate Line or Selection",
            (Self::DeleteLine, UiLanguage::ZhCn) => "删除当前行",
            (Self::DeleteLine, UiLanguage::EnUs) => "Delete Line",
            (Self::ToggleLineComment, UiLanguage::ZhCn) => "使用行注释",
            (Self::ToggleLineComment, UiLanguage::EnUs) => "Comment with Line Comment",
            (Self::ReformatCode, UiLanguage::ZhCn) => "重新格式化代码",
            (Self::ReformatCode, UiLanguage::EnUs) => "Reformat Code",
            (Self::RenameSymbol, UiLanguage::ZhCn) => "重命名...",
            (Self::RenameSymbol, UiLanguage::EnUs) => "Rename...",
            (Self::ShowIntentionActions, UiLanguage::ZhCn) => "显示上下文操作",
            (Self::ShowIntentionActions, UiLanguage::EnUs) => "Show Context Actions",
            (Self::InlineAiEdit, UiLanguage::ZhCn) => "AI 行内编辑 (Ctrl+K)",
            (Self::InlineAiEdit, UiLanguage::EnUs) => "Inline AI Edit (Ctrl+K)",
            (Self::SendSelectionToAgent, UiLanguage::ZhCn) => "询问 AI 助手",
            (Self::SendSelectionToAgent, UiLanguage::EnUs) => "Ask AI Assistant",
            (Self::AcceptInlineDiff, UiLanguage::ZhCn) => "接受行内 Diff",
            (Self::AcceptInlineDiff, UiLanguage::EnUs) => "Accept Inline Diff",
            (Self::RejectInlineDiff, UiLanguage::ZhCn) => "拒绝行内 Diff",
            (Self::RejectInlineDiff, UiLanguage::EnUs) => "Reject Inline Diff",
            (Self::RevertLastCheckpoint, UiLanguage::ZhCn) => "回滚至上次快照",
            (Self::RevertLastCheckpoint, UiLanguage::EnUs) => "Revert to Last Checkpoint",
            (Self::SwitchToAntigravity, UiLanguage::ZhCn) => "使用 Google Antigravity (ACP)",
            (Self::SwitchToAntigravity, UiLanguage::EnUs) => "Use Google Antigravity (ACP)",
            (Self::SwitchToClaudeCode, UiLanguage::ZhCn) => "使用 Claude Code CLI",
            (Self::SwitchToClaudeCode, UiLanguage::EnUs) => "Use Claude Code CLI",
            (Self::SwitchToCodexCli, UiLanguage::ZhCn) => "使用 OpenAI Codex CLI",
            (Self::SwitchToCodexCli, UiLanguage::EnUs) => "Use OpenAI Codex CLI",
            (Self::RunActiveConfig, UiLanguage::ZhCn) => "运行当前配置",
            (Self::RunActiveConfig, UiLanguage::EnUs) => "Run Active Configuration",
            (Self::RunCurrentFile, UiLanguage::ZhCn) => "运行当前文件",
            (Self::RunCurrentFile, UiLanguage::EnUs) => "Run Current File",
            (Self::DebugActiveConfig, UiLanguage::ZhCn) => "调试当前配置",
            (Self::DebugActiveConfig, UiLanguage::EnUs) => "Debug Active Configuration",
            (Self::ToggleBreakpoint, UiLanguage::ZhCn) => "切换行断点",
            (Self::ToggleBreakpoint, UiLanguage::EnUs) => "Toggle Line Breakpoint",
            (Self::CheckMobileDevicesAndSdk, UiLanguage::ZhCn) => "移动端设备与 SDK 检测...",
            (Self::CheckMobileDevicesAndSdk, UiLanguage::EnUs) => "Mobile Devices & SDK...",
            (Self::GitCommitPopup, UiLanguage::ZhCn) => "提交...",
            (Self::GitCommitPopup, UiLanguage::EnUs) => "Commit...",
            (Self::GitPull, UiLanguage::ZhCn) => "更新项目...",
            (Self::GitPull, UiLanguage::EnUs) => "Update Project...",
            (Self::GitPush, UiLanguage::ZhCn) => "推送...",
            (Self::GitPush, UiLanguage::EnUs) => "Push...",
            (Self::GitFetch, UiLanguage::ZhCn) => "获取 (Fetch)",
            (Self::GitFetch, UiLanguage::EnUs) => "Fetch",
            (Self::GitMerge, UiLanguage::ZhCn) => "合并...",
            (Self::GitMerge, UiLanguage::EnUs) => "Merge...",
            (Self::GitRebase, UiLanguage::ZhCn) => "变基...",
            (Self::GitRebase, UiLanguage::EnUs) => "Rebase...",
            (Self::GitBranches, UiLanguage::ZhCn) => "分支...",
            (Self::GitBranches, UiLanguage::EnUs) => "Branches...",
            (Self::GitNewBranch, UiLanguage::ZhCn) => "新建分支...",
            (Self::GitNewBranch, UiLanguage::EnUs) => "New Branch...",
            (Self::GitNewTag, UiLanguage::ZhCn) => "新建标签...",
            (Self::GitNewTag, UiLanguage::EnUs) => "New Tag...",
            (Self::GitStashChanges, UiLanguage::ZhCn) => "暂存更改 (Stash)...",
            (Self::GitStashChanges, UiLanguage::EnUs) => "Stash Changes...",
            (Self::GitUnstashChanges, UiLanguage::ZhCn) => "恢复暂存 (Unstash)...",
            (Self::GitUnstashChanges, UiLanguage::EnUs) => "Unstash Changes...",
            (Self::GitResetHead, UiLanguage::ZhCn) => "重置 HEAD...",
            (Self::GitResetHead, UiLanguage::EnUs) => "Reset HEAD...",
            (Self::GitRollback, UiLanguage::ZhCn) => "回滚变更...",
            (Self::GitRollback, UiLanguage::EnUs) => "Rollback...",
            (Self::GitShowDiff, UiLanguage::ZhCn) => "当前文件与版本库对比",
            (Self::GitShowDiff, UiLanguage::EnUs) => "Compare with Same Repository Version",
            (Self::GitAnnotateBlame, UiLanguage::ZhCn) => "逐行追溯 (Git Blame)",
            (Self::GitAnnotateBlame, UiLanguage::EnUs) => "Annotate with Git Blame",
            (Self::GitFileHistory, UiLanguage::ZhCn) => "查看当前文件历史",
            (Self::GitFileHistory, UiLanguage::EnUs) => "Show History for File",
            (Self::GitRollbackCurrentFile, UiLanguage::ZhCn) => "回滚当前文件",
            (Self::GitRollbackCurrentFile, UiLanguage::EnUs) => "Rollback Current File",
            (Self::GitShowLog, UiLanguage::ZhCn) => "显示 Git 日志",
            (Self::GitShowLog, UiLanguage::EnUs) => "Show Git Log",
            (Self::GitManageRemotes, UiLanguage::ZhCn) => "管理远程仓库...",
            (Self::GitManageRemotes, UiLanguage::EnUs) => "Manage Remotes...",
            (Self::ToggleProjectWindow, UiLanguage::ZhCn) => "项目",
            (Self::ToggleProjectWindow, UiLanguage::EnUs) => "Project",
            (Self::ToggleCommitWindow, UiLanguage::ZhCn) => "提交",
            (Self::ToggleCommitWindow, UiLanguage::EnUs) => "Commit",
            (Self::ToggleStructureWindow, UiLanguage::ZhCn) => "结构",
            (Self::ToggleStructureWindow, UiLanguage::EnUs) => "Structure",
            (Self::ToggleAgentHubWindow, UiLanguage::ZhCn) => "AI 助手",
            (Self::ToggleAgentHubWindow, UiLanguage::EnUs) => "AI Assistant",
            (Self::ToggleTerminalWindow, UiLanguage::ZhCn) => "终端",
            (Self::ToggleTerminalWindow, UiLanguage::EnUs) => "Terminal",
            (Self::OpenSettingsModal, UiLanguage::ZhCn) => "设置...",
            (Self::OpenSettingsModal, UiLanguage::EnUs) => "Settings...",
            (Self::SwitchTheme, UiLanguage::ZhCn) => "快速切换主题...",
            (Self::SwitchTheme, UiLanguage::EnUs) => "Quick Switch Theme...",
            (Self::SwitchUiLanguage, UiLanguage::ZhCn) => "界面语言: 中文 / English",
            (Self::SwitchUiLanguage, UiLanguage::EnUs) => "UI Language: English / 中文",
            (Self::ZoomInUi, UiLanguage::ZhCn) => "放大界面与字体 (+10%)",
            (Self::ZoomInUi, UiLanguage::EnUs) => "Zoom In UI & Fonts (+10%)",
            (Self::ZoomOutUi, UiLanguage::ZhCn) => "缩小界面与字体 (-10%)",
            (Self::ZoomOutUi, UiLanguage::EnUs) => "Zoom Out UI & Fonts (-10%)",
            (Self::ResetUiZoom, UiLanguage::ZhCn) => "重置界面缩放 (100%)",
            (Self::ResetUiZoom, UiLanguage::EnUs) => "Reset UI Zoom (100%)",
            (Self::WidenLeftPanel, UiLanguage::ZhCn) => "右拉加宽左侧面板",
            (Self::WidenLeftPanel, UiLanguage::EnUs) => "Stretch Left Panel Right",
            (Self::NarrowLeftPanel, UiLanguage::ZhCn) => "左拉缩窄左侧面板",
            (Self::NarrowLeftPanel, UiLanguage::EnUs) => "Stretch Left Panel Left",
            (Self::WidenRightPanel, UiLanguage::ZhCn) => "左拉加宽右侧面板",
            (Self::WidenRightPanel, UiLanguage::EnUs) => "Stretch Right Panel Left",
            (Self::RaiseBottomPanel, UiLanguage::ZhCn) => "上拉加高底部面板",
            (Self::RaiseBottomPanel, UiLanguage::EnUs) => "Stretch Bottom Panel Up",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_and_theme_switching() {
        assert_eq!(LanguageId::from_path(Path::new("src/main.rs")), LanguageId::Rust);
        assert_eq!(LanguageId::from_path(Path::new("web/app.tsx")), LanguageId::TypeScript);
        assert_eq!(LanguageId::from_path(Path::new("agent/run.py")), LanguageId::Python);
        let java = LanguageId::from_path(Path::new("src/main/java/App.java"));
        assert_eq!(java, LanguageId::Java);
        assert_eq!(java.display_name(), "Java");
        assert_eq!(java.lsp_language_id(), "java");
        assert_eq!(java.default_lsp_binary(), Some("jdtls"));
        assert_eq!(java.default_dap_adapter(), Some("java-debug"));

        let vue = LanguageId::from_path(Path::new("src/App.vue"));
        assert_eq!(vue, LanguageId::Vue);
        assert_eq!(vue.display_name(), "Vue");
        assert_eq!(vue.lsp_language_id(), "vue");
        assert_eq!(vue.default_lsp_binary(), Some("vue-language-server"));
        assert_eq!(vue.default_dap_adapter(), Some("js-debug"));
        assert_eq!(LanguageId::from_path(Path::new("src/App.svelte")), LanguageId::Svelte);
        assert_eq!(LanguageId::from_path(Path::new("src/pages/index.astro")), LanguageId::Astro);
        assert_eq!(LanguageId::from_fence_tag("vue"), LanguageId::Vue);

        assert_eq!(KeymapScheme::default(), KeymapScheme::IntelliJIdea);
        assert_eq!(KeymapScheme::IntelliJIdea.next(), KeymapScheme::Eclipse);
        assert_eq!(IdeAction::DeleteLine.shortcut_label_for(KeymapScheme::Eclipse), "Ctrl+D");
        assert_eq!(IdeAction::DuplicateLine.shortcut_label_for(KeymapScheme::Eclipse), "Ctrl+Alt+Down");
        assert_eq!(IdeAction::ReformatCode.shortcut_label_for(KeymapScheme::Eclipse), "Ctrl+Shift+F");
        assert_eq!(IdeAction::GoToDeclaration.shortcut_label_for(KeymapScheme::Eclipse), "F3");
        assert_eq!(IdeAction::RenameSymbol.shortcut_label_for(KeymapScheme::Eclipse), "Alt+Shift+R");
        assert_eq!(IdeAction::FindInFiles.shortcut_label_for(KeymapScheme::Eclipse), "Ctrl+H");

        let t = ThemeKind::IslandsDark.next();
        assert_eq!(t, ThemeKind::DarculaClassic);
    }

    #[test]
    fn test_menu_and_action_i18n_coverage() {
        assert_eq!(UiLanguage::default(), UiLanguage::ZhCn);
        assert_eq!(UiLanguage::ZhCn.toggle(), UiLanguage::EnUs);
        assert_eq!(UiLanguage::EnUs.toggle(), UiLanguage::ZhCn);

        for &cat in MainMenuCategory::all() {
            let zh = cat.label_i18n(UiLanguage::ZhCn);
            let en = cat.label_i18n(UiLanguage::EnUs);
            assert!(!zh.is_empty());
            assert!(!en.is_empty());
            if cat != MainMenuCategory::Git {
                assert_ne!(zh, en, "category {cat:?} should differ between ZH and EN");
            }
        }

        assert!(MainMenuCategory::File.items().contains(&IdeAction::OpenFolder));
        assert!(MainMenuCategory::View.items().contains(&IdeAction::SwitchUiLanguage));

        for &act in IdeAction::all() {
            assert!(!act.menu_title_i18n(UiLanguage::ZhCn).is_empty());
            assert!(!act.menu_title_i18n(UiLanguage::EnUs).is_empty());
            assert!(!act.title_i18n(UiLanguage::ZhCn).is_empty());
            assert!(!act.title_i18n(UiLanguage::EnUs).is_empty());
        }
    }
}
