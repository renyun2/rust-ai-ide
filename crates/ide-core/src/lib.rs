use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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

/// 首发重点支持的编程语言与配置文件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LanguageId {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Json,
    Toml,
    Markdown,
    PlainText,
}

impl LanguageId {
    pub fn from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "rs" => Self::Rust,
            "ts" | "tsx" => Self::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "py" | "pyi" => Self::Python,
            "json" | "jsonc" => Self::Json,
            "toml" => Self::Toml,
            "md" | "markdown" => Self::Markdown,
            _ => Self::PlainText,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::TypeScript => "TypeScript",
            Self::JavaScript => "JavaScript",
            Self::Python => "Python",
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
            _ => None,
        }
    }

    pub fn default_dap_adapter(&self) -> Option<&'static str> {
        match self {
            Self::Rust => Some("codelldb"),
            Self::TypeScript | Self::JavaScript => Some("js-debug"),
            Self::Python => Some("debugpy"),
            _ => None,
        }
    }
}

/// IDEA 顶部工具栏 Run/Debug Configuration 定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunConfiguration {
    pub name: String,
    pub language: LanguageId,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

impl RunConfiguration {
    pub fn detect_for_workspace(workspace_root: &Path) -> Vec<Self> {
        let mut configs = Vec::new();

        if workspace_root.join("Cargo.toml").exists() {
            configs.push(Self {
                name: "Cargo Check (Workspace)".to_string(),
                language: LanguageId::Rust,
                command: "cargo".to_string(),
                args: vec![
                    "check".to_string(),
                    "--workspace".to_string(),
                    "--offline".to_string(),
                ],
                cwd: workspace_root.to_path_buf(),
            });
            configs.push(Self {
                name: "Cargo Test (All Crates)".to_string(),
                language: LanguageId::Rust,
                command: "cargo".to_string(),
                args: vec![
                    "test".to_string(),
                    "--workspace".to_string(),
                    "--offline".to_string(),
                ],
                cwd: workspace_root.to_path_buf(),
            });
            configs.push(Self {
                name: "Cargo Run (ide-app)".to_string(),
                language: LanguageId::Rust,
                command: "cargo".to_string(),
                args: vec![
                    "run".to_string(),
                    "-p".to_string(),
                    "ide-app".to_string(),
                    "--offline".to_string(),
                    "--".to_string(),
                    "--self-check".to_string(),
                ],
                cwd: workspace_root.to_path_buf(),
            });
        }

        if workspace_root.join("package.json").exists() {
            configs.push(Self {
                name: "npm run dev (TS/JS)".to_string(),
                language: LanguageId::TypeScript,
                command: "npm".to_string(),
                args: vec!["run".to_string(), "dev".to_string()],
                cwd: workspace_root.to_path_buf(),
            });
            configs.push(Self {
                name: "npm test (TS/JS)".to_string(),
                language: LanguageId::TypeScript,
                command: "npm".to_string(),
                args: vec!["test".to_string()],
                cwd: workspace_root.to_path_buf(),
            });
        }

        if workspace_root.join("pyproject.toml").exists()
            || workspace_root.join("requirements.txt").exists()
            || workspace_root.join("main.py").exists()
        {
            configs.push(Self {
                name: "Python: main.py".to_string(),
                language: LanguageId::Python,
                command: "python".to_string(),
                args: vec!["main.py".to_string()],
                cwd: workspace_root.to_path_buf(),
            });
        }

        if configs.is_empty() {
            configs.push(Self {
                name: "Default Shell Task".to_string(),
                language: LanguageId::PlainText,
                command: "cargo".to_string(),
                args: vec!["--version".to_string()],
                cwd: workspace_root.to_path_buf(),
            });
        }

        configs
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

/// 顶部主菜单分类（对齐 IntelliJ IDEA Main Menu）
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
        match self {
            Self::File => "File",
            Self::Edit => "Edit",
            Self::View => "View",
            Self::Navigate => "Navigate",
            Self::Code => "Code",
            Self::Refactor => "Refactor",
            Self::Run => "Run",
            Self::Git => "Git",
            Self::Tools => "Tools",
            Self::Window => "Window",
            Self::Help => "Help",
        }
    }

