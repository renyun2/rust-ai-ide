use crate::canvas::{FontStyle, Rect, SurfaceCanvas};
use ide_agent_hub::{AgentHub, AgentProviderKind};
use ide_core::{
    IdeAction, IdeaTheme, MainMenuCategory, RunConfiguration, ThemeKind, ToolWindowId,
};
use ide_dap::DapDebugEngine;
use ide_git::{DiffLineKind, GitEngine};
use ide_lsp::LspHub;
use ide_search::{find_in_files, search_everywhere, ContentSearchHit, SearchEverywhereItem};
use ide_syntax::{
    detect_runnable_gutter_entries, extract_structure_symbols, highlight_line, RunnableGutterEntry,
    StructureSymbol, TokenKind,
};
use ide_terminal::TerminalSession;
use ide_vfs::{ProjectTreeState, TextBuffer};
use std::path::PathBuf;

/// 可拖拽调整大小的面板分割线标识
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragSplitter {
    LeftPanel,
    RightPanel,
    BottomPanel,
}

#[derive(Debug, Clone)]
pub enum HitAction {
    // 顶部主菜单与弹窗
    ToggleHamburgerMenu,
    OpenMenuCategory(MainMenuCategory),
    ExecuteIdeAction(IdeAction),
    ToggleGitBranchPopup,
    CheckoutGitBranch(String),
    CreateNewGitBranchPrompt,
    // 编辑器与标签页
    SelectTab(usize),
    CloseTab(usize),
    OpenFile(PathBuf),
    JumpToLine(usize),
    // 项目树文件夹与文件操作
    ToggleDirectory(PathBuf),
    PromptNewFile,
    PromptNewFolder,
    ExpandAllDirs,
    CollapseAllDirs,
    // 左右底部面板切换
    ToggleLeftTool(ToolWindowId),
    ToggleRightAgentHub,
    SelectBottomTool(ToolWindowId),
    // Git 面板交互
    ToggleStageFile(PathBuf),
    StageAllGitChanges,
    CommitAllGitChanges,
    GenerateAiCommitMessage,
    RollbackGitFile(PathBuf),
    FocusGitCommitMessageInput,
    // Multi-Agent 与内联编辑
    SelectAgentProvider(AgentProviderKind),
    TriggerCtrlKInlineEdit,
    AcceptInlineDiff,
    RejectInlineDiff,
    SendSelectionToAgent,
    RevertLastCheckpoint,
    FocusAgentChatInput,
    SubmitAgentChatPrompt,
    // 终端与运行调试
    FocusTerminalInput,
    SubmitTerminalCommand,
    RunActiveConfig,
    DebugActiveConfig,
    CycleRunConfig,
    ToggleSearchEverywhere,
    ToggleBreakpointAtLine(usize),
    RunGutterEntry(usize),
    // 设置与主题切换
    SelectTheme(ThemeKind),
    CloseModalOrPopup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    Editor,
    CtrlKPrompt,
    AgentChatInput,
    TerminalInput,
    GitCommitInput,
    SearchEverywhereModal,
    FindInFilesModal,
    NewFileModal,
    NewFolderModal,
    NewGitBranchModal,
    SettingsModal,
}

pub struct IdeApplicationState {
    pub workspace_root: PathBuf,
    pub theme: IdeaTheme,
    pub project_tree: ProjectTreeState,
    pub open_buffers: Vec<TextBuffer>,
    pub active_tab: usize,
    pub scroll_line: usize,
    pub tree_scroll: usize,
    // 可拖拽调节的停靠面板尺寸
    pub left_tool: Option<ToolWindowId>,
    pub left_panel_width: i32,
    pub right_agent_open: bool,
    pub right_panel_width: i32,
    pub bottom_tool: Option<ToolWindowId>,
    pub bottom_panel_height: i32,
    pub active_splitter_drag: Option<DragSplitter>,
    // 顶部主菜单与弹窗状态
    pub hamburger_menu_expanded: bool,
    pub active_menu_category: Option<MainMenuCategory>,
    pub git_branch_popup_open: bool,
    // 子系统引擎
    pub run_configs: Vec<RunConfiguration>,
    pub active_run_config: usize,
    pub lsp_hub: LspHub,
    pub dap_engine: DapDebugEngine,
    pub git_engine: GitEngine,
    pub terminal: TerminalSession,
    pub agent_hub: AgentHub,
    // 焦点与输入框状态
    pub focus: FocusTarget,
    pub ctrl_k_input: String,
    pub agent_chat_input: String,
    pub modal_text_input: String,
    pub search_query: String,
    pub search_results: Vec<SearchEverywhereItem>,
    pub content_search_hits: Vec<ContentSearchHit>,
    pub hit_regions: Vec<(Rect, HitAction)>,
    pub status_notification: String,
}

impl IdeApplicationState {
    pub fn new(workspace_root: PathBuf) -> Self {
        let project_tree = ProjectTreeState::new(&workspace_root);
        let run_configs = RunConfiguration::detect_for_workspace(&workspace_root);
        let lsp_hub = LspHub::new(&workspace_root);
        let dap_engine = DapDebugEngine::new();
        let git_engine = GitEngine::open(&workspace_root);
        let terminal = TerminalSession::new("Local PowerShell (PTY)", &workspace_root);
        let agent_hub = AgentHub::new(&workspace_root);

        let mut open_buffers = Vec::new();
        let default_candidates = [
            workspace_root.join("crates/ide-agent-hub/src/lib.rs"),
            workspace_root.join("crates/ide-core/src/lib.rs"),
            workspace_root.join("Cargo.toml"),
        ];
        for cand in default_candidates {
            if cand.exists() {
                if let Ok(buf) = TextBuffer::open(&cand) {
                    open_buffers.push(buf);
                }
            }
        }
        if open_buffers.is_empty() {
            open_buffers.push(TextBuffer::from_text(
                workspace_root.join("src/main.rs"),
                "fn main() {\n    let res = Some(42).unwrap();\n    println!(\"Hello Rust AI IDE: {res}\");\n}\n",
            ));
        }

        let mut state = Self {
            workspace_root,
            theme: IdeaTheme::jetbrains_islands_dark(),
            project_tree,
            open_buffers,
            active_tab: 0,
            scroll_line: 0,
            tree_scroll: 0,
            left_tool: Some(ToolWindowId::Project),
            left_panel_width: 280,
            right_agent_open: true,
            right_panel_width: 430,
            bottom_tool: Some(ToolWindowId::Terminal),
            bottom_panel_height: 215,
            active_splitter_drag: None,
            hamburger_menu_expanded: true, // 默认展开 IDEA 顶部菜单栏，方便直接点击 File/Edit/View/Git/Run/Tools
            active_menu_category: None,
            git_branch_popup_open: false,
            run_configs,
            active_run_config: 0,
            lsp_hub,
            dap_engine,
            git_engine,
            terminal,
            agent_hub,
            focus: FocusTarget::Editor,
            ctrl_k_input: "将 unwrap() 替换为安全错误处理并补充文档注释".to_string(),
            agent_chat_input: String::new(),
            modal_text_input: String::new(),
            search_query: String::new(),
            search_results: Vec::new(),
            content_search_hits: Vec::new(),
            hit_regions: Vec::new(),
            status_notification: "IDEA Islands UI 就绪 | 拖拽面板边缘可缩放窗口 | Ctrl+Alt+S 设置与快捷键 | 双击 Shift 全局搜索".to_string(),
        };

        let first_path = state.active_buffer().path.clone();
        state.dap_engine.toggle_breakpoint(&first_path, 17);
        state
    }

    pub fn active_buffer(&self) -> &TextBuffer {
        &self.open_buffers[self.active_tab.min(self.open_buffers.len().saturating_sub(1))]
    }

    pub fn active_buffer_mut(&mut self) -> &mut TextBuffer {
        let idx = self.active_tab.min(self.open_buffers.len().saturating_sub(1));
        &mut self.open_buffers[idx]
    }

    pub fn open_file(&mut self, path: PathBuf) {
        self.project_tree.selected_path = Some(path.clone());
        if let Some(existing_idx) = self.open_buffers.iter().position(|b| b.path == path) {
            self.active_tab = existing_idx;
            self.scroll_line = 0;
            return;
        }
        if let Ok(buf) = TextBuffer::open(&path) {
            self.open_buffers.push(buf);
            self.active_tab = self.open_buffers.len() - 1;
            self.scroll_line = 0;
            self.status_notification = format!("已打开: {}", path.display());
        }
    }

    pub fn close_tab(&mut self, idx: usize) {
        if self.open_buffers.len() <= 1 || idx >= self.open_buffers.len() {
            return;
        }
        let closed = self.open_buffers.remove(idx);
        if self.active_tab >= self.open_buffers.len() {
            self.active_tab = self.open_buffers.len() - 1;
        }
        self.status_notification = format!("已关闭标签页: {}", closed.path.display());
    }

