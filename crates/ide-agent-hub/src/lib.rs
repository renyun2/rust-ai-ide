use ide_core::LanguageId;
use ide_git::{compute_line_diff, DiffLine};
use ide_lsp::find_executable_in_path;
use ide_search::find_in_files;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use uuid::Uuid;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 支持的 AI Coding Agent 后端
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentProviderKind {
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
            Self::GoogleAntigravityAcp,
            Self::ClaudeCodeCli,
            Self::OpenAiCodexCli,
            Self::GeminiCli,
            Self::CustomLlmApi,
            Self::CursorInlineEngine,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::GoogleAntigravityAcp => "Antigravity (ACP / 智能路由)",
            Self::ClaudeCodeCli => "Claude Code (CLI)",
            Self::OpenAiCodexCli => "OpenAI Codex (CLI)",
            Self::GeminiCli => "Gemini (CLI)",
            Self::CustomLlmApi => "自定义 HTTP API",
            Self::CursorInlineEngine => "Cursor 行内引擎",
        }
    }

    pub fn short_name(&self) -> &'static str {
        match self {
            Self::GoogleAntigravityAcp => "Antigravity",
            Self::ClaudeCodeCli => "Claude Code",
            Self::OpenAiCodexCli => "OpenAI Codex",
            Self::GeminiCli => "Gemini CLI",
            Self::CustomLlmApi => "HTTP API",
            Self::CursorInlineEngine => "Cursor",
        }
    }

    pub fn protocol_badge(&self) -> &'static str {
        match self {
            Self::GoogleAntigravityAcp => "ACP JSON-RPC / Auto",
            Self::ClaudeCodeCli => "claude -p (Headless)",
            Self::OpenAiCodexCli => "codex exec -",
            Self::GeminiCli => "gemini --skip-trust -p",
            Self::CustomLlmApi => "OpenAI / Anthropic / Gemini SSE",
            Self::CursorInlineEngine => "Async Inline Diff (Ctrl+K)",
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

        // 若未在工作区文件中配置 Key，自动读取常见环境变量
        if cfg.api_key.trim().is_empty() {
            if let Ok(k) = std::env::var("OPENAI_API_KEY") {
                if !k.trim().is_empty() {
                    cfg.api_key = k;
                    cfg.format = AiApiFormat::OpenAiCompatible;
                    if let Ok(u) = std::env::var("OPENAI_BASE_URL") {
                        if !u.trim().is_empty() {
                            cfg.base_url = u;
                        }
                    }
                    if let Ok(m) = std::env::var("OPENAI_MODEL") {
                        if !m.trim().is_empty() {
                            cfg.model = m;
                        }
                    }
                }
            } else if let Ok(k) = std::env::var("DEEPSEEK_API_KEY") {
                if !k.trim().is_empty() {
                    cfg.api_key = k;
                    cfg.format = AiApiFormat::OpenAiCompatible;
                    cfg.base_url = "https://api.deepseek.com/v1".to_string();
                    cfg.model = "deepseek-chat".to_string();
                }
            } else if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
                if !k.trim().is_empty() {
                    cfg.api_key = k;
                    cfg.format = AiApiFormat::Anthropic;
                    cfg.base_url = "https://api.anthropic.com/v1".to_string();
                    cfg.model = "claude-sonnet-4-20250514".to_string();
                }
            } else if let Ok(k) = std::env::var("GEMINI_API_KEY") {
                if !k.trim().is_empty() {
                    cfg.api_key = k;
                    cfg.format = AiApiFormat::Gemini;
                    cfg.base_url = "https://generativelanguage.googleapis.com/v1beta".to_string();
                    cfg.model = "gemini-2.5-flash".to_string();
                }
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
    pub available: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentChatMessage {
    pub role: String,
    pub agent: AgentProviderKind,
    pub content: String,
    pub thought: Option<String>,
    pub checkpoint_id: Option<String>,
    pub patch: Option<ProposedFilePatch>,
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

struct PendingInlineMeta {
    file_path: PathBuf,
    language: LanguageId,
    start_line: usize,
    end_line: usize,
    prompt: String,
    original_code: String,
}

pub struct AgentHub {
    pub workspace_root: PathBuf,
    pub active_provider: AgentProviderKind,
    pub providers: Vec<AgentRegistration>,
    pub api_config: AiApiConfig,
    pub messages: Vec<AgentChatMessage>,
    pub inline_edit: Option<InlineEditSession>,
    pub checkpoints: Vec<WorkspaceCheckpoint>,
    pub loaded_rules_summary: String,
    pub is_busy: Arc<AtomicBool>,
    cancel_requested: Arc<AtomicBool>,
    active_child_pid: Arc<Mutex<Option<u32>>>,
    streaming_text: Arc<Mutex<Option<String>>>,
    async_inbox: Arc<Mutex<Vec<AgentChatMessage>>>,
    async_checkpoints_inbox: Arc<Mutex<Vec<WorkspaceCheckpoint>>>,
    modified_files_inbox: Arc<Mutex<Vec<PathBuf>>>,
    async_inline_result: Arc<Mutex<Option<(PendingInlineMeta, Result<String, String>)>>>,
    async_commit_msg: Arc<Mutex<Option<Result<String, String>>>>,
    wakeup: Option<AgentWakeupCallback>,
}

impl AgentHub {
    pub fn new(workspace_root: &Path) -> Self {
        Self::new_with_wakeup(workspace_root, None)
    }

    pub fn new_with_wakeup(workspace_root: &Path, wakeup: Option<AgentWakeupCallback>) -> Self {
        let claude_path =
            find_executable_in_path("claude.cmd").or_else(|| find_executable_in_path("claude"));
        let codex_path =
            find_executable_in_path("codex.cmd").or_else(|| find_executable_in_path("codex"));
        let gemini_path =
            find_executable_in_path("gemini.cmd").or_else(|| find_executable_in_path("gemini"));
        let acp_env = std::env::var("ACP_AGENT_CMD")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let api_config = AiApiConfig::load_for_workspace(workspace_root);

        let providers = vec![
            AgentRegistration {
                kind: AgentProviderKind::GoogleAntigravityAcp,
                binary_path: acp_env.as_ref().map(PathBuf::from).or_else(|| gemini_path.clone()),
                available: api_config.is_configured()
                    || acp_env.is_some()
                    || gemini_path.is_some()
                    || claude_path.is_some()
                    || codex_path.is_some(),
                status_text: if let Some(cmd) = &acp_env {
                    format!("ACP Agent -> {cmd}")
                } else if api_config.is_configured() {
                    format!("Auto-route -> HTTP API ({})", api_config.model)
                } else if gemini_path.is_some() {
                    "Auto-route -> Gemini CLI (--skip-trust)".to_string()
                } else if claude_path.is_some() {
                    "Auto-route -> Claude Code CLI".to_string()
                } else if codex_path.is_some() {
                    "Auto-route -> OpenAI Codex CLI".to_string()
                } else {
                    "未配置 AI Key/CLI：请点击 ⚙ 配置 API Key 或本地 Ollama".to_string()
                },
            },
            AgentRegistration {
                kind: AgentProviderKind::ClaudeCodeCli,
                status_text: match &claude_path {
                    Some(p) => format!("Detected: {}", p.display()),
                    None => "Not found in PATH (npm i -g @anthropic-ai/claude-code)".to_string(),
                },
                available: claude_path.is_some(),
                binary_path: claude_path,
            },
            AgentRegistration {
                kind: AgentProviderKind::OpenAiCodexCli,
                status_text: match &codex_path {
                    Some(p) => format!("Detected: {}", p.display()),
                    None => "Not found in PATH (npm i -g @openai/codex)".to_string(),
                },
                available: codex_path.is_some(),
                binary_path: codex_path,
            },
            AgentRegistration {
                kind: AgentProviderKind::GeminiCli,
                status_text: match &gemini_path {
                    Some(p) => format!("Detected: {}", p.display()),
                    None => "Not found in PATH (npm i -g @google/gemini-cli)".to_string(),
                },
                available: gemini_path.is_some(),
                binary_path: gemini_path,
            },
            AgentRegistration {
                kind: AgentProviderKind::CustomLlmApi,
                binary_path: None,
                available: api_config.is_configured(),
                status_text: if api_config.is_configured() {
                    format!("{} ({})", api_config.model, api_config.base_url)
                } else {
                    "点击右上角 ⚙ 配置 OpenAI / DeepSeek / Claude / Gemini / Ollama API".to_string()
                },
            },
            AgentRegistration {
                kind: AgentProviderKind::CursorInlineEngine,
                binary_path: None,
                available: api_config.is_configured(),
                status_text: if api_config.is_configured() {
                    format!("Ctrl+K -> {} ({})", api_config.model, api_config.base_url)
                } else {
                    "需配置 HTTP API 或 CLI 后执行真实 Ctrl+K 修改".to_string()
                },
            },
        ];

        let rules_summary = load_workspace_agent_rules(workspace_root);
        let route_summary = providers[0].status_text.clone();

        let messages = vec![AgentChatMessage {
            role: "system".to_string(),
            agent: AgentProviderKind::GoogleAntigravityAcp,
            content: format!(
                "AI 智能体工作台已就绪 · {}\n• Ask：实时流式问答与源码分析 | Edit：自动抽取代码块生成 Diff 补丁卡片 | Agent：真实调用读文件/搜索/改写文件/执行终端命令工具链\n• 快捷命令：/check (cargo check)、/search <关键词> | 点击右上角 ⚙ 配置 OpenAI / DeepSeek / Claude / Gemini / Ollama API\n{}",
                route_summary, rules_summary
            ),
            thought: Some("Real AI Agent hub initialized (HTTP SSE + Tool Calling + ACP + CLI).".to_string()),
            checkpoint_id: None,
            patch: None,
        }];

        Self {
            workspace_root: workspace_root.to_path_buf(),
            active_provider: AgentProviderKind::GoogleAntigravityAcp,
            providers,
            api_config,
            messages,
            inline_edit: None,
            checkpoints: Vec::new(),
            loaded_rules_summary: rules_summary,
            is_busy: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            active_child_pid: Arc::new(Mutex::new(None)),
            streaming_text: Arc::new(Mutex::new(None)),
            async_inbox: Arc::new(Mutex::new(Vec::new())),
            async_checkpoints_inbox: Arc::new(Mutex::new(Vec::new())),
            modified_files_inbox: Arc::new(Mutex::new(Vec::new())),
            async_inline_result: Arc::new(Mutex::new(None)),
            async_commit_msg: Arc::new(Mutex::new(None)),
            wakeup,
        }
    }

    pub fn set_wakeup(&mut self, wakeup: Option<AgentWakeupCallback>) {
        self.wakeup = wakeup;
    }

    pub fn update_api_config(&mut self, new_cfg: AiApiConfig) -> anyhow::Result<()> {
        self.api_config = new_cfg;
        let configured = self.api_config.is_configured();
        if let Some(reg) = self
            .providers
            .iter_mut()
            .find(|p| p.kind == AgentProviderKind::CustomLlmApi)
        {
            reg.available = configured;
            reg.status_text = if configured {
                format!("{} ({})", self.api_config.model, self.api_config.base_url)
            } else {
                "点击右上角 ⚙ 配置 API Key / Base URL".to_string()
            };
        }
        if let Some(reg) = self
            .providers
            .iter_mut()
            .find(|p| p.kind == AgentProviderKind::CursorInlineEngine)
        {
            reg.available = configured;
            reg.status_text = if configured {
                format!("Ctrl+K -> {} ({})", self.api_config.model, self.api_config.base_url)
            } else {
                "需配置 HTTP API 或 CLI 后执行真实 Ctrl+K 修改".to_string()
            };
        }
        if configured {
            if let Some(reg) = self
                .providers
                .iter_mut()
                .find(|p| p.kind == AgentProviderKind::GoogleAntigravityAcp)
            {
                reg.available = true;
                reg.status_text = format!("Auto-route -> HTTP API ({})", self.api_config.model);
            }
        }
        self.api_config.save_for_workspace(&self.workspace_root)
    }

    pub fn streaming_preview(&self) -> Option<String> {
        self.streaming_text.lock().clone()
    }

    pub fn cancel_active_request(&mut self) {
        if !self.is_busy.load(Ordering::Relaxed) {
            return;
        }
        self.cancel_requested.store(true, Ordering::SeqCst);
        if let Some(pid) = self.active_child_pid.lock().take() {
            #[cfg(target_os = "windows")]
            {
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .output();
            }
        }
        self.is_busy.store(false, Ordering::SeqCst);
        let partial = self.streaming_text.lock().take().unwrap_or_default();
        let content = if partial.trim().is_empty() {
            "⏹ 已取消当前 AI 请求。".to_string()
        } else {
            format!("{}\n\n[⏹ 已手动停止生成]", partial.trim_end())
        };
        self.messages.push(AgentChatMessage {
            role: "assistant".to_string(),
            agent: self.active_provider,
            content,
            thought: Some("Request cancelled by user.".to_string()),
            checkpoint_id: None,
            patch: None,
        });
        if let Some(cb) = &self.wakeup {
            cb();
        }
    }

    /// 轮询后台异步 Agent / Cargo Check 任务返回的消息；若有新消息返回 true
    pub fn poll_async_messages(&mut self) -> bool {
        let mut changed = false;
        {
            let mut ck_inbox = self.async_checkpoints_inbox.lock();
            if !ck_inbox.is_empty() {
                self.checkpoints.extend(ck_inbox.drain(..));
                changed = true;
            }
        }
        {
            let mut inbox = self.async_inbox.lock();
            if !inbox.is_empty() {
                self.messages.extend(inbox.drain(..));
                changed = true;
            }
        }
        changed
    }

    /// 轮询 Agent 工具在磁盘上修改过的文件路径（供编辑器热刷新缓冲区与 Git 状态）
    pub fn poll_modified_files(&self) -> Vec<PathBuf> {
        let mut guard = self.modified_files_inbox.lock();
        if guard.is_empty() {
            Vec::new()
        } else {
            guard.drain(..).collect()
        }
    }

    /// 轮询后台异步 `Ctrl+K` 行内编辑结果
    pub fn poll_inline_edit_result(&mut self) -> bool {
        let Some((meta, result)) = self.async_inline_result.lock().take() else {
            return false;
        };
        match result {
            Ok(code) => {
                let mut session = InlineEditSession::create(
                    meta.file_path,
                    meta.language,
                    meta.start_line,
                    meta.end_line,
                    meta.prompt,
                    meta.original_code,
                    code,
                );
                session.loading = false;
                session.error = None;
                self.inline_edit = Some(session);
            }
            Err(err_msg) => {
                let mut session = InlineEditSession::pending(
                    meta.file_path,
                    meta.language,
                    meta.start_line,
                    meta.end_line,
                    meta.prompt,
                    meta.original_code,
                );
                session.loading = false;
                session.error = Some(err_msg);
                self.inline_edit = Some(session);
            }
        }
        true
    }

    pub fn poll_commit_message(&self) -> Option<Result<String, String>> {
        self.async_commit_msg.lock().take()
    }

    pub fn clear_messages(&mut self) {
        self.messages.truncate(1);
        *self.streaming_text.lock() = None;
    }

    /// 创建文件修改前的原子 Checkpoint 快照
    pub fn create_checkpoint(&mut self, label: impl Into<String>, files: &[(&Path, &str)]) -> String {
        let id = format!("ckpt-{}", &Uuid::new_v4().to_string()[..8]);
        let mut backups = HashMap::new();
        for (p, content) in files {
            backups.insert(p.to_path_buf(), Some((*content).to_string()));
        }
        self.checkpoints.push(WorkspaceCheckpoint {
            id: id.clone(),
            label: label.into(),
            file_backups: backups,
        });
        id
    }

    /// 异步触发 Cursor 风格 `Ctrl+K` 行内编辑（真实调用 LLM API 或 CLI，无假补丁）
    pub fn propose_ctrl_k_inline_edit(
        &mut self,
        file_path: PathBuf,
        language: LanguageId,
        start_line: usize,
        end_line: usize,
        original_slice: &str,
        user_prompt: &str,
    ) -> &InlineEditSession {
        let prompt_trimmed = user_prompt.trim();
        let backend = self.resolve_execution_backend();
        if prompt_trimmed.is_empty() {
            let mut s = InlineEditSession::pending(
                file_path,
                language,
                start_line,
                end_line,
                String::new(),
                original_slice.to_string(),
            );
            s.loading = false;
            s.error = Some("请输入具体修改指令后按 Enter 提交至 AI 模型生成 Diff".to_string());
            self.inline_edit = Some(s);
            return self.inline_edit.as_ref().unwrap();
        }

        if matches!(backend, ExecutionBackend::LocalOffline) {
            let mut s = InlineEditSession::pending(
                file_path,
                language,
                start_line,
                end_line,
                prompt_trimmed.to_string(),
                original_slice.to_string(),
            );
            s.loading = false;
            s.error = Some("未配置 AI 模型：请点击右上角 ⚙ 配置 API Key / Ollama，或登录 Claude/Gemini/Codex CLI。".to_string());
            self.inline_edit = Some(s);
            return self.inline_edit.as_ref().unwrap();
        }

        self.inline_edit = Some(InlineEditSession::pending(
            file_path.clone(),
            language,
            start_line,
            end_line,
            prompt_trimmed.to_string(),
            original_slice.to_string(),
        ));

        let meta = PendingInlineMeta {
            file_path: file_path.clone(),
            language,
            start_line,
            end_line,
            prompt: prompt_trimmed.to_string(),
            original_code: original_slice.to_string(),
        };

        let sys_prompt = "You are an inline code editor engine. Output ONLY the modified replacement code for the given snippet inside a single fenced code block (no extra prose outside the code block). Preserve indentation and language semantics.";
        let user_msg = format!(
            "File: {}\nLanguage: {}\nLines: {}-{}\nInstruction: {}\n\nOriginal code:\n```{}\n{}\n```",
            file_path.display(),
            language.display_name(),
            start_line + 1,
            end_line + 1,
            prompt_trimmed,
            language.lsp_language_id(),
            original_slice
        );

        let result_slot = self.async_inline_result.clone();
        let wakeup = self.wakeup.clone();
        let root = self.workspace_root.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let dummy_stream = Arc::new(Mutex::new(None));
        let dummy_pid = Arc::new(Mutex::new(None));

        thread::spawn(move || {
            let raw_res = match backend {
                ExecutionBackend::HttpApi(cfg) => execute_curl_llm_streaming(
                    &cfg,
                    sys_prompt,
                    &user_msg,
                    &dummy_stream,
                    &cancel,
                    &dummy_pid,
                    None,
                ),
                ExecutionBackend::Cli(kind, bin) => {
                    execute_cli_once(kind, &bin, &root, &format!("{sys_prompt}\n\n{user_msg}"), &dummy_stream, &dummy_pid, None)
                }
                ExecutionBackend::Acp(acp_cmd) => {
                    execute_acp_stdio_session(&acp_cmd, &root, &format!("{sys_prompt}\n\n{user_msg}"), Some(&meta.file_path), &dummy_stream, &dummy_pid, None, None)
                }
                ExecutionBackend::LocalOffline => Err("No AI backend configured".to_string()),
            };

            let final_res = raw_res.and_then(|reply| {
                let extracted = extract_first_fenced_code_block(&reply).unwrap_or(reply);
                let trimmed = extracted.trim_end().to_string();
                if trimmed.is_empty() {
                    Err("AI 返回了空代码块".to_string())
                } else {
                    Ok(format!("{trimmed}\n"))
                }
            });

            *result_slot.lock() = Some((meta, final_res));
            if let Some(cb) = wakeup {
                cb();
            }
        });

        self.inline_edit.as_ref().unwrap()
    }

    /// 请求 AI 依据当前 Git Diff 异步生成 Commit Message
    pub fn request_ai_commit_message(&mut self, diff_text: String, changed_files: Vec<String>) {
        let backend = self.resolve_execution_backend();
        let cfg = match backend {
            ExecutionBackend::HttpApi(c) => c,
            _ => {
                *self.async_commit_msg.lock() = Some(Err(
                    "请先在 AI 面板右上角 ⚙ 配置 HTTP API Key / 本地 Ollama 后再生成 Commit 信息"
                        .to_string(),
                ));
                if let Some(cb) = &self.wakeup {
                    cb();
                }
                return;
            }
        };

        let slot = self.async_commit_msg.clone();
        let wakeup = self.wakeup.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let dummy_stream = Arc::new(Mutex::new(None));
        let dummy_pid = Arc::new(Mutex::new(None));

        thread::spawn(move || {
            let sys = "Generate a concise Conventional Commits message (1 title line <= 72 chars, optional short bullet list). Output ONLY the commit message text without markdown fences.";
            let diff_clipped: String = diff_text.chars().take(4500).collect();
            let user = format!(
                "Changed files:\n{}\n\nDiff:\n{}",
                changed_files.join("\n"),
                diff_clipped
            );
            let res = execute_curl_llm_streaming(
                &cfg,
                sys,
                &user,
                &dummy_stream,
                &cancel,
                &dummy_pid,
                None,
            )
            .map(|s| s.trim().trim_matches('`').to_string());
            *slot.lock() = Some(res);
            if let Some(cb) = wakeup {
                cb();
            }
        });
    }

    /// 确定当前 Provider 实际应该走哪个后端（HTTP API / ACP / CLI / 未配置）
    fn resolve_execution_backend(&self) -> ExecutionBackend {
        let find_cli = |kind: AgentProviderKind| -> Option<PathBuf> {
            self.providers
                .iter()
                .find(|p| p.kind == kind)
                .and_then(|p| p.binary_path.clone())
        };

        if let Ok(acp_cmd) = std::env::var("ACP_AGENT_CMD") {
            if !acp_cmd.trim().is_empty()
                && self.active_provider == AgentProviderKind::GoogleAntigravityAcp
            {
                return ExecutionBackend::Acp(acp_cmd);
            }
        }

        match self.active_provider {
            AgentProviderKind::ClaudeCodeCli => {
                if let Some(bin) = find_cli(AgentProviderKind::ClaudeCodeCli) {
                    return ExecutionBackend::Cli(AgentProviderKind::ClaudeCodeCli, bin);
                }
            }
            AgentProviderKind::OpenAiCodexCli => {
                if let Some(bin) = find_cli(AgentProviderKind::OpenAiCodexCli) {
                    return ExecutionBackend::Cli(AgentProviderKind::OpenAiCodexCli, bin);
                }
            }
            AgentProviderKind::GeminiCli => {
                if let Some(bin) = find_cli(AgentProviderKind::GeminiCli) {
                    return ExecutionBackend::Cli(AgentProviderKind::GeminiCli, bin);
                }
            }
            AgentProviderKind::CustomLlmApi | AgentProviderKind::CursorInlineEngine => {
                if self.api_config.is_configured() {
                    return ExecutionBackend::HttpApi(self.api_config.clone());
                }
            }
            AgentProviderKind::GoogleAntigravityAcp => {
                if self.api_config.is_configured() {
                    return ExecutionBackend::HttpApi(self.api_config.clone());
                }
                if let Some(bin) = find_cli(AgentProviderKind::GeminiCli) {
                    return ExecutionBackend::Cli(AgentProviderKind::GeminiCli, bin);
                }
                if let Some(bin) = find_cli(AgentProviderKind::ClaudeCodeCli) {
                    return ExecutionBackend::Cli(AgentProviderKind::ClaudeCodeCli, bin);
                }
                if let Some(bin) = find_cli(AgentProviderKind::OpenAiCodexCli) {
                    return ExecutionBackend::Cli(AgentProviderKind::OpenAiCodexCli, bin);
                }
            }
        }
        ExecutionBackend::LocalOffline
    }

    /// AI 智能体工作台核心入口：真实调用 HTTP API (含 Agent 多步工具循环) / ACP / CLI
    pub fn submit_workbench_prompt(
        &mut self,
        user_prompt: &str,
        mode: AgentWorkbenchMode,
        ctx: Option<&IdeCodeContext>,
    ) {
        let trimmed = user_prompt.trim();
        if trimmed.is_empty() {
            return;
        }

        let file_label = ctx
            .map(|c| {
                c.file_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string()
            })
            .unwrap_or_else(|| "workspace".to_string());

        let user_display = if let Some(c) = ctx {
            format!("{}  [@{}:L{}]", trimmed, file_label, c.cursor_line + 1)
        } else {
            trimmed.to_string()
        };

        self.messages.push(AgentChatMessage {
            role: "user".to_string(),
            agent: self.active_provider,
            content: user_display,
            thought: None,
            checkpoint_id: None,
            patch: None,
        });

        // 1. 内置快捷斜杠命令（/check, /search）
        if trimmed.starts_with("/check")
            || trimmed.eq_ignore_ascii_case("cargo check")
            || trimmed == "编译检查"
        {
            self.dispatch_cargo_check_async();
            return;
        }

        if let Some(query) = trimmed.strip_prefix("/search ") {
            let hits = find_in_files(&self.workspace_root, query.trim(), 12).unwrap_or_default();
            let summary = if hits.is_empty() {
                format!("未在工作区检索到 `{}` 的匹配项。", query.trim())
            } else {
                let mut lines = vec![format!("工作区检索 `{}` 命中 {} 处：", query.trim(), hits.len())];
                for h in hits.iter().take(8) {
                    let rel = h
                        .path
                        .strip_prefix(&self.workspace_root)
                        .unwrap_or(&h.path)
                        .to_string_lossy();
                    lines.push(format!("• {}:{}  {}", rel, h.line_number, h.line_text.trim()));
                }
                lines.join("\n")
            };
            self.messages.push(AgentChatMessage {
                role: "assistant".to_string(),
                agent: self.active_provider,
                content: summary,
                thought: Some("Executed workspace search.".to_string()),
                checkpoint_id: None,
                patch: None,
            });
            return;
        }

        // 2. 路由到真实 AI 后端（HTTP LLM API / ACP / CLI）
        match self.resolve_execution_backend() {
            ExecutionBackend::HttpApi(cfg) => {
                if mode == AgentWorkbenchMode::Agent {
                    self.dispatch_http_agent_loop_async(cfg, trimmed.to_string(), ctx.cloned());
                } else {
                    self.dispatch_http_api_async(cfg, trimmed.to_string(), mode, ctx.cloned());
                }
            }
            ExecutionBackend::Acp(acp_cmd) => {
                self.dispatch_acp_async(acp_cmd, trimmed.to_string(), mode, ctx.cloned());
            }
            ExecutionBackend::Cli(cli_kind, bin) => {
                self.dispatch_cli_async(cli_kind, bin, trimmed.to_string(), mode, ctx.cloned());
            }
            ExecutionBackend::LocalOffline => {
                let detected_clis: Vec<&str> = self
                    .providers
                    .iter()
                    .filter(|p| {
                        p.available
                            && matches!(
                                p.kind,
                                AgentProviderKind::ClaudeCodeCli
                                    | AgentProviderKind::OpenAiCodexCli
                                    | AgentProviderKind::GeminiCli
                            )
                    })
                    .map(|p| p.kind.short_name())
                    .collect();
                let cli_hint = if detected_clis.is_empty() {
                    "当前环境中也未检测到已登录的 CLI。".to_string()
                } else {
                    format!(
                        "本机已检测到 CLI（{}），可点击上方供应商标签切换，或先在终端完成对应 CLI 登录。",
                        detected_clis.join(" / ")
                    )
                };
                self.messages.push(AgentChatMessage {
                    role: "assistant".to_string(),
                    agent: self.active_provider,
                    content: format!(
                        "⚠ 未配置可用的 AI 模型后端。\n请点击 AI 面板右上角 `⚙` 填写 API Key / Base URL（支持 OpenAI、DeepSeek、Claude、Gemini 及本地 Ollama `http://127.0.0.1:11434/v1`），然后即可真实对话与调用工作区 Agent 工具。\n{}",
                        cli_hint
                    ),
                    thought: Some("No AI backend configured.".to_string()),
                    checkpoint_id: None,
                    patch: None,
                });
            }
        }
    }

    fn build_system_and_user_prompt(
        &self,
        prompt: &str,
        mode: AgentWorkbenchMode,
        ctx: Option<&IdeCodeContext>,
    ) -> (String, String) {
        let mut system = String::from(
            "You are an expert software engineer embedded inside Rust AI IDE (IntelliJ IDEA Islands UI). Reply clearly and concisely in the user's language.",
        );
        if mode == AgentWorkbenchMode::Edit {
            system.push_str(
                " When the user asks to modify, fix, refactor, document, or add tests to code, output a concise explanation AND include a single complete fenced code block (e.g. ```rust ... ```) for the replacement block so the IDE can apply it as a one-click patch.",
            );
        } else if mode == AgentWorkbenchMode::Agent {
            system.push_str(
                " You are operating in Autonomous Agent mode in workspace root. Use the provided tools (list_dir, read_file, search_code, replace_in_file, write_file, run_command) to inspect files, verify code, edit files directly when requested, and check builds before summarizing what you changed.",
            );
        }
        if !self.loaded_rules_summary.is_empty() {
            system.push_str("\nWorkspace rules detected: ");
            system.push_str(&self.loaded_rules_summary);
        }

        let mut user_msg = String::new();
        let recent: Vec<&AgentChatMessage> = self
            .messages
            .iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .rev()
            .skip(1)
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if !recent.is_empty() {
            user_msg.push_str("## Recent Conversation\n");
            for m in recent {
                let role_tag = if m.role == "user" { "User" } else { "Assistant" };
                let snippet: String = m.content.chars().take(600).collect();
                user_msg.push_str(&format!("{role_tag}: {snippet}\n"));
            }
            user_msg.push('\n');
        }

        if let Some(c) = ctx {
            let (start_line, end_line, block) =
                extract_enclosing_block(&c.file_text, c.cursor_line);
            user_msg.push_str(&format!(
                "## Active IDE Context\nWorkspace Root: {}\nFile: {} (Language: {}, Cursor Line: {}, Enclosing Block: L{}-L{})\n",
                self.workspace_root.display(),
                c.file_path.display(),
                c.language.display_name(),
                c.cursor_line + 1,
                start_line + 1,
                end_line + 1
            ));
            if !c.diagnostics_summary.trim().is_empty() {
                user_msg.push_str(&format!("Diagnostics: {}\n", c.diagnostics_summary));
            }
            user_msg.push_str(&format!(
                "```{}\n{}\n```\n\n",
                c.language.lsp_language_id(),
                block.trim_end()
            ));
        } else {
            user_msg.push_str(&format!(
                "## Workspace Root\n{}\n\n",
                self.workspace_root.display()
            ));
        }

        user_msg.push_str("## User Request\n");
        user_msg.push_str(prompt);
        (system, user_msg)
    }

    fn dispatch_http_api_async(
        &mut self,
        cfg: AiApiConfig,
        prompt: String,
        mode: AgentWorkbenchMode,
        ctx: Option<IdeCodeContext>,
    ) {
        if self.is_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancel_requested.store(false, Ordering::SeqCst);
        *self.streaming_text.lock() = Some(format!("正在连接 {} ({})...", cfg.model, cfg.base_url));

        let (sys_prompt, user_prompt) = self.build_system_and_user_prompt(&prompt, mode, ctx.as_ref());
        let inbox = self.async_inbox.clone();
        let busy = self.is_busy.clone();
        let cancel = self.cancel_requested.clone();
        let stream_buf = self.streaming_text.clone();
        let wakeup = self.wakeup.clone();
        let provider = self.active_provider;
        let active_pid = self.active_child_pid.clone();

        if let Some(cb) = &wakeup {
            cb();
        }

        thread::spawn(move || {
            let reply_res = execute_curl_llm_streaming(
                &cfg,
                &sys_prompt,
                &user_prompt,
                &stream_buf,
                &cancel,
                &active_pid,
                wakeup.as_ref(),
            );

            *active_pid.lock() = None;
            *stream_buf.lock() = None;

            if !cancel.load(Ordering::SeqCst) {
                let (content, patch) = match reply_res {
                    Ok(text) => {
                        let p = maybe_extract_patch_from_reply(&text, mode, ctx.as_ref());
                        (text, p)
                    }
                    Err(err) => (format!("✕ HTTP API 请求失败：\n{err}"), None),
                };
                inbox.lock().push(AgentChatMessage {
                    role: "assistant".to_string(),
                    agent: provider,
                    content,
                    thought: Some(format!("Completed {} HTTP API call.", cfg.model)),
                    checkpoint_id: None,
                    patch,
                });
            }

            busy.store(false, Ordering::SeqCst);
            if let Some(cb) = wakeup {
                cb();
            }
        });
    }

    /// 真实 Agent 多步工具调用闭环（读文件/列目录/搜索/替换/写入/执行命令 + 自动 Checkpoint）
    fn dispatch_http_agent_loop_async(
        &mut self,
        cfg: AiApiConfig,
        prompt: String,
        ctx: Option<IdeCodeContext>,
    ) {
        if self.is_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancel_requested.store(false, Ordering::SeqCst);
        *self.streaming_text.lock() =
            Some(format!("🤖 Agent ({}) 正在规划工具调用...", cfg.model));

        let (sys_prompt, user_prompt) =
            self.build_system_and_user_prompt(&prompt, AgentWorkbenchMode::Agent, ctx.as_ref());
        let root = self.workspace_root.clone();
        let inbox = self.async_inbox.clone();
        let ck_inbox = self.async_checkpoints_inbox.clone();
        let mod_inbox = self.modified_files_inbox.clone();
        let busy = self.is_busy.clone();
        let cancel = self.cancel_requested.clone();
        let stream_buf = self.streaming_text.clone();
        let wakeup = self.wakeup.clone();
        let provider = self.active_provider;
        let active_pid = self.active_child_pid.clone();

        if let Some(cb) = &wakeup {
            cb();
        }

        thread::spawn(move || {
            let mut ckpt_backups: HashMap<PathBuf, Option<String>> = HashMap::new();
            let res = execute_agent_tool_loop(
                &cfg,
                &root,
                &sys_prompt,
                &user_prompt,
                &mut ckpt_backups,
                &mod_inbox,
                &stream_buf,
                &cancel,
                &active_pid,
                wakeup.as_ref(),
            );

            *active_pid.lock() = None;
            *stream_buf.lock() = None;

            if !cancel.load(Ordering::SeqCst) {
                let ckpt_id = if !ckpt_backups.is_empty() {
                    let id = format!("ckpt-{}", &Uuid::new_v4().to_string()[..8]);
                    ck_inbox.lock().push(WorkspaceCheckpoint {
                        id: id.clone(),
                        label: format!("Agent edits ({} files)", ckpt_backups.len()),
                        file_backups: ckpt_backups,
                    });
                    Some(id)
                } else {
                    None
                };

                let (content, patch) = match res {
                    Ok((text, steps)) => {
                        let summary = if steps.is_empty() {
                            text
                        } else {
                            format!("{}\n\n---\n{}", steps.join("\n"), text)
                        };
                        let p = maybe_extract_patch_from_reply(
                            &summary,
                            AgentWorkbenchMode::Agent,
                            ctx.as_ref(),
                        );
                        (summary, p)
                    }
                    Err(err) => (format!("✕ Agent 执行失败：\n{err}"), None),
                };
                inbox.lock().push(AgentChatMessage {
                    role: "assistant".to_string(),
                    agent: provider,
                    content,
                    thought: Some(format!("Completed Agent tool loop via {}.", cfg.model)),
                    checkpoint_id: ckpt_id,
                    patch,
                });
            }

            busy.store(false, Ordering::SeqCst);
            if let Some(cb) = wakeup {
                cb();
            }
        });
    }

    fn dispatch_acp_async(
        &mut self,
        acp_cmd: String,
        prompt: String,
        mode: AgentWorkbenchMode,
        ctx: Option<IdeCodeContext>,
    ) {
        if self.is_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancel_requested.store(false, Ordering::SeqCst);
        *self.streaming_text.lock() = Some(format!("正在通过 ACP ({acp_cmd}) 建立会话..."));

        let (sys_prompt, user_prompt) = self.build_system_and_user_prompt(&prompt, mode, ctx.as_ref());
        let combined = format!("{sys_prompt}\n\n{user_prompt}");
        let root = self.workspace_root.clone();
        let inbox = self.async_inbox.clone();
        let mod_inbox = self.modified_files_inbox.clone();
        let busy = self.is_busy.clone();
        let cancel = self.cancel_requested.clone();
        let stream_buf = self.streaming_text.clone();
        let wakeup = self.wakeup.clone();
        let active_pid = self.active_child_pid.clone();
        let provider = self.active_provider;

        if let Some(cb) = &wakeup {
            cb();
        }

        thread::spawn(move || {
            let active_file = ctx.as_ref().map(|c| c.file_path.as_path());
            let res = execute_acp_stdio_session(
                &acp_cmd,
                &root,
                &combined,
                active_file,
                &stream_buf,
                &active_pid,
                Some(&mod_inbox),
                wakeup.as_ref(),
            );
            *active_pid.lock() = None;
            *stream_buf.lock() = None;

            if !cancel.load(Ordering::SeqCst) {
                let (content, patch) = match res {
                    Ok(reply) => {
                        let p = maybe_extract_patch_from_reply(&reply, mode, ctx.as_ref());
                        (reply, p)
                    }
                    Err(e) => (format!("✕ ACP 会话失败：\n{e}"), None),
                };
                inbox.lock().push(AgentChatMessage {
                    role: "assistant".to_string(),
                    agent: provider,
                    content,
                    thought: Some(format!("Completed ACP session via {acp_cmd}.")),
                    checkpoint_id: None,
                    patch,
                });
            }
            busy.store(false, Ordering::SeqCst);
            if let Some(cb) = wakeup {
                cb();
            }
        });
    }

    fn dispatch_cli_async(
        &mut self,
        cli_kind: AgentProviderKind,
        bin: PathBuf,
        prompt: String,
        mode: AgentWorkbenchMode,
        ctx: Option<IdeCodeContext>,
    ) {
        if self.is_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancel_requested.store(false, Ordering::SeqCst);

        let cli_label = bin
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cli")
            .to_string();
        *self.streaming_text.lock() = Some(format!("正在通过 `{cli_label}` 思考中..."));

        let (sys_prompt, user_prompt) = self.build_system_and_user_prompt(&prompt, mode, ctx.as_ref());
        let combined_prompt = format!("{sys_prompt}\n\n{user_prompt}");

        let root = self.workspace_root.clone();
        let inbox = self.async_inbox.clone();
        let busy = self.is_busy.clone();
        let cancel = self.cancel_requested.clone();
        let stream_buf = self.streaming_text.clone();
        let wakeup = self.wakeup.clone();
        let active_pid = self.active_child_pid.clone();
        let display_provider = self.active_provider;

        if let Some(cb) = &wakeup {
            cb();
        }

        thread::spawn(move || {
            let final_text = execute_cli_once(
                cli_kind,
                &bin,
                &root,
                &combined_prompt,
                &stream_buf,
                &active_pid,
                wakeup.as_ref(),
            );

            *active_pid.lock() = None;
            *stream_buf.lock() = None;

            if !cancel.load(Ordering::SeqCst) {
                let (content, patch) = match final_text {
                    Ok(reply) => {
                        let p = maybe_extract_patch_from_reply(&reply, mode, ctx.as_ref());
                        (reply, p)
                    }
                    Err(err_msg) => (
                        format!(
                            "✕ `{cli_label}` 调用异常：\n{err_msg}\n\n提示：若该 CLI 尚未登录认证或缺少 API Key，可先在底部终端运行登录命令，或点击 AI 面板右上角 ⚙ 配置 OpenAI / DeepSeek / Claude / Gemini / Ollama API Key。"
                        ),
                        None,
                    ),
                };

                inbox.lock().push(AgentChatMessage {
                    role: "assistant".to_string(),
                    agent: display_provider,
                    content,
                    thought: Some(format!("Completed async CLI via {cli_label}.")),
                    checkpoint_id: None,
                    patch,
                });
            }

            busy.store(false, Ordering::SeqCst);
            if let Some(cb) = wakeup {
                cb();
            }
        });
    }

    fn dispatch_cargo_check_async(&mut self) {
        if self.is_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancel_requested.store(false, Ordering::SeqCst);
        let root = self.workspace_root.clone();
        let inbox = self.async_inbox.clone();
        let busy = self.is_busy.clone();
        let stream_buf = self.streaming_text.clone();
        let wakeup = self.wakeup.clone();
        let provider = self.active_provider;

        *stream_buf.lock() = Some("正在后台执行 `cargo check --workspace --offline`...".to_string());
        if let Some(cb) = &wakeup {
            cb();
        }

        thread::spawn(move || {
            let mut cmd = Command::new("cargo");
            cmd.args(["check", "--workspace", "--offline", "--message-format=short"])
                .current_dir(&root);
            #[cfg(target_os = "windows")]
            cmd.creation_flags(CREATE_NO_WINDOW);

            let output = cmd.output();
            let text = match output {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let combined: Vec<&str> = stderr
                        .lines()
                        .chain(stdout.lines())
                        .filter(|l| !l.trim().is_empty())
                        .collect();
                    let status = if out.status.success() {
                        "✓ Cargo Check 通过（0 错误）"
                    } else {
                        "✕ Cargo Check 发现编译问题"
                    };
                    let tail: Vec<&str> = combined
                        .into_iter()
                        .rev()
                        .take(15)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    format!("{}\n{}", status, tail.join("\n"))
                }
                Err(e) => format!("执行 cargo check 失败: {e}"),
            };
            *stream_buf.lock() = None;
            inbox.lock().push(AgentChatMessage {
                role: "assistant".to_string(),
                agent: provider,
                content: text,
                thought: Some("Completed background cargo check.".to_string()),
                checkpoint_id: None,
                patch: None,
            });
            busy.store(false, Ordering::SeqCst);
            if let Some(cb) = wakeup {
                cb();
            }
        });
    }

    /// 兼容调用签名
    pub fn send_prompt_with_ide_context(
        &mut self,
        user_prompt: &str,
        active_file: Option<&Path>,
        selected_code: Option<&str>,
        diagnostics_summary: Option<&str>,
    ) {
        let ctx = active_file.map(|p| {
            let file_text = selected_code
                .map(|s| s.to_string())
                .or_else(|| fs::read_to_string(p).ok())
                .unwrap_or_default();
            IdeCodeContext {
                file_path: p.to_path_buf(),
                language: LanguageId::from_path(p),
                file_text,
                cursor_line: 0,
                diagnostics_summary: diagnostics_summary.unwrap_or("").to_string(),
                git_changed_count: 0,
            }
        });
        self.submit_workbench_prompt(user_prompt, AgentWorkbenchMode::Ask, ctx.as_ref());
    }
}

enum ExecutionBackend {
    HttpApi(AiApiConfig),
    Acp(String),
    Cli(AgentProviderKind, PathBuf),
    LocalOffline,
}

fn execute_cli_once(
    cli_kind: AgentProviderKind,
    bin: &Path,
    root: &Path,
    combined_prompt: &str,
    stream_buf: &Arc<Mutex<Option<String>>>,
    active_pid: &Arc<Mutex<Option<u32>>>,
    wakeup: Option<&AgentWakeupCallback>,
) -> Result<String, String> {
    let mut child = spawn_cli_with_piped_stdin(cli_kind, bin, root)
        .map_err(|e| format!("启动 CLI 失败: {e}"))?;
    let pid = child.id();
    *active_pid.lock() = Some(pid);

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(combined_prompt.as_bytes());
        let _ = stdin.flush();
    }

    let stdout_acc = Arc::new(Mutex::new(String::new()));
    let stderr_acc = Arc::new(Mutex::new(String::new()));
    let auth_blocked = Arc::new(AtomicBool::new(false));

    let out_thread = child.stdout.take().map(|out| {
        let acc = stdout_acc.clone();
        let sbuf = stream_buf.clone();
        let wk = wakeup.cloned();
        let auth_flag = auth_blocked.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(out);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line) {
                if n == 0 {
                    break;
                }
                let clean = strip_ansi_basic(&line);
                if clean.contains("Opening authentication page")
                    || clean.contains("Do you want to continue? [Y/n]")
                {
                    auth_flag.store(true, Ordering::SeqCst);
                    break;
                }
                {
                    let mut g = acc.lock();
                    g.push_str(&clean);
                    *sbuf.lock() = Some(g.clone());
                }
                if let Some(cb) = &wk {
                    cb();
                }
                line.clear();
            }
        })
    });

    let err_thread = child.stderr.take().map(|err| {
        let acc = stderr_acc.clone();
        let auth_flag = auth_blocked.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(err);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line) {
                if n == 0 {
                    break;
                }
                let clean = strip_ansi_basic(&line);
                if clean.contains("Opening authentication page")
                    || clean.contains("Do you want to continue? [Y/n]")
                {
                    auth_flag.store(true, Ordering::SeqCst);
                    break;
                }
                acc.lock().push_str(&clean);
                line.clear();
            }
        })
    });

    // Wait while also monitoring auth prompt block
    let status = loop {
        if auth_blocked.load(Ordering::SeqCst) {
            let _ = child.kill();
            #[cfg(target_os = "windows")]
            {
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
            }
            break Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "CLI 需要交互式登录认证",
            ));
        }
        match child.try_wait() {
            Ok(Some(st)) => break Ok(st),
            Ok(None) => thread::sleep(std::time::Duration::from_millis(60)),
            Err(e) => break Err(e),
        }
    };

    if let Some(t) = out_thread {
        let _ = t.join();
    }
    if let Some(t) = err_thread {
        let _ = t.join();
    }

    if auth_blocked.load(Ordering::SeqCst) {
        return Err(
            "CLI 需要先完成交互式登录认证（请在底部终端手动运行该 CLI 完成一次登录后重试，或配置 HTTP API Key）。"
                .to_string(),
        );
    }

    let out_str = stdout_acc.lock().trim().to_string();
    let err_str = stderr_acc.lock().trim().to_string();

    if !out_str.is_empty() {
        Ok(out_str)
    } else if !err_str.is_empty() {
        Err(err_str)
    } else {
        match status {
            Ok(st) if st.success() => Ok("CLI 执行完成（无文本输出）。".to_string()),
            Ok(st) => Err(format!("CLI 退出状态: {st}")),
            Err(e) => Err(format!("等待 CLI 进程失败: {e}")),
        }
    }
}

