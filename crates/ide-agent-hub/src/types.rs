use ide_core::LanguageId;
use ide_git::{compute_line_diff, DiffLine};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 思考深度（Reasoning Effort），兼容 Cursor CLI (`effort=...` / `reasoning_effort=...`) 与 Antigravity CLI (`--effort`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    ExtraHigh,
    Max,
}

impl ReasoningEffort {
    pub fn as_cli_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::ExtraHigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Low => "思考:低",
            Self::Medium => "思考:中",
            Self::High => "思考:高",
            Self::ExtraHigh => "思考:超高",
            Self::Max => "思考:Max",
        }
    }

    pub fn label_en(&self) -> &'static str {
        match self {
            Self::Low => "Think:Low",
            Self::Medium => "Think:Med",
            Self::High => "Think:High",
            Self::ExtraHigh => "Think:XHigh",
            Self::Max => "Think:Max",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Low => Self::Medium,
            Self::Medium => Self::High,
            Self::High => Self::ExtraHigh,
            Self::ExtraHigh => Self::Max,
            Self::Max => Self::Low,
        }
    }

    pub fn from_str_lossy(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "low" => Self::Low,
            "high" => Self::High,
            "xhigh" | "extra_high" | "extra-high" => Self::ExtraHigh,
            "max" => Self::Max,
            _ => Self::Medium,
        }
    }
}

/// 支持的本地 AI Coding Agent CLI 后端
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentProviderKind {
    CursorCli,
    GoogleAntigravityAcp,
    ClaudeCodeCli,
    OpenAiCodexCli,
    GeminiCli,
    CustomLlmApi,
    CursorInlineEngine,
}