    pub fn execute_ide_action(&mut self, action: IdeAction) {
        self.active_menu_category = None;
        match action {
            IdeAction::SearchEverywhere | IdeAction::GoToFile | IdeAction::FindAction => {
                self.focus = FocusTarget::SearchEverywhereModal;
                self.search_results =
                    search_everywhere(&self.workspace_root, &self.search_query, 14);
            }
            IdeAction::FindInFiles => {
                self.focus = FocusTarget::FindInFilesModal;
                if self.search_query.is_empty() {
                    self.search_query = "AgentProviderKind".to_string();
                }
                self.content_search_hits =
                    find_in_files(&self.workspace_root, &self.search_query, 20)
                        .unwrap_or_default();
            }
            IdeAction::GoToDeclaration => {
                let buf = self.active_buffer();
                let symbols = extract_structure_symbols(&buf.text(), buf.language);
                if let Some(sym) = symbols.first() {
                    let line = sym.line;
                    self.active_buffer_mut().cursor_line = line;
                    self.scroll_line = line.saturating_sub(3);
                    self.status_notification =
                        format!("已跳转至符号定义: {} (第 {} 行)", sym.name, line + 1);
                }
            }
            IdeAction::NewFileInProject => {
                self.modal_text_input = "new_module.rs".to_string();
                self.focus = FocusTarget::NewFileModal;
            }
            IdeAction::NewFolderInProject => {
                self.modal_text_input = "new_package".to_string();
                self.focus = FocusTarget::NewFolderModal;
            }
            IdeAction::DeleteSelectedPath => {
                if let Some(sel) = self.project_tree.selected_path.clone() {
                    if self.project_tree.delete_path(&sel).is_ok() {
                        self.git_engine.refresh();
                        self.status_notification = format!("已删除: {}", sel.display());
                    }
                }
            }
            IdeAction::SaveActiveFile => {
                if self.active_buffer_mut().save().is_ok() {
                    self.git_engine.refresh();
                    self.status_notification = "已保存所有文档并刷新 Git 状态".to_string();
                }
            }
            IdeAction::CloseActiveTab => {
                let idx = self.active_tab;
                self.close_tab(idx);
            }
            IdeAction::UndoEdit => {
                self.active_buffer_mut().undo();
                self.status_notification = "已撤销编辑 (Ctrl+Z)".to_string();
            }
            IdeAction::RedoEdit => {
                self.active_buffer_mut().redo();
                self.status_notification = "已重做编辑 (Ctrl+Shift+Z)".to_string();
            }
            IdeAction::DuplicateLine => {
                self.active_buffer_mut().duplicate_current_line();
                self.status_notification = "已复制当前行 (IDEA Ctrl+D)".to_string();
            }
            IdeAction::DeleteLine => {
                self.active_buffer_mut().delete_current_line();
                self.status_notification = "已删除当前行 (IDEA Ctrl+Y)".to_string();
            }
            IdeAction::ToggleLineComment => {
                self.active_buffer_mut().toggle_line_comment();
                self.status_notification = "已切换行注释 (IDEA Ctrl+/)".to_string();
            }
            IdeAction::ReformatCode => {
                let _ = self.active_buffer_mut().save();
                self.status_notification = "已格式化当前代码文件 (IDEA Ctrl+Alt+L)".to_string();
            }
            IdeAction::RenameSymbol | IdeAction::ShowIntentionActions => {
                self.trigger_ctrl_k_inline_edit();
            }
            IdeAction::InlineAiEdit => self.trigger_ctrl_k_inline_edit(),
            IdeAction::SendSelectionToAgent => {
                let buf = self.active_buffer().clone();
                let start = buf.cursor_line;
                let end = (start + 8).min(buf.line_count().saturating_sub(1));
                let sel = buf.slice_lines(start, end);
                self.agent_hub.send_prompt_with_ide_context(
                    "请审阅并优化当前选中代码片段",
                    Some(&buf.path),
                    Some(&sel),
                    None,
                );
                self.right_agent_open = true;
                self.status_notification = format!(
                    "已将第 {}..{} 行发送至 {}",
                    start + 1,
                    end + 1,
                    self.agent_hub.active_provider.display_name()
                );
            }
            IdeAction::AcceptInlineDiff => self.accept_inline_diff(),
            IdeAction::RejectInlineDiff => self.reject_inline_diff(),
            IdeAction::RevertLastCheckpoint => self.revert_last_checkpoint(),
            IdeAction::SwitchToAntigravity => {
                self.agent_hub.active_provider = AgentProviderKind::GoogleAntigravityAcp;
                self.right_agent_open = true;
            }
            IdeAction::SwitchToClaudeCode => {
                self.agent_hub.active_provider = AgentProviderKind::ClaudeCodeCli;
                self.right_agent_open = true;
            }
            IdeAction::SwitchToCodexCli => {
                self.agent_hub.active_provider = AgentProviderKind::OpenAiCodexCli;
                self.right_agent_open = true;
            }
            IdeAction::RunActiveConfig => {
                if let Some(cfg) = self.run_configs.get(self.active_run_config).cloned() {
                    self.bottom_tool = Some(ToolWindowId::Terminal);
                    self.terminal.execute_run_configuration(&cfg);
                    self.status_notification = format!("已运行配置: {}", cfg.name);
                }
            }
            IdeAction::DebugActiveConfig => {
                let buf = self.active_buffer().clone();
                self.dap_engine
                    .start_debug_session(buf.language, &buf.path, buf.cursor_line);
                self.bottom_tool = Some(ToolWindowId::Debug);
                self.status_notification =
                    format!("DAP 调试会话已启动 ({})", self.dap_engine.active_adapter);
            }
            IdeAction::ToggleBreakpoint => {
                let path = self.active_buffer().path.clone();
                let line = self.active_buffer().cursor_line;
                self.dap_engine.toggle_breakpoint(&path, line);
            }
            IdeAction::GitCommitPopup | IdeAction::ToggleCommitWindow => {
                self.git_engine.refresh();
                self.left_tool = Some(ToolWindowId::Commit);
            }
            IdeAction::GitPull => {
                if let Ok(msg) = self.git_engine.pull() {
                    self.status_notification = msg;
                }
            }
            IdeAction::GitPush => {
                if let Ok(msg) = self.git_engine.push() {
                    self.status_notification = msg;
                }
            }
            IdeAction::GitNewBranch => {
                self.modal_text_input = "feature/ai-agent-flow".to_string();
                self.focus = FocusTarget::NewGitBranchModal;
            }
            IdeAction::ToggleProjectWindow => {
                self.left_tool = if self.left_tool == Some(ToolWindowId::Project) {
                    None
                } else {
                    Some(ToolWindowId::Project)
                };
            }
            IdeAction::ToggleStructureWindow => {
                self.left_tool = if self.left_tool == Some(ToolWindowId::Structure) {
                    None
                } else {
                    Some(ToolWindowId::Structure)
                };
            }
            IdeAction::ToggleAgentHubWindow => {
                self.right_agent_open = !self.right_agent_open;
            }
            IdeAction::ToggleTerminalWindow => {
                self.bottom_tool = if self.bottom_tool == Some(ToolWindowId::Terminal) {
                    None
                } else {
                    Some(ToolWindowId::Terminal)
                };
            }
            IdeAction::OpenSettingsModal => {
                self.focus = FocusTarget::SettingsModal;
            }
            IdeAction::SwitchTheme => {
                let next_kind = self.theme.kind.next();
                self.theme = IdeaTheme::from_kind(next_kind);
                self.status_notification =
                    format!("已切换主题为: {}", next_kind.display_name());
            }
            IdeAction::WidenLeftPanel => {
                self.left_panel_width = (self.left_panel_width + 35).min(520);
            }
            IdeAction::NarrowLeftPanel => {
                self.left_panel_width = (self.left_panel_width - 35).max(180);
            }
            IdeAction::WidenRightPanel => {
                self.right_panel_width = (self.right_panel_width + 35).min(600);
            }
            IdeAction::RaiseBottomPanel => {
                self.bottom_panel_height = (self.bottom_panel_height + 30).min(420);
            }
        }
    }

    pub fn trigger_ctrl_k_inline_edit(&mut self) {
        self.focus = FocusTarget::CtrlKPrompt;
        let buf = self.active_buffer().clone();
        let start = buf.cursor_line.min(buf.line_count().saturating_sub(1));
        let end = (start + 5).min(buf.line_count().saturating_sub(1));
        let slice = buf.slice_lines(start, end);
        let prompt = self.ctrl_k_input.clone();

        let full_text = buf.text();
        let _ckpt_id = self.agent_hub.create_checkpoint(
            format!("Before Ctrl+K on {}", buf.path.display()),
            &[(&buf.path, &full_text)],
        );

        self.agent_hub
            .propose_ctrl_k_inline_edit(buf.path, buf.language, start, end, &slice, &prompt);
        self.status_notification =
            "Cursor Ctrl+K 行内修改已生成：按 Tab 接受修改，按 Esc 拒绝".to_string();
    }

    pub fn accept_inline_diff(&mut self) {
        if let Some(session) = self.agent_hub.inline_edit.take() {
            if session.active {
                let buf = self.active_buffer_mut();
                buf.replace_line_range(session.start_line, session.end_line, &session.proposed_code);
                self.focus = FocusTarget::Editor;
                self.status_notification = format!(
                    "已应用 AI 行内修改（第 {}..{} 行，可随时点击快照回滚）",
                    session.start_line + 1,
                    session.end_line + 1
                );
            }
        }
    }

    pub fn reject_inline_diff(&mut self) {
        self.agent_hub.inline_edit = None;
        self.focus = FocusTarget::Editor;
        self.status_notification = "已拒绝 AI 行内修改".to_string();
    }

    pub fn revert_last_checkpoint(&mut self) {
        if let Some(ckpt) = self.agent_hub.checkpoints.pop() {
            for (path, content) in ckpt.file_backups {
                if let Some(buf) = self.open_buffers.iter_mut().find(|b| b.path == path) {
                    *buf = TextBuffer::from_text(path, &content);
                }
            }
            self.status_notification = format!("已回滚至快照 Checkpoint: {}", ckpt.id);
        } else {
            self.status_notification = "当前没有可回滚的 Checkpoint 快照".to_string();
        }
    }

    /// 检测鼠标按下位置是否命中左右或底部面板的可拖拽分割线
    pub fn try_begin_splitter_drag(&mut self, x: i32, y: i32, win_w: i32, win_h: i32) -> bool {
        let top_h = 40;
        let status_h = 28;
        let stripe_w = 42;
        if y <= top_h || y >= win_h - status_h {
            return false;
        }

        if self.left_tool.is_some() {
            let split_x = stripe_w + self.left_panel_width;
            if (x - split_x).abs() <= 5 {
                self.active_splitter_drag = Some(DragSplitter::LeftPanel);
                return true;
            }
        }
        if self.right_agent_open {
            let split_x = win_w - stripe_w - self.right_panel_width;
            if (x - split_x).abs() <= 5 {
                self.active_splitter_drag = Some(DragSplitter::RightPanel);
                return true;
            }
        }
        if self.bottom_tool.is_some() {
            let split_y = win_h - status_h - self.bottom_panel_height;
            if (y - split_y).abs() <= 5 {
                self.active_splitter_drag = Some(DragSplitter::BottomPanel);
                return true;
            }
        }
        false
    }

    pub fn update_splitter_drag(&mut self, x: i32, y: i32, win_w: i32, win_h: i32) -> bool {
        let Some(drag) = self.active_splitter_drag else {
            return false;
        };
        let stripe_w = 42;
        let status_h = 28;
        match drag {
            DragSplitter::LeftPanel => {
                self.left_panel_width = (x - stripe_w).clamp(170, 540);
            }
            DragSplitter::RightPanel => {
                self.right_panel_width = (win_w - stripe_w - x).clamp(260, 640);
            }
            DragSplitter::BottomPanel => {
                self.bottom_panel_height = (win_h - status_h - y).clamp(110, 460);
            }
        }
        true
    }

    pub fn end_splitter_drag(&mut self) {
        self.active_splitter_drag = None;
    }

    pub fn handle_mouse_click(&mut self, x: i32, y: i32) {
        let action = self
            .hit_regions
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(x, y))
            .map(|(_, act)| act.clone());

        let Some(act) = action else {
            self.active_menu_category = None;
            self.git_branch_popup_open = false;
            return;
        };