fn spawn_cli_with_piped_stdin(
    cli_kind: AgentProviderKind,
    bin: &Path,
    cwd: &Path,
) -> std::io::Result<std::process::Child> {
    let args: Vec<&str> = match cli_kind {
        AgentProviderKind::ClaudeCodeCli => vec!["-p", "--output-format", "text"],
        AgentProviderKind::OpenAiCodexCli => vec!["exec", "-"],
        AgentProviderKind::GeminiCli => vec!["--skip-trust", "-o", "text", "-p", ""],
        _ => vec![],
    };

    #[cfg(target_os = "windows")]
    {
        let ext = bin
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "cmd" || ext == "bat" {
            let mut cmd = Command::new("cmd");
            cmd.arg("/C").arg(bin).args(&args);
            cmd.current_dir(cwd)
                .env("GEMINI_CLI_TRUST_WORKSPACE", "true")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(CREATE_NO_WINDOW);
            return cmd.spawn();
        }
    }

    let mut cmd = Command::new(bin);
    cmd.args(&args)
        .current_dir(cwd)
        .env("GEMINI_CLI_TRUST_WORKSPACE", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()
}

/// 真实 ACP (Agent Client Protocol) JSON-RPC 2.0 Stdio 客户端（支持流式更新 + 服务端读写文件工具回调）
fn execute_acp_stdio_session(
    acp_cmd_line: &str,
    root: &Path,
    prompt_text: &str,
    active_file: Option<&Path>,
    stream_buf: &Arc<Mutex<Option<String>>>,
    active_pid: &Arc<Mutex<Option<u32>>>,
    modified_inbox: Option<&Arc<Mutex<Vec<PathBuf>>>>,
    wakeup: Option<&AgentWakeupCallback>,
) -> Result<String, String> {
    let parts: Vec<&str> = acp_cmd_line.split_whitespace().collect();
    if parts.is_empty() {
        return Err("ACP 命令为空".to_string());
    }
    let mut cmd = Command::new(parts[0]);
    if parts.len() > 1 {
        cmd.args(&parts[1..]);
    }
    cmd.current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 ACP Agent `{acp_cmd_line}`: {e}"))?;
    *active_pid.lock() = Some(child.id());

    let mut stdin = child.stdin.take().ok_or("Missing ACP stdin")?;
    let stdout = child.stdout.take().ok_or("Missing ACP stdout")?;
    let mut reader = BufReader::new(stdout);

    let send_json = |stdin: &mut std::process::ChildStdin, val: &Value| -> Result<(), String> {
        let mut s = serde_json::to_string(val).map_err(|e| e.to_string())?;
        s.push('\n');
        stdin.write_all(s.as_bytes()).map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())
    };

    // 1. initialize
    let init_req = AcpProtocol::build_initialize_request(1);
    send_json(&mut stdin, &init_req)?;

    let mut prompt_sent = false;
    let mut assembled = String::new();
    let mut line = String::new();

    while let Ok(n) = reader.read_line(&mut line) {
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            if let Ok(msg) = serde_json::from_str::<Value>(trimmed) {
                let id = msg.get("id").and_then(|v| v.as_u64());
                let method = msg.get("method").and_then(|v| v.as_str());

                if id == Some(1) && method.is_none() {
                    if let Some(err) = msg.get("error") {
                        let _ = child.kill();
                        return Err(format!("ACP initialize error: {err}"));
                    }
                    let new_req = AcpProtocol::build_session_new_request(2, root);
                    send_json(&mut stdin, &new_req)?;
                } else if id == Some(2) && method.is_none() {
                    if let Some(err) = msg.get("error") {
                        let _ = child.kill();
                        return Err(format!("ACP session/new error: {err}"));
                    }
                    let session_id = msg["result"]["sessionId"]
                        .as_str()
                        .unwrap_or("default")
                        .to_string();
                    if !prompt_sent {
                        prompt_sent = true;
                        let prompt_req = AcpProtocol::build_session_prompt_request(
                            3,
                            &session_id,
                            prompt_text,
                            active_file,
                        );
                        send_json(&mut stdin, &prompt_req)?;
                    }
                } else if id == Some(3) && method.is_none() {
                    if let Some(err) = msg.get("error") {
                        let _ = child.kill();
                        return Err(format!("ACP session/prompt error: {err}"));
                    }
                    if assembled.is_empty() {
                        if let Some(text) = msg["result"]["output"].as_str() {
                            assembled.push_str(text);
                        }
                    }
                    break;
                } else if let Some(m) = method {
                    if m == "session/update" {
                        let update = &msg["params"]["update"];
                        if let Some(chunk) = update["content"]["text"].as_str() {
                            assembled.push_str(chunk);
                            *stream_buf.lock() = Some(assembled.clone());
                            if let Some(cb) = wakeup {
                                cb();
                            }
                        } else if let Some(text) = update["text"].as_str() {
                            assembled.push_str(text);
                            *stream_buf.lock() = Some(assembled.clone());
                            if let Some(cb) = wakeup {
                                cb();
                            }
                        }
                    } else if m == "fs/read_text_file" {
                        if let Some(req_id) = msg.get("id") {
                            let path_str = msg["params"]["path"].as_str().unwrap_or("");
                            let resp = match resolve_safe_workspace_path(root, path_str)
                                .and_then(|p| fs::read_to_string(&p).map_err(|e| e.to_string()))
                            {
                                Ok(content) => json!({"jsonrpc": "2.0", "id": req_id, "result": {"content": content}}),
                                Err(e) => json!({"jsonrpc": "2.0", "id": req_id, "error": {"code": -32000, "message": e}}),
                            };
                            let _ = send_json(&mut stdin, &resp);
                        }
                    } else if m == "fs/write_text_file" {
                        if let Some(req_id) = msg.get("id") {
                            let path_str = msg["params"]["path"].as_str().unwrap_or("");
                            let content = msg["params"]["content"].as_str().unwrap_or("");
                            let resp = match resolve_safe_workspace_path(root, path_str).and_then(|p| {
                                if let Some(parent) = p.parent() {
                                    let _ = fs::create_dir_all(parent);
                                }
                                fs::write(&p, content).map_err(|e| e.to_string())?;
                                if let Some(inbox) = modified_inbox {
                                    inbox.lock().push(p);
                                }
                                Ok(())
                            }) {
                                Ok(()) => json!({"jsonrpc": "2.0", "id": req_id, "result": Value::Null}),
                                Err(e) => json!({"jsonrpc": "2.0", "id": req_id, "error": {"code": -32000, "message": e}}),
                            };
                            let _ = send_json(&mut stdin, &resp);
                        }
                    }
                }
            }
        }
        line.clear();
    }

    let _ = child.kill();
    let _ = child.wait();
    if assembled.trim().is_empty() {
        Err("ACP 会话未返回有效文本内容".to_string())
    } else {
        Ok(assembled)
    }
}