    pub fn items(&self) -> Vec<IdeAction> {
        match self {
            Self::File => vec![
                IdeAction::NewFileInProject,
                IdeAction::NewFolderInProject,
                IdeAction::SaveActiveFile,
                IdeAction::OpenSettingsModal,
            ],
            Self::Edit => vec![
                IdeAction::UndoEdit,
                IdeAction::RedoEdit,
                IdeAction::DuplicateLine,
                IdeAction::DeleteLine,
                IdeAction::FindInFiles,
            ],
            Self::View => vec![
                IdeAction::ToggleProjectWindow,
                IdeAction::ToggleCommitWindow,
                IdeAction::ToggleStructureWindow,
                IdeAction::ToggleAgentHubWindow,
                IdeAction::ToggleTerminalWindow,
                IdeAction::SwitchTheme,
            ],
            Self::Navigate => vec![
                IdeAction::SearchEverywhere,
                IdeAction::GoToFile,
                IdeAction::FindAction,
                IdeAction::GoToDeclaration,
            ],
            Self::Code => vec![
                IdeAction::ToggleLineComment,
                IdeAction::ReformatCode,
                IdeAction::InlineAiEdit,
                IdeAction::ShowIntentionActions,
            ],
            Self::Refactor => vec![IdeAction::RenameSymbol, IdeAction::InlineAiEdit],
            Self::Run => vec![
                IdeAction::RunActiveConfig,
                IdeAction::DebugActiveConfig,
                IdeAction::ToggleBreakpoint,
            ],
            Self::Git => vec![
                IdeAction::GitCommitPopup,
                IdeAction::GitPull,
                IdeAction::GitPush,
                IdeAction::GitNewBranch,
                IdeAction::RevertLastCheckpoint,
            ],
            Self::Tools => vec![
                IdeAction::SendSelectionToAgent,
                IdeAction::SwitchToAntigravity,
                IdeAction::SwitchToClaudeCode,
                IdeAction::SwitchToCodexCli,
            ],
            Self::Window => vec![
                IdeAction::WidenLeftPanel,
                IdeAction::NarrowLeftPanel,
                IdeAction::WidenRightPanel,
                IdeAction::RaiseBottomPanel,
            ],
            Self::Help => vec![IdeAction::FindAction, IdeAction::OpenSettingsModal],
        }
    }
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
    // 文件与项目树操作
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
    DebugActiveConfig,       // Shift+F9
    ToggleBreakpoint,        // Ctrl+F8
    // Git 操作 (VCS)
    GitCommitPopup,          // Ctrl+M / Alt+0
    GitPull,                 // Ctrl+T
    GitPush,                 // Ctrl+Shift+K
    GitNewBranch,
    // 工具窗口与设置
    ToggleProjectWindow,     // Alt+1
    ToggleCommitWindow,      // Alt+0
    ToggleStructureWindow,   // Alt+7
    ToggleAgentHubWindow,    // Alt+8
    ToggleTerminalWindow,    // Alt+F12
    OpenSettingsModal,       // Ctrl+Alt+S
    SwitchTheme,             // Ctrl+`
    WidenLeftPanel,
    NarrowLeftPanel,
    WidenRightPanel,
    RaiseBottomPanel,
}

