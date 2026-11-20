use crate::acp::execute_acp_stdio_session;
use crate::cli_discovery::{
    discover_local_cli_registrations, probe_agy_cli_models, probe_cli_rewind_capabilities,
    probe_cursor_cli_list_models, push_unique_model, resolve_agy_model_and_effort,
    resolve_cursor_model_id,
};
use crate::cli_runner::{
    compute_file_diff_counts, execute_cli_once, paths_same_file, CliRunParams, TurnDiffTracker,
};

fn build_turn_file_changes(
    modified_files: &[PathBuf],
    backups: &HashMap<PathBuf, Option<String>>,
    stats: &HashMap<PathBuf, (usize, usize)>,
) -> Vec<TurnFileChange> {
    modified_files
        .iter()
        .map(|p| {
            let (added_lines, removed_lines) = stats.get(p).copied().unwrap_or((0, 0));
            let before_content = backups.get(p).cloned().flatten();
            TurnFileChange {
                path: p.clone(),
                added_lines,
                removed_lines,
                before_content,
            }
        })
        .collect()
}
use crate::http_fallback::{execute_agent_tool_loop, execute_curl_llm_streaming};
use crate::tools::{
    extract_enclosing_block, extract_first_fenced_code_block, load_workspace_agent_rules,
    maybe_extract_patch_from_reply,
};
use crate::types::{
    AgentAttachedItem, AgentChatMessage, AgentProviderKind, AgentRegistration, AgentSession,
    AgentWakeupCallback, AgentWorkbenchMode, AiApiConfig, IdeCodeContext, InlineEditSession,
    PastedCodeSnippet, PersistedAgentHistory, ReasoningEffort, SessionFileChange,
    StreamingAgentProgress, TurnFileChange, TurnSyncAnchor, WorkspaceCheckpoint,
};
use ide_core::LanguageId;
use ide_search::find_in_files;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use uuid::Uuid;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

struct PendingInlineMeta {
    file_path: PathBuf,
    language: LanguageId,
    start_line: usize,
    end_line: usize,
    prompt: String,
    original_code: String,
}

enum ExecutionBackend {
    HttpApi(AiApiConfig),
    Acp(String),
    Cli(CliRunParams),
    LocalOffline,
}

#[derive(Clone)]
struct DiscoveredModelUpdate {
    kind: AgentProviderKind,
    display_models: Vec<String>,
    exact_catalog: Vec<String>,
    supports_session_resume: Option<bool>,
    supports_checkpoint_rewind: Option<bool>,
}

struct CliTurnSyncUpdate {
    session_id: String,
    user_msg_idx: usize,
    cli_session_id: Option<String>,
    checkpoint_backups: HashMap<PathBuf, Option<String>>,
    file_diff_stats: HashMap<PathBuf, (usize, usize)>,
}

#[derive(Clone)]
struct SessionExecutionSlot {
    is_busy: Arc<AtomicBool>,
    cancel_requested: Arc<AtomicBool>,
    active_child_pid: Arc<Mutex<Option<u32>>>,
    streaming_text: Arc<Mutex<Option<StreamingAgentProgress>>>,
}

impl SessionExecutionSlot {
    fn new() -> Self {
        Self {
            is_busy: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            active_child_pid: Arc::new(Mutex::new(None)),
            streaming_text: Arc::new(Mutex::new(None)),
        }
    }
}

pub struct AgentHub {
    pub workspace_root: PathBuf,
    pub active_provider: AgentProviderKind,
    pub providers: Vec<AgentRegistration>,
    pub api_config: AiApiConfig,
    pub messages: Vec<AgentChatMessage>,
    pub inline_edit: Option<InlineEditSession>,
    pub checkpoints: Vec<WorkspaceCheckpoint>,
    pub sessions: Vec<AgentSession>,
    pub active_session_idx: usize,
    next_session_seq: usize,
    pub loaded_rules_summary: String,
    /// 全局汇总忙标志：任意会话正在运行时为 true（供 UI 动画驱动）
    pub is_busy: Arc<AtomicBool>,
    session_slots: Arc<Mutex<HashMap<String, SessionExecutionSlot>>>,
    async_inbox: Arc<Mutex<Vec<(String, AgentChatMessage)>>>,
    async_checkpoints_inbox: Arc<Mutex<Vec<(String, WorkspaceCheckpoint)>>>,
    async_cli_sync_inbox: Arc<Mutex<Vec<CliTurnSyncUpdate>>>,
    modified_files_inbox: Arc<Mutex<Vec<PathBuf>>>,
    async_inline_result: Arc<Mutex<Option<(PendingInlineMeta, Result<String, String>)>>>,
    async_commit_msg: Arc<Mutex<Option<Result<String, String>>>>,
    async_discovered_models: Arc<Mutex<Vec<DiscoveredModelUpdate>>>,
    history_file_path: Option<PathBuf>,
    wakeup: Option<AgentWakeupCallback>,
}

impl AgentHub {
    pub fn new(workspace_root: &Path) -> Self {
        Self::new_with_wakeup(workspace_root, None)
    }

    pub fn new_with_wakeup(workspace_root: &Path, wakeup: Option<AgentWakeupCallback>) -> Self {
        let api_config = AiApiConfig::load_for_workspace(workspace_root);
        let providers = discover_local_cli_registrations();
        let active_provider = providers
            .iter()
            .find(|p| p.available)
            .map(|p| p.kind)
            .unwrap_or(AgentProviderKind::CursorCli);
        let active_reg = providers.iter().find(|p| p.kind == active_provider);
        let active_model = active_reg
            .map(|p| p.active_model.clone())
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "auto".to_string());
        let active_fast = active_reg.map(|p| p.fast_mode).unwrap_or(false);
        let active_effort = active_reg
            .map(|p| p.reasoning_effort)
            .unwrap_or(ReasoningEffort::Medium);

        let rules_summary = load_workspace_agent_rules(workspace_root);
        let detected_summary: Vec<String> = providers
            .iter()
            .filter(|p| p.available)
            .map(|p| format!("{} [{}]", p.kind.short_name(), p.active_model))
            .collect();
        let route_summary = if detected_summary.is_empty() {
            "未检测到本地 CLI（请安装 cursor-agent / agy / claude / codex CLI）"
                .to_string()
        } else {
            format!("已自动发现本地 CLI 与模型：{}", detected_summary.join(" · "))
        };

        let messages = vec![AgentChatMessage {
            role: "system".to_string(),
            agent: active_provider,
            content: format!(
                "AI 智能体工作台已就绪（零配置直连本地 CLI）\n• {}\n• 支持多会话切换、文件/图片直接拖拽预览、粘贴长代码折叠显示与文本/文件引用跳转\n• Ask：实时问答 | Edit：生成补丁卡片 | Agent：真实调用本地 CLI 读写工作区文件\n{}",
                route_summary, rules_summary
            ),
            thought: Some(
                "Local CLI AI Agent Hub initialized with multi-session & drag-drop support."
                    .to_string(),
            ),
            checkpoint_id: None,
            patch: None,
            modified_files: Vec::new(),
            file_changes: Vec::new(),
            attachments: Vec::new(),
        }];

        let first_session = AgentSession {
            id: format!("sess-{}", &Uuid::new_v4().to_string()[..8]),
            title: "会话 1".to_string(),
            custom_title: false,
            provider: active_provider,
            active_model,
            fast_mode: active_fast,
            reasoning_effort: active_effort,
            messages: Vec::new(),
            checkpoints: Vec::new(),
            attached_items: Vec::new(),
            pasted_snippets: Vec::new(),
            draft_prompt: String::new(),
            is_busy: false,
            created_order: 1,
            cli_session_id: None,
            pending_fork_session: false,
            changed_files: Vec::new(),
            turn_anchors: Vec::new(),
        };