fn execute_curl_llm_streaming(
    cfg: &AiApiConfig,
    system_prompt: &str,
    user_prompt: &str,
    stream_buf: &Arc<Mutex<Option<String>>>,
    cancel: &Arc<AtomicBool>,
    active_pid: &Arc<Mutex<Option<u32>>>,
    wakeup: Option<&AgentWakeupCallback>,
) -> Result<String, String> {
    let base = cfg.base_url.trim_end_matches('/');
    let (url, headers, body_val) = match cfg.format {
        AiApiFormat::OpenAiCompatible => {
            let endpoint = if base.ends_with("/chat/completions") {
                base.to_string()
            } else {
                format!("{base}/chat/completions")
            };
            let mut hdrs = vec![("Content-Type".to_string(), "application/json".to_string())];
            if !cfg.api_key.trim().is_empty() {
                hdrs.push((
                    "Authorization".to_string(),
                    format!("Bearer {}", cfg.api_key.trim()),
                ));
            }
            let body = json!({
                "model": cfg.model.trim(),
                "stream": true,
                "messages": [
                    { "role": "system", "content": system_prompt },
                    { "role": "user", "content": user_prompt }
                ]
            });
            (endpoint, hdrs, body)
        }
        AiApiFormat::Anthropic => {
            let endpoint = if base.ends_with("/messages") {
                base.to_string()
            } else {
                format!("{base}/messages")
            };
            let hdrs = vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("x-api-key".to_string(), cfg.api_key.trim().to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ];
            let body = json!({
                "model": cfg.model.trim(),
                "max_tokens": 4096,
                "stream": true,
                "system": system_prompt,
                "messages": [
                    { "role": "user", "content": user_prompt }
                ]
            });
            (endpoint, hdrs, body)
        }
        AiApiFormat::Gemini => {
            let endpoint = format!(
                "{}/models/{}:streamGenerateContent?alt=sse&key={}",
                base,
                cfg.model.trim(),
                cfg.api_key.trim()
            );
            let hdrs = vec![("Content-Type".to_string(), "application/json".to_string())];
            let body = json!({
                "systemInstruction": { "parts": [{ "text": system_prompt }] },
                "contents": [
                    { "role": "user", "parts": [{ "text": user_prompt }] }
                ]
            });
            (endpoint, hdrs, body)
        }
    };

    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "--no-buffer", "-X", "POST", &url]);
    for (k, v) in &headers {
        cmd.arg("-H").arg(format!("{k}: {v}"));
    }
    cmd.args(["--data-binary", "@-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 curl.exe: {e}"))?;
    *active_pid.lock() = Some(child.id());

    if let Some(mut stdin) = child.stdin.take() {
        let payload = serde_json::to_vec(&body_val).map_err(|e| e.to_string())?;
        let _ = stdin.write_all(&payload);
        let _ = stdin.flush();
    }

    let stdout = child.stdout.take().ok_or("Missing curl stdout")?;
    let stderr_handle = child.stderr.take().map(|err| {
        thread::spawn(move || {
            let mut s = String::new();
            let _ = BufReader::new(err).read_to_string(&mut s);
            s
        })
    });

    let mut assembled = String::new();
    let mut raw_fallback = String::new();
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();

    while let Ok(n) = reader.read_line(&mut line) {
        if n == 0 || cancel.load(Ordering::SeqCst) {
            break;
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            raw_fallback.push_str(trimmed);
            raw_fallback.push('\n');
            if let Some(data) = trimmed.strip_prefix("data:") {
                let data = data.trim();
                if data != "[DONE]" {
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        let chunk = v["choices"][0]["delta"]["content"]
                            .as_str()
                            .or_else(|| v["choices"][0]["message"]["content"].as_str())
                            .or_else(|| {
                                if v["type"] == "content_block_delta" {
                                    v["delta"]["text"].as_str()
                                } else {
                                    None
                                }
                            })
                            .or_else(|| {
                                v["candidates"][0]["content"]["parts"][0]["text"].as_str()
                            });
                        if let Some(delta) = chunk {
                            assembled.push_str(delta);
                            *stream_buf.lock() = Some(assembled.clone());
                            if let Some(cb) = wakeup {
                                cb();
                            }
                        } else if v["type"] == "error" {
                            if let Some(err_m) = v["error"]["message"].as_str() {
                                return Err(err_m.to_string());
                            }
                        }
                    }
                }
            }
        }
        line.clear();
    }

    let status = child.wait().map_err(|e| e.to_string())?;
    let err_str = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if !assembled.is_empty() {
        return Ok(assembled);
    }

    let raw = raw_fallback.trim();
    if let Ok(v) = serde_json::from_str::<Value>(raw) {
        if let Some(msg) = v["choices"][0]["message"]["content"].as_str() {
            return Ok(msg.to_string());
        }
        if let Some(msg) = v["content"][0]["text"].as_str() {
            return Ok(msg.to_string());
        }
        if let Some(msg) = v["candidates"][0]["content"]["parts"][0]["text"].as_str() {
            return Ok(msg.to_string());
        }
        if let Some(err_msg) = v["error"]["message"].as_str() {
            return Err(err_msg.to_string());
        }
    }

    if !err_str.is_empty() {
        Err(err_str)
    } else if !raw.is_empty() {
        Err(format!("API 返回非预期响应: {raw}"))
    } else if !status.success() {
        Err(format!("curl 退出码: {status}"))
    } else {
        Ok("（空响应）".to_string())
    }
}

