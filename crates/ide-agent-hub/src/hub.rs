use crate::acp::execute_acp_stdio_session;
use crate::cli_discovery::{
    discover_local_cli_registrations, probe_agy_cli_models, probe_cursor_cli_list_models,
    probe_gemini_cli_list_models, push_unique_model, resolve_agy_model_and_effort,
    resolve_cursor_model_id,
};
use crate::cli_runner::{execute_cli_once, CliRunParams};
use crate::http_fallback::{execute_agent_tool_loop, execute_curl_llm_streaming};
use crate::tools::{
    extract_enclosing_block, extract_first_fenced_code_block, load_workspace_agent_rules,
    maybe_extract_patch_from_reply,
};
use crate::types::{
    AgentAttachedItem, AgentChatMessage, AgentProviderKind, AgentRegistration, AgentSession,
    AgentWakeupCallback, AgentWorkbenchMode, AiApiConfig, IdeCodeContext, InlineEditSession,
    PastedCodeSnippet, ReasoningEffort, WorkspaceCheckpoint,
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

struct DiscoveredModelUpdate {
    kind: AgentProviderKind,
    display_models: Vec<String>,
    exact_catalog: Vec<String>,
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
    pub is_busy: Arc<AtomicBool>,
    cancel_requested: Arc<AtomicBool>,
    active_child_pid: Arc<Mutex<Option<u32>>>,
    streaming_text: Arc<Mutex<Option<String>>>,
    streaming_session_id: Arc<Mutex<Option<String>>>,
    async_inbox: Arc<Mutex<Vec<(String, AgentChatMessage)>>>,
    async_checkpoints_inbox: Arc<Mutex<Vec<(String, WorkspaceCheckpoint)>>>,
    modified_files_inbox: Arc<Mutex<Vec<PathBuf>>>,
    async_inline_result: Arc<Mutex<Option<(PendingInlineMeta, Result<String, String>)>>>,
    async_commit_msg: Arc<Mutex<Option<Result<String, String>>>>,
    async_discovered_models: Arc<Mutex<Vec<DiscoveredModelUpdate>>>,
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
            "未检测到本地 CLI（请安装 cursor-agent / agy / claude / codex / gemini CLI）"
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
            cancel_requested: Arc::new(AtomicBool::new(false)),
            active_child_pid: Arc::new(Mutex::new(None)),
            streaming_text: Arc::new(Mutex::new(None)),
            streaming_session_id: Arc::new(Mutex::new(None)),
            async_inbox: Arc::new(Mutex::new(Vec::new())),
            async_checkpoints_inbox: Arc::new(Mutex::new(Vec::new())),
            modified_files_inbox: Arc::new(Mutex::new(Vec::new())),
            async_inline_result: Arc::new(Mutex::new(None)),
            async_commit_msg: Arc::new(Mutex::new(None)),
            async_discovered_models: Arc::new(Mutex::new(Vec::new())),
            wakeup,
        };
        if hub.wakeup.is_some() {
            hub.spawn_async_cli_model_probe();
        }
        hub
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
        let cursor_bin = self
            .providers
            .iter()
            .find(|p| p.kind == AgentProviderKind::CursorCli)
            .and_then(|p| p.binary_path.clone());
        let agy_bin = self
            .providers
            .iter()
            .find(|p| p.kind == AgentProviderKind::GoogleAntigravityAcp)
            .and_then(|p| p.binary_path.clone());
        let gemini_bin = self
            .providers
            .iter()
            .find(|p| p.kind == AgentProviderKind::GeminiCli)
            .and_then(|p| p.binary_path.clone());

        if let Some(bin) = cursor_bin {
            let slot = self.async_discovered_models.clone();
            let wakeup = self.wakeup.clone();
            thread::spawn(move || {
                if let Some((base_models, exact_models)) = probe_cursor_cli_list_models(&bin) {
                    slot.lock().push(DiscoveredModelUpdate {
                        kind: AgentProviderKind::CursorCli,
                        display_models: base_models,
                        exact_catalog: exact_models,
                    });
                    if let Some(cb) = wakeup {
                        cb();
                    }
                }
            });
        }

        if let Some(bin) = agy_bin {
            let slot = self.async_discovered_models.clone();
            let wakeup = self.wakeup.clone();
            thread::spawn(move || {
                if let Some(models) = probe_agy_cli_models(&bin) {
                    slot.lock().push(DiscoveredModelUpdate {
                        kind: AgentProviderKind::GoogleAntigravityAcp,
                        display_models: models.clone(),
                        exact_catalog: models,
                    });
                    if let Some(cb) = wakeup {
                        cb();
                    }
                }
            });
        }

        if let Some(bin) = gemini_bin {
            let slot = self.async_discovered_models.clone();
            let wakeup = self.wakeup.clone();
            thread::spawn(move || {
                if let Some(models) = probe_gemini_cli_list_models(&bin) {
                    slot.lock().push(DiscoveredModelUpdate {
                        kind: AgentProviderKind::GeminiCli,
                        display_models: models.clone(),
                        exact_catalog: models,
                    });
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
        };
        self.sessions.push(sess);
        let new_idx = self.sessions.len() - 1;
        self.sync_session_in(new_idx);
        new_idx
    }

    pub fn switch_session(&mut self, target_idx: usize) -> bool {
        if target_idx >= self.sessions.len() || target_idx == self.active_session_idx {
            return false;
        }
        self.sync_active_session_out();
        self.sync_session_in(target_idx);
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
            return true;
        }
        self.sync_active_session_out();
        self.sessions.remove(idx);
        let next_idx = if idx < self.active_session_idx {
            self.active_session_idx - 1
        } else {
            self.active_session_idx.min(self.sessions.len() - 1)
        };
        self.sync_session_in(next_idx);
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

    pub fn is_active_session_busy(&self) -> bool {
        if !self.is_busy.load(Ordering::Relaxed) {
            return false;
        }
        let active_id = self.active_session().id.as_str();
        self.streaming_session_id.lock().as_deref() == Some(active_id)
    }

    pub fn streaming_preview(&self) -> Option<String> {
        if !self.is_active_session_busy() {
            return None;
        }
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
        let target_sid = self.streaming_session_id.lock().take();
        self.is_busy.store(false, Ordering::SeqCst);
        let partial = self.streaming_text.lock().take().unwrap_or_default();
        let content = if partial.trim().is_empty() {
            "⏹ 已取消当前 AI 请求。".to_string()
        } else {
            format!("{}\n\n[⏹ 已手动停止生成]", partial.trim_end())
        };
        let msg = AgentChatMessage {
            role: "assistant".to_string(),
            agent: self.active_provider,
            content,
            thought: Some("Request cancelled by user.".to_string()),
            checkpoint_id: None,
            patch: None,
            modified_files: Vec::new(),
            attachments: Vec::new(),
        };
        let active_id = self.active_session().id.clone();
        if target_sid.as_deref().unwrap_or(&active_id) == active_id {
            self.messages.push(msg);
        } else if let Some(sid) = &target_sid {
            if let Some(sess) = self.sessions.iter_mut().find(|s| &s.id == sid) {
                sess.messages.push(msg);
                sess.is_busy = false;
            }
        }
        self.active_session_mut().is_busy = false;
        if let Some(cb) = &self.wakeup {
            cb();
        }
    }

    /// 轮询后台异步消息、Checkpoint 与自动探测到的 CLI 模型列表；若有更新返回 true
    pub fn poll_async_messages(&mut self) -> bool {
        let mut changed = false;
        let global_busy = self.is_busy.load(Ordering::Relaxed);
        let stream_sid = self.streaming_session_id.lock().clone();
        for sess in &mut self.sessions {
            let should_busy = global_busy && stream_sid.as_deref() == Some(sess.id.as_str());
            if sess.is_busy != should_busy {
                sess.is_busy = should_busy;
                changed = true;
            }
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
        self.messages.clear();
        if self.streaming_session_id.lock().as_deref() == Some(self.active_session().id.as_str()) {
            *self.streaming_text.lock() = None;
        }
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
                "未检测到本地 AI CLI：请确认本机已安装 cursor-agent / agy / claude / codex / gemini CLI 并点击右上角 ↻ 刷新。"
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
                "未检测到可用的本地 AI CLI（cursor-agent / agy / claude / codex / gemini），请点击右上角 ↻ 刷新 CLI 状态"
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
        let make_params = |kind: AgentProviderKind| -> Option<CliRunParams> {
            let reg = self.providers.iter().find(|p| p.kind == kind)?;
            let bin = reg.binary_path.clone()?;
            Some(CliRunParams {
                kind,
                bin,
                root: self.workspace_root.clone(),
                model: reg.active_model.clone(),
                exact_catalog: reg.exact_model_catalog.clone(),
                fast_mode: reg.fast_mode,
                effort: reg.reasoning_effort,
                mode,
            })
        };

        if let Ok(acp_cmd) = std::env::var("ACP_AGENT_CMD") {
            if !acp_cmd.trim().is_empty()
                && self.active_provider == AgentProviderKind::GoogleAntigravityAcp
            {
                return ExecutionBackend::Acp(acp_cmd);
            }
        }

        let target = self.effective_provider_kind();
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

        self.messages.push(AgentChatMessage {
            role: "user".to_string(),
            agent: self.active_provider,
            content: user_display,
            thought: None,
            checkpoint_id: None,
            patch: None,
            modified_files: Vec::new(),
            attachments: attached_items.clone(),
        });

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
                attachments: Vec::new(),
            });
            return;
        }

        let target_session_id = self.active_session().id.clone();
        match self.resolve_execution_backend(mode) {
            ExecutionBackend::Cli(params) => {
                self.dispatch_cli_async(
                    params,
                    target_session_id,
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
                    content: "⚠ 未检测到本机 AI CLI。\n请确认已安装 `cursor-agent` (Cursor CLI)、`agy` (Antigravity CLI)、`claude`、`codex` 或 `gemini` CLI，然后点击右上角 `↻` 刷新即可自动识别模型列表。".to_string(),
                    thought: Some("No local CLI detected.".to_string()),
                    checkpoint_id: None,
                    patch: None,
                    modified_files: Vec::new(),
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
        if self.is_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancel_requested.store(false, Ordering::SeqCst);
        *self.streaming_session_id.lock() = Some(target_session_id.clone());
        *self.streaming_text.lock() = Some(format!("正在连接 {} ({})...", cfg.model, cfg.base_url));

        let (sys_prompt, user_prompt) = self.build_system_and_user_prompt(
            &prompt,
            mode,
            ctx.as_ref(),
            &attachments,
            &pasted_snippets,
        );
        let inbox = self.async_inbox.clone();
        let busy = self.is_busy.clone();
        let cancel = self.cancel_requested.clone();
        let stream_buf = self.streaming_text.clone();
        let stream_sid = self.streaming_session_id.clone();
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
            *stream_sid.lock() = None;

            if !cancel.load(Ordering::SeqCst) {
                let (content, patch) = match reply_res {
                    Ok(text) => {
                        let p = maybe_extract_patch_from_reply(&text, mode, ctx.as_ref());
                        (text, p)
                    }
                    Err(err) => (format!("✕ 请求失败：\n{err}"), None),
                };
                inbox.lock().push((
                    target_session_id,
                    AgentChatMessage {
                        role: "assistant".to_string(),
                        agent: provider,
                        content,
                        thought: Some(format!("Completed {} call.", cfg.model)),
                        checkpoint_id: None,
                        patch,
                        modified_files: Vec::new(),
                        attachments: Vec::new(),
                    },
                ));
            }

            busy.store(false, Ordering::SeqCst);
            if let Some(cb) = wakeup {
                cb();
            }
        });
    }

    fn dispatch_http_agent_loop_async(
        &mut self,
        cfg: AiApiConfig,
        target_session_id: String,
        prompt: String,
        ctx: Option<IdeCodeContext>,
        attachments: Vec<AgentAttachedItem>,
        pasted_snippets: Vec<PastedCodeSnippet>,
    ) {
        if self.is_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancel_requested.store(false, Ordering::SeqCst);
        *self.streaming_session_id.lock() = Some(target_session_id.clone());
        *self.streaming_text.lock() =
            Some(format!("🤖 Agent ({}) 正在规划工具调用...", cfg.model));

        let (sys_prompt, user_prompt) = self.build_system_and_user_prompt(
            &prompt,
            AgentWorkbenchMode::Agent,
            ctx.as_ref(),
            &attachments,
            &pasted_snippets,
        );
        let root = self.workspace_root.clone();
        let inbox = self.async_inbox.clone();
        let ck_inbox = self.async_checkpoints_inbox.clone();
        let mod_inbox = self.modified_files_inbox.clone();
        let busy = self.is_busy.clone();
        let cancel = self.cancel_requested.clone();
        let stream_buf = self.streaming_text.clone();
        let stream_sid = self.streaming_session_id.clone();
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
            *stream_sid.lock() = None;

            if !cancel.load(Ordering::SeqCst) {
                let touched_files: Vec<PathBuf> = ckpt_backups.keys().cloned().collect();
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
                    target_session_id,
                    AgentChatMessage {
                        role: "assistant".to_string(),
                        agent: provider,
                        content,
                        thought: Some(format!("Completed Agent tool loop via {}.", cfg.model)),
                        checkpoint_id: ckpt_id,
                        patch,
                        modified_files: touched_files,
                        attachments: Vec::new(),
                    },
                ));
            }

            busy.store(false, Ordering::SeqCst);
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
        prompt: String,
        mode: AgentWorkbenchMode,
        ctx: Option<IdeCodeContext>,
        attachments: Vec<AgentAttachedItem>,
        pasted_snippets: Vec<PastedCodeSnippet>,
    ) {
        if self.is_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancel_requested.store(false, Ordering::SeqCst);
        *self.streaming_session_id.lock() = Some(target_session_id.clone());
        *self.streaming_text.lock() = Some(format!("正在通过 ACP ({acp_cmd}) 建立会话..."));

        let (sys_prompt, user_prompt) = self.build_system_and_user_prompt(
            &prompt,
            mode,
            ctx.as_ref(),
            &attachments,
            &pasted_snippets,
        );
        let combined = format!("{sys_prompt}\n\n{user_prompt}");
        let root = self.workspace_root.clone();
        let inbox = self.async_inbox.clone();
        let mod_inbox = self.modified_files_inbox.clone();
        let busy = self.is_busy.clone();
        let cancel = self.cancel_requested.clone();
        let stream_buf = self.streaming_text.clone();
        let stream_sid = self.streaming_session_id.clone();
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
            *stream_sid.lock() = None;

            if !cancel.load(Ordering::SeqCst) {
                let (content, patch, touched) = match res {
                    Ok((reply, files)) => {
                        let p = maybe_extract_patch_from_reply(&reply, mode, ctx.as_ref());
                        (reply, p, files)
                    }
                    Err(e) => (format!("✕ ACP 会话失败：\n{e}"), None, Vec::new()),
                };
                inbox.lock().push((
                    target_session_id,
                    AgentChatMessage {
                        role: "assistant".to_string(),
                        agent: provider,
                        content,
                        thought: Some(format!("Completed ACP session via {acp_cmd}.")),
                        checkpoint_id: None,
                        patch,
                        modified_files: touched,
                        attachments: Vec::new(),
                    },
                ));
            }
            busy.store(false, Ordering::SeqCst);
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
        prompt: String,
        mode: AgentWorkbenchMode,
        ctx: Option<IdeCodeContext>,
        attachments: Vec<AgentAttachedItem>,
        pasted_snippets: Vec<PastedCodeSnippet>,
    ) {
        if self.is_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancel_requested.store(false, Ordering::SeqCst);
        *self.streaming_session_id.lock() = Some(target_session_id.clone());

        let cli_label = params
            .bin
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cli")
            .to_string();
        let resolved_badge = self.active_resolved_cli_model();
        *self.streaming_text.lock() = Some(format!(
            "正在通过 `{cli_label}` [{resolved_badge}] 实时处理中..."
        ));

        let (sys_prompt, user_prompt) = self.build_system_and_user_prompt(
            &prompt,
            mode,
            ctx.as_ref(),
            &attachments,
            &pasted_snippets,
        );
        let combined_prompt = format!("{sys_prompt}\n\n{user_prompt}");

        let inbox = self.async_inbox.clone();
        let mod_inbox = self.modified_files_inbox.clone();
        let busy = self.is_busy.clone();
        let cancel = self.cancel_requested.clone();
        let stream_buf = self.streaming_text.clone();
        let stream_sid = self.streaming_session_id.clone();
        let wakeup = self.wakeup.clone();
        let active_pid = self.active_child_pid.clone();
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

            *active_pid.lock() = None;
            *stream_buf.lock() = None;
            *stream_sid.lock() = None;

            if !cancel.load(Ordering::SeqCst) {
                let (content, thought, patch, modified_files) = match run_res {
                    Ok(out) => {
                        let p = maybe_extract_patch_from_reply(&out.reply, mode, ctx.as_ref());
                        (
                            out.reply,
                            out.thought.or_else(|| {
                                Some(format!("Completed via {} ({})", cli_label, out.resolved_model))
                            }),
                            p,
                            out.modified_files,
                        )
                    }
                    Err(err_msg) => (
                        format!(
                            "✕ `{cli_label}` ({resolved_badge}) 调用异常：\n{err_msg}\n\n提示：可点击下方【模型】或【思考深度】调整参数后重试，或点击顶部切换其他本地 CLI。"
                        ),
                        None,
                        None,
                        Vec::new(),
                    ),
                };

                inbox.lock().push((
                    target_session_id,
                    AgentChatMessage {
                        role: "assistant".to_string(),
                        agent: display_provider,
                        content,
                        thought,
                        checkpoint_id: None,
                        patch,
                        modified_files,
                        attachments: Vec::new(),
                    },
                ));
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
        let target_session_id = self.active_session().id.clone();
        *self.streaming_session_id.lock() = Some(target_session_id.clone());
        let root = self.workspace_root.clone();
        let inbox = self.async_inbox.clone();
        let busy = self.is_busy.clone();
        let stream_buf = self.streaming_text.clone();
        let stream_sid = self.streaming_session_id.clone();
        let wakeup = self.wakeup.clone();
        let provider = self.active_provider;

        *stream_buf.lock() =
            Some("正在后台执行 `cargo check --workspace --offline`...".to_string());
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
            *stream_sid.lock() = None;
            inbox.lock().push((
                target_session_id,
                AgentChatMessage {
                    role: "assistant".to_string(),
                    agent: provider,
                    content: text,
                    thought: Some("Completed background cargo check.".to_string()),
                    checkpoint_id: None,
                    patch: None,
                    modified_files: Vec::new(),
                    attachments: Vec::new(),
                },
            ));
            busy.store(false, Ordering::SeqCst);
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

        hub.active_provider = AgentProviderKind::GeminiCli;
        assert_eq!(
            hub.mode_cli_flag_for_active(AgentWorkbenchMode::Ask),
            "--approval-mode plan"
        );
    }
}