impl AgentProviderKind {
    pub fn all() -> &'static [Self] {
        &[
            Self::CursorCli,
            Self::GoogleAntigravityAcp,
            Self::ClaudeCodeCli,
            Self::OpenAiCodexCli,
            Self::GeminiCli,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::CursorCli | Self::CursorInlineEngine => "Cursor Agent (CLI)",
            Self::GoogleAntigravityAcp => "Antigravity (agy CLI)",
            Self::ClaudeCodeCli => "Claude Code (CLI)",
            Self::OpenAiCodexCli => "OpenAI Codex (CLI)",
            Self::GeminiCli => "Gemini (CLI)",
            Self::CustomLlmApi => "本地 CLI 网关",
        }
    }

    pub fn short_name(&self) -> &'static str {
        match self {
            Self::CursorCli | Self::CursorInlineEngine => "Cursor CLI",
            Self::GoogleAntigravityAcp => "Antigravity CLI",
            Self::ClaudeCodeCli => "Claude CLI",
            Self::OpenAiCodexCli => "Codex CLI",
            Self::GeminiCli => "Gemini CLI",
            Self::CustomLlmApi => "Local CLI",
        }
    }

    pub fn compact_name(&self) -> &'static str {
        match self {
            Self::CursorCli | Self::CursorInlineEngine => "Cursor",
            Self::GoogleAntigravityAcp => "Antigravity",
            Self::ClaudeCodeCli => "Claude",
            Self::OpenAiCodexCli => "Codex",
            Self::GeminiCli => "Gemini",
            Self::CustomLlmApi => "Local",
        }
    }

    pub fn protocol_badge(&self) -> &'static str {
        match self {
            Self::CursorCli | Self::CursorInlineEngine => "cursor-agent -p --model",
            Self::GoogleAntigravityAcp => "agy -p --model --effort",
            Self::ClaudeCodeCli => "claude -p --model",
            Self::OpenAiCodexCli => "codex exec -m",
            Self::GeminiCli => "gemini --skip-trust -m",
            Self::CustomLlmApi => "Local CLI Auto",
        }
    }

    pub fn supports_fast_mode(&self) -> bool {
        matches!(self, Self::CursorCli | Self::CursorInlineEngine)
    }

    pub fn supports_reasoning_effort(&self) -> bool {
        matches!(
            self,
            Self::CursorCli
                | Self::CursorInlineEngine
                | Self::GoogleAntigravityAcp
                | Self::ClaudeCodeCli
                | Self::OpenAiCodexCli
        )
    }

    /// 不同本地 CLI 真实支持的工作模式列表（按常用优先级排序）
    pub fn supported_modes(&self) -> &'static [AgentWorkbenchMode] {
        match self {
            // Cursor CLI 仅提供全能 Agent (`-f`) 与只读问答 (`--mode ask`)，无独立 Edit-only 模式
            Self::CursorCli | Self::CursorInlineEngine => {
                &[AgentWorkbenchMode::Agent, AgentWorkbenchMode::Ask]
            }
            // Codex CLI exec 仅区分读写沙箱 (`-s workspace-write`) 与只读 (`-s read-only`)
            Self::OpenAiCodexCli => &[AgentWorkbenchMode::Agent, AgentWorkbenchMode::Ask],
            // Antigravity (`auto` / `accept-edits` / `plan`)、Claude (`bypassPermissions` / `acceptEdits` / `plan`)、
            // Gemini (`yolo` / `auto_edit` / `plan`) 与内置网关均完整支持三种模式
            Self::GoogleAntigravityAcp
            | Self::ClaudeCodeCli
            | Self::GeminiCli
            | Self::CustomLlmApi => &[
                AgentWorkbenchMode::Agent,
                AgentWorkbenchMode::Edit,
                AgentWorkbenchMode::Ask,
            ],
        }
    }

    pub fn supports_mode(&self, mode: AgentWorkbenchMode) -> bool {
        self.supported_modes().contains(&mode)
    }

    pub fn normalize_mode(&self, mode: AgentWorkbenchMode) -> AgentWorkbenchMode {
        if self.supports_mode(mode) {
            mode
        } else {
            self.supported_modes()
                .first()
                .copied()
                .unwrap_or(AgentWorkbenchMode::Agent)
        }
    }

    pub fn next_supported_mode(&self, current: AgentWorkbenchMode) -> AgentWorkbenchMode {
        let modes = self.supported_modes();
        if let Some(pos) = modes.iter().position(|&m| m == current) {
            modes[(pos + 1) % modes.len()]
        } else {
            modes.first().copied().unwrap_or(AgentWorkbenchMode::Agent)
        }
    }

    /// 返回当前 CLI 对该模式实际透传的底层命令行参数标签
    pub fn mode_cli_flag(&self, mode: AgentWorkbenchMode) -> &'static str {
        match self {
            Self::CursorCli | Self::CursorInlineEngine => match mode {
                AgentWorkbenchMode::Ask => "--mode ask",
                AgentWorkbenchMode::Edit | AgentWorkbenchMode::Agent => "-f",
            },
            Self::GoogleAntigravityAcp => match mode {
                AgentWorkbenchMode::Ask => "--mode plan",
                AgentWorkbenchMode::Edit => "--mode accept-edits",
                AgentWorkbenchMode::Agent => "--mode auto",
            },
            Self::ClaudeCodeCli => match mode {
                AgentWorkbenchMode::Ask => "--permission-mode plan",
                AgentWorkbenchMode::Edit => "--permission-mode acceptEdits",
                AgentWorkbenchMode::Agent => "bypassPermissions",
            },
            Self::OpenAiCodexCli => match mode {
                AgentWorkbenchMode::Ask => "-s read-only",
                AgentWorkbenchMode::Edit | AgentWorkbenchMode::Agent => "-s workspace-write",
            },
            Self::GeminiCli => match mode {
                AgentWorkbenchMode::Ask => "--approval-mode plan",
                AgentWorkbenchMode::Edit => "--approval-mode auto_edit",
                AgentWorkbenchMode::Agent => "--approval-mode yolo",
            },
            Self::CustomLlmApi => match mode {
                AgentWorkbenchMode::Ask => "chat-qa",
                AgentWorkbenchMode::Edit => "patch-diff",
                AgentWorkbenchMode::Agent => "tool-loop",
            },
        }
    }

    pub fn mode_description(&self, mode: AgentWorkbenchMode, zh: bool) -> &'static str {
        match (self, mode, zh) {
            (_, AgentWorkbenchMode::Agent, true) => "自主读写工作区文件、执行命令与工具",
            (_, AgentWorkbenchMode::Agent, false) => "Autonomous workspace edits & tools",
            (Self::GoogleAntigravityAcp | Self::ClaudeCodeCli | Self::GeminiCli, AgentWorkbenchMode::Edit, true) => {
                "自动应用文件编辑，高风险操作受限"
            }
            (Self::GoogleAntigravityAcp | Self::ClaudeCodeCli | Self::GeminiCli, AgentWorkbenchMode::Edit, false) => {
                "Auto-accept file edits with guarded tools"
            }
            (_, AgentWorkbenchMode::Edit, true) => "生成可一键应用的代码补丁卡片",
            (_, AgentWorkbenchMode::Edit, false) => "Propose one-click code diff patches",
            (Self::GoogleAntigravityAcp | Self::ClaudeCodeCli | Self::GeminiCli, AgentWorkbenchMode::Ask, true) => {
                "只读规划与代码问答，不修改工作区文件"
            }
            (Self::GoogleAntigravityAcp | Self::ClaudeCodeCli | Self::GeminiCli, AgentWorkbenchMode::Ask, false) => {
                "Read-only planning & Q&A without edits"
            }
            (_, AgentWorkbenchMode::Ask, true) => "纯只读问答分析，禁止写入文件",
            (_, AgentWorkbenchMode::Ask, false) => "Read-only Q&A without modifying files",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiApiFormat {
    OpenAiCompatible,
    Anthropic,
    Gemini,
}

impl AiApiFormat {
    pub fn label(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "OpenAI 兼容 (/chat/completions)",
            Self::Anthropic => "Anthropic (/v1/messages)",
            Self::Gemini => "Google Gemini REST",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::OpenAiCompatible => Self::Anthropic,
            Self::Anthropic => Self::Gemini,
            Self::Gemini => Self::OpenAiCompatible,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiApiConfig {
    pub format: AiApiFormat,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl Default for AiApiConfig {
    fn default() -> Self {
        Self {
            format: AiApiFormat::OpenAiCompatible,
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
            model: "gpt-4o-mini".to_string(),
        }
    }
}

impl AiApiConfig {
    pub fn config_file_path(workspace_root: &Path) -> PathBuf {
        workspace_root.join(".openidea").join("ai_config.json")
    }

    pub fn load_for_workspace(workspace_root: &Path) -> Self {
        let mut cfg = Self::default();
        let path = Self::config_file_path(workspace_root);
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(parsed) = serde_json::from_str::<Self>(&raw) {
                cfg = parsed;
            }
        }
        cfg
    }

    pub fn save_for_workspace(&self, workspace_root: &Path) -> anyhow::Result<()> {
        let dir = workspace_root.join(".openidea");
        if !dir.exists() {
            fs::create_dir_all(&dir)?;
        }
        let path = Self::config_file_path(workspace_root);
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    pub fn is_configured(&self) -> bool {
        let url_low = self.base_url.to_ascii_lowercase();
        let is_local_endpoint = url_low.contains("localhost") || url_low.contains("127.0.0.1");
        (!self.api_key.trim().is_empty() || is_local_endpoint) && !self.base_url.trim().is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentWorkbenchMode {
    Ask,
    Edit,
    Agent,
}

impl AgentWorkbenchMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ask => "Ask",
            Self::Edit => "Edit",
            Self::Agent => "Agent",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Ask => Self::Edit,
            Self::Edit => Self::Agent,
            Self::Agent => Self::Ask,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRegistration {
    pub kind: AgentProviderKind,
    pub binary_path: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub available: bool,
    pub active_model: String,
    pub discovered_models: Vec<String>,
    pub exact_model_catalog: Vec<String>,
    pub fast_mode: bool,
    pub reasoning_effort: ReasoningEffort,
    pub status_text: String,
}

/// AI 编辑台在聊天流中直接产出的可执行代码补丁卡片
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedFilePatch {
    pub id: String,
    pub file_path: PathBuf,
    pub language: LanguageId,
    pub start_line: usize,
    pub end_line: usize,
    pub summary: String,
    pub original_code: String,
    pub proposed_code: String,
    pub diff_lines: Vec<DiffLine>,
    pub applied: bool,
    pub dismissed: bool,
}

/// 附加到对话或随消息发送的文件/图片项（支持拖拽文件、拖拽图片直接预览）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAttachedItem {
    pub path: PathBuf,
    pub display_name: String,
    pub is_image: bool,
    pub is_dir: bool,
    pub width: u32,
    pub height: u32,
}

/// 粘贴进 Composer 的多行代码片段（支持折叠芯片与发送后可折叠代码块展示）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PastedCodeSnippet {
    pub id: String,
    pub title: String,
    pub language: String,
    pub content: String,
    pub line_count: usize,
}

/// 智能体对话消息（支持携带改动文件列表、附件/图片预览、代码补丁卡片与可点击文件链接）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentChatMessage {
    pub role: String,
    pub agent: AgentProviderKind,
    pub content: String,
    pub thought: Option<String>,
    pub checkpoint_id: Option<String>,
    pub patch: Option<ProposedFilePatch>,
    #[serde(default)]
    pub modified_files: Vec<PathBuf>,
    #[serde(default)]
    pub attachments: Vec<AgentAttachedItem>,
}

/// 独立的 AI 多会话状态容器
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: String,
    pub title: String,
    pub custom_title: bool,
    pub provider: AgentProviderKind,
    pub active_model: String,
    pub fast_mode: bool,
    pub reasoning_effort: ReasoningEffort,
    pub messages: Vec<AgentChatMessage>,
    pub checkpoints: Vec<WorkspaceCheckpoint>,
    pub attached_items: Vec<AgentAttachedItem>,
    pub pasted_snippets: Vec<PastedCodeSnippet>,
    pub draft_prompt: String,
    pub is_busy: bool,
    pub created_order: usize,
}

/// 传入 AI 编辑台的真实 IDE 编辑器与工程上下文
#[derive(Debug, Clone)]
pub struct IdeCodeContext {
    pub file_path: PathBuf,
    pub language: LanguageId,
    pub file_text: String,
    pub cursor_line: usize,
    pub diagnostics_summary: String,
    pub git_changed_count: usize,
}

/// Cursor 风格 `Ctrl+K` 行内编辑与 Diff Apply 会话状态机
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineEditSession {
    pub file_path: PathBuf,
    pub language: LanguageId,
    pub start_line: usize,
    pub end_line: usize,
    pub prompt: String,
    pub original_code: String,
    pub proposed_code: String,
    pub diff_lines: Vec<DiffLine>,
    pub active: bool,
    pub loading: bool,
    pub error: Option<String>,
}

impl InlineEditSession {
    pub fn create(
        file_path: PathBuf,
        language: LanguageId,
        start_line: usize,
        end_line: usize,
        prompt: String,
        original_code: String,
        proposed_code: String,
    ) -> Self {
        let diff_lines = compute_line_diff(&original_code, &proposed_code);
        Self {
            file_path,
            language,
            start_line,
            end_line,
            prompt,
            original_code,
            proposed_code,
            diff_lines,
            active: true,
            loading: false,
            error: None,
        }
    }

    pub fn pending(
        file_path: PathBuf,
        language: LanguageId,
        start_line: usize,
        end_line: usize,
        prompt: String,
        original_code: String,
    ) -> Self {
        Self {
            file_path,
            language,
            start_line,
            end_line,
            prompt,
            original_code,
            proposed_code: String::new(),
            diff_lines: Vec::new(),
            active: true,
            loading: true,
            error: None,
        }
    }
}

/// 工作区原子快照（用于在 Agent 修改文件前自动保存状态，支持一键 Revert to Checkpoint）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceCheckpoint {
    pub id: String,
    pub label: String,
    pub file_backups: HashMap<PathBuf, Option<String>>,
}

pub type AgentWakeupCallback = Arc<dyn Fn() + Send + Sync>;