fn execute_curl_json_post(
    url: &str,
    headers: &[(String, String)],
    body: &Value,
    active_pid: &Arc<Mutex<Option<u32>>>,
) -> Result<Value, String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-X", "POST", url]);
    for (k, v) in headers {
        cmd.arg("-H").arg(format!("{k}: {v}"));
    }
    cmd.args(["--data-binary", "@-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 curl.exe: {e}"))?;
    *active_pid.lock() = Some(child.id());

    if let Some(mut stdin) = child.stdin.take() {
        let payload = serde_json::to_vec(body).map_err(|e| e.to_string())?;
        let _ = stdin.write_all(&payload);
        let _ = stdin.flush();
    }

    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    *active_pid.lock() = None;
    let out_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let err_str = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if out_str.is_empty() {
        return Err(if !err_str.is_empty() {
            err_str
        } else {
            format!("curl 请求失败 ({})", output.status)
        });
    }

    let v: Value = serde_json::from_str(&out_str)
        .map_err(|e| format!("解析 API JSON 失败: {e}\n原始响应: {out_str}"))?;
    if let Some(err_msg) = v["error"]["message"].as_str() {
        return Err(err_msg.to_string());
    }
    Ok(v)
}

fn agent_tool_declarations() -> Vec<Value> {
    vec![
        json!({
            "name": "list_dir",
            "description": "List files and directories inside the workspace.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative directory path from workspace root (empty or '.' for root)." }
                }
            }
        }),
        json!({
            "name": "read_file",
            "description": "Read text file contents with line numbers.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative or workspace file path." },
                    "start_line": { "type": "integer", "description": "Optional 1-indexed start line." },
                    "end_line": { "type": "integer", "description": "Optional 1-indexed inclusive end line." }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "search_code",
            "description": "Search text across workspace files.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Text pattern to search." },
                    "max_results": { "type": "integer", "description": "Max matches (default 20)." }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "replace_in_file",
            "description": "Replace an exact substring in a workspace file (automatically creates checkpoint backup).",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative or workspace file path." },
                    "old_text": { "type": "string", "description": "Exact existing text to replace." },
                    "new_text": { "type": "string", "description": "Replacement text." }
                },
                "required": ["path", "old_text", "new_text"]
            }
        }),
        json!({
            "name": "write_file",
            "description": "Create or overwrite a workspace file (automatically creates checkpoint backup).",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative or workspace file path." },
                    "content": { "type": "string", "description": "Full file content." }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "run_command",
            "description": "Execute a shell command in workspace root (e.g., cargo check, cargo test, git status).",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "PowerShell command line to run." }
                },
                "required": ["command"]
            }
        }),
    ]
}