impl IdeAction {
    pub fn all() -> &'static [Self] {
        &[
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
            Self::DebugActiveConfig,
            Self::ToggleBreakpoint,
            Self::GitCommitPopup,
            Self::GitPull,
            Self::GitPush,
            Self::GitNewBranch,
            Self::ToggleProjectWindow,
            Self::ToggleCommitWindow,
            Self::ToggleStructureWindow,
            Self::ToggleAgentHubWindow,
            Self::ToggleTerminalWindow,
            Self::OpenSettingsModal,
            Self::SwitchTheme,
        ]
    }

    pub fn shortcut_label(&self) -> &'static str {
        match self {
            Self::SearchEverywhere => "Double Shift",
            Self::FindAction => "Ctrl+Shift+A",
            Self::GoToFile => "Ctrl+Shift+N",
            Self::FindInFiles => "Ctrl+Shift+F",
            Self::GoToDeclaration => "Ctrl+B",
            Self::NewFileInProject => "Alt+Insert",
            Self::NewFolderInProject => "Ctrl+Alt+N",
            Self::DeleteSelectedPath => "Delete",
            Self::SaveActiveFile => "Ctrl+S",
            Self::CloseActiveTab => "Ctrl+F4",
            Self::UndoEdit => "Ctrl+Z",
            Self::RedoEdit => "Ctrl+Shift+Z",
            Self::DuplicateLine => "Ctrl+D",
            Self::DeleteLine => "Ctrl+Back",
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
            Self::DebugActiveConfig => "Shift+F9",
            Self::ToggleBreakpoint => "Ctrl+F8",
            Self::GitCommitPopup => "Alt+0",
            Self::GitPull => "Ctrl+T",
            Self::GitPush => "Ctrl+Shift+K",
            Self::GitNewBranch => "Ctrl+Alt+B",
            Self::ToggleProjectWindow => "Alt+1",
            Self::ToggleCommitWindow => "Alt+0",
            Self::ToggleStructureWindow => "Alt+7",
            Self::ToggleAgentHubWindow => "Alt+8",
            Self::ToggleTerminalWindow => "Alt+F12",
            Self::OpenSettingsModal => "Ctrl+Alt+S",
            Self::SwitchTheme => "Ctrl+`",
            Self::WidenLeftPanel => "Ctrl+Shift+Right",
            Self::NarrowLeftPanel => "Ctrl+Shift+Left",
            Self::WidenRightPanel => "Ctrl+Alt+Left",
            Self::RaiseBottomPanel => "Ctrl+Shift+Up",
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            Self::SearchEverywhere => "Search Everywhere (全局搜索文件/符号/命令)",
            Self::FindAction => "Find Action... (查找并执行 IDE 动作)",
            Self::GoToFile => "Go to File... (按文件名快速跳转)",
            Self::FindInFiles => "Find in Files... (Ripgrep 全局文本搜索)",
            Self::GoToDeclaration => "Go to Declaration or Usages (跳转定义)",
            Self::NewFileInProject => "New File... (在当前目录下新建源码文件)",
            Self::NewFolderInProject => "New Directory... (在当前目录下新建文件夹)",
            Self::DeleteSelectedPath => "Delete File/Directory (删除选中项)",
            Self::SaveActiveFile => "Save All Documents (保存当前及所有修改)",
            Self::CloseActiveTab => "Close Active Editor Tab (关闭当前标签页)",
            Self::UndoEdit => "Undo (撤销上一步编辑)",
            Self::RedoEdit => "Redo (重做编辑)",
            Self::DuplicateLine => "Duplicate Line or Selection (IDEA 复制当前行)",
            Self::DeleteLine => "Delete Line at Caret (IDEA 删除当前行)",
            Self::ToggleLineComment => "Comment with Line Comment (切换行注释)",
            Self::ReformatCode => "Reformat Code (IDEA 代码格式化)",
            Self::RenameSymbol => "Rename Symbol... (安全重命名符号)",
            Self::ShowIntentionActions => "Show Context Actions & AI Fix (意图修复)",
            Self::InlineAiEdit => "Cursor Ctrl+K Inline AI Edit (行内 AI 重构 Diff)",
            Self::SendSelectionToAgent => "Send Selection to Active Agent (发送选区至智能体)",
            Self::AcceptInlineDiff => "Accept AI Proposed Hunk Diff (接受 AI 修改)",
            Self::RejectInlineDiff => "Reject AI Proposed Hunk Diff (拒绝 AI 修改)",
            Self::RevertLastCheckpoint => "Revert to Last Checkpoint (回滚至快照)",
            Self::SwitchToAntigravity => "Switch Agent: Google Antigravity (ACP)",
            Self::SwitchToClaudeCode => "Switch Agent: Claude Code CLI",
            Self::SwitchToCodexCli => "Switch Agent: OpenAI Codex CLI",
            Self::RunActiveConfig => "Run Active Configuration (运行当前配置)",
            Self::DebugActiveConfig => "Debug Active Configuration (启动 DAP 调试)",
            Self::ToggleBreakpoint => "Toggle Line Breakpoint (切换行断点)",
            Self::GitCommitPopup => "Commit... (打开 Git 提交与暂存面板)",
            Self::GitPull => "Git Pull (从远程拉取更新)",
            Self::GitPush => "Git Push... (推送提交至远程仓库)",
            Self::GitNewBranch => "Git New Branch... (基于当前 HEAD 创建新分支)",
            Self::ToggleProjectWindow => "Project Tool Window (切换项目树面板)",
            Self::ToggleCommitWindow => "Commit Tool Window (切换 Git 提交面板)",
            Self::ToggleStructureWindow => "Structure Tool Window (切换代码结构大纲)",
            Self::ToggleAgentHubWindow => "Multi-Agent Hub Window (切换右侧 AI 面板)",
            Self::ToggleTerminalWindow => "Terminal Tool Window (切换底部终端)",
            Self::OpenSettingsModal => "Settings... (打开 IDE 设置与快捷键映射)",
            Self::SwitchTheme => "Switch Theme (切换 Islands Dark / Darcula / Light)",
            Self::WidenLeftPanel => "Stretch Left Tool Window Right (加宽左侧栏)",
            Self::NarrowLeftPanel => "Stretch Left Tool Window Left (缩窄左侧栏)",
            Self::WidenRightPanel => "Stretch Right Tool Window Left (加宽右侧 AI 栏)",
            Self::RaiseBottomPanel => "Stretch Bottom Tool Window Up (加高底部面板)",
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
        let t = ThemeKind::IslandsDark.next();
        assert_eq!(t, ThemeKind::DarculaClassic);
    }
}