        let hub = Self {
            workspace_root: workspace_root.to_path_buf(),
            active_provider,
            providers,
            api_config,
            messages,
            inline_edit: None,
            checkpoints: Vec::new(),
            sessions: vec![first_session],
            active_session_idx: 0,
            next_session_seq: 2,
            loaded_rules_summary: rules_summary,
            is_busy: Arc::new(AtomicBool::new(false)),
            session_slots: Arc::new(Mutex::new(HashMap::new())),
            async_inbox: Arc::new(Mutex::new(Vec::new())),
            async_checkpoints_inbox: Arc::new(Mutex::new(Vec::new())),
            async_cli_sync_inbox: Arc::new(Mutex::new(Vec::new())),
            modified_files_inbox: Arc::new(Mutex::new(Vec::new())),
            async_inline_result: Arc::new(Mutex::new(None)),
            async_commit_msg: Arc::new(Mutex::new(None)),
            async_discovered_models: Arc::new(Mutex::new(Vec::new())),
            history_file_path: None,
            wakeup,
        };
        if hub.wakeup.is_some() {
            hub.spawn_async_cli_model_probe();
        }
        hub
    }

    pub fn set_history_file_path(&mut self, path: Option<PathBuf>) {
        self.history_file_path = path.clone();
        if let Some(p) = path {
            if let Ok(raw) = fs::read_to_string(&p) {
                if let Ok(hist) = serde_json::from_str::<PersistedAgentHistory>(&raw) {
                    if !hist.sessions.is_empty() {
                        self.import_history(hist);
                    }
                }
            }
        }
    }

    pub fn export_history(&self) -> PersistedAgentHistory {
        let mut sessions = self.sessions.clone();
        let active_idx = self.active_session_idx.min(sessions.len().saturating_sub(1));
        if let Some(s) = sessions.get_mut(active_idx) {
            s.messages = self.messages.clone();
            s.checkpoints = self.checkpoints.clone();
            s.provider = self.active_provider;
            s.active_model = self.active_model();
            s.fast_mode = self.active_fast_mode();
            s.reasoning_effort = self.active_reasoning_effort();
        }
        for s in &mut sessions {
            s.is_busy = false;
        }
        PersistedAgentHistory {
            active_provider: self.active_provider,
            sessions,
            active_session_idx: active_idx,
            next_session_seq: self.next_session_seq,
        }
    }

    pub fn save_history(&self) {
        let Some(path) = &self.history_file_path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.export_history()) {
            let _ = fs::write(path, json);
        }
    }

    pub fn import_history(&mut self, mut hist: PersistedAgentHistory) {
        if hist.sessions.is_empty() {
            return;
        }
        for s in &mut hist.sessions {
            s.is_busy = false;
        }
        let target_idx = hist.active_session_idx.min(hist.sessions.len() - 1);
        self.sessions = hist.sessions;
        self.next_session_seq = hist.next_session_seq.max(self.sessions.len() + 1);
        self.active_session_idx = target_idx;
        let s = &self.sessions[target_idx];
        self.messages = s.messages.clone();
        self.checkpoints = s.checkpoints.clone();
        self.active_provider = if s.provider != AgentProviderKind::CustomLlmApi
            || hist.active_provider == AgentProviderKind::CustomLlmApi
        {
            s.provider
        } else {
            hist.active_provider
        };
        let prov = self.active_provider;
        let model = s.active_model.clone();
        let fast = s.fast_mode;
        let effort = s.reasoning_effort;
        if let Some(reg) = self.providers.iter_mut().find(|p| p.kind == prov) {
            if !model.is_empty() {
                reg.active_model = model.clone();
                push_unique_model(&mut reg.discovered_models, model);
            }
            reg.fast_mode = fast;
            reg.reasoning_effort = effort;
        }
        self.update_registration_status(prov);
    }

    pub fn set_wakeup(&mut self, wakeup: Option<AgentWakeupCallback>) {
        self.wakeup = wakeup;
    }

    fn effective_provider_kind(&self) -> AgentProviderKind {
        if self.active_provider == AgentProviderKind::CursorInlineEngine {
            self.primary_available_cli_kind()
        } else {
            self.active_provider
        }
    }

    pub fn active_registration(&self) -> Option<&AgentRegistration> {
        let k = self.effective_provider_kind();
        self.providers.iter().find(|p| p.kind == k)
    }

    pub fn active_model(&self) -> String {
        self.active_registration()
            .map(|p| p.active_model.clone())
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "auto".to_string())
    }

    pub fn active_resolved_cli_model(&self) -> String {
        let Some(reg) = self.active_registration() else {
            return "auto".to_string();
        };
        match reg.kind {
            AgentProviderKind::CursorCli | AgentProviderKind::CursorInlineEngine => {
                resolve_cursor_model_id(
                    &reg.active_model,
                    reg.reasoning_effort,
                    reg.fast_mode,
                    &reg.exact_model_catalog,
                )
            }
            AgentProviderKind::GoogleAntigravityAcp => {
                resolve_agy_model_and_effort(
                    &reg.active_model,
                    reg.reasoning_effort,
                    &reg.exact_model_catalog,
                )
                .0
            }
            _ => reg.active_model.clone(),
        }
    }

    pub fn active_fast_mode(&self) -> bool {
        self.active_registration()
            .map(|p| p.fast_mode)
            .unwrap_or(false)
    }

    pub fn active_reasoning_effort(&self) -> ReasoningEffort {
        self.active_registration()
            .map(|p| p.reasoning_effort)
            .unwrap_or(ReasoningEffort::Medium)
    }

    pub fn supports_fast_mode(&self) -> bool {
        self.effective_provider_kind().supports_fast_mode()
    }

    pub fn supports_reasoning_effort(&self) -> bool {
        self.effective_provider_kind().supports_reasoning_effort()
    }

    pub fn active_cli_supports_session_resume(&self) -> bool {
        if self.active_provider == AgentProviderKind::CustomLlmApi {
            return false;
        }
        self.active_registration()
            .map(|r| r.available && r.supports_session_resume)
            .unwrap_or(false)
    }

    pub fn active_cli_supports_checkpoint_rewind(&self) -> bool {
        if self.active_provider == AgentProviderKind::CustomLlmApi {
            return false;
        }
        self.active_registration()
            .map(|r| r.available && r.supports_checkpoint_rewind)
            .unwrap_or(false)
    }

    pub fn set_provider_capabilities(
        &mut self,
        kind: AgentProviderKind,
        available: bool,
        supports_session_resume: bool,
        supports_checkpoint_rewind: bool,
    ) {
        if let Some(reg) = self.providers.iter_mut().find(|p| p.kind == kind) {
            reg.available = available;
            reg.supports_session_resume = supports_session_resume;
            reg.supports_checkpoint_rewind = supports_checkpoint_rewind;
        }
    }

    pub fn supported_modes_for_active(&self) -> &'static [AgentWorkbenchMode] {
        self.effective_provider_kind().supported_modes()
    }

    pub fn supports_mode_for_active(&self, mode: AgentWorkbenchMode) -> bool {
        self.effective_provider_kind().supports_mode(mode)
    }

    pub fn normalize_mode_for_active(&self, mode: AgentWorkbenchMode) -> AgentWorkbenchMode {
        self.effective_provider_kind().normalize_mode(mode)
    }

    pub fn next_mode_for_active(&self, current: AgentWorkbenchMode) -> AgentWorkbenchMode {
        self.effective_provider_kind().next_supported_mode(current)
    }

    pub fn mode_cli_flag_for_active(&self, mode: AgentWorkbenchMode) -> &'static str {
        self.effective_provider_kind().mode_cli_flag(mode)
    }

    pub fn mode_description_for_active(&self, mode: AgentWorkbenchMode, zh: bool) -> &'static str {
        self.effective_provider_kind().mode_description(mode, zh)
    }

    pub fn available_models_for_active(&self) -> Vec<String> {
        self.active_registration()
            .map(|p| p.discovered_models.clone())
            .unwrap_or_default()
    }

    pub fn select_model_for_active(&mut self, model: impl Into<String>) {
        let k = self.effective_provider_kind();
        self.select_model_for_provider(k, model.into());
    }

    pub fn toggle_fast_mode_for_active(&mut self) -> bool {
        let k = self.effective_provider_kind();
        let next = if let Some(reg) = self.providers.iter_mut().find(|p| p.kind == k) {
            reg.fast_mode = !reg.fast_mode;
            reg.fast_mode
        } else {
            return false;
        };
        self.update_registration_status(k);
        next
    }

    pub fn cycle_reasoning_effort_for_active(&mut self) -> ReasoningEffort {
        let k = self.effective_provider_kind();
        let next = if let Some(reg) = self.providers.iter_mut().find(|p| p.kind == k) {
            reg.reasoning_effort = reg.reasoning_effort.next();
            if k == AgentProviderKind::GoogleAntigravityAcp {
                let (resolved, _) = resolve_agy_model_and_effort(
                    &reg.active_model,
                    reg.reasoning_effort,
                    &reg.exact_model_catalog,
                );
                reg.active_model = resolved;
            }
            reg.reasoning_effort
        } else {
            ReasoningEffort::Medium
        };
        self.update_registration_status(k);
        next
    }

    pub fn select_model_for_provider(&mut self, kind: AgentProviderKind, model: String) {
        let trimmed = model.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        if let Some(reg) = self.providers.iter_mut().find(|p| p.kind == kind) {
            reg.active_model = trimmed.clone();
            if !reg.discovered_models.iter().any(|x| x == &trimmed) {
                reg.discovered_models.insert(0, trimmed);
            }
        }
        self.update_registration_status(kind);
    }

    fn update_registration_status(&mut self, kind: AgentProviderKind) {
        if let Some(reg) = self.providers.iter_mut().find(|p| p.kind == kind) {
            if let Some(bin) = &reg.binary_path {
                let bin_name = bin
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("cli");
                let resolved = match kind {
                    AgentProviderKind::CursorCli => resolve_cursor_model_id(
                        &reg.active_model,
                        reg.reasoning_effort,
                        reg.fast_mode,
                        &reg.exact_model_catalog,
                    ),
                    AgentProviderKind::GoogleAntigravityAcp => {
                        resolve_agy_model_and_effort(
                            &reg.active_model,
                            reg.reasoning_effort,
                            &reg.exact_model_catalog,
                        )
                        .0
                    }
                    _ => reg.active_model.clone(),
                };
                reg.status_text = format!(
                    "{bin_name} · {resolved}{}",
                    if kind == AgentProviderKind::CursorCli && reg.fast_mode {
                        " · ⚡快速"
                    } else {
                        ""
                    }
                );
            }
        }
    }

    /// 重新扫描本机所有 CLI 二进制、配置文件，并异步探测实时模型目录
    pub fn refresh_local_cli_agents(&mut self) {
        let prev_state: HashMap<AgentProviderKind, (String, bool, ReasoningEffort)> = self
            .providers
            .iter()
            .map(|p| (p.kind, (p.active_model.clone(), p.fast_mode, p.reasoning_effort)))
            .collect();
        let mut fresh = discover_local_cli_registrations();
        for reg in &mut fresh {
            if let Some((prev_m, prev_fast, prev_eff)) = prev_state.get(&reg.kind) {
                if !prev_m.is_empty() {
                    reg.active_model = prev_m.clone();
                    push_unique_model(&mut reg.discovered_models, prev_m);
                }
                reg.fast_mode = *prev_fast;
                reg.reasoning_effort = *prev_eff;
            }
        }
        self.providers = fresh;
        for kind in AgentProviderKind::all() {
            self.update_registration_status(*kind);
        }
        self.spawn_async_cli_model_probe();
    }

    fn primary_available_cli_kind(&self) -> AgentProviderKind {
        for kind in AgentProviderKind::all() {
            if self
                .providers
                .iter()
                .any(|p| p.kind == *kind && p.available)
            {
                return *kind;
            }
        }
        AgentProviderKind::CursorCli
    }

    fn spawn_async_cli_model_probe(&self) {
        use std::sync::OnceLock;
        type ProbeCache = Mutex<HashMap<(AgentProviderKind, PathBuf), DiscoveredModelUpdate>>;
        static CLI_PROBE_CACHE: OnceLock<ProbeCache> = OnceLock::new();
        let probe_cache = CLI_PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

        let bins: Vec<(AgentProviderKind, PathBuf)> = self
            .providers
            .iter()
            .filter_map(|p| p.binary_path.clone().map(|b| (p.kind, b)))
            .collect();

        for (kind, bin) in bins {
            if let Some(cached) = probe_cache.lock().get(&(kind, bin.clone())).cloned() {
                self.async_discovered_models.lock().push(cached);
                if let Some(cb) = &self.wakeup {
                    cb();
                }
                continue;
            }
            let slot = self.async_discovered_models.clone();
            let wakeup = self.wakeup.clone();
            thread::spawn(move || {
                let (display_models, exact_catalog) = match kind {
                    AgentProviderKind::CursorCli => probe_cursor_cli_list_models(&bin)
                        .unwrap_or_default(),
                    AgentProviderKind::GoogleAntigravityAcp => {
                        let m = probe_agy_cli_models(&bin).unwrap_or_default();
                        (m.clone(), m)
                    }
                    _ => (Vec::new(), Vec::new()),
                };
                let caps = probe_cli_rewind_capabilities(kind, &bin);
                if !display_models.is_empty() || !exact_catalog.is_empty() || caps.is_some() {
                    let update = DiscoveredModelUpdate {
                        kind,
                        display_models,
                        exact_catalog,
                        supports_session_resume: caps.map(|c| c.0),
                        supports_checkpoint_rewind: caps.map(|c| c.1),
                    };
                    let cache = CLI_PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
                    cache.lock().insert((kind, bin), update.clone());
                    slot.lock().push(update);
                    if let Some(cb) = wakeup {
                        cb();
                    }
                }
            });
        }
    }

    pub fn update_api_config(&mut self, new_cfg: AiApiConfig) -> anyhow::Result<()> {
        self.api_config = new_cfg;
        if !self.api_config.model.trim().is_empty() {
            self.select_model_for_active(self.api_config.model.clone());
        }
        Ok(())
    }

    pub fn active_session(&self) -> &AgentSession {
        let idx = self.active_session_idx.min(self.sessions.len().saturating_sub(1));
        &self.sessions[idx]
    }

    pub fn active_session_mut(&mut self) -> &mut AgentSession {
        let idx = self.active_session_idx.min(self.sessions.len().saturating_sub(1));
        &mut self.sessions[idx]
    }

    fn sync_active_session_out(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let idx = self.active_session_idx.min(self.sessions.len() - 1);
        let provider = self.active_provider;
        let model = self.active_model();
        let fast = self.active_fast_mode();
        let effort = self.active_reasoning_effort();
        let msgs = std::mem::take(&mut self.messages);
        let cks = std::mem::take(&mut self.checkpoints);
        let s = &mut self.sessions[idx];
        s.messages = msgs;
        s.checkpoints = cks;
        s.provider = provider;
        s.active_model = model;
        s.fast_mode = fast;
        s.reasoning_effort = effort;
    }

    fn sync_session_in(&mut self, idx: usize) {
        if idx >= self.sessions.len() {
            return;
        }
        self.active_session_idx = idx;
        let (msgs, cks, prov, model, fast, effort) = {
            let s = &mut self.sessions[idx];
            (
                std::mem::take(&mut s.messages),
                std::mem::take(&mut s.checkpoints),
                s.provider,
                s.active_model.clone(),
                s.fast_mode,
                s.reasoning_effort,
            )
        };
        self.messages = msgs;
        self.checkpoints = cks;
        self.active_provider = prov;
        if let Some(reg) = self.providers.iter_mut().find(|p| p.kind == prov) {
            if !model.is_empty() {
                reg.active_model = model.clone();
                push_unique_model(&mut reg.discovered_models, &model);
            }
            reg.fast_mode = fast;
            reg.reasoning_effort = effort;
        }
        self.update_registration_status(prov);
    }

    pub fn new_session(&mut self) -> usize {
        self.sync_active_session_out();
        let seq = self.next_session_seq;
        self.next_session_seq += 1;
        let sess = AgentSession {
            id: format!("sess-{}", &Uuid::new_v4().to_string()[..8]),
            title: format!("会话 {seq}"),
            custom_title: false,
            provider: self.active_provider,
            active_model: self.active_model(),
            fast_mode: self.active_fast_mode(),
            reasoning_effort: self.active_reasoning_effort(),
            messages: Vec::new(),
            checkpoints: Vec::new(),
            attached_items: Vec::new(),
            pasted_snippets: Vec::new(),
            draft_prompt: String::new(),
            is_busy: false,
            created_order: seq,
            cli_session_id: None,
            pending_fork_session: false,
            changed_files: Vec::new(),
            turn_anchors: Vec::new(),
        };
        self.sessions.push(sess);
        let new_idx = self.sessions.len() - 1;
        self.sync_session_in(new_idx);
        self.save_history();
        new_idx
    }

    pub fn switch_session(&mut self, target_idx: usize) -> bool {
        if target_idx >= self.sessions.len() || target_idx == self.active_session_idx {
            return false;
        }
        self.sync_active_session_out();
        self.sync_session_in(target_idx);
        self.save_history();
        true
    }

    pub fn close_session(&mut self, idx: usize) -> bool {
        if idx >= self.sessions.len() {
            return false;
        }
        if self.sessions.len() == 1 {
            self.messages.clear();
            self.checkpoints.clear();
            let seq = self.next_session_seq;
            self.next_session_seq += 1;
            let s = &mut self.sessions[0];
            s.id = format!("sess-{}", &Uuid::new_v4().to_string()[..8]);
            s.title = format!("会话 {seq}");
            s.custom_title = false;
            s.attached_items.clear();
            s.pasted_snippets.clear();
            s.draft_prompt.clear();
            s.is_busy = false;
            self.save_history();
            return true;
        }
        self.sync_active_session_out();
        let removed_id = self.sessions.remove(idx).id;
        {
            let mut slots = self.session_slots.lock();
            if let Some(slot) = slots.remove(&removed_id) {
                slot.cancel_requested.store(true, Ordering::SeqCst);
                Self::kill_slot_process(&slot);
                slot.is_busy.store(false, Ordering::SeqCst);
            }
            Self::sync_global_busy_flag(&slots, &self.is_busy);
        }
        let next_idx = if idx < self.active_session_idx {
            self.active_session_idx - 1
        } else {
            self.active_session_idx.min(self.sessions.len() - 1)
        };
        self.sync_session_in(next_idx);
        self.save_history();
        true
    }

    pub fn rename_active_session(&mut self, title: impl Into<String>) {
        let t = title.into().trim().to_string();
        if t.is_empty() {
            return;
        }
        let s = self.active_session_mut();
        s.title = t;
        s.custom_title = true;
        self.save_history();
    }

    pub fn attach_item_to_active_session(&mut self, item: AgentAttachedItem) {
        let s = self.active_session_mut();
        if let Some(existing) = s.attached_items.iter_mut().find(|x| x.path == item.path) {
            *existing = item;
        } else {
            s.attached_items.push(item);
        }
    }

    pub fn remove_attached_item_from_active(&mut self, path: &Path) {
        self.active_session_mut()
            .attached_items
            .retain(|x| x.path != path);
    }

    pub fn add_pasted_snippet_to_active_session(&mut self, snippet: PastedCodeSnippet) {
        self.active_session_mut().pasted_snippets.push(snippet);
    }

    pub fn remove_pasted_snippet_from_active(&mut self, id: &str) {
        self.active_session_mut()
            .pasted_snippets
            .retain(|s| s.id != id);
    }

    fn sync_global_busy_flag(
        slots: &HashMap<String, SessionExecutionSlot>,
        global_busy: &AtomicBool,
    ) {
        let any_busy = slots
            .values()
            .any(|s| s.is_busy.load(Ordering::Relaxed));
        global_busy.store(any_busy, Ordering::SeqCst);
    }

    fn try_begin_session_execution(
        &mut self,
        session_id: &str,
        initial_progress: StreamingAgentProgress,
    ) -> Option<SessionExecutionSlot> {
        let slot = {
            let mut slots = self.session_slots.lock();
            let slot = slots
                .entry(session_id.to_string())
                .or_insert_with(SessionExecutionSlot::new)
                .clone();
            if slot.is_busy.swap(true, Ordering::SeqCst) {
                return None;
            }
            slot.cancel_requested.store(false, Ordering::SeqCst);
            *slot.active_child_pid.lock() = None;
            *slot.streaming_text.lock() = Some(initial_progress);
            self.is_busy.store(true, Ordering::SeqCst);
            slot
        };
        if let Some(sess) = self.sessions.iter_mut().find(|s| s.id == session_id) {
            sess.is_busy = true;
        }
        Some(slot)
    }

    fn finish_session_slot(
        slots: &Arc<Mutex<HashMap<String, SessionExecutionSlot>>>,
        global_busy: &Arc<AtomicBool>,
        session_id: &str,
    ) {
        let guard = slots.lock();
        if let Some(slot) = guard.get(session_id) {
            *slot.active_child_pid.lock() = None;
            *slot.streaming_text.lock() = None;
            slot.is_busy.store(false, Ordering::SeqCst);
        }
        Self::sync_global_busy_flag(&guard, global_busy);
    }

    fn kill_slot_process(slot: &SessionExecutionSlot) {
        if let Some(pid) = slot.active_child_pid.lock().take() {
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
    }

    pub fn is_active_session_busy(&self) -> bool {
        let active_id = self.active_session().id.as_str();
        let slots = self.session_slots.lock();
        slots
            .get(active_id)
            .map(|s| s.is_busy.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    pub fn streaming_progress(&self) -> Option<StreamingAgentProgress> {
        let active_id = self.active_session().id.as_str();
        let slot = {
            let slots = self.session_slots.lock();
            slots.get(active_id).cloned()
        }?;
        if !slot.is_busy.load(Ordering::Relaxed) {
            return None;
        }
        let progress = slot.streaming_text.lock().clone();
        progress
    }

    pub fn set_active_streaming_progress(&mut self, progress: Option<StreamingAgentProgress>) {
        let sid = self.active_session().id.clone();
        let mut slots = self.session_slots.lock();
        match progress {
            Some(p) => {
                let slot = slots
                    .entry(sid)
                    .or_insert_with(SessionExecutionSlot::new);
                *slot.streaming_text.lock() = Some(p);
                slot.is_busy.store(true, Ordering::SeqCst);
                Self::sync_global_busy_flag(&slots, &self.is_busy);
                drop(slots);
                self.active_session_mut().is_busy = true;
            }
            None => {
                if let Some(slot) = slots.get(&sid) {
                    *slot.streaming_text.lock() = None;
                    slot.is_busy.store(false, Ordering::SeqCst);
                }
                Self::sync_global_busy_flag(&slots, &self.is_busy);
                drop(slots);
                self.active_session_mut().is_busy = false;
            }
        }
    }

    pub fn streaming_preview(&self) -> Option<String> {
        self.streaming_progress().map(|p| p.to_preview_string())
    }

    pub fn cancel_active_request(&mut self) {
        let active_id = self.active_session().id.clone();
        let slot = {
            let slots = self.session_slots.lock();
            slots.get(&active_id).cloned()
        };
        let Some(slot) = slot else {
            return;
        };
        if !slot.is_busy.load(Ordering::Relaxed) {
            return;
        }
        slot.cancel_requested.store(true, Ordering::SeqCst);
        Self::kill_slot_process(&slot);
        slot.is_busy.store(false, Ordering::SeqCst);
        let partial = slot.streaming_text.lock().take();
        {
            let slots = self.session_slots.lock();
            Self::sync_global_busy_flag(&slots, &self.is_busy);
        }
        let (content, thought) = match partial {
            Some(prog) => {
                let c = if prog.reply_text.trim().is_empty() {
                    "⏹ 已取消当前 AI 请求。".to_string()
                } else {
                    format!("{}\n\n[⏹ 已手动停止生成]", prog.reply_text.trim_end())
                };
                let t = prog
                    .format_thought_summary()
                    .unwrap_or_else(|| "Request cancelled by user.".to_string());
                (c, Some(t))
            }
            None => (
                "⏹ 已取消当前 AI 请求。".to_string(),
                Some("Request cancelled by user.".to_string()),
            ),
        };
        let msg = AgentChatMessage {
            role: "assistant".to_string(),
            agent: self.active_provider,
            content,
            thought,
            checkpoint_id: None,
            patch: None,
            modified_files: Vec::new(),
            file_changes: Vec::new(),
            attachments: Vec::new(),
        };
        self.messages.push(msg);
        self.active_session_mut().is_busy = false;
        self.save_history();
        if let Some(cb) = &self.wakeup {
            cb();
        }
    }

    /// 轮询后台异步消息、Checkpoint 与自动探测到的 CLI 模型列表；若有更新返回 true
    pub fn poll_async_messages(&mut self) -> bool {
        let mut changed = false;
        {
            let slots = self.session_slots.lock();
            for sess in &mut self.sessions {
                let should_busy = slots
                    .get(&sess.id)
                    .map(|s| s.is_busy.load(Ordering::Relaxed))
                    .unwrap_or(false);
                if sess.is_busy != should_busy {
                    sess.is_busy = should_busy;
                    changed = true;
                }
            }
            Self::sync_global_busy_flag(&slots, &self.is_busy);
        }

        let updates: Vec<DiscoveredModelUpdate> = {
            let mut guard = self.async_discovered_models.lock();
            guard.drain(..).collect()
        };
        for upd in updates {
            let mut touched = false;
            if let Some(reg) = self.providers.iter_mut().find(|p| p.kind == upd.kind) {
                for m in upd.display_models {
                    push_unique_model(&mut reg.discovered_models, m);
                }
                if !upd.exact_catalog.is_empty() {
                    reg.exact_model_catalog = upd.exact_catalog;
                }
                if let Some(res) = upd.supports_session_resume {
                    reg.supports_session_resume = res;
                }
                if let Some(rw) = upd.supports_checkpoint_rewind {
                    reg.supports_checkpoint_rewind = rw;
                }
                if reg.active_model.is_empty() {
                    if let Some(first) = reg.discovered_models.first() {
                        reg.active_model = first.clone();
                    }
                }
                touched = true;
                changed = true;
            }
            if touched {
                self.update_registration_status(upd.kind);
            }
        }
        let mut history_dirty = false;
        let active_id = self.active_session().id.clone();
        {
            let mut ck_inbox = self.async_checkpoints_inbox.lock();
            for (sid, ckpt) in ck_inbox.drain(..) {
                if sid == active_id {
                    self.checkpoints.push(ckpt);
                } else if let Some(sess) = self.sessions.iter_mut().find(|s| s.id == sid) {
                    sess.checkpoints.push(ckpt);
                }
                changed = true;
                history_dirty = true;
            }
        }
        {
            let mut inbox = self.async_inbox.lock();
            for (sid, msg) in inbox.drain(..) {
                if sid == active_id {
                    self.messages.push(msg);
                } else if let Some(sess) = self.sessions.iter_mut().find(|s| s.id == sid) {
                    sess.messages.push(msg);
                }
                changed = true;
                history_dirty = true;
            }
        }
        {
            let syncs: Vec<CliTurnSyncUpdate> = self.async_cli_sync_inbox.lock().drain(..).collect();
            for sync in syncs {
                self.apply_cli_turn_sync_update(sync, &active_id);
                changed = true;
                history_dirty = true;
            }
        }
        if history_dirty {
            self.save_history();
        }
        changed
    }

    fn apply_cli_turn_sync_update(&mut self, sync: CliTurnSyncUpdate, active_id: &str) {
        let target_is_active = sync.session_id == active_id;
        let ckpt_id = if !sync.checkpoint_backups.is_empty() {
            let id = format!("ckpt-{}", &Uuid::new_v4().to_string()[..8]);
            let ckpt = WorkspaceCheckpoint {
                id: id.clone(),
                label: format!("CLI turn {} ({} files)", sync.user_msg_idx + 1, sync.checkpoint_backups.len()),
                file_backups: sync.checkpoint_backups.clone(),
            };
            if target_is_active {
                self.checkpoints.push(ckpt);
            } else if let Some(sess) = self.sessions.iter_mut().find(|s| s.id == sync.session_id) {
                sess.checkpoints.push(ckpt);
            }
            Some(id)
        } else {
            None
        };

        if let Some(sess) = self.sessions.iter_mut().find(|s| s.id == sync.session_id) {
            if let Some(sid) = sync.cli_session_id {
                sess.cli_session_id = Some(sid);
            }
            if let Some(cid) = ckpt_id {
                if let Some(anchor) = sess
                    .turn_anchors
                    .iter_mut()
                    .find(|a| a.user_msg_idx == sync.user_msg_idx)
                {
                    anchor.checkpoint_id = Some(cid);
                } else {
                    sess.turn_anchors.push(TurnSyncAnchor {
                        user_msg_idx: sync.user_msg_idx,
                        pre_turn_cli_session_id: None,
                        checkpoint_id: Some(cid),
                    });
                }
            }
            for (path, baseline) in sync.checkpoint_backups {
                let (added, removed) = sync
                    .file_diff_stats
                    .get(&path)
                    .copied()
                    .unwrap_or((0, 0));
                if let Some(pos) = sess
                    .changed_files
                    .iter()
                    .position(|f| paths_same_file(&f.path, &path))
                {
                    let after = fs::read_to_string(&path).unwrap_or_default();
                    let (new_add, new_del) = compute_file_diff_counts(
                        sess.changed_files[pos].original_content.as_deref().unwrap_or(""),
                        &after,
                    );
                    if new_add == 0 && new_del == 0 {
                        sess.changed_files.remove(pos);
                    } else {
                        sess.changed_files[pos].added_lines = new_add;
                        sess.changed_files[pos].removed_lines = new_del;
                    }
                } else if added > 0 || removed > 0 {
                    sess.changed_files.push(SessionFileChange {
                        path,
                        added_lines: added,
                        removed_lines: removed,
                        original_content: baseline,
                        first_turn_msg_idx: sync.user_msg_idx,
                    });
                }
            }
        }
    }

    pub fn active_session_changed_files(&self) -> &[SessionFileChange] {
        &self.active_session().changed_files
    }

    pub fn count_rewind_impact(&self, user_msg_idx: usize) -> (usize, Vec<PathBuf>) {
        let dropped = self.messages.len().saturating_sub(user_msg_idx + 1);
        let mut affected: Vec<PathBuf> = Vec::new();
        for ch in &self.active_session().changed_files {
            if ch.first_turn_msg_idx >= user_msg_idx && !affected.contains(&ch.path) {
                affected.push(ch.path.clone());
            }
        }
        for msg in self.messages.iter().skip(user_msg_idx) {
            for p in &msg.modified_files {
                if !affected.contains(p) {
                    affected.push(p.clone());
                }
            }
        }
        let target_ckpts: Vec<&str> = self
            .active_session()
            .turn_anchors
            .iter()
            .filter(|a| a.user_msg_idx >= user_msg_idx)
            .filter_map(|a| a.checkpoint_id.as_deref())
            .collect();
        for ckpt in &self.checkpoints {
            if target_ckpts.contains(&ckpt.id.as_str()) {
                for p in ckpt.file_backups.keys() {
                    if !affected.contains(p) {
                        affected.push(p.clone());
                    }
                }
            }
        }
        (dropped, affected)
    }

    fn restore_file_backup(path: &Path, backup: Option<&str>) {
        match backup {
            Some(content) => {
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::write(path, content);
            }
            None => {
                let _ = fs::remove_file(path);
            }
        }
    }

    pub fn revert_session_changed_file(&mut self, path: &Path) -> bool {
        let sess = self.active_session_mut();
        let Some(pos) = sess.changed_files.iter().position(|f| f.path == path) else {
            return false;
        };
        let entry = sess.changed_files.remove(pos);
        Self::restore_file_backup(&entry.path, entry.original_content.as_deref());
        self.save_history();
        true
    }

    pub fn revert_all_session_changed_files(&mut self) -> Vec<PathBuf> {
        let entries: Vec<SessionFileChange> = self
            .active_session_mut()
            .changed_files
            .drain(..)
            .collect();
        let mut restored = Vec::with_capacity(entries.len());
        for entry in entries {
            Self::restore_file_backup(&entry.path, entry.original_content.as_deref());
            restored.push(entry.path);
        }
        self.save_history();
        restored
    }

    pub fn keep_all_session_changed_files(&mut self) {
        self.active_session_mut().changed_files.clear();
        self.save_history();
    }

    pub fn edit_message_and_rewind(
        &mut self,
        user_msg_idx: usize,
        new_prompt: String,
        revert_files: bool,
        mode: AgentWorkbenchMode,
        ctx: Option<&IdeCodeContext>,
    ) -> Vec<PathBuf> {
        if user_msg_idx >= self.messages.len() || self.is_active_session_busy() {
            return Vec::new();
        }
        let mut restored_paths: Vec<PathBuf> = Vec::new();

        let (pre_turn_sid, target_ckpt_ids): (Option<String>, Vec<String>) = {
            let sess = self.active_session();
            let sid = sess
                .turn_anchors
                .iter()
                .find(|a| a.user_msg_idx == user_msg_idx)
                .and_then(|a| a.pre_turn_cli_session_id.clone());
            let ckpts = sess
                .turn_anchors
                .iter()
                .filter(|a| a.user_msg_idx >= user_msg_idx)
                .filter_map(|a| a.checkpoint_id.clone())
                .collect();
            (sid, ckpts)
        };

        if revert_files {
            // 按逆序从新到旧回放受影响轮次的快照，使最早状态生效
            let mut remaining = Vec::with_capacity(self.checkpoints.len());
            let mut to_restore = Vec::new();
            for ckpt in self.checkpoints.drain(..) {
                if target_ckpt_ids.contains(&ckpt.id) {
                    to_restore.push(ckpt);
                } else {
                    remaining.push(ckpt);
                }
            }
            self.checkpoints = remaining;
            for ckpt in to_restore.into_iter().rev() {
                for (path, backup) in ckpt.file_backups {
                    Self::restore_file_backup(&path, backup.as_deref());
                    if !restored_paths.contains(&path) {
                        restored_paths.push(path);
                    }
                }
            }
            let sess = self.active_session_mut();
            let mut kept_changes = Vec::new();
            for mut ch in sess.changed_files.drain(..) {
                if ch.first_turn_msg_idx >= user_msg_idx {
                    if !restored_paths.contains(&ch.path) {
                        Self::restore_file_backup(&ch.path, ch.original_content.as_deref());
                        restored_paths.push(ch.path);
                    }
                } else {
                    let after = fs::read_to_string(&ch.path).unwrap_or_default();
                    let (a, d) = compute_file_diff_counts(
                        ch.original_content.as_deref().unwrap_or(""),
                        &after,
                    );
                    ch.added_lines = a;
                    ch.removed_lines = d;
                    if a > 0 || d > 0 {
                        kept_changes.push(ch);
                    }
                }
            }
            sess.changed_files = kept_changes;
        }

        // 截断后续对话与锚点，并与 CLI 客户端会话同步
        self.messages.truncate(user_msg_idx);
        let effective = self.effective_provider_kind();
        let sess = self.active_session_mut();
        sess.turn_anchors.retain(|a| a.user_msg_idx < user_msg_idx);
        if effective == AgentProviderKind::ClaudeCodeCli {
            sess.cli_session_id = pre_turn_sid.clone();
            sess.pending_fork_session = pre_turn_sid.is_some();
        } else {
            // Cursor CLI 等无独立 --fork 参数的客户端：开启与回退后消息栈一致的同步新会话
            sess.cli_session_id = None;
            sess.pending_fork_session = false;
        }

        self.submit_workbench_prompt(&new_prompt, mode, ctx);
        restored_paths
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
        self.messages.clear();
        let active_id = self.active_session().id.clone();
        if let Some(slot) = self.session_slots.lock().get(&active_id).cloned() {
            *slot.streaming_text.lock() = None;
        }
        self.save_history();
    }

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
        let backend = self.resolve_execution_backend(AgentWorkbenchMode::Ask);
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
            s.error =
                Some("请输入具体修改指令后按 Enter 提交至本地 CLI 模型生成 Diff".to_string());
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
            s.error = Some(
                "未检测到本地 AI CLI：请确认本机已安装 cursor-agent / agy / claude / codex CLI 并点击右上角 ↻ 刷新。"
                    .to_string(),
            );
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
                ExecutionBackend::Cli(params) => execute_cli_once(
                    &params,
                    &format!("{sys_prompt}\n\n{user_msg}"),
                    &dummy_stream,
                    &dummy_pid,
                    None,
                    None,
                )
                .map(|o| o.reply),
                ExecutionBackend::Acp(acp_cmd) => execute_acp_stdio_session(
                    &acp_cmd,
                    &root,
                    &format!("{sys_prompt}\n\n{user_msg}"),
                    Some(&meta.file_path),
                    &dummy_stream,
                    &dummy_pid,
                    None,
                    None,
                )
                .map(|(txt, _)| txt),
                ExecutionBackend::HttpApi(cfg) => execute_curl_llm_streaming(
                    &cfg,
                    sys_prompt,
                    &user_msg,
                    &dummy_stream,
                    &cancel,
                    &dummy_pid,
                    None,
                ),
                ExecutionBackend::LocalOffline => Err("No local CLI agent detected".to_string()),
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

    pub fn request_ai_commit_message(&mut self, diff_text: String, changed_files: Vec<String>) {
        let backend = self.resolve_execution_backend(AgentWorkbenchMode::Ask);
        if matches!(backend, ExecutionBackend::LocalOffline) {
            *self.async_commit_msg.lock() = Some(Err(
                "未检测到可用的本地 AI CLI（cursor-agent / agy / claude / codex），请点击右上角 ↻ 刷新 CLI 状态"
                    .to_string(),
            ));
            if let Some(cb) = &self.wakeup {
                cb();
            }
            return;
        }

        let slot = self.async_commit_msg.clone();
        let wakeup = self.wakeup.clone();
        let root = self.workspace_root.clone();
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
            let res = match backend {
                ExecutionBackend::Cli(params) => execute_cli_once(
                    &params,
                    &format!("{sys}\n\n{user}"),
                    &dummy_stream,
                    &dummy_pid,
                    None,
                    None,
                )
                .map(|o| o.reply),
                ExecutionBackend::Acp(acp_cmd) => execute_acp_stdio_session(
                    &acp_cmd,
                    &root,
                    &format!("{sys}\n\n{user}"),
                    None,
                    &dummy_stream,
                    &dummy_pid,
                    None,
                    None,
                )
                .map(|(txt, _)| txt),
                ExecutionBackend::HttpApi(cfg) => execute_curl_llm_streaming(
                    &cfg,
                    sys,
                    &user,
                    &dummy_stream,
                    &cancel,
                    &dummy_pid,
                    None,
                ),
                ExecutionBackend::LocalOffline => Err("No local CLI available".to_string()),
            }
            .map(|s| s.trim().trim_matches('`').to_string());
            *slot.lock() = Some(res);
            if let Some(cb) = wakeup {
                cb();
            }
        });
    }

    fn resolve_execution_backend(&self, mode: AgentWorkbenchMode) -> ExecutionBackend {
        let target = self.effective_provider_kind();
        let active_sess = self.active_session();
        let make_params = |kind: AgentProviderKind| -> Option<CliRunParams> {
            let reg = self.providers.iter().find(|p| p.kind == kind)?;
            let bin = reg.binary_path.clone()?;
            let use_resume = kind == target && reg.supports_session_resume;
            Some(CliRunParams {
                kind,
                bin,
                root: self.workspace_root.clone(),
                model: reg.active_model.clone(),
                exact_catalog: reg.exact_model_catalog.clone(),
                fast_mode: reg.fast_mode,
                effort: reg.reasoning_effort,
                mode,
                resume_session_id: if use_resume {
                    active_sess.cli_session_id.clone()
                } else {
                    None
                },
                fork_session: use_resume && active_sess.pending_fork_session,
            })
        };

        if let Ok(acp_cmd) = std::env::var("ACP_AGENT_CMD") {
            if !acp_cmd.trim().is_empty()
                && self.active_provider == AgentProviderKind::GoogleAntigravityAcp
            {
                return ExecutionBackend::Acp(acp_cmd);
            }
        }

        if let Some(params) = make_params(target) {
            return ExecutionBackend::Cli(params);
        }

        for fallback_kind in AgentProviderKind::all() {
            if let Some(params) = make_params(*fallback_kind) {
                return ExecutionBackend::Cli(params);
            }
        }

        if self.api_config.is_configured() {
            return ExecutionBackend::HttpApi(self.api_config.clone());
        }
        ExecutionBackend::LocalOffline
    }

    pub fn submit_workbench_prompt(
        &mut self,
        user_prompt: &str,
        mode: AgentWorkbenchMode,
        ctx: Option<&IdeCodeContext>,
    ) {
        let mode = self.normalize_mode_for_active(mode);
        let trimmed_raw = user_prompt.trim();
        let (attached_items, pasted_snippets) = {
            let s = self.active_session_mut();
            (
                std::mem::take(&mut s.attached_items),
                std::mem::take(&mut s.pasted_snippets),
            )
        };

        if trimmed_raw.is_empty() && attached_items.is_empty() && pasted_snippets.is_empty() {
            return;
        }

        let effective_prompt = if trimmed_raw.is_empty() {
            if !attached_items.is_empty() && pasted_snippets.is_empty() {
                "请结合附加的文件/图片内容进行分析或修改".to_string()
            } else {
                "请分析并处理附加的代码片段".to_string()
            }
        } else {
            trimmed_raw.to_string()
        };

        // 首条用户提问自动提炼为会话标题
        {
            let s = self.active_session_mut();
            if !s.custom_title {
                let raw_title = if !trimmed_raw.is_empty() {
                    trimmed_raw.lines().next().unwrap_or(trimmed_raw).trim()
                } else if let Some(first_att) = attached_items.first() {
                    first_att.display_name.as_str()
                } else if let Some(first_snip) = pasted_snippets.first() {
                    first_snip.title.as_str()
                } else {
                    "新对话"
                };
                let short: String = raw_title.chars().take(16).collect();
                let suffix = if raw_title.chars().count() > 16 { "…" } else { "" };
                if !short.is_empty() {
                    s.title = format!("{short}{suffix}");
                }
            }
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

        let mut user_display = if let Some(c) = ctx {
            format!("{}  [@{}:L{}]", effective_prompt, file_label, c.cursor_line + 1)
        } else {
            effective_prompt.clone()
        };

        for snip in &pasted_snippets {
            let lang_tag = if snip.language.is_empty() {
                "text"
            } else {
                snip.language.as_str()
            };
            user_display.push_str(&format!("\n\n```{lang_tag}\n{}\n```", snip.content.trim_end()));
        }

        let user_msg_idx = self.messages.len();
        self.messages.push(AgentChatMessage {
            role: "user".to_string(),
            agent: self.active_provider,
            content: user_display,
            thought: None,
            checkpoint_id: None,
            patch: None,
            modified_files: Vec::new(),
            file_changes: Vec::new(),
            attachments: attached_items.clone(),
        });
        {
            let sess = self.active_session_mut();
            let pre_sid = sess.cli_session_id.clone();
            sess.turn_anchors.retain(|a| a.user_msg_idx != user_msg_idx);
            sess.turn_anchors.push(TurnSyncAnchor {
                user_msg_idx,
                pre_turn_cli_session_id: pre_sid,
                checkpoint_id: None,
            });
        }
        self.save_history();

        let trimmed = effective_prompt.as_str();
        if trimmed.starts_with("/check")
            || trimmed.eq_ignore_ascii_case("cargo check")
            || trimmed == "编译检查"
        {
            self.dispatch_cargo_check_async();
            return;
        }

        if let Some(query) = trimmed.strip_prefix("/search ") {
            let hits = find_in_files(&self.workspace_root, query.trim(), 12).unwrap_or_default();
            let mut hit_files = Vec::new();
            let summary = if hits.is_empty() {
                format!("未在工作区检索到 `{}` 的匹配项。", query.trim())
            } else {
                let mut lines =
                    vec![format!("工作区检索 `{}` 命中 {} 处：", query.trim(), hits.len())];
                for h in hits.iter().take(8) {
                    if !hit_files.contains(&h.path) {
                        hit_files.push(h.path.clone());
                    }
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
                modified_files: hit_files,
                file_changes: Vec::new(),
                attachments: Vec::new(),
            });
            return;
        }

        let target_session_id = self.active_session().id.clone();
        match self.resolve_execution_backend(mode) {
            ExecutionBackend::Cli(params) => {
                self.active_session_mut().pending_fork_session = false;
                self.dispatch_cli_async(
                    params,
                    target_session_id,
                    user_msg_idx,
                    effective_prompt,
                    mode,
                    ctx.cloned(),
                    attached_items,
                    pasted_snippets,
                );
            }
            ExecutionBackend::Acp(acp_cmd) => {
                self.dispatch_acp_async(
                    acp_cmd,
                    target_session_id,
                    user_msg_idx,
                    effective_prompt,
                    mode,
                    ctx.cloned(),
                    attached_items,
                    pasted_snippets,
                );
            }
            ExecutionBackend::HttpApi(cfg) => {
                if mode == AgentWorkbenchMode::Agent {
                    self.dispatch_http_agent_loop_async(
                        cfg,
                        target_session_id,
                        user_msg_idx,
                        effective_prompt,
                        ctx.cloned(),
                        attached_items,
                        pasted_snippets,
                    );
                } else {
                    self.dispatch_http_api_async(
                        cfg,
                        target_session_id,
                        effective_prompt,
                        mode,
                        ctx.cloned(),
                        attached_items,
                        pasted_snippets,
                    );
                }
            }
            ExecutionBackend::LocalOffline => {
                self.messages.push(AgentChatMessage {
                    role: "assistant".to_string(),
                    agent: self.active_provider,
                    content: "⚠ 未检测到本机 AI CLI。\n请确认已安装 `cursor-agent` (Cursor CLI)、`agy` (Antigravity CLI)、`claude` 或 `codex` CLI，然后点击右上角 `↻` 刷新即可自动识别模型列表。".to_string(),
                    thought: Some("No local CLI detected.".to_string()),
                    checkpoint_id: None,
                    patch: None,
                    modified_files: Vec::new(),
                    file_changes: Vec::new(),
                    attachments: Vec::new(),
                });
            }
        }
    }

    fn build_system_and_user_prompt(
        &self,
        prompt: &str,
        mode: AgentWorkbenchMode,
        ctx: Option<&IdeCodeContext>,
        attachments: &[AgentAttachedItem],
        pasted_snippets: &[PastedCodeSnippet],
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
                " You are operating in Autonomous Agent mode in workspace root. Inspect files, verify code, edit files directly when requested, and check builds before summarizing what you changed.",
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
            let block_clipped: String = block.trim_end().chars().take(6000).collect();
            user_msg.push_str(&format!(
                "```{}\n{}\n```\n\n",
                c.language.lsp_language_id(),
                block_clipped
            ));
        } else {
            user_msg.push_str(&format!(
                "## Workspace Root\n{}\n\n",
                self.workspace_root.display()
            ));
        }

        if !attachments.is_empty() {
            user_msg.push_str("## User Attached Files & Images (Dragged / Pasted)\n");
            for item in attachments {
                if item.is_image {
                    user_msg.push_str(&format!(
                        "- [Image] `{}` ({}x{}, full path: `{}`) — please inspect this image file if supported by your CLI/vision tools.\n",
                        item.display_name,
                        item.width,
                        item.height,
                        item.path.display()
                    ));
                } else if item.is_dir {
                    user_msg.push_str(&format!(
                        "- [Directory] `{}` (path: `{}`)\n",
                        item.display_name,
                        item.path.display()
                    ));
                } else {
                    user_msg.push_str(&format!(
                        "- [File] `{}` (path: `{}`)\n",
                        item.display_name,
                        item.path.display()
                    ));
                    if let Ok(content) = fs::read_to_string(&item.path) {
                        let clipped: String = content.chars().take(3600).collect();
                        let lang = LanguageId::from_path(&item.path).lsp_language_id();
                        user_msg.push_str(&format!("```{lang}\n{clipped}\n```\n"));
                    }
                }
            }
            user_msg.push('\n');
        }

        if !pasted_snippets.is_empty() {
            user_msg.push_str("## User Pasted Code Snippets\n");
            for snip in pasted_snippets {
                user_msg.push_str(&format!(
                    "### {} ({} lines)\n```{}\n{}\n```\n",
                    snip.title, snip.line_count, snip.language, snip.content
                ));
            }
            user_msg.push('\n');
        }

        user_msg.push_str("## User Request\n");
        user_msg.push_str(prompt);
        (system, user_msg)
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_http_api_async(
        &mut self,
        cfg: AiApiConfig,
        target_session_id: String,
        prompt: String,
        mode: AgentWorkbenchMode,
        ctx: Option<IdeCodeContext>,
        attachments: Vec<AgentAttachedItem>,
        pasted_snippets: Vec<PastedCodeSnippet>,
    ) {
        let Some(slot) = self.try_begin_session_execution(
            &target_session_id,
            StreamingAgentProgress::with_phase(format!(
                "正在连接 {} ({})...",
                cfg.model, cfg.base_url
            )),
        ) else {
            return;
        };

        let (sys_prompt, user_prompt) = self.build_system_and_user_prompt(
            &prompt,
            mode,
            ctx.as_ref(),
            &attachments,
            &pasted_snippets,
        );
        let inbox = self.async_inbox.clone();
        let slots = self.session_slots.clone();
        let global_busy = self.is_busy.clone();
        let cancel = slot.cancel_requested.clone();
        let stream_buf = slot.streaming_text.clone();
        let wakeup = self.wakeup.clone();
        let provider = self.active_provider;
        let active_pid = slot.active_child_pid.clone();

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

            if !cancel.load(Ordering::SeqCst) {
                let (content, patch) = match reply_res {
                    Ok(text) => {
                        let p = maybe_extract_patch_from_reply(&text, mode, ctx.as_ref());
                        (text, p)
                    }
                    Err(err) => (format!("✕ 请求失败：\n{err}"), None),
                };
                inbox.lock().push((
                    target_session_id.clone(),
                    AgentChatMessage {
                        role: "assistant".to_string(),
                        agent: provider,
                        content,
                        thought: Some(format!("Completed {} call.", cfg.model)),
                        checkpoint_id: None,
                        patch,
                        modified_files: Vec::new(),
                        file_changes: Vec::new(),
                        attachments: Vec::new(),
                    },
                ));
            }

            Self::finish_session_slot(&slots, &global_busy, &target_session_id);
            if let Some(cb) = wakeup {
                cb();
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_http_agent_loop_async(
        &mut self,
        cfg: AiApiConfig,
        target_session_id: String,
        user_msg_idx: usize,
        prompt: String,
        ctx: Option<IdeCodeContext>,
        attachments: Vec<AgentAttachedItem>,
        pasted_snippets: Vec<PastedCodeSnippet>,
    ) {
        let Some(slot) = self.try_begin_session_execution(
            &target_session_id,
            StreamingAgentProgress::with_phase(format!(
                "🤖 Agent ({}) 正在规划工具调用...",
                cfg.model
            )),
        ) else {
            return;
        };

        let (sys_prompt, user_prompt) = self.build_system_and_user_prompt(
            &prompt,
            AgentWorkbenchMode::Agent,
            ctx.as_ref(),
            &attachments,
            &pasted_snippets,
        );
        let root = self.workspace_root.clone();
        let inbox = self.async_inbox.clone();
        let sync_inbox = self.async_cli_sync_inbox.clone();
        let ck_inbox = self.async_checkpoints_inbox.clone();
        let mod_inbox = self.modified_files_inbox.clone();
        let slots = self.session_slots.clone();
        let global_busy = self.is_busy.clone();
        let cancel = slot.cancel_requested.clone();
        let stream_buf = slot.streaming_text.clone();
        let wakeup = self.wakeup.clone();
        let provider = self.active_provider;
        let active_pid = slot.active_child_pid.clone();

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

            if !cancel.load(Ordering::SeqCst) {
                let mut touched_files: Vec<PathBuf> = Vec::new();
                let mut file_changes = Vec::new();
                let mut file_diff_stats = HashMap::new();
                for (path, backup) in &ckpt_backups {
                    let after = fs::read_to_string(path).unwrap_or_default();
                    let (added, removed) =
                        compute_file_diff_counts(backup.as_deref().unwrap_or(""), &after);
                    if added == 0 && removed == 0 && backup.is_some() {
                        continue;
                    }
                    touched_files.push(path.clone());
                    file_diff_stats.insert(path.clone(), (added, removed));
                    file_changes.push(TurnFileChange {
                        path: path.clone(),
                        added_lines: added,
                        removed_lines: removed,
                        before_content: backup.clone(),
                    });
                }
                if !ckpt_backups.is_empty() {
                    sync_inbox.lock().push(CliTurnSyncUpdate {
                        session_id: target_session_id.clone(),
                        user_msg_idx,
                        cli_session_id: None,
                        checkpoint_backups: ckpt_backups.clone(),
                        file_diff_stats,
                    });
                }
                let ckpt_id = if !ckpt_backups.is_empty() {
                    let id = format!("ckpt-{}", &Uuid::new_v4().to_string()[..8]);
                    ck_inbox.lock().push((
                        target_session_id.clone(),
                        WorkspaceCheckpoint {
                            id: id.clone(),
                            label: format!("Agent edits ({} files)", ckpt_backups.len()),
                            file_backups: ckpt_backups,
                        },
                    ));
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
                inbox.lock().push((
                    target_session_id.clone(),
                    AgentChatMessage {
                        role: "assistant".to_string(),
                        agent: provider,
                        content,
                        thought: Some(format!("Completed Agent tool loop via {}.", cfg.model)),
                        checkpoint_id: ckpt_id,
                        patch,
                        modified_files: touched_files,
                        file_changes,
                        attachments: Vec::new(),
                    },
                ));
            }

            Self::finish_session_slot(&slots, &global_busy, &target_session_id);
            if let Some(cb) = wakeup {
                cb();
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_acp_async(
        &mut self,
        acp_cmd: String,
        target_session_id: String,
        user_msg_idx: usize,
        prompt: String,
        mode: AgentWorkbenchMode,
        ctx: Option<IdeCodeContext>,
        attachments: Vec<AgentAttachedItem>,
        pasted_snippets: Vec<PastedCodeSnippet>,
    ) {
        let Some(slot) = self.try_begin_session_execution(
            &target_session_id,
            StreamingAgentProgress::with_phase(format!("正在通过 ACP ({acp_cmd}) 建立会话...")),
        ) else {
            return;
        };

        let (sys_prompt, user_prompt) = self.build_system_and_user_prompt(
            &prompt,
            mode,
            ctx.as_ref(),
            &attachments,
            &pasted_snippets,
        );
        let combined = format!("{sys_prompt}\n\n{user_prompt}");
        let diff_tracker = TurnDiffTracker::capture(&self.workspace_root, mode);
        let root = self.workspace_root.clone();
        let inbox = self.async_inbox.clone();
        let sync_inbox = self.async_cli_sync_inbox.clone();
        let mod_inbox = self.modified_files_inbox.clone();
        let slots = self.session_slots.clone();
        let global_busy = self.is_busy.clone();
        let cancel = slot.cancel_requested.clone();
        let stream_buf = slot.streaming_text.clone();
        let wakeup = self.wakeup.clone();
        let active_pid = slot.active_child_pid.clone();
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

            if !cancel.load(Ordering::SeqCst) {
                let (content, patch, extra_files) = match res {
                    Ok((reply, files)) => {
                        let p = maybe_extract_patch_from_reply(&reply, mode, ctx.as_ref());
                        (reply, p, files)
                    }
                    Err(e) => (format!("✕ ACP 会话失败：\n{e}"), None, Vec::new()),
                };
                let (modified_files, checkpoint_backups, file_diff_stats) =
                    diff_tracker.finish(&extra_files);
                let file_changes = build_turn_file_changes(
                    &modified_files,
                    &checkpoint_backups,
                    &file_diff_stats,
                );
                if !checkpoint_backups.is_empty() {
                    sync_inbox.lock().push(CliTurnSyncUpdate {
                        session_id: target_session_id.clone(),
                        user_msg_idx,
                        cli_session_id: None,
                        checkpoint_backups,
                        file_diff_stats,
                    });
                }
                inbox.lock().push((
                    target_session_id.clone(),
                    AgentChatMessage {
                        role: "assistant".to_string(),
                        agent: provider,
                        content,
                        thought: Some(format!("Completed ACP session via {acp_cmd}.")),
                        checkpoint_id: None,
                        patch,
                        modified_files,
                        file_changes,
                        attachments: Vec::new(),
                    },
                ));
            }
            Self::finish_session_slot(&slots, &global_busy, &target_session_id);
            if let Some(cb) = wakeup {
                cb();
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_cli_async(
        &mut self,
        params: CliRunParams,
        target_session_id: String,
        user_msg_idx: usize,
        prompt: String,
        mode: AgentWorkbenchMode,
        ctx: Option<IdeCodeContext>,
        attachments: Vec<AgentAttachedItem>,
        pasted_snippets: Vec<PastedCodeSnippet>,
    ) {
        let cli_label = params
            .bin
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cli")
            .to_string();
        let resolved_badge = self.active_resolved_cli_model();
        let Some(slot) = self.try_begin_session_execution(
            &target_session_id,
            StreamingAgentProgress::with_phase(format!(
                "正在通过 `{cli_label}` [{resolved_badge}] 实时处理中..."
            )),
        ) else {
            return;
        };

        let (sys_prompt, user_prompt) = self.build_system_and_user_prompt(
            &prompt,
            mode,
            ctx.as_ref(),
            &attachments,
            &pasted_snippets,
        );
        let combined_prompt = format!("{sys_prompt}\n\n{user_prompt}");

        let inbox = self.async_inbox.clone();
        let sync_inbox = self.async_cli_sync_inbox.clone();
        let mod_inbox = self.modified_files_inbox.clone();
        let slots = self.session_slots.clone();
        let global_busy = self.is_busy.clone();
        let cancel = slot.cancel_requested.clone();
        let stream_buf = slot.streaming_text.clone();
        let wakeup = self.wakeup.clone();
        let active_pid = slot.active_child_pid.clone();
        let display_provider = self.active_provider;

        if let Some(cb) = &wakeup {
            cb();
        }

        thread::spawn(move || {
            let run_res = execute_cli_once(
                &params,
                &combined_prompt,
                &stream_buf,
                &active_pid,
                Some(&mod_inbox),
                wakeup.as_ref(),
            );

            if !cancel.load(Ordering::SeqCst) {
                let (content, thought, patch, modified_files, file_changes) = match run_res {
                    Ok(out) => {
                        let p = maybe_extract_patch_from_reply(&out.reply, mode, ctx.as_ref());
                        let fc = build_turn_file_changes(
                            &out.modified_files,
                            &out.checkpoint_backups,
                            &out.file_diff_stats,
                        );
                        sync_inbox.lock().push(CliTurnSyncUpdate {
                            session_id: target_session_id.clone(),
                            user_msg_idx,
                            cli_session_id: out.cli_session_id,
                            checkpoint_backups: out.checkpoint_backups,
                            file_diff_stats: out.file_diff_stats,
                        });
                        (
                            out.reply,
                            out.thought.or_else(|| {
                                Some(format!("Completed via {} ({})", cli_label, out.resolved_model))
                            }),
                            p,
                            out.modified_files,
                            fc,
                        )
                    }
                    Err(err_msg) => (
                        format!(
                            "✕ `{cli_label}` ({resolved_badge}) 调用异常：\n{err_msg}\n\n提示：可点击下方【模型】或【思考深度】调整参数后重试，或点击顶部切换其他本地 CLI。"
                        ),
                        None,
                        None,
                        Vec::new(),
                        Vec::new(),
                    ),
                };

                inbox.lock().push((
                    target_session_id.clone(),
                    AgentChatMessage {
                        role: "assistant".to_string(),
                        agent: display_provider,
                        content,
                        thought,
                        checkpoint_id: None,
                        patch,
                        modified_files,
                        file_changes,
                        attachments: Vec::new(),
                    },
                ));
            }

            Self::finish_session_slot(&slots, &global_busy, &target_session_id);
            if let Some(cb) = wakeup {
                cb();
            }
        });
    }

    fn dispatch_cargo_check_async(&mut self) {
        let target_session_id = self.active_session().id.clone();
        let Some(_slot) = self.try_begin_session_execution(
            &target_session_id,
            StreamingAgentProgress::with_phase(
                "正在后台执行 `cargo check --workspace --offline`...",
            ),
        ) else {
            return;
        };
        let root = self.workspace_root.clone();
        let inbox = self.async_inbox.clone();
        let slots = self.session_slots.clone();
        let global_busy = self.is_busy.clone();
        let wakeup = self.wakeup.clone();
        let provider = self.active_provider;

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
            inbox.lock().push((
                target_session_id.clone(),
                AgentChatMessage {
                    role: "assistant".to_string(),
                    agent: provider,
                    content: text,
                    thought: Some("Completed background cargo check.".to_string()),
                    checkpoint_id: None,
                    patch: None,
                    modified_files: Vec::new(),
                    file_changes: Vec::new(),
                    attachments: Vec::new(),
                },
            ));
            Self::finish_session_slot(&slots, &global_busy, &target_session_id);
            if let Some(cb) = wakeup {
                cb();
            }
        });
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::execute_workspace_tool;
    use serde_json::json;

    #[test]
    fn test_acp_protocol_and_workspace_tools() {
        let init = crate::acp::AcpProtocol::build_initialize_request(1);
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

    #[test]
    fn test_cursor_and_agy_model_resolution_and_hub_toggles() {
        let catalog = vec![
            "auto".to_string(),
            "composer-2.5".to_string(),
            "composer-2.5-fast".to_string(),
            "claude-opus-5-5-high".to_string(),
            "claude-opus-5-5-high-fast".to_string(),
            "gemini-4-argon-medium".to_string(),
        ];
        assert_eq!(
            resolve_cursor_model_id("composer-2.5", ReasoningEffort::High, true, &catalog),
            "composer-2.5-fast"
        );
        assert_eq!(
            resolve_cursor_model_id("claude-opus-5-5", ReasoningEffort::High, true, &catalog),
            "claude-opus-5-5-high-fast"
        );
        assert_eq!(
            resolve_cursor_model_id("gemini-4-argon", ReasoningEffort::High, true, &catalog),
            "gemini-4-argon-medium"
        );

        let agy_catalog = vec![
            "Gemini 4 Argon (Medium)".to_string(),
            "Gemini 4 Argon (High)".to_string(),
            "Gemini 4 Hydrogen (XHigh)".to_string(),
        ];
        assert_eq!(
            resolve_agy_model_and_effort("Gemini 4 Argon (Medium)", ReasoningEffort::High, &agy_catalog),
            ("Gemini 4 Argon (High)".to_string(), None)
        );

        let mut hub = AgentHub::new(Path::new("."));
        assert!(!hub.providers.is_empty());
        assert!(!hub.active_model().is_empty());
        hub.select_model_for_active("custom-local-cli-model");
        assert_eq!(hub.active_model(), "custom-local-cli-model");
    }

    #[test]
    fn test_multi_session_lifecycle_and_attachments() {
        let mut hub = AgentHub::new(Path::new("."));
        assert_eq!(hub.sessions.len(), 1);
        let first_msg_cnt = hub.messages.len();

        let new_idx = hub.new_session();
        assert_eq!(new_idx, 1);
        assert_eq!(hub.sessions.len(), 2);
        assert_eq!(hub.messages.len(), 0);

        hub.attach_item_to_active_session(AgentAttachedItem {
            path: PathBuf::from("Cargo.toml"),
            display_name: "Cargo.toml".to_string(),
            is_image: false,
            is_dir: false,
            width: 0,
            height: 0,
        });
        hub.add_pasted_snippet_to_active_session(PastedCodeSnippet {
            id: "snip-1".to_string(),
            title: "粘贴代码 (rust)".to_string(),
            language: "rust".to_string(),
            content: "fn a() {}\nfn b() {}".to_string(),
            line_count: 2,
        });
        assert_eq!(hub.active_session().attached_items.len(), 1);
        assert_eq!(hub.active_session().pasted_snippets.len(), 1);

        // 切回会话 0 仍保留原消息
        assert!(hub.switch_session(0));
        assert_eq!(hub.messages.len(), first_msg_cnt);

        // 切回会话 1 附件和代码片段保留
        assert!(hub.switch_session(1));
        assert_eq!(hub.active_session().attached_items.len(), 1);
        assert_eq!(hub.active_session().pasted_snippets.len(), 1);

        // 在第 2 个会话提交内置 /search 指令（不依赖外部网络/CLI）
        hub.submit_workbench_prompt("/search ide_agent_hub", AgentWorkbenchMode::Ask, None);
        assert!(hub.messages.len() >= 1);
        assert!(hub.messages[0].content.contains("```rust"));
        assert_eq!(hub.messages[0].attachments.len(), 1);
        assert!(hub.active_session().attached_items.is_empty());
        assert!(hub.active_session().pasted_snippets.is_empty());

        // 关闭当前会话后回到会话 0
        assert!(hub.close_session(1));
        assert_eq!(hub.sessions.len(), 1);
        assert_eq!(hub.messages.len(), first_msg_cnt);
    }

    #[test]
    fn test_per_cli_supported_modes_and_normalization() {
        let mut hub = AgentHub::new(Path::new("."));

        hub.active_provider = AgentProviderKind::OpenAiCodexCli;
        assert_eq!(
            hub.supported_modes_for_active(),
            &[AgentWorkbenchMode::Agent, AgentWorkbenchMode::Ask]
        );
        assert!(!hub.supports_mode_for_active(AgentWorkbenchMode::Edit));
        assert_eq!(
            hub.normalize_mode_for_active(AgentWorkbenchMode::Edit),
            AgentWorkbenchMode::Agent
        );
        assert_eq!(
            hub.next_mode_for_active(AgentWorkbenchMode::Agent),
            AgentWorkbenchMode::Ask
        );
        assert_eq!(
            hub.next_mode_for_active(AgentWorkbenchMode::Ask),
            AgentWorkbenchMode::Agent
        );

        hub.active_provider = AgentProviderKind::CursorCli;
        assert_eq!(
            hub.supported_modes_for_active(),
            &[AgentWorkbenchMode::Agent, AgentWorkbenchMode::Ask]
        );
        assert_eq!(
            hub.mode_cli_flag_for_active(AgentWorkbenchMode::Ask),
            "--mode ask"
        );

        hub.active_provider = AgentProviderKind::GoogleAntigravityAcp;
        assert_eq!(
            hub.supported_modes_for_active(),
            &[
                AgentWorkbenchMode::Agent,
                AgentWorkbenchMode::Edit,
                AgentWorkbenchMode::Ask,
            ]
        );
        assert!(hub.supports_mode_for_active(AgentWorkbenchMode::Edit));
        assert_eq!(
            hub.normalize_mode_for_active(AgentWorkbenchMode::Edit),
            AgentWorkbenchMode::Edit
        );
        assert_eq!(
            hub.mode_cli_flag_for_active(AgentWorkbenchMode::Edit),
            "--mode accept-edits"
        );

        hub.active_provider = AgentProviderKind::ClaudeCodeCli;
        assert!(hub.supports_reasoning_effort());
        assert_eq!(
            hub.supported_modes_for_active(),
            &[
                AgentWorkbenchMode::Agent,
                AgentWorkbenchMode::Edit,
                AgentWorkbenchMode::Ask,
            ]
        );
        assert_eq!(
            hub.mode_cli_flag_for_active(AgentWorkbenchMode::Edit),
            "--permission-mode acceptEdits"
        );
    }

    #[test]
    fn test_streaming_progress_preserves_thought_and_execution_steps() {
        let root = Path::new(".");
        let reply_acc = Arc::new(Mutex::new(String::new()));
        let thought_acc = Arc::new(Mutex::new(String::new()));
        let err_slot = Arc::new(Mutex::new(None::<String>));
        let touched = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
        let sbuf = Arc::new(Mutex::new(Some(StreamingAgentProgress::with_phase(
            "正在连接...",
        ))));

        let ev_think = serde_json::json!({
            "type": "thinking",
            "subtype": "delta",
            "text": "The user wants to inspect Cargo.toml first."
        });
        let ev_tool = serde_json::json!({
            "type": "tool_call",
            "subtype": "started",
            "tool_call": { "readToolCall": { "args": { "path": "Cargo.toml" } } }
        });
        let ev_reply = serde_json::json!({
            "type": "assistant",
            "timestamp_ms": 1,
            "message": { "content": [{ "text": "我先查看工作区配置。" }] }
        });

        for ev in [&ev_think, &ev_tool, &ev_reply] {
            crate::cli_runner::handle_stream_json_event(
                AgentProviderKind::CursorCli,
                root,
                ev,
                &reply_acc,
                &thought_acc,
                &err_slot,
                &touched,
                &sbuf,
            );
        }

        let snap = sbuf.lock().clone().expect("progress present");
        assert!(snap.thought_text.contains("inspect Cargo.toml"));
        assert!(snap
            .execution_steps
            .iter()
            .any(|s| s.contains("读取") && s.contains("Cargo.toml")));
        assert_eq!(snap.reply_text, "我先查看工作区配置。");

        // Claude stream_event 也同时保留思考与工具执行步骤
        let claude_sbuf = Arc::new(Mutex::new(None));
        let claude_reply = Arc::new(Mutex::new(String::new()));
        let claude_thought = Arc::new(Mutex::new(String::new()));
        let claude_events = [
            serde_json::json!({
                "type": "stream_event",
                "event": {
                    "type": "content_block_delta",
                    "delta": { "type": "thinking_delta", "thinking": "Checking agent_panel.rs layout." }
                }
            }),
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "name": "Grep",
                        "input": { "pattern": "BusyIndicator" }
                    }]
                }
            }),
            serde_json::json!({
                "type": "stream_event",
                "event": {
                    "type": "content_block_delta",
                    "delta": { "type": "text_delta", "text": "正在修改渲染逻辑。" }
                }
            }),
        ];
        for ev in &claude_events {
            crate::cli_runner::handle_stream_json_event(
                AgentProviderKind::ClaudeCodeCli,
                root,
                ev,
                &claude_reply,
                &claude_thought,
                &err_slot,
                &touched,
                &claude_sbuf,
            );
        }
        let claude_snap = claude_sbuf.lock().clone().expect("claude progress present");
        assert!(claude_snap.thought_text.contains("Checking agent_panel.rs"));
        assert!(claude_snap
            .execution_steps
            .iter()
            .any(|s| s.contains("搜索") && s.contains("BusyIndicator")));
        assert_eq!(claude_snap.reply_text, "正在修改渲染逻辑。");
    }

    #[test]
    fn test_cli_capability_detection_and_message_edit_rewind() {
        use crate::cli_discovery::parse_cli_rewind_capabilities;

        assert_eq!(
            parse_cli_rewind_capabilities(
                AgentProviderKind::CursorCli,
                "Usage: cursor-agent [options]\n  --resume <chatId>\n  create-chat"
            ),
            (true, true)
        );
        assert_eq!(
            parse_cli_rewind_capabilities(
                AgentProviderKind::CursorCli,
                "Usage: cursor-agent --print"
            ),
            (false, false)
        );
        assert_eq!(
            parse_cli_rewind_capabilities(
                AgentProviderKind::ClaudeCodeCli,
                "  --resume <id>\n  --fork-session"
            ),
            (true, true)
        );
        assert_eq!(
            parse_cli_rewind_capabilities(AgentProviderKind::ClaudeCodeCli, "  --resume <id>"),
            (true, false)
        );
        assert_eq!(
            parse_cli_rewind_capabilities(
                AgentProviderKind::GoogleAntigravityAcp,
                "  --conversation <id>"
            ),
            (true, false)
        );
        assert_eq!(
            parse_cli_rewind_capabilities(AgentProviderKind::OpenAiCodexCli, "resume [ID]"),
            (true, false)
        );

        let mut hub = AgentHub::new(Path::new("."));
        for reg in &mut hub.providers {
            reg.binary_path = None;
        }
        hub.api_config = AiApiConfig::default();
        hub.active_provider = AgentProviderKind::ClaudeCodeCli;
        hub.set_provider_capabilities(AgentProviderKind::ClaudeCodeCli, true, true, true);
        assert!(hub.active_cli_supports_checkpoint_rewind());

        let tmp_file = std::env::temp_dir().join(format!("rust_ai_ide_rewind_{}.txt", Uuid::new_v4()));
        fs::write(&tmp_file, "line1\nchanged_by_turn_1\n").unwrap();

        hub.messages.clear();
        hub.messages.push(AgentChatMessage {
            role: "user".to_string(),
            agent: AgentProviderKind::ClaudeCodeCli,
            content: "first prompt".to_string(),
            thought: None,
            checkpoint_id: None,
            patch: None,
            modified_files: Vec::new(),
            file_changes: Vec::new(),
            attachments: Vec::new(),
        });
        hub.messages.push(AgentChatMessage {
            role: "assistant".to_string(),
            agent: AgentProviderKind::ClaudeCodeCli,
            content: "first reply".to_string(),
            thought: None,
            checkpoint_id: None,
            patch: None,
            modified_files: Vec::new(),
            file_changes: Vec::new(),
            attachments: Vec::new(),
        });
        hub.messages.push(AgentChatMessage {
            role: "user".to_string(),
            agent: AgentProviderKind::ClaudeCodeCli,
            content: "second prompt".to_string(),
            thought: None,
            checkpoint_id: None,
            patch: None,
            modified_files: vec![tmp_file.clone()],
            file_changes: Vec::new(),
            attachments: Vec::new(),
        });
        hub.messages.push(AgentChatMessage {
            role: "assistant".to_string(),
            agent: AgentProviderKind::ClaudeCodeCli,
            content: "second reply".to_string(),
            thought: None,
            checkpoint_id: None,
            patch: None,
            modified_files: vec![tmp_file.clone()],
            file_changes: Vec::new(),
            attachments: Vec::new(),
        });

        let mut backups = HashMap::new();
        backups.insert(tmp_file.clone(), Some("line1\noriginal\n".to_string()));
        let mut stats = HashMap::new();
        stats.insert(tmp_file.clone(), (1usize, 1usize));
        let sid = hub.active_session().id.clone();
        {
            let sess = hub.active_session_mut();
            sess.cli_session_id = Some("claude-sid-turn-1".to_string());
            sess.turn_anchors.push(TurnSyncAnchor {
                user_msg_idx: 2,
                pre_turn_cli_session_id: Some("claude-sid-turn-0".to_string()),
                checkpoint_id: None,
            });
        }
        hub.apply_cli_turn_sync_update(
            CliTurnSyncUpdate {
                session_id: sid.clone(),
                user_msg_idx: 2,
                cli_session_id: Some("claude-sid-turn-2".to_string()),
                checkpoint_backups: backups,
                file_diff_stats: stats,
            },
            &sid,
        );
        assert_eq!(hub.active_session_changed_files().len(), 1);
        let (dropped, affected) = hub.count_rewind_impact(2);
        assert_eq!(dropped, 1);
        assert_eq!(affected, vec![tmp_file.clone()]);

        // 编辑消息 idx=2 并选择回退文件
        let restored = hub.edit_message_and_rewind(
            2,
            "edited second prompt".to_string(),
            true,
            AgentWorkbenchMode::Agent,
            None,
        );
        assert_eq!(restored, vec![tmp_file.clone()]);
        assert_eq!(fs::read_to_string(&tmp_file).unwrap(), "line1\noriginal\n");
        assert!(hub.active_session_changed_files().is_empty());
        // 与 Claude CLI 会话锚点同步（回退到 turn 2 前的 claude-sid-turn-0 并标记 fork_session）
        assert_eq!(
            hub.active_session().cli_session_id.as_deref(),
            Some("claude-sid-turn-0")
        );
        assert!(hub.active_session().pending_fork_session);
        assert_eq!(hub.messages[2].content, "edited second prompt");

        // Cursor CLI 在回退后清空 cli_session_id 以避免保留被回退轮次上下文
        hub.active_provider = AgentProviderKind::CursorCli;
        hub.set_provider_capabilities(AgentProviderKind::CursorCli, true, true, true);
        hub.active_session_mut().cli_session_id = Some("cursor-old-chat".to_string());
        hub.edit_message_and_rewind(
            2,
            "cursor edited prompt".to_string(),
            false,
            AgentWorkbenchMode::Agent,
            None,
        );
        assert_eq!(hub.active_session().cli_session_id, None);
        assert!(!hub.active_session().pending_fork_session);

        let _ = fs::remove_file(&tmp_file);
    }

    #[test]
    fn test_agent_history_persistence_roundtrip() {
        let hist_path = std::env::temp_dir().join(format!("rust_ai_ide_hist_{}.json", Uuid::new_v4()));

        let mut hub1 = AgentHub::new(Path::new("."));
        hub1.set_history_file_path(Some(hist_path.clone()));
        hub1.messages.push(AgentChatMessage {
            role: "user".to_string(),
            agent: AgentProviderKind::CursorCli,
            content: "持久化的第一条提问".to_string(),
            thought: None,
            checkpoint_id: None,
            patch: None,
            modified_files: Vec::new(),
            file_changes: Vec::new(),
            attachments: Vec::new(),
        });
        hub1.active_session_mut().cli_session_id = Some("cursor-chat-99".to_string());
        hub1.rename_active_session("修复支付逻辑");
        let s2 = hub1.new_session();
        assert_eq!(s2, 1);
        hub1.messages.push(AgentChatMessage {
            role: "user".to_string(),
            agent: AgentProviderKind::CursorCli,
            content: "第二个会话的问题".to_string(),
            thought: None,
            checkpoint_id: None,
            patch: None,
            modified_files: Vec::new(),
            file_changes: Vec::new(),
            attachments: Vec::new(),
        });
        hub1.save_history();

        let mut hub2 = AgentHub::new(Path::new("."));
        hub2.set_history_file_path(Some(hist_path.clone()));
        assert_eq!(hub2.sessions.len(), 2);
        assert_eq!(hub2.active_session_idx, 1);
        assert_eq!(hub2.sessions[0].title, "修复支付逻辑");
        assert_eq!(
            hub2.sessions[0].cli_session_id.as_deref(),
            Some("cursor-chat-99")
        );
        assert!(hub2.messages.iter().any(|m| m.content == "第二个会话的问题"));
        assert!(hub2.switch_session(0));
        assert!(hub2.messages.iter().any(|m| m.content == "持久化的第一条提问"));
        assert!(!hub2.active_session().is_busy);

        let _ = fs::remove_file(&hist_path);
    }

    #[test]
    fn test_turn_diff_tracker_accuracy_and_crlf_normalization() {
        let tmp = std::env::temp_dir().join(format!("rust_ai_ide_diff_tracker_{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let run_git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&tmp)
                .output()
                .expect("git")
        };
        assert!(run_git(&["init"]).status.success());
        let _ = run_git(&["config", "user.email", "test@example.com"]);
        let _ = run_git(&["config", "user.name", "Tester"]);

        let file_a = tmp.join("a.rs");
        fs::write(&file_a, "line1\nline2\nline3\nline4\n").unwrap();
        assert!(run_git(&["add", "a.rs"]).status.success());
        assert!(run_git(&["commit", "-m", "init"]).status.success());

        // Pre-existing uncommitted edit + CRLF line endings
        fs::write(&file_a, "line1\r\npre_dirty_2\r\nline3\r\nline4\r\n").unwrap();

        // Ask mode: should not report pre-existing dirty files even if read
        let ask_tracker = TurnDiffTracker::capture(&tmp, AgentWorkbenchMode::Ask);
        let (ask_mods, _, _) = ask_tracker.finish(&[]);
        assert!(ask_mods.is_empty());

        // Agent mode: untouched pre-dirty file should yield 0 changes; modifying 1 line counts +1 -1 only
        let agent_tracker_no_edit = TurnDiffTracker::capture(&tmp, AgentWorkbenchMode::Agent);
        let (no_edit_mods, _, _) = agent_tracker_no_edit.finish(&[]);
        assert!(no_edit_mods.is_empty());

        let agent_tracker_edit = TurnDiffTracker::capture(&tmp, AgentWorkbenchMode::Agent);
        fs::write(&file_a, "line1\r\npre_dirty_2\r\nline3\r\nagent_line4\r\n").unwrap();
        let (edit_mods, _, stats) = agent_tracker_edit.finish(&[]);
        assert_eq!(edit_mods.len(), 1);
        assert_eq!(stats.get(&edit_mods[0]).copied(), Some((1, 1)));

        // Untouched CRLF file compared to LF HEAD yields +1 -1 on the 1 changed line, not +4 -4
        assert_eq!(
            compute_file_diff_counts("l1\nl2\nl3\nl4\n", "l1\r\nl2\r\nl3_x\r\nl4\r\n"),
            (1, 1)
        );
        // Double CR (\r\r\n), standalone \r, BOM, and missing trailing EOF newline must not inflate +N -N
        assert_eq!(
            compute_file_diff_counts("a\nb\nc", "\u{FEFF}a\r\r\nb\rc\nd\ne"),
            (2, 0)
        );
        assert_eq!(
            compute_file_diff_counts("a\nb\nc\n", "a\r\r\nb\rc"),
            (0, 0)
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_turn_diff_tracker_handles_git_subdirectory_accurately() {
        let repo = std::env::temp_dir().join(format!("rust_ai_ide_subrepo_{}", Uuid::new_v4()));
        let sub = repo.join("packages/web");
        fs::create_dir_all(&sub).unwrap();
        let run_git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git")
        };
        assert!(run_git(&["init"]).status.success());
        let _ = run_git(&["config", "user.email", "test@example.com"]);
        let _ = run_git(&["config", "user.name", "Tester"]);

        let target = sub.join("app.ts");
        fs::write(&target, "const a = 1;\nconst b = 2;\n").unwrap();
        assert!(run_git(&["add", "-A"]).status.success());
        assert!(run_git(&["commit", "-m", "init"]).status.success());

        // Pre-dirty modification inside subdirectory
        fs::write(&target, "const a = 1;\nconst b = 2;\nconst c = 3;\n").unwrap();
        let tracker = TurnDiffTracker::capture(&sub, AgentWorkbenchMode::Agent);
        // Agent appends 2 lines cleanly without CRLF/newline artifacts
        fs::write(
            &target,
            "const a = 1;\r\nconst b = 2;\r\nconst c = 3;\r\nconst d = 4;\r\nconst e = 5;",
        )
        .unwrap();
        let (mods, _, stats) = tracker.finish(&[]);
        assert_eq!(mods.len(), 1);
        assert_eq!(stats.get(&mods[0]).copied(), Some((2, 0)));

        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn test_parallel_sessions_run_concurrently_without_blocking_new_session() {
        let mut hub = AgentHub::new(Path::new("."));
        let s1_id = hub.active_session().id.clone();

        // Simulate Session 1 currently running in background
        let slot1 = hub
            .try_begin_session_execution(
                &s1_id,
                StreamingAgentProgress::with_phase("Session 1 streaming..."),
            )
            .expect("session 1 should acquire its own slot");
        assert!(hub.is_active_session_busy());
        assert!(hub.is_busy.load(Ordering::Relaxed));

        // Create and switch to Session 2 while Session 1 is still busy
        let s2_idx = hub.new_session();
        assert_eq!(s2_idx, 1);
        let s2_id = hub.active_session().id.clone();
        assert_ne!(s1_id, s2_id);
        assert!(!hub.is_active_session_busy());
        assert!(hub.streaming_progress().is_none());

        // Session 2 MUST be able to acquire its own execution slot & submit concurrently!
        let _slot2 = hub
            .try_begin_session_execution(
                &s2_id,
                StreamingAgentProgress::with_phase("Session 2 streaming..."),
            )
            .expect("session 2 must not be blocked by session 1");
        assert!(hub.is_active_session_busy());
        assert_eq!(
            hub.streaming_progress().unwrap().phase,
            "Session 2 streaming..."
        );

        // Cancelling Session 2 stops ONLY Session 2, keeping Session 1 alive
        hub.cancel_active_request();
        assert!(!hub.is_active_session_busy());
        assert!(hub.is_busy.load(Ordering::Relaxed));
        assert!(slot1.is_busy.load(Ordering::Relaxed));

        // Switching back to Session 1 restores Session 1's active streaming progress
        assert!(hub.switch_session(0));
        assert!(hub.is_active_session_busy());
        assert_eq!(
            hub.streaming_progress().unwrap().phase,
            "Session 1 streaming..."
        );

        // Finishing Session 1 clears global busy flag cleanly
        AgentHub::finish_session_slot(&hub.session_slots, &hub.is_busy, &s1_id);
        assert!(!hub.is_active_session_busy());
        assert!(!hub.is_busy.load(Ordering::Relaxed));

        // Re-submitting in Session 2 now works cleanly (e.g. local command / offline fallback)
        assert!(hub.switch_session(1));
        hub.submit_workbench_prompt("/search AgentHub", AgentWorkbenchMode::Ask, None);
        assert!(hub.messages.iter().any(|m| m.role == "assistant"));
    }
}