fn execute_agent_tool_loop(
    cfg: &AiApiConfig,
    root: &Path,
    system_prompt: &str,
    user_prompt: &str,
    ckpt_backups: &mut HashMap<PathBuf, Option<String>>,
    modified_inbox: &Arc<Mutex<Vec<PathBuf>>>,
    stream_buf: &Arc<Mutex<Option<String>>>,
    cancel: &Arc<AtomicBool>,
    active_pid: &Arc<Mutex<Option<u32>>>,
    wakeup: Option<&AgentWakeupCallback>,
) -> Result<(String, Vec<String>), String> {
    let decls = agent_tool_declarations();
    let mut step_logs: Vec<String> = Vec::new();
    let max_turns = 8usize;

    match cfg.format {
        AiApiFormat::OpenAiCompatible => {
            let base = cfg.base_url.trim_end_matches('/');
            let url = if base.ends_with("/chat/completions") {
                base.to_string()
            } else {
                format!("{base}/chat/completions")
            };
            let mut hdrs = vec![("Content-Type".to_string(), "application/json".to_string())];
            if !cfg.api_key.trim().is_empty() {
                hdrs.push(("Authorization".to_string(), format!("Bearer {}", cfg.api_key.trim())));
            }
            let tools: Vec<Value> = decls
                .iter()
                .map(|d| json!({ "type": "function", "function": d }))
                .collect();
            let mut msgs = vec![
                json!({ "role": "system", "content": system_prompt }),
                json!({ "role": "user", "content": user_prompt }),
            ];

            for turn in 0..max_turns {
                if cancel.load(Ordering::SeqCst) {
                    return Err("Cancelled".to_string());
                }
                let body = json!({
                    "model": cfg.model.trim(),
                    "messages": msgs,
                    "tools": tools,
                    "tool_choice": "auto"
                });
                let resp = execute_curl_json_post(&url, &hdrs, &body, active_pid)?;
                let msg = &resp["choices"][0]["message"];
                let content = msg["content"].as_str().unwrap_or("").to_string();
                let tool_calls = msg["tool_calls"].as_array().cloned().unwrap_or_default();

                if tool_calls.is_empty() {
                    return Ok((content, step_logs));
                }

                msgs.push(msg.clone());
                for tc in tool_calls {
                    let call_id = tc["id"].as_str().unwrap_or("call_1");
                    let name = tc["function"]["name"].as_str().unwrap_or("");
                    let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                    let args: Value = serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));

                    let step_desc = format!("⚙ [{}/{}] {}({})", turn + 1, max_turns, name, summarize_tool_args(&args));
                    step_logs.push(step_desc);
                    *stream_buf.lock() = Some(format!("{}\n\n正在执行工具 `{}`...", step_logs.join("\n"), name));
                    if let Some(cb) = wakeup {
                        cb();
                    }

                    let out = execute_workspace_tool(root, name, &args, ckpt_backups, modified_inbox);
                    step_logs.push(format!("  ✓ {name} -> {}", first_line_preview(&out)));
                    *stream_buf.lock() = Some(step_logs.join("\n"));
                    if let Some(cb) = wakeup {
                        cb();
                    }

                    msgs.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": out
                    }));
                }
            }
            Ok(("已达到单轮最大工具调用步数限制，以上为当前阶段执行结果。".to_string(), step_logs))
        }
        AiApiFormat::Anthropic => {
            let base = cfg.base_url.trim_end_matches('/');
            let url = if base.ends_with("/messages") {
                base.to_string()
            } else {
                format!("{base}/messages")
            };
            let hdrs = vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("x-api-key".to_string(), cfg.api_key.trim().to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ];
            let tools: Vec<Value> = decls
                .iter()
                .map(|d| {
                    json!({
                        "name": d["name"],
                        "description": d["description"],
                        "input_schema": d["parameters"]
                    })
                })
                .collect();
            let mut msgs = vec![json!({ "role": "user", "content": user_prompt })];

            for turn in 0..max_turns {
                if cancel.load(Ordering::SeqCst) {
                    return Err("Cancelled".to_string());
                }
                let body = json!({
                    "model": cfg.model.trim(),
                    "max_tokens": 4096,
                    "system": system_prompt,
                    "tools": tools,
                    "messages": msgs
                });
                let resp = execute_curl_json_post(&url, &hdrs, &body, active_pid)?;
                let blocks = resp["content"].as_array().cloned().unwrap_or_default();
                let mut text_parts = Vec::new();
                let mut tool_uses = Vec::new();
                for b in &blocks {
                    match b["type"].as_str() {
                        Some("text") => {
                            if let Some(t) = b["text"].as_str() {
                                text_parts.push(t.to_string());
                            }
                        }
                        Some("tool_use") => tool_uses.push(b.clone()),
                        _ => {}
                    }
                }
                if tool_uses.is_empty() {
                    return Ok((text_parts.join("\n"), step_logs));
                }
                msgs.push(json!({ "role": "assistant", "content": blocks }));
                let mut tool_results = Vec::new();
                for tu in tool_uses {
                    let id = tu["id"].as_str().unwrap_or("tu_1");
                    let name = tu["name"].as_str().unwrap_or("");
                    let args = &tu["input"];
                    step_logs.push(format!("⚙ [{}/{}] {}({})", turn + 1, max_turns, name, summarize_tool_args(args)));
                    *stream_buf.lock() = Some(step_logs.join("\n"));
                    if let Some(cb) = wakeup {
                        cb();
                    }
                    let out = execute_workspace_tool(root, name, args, ckpt_backups, modified_inbox);
                    step_logs.push(format!("  ✓ {name} -> {}", first_line_preview(&out)));
                    tool_results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": out
                    }));
                }
                msgs.push(json!({ "role": "user", "content": tool_results }));
            }
            Ok(("已达到单轮最大工具调用步数限制。".to_string(), step_logs))
        }
        AiApiFormat::Gemini => {
            let base = cfg.base_url.trim_end_matches('/');
            let url = format!(
                "{}/models/{}:generateContent?key={}",
                base,
                cfg.model.trim(),
                cfg.api_key.trim()
            );
            let hdrs = vec![("Content-Type".to_string(), "application/json".to_string())];
            let tools = json!([{ "functionDeclarations": decls }]);
            let mut contents = vec![json!({ "role": "user", "parts": [{ "text": user_prompt }] })];

            for turn in 0..max_turns {
                if cancel.load(Ordering::SeqCst) {
                    return Err("Cancelled".to_string());
                }
                let body = json!({
                    "systemInstruction": { "parts": [{ "text": system_prompt }] },
                    "tools": tools,
                    "contents": contents
                });
                let resp = execute_curl_json_post(&url, &hdrs, &body, active_pid)?;
                let cand_content = &resp["candidates"][0]["content"];
                let parts = cand_content["parts"].as_array().cloned().unwrap_or_default();
                let mut text_parts = Vec::new();
                let mut fn_calls = Vec::new();
                for p in &parts {
                    if let Some(t) = p["text"].as_str() {
                        text_parts.push(t.to_string());
                    }
                    if p.get("functionCall").is_some() {
                        fn_calls.push(p["functionCall"].clone());
                    }
                }
                if fn_calls.is_empty() {
                    return Ok((text_parts.join("\n"), step_logs));
                }
                contents.push(cand_content.clone());
                let mut resp_parts = Vec::new();
                for fc in fn_calls {
                    let name = fc["name"].as_str().unwrap_or("");
                    let args = &fc["args"];
                    step_logs.push(format!("⚙ [{}/{}] {}({})", turn + 1, max_turns, name, summarize_tool_args(args)));
                    *stream_buf.lock() = Some(step_logs.join("\n"));
                    if let Some(cb) = wakeup {
                        cb();
                    }
                    let out = execute_workspace_tool(root, name, args, ckpt_backups, modified_inbox);
                    step_logs.push(format!("  ✓ {name} -> {}", first_line_preview(&out)));
                    resp_parts.push(json!({
                        "functionResponse": {
                            "name": name,
                            "response": { "result": out }
                        }
                    }));
                }
                contents.push(json!({ "role": "user", "parts": resp_parts }));
            }
            Ok(("已达到单轮最大工具调用步数限制。".to_string(), step_logs))
        }
    }
}