        match act {
            HitAction::ToggleHamburgerMenu => {
                self.hamburger_menu_expanded = !self.hamburger_menu_expanded;
                if !self.hamburger_menu_expanded {
                    self.active_menu_category = None;
                }
            }
            HitAction::OpenMenuCategory(cat) => {
                self.git_branch_popup_open = false;
                if self.active_menu_category == Some(cat) {
                    self.active_menu_category = None;
                } else {
                    self.active_menu_category = Some(cat);
                }
            }
            HitAction::ExecuteIdeAction(ide_act) => {
                self.execute_ide_action(ide_act);
            }
            HitAction::ToggleGitBranchPopup => {
                self.active_menu_category = None;
                self.git_engine.refresh();
                self.git_branch_popup_open = !self.git_branch_popup_open;
            }
            HitAction::CheckoutGitBranch(branch) => {
                self.git_branch_popup_open = false;
                if let Ok(msg) = self.git_engine.checkout_branch(&branch) {
                    self.status_notification = msg;
                }
            }
            HitAction::CreateNewGitBranchPrompt => {
                self.git_branch_popup_open = false;
                self.modal_text_input = "feature/new-agent".to_string();
                self.focus = FocusTarget::NewGitBranchModal;
            }
            HitAction::SelectTab(idx) => {
                self.active_tab = idx;
                self.scroll_line = 0;
                self.focus = FocusTarget::Editor;
            }
            HitAction::CloseTab(idx) => self.close_tab(idx),
            HitAction::OpenFile(p) => {
                self.open_file(p);
                self.focus = FocusTarget::Editor;
            }
            HitAction::JumpToLine(line) => {
                self.active_buffer_mut().cursor_line = line;
                self.focus = FocusTarget::Editor;
            }
            HitAction::ToggleDirectory(dir) => {
                self.project_tree.toggle_dir(&dir);
                self.status_notification = format!("已切换目录展开状态: {}", dir.display());
            }
            HitAction::PromptNewFile => self.execute_ide_action(IdeAction::NewFileInProject),
            HitAction::PromptNewFolder => self.execute_ide_action(IdeAction::NewFolderInProject),
            HitAction::ExpandAllDirs => {
                self.project_tree.expand_all();
                self.status_notification = "已展开全部项目目录".to_string();
            }
            HitAction::CollapseAllDirs => {
                self.project_tree.collapse_all();
                self.status_notification = "已折叠全部项目目录".to_string();
            }
            HitAction::ToggleLeftTool(tool) => {
                if self.left_tool == Some(tool) {
                    self.left_tool = None;
                } else {
                    if tool == ToolWindowId::Commit {
                        self.git_engine.refresh();
                    }
                    self.left_tool = Some(tool);
                }
            }
            HitAction::ToggleRightAgentHub => {
                self.right_agent_open = !self.right_agent_open;
            }
            HitAction::SelectBottomTool(tool) => {
                if self.bottom_tool == Some(tool) {
                    self.bottom_tool = None;
                } else {
                    self.bottom_tool = Some(tool);
                }
            }
            HitAction::ToggleStageFile(path) => {
                let _ = self.git_engine.toggle_stage_file(&path);
            }
            HitAction::StageAllGitChanges => {
                if let Ok(msg) = self.git_engine.stage_all() {
                    self.status_notification = msg;
                }
            }
            HitAction::CommitAllGitChanges => {
                let msg = self.git_engine.commit_message_draft.clone();
                if let Ok(res) = self.git_engine.commit_all(&msg) {
                    self.status_notification = format!("Git Commit: {res}");
                }
            }
            HitAction::GenerateAiCommitMessage => {
                let generated = self.git_engine.generate_ai_commit_message();
                self.status_notification = format!("AI 已生成提交说明: {generated}");
            }
            HitAction::RollbackGitFile(path) => {
                if let Ok(msg) = self.git_engine.rollback_file(&path) {
                    self.status_notification = msg;
                }
            }
            HitAction::FocusGitCommitMessageInput => {
                self.focus = FocusTarget::GitCommitInput;
            }
            HitAction::SelectAgentProvider(provider) => {
                self.agent_hub.active_provider = provider;
                self.status_notification =
                    format!("已切换智能体为: {}", provider.display_name());
            }
            HitAction::TriggerCtrlKInlineEdit => self.trigger_ctrl_k_inline_edit(),
            HitAction::AcceptInlineDiff => self.accept_inline_diff(),
            HitAction::RejectInlineDiff => self.reject_inline_diff(),
            HitAction::SendSelectionToAgent => {
                self.execute_ide_action(IdeAction::SendSelectionToAgent)
            }
            HitAction::RevertLastCheckpoint => self.revert_last_checkpoint(),
            HitAction::FocusAgentChatInput => {
                self.focus = FocusTarget::AgentChatInput;
            }
            HitAction::SubmitAgentChatPrompt => {
                let prompt = if self.agent_chat_input.trim().is_empty() {
                    "请检查当前文件的结构与错误处理，并给出改进建议".to_string()
                } else {
                    std::mem::take(&mut self.agent_chat_input)
                };
                let active_path = self.active_buffer().path.clone();
                self.agent_hub
                    .send_prompt_with_ide_context(&prompt, Some(&active_path), None, None);
                self.status_notification = format!(
                    "已向 {} 发送指令",
                    self.agent_hub.active_provider.display_name()
                );
            }
            HitAction::FocusTerminalInput => {
                self.focus = FocusTarget::TerminalInput;
            }
            HitAction::SubmitTerminalCommand => {
                let cmd = if self.terminal.input_buffer.trim().is_empty() {
                    "git status -s".to_string()
                } else {
                    std::mem::take(&mut self.terminal.input_buffer)
                };
                self.terminal.execute_command_line(&cmd);
                self.git_engine.refresh();
                self.status_notification = format!("终端已执行: {cmd}");
            }
            HitAction::RunActiveConfig => self.execute_ide_action(IdeAction::RunActiveConfig),
            HitAction::DebugActiveConfig => self.execute_ide_action(IdeAction::DebugActiveConfig),
            HitAction::CycleRunConfig => {
                if !self.run_configs.is_empty() {
                    self.active_run_config = (self.active_run_config + 1) % self.run_configs.len();
                }
            }
            HitAction::ToggleSearchEverywhere => {
                self.execute_ide_action(IdeAction::SearchEverywhere)
            }
            HitAction::ToggleBreakpointAtLine(line) => {
                let path = self.active_buffer().path.clone();
                let added = self.dap_engine.toggle_breakpoint(&path, line);
                self.status_notification = if added {
                    format!("已在 {}:{} 添加红点断点 (Ctrl+F8)", path.display(), line + 1)
                } else {
                    format!("已移除 {}:{} 的断点", path.display(), line + 1)
                };
            }
            HitAction::RunGutterEntry(line) => {
                self.bottom_tool = Some(ToolWindowId::Terminal);
                self.terminal
                    .execute_command_line("cargo test --workspace --offline");
                self.status_notification = format!("已运行第 {} 行测试/入口", line + 1);
            }
            HitAction::SelectTheme(kind) => {
                self.theme = IdeaTheme::from_kind(kind);
                self.status_notification = format!("已应用主题: {}", kind.display_name());
            }
            HitAction::CloseModalOrPopup => {
                self.focus = FocusTarget::Editor;
                self.active_menu_category = None;
                self.git_branch_popup_open = false;
            }
        }
    }

    /// 渲染最新 JetBrains IntelliJ IDEA (Islands New UI) 完整工作台
    pub fn render(&mut self, canvas: &mut SurfaceCanvas) {
        self.hit_regions.clear();
        let w = canvas.width as i32;
        let h = canvas.height as i32;
        let t = self.theme;

        // 1. Islands 外围深色底板
        canvas.clear(t.window_bg);

        let top_bar_h = 40;
        let status_bar_h = 26;
        let left_stripe_w = 42;
        let right_stripe_w = 42;

        // 2. 顶部 Main Toolbar（含三横杠主菜单 File/Edit/View/Navigate/Code/Run/Git/Tools/Window/Help + Git 分支 Widget + Search Everywhere + Run/Debug Widget）
        self.render_main_toolbar(canvas, Rect::new(0, 0, w, top_bar_h));

        // 3. 底部 Status Bar
        self.render_status_bar(canvas, Rect::new(0, h - status_bar_h, w, status_bar_h));

        let body_y = top_bar_h;
        let body_h = (h - top_bar_h - status_bar_h).max(120);

        // 4. 左右 Tool Window 图标停靠条 (Tool Stripes)
        self.render_left_stripe(canvas, Rect::new(0, body_y, left_stripe_w, body_h));
        self.render_right_stripe(
            canvas,
            Rect::new(w - right_stripe_w, body_y, right_stripe_w, body_h),
        );

        let left_w = if self.left_tool.is_some() {
            self.left_panel_width
        } else {
            0
        };
        let right_w = if self.right_agent_open {
            self.right_panel_width
        } else {
            0
        };
        let bottom_h = if self.bottom_tool.is_some() {
            self.bottom_panel_height
        } else {
            0
        };
        let upper_h = (body_h - bottom_h).max(100);

        // 5. 左侧悬浮岛屿面板 (Project / Commit / Structure)
        if let Some(tool) = self.left_tool {
            let island = Rect::new(left_stripe_w + 2, body_y + 2, left_w - 4, upper_h - 4);
            self.render_left_tool_window(canvas, island, tool);
        }

        // 6. 右侧 Multi-Agent 悬浮岛屿面板
        if self.right_agent_open {
            let island = Rect::new(
                w - right_stripe_w - right_w + 2,
                body_y + 2,
                right_w - 4,
                upper_h - 4,
            );
            self.render_right_agent_hub(canvas, island);
        }

        // 7. 中央代码编辑器区域
        let editor_x = left_stripe_w + left_w + 2;
        let editor_w = (w - left_stripe_w - right_stripe_w - left_w - right_w - 4).max(220);
        let editor_rect = Rect::new(editor_x, body_y + 2, editor_w, upper_h - 4);
        self.render_center_editor(canvas, editor_rect);

        // 8. 底部悬浮岛屿面板 (Terminal / Problems / Debug / Git Log)
        if let Some(btool) = self.bottom_tool {
            let bottom_island = Rect::new(
                left_stripe_w + 2,
                body_y + upper_h + 2,
                w - left_stripe_w - right_stripe_w - 4,
                bottom_h - 4,
            );
            self.render_bottom_tool_window(canvas, bottom_island, btool);
        }

        // 9. 顶层弹出菜单与模态对话框（Main Menu Dropdown / Git Branches Popup / Search Everywhere / Find in Files / New File / Settings）
        if let Some(cat) = self.active_menu_category {
            self.render_main_menu_dropdown(canvas, cat);
        }
        if self.git_branch_popup_open {
            self.render_git_branch_popup(canvas);
        }
        match self.focus {
            FocusTarget::SearchEverywhereModal => {
                self.render_search_everywhere_modal(canvas, w, h);
            }
            FocusTarget::FindInFilesModal => {
                self.render_find_in_files_modal(canvas, w, h);
            }
            FocusTarget::NewFileModal
            | FocusTarget::NewFolderModal
            | FocusTarget::NewGitBranchModal => {
                self.render_input_prompt_modal(canvas, w, h);
            }
            FocusTarget::SettingsModal => {
                self.render_settings_modal(canvas, w, h);
            }
            _ => {}
        }
    }

    fn render_main_toolbar(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        canvas.fill_rect(rect, t.main_toolbar_bg);
        canvas.fill_rect(
            Rect::new(rect.x, rect.y + rect.h - 1, rect.w, 1),
            t.border_color,
        );

        // 1. 左侧：三横杠主菜单按钮 (≡) + 可展开的 File | Edit | View | Navigate | Code | Refactor | Run | Git | Tools | Window | Help
        let ham_btn = Rect::new(8, 6, 28, 28);
        canvas.fill_rounded_rect(ham_btn, 6, t.panel_bg);
        canvas.draw_hamburger_icon(15, 14, t.text_primary);
        self.hit_regions
            .push((ham_btn, HitAction::ToggleHamburgerMenu));

        let mut cur_x = 40;
        if self.hamburger_menu_expanded {
            for &cat in MainMenuCategory::all() {
                let (tw, _) = canvas.measure_text(cat.label(), FontStyle::UiSmall);
                let item_w = tw + 12;
                let item_rect = Rect::new(cur_x, 7, item_w, 26);
                if self.active_menu_category == Some(cat) {
                    canvas.fill_rounded_rect(item_rect, 5, t.popup_hover_bg);
                }
                canvas.draw_text_centered(
                    item_rect,
                    cat.label(),
                    if self.active_menu_category == Some(cat) {
                        0xFFFFFF
                    } else {
                        t.text_secondary
                    },
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((item_rect, HitAction::OpenMenuCategory(cat)));
                cur_x += item_w + 2;
            }
            cur_x += 6;
        }

        // 2. 项目徽标 + VCS Git 分支小部件（点击弹出 IDEA Git Branches 菜单）
        let proj_box = Rect::new(cur_x, 6, 132, 28);
        canvas.stroke_rounded_rect(proj_box, 6, t.border_color, t.panel_bg);
        canvas.fill_rounded_rect(Rect::new(cur_x + 6, 10, 20, 20), 4, t.accent_blue);
        canvas.draw_text_centered(
            Rect::new(cur_x + 6, 10, 20, 20),
            "RS",
            0xFFFFFF,
            FontStyle::UiSmall,
        );
        canvas.draw_text_styled(
            cur_x + 31,
            11,
            "rust-ai-ide",
            t.text_primary,
            82,
            FontStyle::UiBold,
        );
        canvas.draw_chevron_down(cur_x + 116, 18, t.text_secondary);
        self.hit_regions.push((
            proj_box,
            HitAction::ExecuteIdeAction(IdeAction::OpenSettingsModal),
        ));
        cur_x += 138;

        let git_box = Rect::new(cur_x, 6, 108, 28);
        canvas.stroke_rounded_rect(
            git_box,
            6,
            if self.git_branch_popup_open {
                t.accent_blue
            } else {
                t.border_color
            },
            t.panel_bg,
        );
        canvas.draw_git_branch_icon(cur_x + 10, 13, t.accent_blue);
        canvas.draw_text_styled(
            cur_x + 26,
            11,
            &self.git_engine.current_branch,
            t.text_primary,
            60,
            FontStyle::UiBold,
        );
        canvas.draw_chevron_down(cur_x + 90, 18, t.text_secondary);
        self.hit_regions
            .push((git_box, HitAction::ToggleGitBranchPopup));
        cur_x += 116;

        // 3. 右侧：Run/Debug Configuration 选择器 + 运行 / 调试 / 停止 + 设置按钮
        let right_controls_w = 408;
        let right_start = (rect.w - right_controls_w).max(cur_x + 180);

        // 4. 中部：自适应 Search Everywhere 搜索框（带抗锯齿放大镜图标）
        let search_w = (right_start - cur_x - 12).clamp(160, 320);
        let search_box = Rect::new(cur_x, 6, search_w, 28);
        canvas.stroke_rounded_rect(search_box, 6, t.border_color, t.panel_bg);
        canvas.draw_search_icon(cur_x + 10, 12, t.text_secondary);
        canvas.draw_text_styled(
            cur_x + 30,
            11,
            "Search Everywhere (双击 Shift)",
            t.text_muted,
            search_w - 38,
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((search_box, HitAction::ToggleSearchEverywhere));

        // Run Configuration 选择器
        let cfg_box = Rect::new(right_start, 6, 188, 28);
        canvas.stroke_rounded_rect(cfg_box, 6, t.border_color, t.panel_bg);
        let cfg_name = self
            .run_configs
            .get(self.active_run_config)
            .map(|c| c.name.as_str())
            .unwrap_or("Cargo Check");
        canvas.draw_text_styled(
            right_start + 10,
            11,
            cfg_name,
            t.text_primary,
            156,
            FontStyle::UiSmall,
        );
        canvas.draw_chevron_down(right_start + 170, 18, t.text_secondary);
        self.hit_regions.push((cfg_box, HitAction::CycleRunConfig));

        // Run (Shift+F10)
        let run_btn = Rect::new(right_start + 194, 6, 62, 28);
        canvas.stroke_rounded_rect(run_btn, 6, t.run_green, t.panel_bg);
        canvas.fill_play_triangle(right_start + 204, 14, 11, t.run_green);
        canvas.draw_text_styled(
            right_start + 218,
            11,
            "运行",
            t.run_green,
            34,
            FontStyle::UiBold,
        );
        self.hit_regions.push((run_btn, HitAction::RunActiveConfig));

        // Debug (Shift+F9)
        let dbg_btn = Rect::new(right_start + 262, 6, 68, 28);
        canvas.stroke_rounded_rect(dbg_btn, 6, t.debug_orange, t.panel_bg);
        canvas.draw_debug_icon(right_start + 270, 13, t.debug_orange);
        canvas.draw_text_styled(
            right_start + 286,
            11,
            "调试",
            t.debug_orange,
            36,
            FontStyle::UiBold,
        );
        self.hit_regions
            .push((dbg_btn, HitAction::DebugActiveConfig));

        // Stop
        let stop_btn = Rect::new(right_start + 336, 6, 62, 28);
        canvas.stroke_rounded_rect(
            stop_btn,
            6,
            t.border_color,
            t.panel_bg,
        );
        canvas.draw_text_centered(
            stop_btn,
            "设置",
            t.text_secondary,
            FontStyle::UiRegular,
        );
        self.hit_regions.push((
            stop_btn,
            HitAction::ExecuteIdeAction(IdeAction::OpenSettingsModal),
        ));
    }

    fn render_left_stripe(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        canvas.fill_rect(rect, t.tool_stripe_bg);

        let items = [
            ("项目", ToolWindowId::Project, rect.y + 8),
            ("Git", ToolWindowId::Commit, rect.y + 48),
            ("结构", ToolWindowId::Structure, rect.y + 88),
        ];
        for (label, id, y) in items {
            let btn = Rect::new(rect.x + 4, y, 34, 32);
            let active = self.left_tool == Some(id);
            if active {
                canvas.fill_rounded_rect(btn, 6, t.panel_bg);
                canvas.fill_rounded_rect(Rect::new(rect.x + 1, y + 6, 3, 20), 1, t.accent_blue);
            }
            canvas.draw_text_centered(
                btn,
                label,
                if active { t.text_primary } else { t.text_muted },
                if active {
                    FontStyle::UiBold
                } else {
                    FontStyle::UiSmall
                },
            );
            self.hit_regions
                .push((btn, HitAction::ToggleLeftTool(id)));
        }

        // 左下角终端与问题诊断快捷图标
        let b_items = [
            ("终端", ToolWindowId::Terminal, rect.y + rect.h - 82),
            ("调试", ToolWindowId::Debug, rect.y + rect.h - 42),
        ];
        for (label, id, y) in b_items {
            let btn = Rect::new(rect.x + 4, y, 34, 32);
            let active = self.bottom_tool == Some(id);
            if active {
                canvas.fill_rounded_rect(btn, 6, t.panel_bg);
                canvas.fill_rounded_rect(Rect::new(rect.x + 1, y + 6, 3, 20), 1, t.accent_blue);
            }
            canvas.draw_text_centered(
                btn,
                label,
                if active { t.text_primary } else { t.text_muted },
                FontStyle::UiSmall,
            );
            self.hit_regions
                .push((btn, HitAction::SelectBottomTool(id)));
        }
    }

    fn render_right_stripe(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        canvas.fill_rect(rect, t.tool_stripe_bg);

        let btn = Rect::new(rect.x + 4, rect.y + 8, 34, 32);
        if self.right_agent_open {
            canvas.fill_rounded_rect(btn, 6, t.panel_bg);
            canvas.fill_rounded_rect(
                Rect::new(rect.x + rect.w - 4, rect.y + 14, 3, 20),
                1,
                t.accent_blue,
            );
        }
        canvas.draw_text_centered(btn, "AI", t.accent_blue, FontStyle::UiBold);
        self.hit_regions
            .push((btn, HitAction::ToggleRightAgentHub));

        // 主题一键切换按钮
        let theme_btn = Rect::new(rect.x + 4, rect.y + 48, 34, 32);
        canvas.fill_rounded_rect(theme_btn, 6, t.panel_bg);
        canvas.draw_text_centered(theme_btn, "主题", t.text_secondary, FontStyle::UiSmall);
        self.hit_regions.push((
            theme_btn,
            HitAction::ExecuteIdeAction(IdeAction::SwitchTheme),
        ));
    }

    fn render_left_tool_window(
        &mut self,
        canvas: &mut SurfaceCanvas,
        rect: Rect,
        tool: ToolWindowId,
    ) {
        let t = self.theme;
        // Islands UI 圆角独立岛屿卡片
        canvas.stroke_rounded_rect(rect, 8, t.border_color, t.panel_bg);

        let title = match tool {
            ToolWindowId::Project => "项目 Project",
            ToolWindowId::Commit => "Git 提交 Commit",
            ToolWindowId::Structure => "结构 Structure",
            _ => "Tool Window",
        };
        canvas.draw_text_styled(
            rect.x + 12,
            rect.y + 8,
            title,
            t.text_primary,
            130,
            FontStyle::UiBold,
        );

        match tool {
            ToolWindowId::Project => {
                // 顶部文件夹与文件操作工具栏：[+文件] [+目录] [展开] [折叠] [删除]
                let btn_y = rect.y + 6;
                let mut bx = rect.x + rect.w - 154;

                let new_file_btn = Rect::new(bx, btn_y, 36, 22);
                canvas.stroke_rounded_rect(new_file_btn, 4, t.border_color, t.window_bg);
                canvas.draw_text_centered(
                    new_file_btn,
                    "+文件",
                    t.syn_function,
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((new_file_btn, HitAction::PromptNewFile));
                bx += 40;

                let new_dir_btn = Rect::new(bx, btn_y, 36, 22);
                canvas.stroke_rounded_rect(new_dir_btn, 4, t.border_color, t.window_bg);
                canvas.draw_text_centered(
                    new_dir_btn,
                    "+目录",
                    t.run_green,
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((new_dir_btn, HitAction::PromptNewFolder));
                bx += 40;

                let exp_btn = Rect::new(bx, btn_y, 32, 22);
                canvas.stroke_rounded_rect(exp_btn, 4, t.border_color, t.window_bg);
                canvas.draw_text_centered(exp_btn, "展开", t.text_secondary, FontStyle::UiSmall);
                self.hit_regions.push((exp_btn, HitAction::ExpandAllDirs));
                bx += 36;

                let col_btn = Rect::new(bx, btn_y, 32, 22);
                canvas.stroke_rounded_rect(col_btn, 4, t.border_color, t.window_bg);
                canvas.draw_text_centered(col_btn, "折叠", t.text_secondary, FontStyle::UiSmall);
                self.hit_regions.push((col_btn, HitAction::CollapseAllDirs));

                canvas.fill_rect(
                    Rect::new(rect.x + 1, rect.y + 32, rect.w - 2, 1),
                    t.border_color,
                );

                let mut y = rect.y + 38;
                let entries: Vec<_> = self
                    .project_tree
                    .visible_entries
                    .iter()
                    .skip(self.tree_scroll)
                    .take(32)
                    .cloned()
                    .collect();

                for entry in entries {
                    if y + 22 > rect.y + rect.h - 4 {
                        break;
                    }
                    let row_rect = Rect::new(rect.x + 6, y, rect.w - 12, 22);
                    let is_active = (!entry.is_dir && self.active_buffer().path == entry.path)
                        || (self.project_tree.selected_path.as_ref() == Some(&entry.path));
                    if is_active {
                        canvas.fill_rounded_rect(row_rect, 5, t.editor_selection_bg);
                    }
                    let indent = (entry.depth as i32) * 14;
                    let base_x = rect.x + 12 + indent;
                    let text_x = if entry.is_dir {
                        if entry.expanded {
                            canvas.draw_chevron_down(base_x, y + 9, t.text_secondary);
                        } else {
                            canvas.draw_chevron_right(base_x + 2, y + 7, t.text_secondary);
                        }
                        canvas.draw_folder_icon(base_x + 12, y + 4, 0xD9A343);
                        base_x + 30
                    } else {
                        let (badge, badge_color) = match entry.language {
                            ide_core::LanguageId::Rust => ("rs", t.syn_keyword),
                            ide_core::LanguageId::TypeScript | ide_core::LanguageId::JavaScript => {
                                ("ts", t.syn_function)
                            }
                            ide_core::LanguageId::Python => ("py", t.warning_yellow),
                            ide_core::LanguageId::Toml => ("toml", t.syn_type),
                            ide_core::LanguageId::Markdown => ("md", t.syn_field_or_property),
                            _ => ("file", t.text_muted),
                        };
                        canvas.draw_text_styled(
                            base_x + 8,
                            y + 3,
                            badge,
                            badge_color,
                            30,
                            FontStyle::UiSmall,
                        ) + 6
                    };

                    let color = if is_active {
                        0xFFFFFF
                    } else if entry.is_dir {
                        t.text_primary
                    } else {
                        t.text_secondary
                    };
                    canvas.draw_text_styled(
                        text_x,
                        y + 2,
                        &entry.name,
                        color,
                        rect.x + rect.w - text_x - 8,
                        if is_active {
                            FontStyle::UiBold
                        } else {
                            FontStyle::UiRegular
                        },
                    );
                    if entry.is_dir {
                        self.hit_regions
                            .push((row_rect, HitAction::ToggleDirectory(entry.path)));
                    } else {
                        self.hit_regions
                            .push((row_rect, HitAction::OpenFile(entry.path)));
                    }
                    y += 22;
                }
            }
            ToolWindowId::Commit => {
                canvas.fill_rect(
                    Rect::new(rect.x + 1, rect.y + 32, rect.w - 2, 1),
                    t.border_color,
                );
                let mut y = rect.y + 38;

                // 分支与快捷 Pull / Push / Stage All 按钮
                let pull_btn = Rect::new(rect.x + 8, y, 58, 24);
                canvas.stroke_rounded_rect(pull_btn, 5, t.border_color, t.window_bg);
                canvas.draw_text_centered(pull_btn, "↓ Pull", t.syn_function, FontStyle::UiSmall);
                self.hit_regions.push((
                    pull_btn,
                    HitAction::ExecuteIdeAction(IdeAction::GitPull),
                ));

                let push_btn = Rect::new(rect.x + 72, y, 58, 24);
                canvas.stroke_rounded_rect(push_btn, 5, t.border_color, t.window_bg);
                canvas.draw_text_centered(push_btn, "↑ Push", t.run_green, FontStyle::UiSmall);
                self.hit_regions.push((
                    push_btn,
                    HitAction::ExecuteIdeAction(IdeAction::GitPush),
                ));

                let stage_btn = Rect::new(rect.x + 136, y, 68, 24);
                canvas.stroke_rounded_rect(stage_btn, 5, t.border_color, t.window_bg);
                canvas.draw_text_centered(stage_btn, "+ 全暂存", t.text_primary, FontStyle::UiSmall);
                self.hit_regions
                    .push((stage_btn, HitAction::StageAllGitChanges));

                let ai_msg_btn = Rect::new(rect.x + 210, y, (rect.w - 218).max(48), 24);
                canvas.stroke_rounded_rect(ai_msg_btn, 5, t.accent_blue, t.window_bg);
                canvas.draw_text_centered(ai_msg_btn, "AI 写提交", t.accent_blue, FontStyle::UiSmall);
                self.hit_regions
                    .push((ai_msg_btn, HitAction::GenerateAiCommitMessage));
                y += 30;

                canvas.draw_text_styled(
                    rect.x + 10,
                    y,
                    &format!("Changes ({})", self.git_engine.changed_files.len()),
                    t.text_secondary,
                    rect.w - 20,
                    FontStyle::UiBold,
                );
                y += 22;

                let changed_snapshot = self.git_engine.changed_files.clone();
                if changed_snapshot.is_empty() {
                    canvas.draw_text_styled(
                        rect.x + 12,
                        y,
                        "工作区干净 (修改任意文件后自动显示)",
                        t.text_muted,
                        rect.w - 24,
                        FontStyle::UiSmall,
                    );
                    y += 24;
                } else {
                    for ch in changed_snapshot.iter().take(10) {
                        if y + 22 > rect.y + rect.h - 100 {
                            break;
                        }
                        let row = Rect::new(rect.x + 8, y, rect.w - 56, 20);
                        let chk = if ch.staged { "[✓]" } else { "[ ]" };
                        let fname = ch
                            .path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("file");
                        let status_color = match ch.status {
                            ide_git::GitFileStatusKind::Added
                            | ide_git::GitFileStatusKind::Untracked => t.run_green,
                            ide_git::GitFileStatusKind::Deleted => t.breakpoint_red,
                            ide_git::GitFileStatusKind::Modified => t.syn_function,
                        };
                        canvas.draw_text_styled(
                            rect.x + 10,
                            y + 2,
                            &format!("{chk} {fname}"),
                            status_color,
                            rect.w - 64,
                            FontStyle::UiSmall,
                        );
                        self.hit_regions
                            .push((row, HitAction::ToggleStageFile(ch.path.clone())));

                        let rb_btn = Rect::new(rect.x + rect.w - 44, y, 36, 20);
                        canvas.stroke_rounded_rect(rb_btn, 4, t.border_color, t.window_bg);
                        canvas.draw_text_centered(
                            rb_btn,
                            "回滚",
                            t.breakpoint_red,
                            FontStyle::UiSmall,
                        );
                        self.hit_regions
                            .push((rb_btn, HitAction::RollbackGitFile(ch.path.clone())));
                        y += 22;
                    }
                }

                // 底部 Commit Message 输入框与 Commit 按钮
                let commit_box_y = (rect.y + rect.h - 86).max(y + 4);
                let msg_box = Rect::new(rect.x + 8, commit_box_y, rect.w - 16, 44);
                canvas.stroke_rounded_rect(
                    msg_box,
                    5,
                    if self.focus == FocusTarget::GitCommitInput {
                        t.accent_blue
                    } else {
                        t.border_color
                    },
                    t.window_bg,
                );
                canvas.draw_text_styled(
                    msg_box.x + 8,
                    msg_box.y + 6,
                    &self.git_engine.commit_message_draft,
                    t.text_primary,
                    msg_box.w - 16,
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((msg_box, HitAction::FocusGitCommitMessageInput));

                let commit_btn = Rect::new(rect.x + 8, commit_box_y + 50, rect.w - 16, 28);
                canvas.fill_rounded_rect(commit_btn, 6, t.accent_blue);
                canvas.draw_text_centered(
                    commit_btn,
                    "提交变更 Commit (Ctrl+Enter)",
                    0xFFFFFF,
                    FontStyle::UiBold,
                );
                self.hit_regions
                    .push((commit_btn, HitAction::CommitAllGitChanges));
            }
            ToolWindowId::Structure => {
                canvas.fill_rect(
                    Rect::new(rect.x + 1, rect.y + 32, rect.w - 2, 1),
                    t.border_color,
                );
                let mut y = rect.y + 38;
                let buf = self.active_buffer();
                let symbols: Vec<StructureSymbol> =
                    extract_structure_symbols(&buf.text(), buf.language);
                for sym in symbols {
                    if y + 22 > rect.y + rect.h - 6 {
                        break;
                    }
                    let row = Rect::new(rect.x + 6, y, rect.w - 12, 22);
                    let label = format!(
                        "[{}]  {}   :{}",
                        sym.kind.icon_badge(),
                        sym.name,
                        sym.line + 1
                    );
                    canvas.draw_text_styled(
                        rect.x + 12,
                        y + 2,
                        &label,
                        t.syn_function,
                        rect.w - 24,
                        FontStyle::CodeMono,
                    );
                    self.hit_regions
                        .push((row, HitAction::JumpToLine(sym.line)));
                    y += 22;
                }
            }
            _ => {}
        }
    }

    fn render_center_editor(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        canvas.stroke_rounded_rect(rect, 8, t.border_color, t.editor_bg);

        // 1. 顶部多文件 Editor Tabs 栏（带关闭按钮 `×` 与同名文件自动区分）
        let tab_bar = Rect::new(rect.x + 1, rect.y + 1, rect.w - 2, 34);
        canvas.fill_rounded_rect(tab_bar, 7, t.tab_inactive_bg);
        canvas.fill_rect(
            Rect::new(rect.x + 1, rect.y + 34, rect.w - 2, 1),
            t.border_color,
        );

        let mut tab_x = rect.x + 4;
        let open_snapshot: Vec<(PathBuf, bool)> = self
            .open_buffers
            .iter()
            .map(|b| (b.path.clone(), b.dirty))
            .collect();

        for (idx, (path, dirty)) in open_snapshot.iter().enumerate() {
            let fname = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("untitled");
            let duplicate_count = open_snapshot
                .iter()
                .filter(|(p, _)| p.file_name() == path.file_name())
                .count();

            let disambiguated = if duplicate_count > 1 {
                let crate_dir = path
                    .parent()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                if !crate_dir.is_empty() {
                    format!("{crate_dir}/{fname}")
                } else {
                    fname.to_string()
                }
            } else {
                fname.to_string()
            };

            let label = if *dirty {
                format!("● {disambiguated}")
            } else {
                disambiguated
            };

            let is_active = idx == self.active_tab;
            let font_style = if is_active {
                FontStyle::UiBold
            } else {
                FontStyle::UiRegular
            };
            let (tw, _) = canvas.measure_text(&label, font_style);
            let tab_w = (tw + 44).clamp(110, 240);
            if tab_x + tab_w > rect.x + rect.w - 265 {
                break;
            }
            let tab_rect = Rect::new(tab_x, rect.y + 3, tab_w, 30);

            if is_active {
                canvas.fill_rounded_rect(tab_rect, 6, t.tab_active_bg);
                canvas.fill_rounded_rect(
                    Rect::new(tab_x + 10, rect.y + 31, tab_w - 20, 3),
                    1,
                    t.tab_underline_accent,
                );
            }
            canvas.draw_text_styled(
                tab_x + 12,
                rect.y + 9,
                &label,
                if is_active {
                    t.text_primary
                } else {
                    t.text_secondary
                },
                tab_w - 30,
                font_style,
            );
            self.hit_regions
                .push((tab_rect, HitAction::SelectTab(idx)));

            // 标签页右侧关闭按钮 `×`
            if open_snapshot.len() > 1 {
                let close_rect = Rect::new(tab_x + tab_w - 22, rect.y + 9, 16, 16);
                canvas.draw_text_centered(
                    close_rect,
                    "×",
                    t.text_muted,
                    FontStyle::UiRegular,
                );
                self.hit_regions
                    .push((close_rect, HitAction::CloseTab(idx)));
            }
            tab_x += tab_w + 2;
        }

        // 右上角 Cursor 风格 `Ctrl+K 行内编辑` 与 `Alt+L 发送至AI` 按钮
        let send_btn_w = 118;
        let ctrl_k_btn_w = 136;
        let ctrl_k_btn = Rect::new(
            rect.x + rect.w - send_btn_w - ctrl_k_btn_w - 16,
            rect.y + 5,
            ctrl_k_btn_w,
            24,
        );
        canvas.stroke_rounded_rect(ctrl_k_btn, 5, t.accent_blue, t.panel_bg);
        canvas.draw_text_centered(
            ctrl_k_btn,
            "Ctrl+K 行内编辑",
            t.accent_blue,
            FontStyle::UiBold,
        );
        self.hit_regions
            .push((ctrl_k_btn, HitAction::TriggerCtrlKInlineEdit));

        let send_btn = Rect::new(
            rect.x + rect.w - send_btn_w - 8,
            rect.y + 5,
            send_btn_w,
            24,
        );
        canvas.stroke_rounded_rect(send_btn, 5, t.border_color, t.panel_bg);
        canvas.draw_text_centered(
            send_btn,
            "Alt+L 发送至AI",
            t.text_primary,
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((send_btn, HitAction::SendSelectionToAgent));

        // 2. Breadcrumbs 面包屑导航栏
        let crumb_bar = Rect::new(rect.x + 1, rect.y + 35, rect.w - 2, 24);
        canvas.fill_rect(crumb_bar, t.editor_bg);
        canvas.fill_rect(
            Rect::new(rect.x + 1, rect.y + 58, rect.w - 2, 1),
            t.border_color,
        );
        let active_path_str = self
            .active_buffer()
            .path
            .strip_prefix(&self.workspace_root)
            .unwrap_or(&self.active_buffer().path)
            .to_string_lossy()
            .replace('\\', "  ›  ");
        canvas.draw_text_styled(
            rect.x + 14,
            rect.y + 39,
            &format!(
                "{}   |   Ctrl+D 复制行 · Ctrl+Y 删行 · Ctrl+/ 注释 · Ctrl+Alt+L 格式化",
                active_path_str
            ),
            t.text_muted,
            rect.w - 28,
            FontStyle::UiSmall,
        );

        // 3. Cursor `Ctrl+K` 行内编辑悬浮审批窗（激活时显示）
        let mut code_top_y = rect.y + 60;
        if let Some(inline_session) = self.agent_hub.inline_edit.clone() {
            if inline_session.active {
                let banner_h = 122;
                let banner = Rect::new(rect.x + 66, code_top_y + 4, rect.w - 78, banner_h);
                canvas.stroke_rounded_rect(banner, 6, t.accent_blue, t.panel_bg);

                let accept_w = 110;
                let reject_w = 104;
                let btns_total = accept_w + reject_w + 24;

                canvas.draw_text_styled(
                    banner.x + 12,
                    banner.y + 7,
                    &format!(
                        "Cursor Ctrl+K 行内修改 [{}] — {}",
                        self.agent_hub.active_provider.display_name(),
                        inline_session.prompt
                    ),
                    t.syn_function,
                    banner.w - btns_total - 16,
                    FontStyle::UiBold,
                );

                let accept_btn = Rect::new(
                    banner.x + banner.w - accept_w - reject_w - 16,
                    banner.y + 5,
                    accept_w,
                    24,
                );
                canvas.stroke_rounded_rect(accept_btn, 5, t.run_green, t.diff_added_bg);
                canvas.draw_text_centered(
                    accept_btn,
                    "✓ 接受 (Tab)",
                    t.diff_added_fg,
                    FontStyle::UiBold,
                );
                self.hit_regions
                    .push((accept_btn, HitAction::AcceptInlineDiff));

                let reject_btn = Rect::new(
                    banner.x + banner.w - reject_w - 8,
                    banner.y + 5,
                    reject_w,
                    24,
                );
                canvas.stroke_rounded_rect(reject_btn, 5, t.breakpoint_red, t.diff_removed_bg);
                canvas.draw_text_centered(
                    reject_btn,
                    "✕ 拒绝 (Esc)",
                    t.diff_removed_fg,
                    FontStyle::UiBold,
                );
                self.hit_regions
                    .push((reject_btn, HitAction::RejectInlineDiff));

                let mut diff_y = banner.y + 34;
                for dline in inline_session.diff_lines.iter().take(4) {
                    let (row_bg, fg, sign) = match dline.kind {
                        DiffLineKind::Added => (t.diff_added_bg, t.diff_added_fg, "+ "),
                        DiffLineKind::Removed => (t.diff_removed_bg, t.diff_removed_fg, "- "),
                        DiffLineKind::Context => (t.panel_bg, t.text_secondary, "  "),
                    };
                    canvas.fill_rounded_rect(
                        Rect::new(banner.x + 8, diff_y, banner.w - 16, 20),
                        3,
                        row_bg,
                    );
                    canvas.draw_text_styled(
                        banner.x + 14,
                        diff_y + 1,
                        &format!("{}{}", sign, dline.content),
                        fg,
                        banner.w - 28,
                        FontStyle::CodeMono,
                    );
                    diff_y += 21;
                }

                code_top_y += banner_h + 10;
            }
        }

        // 4. 代码编辑器 Gutter + Cascadia Mono 等宽语法高亮画布
        let gutter_w = 62;
        canvas.fill_rect(
            Rect::new(
                rect.x + 1,
                code_top_y,
                gutter_w,
                rect.y + rect.h - code_top_y - 2,
            ),
            t.editor_gutter_bg,
        );
        canvas.fill_rect(
            Rect::new(
                rect.x + gutter_w,
                code_top_y,
                1,
                rect.y + rect.h - code_top_y - 2,
            ),
            t.border_color,
        );

        let buf = self.active_buffer().clone();
        let runnables: Vec<RunnableGutterEntry> =
            detect_runnable_gutter_entries(&buf.text(), buf.language);

        let line_h = 22;
        let max_visible_lines = ((rect.y + rect.h - code_top_y - 4) / line_h).max(1) as usize;

        for vis_idx in 0..max_visible_lines {
            let line_idx = self.scroll_line + vis_idx;
            if line_idx >= buf.line_count() {
                break;
            }
            let row_y = code_top_y + (vis_idx as i32) * line_h;

            if line_idx == buf.cursor_line {
                canvas.fill_rect(
                    Rect::new(
                        rect.x + gutter_w + 1,
                        row_y,
                        rect.w - gutter_w - 3,
                        line_h,
                    ),
                    t.editor_current_line_bg,
                );
            }

            let gutter_row = Rect::new(rect.x + 1, row_y, gutter_w, line_h);
            self.hit_regions
                .push((gutter_row, HitAction::ToggleBreakpointAtLine(line_idx)));

            let lineno_str = format!("{:>3}", line_idx + 1);
            canvas.draw_text_styled(
                rect.x + 8,
                row_y + 2,
                &lineno_str,
                if line_idx == buf.cursor_line {
                    t.text_primary
                } else {
                    t.text_muted
                },
                32,
                FontStyle::CodeMono,
            );

            if self.dap_engine.has_breakpoint(&buf.path, line_idx) {
                canvas.fill_circle(rect.x + 44, row_y + 11, 5, t.breakpoint_red);
            }

            if runnables.iter().any(|r| r.line == line_idx) {
                let play_rect = Rect::new(rect.x + 48, row_y + 4, 14, 14);
                canvas.fill_play_triangle(rect.x + 50, row_y + 6, 10, t.run_green);
                self.hit_regions
                    .push((play_rect, HitAction::RunGutterEntry(line_idx)));
            }

            let code_row = Rect::new(rect.x + gutter_w + 1, row_y, rect.w - gutter_w - 3, line_h);
            self.hit_regions
                .push((code_row, HitAction::JumpToLine(line_idx)));

            let line_str = buf.line_text(line_idx);
            let spans = highlight_line(&line_str, buf.language);
            let mut code_x = rect.x + gutter_w + 14;
            let right_limit = rect.x + rect.w - 10;
            for span in spans {
                if code_x >= right_limit {
                    break;
                }
                let font_style = if span.kind == TokenKind::Keyword {
                    FontStyle::CodeMonoBold
                } else {
                    FontStyle::CodeMono
                };
                code_x = canvas.draw_text_styled(
                    code_x,
                    row_y + 2,
                    &span.text,
                    span.kind.color(&t),
                    right_limit - code_x,
                    font_style,
                );
            }
        }
    }

    fn render_right_agent_hub(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        canvas.stroke_rounded_rect(rect, 8, t.border_color, t.panel_bg);

        canvas.draw_text_styled(
            rect.x + 12,
            rect.y + 8,
            "Multi-Agent 协同中枢 (ACP · CLI · Cursor)",
            t.text_primary,
            rect.w - 24,
            FontStyle::UiBold,
        );
        canvas.fill_rect(
            Rect::new(rect.x + 1, rect.y + 32, rect.w - 2, 1),
            t.border_color,
        );

        let mut tab_y = rect.y + 38;
        for reg in self.agent_hub.providers.clone() {
            let row = Rect::new(rect.x + 8, tab_y, rect.w - 16, 28);
            let is_selected = reg.kind == self.agent_hub.active_provider;
            canvas.stroke_rounded_rect(
                row,
                6,
                if is_selected {
                    t.accent_blue
                } else {
                    t.border_color
                },
                if is_selected {
                    t.popup_hover_bg
                } else {
                    t.editor_bg
                },
            );
            let dot_color = if reg.available {
                t.run_green
            } else {
                t.warning_yellow
            };
            canvas.fill_circle(row.x + 13, row.y + 14, 4, dot_color);

            canvas.draw_text_styled(
                row.x + 25,
                row.y + 5,
                reg.kind.display_name(),
                if is_selected {
                    0xFFFFFF
                } else {
                    t.text_primary
                },
                220,
                if is_selected {
                    FontStyle::UiBold
                } else {
                    FontStyle::UiRegular
                },
            );

            let badge = reg.kind.protocol_badge();
            let (bw, _) = canvas.measure_text(badge, FontStyle::UiSmall);
            canvas.draw_text_styled(
                row.x + row.w - bw - 10,
                row.y + 7,
                badge,
                if is_selected {
                    0xDFE1E5
                } else {
                    t.text_muted
                },
                bw + 4,
                FontStyle::UiSmall,
            );

            self.hit_regions
                .push((row, HitAction::SelectAgentProvider(reg.kind)));
            tab_y += 31;
        }

        // 快捷动作按钮栏
        let action_y = tab_y + 2;
        let btn_w = (rect.w - 24) / 3;

        let b1 = Rect::new(rect.x + 8, action_y, btn_w, 26);
        canvas.stroke_rounded_rect(b1, 5, t.accent_blue, t.window_bg);
        canvas.draw_text_centered(b1, "Ctrl+K 改写", t.syn_function, FontStyle::UiBold);
        self.hit_regions
            .push((b1, HitAction::TriggerCtrlKInlineEdit));

        let b2 = Rect::new(rect.x + 12 + btn_w, action_y, btn_w, 26);
        canvas.stroke_rounded_rect(b2, 5, t.border_color, t.window_bg);
        canvas.draw_text_centered(b2, "投喂选区 (Alt+L)", t.text_primary, FontStyle::UiSmall);
        self.hit_regions
            .push((b2, HitAction::SendSelectionToAgent));

        let b3 = Rect::new(rect.x + 16 + btn_w * 2, action_y, btn_w, 26);
        canvas.stroke_rounded_rect(b3, 5, t.warning_yellow, t.window_bg);
        canvas.draw_text_centered(
            b3,
            &format!("快照回滚 ({})", self.agent_hub.checkpoints.len()),
            t.warning_yellow,
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((b3, HitAction::RevertLastCheckpoint));

        // 对话消息卡片区
        let chat_top = action_y + 32;
        let input_area_h = 38;
        let chat_box = Rect::new(
            rect.x + 8,
            chat_top,
            rect.w - 16,
            (rect.y + rect.h - chat_top - input_area_h - 10).max(40),
        );
        canvas.stroke_rounded_rect(chat_box, 6, t.border_color, t.editor_bg);

        let mut msg_y = chat_box.y + 8;
        let wrap_w = chat_box.w - 24;
        let messages_snapshot = self.agent_hub.messages.clone();
        for msg in messages_snapshot.iter().rev().take(3).rev() {
            if msg_y + 18 > chat_box.y + chat_box.h {
                break;
            }
            let header_color = if msg.role == "user" {
                t.syn_keyword
            } else {
                t.syn_function
            };
            canvas.draw_text_styled(
                chat_box.x + 10,
                msg_y,
                &format!("● {} — {}", msg.role.to_uppercase(), msg.agent.display_name()),
                header_color,
                wrap_w,
                FontStyle::UiBold,
            );
            msg_y += 20;

            let wrapped_lines =
                canvas.wrap_text_by_pixel_width(&msg.content, wrap_w, FontStyle::UiSmall);
            for line_str in wrapped_lines {
                if msg_y + 17 > chat_box.y + chat_box.h {
                    break;
                }
                canvas.draw_text_styled(
                    chat_box.x + 12,
                    msg_y,
                    &line_str,
                    t.text_primary,
                    wrap_w,
                    FontStyle::UiSmall,
                );
                msg_y += 17;
            }
            msg_y += 8;
        }

        // 底部可交互 Agent 对话输入框 + 发送按钮
        let input_y = rect.y + rect.h - input_area_h - 4;
        let send_w = 58;
        let input_box = Rect::new(rect.x + 8, input_y, rect.w - send_w - 22, 30);
        let is_focused = self.focus == FocusTarget::AgentChatInput;
        canvas.stroke_rounded_rect(
            input_box,
            6,
            if is_focused {
                t.accent_blue
            } else {
                t.border_color
            },
            t.editor_bg,
        );
        let placeholder = if self.agent_chat_input.is_empty() {
            if is_focused {
                "输入给智能体的指令..._"
            } else {
                "点击此处向 Claude / Codex / Antigravity 发送指令..."
            }
        } else {
            &self.agent_chat_input
        };
        canvas.draw_text_styled(
            input_box.x + 10,
            input_box.y + 6,
            placeholder,
            if self.agent_chat_input.is_empty() && !is_focused {
                t.text_muted
            } else {
                t.text_primary
            },
            input_box.w - 18,
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((input_box, HitAction::FocusAgentChatInput));

        let submit_btn = Rect::new(rect.x + rect.w - send_w - 8, input_y, send_w, 30);
        canvas.fill_rounded_rect(submit_btn, 6, t.accent_blue);
        canvas.draw_text_centered(submit_btn, "发送", 0xFFFFFF, FontStyle::UiBold);
        self.hit_regions
            .push((submit_btn, HitAction::SubmitAgentChatPrompt));
    }

    fn render_bottom_tool_window(
        &mut self,
        canvas: &mut SurfaceCanvas,
        rect: Rect,
        active_tool: ToolWindowId,
    ) {
        let t = self.theme;
        canvas.stroke_rounded_rect(rect, 8, t.border_color, t.panel_bg);

        let tabs = [
            ("终端 Terminal (Alt+F12)", ToolWindowId::Terminal),
            ("问题诊断 Problems (Alt+6)", ToolWindowId::Problems),
            ("调试器 Debug (Alt+5)", ToolWindowId::Debug),
            ("版本日志 Git Log (Alt+9)", ToolWindowId::GitLog),
        ];

        let mut tx = rect.x + 10;
        for (title, id) in tabs {
            let is_sel = id == active_tool;
            let style = if is_sel {
                FontStyle::UiBold
            } else {
                FontStyle::UiRegular
            };
            let (tw_measured, _) = canvas.measure_text(title, style);
            let tw = tw_measured + 26;
            let trect = Rect::new(tx, rect.y + 2, tw, 28);
            if is_sel {
                canvas.fill_rounded_rect(trect, 5, t.editor_bg);
                canvas.fill_rounded_rect(Rect::new(tx + 8, rect.y + 27, tw - 16, 3), 1, t.accent_blue);
            }
            canvas.draw_text_centered(
                trect,
                title,
                if is_sel {
                    t.text_primary
                } else {
                    t.text_secondary
                },
                style,
            );
            self.hit_regions
                .push((trect, HitAction::SelectBottomTool(id)));
            tx += tw + 4;
        }

        canvas.fill_rect(
            Rect::new(rect.x + 1, rect.y + 31, rect.w - 2, 1),
            t.border_color,
        );

        let content_rect = Rect::new(rect.x + 2, rect.y + 32, rect.w - 4, rect.h - 34);
        canvas.fill_rounded_rect(content_rect, 6, t.editor_bg);

        let mut y = content_rect.y + 8;
        match active_tool {
            ToolWindowId::Terminal => {
                // 历史输出
                let max_lines = ((content_rect.h - 42) / 19).max(2) as usize;
                for line in self
                    .terminal
                    .history_lines
                    .iter()
                    .rev()
                    .take(max_lines)
                    .rev()
                {
                    canvas.draw_text_styled(
                        content_rect.x + 12,
                        y,
                        line,
                        t.text_primary,
                        content_rect.w - 24,
                        FontStyle::CodeMono,
                    );
                    y += 19;
                }

                // 底部可交互命令输入行
                let cmd_y = content_rect.y + content_rect.h - 30;
                let cmd_box = Rect::new(content_rect.x + 8, cmd_y, content_rect.w - 96, 25);
                let term_focused = self.focus == FocusTarget::TerminalInput;
                canvas.stroke_rounded_rect(
                    cmd_box,
                    5,
                    if term_focused {
                        t.accent_blue
                    } else {
                        t.border_color
                    },
                    t.panel_bg,
                );
                let prompt_line = format!(
                    "PS {}> {}{}",
                    self.workspace_root.display(),
                    self.terminal.input_buffer,
                    if term_focused { "_" } else { "  (点击输入命令并回车执行)" }
                );
                canvas.draw_text_styled(
                    cmd_box.x + 8,
                    cmd_box.y + 3,
                    &prompt_line,
                    if term_focused {
                        t.text_primary
                    } else {
                        t.text_secondary
                    },
                    cmd_box.w - 16,
                    FontStyle::CodeMono,
                );
                self.hit_regions
                    .push((cmd_box, HitAction::FocusTerminalInput));

                let exec_btn = Rect::new(
                    content_rect.x + content_rect.w - 82,
                    cmd_y,
                    74,
                    25,
                );
                canvas.fill_rounded_rect(exec_btn, 5, t.run_green);
                canvas.draw_text_centered(exec_btn, "执行 ⏎", 0xFFFFFF, FontStyle::UiBold);
                self.hit_regions
                    .push((exec_btn, HitAction::SubmitTerminalCommand));
            }
            ToolWindowId::Problems => {
                for diag in &self.lsp_hub.diagnostics {
                    if y + 20 > content_rect.y + content_rect.h {
                        break;
                    }
                    canvas.draw_text_styled(
                        content_rect.x + 14,
                        y,
                        &format!(
                            "[{:?}] {}:{} — {}",
                            diag.severity,
                            diag.file_path.display(),
                            diag.line,
                            diag.message
                        ),
                        t.warning_yellow,
                        content_rect.w - 28,
                        FontStyle::CodeMono,
                    );
                    y += 21;
                }
                for srv in &self.lsp_hub.servers {
                    if y + 20 > content_rect.y + content_rect.h {
                        break;
                    }
                    canvas.draw_text_styled(
                        content_rect.x + 14,
                        y,
                        &format!(
                            "LSP [{}] -> {} ({})",
                            srv.language.display_name(),
                            srv.binary_name,
                            srv.state_label
                        ),
                        t.syn_type,
                        content_rect.w - 28,
                        FontStyle::CodeMono,
                    );
                    y += 20;
                }
            }
            ToolWindowId::Debug => {
                let summary = self.dap_engine.export_ai_debug_context();
                for line in summary.lines() {
                    if y + 20 > content_rect.y + content_rect.h {
                        break;
                    }
                    canvas.draw_text_styled(
                        content_rect.x + 14,
                        y,
                        line,
                        t.debug_orange,
                        content_rect.w - 28,
                        FontStyle::CodeMono,
                    );
                    y += 20;
                }
            }
            ToolWindowId::GitLog => {
                if self.git_engine.recent_commits.is_empty() {
                    canvas.draw_text_styled(
                        content_rect.x + 14,
                        y,
                        "当前仓库暂无提交记录 — 可点击左侧 [Git] 面板一键提交代码",
                        t.text_muted,
                        content_rect.w - 28,
                        FontStyle::UiRegular,
                    );
                } else {
                    for c in &self.git_engine.recent_commits {
                        if y + 20 > content_rect.y + content_rect.h {
                            break;
                        }
                        canvas.draw_text_styled(
                            content_rect.x + 14,
                            y,
                            &format!("{}  |  {}  |  {}  |  {}", c.hash, c.date, c.author, c.subject),
                            t.text_primary,
                            content_rect.w - 28,
                            FontStyle::CodeMono,
                        );
                        y += 20;
                    }
                }
            }
            _ => {}
        }
    }

    fn render_status_bar(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        canvas.fill_rect(rect, t.status_bar_bg);
        canvas.fill_rect(Rect::new(rect.x, rect.y, rect.w, 1), t.border_color);

        let buf = self.active_buffer();
        let right_info = format!(
            "Ln {}, Col {}   |   UTF-8   |   {}   |   {}",
            buf.cursor_line + 1,
            buf.cursor_col + 1,
            buf.language.display_name(),
            t.kind.display_name()
        );
        let (right_w, _) = canvas.measure_text(&right_info, FontStyle::UiSmall);
        let right_x = (rect.w - right_w - 16).max(rect.w / 2);

        canvas.draw_git_branch_icon(rect.x + 12, rect.y + 7, t.accent_blue);
        let lsp_status = self.lsp_hub.status_summary(buf.language);
        let left_info = format!(
            "{}   |   {}   |   {}",
            self.git_engine.current_branch, lsp_status, self.status_notification
        );
        canvas.draw_text_styled(
            rect.x + 28,
            rect.y + 5,
            &left_info,
            t.text_secondary,
            right_x - rect.x - 40,
            FontStyle::UiSmall,
        );

        canvas.draw_text_styled(
            right_x,
            rect.y + 5,
            &right_info,
            t.text_primary,
            right_w + 8,
            FontStyle::UiSmall,
        );
    }

    fn render_main_menu_dropdown(&mut self, canvas: &mut SurfaceCanvas, cat: MainMenuCategory) {
        let t = self.theme;
        let items = cat.items();
        let menu_w = 385;
        let menu_h = (items.len() as i32) * 28 + 12;
        let menu_x = 40;
        let menu_y = 38;
        let box_rect = Rect::new(menu_x, menu_y, menu_w, menu_h);

        canvas.stroke_rounded_rect(box_rect, 7, t.border_color, t.popup_bg);

        let mut iy = menu_y + 6;
        for act in items {
            let row = Rect::new(menu_x + 6, iy, menu_w - 12, 26);
            canvas.fill_rounded_rect(row, 4, t.popup_bg);
            canvas.draw_text_styled(
                row.x + 10,
                iy + 5,
                act.title(),
                t.text_primary,
                menu_w - 120,
                FontStyle::UiSmall,
            );
            let sc = act.shortcut_label();
            let (sw, _) = canvas.measure_text(sc, FontStyle::UiSmall);
            canvas.draw_text_styled(
                row.x + row.w - sw - 8,
                iy + 5,
                sc,
                t.text_muted,
                sw + 4,
                FontStyle::UiSmall,
            );
            self.hit_regions
                .push((row, HitAction::ExecuteIdeAction(act)));
            iy += 28;
        }
    }

    fn render_git_branch_popup(&mut self, canvas: &mut SurfaceCanvas) {
        let t = self.theme;
        let popup_w = 290;
        let branches = self.git_engine.local_branches.clone();
        let popup_h = (branches.len() as i32) * 26 + 148;
        let popup_x = if self.hamburger_menu_expanded { 680 } else { 184 };
        let popup_rect = Rect::new(popup_x, 38, popup_w, popup_h);

        canvas.stroke_rounded_rect(popup_rect, 8, t.accent_blue, t.popup_bg);
        canvas.draw_text_styled(
            popup_rect.x + 12,
            popup_rect.y + 8,
            "Git Branches & VCS Actions",
            t.text_primary,
            popup_w - 24,
            FontStyle::UiBold,
        );

        let mut y = popup_rect.y + 32;
        let actions = [
            ("+ New Branch...", HitAction::CreateNewGitBranchPrompt),
            (
                "Commit Changes... (Alt+0)",
                HitAction::ExecuteIdeAction(IdeAction::GitCommitPopup),
            ),
            (
                "Update Project / Pull (Ctrl+T)",
                HitAction::ExecuteIdeAction(IdeAction::GitPull),
            ),
            (
                "Push Commits... (Ctrl+Shift+K)",
                HitAction::ExecuteIdeAction(IdeAction::GitPush),
            ),
        ];
        for (label, act) in actions {
            let row = Rect::new(popup_rect.x + 6, y, popup_w - 12, 24);
            canvas.fill_rounded_rect(row, 4, t.window_bg);
            canvas.draw_text_styled(
                row.x + 10,
                y + 4,
                label,
                t.syn_function,
                popup_w - 28,
                FontStyle::UiSmall,
            );
            self.hit_regions.push((row, act));
            y += 26;
        }

        canvas.draw_text_styled(
            popup_rect.x + 12,
            y + 2,
            "Local Branches",
            t.text_muted,
            popup_w - 24,
            FontStyle::UiSmall,
        );
        y += 20;

        for b in branches {
            let row = Rect::new(popup_rect.x + 6, y, popup_w - 12, 24);
            let is_curr = b == self.git_engine.current_branch;
            if is_curr {
                canvas.fill_rounded_rect(row, 4, t.popup_hover_bg);
            }
            canvas.draw_git_branch_icon(row.x + 8, y + 5, t.accent_blue);
            canvas.draw_text_styled(
                row.x + 24,
                y + 4,
                &format!("{}{}", b, if is_curr { " (current)" } else { "" }),
                if is_curr { 0xFFFFFF } else { t.text_primary },
                popup_w - 40,
                FontStyle::UiSmall,
            );
            self.hit_regions
                .push((row, HitAction::CheckoutGitBranch(b)));
            y += 25;
        }
    }

    fn render_search_everywhere_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let t = self.theme;
        let mw = 680.min(w - 40);
        let mh = 390.min(h - 60);
        let mx = (w - mw) / 2;
        let my = (h - mh) / 4;
        let modal = Rect::new(mx, my, mw, mh);

        canvas.stroke_rounded_rect(modal, 8, t.accent_blue, t.popup_bg);

        let header = Rect::new(mx + 2, my + 2, mw - 4, 34);
        canvas.fill_rounded_rect(header, 6, t.panel_header_bg);
        canvas.draw_search_icon(mx + 14, my + 11, t.accent_blue);
        canvas.draw_text_styled(
            mx + 34,
            my + 9,
            "Search Everywhere  [All | Classes | Files | Symbols | Actions | Agents]  (Esc 关闭)",
            t.text_primary,
            mw - 48,
            FontStyle::UiBold,
        );

        let input_box = Rect::new(mx + 12, my + 42, mw - 24, 30);
        canvas.stroke_rounded_rect(input_box, 5, t.accent_blue, t.editor_bg);
        canvas.draw_text_styled(
            mx + 16,
            my + 48,
            &format!("> {}_", self.search_query),
            t.text_primary,
            mw - 36,
            FontStyle::CodeMono,
        );

        let mut ry = my + 80;
        let results = self.search_results.clone();
        for item in results.into_iter().take(11) {
            if ry + 24 > my + mh - 8 {
                break;
            }
            let row = Rect::new(mx + 12, ry, mw - 24, 24);
            canvas.fill_rounded_rect(row, 4, t.editor_current_line_bg);
            match item {
                SearchEverywhereItem::File {
                    name,
                    relative_path,
                    full_path,
                } => {
                    canvas.draw_text_styled(
                        mx + 18,
                        ry + 3,
                        &format!("[文件]  {}   ({})", name, relative_path),
                        t.text_primary,
                        mw - 36,
                        FontStyle::UiRegular,
                    );
                    self.hit_regions
                        .push((row, HitAction::OpenFile(full_path)));
                }
                SearchEverywhereItem::Action {
                    action,
                    title,
                    shortcut,
                } => {
                    canvas.draw_text_styled(
                        mx + 18,
                        ry + 3,
                        &format!("[动作]  {}     [{}]", title, shortcut),
                        t.syn_function,
                        mw - 36,
                        FontStyle::UiRegular,
                    );
                    self.hit_regions
                        .push((row, HitAction::ExecuteIdeAction(action)));
                }
                SearchEverywhereItem::AgentCommand {
                    agent_name,
                    description,
                } => {
                    canvas.draw_text_styled(
                        mx + 18,
                        ry + 3,
                        &format!("[Agent] {} — {}", agent_name, description),
                        t.syn_keyword,
                        mw - 36,
                        FontStyle::UiRegular,
                    );
                }
            }
            ry += 26;
        }
    }

    fn render_find_in_files_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let t = self.theme;
        let mw = 700.min(w - 40);
        let mh = 390.min(h - 60);
        let mx = (w - mw) / 2;
        let my = (h - mh) / 4;
        let modal = Rect::new(mx, my, mw, mh);

        canvas.stroke_rounded_rect(modal, 8, t.accent_blue, t.popup_bg);
        canvas.draw_search_icon(mx + 14, my + 11, t.accent_blue);
        canvas.draw_text_styled(
            mx + 34,
            my + 9,
            "Find in Files (Ripgrep 全局内容检索 Ctrl+Shift+F) — 输入关键字实时搜索",
            t.text_primary,
            mw - 48,
            FontStyle::UiBold,
        );

        let input_box = Rect::new(mx + 12, my + 40, mw - 24, 30);
        canvas.stroke_rounded_rect(input_box, 5, t.accent_blue, t.editor_bg);
        canvas.draw_text_styled(
            mx + 16,
            my + 46,
            &format!("Regex/Text > {}_", self.search_query),
            t.text_primary,
            mw - 36,
            FontStyle::CodeMono,
        );

        let mut ry = my + 78;
        let hits = self.content_search_hits.clone();
        for hit in hits.into_iter().take(11) {
            let row = Rect::new(mx + 12, ry, mw - 24, 24);
            canvas.fill_rounded_rect(row, 4, t.editor_current_line_bg);
            let fname = hit
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file");
            canvas.draw_text_styled(
                mx + 18,
                ry + 3,
                &format!("{}:{}  |  {}", fname, hit.line_number, hit.line_text),
                t.text_primary,
                mw - 36,
                FontStyle::CodeMono,
            );
            self.hit_regions
                .push((row, HitAction::OpenFile(hit.path)));
            ry += 26;
        }
    }

    fn render_input_prompt_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let t = self.theme;
        let mw = 460;
        let mh = 135;
        let mx = (w - mw) / 2;
        let my = (h - mh) / 3;
        let modal = Rect::new(mx, my, mw, mh);

        canvas.stroke_rounded_rect(modal, 8, t.accent_blue, t.popup_bg);

        let title = match self.focus {
            FocusTarget::NewFileModal => {
                format!(
                    "New File in {} (Enter 创建 / Esc 取消)",
                    self.project_tree.active_target_dir().display()
                )
            }
            FocusTarget::NewFolderModal => {
                format!(
                    "New Directory in {} (Enter 创建 / Esc 取消)",
                    self.project_tree.active_target_dir().display()
                )
            }
            FocusTarget::NewGitBranchModal => "Create & Checkout New Git Branch (Enter 创建)".to_string(),
            _ => String::new(),
        };

        canvas.draw_text_styled(
            mx + 16,
            my + 14,
            &title,
            t.text_primary,
            mw - 32,
            FontStyle::UiBold,
        );

        let input_box = Rect::new(mx + 16, my + 48, mw - 32, 32);
        canvas.stroke_rounded_rect(input_box, 5, t.accent_blue, t.editor_bg);
        canvas.draw_text_styled(
            mx + 24,
            my + 55,
            &format!("{}_", self.modal_text_input),
            t.text_primary,
            mw - 48,
            FontStyle::CodeMono,
        );

        canvas.draw_text_styled(
            mx + 16,
            my + 92,
            "按 Enter 确认创建，按 Esc 关闭弹窗",
            t.text_muted,
            mw - 32,
            FontStyle::UiSmall,
        );
    }

    fn render_settings_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let t = self.theme;
        let mw = 720.min(w - 40);
        let mh = 460.min(h - 60);
        let mx = (w - mw) / 2;
        let my = (h - mh) / 4;
        let modal = Rect::new(mx, my, mw, mh);

        canvas.stroke_rounded_rect(modal, 8, t.accent_blue, t.popup_bg);
        canvas.draw_text_styled(
            mx + 16,
            my + 12,
            "Settings — Appearance, Theme & IntelliJ IDEA Keymap (Ctrl+Alt+S)",
            t.text_primary,
            mw - 100,
            FontStyle::UiBold,
        );

        let close_btn = Rect::new(mx + mw - 72, my + 8, 58, 24);
        canvas.stroke_rounded_rect(close_btn, 5, t.border_color, t.window_bg);
        canvas.draw_text_centered(close_btn, "关闭", t.text_primary, FontStyle::UiSmall);
        self.hit_regions
            .push((close_btn, HitAction::CloseModalOrPopup));

        // 1. 主题切换器
        canvas.draw_text_styled(
            mx + 16,
            my + 46,
            "1. IDE Theme (点击即刻切换主题配色):",
            t.syn_function,
            mw - 32,
            FontStyle::UiBold,
        );

        let mut tx = mx + 16;
        for &kind in ThemeKind::all() {
            let btn = Rect::new(tx, my + 72, 215, 30);
            let active = self.theme.kind == kind;
            canvas.stroke_rounded_rect(
                btn,
                6,
                if active { t.accent_blue } else { t.border_color },
                if active { t.popup_hover_bg } else { t.editor_bg },
            );
            canvas.draw_text_centered(
                btn,
                kind.display_name(),
                if active { 0xFFFFFF } else { t.text_primary },
                if active {
                    FontStyle::UiBold
                } else {
                    FontStyle::UiSmall
                },
            );
            self.hit_regions.push((btn, HitAction::SelectTheme(kind)));
            tx += 225;
        }

        // 2. IDEA 标准快捷键列表（点击任意一行可直接触发该功能）
        canvas.draw_text_styled(
            mx + 16,
            my + 114,
            "2. IntelliJ IDEA Keymap Reference (点击任意条目可直接执行):",
            t.syn_function,
            mw - 32,
            FontStyle::UiBold,
        );

        let mut ky = my + 140;
        for &act in IdeAction::all().iter().take(12) {
            if ky + 24 > my + mh - 10 {
                break;
            }
            let row = Rect::new(mx + 16, ky, mw - 32, 22);
            canvas.fill_rounded_rect(row, 4, t.editor_bg);
            canvas.draw_text_styled(
                mx + 24,
                ky + 2,
                act.title(),
                t.text_primary,
                mw - 180,
                FontStyle::UiSmall,
            );
            canvas.draw_text_styled(
                mx + mw - 145,
                ky + 2,
                act.shortcut_label(),
                t.syn_keyword,
                120,
                FontStyle::CodeMonoBold,
            );
            self.hit_regions
                .push((row, HitAction::ExecuteIdeAction(act)));
            ky += 25;
        }
    }
}