fn summarize_tool_args(args: &Value) -> String {
    if let Some(p) = args["path"].as_str() {
        return p.to_string();
    }
    if let Some(q) = args["query"].as_str() {
        return q.to_string();
    }
    if let Some(c) = args["command"].as_str() {
        return c.chars().take(48).collect();
    }
    String::new()
}

fn first_line_preview(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(72)
        .collect()
}

fn resolve_safe_workspace_path(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Ok(root.to_path_buf());
    }
    let rel = Path::new(trimmed);
    let candidate = if rel.is_absolute() {
        rel.to_path_buf()
    } else {
        let mut clean = PathBuf::new();
        for comp in rel.components() {
            match comp {
                Component::Normal(s) => clean.push(s),
                Component::CurDir => {}
                Component::ParentDir => {
                    clean.pop();
                }
                _ => {}
            }
        }
        root.join(clean)
    };
    let root_canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let check_target = if candidate.exists() {
        fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone())
    } else if let Some(parent) = candidate.parent() {
        let p_canon = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
        if let Some(name) = candidate.file_name() {
            p_canon.join(name)
        } else {
            p_canon
        }
    } else {
        candidate.clone()
    };
    if !check_target.starts_with(&root_canon) {
        return Err(format!("安全限制：路径超出工作区根目录 ({})", candidate.display()));
    }
    Ok(candidate)
}

fn execute_workspace_tool(
    root: &Path,
    name: &str,
    args: &Value,
    ckpt_backups: &mut HashMap<PathBuf, Option<String>>,
    modified_inbox: &Arc<Mutex<Vec<PathBuf>>>,
) -> String {
    match name {
        "list_dir" => {
            let p_str = args["path"].as_str().unwrap_or(".");
            let dir = match resolve_safe_workspace_path(root, p_str) {
                Ok(d) => d,
                Err(e) => return e,
            };
            let Ok(rd) = fs::read_dir(&dir) else {
                return format!("Error: cannot read directory {}", dir.display());
            };
            let mut entries = Vec::new();
            for e in rd.flatten().take(120) {
                let name = e.file_name().to_string_lossy().to_string();
                if matches!(name.as_str(), ".git" | "target" | "node_modules") {
                    continue;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                entries.push(if is_dir { format!("{name}/") } else { name });
            }
            entries.sort();
            if entries.is_empty() {
                "(empty directory)".to_string()
            } else {
                entries.join("\n")
            }
        }
        "read_file" => {
            let p_str = args["path"].as_str().unwrap_or("");
            let path = match resolve_safe_workspace_path(root, p_str) {
                Ok(p) => p,
                Err(e) => return e,
            };
            let Ok(content) = fs::read_to_string(&path) else {
                return format!("Error: failed to read {}", path.display());
            };
            let lines: Vec<&str> = content.lines().collect();
            let start = args["start_line"]
                .as_u64()
                .map(|n| n.saturating_sub(1) as usize)
                .unwrap_or(0)
                .min(lines.len());
            let end = args["end_line"]
                .as_u64()
                .map(|n| n as usize)
                .unwrap_or((start + 200).min(lines.len()))
                .min(lines.len());
            let mut out = String::new();
            for (i, line) in lines[start..end].iter().enumerate() {
                out.push_str(&format!("{}: {}\n", start + i + 1, line));
            }
            if out.len() > 14000 {
                out.truncate(14000);
                out.push_str("\n...(truncated)");
            }
            out
        }
        "search_code" => {
            let query = args["query"].as_str().unwrap_or("").trim();
            if query.is_empty() {
                return "Error: empty search query".to_string();
            }
            let max_res = args["max_results"].as_u64().unwrap_or(20) as usize;
            let hits = find_in_files(root, query, max_res.min(40)).unwrap_or_default();
            if hits.is_empty() {
                return format!("No matches for `{query}`.");
            }
            hits.iter()
                .map(|h| {
                    let rel = h.path.strip_prefix(root).unwrap_or(&h.path).display();
                    format!("{rel}:{}: {}", h.line_number, h.line_text.trim())
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        "replace_in_file" => {
            let p_str = args["path"].as_str().unwrap_or("");
            let old_text = args["old_text"].as_str().unwrap_or("");
            let new_text = args["new_text"].as_str().unwrap_or("");
            let path = match resolve_safe_workspace_path(root, p_str) {
                Ok(p) => p,
                Err(e) => return e,
            };
            let Ok(content) = fs::read_to_string(&path) else {
                return format!("Error: failed to read {}", path.display());
            };
            let matches_cnt = content.matches(old_text).count();
            if matches_cnt == 0 {
                return "Error: old_text not found in file.".to_string();
            }
            if matches_cnt > 1 {
                return format!("Error: old_text matched {matches_cnt} times; provide more surrounding context.");
            }
            ckpt_backups
                .entry(path.clone())
                .or_insert_with(|| Some(content.clone()));
            let updated = content.replacen(old_text, new_text, 1);
            match fs::write(&path, updated) {
                Ok(()) => {
                    modified_inbox.lock().push(path.clone());
                    format!("OK: replaced block in {}", path.display())
                }
                Err(e) => format!("Error writing {}: {e}", path.display()),
            }
        }
        "write_file" => {
            let p_str = args["path"].as_str().unwrap_or("");
            let content = args["content"].as_str().unwrap_or("");
            let path = match resolve_safe_workspace_path(root, p_str) {
                Ok(p) => p,
                Err(e) => return e,
            };
            ckpt_backups
                .entry(path.clone())
                .or_insert_with(|| fs::read_to_string(&path).ok());
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::write(&path, content) {
                Ok(()) => {
                    modified_inbox.lock().push(path.clone());
                    format!("OK: wrote {} ({} bytes)", path.display(), content.len())
                }
                Err(e) => format!("Error writing {}: {e}", path.display()),
            }
        }
        "run_command" => {
            let cmd_line = args["command"].as_str().unwrap_or("").trim();
            if cmd_line.is_empty() {
                return "Error: empty command".to_string();
            }
            #[cfg(target_os = "windows")]
            let mut cmd = {
                let mut c = Command::new("powershell.exe");
                c.args(["-NoProfile", "-Command", cmd_line]);
                c.creation_flags(CREATE_NO_WINDOW);
                c
            };
            #[cfg(not(target_os = "windows"))]
            let mut cmd = {
                let mut c = Command::new("sh");
                c.args(["-c", cmd_line]);
                c
            };
            cmd.current_dir(root);
            match cmd.output() {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let mut combined = format!("Exit: {}\n{}\n{}", out.status, stdout.trim(), stderr.trim());
                    if combined.len() > 10000 {
                        combined.truncate(10000);
                        combined.push_str("\n...(truncated)");
                    }
                    combined
                }
                Err(e) => format!("Error executing command: {e}"),
            }
        }
        _ => format!("Error: unknown tool `{name}`"),
    }
}

fn strip_ansi_basic(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            while let Some(&nc) = chars.peek() {
                chars.next();
                if ('@'..='~').contains(&nc) {
                    break;
                }
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

fn build_patch_from_block(
    file_path: &Path,
    language: LanguageId,
    start_line: usize,
    end_line: usize,
    original_block: &str,
    proposed_code: String,
) -> ProposedFilePatch {
    let diff_lines = compute_line_diff(original_block, &proposed_code);
    let file_label = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    ProposedFilePatch {
        id: format!("patch-{}", &Uuid::new_v4().to_string()[..8]),
        file_path: file_path.to_path_buf(),
        language,
        start_line,
        end_line,
        summary: format!("{file_label} (L{}-L{})", start_line + 1, end_line + 1),
        original_code: original_block.to_string(),
        proposed_code,
        diff_lines,
        applied: false,
        dismissed: false,
    }
}

fn maybe_extract_patch_from_reply(
    reply: &str,
    mode: AgentWorkbenchMode,
    ctx: Option<&IdeCodeContext>,
) -> Option<ProposedFilePatch> {
    let c = ctx?;
    if mode == AgentWorkbenchMode::Ask && !reply.contains("```") {
        return None;
    }
    let code_block = extract_first_fenced_code_block(reply)?;
    let (start_line, end_line, original_block) =
        extract_enclosing_block(&c.file_text, c.cursor_line);
    if code_block.trim().is_empty() || code_block.trim() == original_block.trim() {
        return None;
    }
    Some(build_patch_from_block(
        &c.file_path,
        c.language,
        start_line,
        end_line,
        &original_block,
        code_block,
    ))
}

pub fn extract_first_fenced_code_block(text: &str) -> Option<String> {
    let mut in_fence = false;
    let mut buf = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !in_fence {
            if trimmed.starts_with("```") {
                in_fence = true;
            }
        } else if trimmed.starts_with("```") {
            if !buf.trim().is_empty() {
                return Some(buf);
            }
            in_fence = false;
            buf.clear();
        } else {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    None
}

/// 从编辑器全文中提取光标所在的函数 / struct / enum / impl 完整代码块（行范围 0-indexed inclusive）
pub fn extract_enclosing_block(text: &str, cursor_line: usize) -> (usize, usize, String) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return (0, 0, String::new());
    }
    let cur = cursor_line.min(lines.len() - 1);

    let mut start = cur;
    for i in (0..=cur).rev() {
        let trimmed = lines[i].trim_start();
        if trimmed.starts_with("pub fn ")
            || trimmed.starts_with("fn ")
            || trimmed.starts_with("pub async fn ")
            || trimmed.starts_with("async fn ")
            || trimmed.starts_with("pub struct ")
            || trimmed.starts_with("struct ")
            || trimmed.starts_with("pub enum ")
            || trimmed.starts_with("enum ")
            || trimmed.starts_with("impl ")
            || trimmed.starts_with("def ")
            || trimmed.starts_with("function ")
            || trimmed.starts_with("export function ")
        {
            start = i;
            break;
        }
        if cur - i >= 18 {
            start = cur.saturating_sub(3);
            break;
        }
    }

    let mut brace_depth: i32 = 0;
    let mut saw_brace = false;
    let mut end = (start + 8).min(lines.len() - 1);
    for (idx, line) in lines.iter().enumerate().skip(start).take(32) {
        for ch in line.chars() {
            if ch == '{' {
                brace_depth += 1;
                saw_brace = true;
            } else if ch == '}' {
                brace_depth -= 1;
            }
        }
        end = idx;
        if saw_brace && brace_depth <= 0 && idx >= cur {
            break;
        }
    }

    let slice = lines[start..=end].join("\n") + "\n";
    (start, end, slice)
}

/// Google Antigravity / Zed ACP (Agent Client Protocol) JSON-RPC 2.0 标准协议构造与解析器
pub struct AcpProtocol;

impl AcpProtocol {
    pub fn build_initialize_request(id: u64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": {
                        "readTextFile": true,
                        "writeTextFile": true
                    },
                    "terminal": true
                }
            }
        })
    }

    pub fn build_session_new_request(id: u64, cwd: &Path) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/new",
            "params": {
                "cwd": cwd.to_string_lossy(),
                "mcpServers": []
            }
        })
    }

    pub fn build_session_prompt_request(
        id: u64,
        session_id: &str,
        prompt_text: &str,
        active_file: Option<&Path>,
    ) -> Value {
        let mut prompt_blocks = vec![json!({
            "type": "text",
            "text": prompt_text
        })];
        if let Some(path) = active_file {
            prompt_blocks.push(json!({
                "type": "resource_link",
                "uri": format!("file:///{}", path.to_string_lossy().replace('\\', "/")),
                "name": path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
            }));
        }
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": prompt_blocks
            }
        })
    }
}

/// 自动扫描并兼容项目根目录下的 `.cursorrules`、`CLAUDE.md`、`AGENTS.md`
pub fn load_workspace_agent_rules(root: &Path) -> String {
    let candidates = ["CLAUDE.md", "AGENTS.md", ".cursorrules"];
    let mut found = Vec::new();
    for file in candidates {
        let p = root.join(file);
        if p.exists() {
            if let Ok(content) = fs::read_to_string(&p) {
                found.push(format!("{} ({} bytes)", file, content.len()));
            }
        }
    }
    if found.is_empty() {
        "• Rules: Auto-watching `CLAUDE.md`, `AGENTS.md`, `.cursorrules`".to_string()
    } else {
        format!("• Rules: {}", found.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acp_protocol_and_workspace_tools() {
        let init = AcpProtocol::build_initialize_request(1);
        assert_eq!(init["method"], "initialize");

        let tmp = std::env::temp_dir().join("rust_ai_ide_agent_tools_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let mut backups = HashMap::new();
        let mod_inbox = Arc::new(Mutex::new(Vec::new()));

        let write_out = execute_workspace_tool(
            &tmp,
            "write_file",
            &json!({"path": "sample.rs", "content": "fn old() -> i32 { 1 }\n"}),
            &mut backups,
            &mod_inbox,
        );
        assert!(write_out.starts_with("OK:"));
        assert_eq!(mod_inbox.lock().len(), 1);

        let rep_out = execute_workspace_tool(
            &tmp,
            "replace_in_file",
            &json!({"path": "sample.rs", "old_text": "1", "new_text": "42"}),
            &mut backups,
            &mod_inbox,
        );
        assert!(rep_out.starts_with("OK:"));
        let read_out = execute_workspace_tool(
            &tmp,
            "read_file",
            &json!({"path": "sample.rs"}),
            &mut backups,
            &mod_inbox,
        );
        assert!(read_out.contains("42"));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_extract_fenced_code_block() {
        let reply = "Here is the fix:\n```rust\nfn hello() {\n    println!(\"hi\");\n}\n```\nDone.";
        let block = extract_first_fenced_code_block(reply).unwrap();
        assert!(block.contains("fn hello()"));
        assert!(!block.contains("```"));
    }
}
