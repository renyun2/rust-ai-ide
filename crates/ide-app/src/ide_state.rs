use crate::agent_panel::{AgentChatCache, AGENT_INPUT_FONT, AGENT_INPUT_LINE_H};
use crate::canvas::{FontStyle, Rect, SurfaceCanvas};
use crate::image_preview::{
    detect_pasted_code_snippet, is_image_file_path, load_image_for_preview,
    read_windows_clipboard_payload, ClipboardPastePayload, DecodedPreviewImage,
};
use crate::text_input::TextInputField;
use ide_agent_hub::{
    AgentAttachedItem, AgentHub, AgentProviderKind, AgentWorkbenchMode, AiApiFormat,
    IdeCodeContext, PastedCodeSnippet,
};
use ide_core::{
    IdeAction, IdeaTheme, MainMenuCategory, MenuEntry, RunConfiguration, ThemeKind, ToolWindowId,
    UiLanguage,
};
use ide_dap::DapDebugEngine;
use ide_git::{DiffLineKind, GitEngine};
use ide_lsp::LspHub;
use ide_search::{
    find_in_files, find_in_files_indexed, index_workspace_files, search_everywhere,
    search_everywhere_indexed, ContentSearchHit, SearchEverywhereItem, WorkspaceFileEntry,
};
use ide_syntax::{
    detect_runnable_gutter_entries, extract_structure_symbols, highlight_line, HighlightSpan,
    RunnableGutterEntry, StructureSymbol, TokenKind,
};
use ide_terminal::TerminalSession;
use ide_vfs::{ProjectTreeState, TextBuffer};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

static FOLDER_PICKER_OPEN: AtomicBool = AtomicBool::new(false);

#[inline]
pub(crate) fn blend_rgb(bg: u32, fg: u32, t: f32) -> u32 {
    let a = t.clamp(0.0, 1.0);
    let inv = 1.0 - a;
    let br = ((bg >> 16) & 0xFF) as f32;
    let bgg = ((bg >> 8) & 0xFF) as f32;
    let bb = (bg & 0xFF) as f32;
    let fr = ((fg >> 16) & 0xFF) as f32;
    let fg_g = ((fg >> 8) & 0xFF) as f32;
    let fb = (fg & 0xFF) as f32;
    let r = (br * inv + fr * a).round() as u32;
    let g = (bgg * inv + fg_g * a).round() as u32;
    let b = (bb * inv + fb * a).round() as u32;
    (r.min(255) << 16) | (g.min(255) << 8) | b.min(255)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderPickMode {
    AskUser,
    NewWindow,
}

fn clean_workspace_path(path: PathBuf) -> PathBuf {
    let resolved = std::fs::canonicalize(&path).unwrap_or(path);
    #[cfg(target_os = "windows")]
    {
        if let Some(s) = resolved.to_str() {
            if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
                return PathBuf::from(format!(r"\\{stripped}"));
            }
            if let Some(stripped) = s.strip_prefix(r"\\?\") {
                return PathBuf::from(stripped);
            }
        }
    }
    resolved
}

#[cfg(target_os = "windows")]
pub fn pick_folder_windows(initial_dir: Option<&Path>) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
        FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
    };

    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let should_uninit = hr.is_ok();

        let result = (|| -> Option<PathBuf> {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL).ok()?;
            let opts = dialog.GetOptions().ok()?;
            dialog
                .SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)
                .ok()?;

            if let Some(dir) = initial_dir {
                if dir.exists() {
                    let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
                    wide.push(0);
                    if let Ok(item) =
                        SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(wide.as_ptr()), None)
                    {
                        let _ = dialog.SetFolder(&item);
                    }
                }
            }

            if dialog.Show(None).is_err() {
                return None;
            }
            let item = dialog.GetResult().ok()?;
            let pwstr = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            if pwstr.0.is_null() {
                return None;
            }
            let mut len = 0usize;
            while *pwstr.0.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(pwstr.0, len);
            let path_str = String::from_utf16_lossy(slice);
            CoTaskMemFree(Some(pwstr.0 as *const core::ffi::c_void));
            Some(PathBuf::from(path_str))
        })();

        if should_uninit {
            CoUninitialize();
        }
        result
    }
}

#[cfg(not(target_os = "windows"))]
pub fn pick_folder_windows(_initial_dir: Option<&Path>) -> Option<PathBuf> {
    None
}

/// 可拖拽调整大小的面板分割线标识
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragSplitter {
    LeftPanel,
    RightPanel,
    BottomPanel,
}

/// 鼠标悬停区域对应的原生指针形态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdeCursorKind {
    Default,
    Text,
    Pointer,
    EwResize,
    NsResize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragScrollbar {
    LeftTree,
    CenterEditor,
    RightPanel,
    BottomTerminal,
    GitLogCommits,
    GitLogDiff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightPanelTab {
    Chat,
    Changes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowControlCmd {
    Minimize,
    ToggleMaximize,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextInputNavAction {
    Left { extend: bool },
    Right { extend: bool },
    WordLeft { extend: bool },
    WordRight { extend: bool },
    Home { extend: bool },
    End { extend: bool },
    DeleteForward,
    DeleteWordBackward,
    DeleteWordForward,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollTrackInfo {
    pub track: Rect,
    pub visible: usize,
    pub total: usize,
}

#[derive(Debug, Clone)]
pub enum ContextMenuAction {
    Ide(IdeAction),
    EditorCut,
    EditorCopy,
    EditorPaste,
    EditorSelectAll,
    InputCut(FocusTarget),
    InputCopy(FocusTarget),
    InputPaste(FocusTarget),
    InputSelectAll(FocusTarget),
    TreeOpenFile(PathBuf),
    TreeNewFileIn(PathBuf),
    TreeNewDirIn(PathBuf),
    TreeRename(PathBuf),
    TreeDelete(PathBuf),
    TreeRevealInExplorer(PathBuf),
    TreeCopyPath(PathBuf),
    TreeExpandAll,
    TreeCollapseAll,
    TabClose(usize),
    TabCloseOthers(usize),
    TabRevealInExplorer(PathBuf),
    TabCopyPath(PathBuf),
    TermCopy,
    TermPaste,
    TermSelectAll,
    TermSendInterrupt,
    TermClear,
    TermNewTab,
    // IDEA Git 上下文菜单操作
    GitToggleStage(PathBuf),
    GitShowWorkingDiff(PathBuf),
    GitRollbackFile(PathBuf),
    GitCheckoutRevision(String),
    GitNewBranchFromCommit(String),
    GitCherryPickCommit(String),
    GitRevertCommit(String),
    GitResetSoftToCommit(String),
    GitCopyText(String),
    AgentCopySelection,
    AgentCopyAllChat,
}

#[derive(Debug, Clone)]
pub struct ContextMenuItem {
    pub label: String,
    pub shortcut: String,
    pub action: ContextMenuAction,
    pub separator_before: bool,
}

#[derive(Debug, Clone)]
pub struct ContextMenuState {
    pub x: i32,
    pub y: i32,
    pub items: Vec<ContextMenuItem>,
}

#[cfg(target_os = "windows")]
pub fn read_windows_clipboard_text() -> Option<String> {
    use windows::Win32::Foundation::{HGLOBAL, HWND};
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;

    unsafe {
        if OpenClipboard(Some(HWND::default())).is_err() {
            return None;
        }
        let result = (|| -> Option<String> {
            let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
            let hglobal = HGLOBAL(handle.0);
            let ptr = GlobalLock(hglobal) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(ptr, len);
            let text = String::from_utf16_lossy(slice);
            let _ = GlobalUnlock(hglobal);
            Some(text.replace("\r\n", "\n"))
        })();
        let _ = CloseClipboard();
        result
    }
}

#[cfg(not(target_os = "windows"))]
pub fn read_windows_clipboard_text() -> Option<String> {
    None
}

#[cfg(target_os = "windows")]
pub fn write_windows_clipboard_text(text: &str) -> bool {
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    const CF_UNICODETEXT: u32 = 13;

    let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
    let mut wide: Vec<u16> = normalized.encode_utf16().collect();
    wide.push(0);
    let byte_len = wide.len() * std::mem::size_of::<u16>();

    unsafe {
        if OpenClipboard(Some(HWND::default())).is_err() {
            return false;
        }
        let ok = (|| -> bool {
            if EmptyClipboard().is_err() {
                return false;
            }
            let Ok(hglobal) = GlobalAlloc(GMEM_MOVEABLE, byte_len) else {
                return false;
            };
            let ptr = GlobalLock(hglobal) as *mut u16;
            if ptr.is_null() {
                let _ = GlobalFree(Some(hglobal));
                return false;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(hglobal);
            if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hglobal.0))).is_err() {
                let _ = GlobalFree(Some(hglobal));
                return false;
            }
            true
        })();
        let _ = CloseClipboard();
        ok
    }
}

#[cfg(not(target_os = "windows"))]
pub fn write_windows_clipboard_text(_text: &str) -> bool {
    false
}

#[derive(Debug, Clone)]
pub enum HitAction {
    // 自定义无边框窗口标题栏控制
    WindowControl(WindowControlCmd),
    // 顶部主菜单与弹窗
    ToggleHamburgerMenu,
    OpenMenuCategory(MainMenuCategory),
    ExecuteIdeAction(IdeAction),
    ToggleProjectPopup,
    ToggleRunConfigPopup,
    SelectRunConfig(usize),
    ToggleMoreMenuPopup,
    ToggleGitBranchPopup,
    CheckoutGitBranch(String),
    CreateNewGitBranchPrompt,
    // 编辑器与标签页
    SelectTab(usize),
    CloseTab(usize),
    OpenFile(PathBuf),
    JumpToLine(usize),
    ClickEditorLine,
    FocusEditorBody,
    // 项目树文件夹与文件操作
    ToggleDirectory(PathBuf),
    PromptNewFile,
    PromptNewFolder,
    ExpandAllDirs,
    CollapseAllDirs,
    // 左右底部面板切换
    ToggleLeftTool(ToolWindowId),
    ToggleRightAgentHub,
    SwitchRightPanelTab(RightPanelTab),
    SelectBottomTool(ToolWindowId),
    // Git 面板与 IDEA Git 历史库交互
    ToggleStageFile(PathBuf),
    StageAllGitChanges,
    UnstageAllGitChanges,
    CommitAllGitChanges,
    CommitAndPushGitChanges,
    ToggleGitAmend,
    GenerateAiCommitMessage,
    RollbackGitFile(PathBuf),
    RollbackAllGitChanges,
    RefreshGitStatus,
    GitFetchAll,
    FocusGitCommitMessageInput,
    InspectGitWorkingFileDiff(PathBuf),
    SelectGitLogBranchFilter(Option<String>),
    SelectGitCommit(String),
    SelectGitCommitFile(usize),
    FocusGitLogSearchInput,
    ClearGitLogSearch,
    ClearGitWorkingDiffTarget,
    GitCheckoutSelectedCommit,
    GitCreateBranchAtSelectedCommit,
    GitCherryPickSelectedCommit,
    GitRevertSelectedCommit,
    GitCopySelectedCommitHash,
    GitMergeBranch(String),
    GitDeleteBranch(String),
    // Multi-Agent 与内联编辑 / AI 编辑台补丁交互
    SelectAgentProvider(AgentProviderKind),
    ToggleAgentProviderPopup,
    ToggleAgentModelPopup,
    SelectAgentModel(String),
    ToggleAgentFastMode,
    CycleAgentReasoningEffort,
    RefreshLocalCliAgents,
    ToggleAgentModePopup,
    SelectAgentMode(AgentWorkbenchMode),
    ToggleAttachActiveContext,
    AttachActiveFileToAgent,
    ClearAgentChat,
    NewAgentSession,
    SelectAgentSession(usize),
    CloseAgentSession(usize),
    ToggleAgentSessionsPopup,
    ToggleCodeBlockCollapse(usize, usize),
    ToggleMessageThoughtCollapse(usize),
    ToggleComposerPastedSnippet(String),
    RemoveComposerPastedSnippet(String),
    RemoveComposerAttachment(PathBuf),
    OpenImagePreview(PathBuf),
    CloseImagePreview,
    CopyAgentMessage(usize),
    CopyAgentCodeBlock(String),
    OpenFileAtLine(PathBuf, Option<usize>),
    FocusAgentChatStream,
    RunAgentQuickPrompt(&'static str),
    ApplyChatPatch(usize),
    InsertChatPatch(usize),
    PreviewPatchInEditor(usize),
    TriggerCtrlKInlineEdit,
    FocusCtrlKPrompt,
    AcceptInlineDiff,
    RejectInlineDiff,
    SendSelectionToAgent,
    RevertLastCheckpoint,
    FocusAgentChatInput,
    SubmitAgentChatPrompt,
    CancelAgentRequest,
    // 本地 CLI 与模型总览弹窗
    OpenAiConfigModal,
    SaveAiConfigModal,
    // 终端与运行调试
    FocusTerminalInput,
    NewTerminalTab,
    SelectTerminalTab(usize),
    CloseTerminalTab(usize),
    ClearTerminal,
    RunActiveConfig,
    DebugActiveConfig,
    ToggleSearchEverywhere,
    ToggleBreakpointAtLine(usize),
    RunGutterEntry(usize),
    // 设置、语言、DPI 缩放、工作区与主题切换
    SelectTheme(ThemeKind),
    SetUiLanguage(UiLanguage),
    SetUiScale(u16),
    OpenFolderDialog,
    OpenFolderInNewWindowDialog,
    PromptOpenWorkspace(PathBuf),
    OpenWorkspaceInNewWindow(PathBuf),
    ConfirmOpenPendingInThisWindow,
    ConfirmOpenPendingInNewWindow,
    CloseModalOrPopup,
    // 右键上下文菜单动作
    TriggerContextMenuAction(ContextMenuAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    Editor,
    CtrlKPrompt,
    AgentChatInput,
    AgentChatStream,
    TerminalInput,
    GitCommitInput,
    GitLogSearchInput,
    SearchEverywhereModal,
    FindInFilesModal,
    NewFileModal,
    NewFolderModal,
    NewGitBranchModal,
    RenamePathModal,
    SettingsModal,
    AiConfigModal(usize),
    OpenProjectChoiceModal,
    ImagePreviewModal,
}

pub struct IdeApplicationState {
    pub workspace_root: PathBuf,
    pub recent_workspaces: Vec<PathBuf>,
    pub workspace_title_dirty: bool,
    pub ui_lang: UiLanguage,
    pub system_dpi_scale: f32,
    pub ui_scale: f32,
    pub theme: IdeaTheme,
    pub project_tree: ProjectTreeState,
    pub open_buffers: Vec<TextBuffer>,
    pub active_tab: usize,
    pub scroll_line: usize,
    pub editor_scroll_x: i32,
    pub tree_scroll: usize,
    pub agent_scroll: usize,
    pub git_changes_scroll: usize,
    pub git_amend_commit: bool,
    pub git_log_scroll: usize,
    pub git_diff_scroll: usize,
    pub git_files_scroll: usize,
    pub git_branches_scroll: usize,
    pub selected_git_commit_hash: Option<String>,
    pub selected_git_commit_file: usize,
    pub git_log_branch_filter: Option<String>,
    pub git_working_diff_target: Option<PathBuf>,
    pub git_new_branch_from_ref: Option<String>,
    pub modal_scroll: usize,
    // 自定义窗口与悬浮态
    pub hover_pos: (i32, i32),
    pub last_hover_hit_idx: Option<usize>,
    pub last_hover_splitter: Option<DragSplitter>,
    pub last_win_size: (i32, i32),
    pub window_maximized: bool,
    pub pending_window_cmd: Option<WindowControlCmd>,
    // 可拖拽调节的停靠面板尺寸
    pub left_tool: Option<ToolWindowId>,
    pub left_panel_width: i32,
    pub right_agent_open: bool,
    pub right_panel_width: i32,
    pub right_panel_tab: RightPanelTab,
    pub bottom_tool: Option<ToolWindowId>,
    pub bottom_panel_height: i32,
    pub active_splitter_drag: Option<DragSplitter>,
    pub active_scrollbar_drag: Option<DragScrollbar>,
    // 面板边界与滚动条信息
    pub left_pane_rect: Rect,
    pub editor_pane_rect: Rect,
    pub editor_code_rect: Rect,
    pub right_pane_rect: Rect,
    pub bottom_pane_rect: Rect,
    pub terminal_content_rect: Rect,
    pub git_log_branches_rect: Rect,
    pub git_log_commits_rect: Rect,
    pub git_log_files_rect: Rect,
    pub git_log_diff_rect: Rect,
    pub left_scroll_info: ScrollTrackInfo,
    pub editor_scroll_info: ScrollTrackInfo,
    pub right_scroll_info: ScrollTrackInfo,
    pub terminal_scroll_info: ScrollTrackInfo,
    pub git_log_commits_scroll_info: ScrollTrackInfo,
    pub git_log_diff_scroll_info: ScrollTrackInfo,
    pub editor_code_start_x: i32,
    pub editor_visible_lines: usize,
    pub editor_char_w: i32,
    pub terminal_line_h: i32,
    pub terminal_char_w: i32,
    pub terminal_scroll_anchor_line: usize,
    pub last_pty_size: (u16, u16),
    pub tab_rects: Vec<(Rect, usize)>,
    pub project_entry_rects: Vec<(Rect, PathBuf, bool)>,
    pub git_commit_row_rects: Vec<(Rect, String, String)>,
    pub git_change_row_rects: Vec<(Rect, PathBuf, bool)>,
    // 顶部主菜单与弹窗状态
    pub hamburger_menu_expanded: bool,
    pub active_menu_category: Option<MainMenuCategory>,
    pub menu_category_rects: Vec<(MainMenuCategory, Rect)>,
    pub project_popup_open: bool,
    pub run_config_popup_open: bool,
    pub more_menu_popup_open: bool,
    pub git_branch_popup_open: bool,
    pub agent_provider_popup_open: bool,
    pub agent_model_popup_open: bool,
    pub agent_mode_popup_open: bool,
    pub agent_sessions_popup_open: bool,
    pub agent_mode: AgentWorkbenchMode,
    pub agent_busy_started_at: Option<Instant>,
    pub agent_provider_pill_rect: Rect,
    pub agent_model_pill_rect: Rect,
    pub agent_mode_pill_rect: Rect,
    pub attach_active_context: bool,
    pub context_menu: Option<ContextMenuState>,
    pub context_menu_rect: Rect,
    pub active_overlay_rects: Vec<Rect>,
    pub overlay_hit_start: usize,
    pub rename_target_path: Option<PathBuf>,
    // 子系统引擎
    pub run_configs: Vec<RunConfiguration>,
    pub active_run_config: usize,
    pub lsp_hub: LspHub,
    pub dap_engine: DapDebugEngine,
    pub git_engine: GitEngine,
    pub terminal: TerminalSession,
    pub agent_hub: AgentHub,
    // 焦点与原生输入框 / IME / 光标交互状态
    pub focus: FocusTarget,
    pub ime_spot: (i32, i32),
    pub caret_visible: bool,
    pub last_input_activity: Instant,
    pub mouse_drag_target: Option<FocusTarget>,
    pub ctrl_k_input: TextInputField,
    pub agent_chat_input: TextInputField,
    pub git_commit_input: TextInputField,
    pub git_log_search_input: TextInputField,
    pub modal_text_input: TextInputField,
    pub search_query_input: TextInputField,
    pub ai_cfg_format: AiApiFormat,
    pub ai_cfg_url: TextInputField,
    pub ai_cfg_key: TextInputField,
    pub ai_cfg_model: TextInputField,
    pub ai_cfg_show_key: bool,
    pub agent_input_text_rect: Rect,
    pub agent_chat_area_rect: Rect,
    pub agent_chat_line_rects: Vec<(Rect, i32, usize, String, FontStyle)>,
    pub agent_chat_sel_anchor: Option<(usize, usize)>,
    pub agent_chat_sel_cursor: Option<(usize, usize)>,
    pub collapsed_code_blocks: HashMap<(usize, usize), bool>,
    pub expanded_message_thoughts: HashMap<usize, bool>,
    pub expanded_composer_snippet_id: Option<String>,
    pub agent_chat_signature_salt: u64,
    pub image_preview_cache: HashMap<PathBuf, Arc<DecodedPreviewImage>>,
    pub active_image_preview: Option<PathBuf>,
    pub prev_focus_before_image_preview: Option<FocusTarget>,
    pub external_drag_hover: Option<(PathBuf, (i32, i32))>,
    pub pending_ide_drag: Option<(PathBuf, (i32, i32))>,
    pub active_ide_drag: Option<(PathBuf, (i32, i32))>,
    pub ctrl_k_input_box_rect: Rect,
    pub git_commit_input_box_rect: Rect,
    pub git_log_search_rect: Rect,
    pub search_modal_text_start_x: i32,
    pub search_modal_input_rect: Rect,
    pub prompt_modal_input_rect: Rect,
    pub ai_cfg_field_rects: [Rect; 3],
    pub search_results: Vec<SearchEverywhereItem>,
    pub content_search_hits: Vec<ContentSearchHit>,
    pub search_loading: bool,
    pub pending_open_workspace: Option<PathBuf>,
    pub folder_pick_mode: FolderPickMode,
    pub is_indexing_workspace: Arc<AtomicBool>,
    pub anim_start: Instant,
    pub last_anim_tick: Instant,
    pub startup_loading_until: Instant,
    pub startup_load_alpha: f32,
    pub git_load_alpha: f32,
    pub tree_load_alpha: f32,
    wakeup_enabled: bool,
    wakeup: Option<Arc<dyn Fn() + Send + Sync>>,
    folder_pick_tx: mpsc::Sender<PathBuf>,
    folder_pick_rx: mpsc::Receiver<PathBuf>,
    workspace_files_cache: Arc<RwLock<Option<Vec<WorkspaceFileEntry>>>>,
    search_seq: Arc<AtomicU64>,
    search_cmd_tx: mpsc::Sender<SearchWorkerCmd>,
    search_event_rx: mpsc::Receiver<AsyncSearchEvent>,
    cached_runnables: Option<(PathBuf, usize, Vec<RunnableGutterEntry>)>,
    cached_structure: Option<(PathBuf, usize, Vec<StructureSymbol>)>,
    cached_highlight_lines: (PathBuf, usize, HashMap<usize, Vec<HighlightSpan>>),
    pub(crate) cached_chat_rows: AgentChatCache,
    pub hit_regions: Vec<(Rect, HitAction)>,
    pub status_notification: String,
}

#[derive(Debug)]
enum SearchWorkerCmd {
    SetWorkspaceRoot(PathBuf),
    ReindexWorkspace,
    SearchEverywhere {
        seq: u64,
        query: String,
        max_results: usize,
    },
    FindInFiles {
        seq: u64,
        pattern: String,
        max_hits: usize,
    },
}

#[derive(Debug)]
enum AsyncSearchEvent {
    SearchEverywhereReady {
        seq: u64,
        items: Vec<SearchEverywhereItem>,
    },
    FindInFilesReady {
        seq: u64,
        hits: Vec<ContentSearchHit>,
    },
}

fn ensure_search_index(
    root: &Path,
    files_cache: &Arc<RwLock<Option<Vec<WorkspaceFileEntry>>>>,
) {
    if let Ok(guard) = files_cache.read() {
        if guard.is_some() {
            return;
        }
    }
    let indexed = index_workspace_files(root);
    if let Ok(mut guard) = files_cache.write() {
        *guard = Some(indexed);
    }
}

fn spawn_search_worker(
    initial_root: PathBuf,
    files_cache: Arc<RwLock<Option<Vec<WorkspaceFileEntry>>>>,
    latest_seq: Arc<AtomicU64>,
    indexing_flag: Arc<AtomicBool>,
    wakeup: Option<Arc<dyn Fn() + Send + Sync>>,
) -> (mpsc::Sender<SearchWorkerCmd>, mpsc::Receiver<AsyncSearchEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<SearchWorkerCmd>();
    let (event_tx, event_rx) = mpsc::channel::<AsyncSearchEvent>();

    let _ = thread::Builder::new()
        .name("ide-search-worker".to_string())
        .spawn(move || {
            let mut root = initial_root;

            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    SearchWorkerCmd::SetWorkspaceRoot(new_root) => {
                        root = new_root;
                        if let Ok(mut guard) = files_cache.write() {
                            *guard = None;
                        }
                    }
                    SearchWorkerCmd::ReindexWorkspace => {
                        indexing_flag.store(true, Ordering::Release);
                        let indexed = index_workspace_files(&root);
                        if let Ok(mut guard) = files_cache.write() {
                            *guard = Some(indexed);
                        }
                        indexing_flag.store(false, Ordering::Release);
                        if let Some(cb) = &wakeup {
                            cb();
                        }
                    }
                    SearchWorkerCmd::SearchEverywhere {
                        seq,
                        query,
                        max_results,
                    } => {
                        if seq != latest_seq.load(Ordering::Acquire) {
                            continue;
                        }
                        ensure_search_index(&root, &files_cache);
                        if seq != latest_seq.load(Ordering::Acquire) {
                            continue;
                        }
                        let items = if let Ok(guard) = files_cache.read() {
                            if let Some(files) = guard.as_ref() {
                                search_everywhere_indexed(files, &query, max_results)
                            } else {
                                search_everywhere(&root, &query, max_results)
                            }
                        } else {
                            search_everywhere(&root, &query, max_results)
                        };
                        let _ = event_tx.send(AsyncSearchEvent::SearchEverywhereReady { seq, items });
                        if let Some(cb) = &wakeup {
                            cb();
                        }
                    }
                    SearchWorkerCmd::FindInFiles {
                        seq,
                        pattern,
                        max_hits,
                    } => {
                        if seq != latest_seq.load(Ordering::Acquire) {
                            continue;
                        }
                        ensure_search_index(&root, &files_cache);
                        if seq != latest_seq.load(Ordering::Acquire) {
                            continue;
                        }
                        let hits = if let Ok(guard) = files_cache.read() {
                            if let Some(files) = guard.as_ref() {
                                find_in_files_indexed(files, &pattern, max_hits).unwrap_or_default()
                            } else {
                                find_in_files(&root, &pattern, max_hits).unwrap_or_default()
                            }
                        } else {
                            find_in_files(&root, &pattern, max_hits).unwrap_or_default()
                        };
                        let _ = event_tx.send(AsyncSearchEvent::FindInFilesReady { seq, hits });
                        if let Some(cb) = &wakeup {
                            cb();
                        }
                    }
                }
            }
        });

    (cmd_tx, event_rx)
}

fn pick_initial_buffers(workspace_root: &Path, project_tree: &ProjectTreeState) -> Vec<TextBuffer> {
    let mut open_buffers = Vec::new();
    let default_candidates = [
        workspace_root.join("crates/ide-agent-hub/src/lib.rs"),
        workspace_root.join("crates/ide-core/src/lib.rs"),
        workspace_root.join("src/main.rs"),
        workspace_root.join("src/lib.rs"),
        workspace_root.join("Cargo.toml"),
        workspace_root.join("package.json"),
        workspace_root.join("main.py"),
        workspace_root.join("README.md"),
    ];
    for cand in default_candidates {
        if open_buffers.len() >= 3 {
            break;
        }
        if cand.is_file() && !open_buffers.iter().any(|b: &TextBuffer| b.path == cand) {
            if let Ok(buf) = TextBuffer::open(&cand) {
                open_buffers.push(buf);
            }
        }
    }
    if open_buffers.is_empty() {
        for entry in &project_tree.visible_entries {
            if open_buffers.len() >= 2 {
                break;
            }
            if !entry.is_dir && entry.path.is_file() {
                if let Ok(meta) = std::fs::metadata(&entry.path) {
                    if meta.len() > 512 * 1024 {
                        continue;
                    }
                }
                if let Ok(buf) = TextBuffer::open(&entry.path) {
                    open_buffers.push(buf);
                }
            }
        }
    }
    if open_buffers.is_empty() {
        open_buffers.push(TextBuffer::from_text(
            workspace_root.join("src/main.rs"),
            "fn main() {\n    let res = Some(42).unwrap();\n    println!(\"Hello Rust AI IDE: {res}\");\n}\n",
        ));
    }
    open_buffers
}

impl IdeApplicationState {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self::new_with_wakeup(workspace_root, None)
    }

    pub fn new_with_wakeup(
        workspace_root: PathBuf,
        wakeup: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let workspace_root = clean_workspace_path(workspace_root);
        let wakeup_enabled = wakeup.is_some();
        let ui_lang = UiLanguage::default();
        let project_tree = ProjectTreeState::new(&workspace_root);
        let run_configs = RunConfiguration::detect_for_workspace(&workspace_root);
        let lsp_hub = LspHub::new(&workspace_root);
        let dap_engine = DapDebugEngine::new();
        let git_engine = if wakeup_enabled {
            GitEngine::open_with_wakeup(&workspace_root, wakeup.clone())
        } else {
            GitEngine::open(&workspace_root)
        };
        let term_title = match ui_lang {
            UiLanguage::ZhCn => "本地",
            UiLanguage::EnUs => "Local",
        };
        let terminal = TerminalSession::new_with_wakeup(term_title, &workspace_root, wakeup.clone());
        let agent_hub = AgentHub::new_with_wakeup(&workspace_root, wakeup.clone());
        let api_cfg = agent_hub.api_config.clone();

        let (folder_pick_tx, folder_pick_rx) = mpsc::channel::<PathBuf>();
        let workspace_files_cache = Arc::new(RwLock::new(None));
        let search_seq = Arc::new(AtomicU64::new(0));
        let is_indexing_workspace = Arc::new(AtomicBool::new(wakeup_enabled));
        let (search_cmd_tx, search_event_rx) = spawn_search_worker(
            workspace_root.clone(),
            Arc::clone(&workspace_files_cache),
            Arc::clone(&search_seq),
            Arc::clone(&is_indexing_workspace),
            wakeup.clone(),
        );
        if wakeup_enabled {
            let _ = search_cmd_tx.send(SearchWorkerCmd::ReindexWorkspace);
        }
        let now = Instant::now();

        let open_buffers = pick_initial_buffers(&workspace_root, &project_tree);
        let recent_workspaces = vec![workspace_root.clone()];

        let mut state = Self {
            workspace_root,
            recent_workspaces,
            workspace_title_dirty: false,
            ui_lang,
            system_dpi_scale: 1.0,
            ui_scale: 1.0,
            theme: IdeaTheme::jetbrains_islands_dark(),
            project_tree,
            open_buffers,
            active_tab: 0,
            scroll_line: 0,
            editor_scroll_x: 0,
            tree_scroll: 0,
            agent_scroll: usize::MAX,
            git_changes_scroll: 0,
            git_amend_commit: false,
            git_log_scroll: 0,
            git_diff_scroll: 0,
            git_files_scroll: 0,
            git_branches_scroll: 0,
            selected_git_commit_hash: None,
            selected_git_commit_file: 0,
            git_log_branch_filter: None,
            git_working_diff_target: None,
            git_new_branch_from_ref: None,
            modal_scroll: 0,
            hover_pos: (-1, -1),
            last_hover_hit_idx: None,
            last_hover_splitter: None,
            last_win_size: (1560, 940),
            window_maximized: false,
            pending_window_cmd: None,
            left_tool: Some(ToolWindowId::Project),
            left_panel_width: 268,
            right_agent_open: true,
            right_panel_width: 440,
            right_panel_tab: RightPanelTab::Chat,
            bottom_tool: Some(ToolWindowId::Terminal),
            bottom_panel_height: 240,
            active_splitter_drag: None,
            active_scrollbar_drag: None,
            left_pane_rect: Rect::new(0, 0, 0, 0),
            editor_pane_rect: Rect::new(0, 0, 0, 0),
            editor_code_rect: Rect::new(0, 0, 0, 0),
            right_pane_rect: Rect::new(0, 0, 0, 0),
            bottom_pane_rect: Rect::new(0, 0, 0, 0),
            terminal_content_rect: Rect::new(0, 0, 0, 0),
            git_log_branches_rect: Rect::new(0, 0, 0, 0),
            git_log_commits_rect: Rect::new(0, 0, 0, 0),
            git_log_files_rect: Rect::new(0, 0, 0, 0),
            git_log_diff_rect: Rect::new(0, 0, 0, 0),
            left_scroll_info: ScrollTrackInfo::default(),
            editor_scroll_info: ScrollTrackInfo::default(),
            right_scroll_info: ScrollTrackInfo::default(),
            terminal_scroll_info: ScrollTrackInfo::default(),
            git_log_commits_scroll_info: ScrollTrackInfo::default(),
            git_log_diff_scroll_info: ScrollTrackInfo::default(),
            editor_code_start_x: 380,
            editor_visible_lines: 24,
            editor_char_w: 9,
            terminal_line_h: 19,
            terminal_char_w: 9,
            terminal_scroll_anchor_line: 0,
            last_pty_size: (24, 120),
            tab_rects: Vec::new(),
            project_entry_rects: Vec::new(),
            git_commit_row_rects: Vec::new(),
            git_change_row_rects: Vec::new(),
            hamburger_menu_expanded: false,
            active_menu_category: None,
            menu_category_rects: Vec::new(),
            project_popup_open: false,
            run_config_popup_open: false,
            more_menu_popup_open: false,
            git_branch_popup_open: false,
            agent_provider_popup_open: false,
            agent_model_popup_open: false,
            agent_mode_popup_open: false,
            agent_sessions_popup_open: false,
            agent_mode: agent_hub.normalize_mode_for_active(AgentWorkbenchMode::Agent),
            agent_busy_started_at: None,
            agent_provider_pill_rect: Rect::new(0, 0, 0, 0),
            agent_model_pill_rect: Rect::new(0, 0, 0, 0),
            agent_mode_pill_rect: Rect::new(0, 0, 0, 0),
            attach_active_context: true,
            context_menu: None,
            context_menu_rect: Rect::new(0, 0, 0, 0),
            active_overlay_rects: Vec::new(),
            overlay_hit_start: usize::MAX,
            rename_target_path: None,
            run_configs,
            active_run_config: 0,
            lsp_hub,
            dap_engine,
            git_engine,
            terminal,
            agent_hub,
            focus: FocusTarget::Editor,
            ime_spot: (420, 200),
            caret_visible: true,
            last_input_activity: Instant::now(),
            mouse_drag_target: None,
            ctrl_k_input: TextInputField::new(
                false,
                "将 unwrap() 替换为安全错误处理并补充文档注释",
            ),
            agent_chat_input: TextInputField::multi_line(),
            git_commit_input: TextInputField::single_line(),
            git_log_search_input: TextInputField::single_line(),
            modal_text_input: TextInputField::single_line(),
            search_query_input: TextInputField::single_line(),
            ai_cfg_format: api_cfg.format,
            ai_cfg_url: TextInputField::new(false, &api_cfg.base_url),
            ai_cfg_key: TextInputField::new(false, &api_cfg.api_key),
            ai_cfg_model: TextInputField::new(false, &api_cfg.model),
            ai_cfg_show_key: false,
            agent_input_text_rect: Rect::new(0, 0, 0, 0),
            agent_chat_area_rect: Rect::new(0, 0, 0, 0),
            agent_chat_line_rects: Vec::new(),
            agent_chat_sel_anchor: None,
            agent_chat_sel_cursor: None,
            collapsed_code_blocks: HashMap::new(),
            expanded_message_thoughts: HashMap::new(),
            expanded_composer_snippet_id: None,
            agent_chat_signature_salt: 1,
            image_preview_cache: HashMap::new(),
            active_image_preview: None,
            prev_focus_before_image_preview: None,
            external_drag_hover: None,
            pending_ide_drag: None,
            active_ide_drag: None,
            ctrl_k_input_box_rect: Rect::new(0, 0, 0, 0),
            git_commit_input_box_rect: Rect::new(0, 0, 0, 0),
            git_log_search_rect: Rect::new(0, 0, 0, 0),
            search_modal_text_start_x: 0,
            search_modal_input_rect: Rect::new(0, 0, 0, 0),
            prompt_modal_input_rect: Rect::new(0, 0, 0, 0),
            ai_cfg_field_rects: [Rect::new(0, 0, 0, 0); 3],
            search_results: Vec::new(),
            content_search_hits: Vec::new(),
            search_loading: false,
            pending_open_workspace: None,
            folder_pick_mode: FolderPickMode::AskUser,
            is_indexing_workspace,
            anim_start: now,
            last_anim_tick: now,
            startup_loading_until: if wakeup_enabled {
                now + Duration::from_millis(650)
            } else {
                now
            },
            startup_load_alpha: if wakeup_enabled { 0.85 } else { 0.0 },
            git_load_alpha: if wakeup_enabled { 0.85 } else { 0.0 },
            tree_load_alpha: if wakeup_enabled { 0.85 } else { 0.0 },
            wakeup_enabled,
            wakeup,
            folder_pick_tx,
            folder_pick_rx,
            workspace_files_cache,
            search_seq,
            search_cmd_tx,
            search_event_rx,
            cached_runnables: None,
            cached_structure: None,
            cached_highlight_lines: (PathBuf::new(), usize::MAX, HashMap::new()),
            cached_chat_rows: AgentChatCache::default(),
            hit_regions: Vec::new(),
            status_notification: "IDEA Islands UI 就绪 | 真实文档选区拖拽 · 原生 ConPTY VT100 终端 · AI Agent 工具链".to_string(),
        };

        let first_path = state.active_buffer().path.clone();
        state.dap_engine.toggle_breakpoint(&first_path, 17);
        state
    }

    #[inline]
    pub(crate) fn tr<'a>(&self, zh: &'a str, en: &'a str) -> &'a str {
        match self.ui_lang {
            UiLanguage::ZhCn => zh,
            UiLanguage::EnUs => en,
        }
    }

    #[inline]
    pub fn effective_scale(&self) -> f32 {
        (self.system_dpi_scale * self.ui_scale).clamp(0.75, 4.0)
    }

    pub fn set_ui_scale(&mut self, scale: f32) {
        self.ui_scale = (scale * 20.0).round() / 20.0;
        self.ui_scale = self.ui_scale.clamp(0.85, 2.0);
        let eff_pct = (self.effective_scale() * 100.0).round() as i32;
        let sys_pct = (self.system_dpi_scale * 100.0).round() as i32;
        let ui_pct = (self.ui_scale * 100.0).round() as i32;
        self.status_notification = format!(
            "{}: {}% (System DPI {}% × UI {}%)",
            self.tr("界面与字体缩放已更新", "UI & font scale updated"),
            eff_pct,
            sys_pct,
            ui_pct
        );
    }

    pub fn adapt_panels_for_logical_width(&mut self, log_w: i32) {
        if log_w < 1360 {
            self.left_panel_width = self.left_panel_width.min(220);
            self.right_panel_width = self.right_panel_width.min(350);
            self.bottom_panel_height = self.bottom_panel_height.min(185);
        } else if log_w < 1480 {
            self.left_panel_width = self.left_panel_width.min(244);
            self.right_panel_width = self.right_panel_width.min(390);
            self.bottom_panel_height = self.bottom_panel_height.min(200);
        }
    }

    pub fn workspace_name(&self) -> String {
        self.workspace_root
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| self.workspace_root.display().to_string())
    }

    pub fn workspace_badge(&self) -> String {
        let name = self.workspace_name();
        let parts: Vec<&str> = name
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .collect();
        if parts.len() >= 2 {
            let a = parts[0].chars().next().unwrap_or('W');
            let b = parts[parts.len() - 1].chars().next().unwrap_or('S');
            format!("{a}{b}").to_uppercase()
        } else {
            let chars: Vec<char> = name.chars().filter(|c| c.is_alphanumeric()).take(2).collect();
            if chars.is_empty() {
                "WS".to_string()
            } else {
                chars.into_iter().collect::<String>().to_uppercase()
            }
        }
    }

    pub fn take_workspace_title_dirty(&mut self) -> bool {
        let dirty = self.workspace_title_dirty;
        self.workspace_title_dirty = false;
        dirty
    }

    pub fn request_open_folder_dialog(&mut self) {
        self.request_open_folder_dialog_with_mode(FolderPickMode::AskUser);
    }

    pub fn request_open_folder_dialog_with_mode(&mut self, mode: FolderPickMode) {
        self.folder_pick_mode = mode;
        self.project_popup_open = false;
        self.more_menu_popup_open = false;
        self.active_menu_category = None;
        self.context_menu = None;
        if FOLDER_PICKER_OPEN.swap(true, Ordering::SeqCst) {
            self.status_notification = self
                .tr("文件夹选择对话框已打开...", "Folder picker dialog is already open...")
                .to_string();
            return;
        }
        let initial = self.workspace_root.clone();
        let tx = self.folder_pick_tx.clone();
        let wakeup = self.wakeup.clone();
        self.status_notification = self
            .tr("正在打开系统文件夹选择对话框...", "Opening folder picker dialog...")
            .to_string();
        let _ = thread::Builder::new()
            .name("ide-folder-picker".to_string())
            .spawn(move || {
                let picked = pick_folder_windows(Some(&initial));
                FOLDER_PICKER_OPEN.store(false, Ordering::SeqCst);
                if let Some(path) = picked {
                    let _ = tx.send(path);
                    if let Some(cb) = &wakeup {
                        cb();
                    }
                }
            });
    }

    pub fn open_workspace_in_new_window(&mut self, path: PathBuf) {
        let clean = clean_workspace_path(path);
        if !clean.is_dir() {
            self.status_notification = format!(
                "{}: {}",
                self.tr("无效的文件夹路径", "Invalid folder path"),
                clean.display()
            );
            return;
        }
        self.recent_workspaces.retain(|p| p != &clean);
        self.recent_workspaces.insert(0, clean.clone());
        self.recent_workspaces.truncate(6);
        self.project_popup_open = false;
        self.pending_open_workspace = None;
        if self.focus == FocusTarget::OpenProjectChoiceModal {
            self.focus = FocusTarget::Editor;
        }

        match std::env::current_exe() {
            Ok(exe) => {
                let mut cmd = std::process::Command::new(exe);
                cmd.arg("--workspace").arg(&clean);
                #[cfg(target_os = "windows")]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(0x0000_0200);
                }
                match cmd.spawn() {
                    Ok(_) => {
                        self.status_notification = format!(
                            "{}: {}",
                            self.tr("已在新窗口开启项目", "Opened project in new window"),
                            clean.display()
                        );
                    }
                    Err(e) => {
                        self.status_notification = format!(
                            "{}: {e}",
                            self.tr("无法启动新窗口", "Failed to launch new window")
                        );
                    }
                }
            }
            Err(e) => {
                self.status_notification = format!(
                    "{}: {e}",
                    self.tr("无法获取可执行文件路径", "Cannot locate executable")
                );
            }
        }
    }

    #[inline]
    pub fn spinner_phase(&self) -> f32 {
        self.anim_start.elapsed().as_secs_f32() * 5.2
    }

    #[inline]
    pub fn has_active_animations(&self) -> bool {
        let now = Instant::now();
        let startup_busy = self.wakeup_enabled
            && (now < self.startup_loading_until
                || !self.git_engine.has_loaded_snapshot
                || self.is_indexing_workspace.load(Ordering::Acquire));
        let git_busy =
            self.git_engine.is_busy() || (self.wakeup_enabled && !self.git_engine.has_loaded_snapshot);
        let search_busy = self.search_loading;
        let agent_busy = self.agent_hub.is_busy.load(Ordering::Relaxed);
        startup_busy
            || git_busy
            || search_busy
            || agent_busy
            || self.startup_load_alpha > 0.01
            || self.git_load_alpha > 0.01
            || self.tree_load_alpha > 0.01
    }

    pub fn step_animations(&mut self) -> bool {
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_anim_tick)
            .as_secs_f32()
            .clamp(0.001, 0.05);
        self.last_anim_tick = now;

        let startup_target = if self.wakeup_enabled
            && (now < self.startup_loading_until
                || !self.git_engine.has_loaded_snapshot
                || self.is_indexing_workspace.load(Ordering::Acquire))
        {
            1.0
        } else {
            0.0
        };
        let git_target = if self.git_engine.is_busy()
            || (self.wakeup_enabled && !self.git_engine.has_loaded_snapshot)
        {
            1.0
        } else {
            0.0
        };
        let tree_target = if self.is_indexing_workspace.load(Ordering::Acquire) {
            1.0
        } else {
            0.0
        };

        let k = (dt * 9.5).min(1.0);
        let ease = |val: &mut f32, target: f32| {
            *val += (target - *val) * k;
            if (*val - target).abs() < 0.01 {
                *val = target;
            }
        };
        ease(&mut self.startup_load_alpha, startup_target);
        ease(&mut self.git_load_alpha, git_target);
        ease(&mut self.tree_load_alpha, tree_target);

        self.has_active_animations()
    }

    pub fn switch_workspace(&mut self, new_root: PathBuf) {
        let clean = clean_workspace_path(new_root);
        if !clean.is_dir() {
            self.status_notification = format!(
                "{}: {}",
                self.tr("无效的文件夹路径", "Invalid folder path"),
                clean.display()
            );
            return;
        }

        self.recent_workspaces.retain(|p| p != &clean);
        self.recent_workspaces.insert(0, clean.clone());
        self.recent_workspaces.truncate(6);

        self.workspace_root = clean.clone();
        self.workspace_title_dirty = true;
        self.project_tree = ProjectTreeState::new(&clean);
        self.run_configs = RunConfiguration::detect_for_workspace(&clean);
        self.active_run_config = 0;
        self.lsp_hub = LspHub::new(&clean);
        self.dap_engine = DapDebugEngine::new();
        self.git_engine = if self.wakeup_enabled {
            GitEngine::open_with_wakeup(&clean, self.wakeup.clone())
        } else {
            GitEngine::open(&clean)
        };
        let term_title = self.tr("本地", "Local");
        self.terminal = TerminalSession::new_with_wakeup(term_title, &clean, self.wakeup.clone());
        self.last_pty_size = (0, 0);
        self.agent_hub = AgentHub::new_with_wakeup(&clean, self.wakeup.clone());
        let api_cfg = self.agent_hub.api_config.clone();
        self.ai_cfg_format = api_cfg.format;
        self.ai_cfg_url.set_text(&api_cfg.base_url);
        self.ai_cfg_key.set_text(&api_cfg.api_key);
        self.ai_cfg_model.set_text(&api_cfg.model);

        if let Ok(mut guard) = self.workspace_files_cache.write() {
            *guard = None;
        }
        self.search_seq.fetch_add(1, Ordering::AcqRel);
        self.search_results.clear();
        self.content_search_hits.clear();
        self.search_loading = false;
        let _ = self
            .search_cmd_tx
            .send(SearchWorkerCmd::SetWorkspaceRoot(clean.clone()));
        if self.wakeup_enabled {
            self.is_indexing_workspace.store(true, Ordering::Release);
            let _ = self.search_cmd_tx.send(SearchWorkerCmd::ReindexWorkspace);
            self.startup_loading_until = Instant::now() + Duration::from_millis(550);
            self.startup_load_alpha = 0.85;
            self.git_load_alpha = 0.85;
            self.tree_load_alpha = 0.85;
        }

        self.open_buffers = pick_initial_buffers(&clean, &self.project_tree);
        self.active_tab = 0;
        self.scroll_line = 0;
        self.editor_scroll_x = 0;
        self.tree_scroll = 0;
        self.git_changes_scroll = 0;
        self.git_amend_commit = false;
        self.git_log_scroll = 0;
        self.git_diff_scroll = 0;
        self.git_files_scroll = 0;
        self.git_branches_scroll = 0;
        self.selected_git_commit_hash = None;
        self.selected_git_commit_file = 0;
        self.git_log_branch_filter = None;
        self.git_working_diff_target = None;
        self.git_new_branch_from_ref = None;
        self.git_log_search_input.clear();
        self.agent_scroll = usize::MAX;
        self.modal_scroll = 0;
        self.cached_runnables = None;
        self.cached_structure = None;
        self.cached_highlight_lines = (PathBuf::new(), usize::MAX, HashMap::new());
        self.project_popup_open = false;
        self.git_branch_popup_open = false;
        self.run_config_popup_open = false;
        self.more_menu_popup_open = false;
        self.active_menu_category = None;
        self.context_menu = None;
        self.focus = FocusTarget::Editor;
        self.touch_caret_activity();

        if let Some(first) = self.open_buffers.first().map(|b| b.path.clone()) {
            self.project_tree.selected_path = Some(first);
        }
        self.status_notification = format!(
            "{}: {}",
            self.tr("已打开工作区", "Opened workspace"),
            clean.display()
        );
    }

    pub fn refresh_workspace_file_index_async(&self) {
        let _ = self.search_cmd_tx.send(SearchWorkerCmd::ReindexWorkspace);
    }

    pub fn trigger_search_everywhere(&mut self, max_results: usize) {
        let query = self.search_query_input.as_str().to_string();
        if let Ok(guard) = self.workspace_files_cache.try_read() {
            if let Some(files) = guard.as_ref() {
                self.search_seq.fetch_add(1, Ordering::AcqRel);
                self.search_results = search_everywhere_indexed(files, &query, max_results);
                self.search_loading = false;
                return;
            }
        }
        if !self.wakeup_enabled {
            self.search_results = search_everywhere(&self.workspace_root, &query, max_results);
            self.search_loading = false;
            return;
        }
        let seq = self.search_seq.fetch_add(1, Ordering::AcqRel) + 1;
        self.search_loading = true;
        let _ = self.search_cmd_tx.send(SearchWorkerCmd::SearchEverywhere {
            seq,
            query,
            max_results,
        });
    }

    pub fn trigger_find_in_files(&mut self, max_hits: usize) {
        let pattern = self.search_query_input.as_str().to_string();
        if pattern.trim().is_empty() {
            self.search_seq.fetch_add(1, Ordering::AcqRel);
            self.content_search_hits.clear();
            self.search_loading = false;
            return;
        }
        if !self.wakeup_enabled {
            self.content_search_hits =
                find_in_files(&self.workspace_root, &pattern, max_hits).unwrap_or_default();
            self.search_loading = false;
            return;
        }
        let seq = self.search_seq.fetch_add(1, Ordering::AcqRel) + 1;
        self.search_loading = true;
        let _ = self.search_cmd_tx.send(SearchWorkerCmd::FindInFiles {
            seq,
            pattern,
            max_hits,
        });
    }

    fn ensure_structure_symbols_cached(&mut self) -> &[StructureSymbol] {
        let (path, rev, lang) = {
            let buf = self.active_buffer();
            (buf.path.clone(), buf.revision(), buf.language)
        };
        let needs_rebuild = match &self.cached_structure {
            Some((p, r, _)) => *p != path || *r != rev,
            None => true,
        };
        if needs_rebuild {
            let text = self.active_buffer().text();
            let syms = extract_structure_symbols(&text, lang);
            self.cached_structure = Some((path, rev, syms));
        }
        &self.cached_structure.as_ref().unwrap().2
    }

    fn ensure_runnables_cached(
        &mut self,
        path: &PathBuf,
        rev: usize,
        lang: ide_core::LanguageId,
    ) {
        let needs_rebuild = match &self.cached_runnables {
            Some((p, r, _)) => p != path || *r != rev,
            None => true,
        };
        if needs_rebuild {
            let text = self.active_buffer().text();
            let entries = detect_runnable_gutter_entries(&text, lang);
            self.cached_runnables = Some((path.clone(), rev, entries));
        }
    }

    fn ensure_highlight_lines_cached(
        &mut self,
        path: &PathBuf,
        rev: usize,
        lang: ide_core::LanguageId,
        visible_lines: &[(usize, String)],
    ) {
        if self.cached_highlight_lines.0 != *path || self.cached_highlight_lines.1 != rev {
            self.cached_highlight_lines.0 = path.clone();
            self.cached_highlight_lines.1 = rev;
            self.cached_highlight_lines.2.clear();
        }
        if self.cached_highlight_lines.2.len() > 2048 {
            self.cached_highlight_lines.2.clear();
        }
        for (line_idx, line_str) in visible_lines {
            self.cached_highlight_lines
                .2
                .entry(*line_idx)
                .or_insert_with(|| highlight_line(line_str, lang));
        }
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
            self.editor_scroll_x = 0;
            return;
        }
        if let Ok(buf) = TextBuffer::open(&path) {
            self.open_buffers.push(buf);
            self.active_tab = self.open_buffers.len() - 1;
            self.scroll_line = 0;
            self.editor_scroll_x = 0;
            self.status_notification = format!("已打开: {}", path.display());
        }
    }

    pub fn open_file_at_line(&mut self, path: PathBuf, line_1_based: Option<usize>) {
        self.open_file(path.clone());
        if let Some(l1) = line_1_based {
            let line_0 = l1.saturating_sub(1);
            let max_line = self.active_buffer().line_count().saturating_sub(1);
            let target = line_0.min(max_line);
            let buf = self.active_buffer_mut();
            buf.cursor_line = target;
            buf.cursor_col = 0;
            buf.clear_selection();
            self.scroll_line = target.saturating_sub(4);
            self.focus = FocusTarget::Editor;
            self.touch_caret_activity();
            self.status_notification = format!("已跳转至 {}:{}", path.display(), l1);
        } else {
            self.focus = FocusTarget::Editor;
            self.touch_caret_activity();
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

    pub fn set_ui_language(&mut self, lang: UiLanguage) {
        self.ui_lang = lang;
        let local_label = self.tr("本地", "Local").to_string();
        for tab in &mut self.terminal.tabs {
            if tab.title == "本地" || tab.title == "Local" {
                tab.title = local_label.clone();
            }
        }
        self.status_notification = format!(
            "{}: {}",
            self.tr("界面语言已切换为", "UI language switched to"),
            lang.display_name()
        );
    }

    pub fn execute_ide_action(&mut self, action: IdeAction) {
        self.active_menu_category = None;
        self.context_menu = None;
        match action {
            IdeAction::OpenFolder => self.request_open_folder_dialog(),
            IdeAction::SwitchUiLanguage => {
                let next = self.ui_lang.toggle();
                self.set_ui_language(next);
            }
            IdeAction::SearchEverywhere | IdeAction::GoToFile | IdeAction::FindAction => {
                self.focus = FocusTarget::SearchEverywhereModal;
                self.search_query_input.select_all();
                self.trigger_search_everywhere(14);
                self.touch_caret_activity();
            }
            IdeAction::FindInFiles => {
                self.focus = FocusTarget::FindInFilesModal;
                if self.search_query_input.is_empty() {
                    self.search_query_input.set_text("AgentProviderKind");
                }
                self.search_query_input.select_all();
                self.trigger_find_in_files(20);
                self.touch_caret_activity();
            }
            IdeAction::GoToDeclaration => {
                let first_sym = self
                    .ensure_structure_symbols_cached()
                    .first()
                    .map(|s| (s.name.clone(), s.line));
                if let Some((name, line)) = first_sym {
                    self.active_buffer_mut().set_cursor(line, 0, false);
                    self.scroll_line = line.saturating_sub(3);
                    self.status_notification = format!(
                        "{}: {} ({} {})",
                        self.tr("已跳转至符号定义", "Navigated to symbol"),
                        name,
                        self.tr("第", "Line"),
                        line + 1
                    );
                }
            }
            IdeAction::NewFileInProject => {
                self.modal_text_input.set_text("new_module.rs");
                self.modal_text_input.select_all();
                self.focus = FocusTarget::NewFileModal;
                self.touch_caret_activity();
            }
            IdeAction::NewFolderInProject => {
                self.modal_text_input.set_text("new_package");
                self.modal_text_input.select_all();
                self.focus = FocusTarget::NewFolderModal;
                self.touch_caret_activity();
            }
            IdeAction::DeleteSelectedPath => {
                if let Some(sel) = self.project_tree.selected_path.clone() {
                    if self.project_tree.delete_path(&sel).is_ok() {
                        self.open_buffers.retain(|b| b.path != sel && !b.path.starts_with(&sel));
                        if self.open_buffers.is_empty() {
                            self.open_buffers.push(TextBuffer::from_text(
                                self.workspace_root.join("untitled.rs"),
                                "",
                            ));
                        }
                        if self.active_tab >= self.open_buffers.len() {
                            self.active_tab = self.open_buffers.len() - 1;
                        }
                        self.git_engine.request_refresh();
                        self.refresh_workspace_file_index_async();
                        self.status_notification = format!(
                            "{}: {}",
                            self.tr("已删除", "Deleted"),
                            sel.display()
                        );
                    }
                }
            }
            IdeAction::SaveActiveFile => {
                if self.active_buffer_mut().save().is_ok() {
                    self.git_engine.request_refresh();
                    self.status_notification = self
                        .tr("已保存当前文档", "Saved current file")
                        .to_string();
                }
            }
            IdeAction::CloseActiveTab => {
                let idx = self.active_tab;
                self.close_tab(idx);
            }
            IdeAction::UndoEdit => {
                self.active_buffer_mut().undo();
                self.ensure_editor_cursor_visible();
                self.status_notification = self
                    .tr("已撤销编辑 (Ctrl+Z)", "Undid edit (Ctrl+Z)")
                    .to_string();
            }
            IdeAction::RedoEdit => {
                self.active_buffer_mut().redo();
                self.ensure_editor_cursor_visible();
                self.status_notification = self
                    .tr("已重做编辑 (Ctrl+Shift+Z)", "Redid edit (Ctrl+Shift+Z)")
                    .to_string();
            }
            IdeAction::DuplicateLine => {
                self.active_buffer_mut().duplicate_current_line();
                self.ensure_editor_cursor_visible();
                self.status_notification = self
                    .tr("已复制当前行 (IDEA Ctrl+D)", "Duplicated line (Ctrl+D)")
                    .to_string();
            }
            IdeAction::DeleteLine => {
                self.active_buffer_mut().delete_current_line();
                self.ensure_editor_cursor_visible();
                self.status_notification = self
                    .tr("已删除当前行 (IDEA Ctrl+Y)", "Deleted line (Ctrl+Y)")
                    .to_string();
            }
            IdeAction::ToggleLineComment => {
                self.active_buffer_mut().toggle_line_comment();
                self.status_notification = self
                    .tr("已切换行注释 (IDEA Ctrl+/)", "Toggled line comment (Ctrl+/)")
                    .to_string();
            }
            IdeAction::ReformatCode => {
                self.active_buffer_mut().reformat_basic();
                let _ = self.active_buffer_mut().save();
                self.ensure_editor_cursor_visible();
                self.status_notification = self
                    .tr(
                        "已整理空白并保存文件 (Ctrl+Alt+L)",
                        "Reformatted & saved file (Ctrl+Alt+L)",
                    )
                    .to_string();
            }
            IdeAction::RenameSymbol | IdeAction::ShowIntentionActions => {
                self.trigger_ctrl_k_inline_edit();
            }
            IdeAction::InlineAiEdit => self.trigger_ctrl_k_inline_edit(),
            IdeAction::SendSelectionToAgent => {
                if let Some(ctx) = self.build_active_ide_code_context() {
                    let line_no = ctx.cursor_line + 1;
                    self.agent_hub.submit_workbench_prompt(
                        self.tr(
                            "请审阅并优化当前光标所在代码块",
                            "Please review and optimize the code block around the cursor",
                        ),
                        self.agent_mode,
                        Some(&ctx),
                    );
                    self.right_agent_open = true;
                    self.right_panel_tab = RightPanelTab::Chat;
                    self.agent_scroll = usize::MAX;
                    self.status_notification = format!(
                        "{} {} {} {}",
                        self.tr("已将第", "Sent line"),
                        line_no,
                        self.tr("行上下文发送至", "context to"),
                        self.agent_hub.active_provider.display_name()
                    );
                }
            }
            IdeAction::AcceptInlineDiff => self.accept_inline_diff(),
            IdeAction::RejectInlineDiff => self.reject_inline_diff(),
            IdeAction::RevertLastCheckpoint => self.revert_last_checkpoint(),
            IdeAction::SwitchToAntigravity => {
                self.agent_hub.active_provider = AgentProviderKind::GoogleAntigravityAcp;
                self.agent_mode = self.agent_hub.normalize_mode_for_active(self.agent_mode);
                self.right_agent_open = true;
            }
            IdeAction::SwitchToClaudeCode => {
                self.agent_hub.active_provider = AgentProviderKind::ClaudeCodeCli;
                self.agent_mode = self.agent_hub.normalize_mode_for_active(self.agent_mode);
                self.right_agent_open = true;
            }
            IdeAction::SwitchToCodexCli => {
                self.agent_hub.active_provider = AgentProviderKind::OpenAiCodexCli;
                self.agent_mode = self.agent_hub.normalize_mode_for_active(self.agent_mode);
                self.right_agent_open = true;
            }
            IdeAction::RunActiveConfig => {
                if let Some(cfg) = self.run_configs.get(self.active_run_config).cloned() {
                    self.bottom_tool = Some(ToolWindowId::Terminal);
                    self.focus = FocusTarget::TerminalInput;
                    self.terminal.execute_run_configuration(&cfg);
                    self.touch_caret_activity();
                    self.status_notification = format!(
                        "{}: {}",
                        self.tr("已运行配置", "Running configuration"),
                        cfg.name
                    );
                }
            }
            IdeAction::DebugActiveConfig => {
                let buf = self.active_buffer().clone();
                self.dap_engine
                    .start_debug_session(buf.language, &buf.path, buf.cursor_line);
                self.bottom_tool = Some(ToolWindowId::Debug);
                self.status_notification = format!(
                    "{} ({})",
                    self.tr("DAP 调试会话已启动", "DAP debug session started"),
                    self.dap_engine.active_adapter
                );
            }
            IdeAction::ToggleBreakpoint => {
                let path = self.active_buffer().path.clone();
                let line = self.active_buffer().cursor_line;
                self.dap_engine.toggle_breakpoint(&path, line);
            }
            IdeAction::GitCommitPopup | IdeAction::ToggleCommitWindow => {
                self.git_engine.request_refresh();
                self.left_tool = Some(ToolWindowId::Commit);
            }
            IdeAction::GitPull => {
                self.git_engine.request_pull();
                self.status_notification = self
                    .tr("正在后台执行 Git Pull...", "Running Git Pull in background...")
                    .to_string();
            }
            IdeAction::GitPush => {
                self.git_engine.request_push();
                self.status_notification = self
                    .tr("正在后台执行 Git Push...", "Running Git Push in background...")
                    .to_string();
            }
            IdeAction::GitNewBranch => {
                self.modal_text_input.set_text("feature/ai-agent-flow");
                self.modal_text_input.select_all();
                self.focus = FocusTarget::NewGitBranchModal;
                self.touch_caret_activity();
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
                    self.focus = FocusTarget::TerminalInput;
                    self.touch_caret_activity();
                    Some(ToolWindowId::Terminal)
                };
            }
            IdeAction::OpenSettingsModal => {
                self.focus = FocusTarget::SettingsModal;
            }
            IdeAction::SwitchTheme => {
                let next_kind = self.theme.kind.next();
                self.theme = IdeaTheme::from_kind(next_kind);
                self.status_notification = format!(
                    "{}: {}",
                    self.tr("已切换主题为", "Switched theme to"),
                    next_kind.display_name()
                );
            }
            IdeAction::ZoomInUi => {
                let next = (self.ui_scale + 0.10).min(2.0);
                self.set_ui_scale(next);
            }
            IdeAction::ZoomOutUi => {
                let next = (self.ui_scale - 0.10).max(0.85);
                self.set_ui_scale(next);
            }
            IdeAction::ResetUiZoom => {
                self.set_ui_scale(1.0);
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

    pub fn build_active_ide_code_context(&self) -> Option<IdeCodeContext> {
        let buf = self.open_buffers.get(self.active_tab)?;
        let diag_summary = self
            .lsp_hub
            .diagnostics
            .iter()
            .take(4)
            .map(|d| format!("{}:{} {}", d.file_path.display(), d.line, d.message))
            .collect::<Vec<_>>()
            .join("; ");
        Some(IdeCodeContext {
            file_path: buf.path.clone(),
            language: buf.language,
            file_text: buf.text(),
            cursor_line: buf.cursor_line,
            diagnostics_summary: diag_summary,
            git_changed_count: self.git_engine.changed_files.len(),
        })
    }

    pub fn submit_agent_prompt(&mut self, prompt: String) {
        self.agent_mode = self.agent_hub.normalize_mode_for_active(self.agent_mode);
        let ctx = if self.attach_active_context {
            self.build_active_ide_code_context()
        } else {
            None
        };
        self.expanded_composer_snippet_id = None;
        self.agent_hub
            .submit_workbench_prompt(&prompt, self.agent_mode, ctx.as_ref());
        self.agent_busy_started_at = if self.agent_hub.is_active_session_busy() {
            Some(Instant::now())
        } else {
            None
        };
        self.right_agent_open = true;
        self.right_panel_tab = RightPanelTab::Chat;
        self.agent_scroll = usize::MAX;
        self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
        self.status_notification = format!(
            "已发送至 {} ({})",
            self.agent_hub.active_provider.display_name(),
            self.agent_mode.label()
        );
    }

    pub fn create_new_agent_session(&mut self) {
        let draft = self.agent_chat_input.as_str().to_string();
        self.agent_hub.active_session_mut().draft_prompt = draft;
        let idx = self.agent_hub.new_session();
        let title = self.agent_hub.sessions[idx].title.clone();
        self.agent_chat_input.clear();
        self.clear_agent_chat_selection();
        self.collapsed_code_blocks.clear();
        self.expanded_message_thoughts.clear();
        self.expanded_composer_snippet_id = None;
        self.agent_sessions_popup_open = false;
        self.agent_mode_popup_open = false;
        self.agent_busy_started_at = None;
        self.right_agent_open = true;
        self.right_panel_tab = RightPanelTab::Chat;
        self.agent_scroll = 0;
        self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
        self.focus = FocusTarget::AgentChatInput;
        self.touch_caret_activity();
        self.status_notification = format!("✦ 已新建并切换到 {title}");
    }

    pub fn switch_agent_session(&mut self, target_idx: usize) {
        if target_idx == self.agent_hub.active_session_idx
            || target_idx >= self.agent_hub.sessions.len()
        {
            self.agent_sessions_popup_open = false;
            return;
        }
        let draft = self.agent_chat_input.as_str().to_string();
        self.agent_hub.active_session_mut().draft_prompt = draft;
        if self.agent_hub.switch_session(target_idx) {
            let next_draft = self.agent_hub.active_session().draft_prompt.clone();
            let title = self.agent_hub.active_session().title.clone();
            self.agent_chat_input.set_text(&next_draft);
            self.ai_cfg_model.set_text(&self.agent_hub.active_model());
            self.agent_mode = self.agent_hub.normalize_mode_for_active(self.agent_mode);
            self.clear_agent_chat_selection();
            self.collapsed_code_blocks.clear();
            self.expanded_message_thoughts.clear();
            self.expanded_composer_snippet_id = None;
            self.agent_sessions_popup_open = false;
            self.agent_mode_popup_open = false;
            self.agent_busy_started_at = if self.agent_hub.is_active_session_busy() {
                Some(Instant::now())
            } else {
                None
            };
            self.agent_scroll = usize::MAX;
            self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
            self.focus = FocusTarget::AgentChatInput;
            self.touch_caret_activity();
            self.status_notification = format!("已切换会话: {title}");
        }
    }

    pub fn close_agent_session(&mut self, idx: usize) {
        let draft = self.agent_chat_input.as_str().to_string();
        self.agent_hub.active_session_mut().draft_prompt = draft;
        if self.agent_hub.close_session(idx) {
            let next_draft = self.agent_hub.active_session().draft_prompt.clone();
            let title = self.agent_hub.active_session().title.clone();
            self.agent_chat_input.set_text(&next_draft);
            self.agent_mode = self.agent_hub.normalize_mode_for_active(self.agent_mode);
            self.clear_agent_chat_selection();
            self.collapsed_code_blocks.clear();
            self.expanded_message_thoughts.clear();
            self.expanded_composer_snippet_id = None;
            self.agent_busy_started_at = if self.agent_hub.is_active_session_busy() {
                Some(Instant::now())
            } else {
                None
            };
            self.agent_scroll = usize::MAX;
            self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
            self.status_notification = format!("已关闭会话 · 当前: {title}");
        }
    }

    pub fn get_or_load_image_preview(&mut self, path: &Path) -> Option<Arc<DecodedPreviewImage>> {
        if let Some(cached) = self.image_preview_cache.get(path) {
            return Some(Arc::clone(cached));
        }
        let decoded = load_image_for_preview(path)?;
        let arc = Arc::new(decoded);
        self.image_preview_cache
            .insert(path.to_path_buf(), Arc::clone(&arc));
        Some(arc)
    }

    pub fn attach_path_to_agent(&mut self, path: PathBuf) {
        let clean = clean_workspace_path(path);
        let display_name = clean
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| clean.display().to_string());
        let is_img = is_image_file_path(&clean);
        let is_dir = clean.is_dir();
        let (w, h) = if is_img {
            self.get_or_load_image_preview(&clean)
                .map(|im| (im.orig_width, im.orig_height))
                .unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        self.agent_hub
            .attach_item_to_active_session(AgentAttachedItem {
                path: clean,
                display_name: display_name.clone(),
                is_image: is_img,
                is_dir,
                width: w,
                height: h,
            });
        self.right_agent_open = true;
        self.right_panel_tab = RightPanelTab::Chat;
        self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
        self.focus = FocusTarget::AgentChatInput;
        self.touch_caret_activity();
        self.status_notification = if is_img {
            if w > 0 && h > 0 {
                format!("🖼️ 已附加图片 `{display_name}` ({w}×{h}) — 支持直接预览")
            } else {
                format!("🖼️ 已附加图片 `{display_name}`")
            }
        } else if is_dir {
            format!("📁 已附加目录 `{display_name}` 到智能体会话")
        } else {
            format!("📎 已附加文件 `{display_name}` 到智能体会话")
        };
    }

    pub fn handle_os_dropped_file(&mut self, path: PathBuf, lx: i32, ly: i32) {
        self.external_drag_hover = None;
        let is_img = is_image_file_path(&path);
        let in_agent_zone = (self.right_agent_open && self.right_pane_rect.contains(lx, ly))
            || is_img;
        if in_agent_zone {
            self.attach_path_to_agent(path);
        } else if path.is_file() {
            self.open_file(path);
            self.focus = FocusTarget::Editor;
            self.touch_caret_activity();
        } else if self.right_agent_open {
            self.attach_path_to_agent(path);
        }
    }

    pub fn paste_into_agent_or_focus(&mut self) {
        if self.focus == FocusTarget::AgentChatInput {
            match read_windows_clipboard_payload() {
                Some(ClipboardPastePayload::Files(paths)) => {
                    for p in paths {
                        self.attach_path_to_agent(p);
                    }
                    return;
                }
                Some(ClipboardPastePayload::ImageFile(img_path)) => {
                    self.attach_path_to_agent(img_path);
                    return;
                }
                Some(ClipboardPastePayload::Text(text)) => {
                    if let Some((title, lang, line_count)) = detect_pasted_code_snippet(&text) {
                        let id = format!("paste-{}", self.agent_chat_signature_salt);
                        self.agent_hub
                            .add_pasted_snippet_to_active_session(PastedCodeSnippet {
                                id: id.clone(),
                                title,
                                language: lang.clone(),
                                content: text,
                                line_count,
                            });
                        self.agent_chat_signature_salt =
                            self.agent_chat_signature_salt.wrapping_add(1);
                        self.status_notification = format!(
                            "📋 已折叠粘贴 {lang} 代码 ({line_count} 行) — 点击标签可展开/预览"
                        );
                    } else {
                        self.insert_text_at_focus(&text);
                    }
                    return;
                }
                None => {}
            }
        }
        if let Some(clip) = read_windows_clipboard_text() {
            self.insert_text_at_focus(&clip);
        }
    }

    pub fn begin_ide_drag_candidate_at(&mut self, x: i32, y: i32) {
        if self.active_overlay_rects.iter().any(|r| r.contains(x, y)) {
            return;
        }
        if let Some((_, p, _)) = self
            .project_entry_rects
            .iter()
            .find(|(r, _, _)| r.contains(x, y))
        {
            self.pending_ide_drag = Some((p.clone(), (x, y)));
            return;
        }
        if let Some((_, p, _)) = self
            .git_change_row_rects
            .iter()
            .find(|(r, _, _)| r.contains(x, y))
        {
            self.pending_ide_drag = Some((p.clone(), (x, y)));
        }
    }

    pub fn update_ide_drag(&mut self, x: i32, y: i32) -> bool {
        if let Some((ref path, _)) = self.active_ide_drag {
            let path = path.clone();
            self.active_ide_drag = Some((path, (x, y)));
            return true;
        }
        if let Some((path, (ox, oy))) = self.pending_ide_drag.clone() {
            if (x - ox).abs() + (y - oy).abs() > 7 {
                self.pending_ide_drag = None;
                self.active_ide_drag = Some((path, (x, y)));
                return true;
            }
        }
        false
    }

    pub fn finish_ide_drag_at(&mut self, x: i32, y: i32) -> bool {
        self.pending_ide_drag = None;
        let Some((path, _)) = self.active_ide_drag.take() else {
            return false;
        };
        if self.right_agent_open && self.right_pane_rect.contains(x, y) {
            self.attach_path_to_agent(path);
            return true;
        }
        true
    }

    pub fn open_image_preview(&mut self, path: PathBuf) {
        let _ = self.get_or_load_image_preview(&path);
        if self.focus != FocusTarget::ImagePreviewModal {
            self.prev_focus_before_image_preview = Some(self.focus);
        }
        self.active_image_preview = Some(path);
        self.focus = FocusTarget::ImagePreviewModal;
    }

    pub fn close_image_preview(&mut self) {
        self.active_image_preview = None;
        let next_focus = self
            .prev_focus_before_image_preview
            .take()
            .unwrap_or(FocusTarget::AgentChatInput);
        self.focus = if next_focus == FocusTarget::ImagePreviewModal {
            FocusTarget::AgentChatInput
        } else {
            next_focus
        };
    }

    pub fn open_ai_config_modal(&mut self) {
        self.agent_hub.refresh_local_cli_agents();
        self.ai_cfg_model.set_text(&self.agent_hub.active_model());
        self.focus = FocusTarget::AiConfigModal(0);
        self.touch_caret_activity();
    }

    pub fn save_ai_config_modal(&mut self) {
        let model_name = self.ai_cfg_model.as_str().trim().to_string();
        if !model_name.is_empty() {
            self.agent_hub.select_model_for_active(model_name.clone());
        }
        self.focus = FocusTarget::AgentChatInput;
        self.touch_caret_activity();
        self.status_notification = format!(
            "✓ 已同步本地 CLI `{}` 模型: {}",
            self.agent_hub.active_provider.short_name(),
            self.agent_hub.active_model()
        );
    }

    pub fn apply_chat_patch(&mut self, msg_idx: usize) {
        let Some(patch) = self
            .agent_hub
            .messages
            .get(msg_idx)
            .and_then(|m| m.patch.clone())
        else {
            return;
        };
        if let Some(pos) = self.open_buffers.iter().position(|b| b.path == patch.file_path) {
            self.active_tab = pos;
        } else {
            self.open_file(patch.file_path.clone());
        }

        let full_text = self.active_buffer().text();
        let path_clone = self.active_buffer().path.clone();
        self.agent_hub.create_checkpoint(
            format!("Before AI Patch {}", patch.summary),
            &[(&path_clone, &full_text)],
        );

        let buf = self.active_buffer_mut();
        buf.replace_line_range(patch.start_line, patch.end_line, &patch.proposed_code);
        buf.cursor_line = patch.start_line;
        buf.cursor_col = 0;
        self.ensure_editor_cursor_visible();
        self.focus = FocusTarget::Editor;

        if let Some(m) = self.agent_hub.messages.get_mut(msg_idx) {
            if let Some(p) = &mut m.patch {
                p.applied = true;
            }
        }
        self.git_engine.request_refresh();
        self.status_notification = format!(
            "✓ 已将 AI 补丁应用至 {} (支持 Ctrl+Alt+Z 回滚)",
            patch.summary
        );
    }

    pub fn insert_chat_patch(&mut self, msg_idx: usize) {
        let Some(patch) = self
            .agent_hub
            .messages
            .get(msg_idx)
            .and_then(|m| m.patch.clone())
        else {
            return;
        };
        let full_text = self.active_buffer().text();
        let path_clone = self.active_buffer().path.clone();
        self.agent_hub.create_checkpoint(
            format!("Before Insert AI Patch {}", patch.summary),
            &[(&path_clone, &full_text)],
        );
        self.active_buffer_mut()
            .insert_str_at_cursor(&patch.proposed_code);
        self.ensure_editor_cursor_visible();
        self.focus = FocusTarget::Editor;
        if let Some(m) = self.agent_hub.messages.get_mut(msg_idx) {
            if let Some(p) = &mut m.patch {
                p.applied = true;
            }
        }
        self.status_notification = format!("✓ 已将补丁代码插入当前光标处 ({})", patch.summary);
    }

    pub fn preview_chat_patch_in_editor(&mut self, msg_idx: usize) {
        let Some(patch) = self
            .agent_hub
            .messages
            .get(msg_idx)
            .and_then(|m| m.patch.clone())
        else {
            return;
        };
        if let Some(pos) = self.open_buffers.iter().position(|b| b.path == patch.file_path) {
            self.active_tab = pos;
        } else {
            self.open_file(patch.file_path.clone());
        }
        self.active_buffer_mut().cursor_line = patch.start_line;
        self.ensure_editor_cursor_visible();
        self.agent_hub.inline_edit = Some(ide_agent_hub::InlineEditSession::create(
            patch.file_path,
            patch.language,
            patch.start_line,
            patch.end_line,
            patch.summary.clone(),
            patch.original_code,
            patch.proposed_code,
        ));
        self.focus = FocusTarget::CtrlKPrompt;
        self.touch_caret_activity();
        self.status_notification =
            "已在中栏打开行内 Diff 预览：按 Tab 接受，按 Esc 拒绝".to_string();
    }

    pub fn take_window_control_cmd(&mut self) -> Option<WindowControlCmd> {
        self.pending_window_cmd.take()
    }

    pub fn is_point_in_toolbar_blank(&self, x: i32, y: i32) -> bool {
        if y < 0 || y >= 38 {
            return false;
        }
        !self.hit_regions.iter().any(|(r, _)| r.contains(x, y))
    }

    /// 鼠标移动悬浮追踪：支持菜单栏划动切换展开子菜单 + 分割线/控件悬浮高亮按需重绘
    pub fn handle_mouse_move(&mut self, x: i32, y: i32) -> bool {
        self.hover_pos = (x, y);
        let mut changed = false;

        // 展开主菜单时，鼠标划过其他一级菜单分类自动切换下拉菜单（1:1 原生桌面菜单体验）
        if self.hamburger_menu_expanded && self.active_menu_category.is_some() {
            if let Some((cat, _)) = self
                .menu_category_rects
                .iter()
                .find(|(_, r)| r.contains(x, y))
            {
                if self.active_menu_category != Some(*cat) {
                    self.active_menu_category = Some(*cat);
                    changed = true;
                }
            }
        }

        let hover_idx = self
            .hit_regions
            .iter()
            .rposition(|(r, _)| r.contains(x, y));
        if hover_idx != self.last_hover_hit_idx {
            self.last_hover_hit_idx = hover_idx;
            changed = true;
        }

        let (win_w, win_h) = self.last_win_size;
        let hover_splitter = self.splitter_at(x, y, win_w, win_h);
        if hover_splitter != self.last_hover_splitter {
            self.last_hover_splitter = hover_splitter;
            changed = true;
        }
        changed
    }

    /// 命中左/右/底三处面板分割线（与 `render()` 坐标完全对齐）
    pub fn splitter_at(&self, x: i32, y: i32, win_w: i32, win_h: i32) -> Option<DragSplitter> {
        let top_bar_h = 38;
        let status_bar_h = 26;
        let stripe_w = 42;
        if win_w <= stripe_w * 2 || win_h <= top_bar_h + status_bar_h {
            return None;
        }
        let body_h = (win_h - top_bar_h - status_bar_h).max(120);
        let bottom_h = if self.bottom_tool.is_some() {
            self.bottom_panel_height
        } else {
            0
        };
        let upper_h = (body_h - bottom_h).max(100);
        let upper_bottom = top_bar_h + upper_h;

        // 遮罩层/菜单激活时不穿透命中底层分割线
        if self.context_menu.is_some()
            || matches!(
                self.focus,
                FocusTarget::SearchEverywhereModal
                    | FocusTarget::FindInFilesModal
                    | FocusTarget::NewFileModal
                    | FocusTarget::NewFolderModal
                    | FocusTarget::NewGitBranchModal
                    | FocusTarget::RenamePathModal
                    | FocusTarget::SettingsModal
                    | FocusTarget::AiConfigModal(_)
            )
            || self
                .active_overlay_rects
                .iter()
                .any(|r| r.contains(x, y))
        {
            return None;
        }

        if self.left_tool.is_some() && y > top_bar_h && y < upper_bottom {
            let split_x = stripe_w + self.left_panel_width;
            if (x - split_x).abs() <= 5 {
                return Some(DragSplitter::LeftPanel);
            }
        }
        if self.right_agent_open && y > top_bar_h && y < upper_bottom {
            let split_x = win_w - stripe_w - self.right_panel_width;
            if (x - split_x).abs() <= 5 {
                return Some(DragSplitter::RightPanel);
            }
        }
        if self.bottom_tool.is_some() && x > stripe_w && x < win_w - stripe_w {
            if (y - upper_bottom).abs() <= 5 {
                return Some(DragSplitter::BottomPanel);
            }
        }
        None
    }

    /// 根据鼠标所在控件返回应切换的原生指针形态
    pub fn cursor_kind_at(&self, x: i32, y: i32, win_w: i32, win_h: i32) -> IdeCursorKind {
        // 1. 拖拽进行中锁定指针，防止快速移动闪烁
        if let Some(drag) = self.active_splitter_drag {
            return match drag {
                DragSplitter::LeftPanel | DragSplitter::RightPanel => IdeCursorKind::EwResize,
                DragSplitter::BottomPanel => IdeCursorKind::NsResize,
            };
        }
        if self.active_scrollbar_drag.is_some() {
            return IdeCursorKind::Default;
        }
        if self.mouse_drag_target.is_some() {
            return IdeCursorKind::Text;
        }

        // 2. 右键上下文菜单优先
        if self.context_menu.is_some() {
            if let Some((_, act)) = self
                .hit_regions
                .iter()
                .rev()
                .find(|(r, _)| r.contains(x, y))
            {
                if matches!(act, HitAction::TriggerContextMenuAction(_)) {
                    return IdeCursorKind::Pointer;
                }
            }
            return IdeCursorKind::Default;
        }

        let overlay_hits: &[(Rect, HitAction)] = if self.overlay_hit_start <= self.hit_regions.len() {
            &self.hit_regions[self.overlay_hit_start..]
        } else {
            &[]
        };

        // 3. 顶层模态弹窗隔离
        let modal_open = matches!(
            self.focus,
            FocusTarget::SearchEverywhereModal
                | FocusTarget::FindInFilesModal
                | FocusTarget::NewFileModal
                | FocusTarget::NewFolderModal
                | FocusTarget::NewGitBranchModal
                | FocusTarget::RenamePathModal
                | FocusTarget::SettingsModal
                | FocusTarget::AiConfigModal(_)
                | FocusTarget::OpenProjectChoiceModal
        );
        if modal_open {
            match self.focus {
                FocusTarget::SearchEverywhereModal | FocusTarget::FindInFilesModal => {
                    if self.search_modal_input_rect.contains(x, y) {
                        return IdeCursorKind::Text;
                    }
                }
                FocusTarget::NewFileModal
                | FocusTarget::NewFolderModal
                | FocusTarget::NewGitBranchModal
                | FocusTarget::RenamePathModal => {
                    if self.prompt_modal_input_rect.contains(x, y) {
                        return IdeCursorKind::Text;
                    }
                }
                FocusTarget::AiConfigModal(_) => {
                    if self.ai_cfg_field_rects.iter().any(|r| r.contains(x, y)) {
                        return IdeCursorKind::Text;
                    }
                }
                _ => {}
            }
            if self.active_overlay_rects.iter().any(|r| r.contains(x, y)) {
                if overlay_hits.iter().rev().any(|(r, _)| r.contains(x, y)) {
                    return IdeCursorKind::Pointer;
                }
            }
            return IdeCursorKind::Default;
        }

        // 4. 顶层弹出下拉菜单（主菜单 / 项目 / 分支 / 运行配置 / 更多 / AI 引擎）
        if self.active_overlay_rects.iter().any(|r| r.contains(x, y)) {
            if overlay_hits.iter().rev().any(|(r, _)| r.contains(x, y)) {
                return IdeCursorKind::Pointer;
            }
            return IdeCursorKind::Default;
        }

        // 5. 面板分割线（左右 <-> 垂直）
        if let Some(splitter) = self.splitter_at(x, y, win_w, win_h) {
            return match splitter {
                DragSplitter::LeftPanel | DragSplitter::RightPanel => IdeCursorKind::EwResize,
                DragSplitter::BottomPanel => IdeCursorKind::NsResize,
            };
        }

        // 6. 滚动条轨道保持标准箭头，避免落入下层 I-beam
        let on_scrollbar = (self.left_tool.is_some() && self.left_scroll_info.track.contains(x, y))
            || self.editor_scroll_info.track.contains(x, y)
            || (self.right_agent_open && self.right_scroll_info.track.contains(x, y))
            || (self.bottom_tool == Some(ToolWindowId::Terminal)
                && self.terminal_scroll_info.track.contains(x, y))
            || (self.bottom_tool == Some(ToolWindowId::GitLog)
                && (self.git_log_commits_scroll_info.track.contains(x, y)
                    || self.git_log_diff_scroll_info.track.contains(x, y)));
        if on_scrollbar {
            return IdeCursorKind::Default;
        }

        // 7. 最上层交互区域（I-beam 文本区 vs 手型可点击控件）
        if let Some((_, act)) = self
            .hit_regions
            .iter()
            .rev()
            .find(|(r, _)| r.contains(x, y))
        {
            return match act {
                HitAction::FocusEditorBody
                | HitAction::ClickEditorLine
                | HitAction::FocusTerminalInput
                | HitAction::FocusAgentChatInput
                | HitAction::FocusAgentChatStream
                | HitAction::FocusCtrlKPrompt
                | HitAction::FocusGitCommitMessageInput
                | HitAction::FocusGitLogSearchInput => IdeCursorKind::Text,
                _ => IdeCursorKind::Pointer,
            };
        }

        // 8. 兜底：编辑器与终端内部文本区
        if self.editor_code_rect.contains(x, y)
            || (self.bottom_tool == Some(ToolWindowId::Terminal)
                && self.terminal_content_rect.contains(x, y))
        {
            return IdeCursorKind::Text;
        }

        IdeCursorKind::Default
    }

    fn is_action_checked(&self, action: IdeAction) -> bool {
        match action {
            IdeAction::ToggleProjectWindow => self.left_tool == Some(ToolWindowId::Project),
            IdeAction::ToggleCommitWindow => self.left_tool == Some(ToolWindowId::Commit),
            IdeAction::ToggleStructureWindow => self.left_tool == Some(ToolWindowId::Structure),
            IdeAction::ToggleAgentHubWindow => self.right_agent_open,
            IdeAction::ToggleTerminalWindow => self.bottom_tool == Some(ToolWindowId::Terminal),
            IdeAction::SwitchToAntigravity => {
                self.agent_hub.active_provider == AgentProviderKind::GoogleAntigravityAcp
            }
            IdeAction::SwitchToClaudeCode => {
                self.agent_hub.active_provider == AgentProviderKind::ClaudeCodeCli
            }
            IdeAction::SwitchToCodexCli => {
                self.agent_hub.active_provider == AgentProviderKind::OpenAiCodexCli
            }
            _ => false,
        }
    }

    pub fn trigger_ctrl_k_inline_edit(&mut self) {
        self.focus = FocusTarget::CtrlKPrompt;
        self.touch_caret_activity();
        let buf = self.active_buffer().clone();
        let (start, end) = if let Some(((sl, _), (el, ec))) = buf.selection_range() {
            let end_line = if ec == 0 && el > sl { el - 1 } else { el };
            (sl, end_line)
        } else {
            let s = buf.cursor_line.min(buf.line_count().saturating_sub(1));
            let e = (s + 5).min(buf.line_count().saturating_sub(1));
            (s, e)
        };
        let slice = buf.slice_lines(start, end);
        let prompt = self.ctrl_k_input.as_str().to_string();

        let full_text = buf.text();
        let _ckpt_id = self.agent_hub.create_checkpoint(
            format!("Before Ctrl+K on {}", buf.path.display()),
            &[(&buf.path, &full_text)],
        );

        let session = self.agent_hub.propose_ctrl_k_inline_edit(
            buf.path,
            buf.language,
            start,
            end,
            &slice,
            &prompt,
        );
        if let Some(err) = session.error.as_deref() {
            self.status_notification = format!("Ctrl+K: {err}");
        } else if session.loading {
            self.status_notification = format!(
                "正在请求 AI 生成第 {}..{} 行行内 Diff...",
                start + 1,
                end + 1
            );
        } else {
            self.status_notification =
                "Cursor Ctrl+K 行内修改已生成：按 Tab 接受修改，按 Esc 拒绝".to_string();
        }
    }

    pub fn accept_inline_diff(&mut self) {
        if let Some(session) = self.agent_hub.inline_edit.take() {
            if session.active && !session.loading && session.error.is_none() {
                let buf = self.active_buffer_mut();
                buf.replace_line_range(session.start_line, session.end_line, &session.proposed_code);
                self.focus = FocusTarget::Editor;
                self.ensure_editor_cursor_visible();
                self.status_notification = format!(
                    "已应用 AI 行内修改（第 {}..{} 行，可随时点击快照回滚）",
                    session.start_line + 1,
                    session.end_line + 1
                );
            } else {
                self.focus = FocusTarget::Editor;
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
            for (path, backup) in ckpt.file_backups {
                match backup {
                    Some(content) => {
                        let _ = std::fs::write(&path, &content);
                        if let Some(buf) = self.open_buffers.iter_mut().find(|b| b.path == path) {
                            *buf = TextBuffer::from_text(path, &content);
                        }
                    }
                    None => {
                        let _ = std::fs::remove_file(&path);
                        self.open_buffers.retain(|b| b.path != path);
                    }
                }
            }
            if self.open_buffers.is_empty() {
                self.open_buffers.push(TextBuffer::from_text(
                    self.workspace_root.join("untitled.rs"),
                    "",
                ));
            }
            if self.active_tab >= self.open_buffers.len() {
                self.active_tab = self.open_buffers.len() - 1;
            }
            self.project_tree.rebuild();
            self.git_engine.request_refresh();
            self.refresh_workspace_file_index_async();
            self.status_notification = format!("已回滚至快照 Checkpoint: {}", ckpt.id);
        } else {
            self.status_notification = "当前没有可回滚的 Checkpoint 快照".to_string();
        }
    }

    pub fn poll_background_updates(&mut self) -> bool {
        let mut changed = false;
        let git_outcome = self.git_engine.poll_updates();
        if git_outcome.changed {
            changed = true;
        }
        if git_outcome.clear_commit_input {
            self.git_commit_input.clear();
            self.git_amend_commit = false;
        }
        if self.selected_git_commit_hash.is_none() {
            if let Some(first) = self.git_engine.recent_commits.first() {
                self.selected_git_commit_hash = Some(first.hash.clone());
            }
        }
        if let Some(msg) = git_outcome.notification {
            self.status_notification = msg;
        }
        if let Some((diff_text, changed_files)) = git_outcome.ai_diff_ready {
            self.agent_hub
                .request_ai_commit_message(diff_text, changed_files);
            self.status_notification = "正在请求 AI 根据 Git Diff 生成 Commit 信息...".to_string();
        }
        while let Ok(ev) = self.search_event_rx.try_recv() {
            let cur_seq = self.search_seq.load(Ordering::Acquire);
            match ev {
                AsyncSearchEvent::SearchEverywhereReady { seq, items } => {
                    if seq == cur_seq {
                        self.search_results = items;
                        self.search_loading = false;
                        changed = true;
                    }
                }
                AsyncSearchEvent::FindInFilesReady { seq, hits } => {
                    if seq == cur_seq {
                        self.content_search_hits = hits;
                        self.search_loading = false;
                        changed = true;
                    }
                }
            }
        }
        if self.agent_hub.poll_async_messages() {
            self.agent_scroll = usize::MAX;
            changed = true;
        }
        if self.agent_hub.is_active_session_busy() {
            if self.agent_busy_started_at.is_none() {
                self.agent_busy_started_at = Some(Instant::now());
            }
        } else {
            self.agent_busy_started_at = None;
        }
        if self.agent_hub.poll_inline_edit_result() {
            if let Some(s) = &self.agent_hub.inline_edit {
                if let Some(err) = &s.error {
                    self.status_notification = format!("Ctrl+K 失败: {err}");
                } else {
                    self.status_notification =
                        "AI 行内 Diff 已生成：按 Tab 接受修改，按 Esc 拒绝".to_string();
                }
            }
            changed = true;
        }
        let modified = self.agent_hub.poll_modified_files();
        if !modified.is_empty() {
            for p in &modified {
                if let Some(buf) = self.open_buffers.iter_mut().find(|b| b.path == *p) {
                    if let Ok(fresh) = TextBuffer::open(p) {
                        *buf = fresh;
                    }
                }
            }
            self.project_tree.rebuild();
            self.git_engine.request_refresh();
            self.refresh_workspace_file_index_async();
            self.status_notification =
                format!("AI Agent 已更新 {} 个文件并热刷新缓冲区", modified.len());
            changed = true;
        }
        if let Some(res) = self.agent_hub.poll_commit_message() {
            match res {
                Ok(msg) => {
                    self.git_commit_input.set_text(&msg);
                    self.git_engine.commit_message_draft = msg.clone();
                    self.status_notification = format!(
                        "{}: {msg}",
                        self.tr("AI 已生成 Commit 信息", "AI generated commit message")
                    );
                }
                Err(err) => {
                    self.status_notification = err;
                }
            }
            changed = true;
        }
        while let Ok(picked) = self.folder_pick_rx.try_recv() {
            match self.folder_pick_mode {
                FolderPickMode::NewWindow => {
                    self.folder_pick_mode = FolderPickMode::AskUser;
                    self.open_workspace_in_new_window(picked);
                }
                FolderPickMode::AskUser => {
                    let clean = clean_workspace_path(picked);
                    self.pending_open_workspace = Some(clean);
                    self.focus = FocusTarget::OpenProjectChoiceModal;
                }
            }
            changed = true;
        }
        changed
    }

    pub fn active_terminal_revision(&self) -> u64 {
        self.terminal.active_pty().current_revision()
    }

    pub fn ensure_editor_cursor_visible(&mut self) {
        let (cursor_line, cursor_col, line_str) = {
            let buf = self.active_buffer();
            (buf.cursor_line, buf.cursor_col, buf.line_text(buf.cursor_line))
        };
        let vis = self.editor_visible_lines.max(4);
        if cursor_line < self.scroll_line {
            self.scroll_line = cursor_line;
        } else if cursor_line >= self.scroll_line + vis {
            self.scroll_line = cursor_line.saturating_sub(vis - 1);
        }

        let char_w = self.editor_char_w.max(8);
        let cursor_px: i32 = line_str
            .chars()
            .take(cursor_col)
            .map(|c| if c.is_ascii() { char_w } else { char_w * 2 })
            .sum();
        let avail_w = (self.editor_code_rect.w - 36).max(160);
        if cursor_px < self.editor_scroll_x {
            self.editor_scroll_x = (cursor_px - 24).max(0);
        } else if cursor_px > self.editor_scroll_x + avail_w {
            self.editor_scroll_x = cursor_px - avail_w + 32;
        }
    }

    pub fn touch_caret_activity(&mut self) {
        self.caret_visible = true;
        self.last_input_activity = Instant::now();
    }

    pub fn active_text_input(&self) -> Option<&TextInputField> {
        match self.focus {
            FocusTarget::CtrlKPrompt => Some(&self.ctrl_k_input),
            FocusTarget::AgentChatInput => Some(&self.agent_chat_input),
            FocusTarget::GitCommitInput => Some(&self.git_commit_input),
            FocusTarget::GitLogSearchInput => Some(&self.git_log_search_input),
            FocusTarget::SearchEverywhereModal | FocusTarget::FindInFilesModal => {
                Some(&self.search_query_input)
            }
            FocusTarget::NewFileModal
            | FocusTarget::NewFolderModal
            | FocusTarget::NewGitBranchModal
            | FocusTarget::RenamePathModal => Some(&self.modal_text_input),
            FocusTarget::AiConfigModal(0) => Some(&self.ai_cfg_url),
            FocusTarget::AiConfigModal(1) => Some(&self.ai_cfg_key),
            FocusTarget::AiConfigModal(2) => Some(&self.ai_cfg_model),
            _ => None,
        }
    }

    pub fn active_text_input_mut(&mut self) -> Option<&mut TextInputField> {
        match self.focus {
            FocusTarget::CtrlKPrompt => Some(&mut self.ctrl_k_input),
            FocusTarget::AgentChatInput => Some(&mut self.agent_chat_input),
            FocusTarget::GitCommitInput => Some(&mut self.git_commit_input),
            FocusTarget::GitLogSearchInput => Some(&mut self.git_log_search_input),
            FocusTarget::SearchEverywhereModal | FocusTarget::FindInFilesModal => {
                Some(&mut self.search_query_input)
            }
            FocusTarget::NewFileModal
            | FocusTarget::NewFolderModal
            | FocusTarget::NewGitBranchModal
            | FocusTarget::RenamePathModal => Some(&mut self.modal_text_input),
            FocusTarget::AiConfigModal(0) => Some(&mut self.ai_cfg_url),
            FocusTarget::AiConfigModal(1) => Some(&mut self.ai_cfg_key),
            FocusTarget::AiConfigModal(2) => Some(&mut self.ai_cfg_model),
            _ => None,
        }
    }

    pub fn on_active_input_mutated(&mut self) {
        match self.focus {
            FocusTarget::SearchEverywhereModal => {
                self.modal_scroll = 0;
                self.trigger_search_everywhere(30);
            }
            FocusTarget::FindInFilesModal => {
                self.modal_scroll = 0;
                self.trigger_find_in_files(30);
            }
            FocusTarget::GitCommitInput => {
                self.git_engine.commit_message_draft = self.git_commit_input.as_str().to_string();
            }
            FocusTarget::GitLogSearchInput => {
                self.git_log_scroll = 0;
            }
            _ => {}
        }
    }

    /// 统一处理输入法提交文本 (`ReceivedImeText`) 与普通字符/空格输入，确保中文拼音/搜狗/微软输入法在所有焦点控件中稳定工作
    pub fn insert_text_at_focus(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(field) = self.active_text_input_mut() {
            field.insert_str(text);
            self.on_active_input_mutated();
            self.touch_caret_activity();
            return;
        }
        if self.focus == FocusTarget::TerminalInput {
            self.terminal.active_pty_mut().send_text(text);
            self.touch_caret_activity();
            return;
        }
        if self.focus == FocusTarget::Editor {
            self.active_buffer_mut().insert_str_at_cursor(text);
            self.ensure_editor_cursor_visible();
            self.touch_caret_activity();
        }
    }

    pub fn backspace_at_focus(&mut self) {
        if let Some(field) = self.active_text_input_mut() {
            field.backspace();
            self.on_active_input_mutated();
            self.touch_caret_activity();
            return;
        }
        if self.focus == FocusTarget::TerminalInput {
            self.terminal.active_pty_mut().send_raw_bytes(b"\x7f");
            self.touch_caret_activity();
            return;
        }
        if self.focus == FocusTarget::Editor {
            self.active_buffer_mut().backspace_at_cursor();
            self.ensure_editor_cursor_visible();
            self.touch_caret_activity();
        }
    }

    pub fn handle_focused_key_nav(&mut self, nav: TextInputNavAction) -> bool {
        let Some(field) = self.active_text_input_mut() else {
            return false;
        };
        let mut mutated = false;
        match nav {
            TextInputNavAction::Left { extend } => field.move_left(extend),
            TextInputNavAction::Right { extend } => field.move_right(extend),
            TextInputNavAction::WordLeft { extend } => field.move_word_left(extend),
            TextInputNavAction::WordRight { extend } => field.move_word_right(extend),
            TextInputNavAction::Home { extend } => field.move_home(extend),
            TextInputNavAction::End { extend } => field.move_end(extend),
            TextInputNavAction::DeleteForward => {
                field.delete_forward();
                mutated = true;
            }
            TextInputNavAction::DeleteWordBackward => {
                field.delete_word_backward();
                mutated = true;
            }
            TextInputNavAction::DeleteWordForward => {
                field.delete_word_forward();
                mutated = true;
            }
        }
        if mutated {
            self.on_active_input_mutated();
        }
        self.touch_caret_activity();
        true
    }

    pub fn select_all_at_focus(&mut self) -> bool {
        if let Some(field) = self.active_text_input_mut() {
            field.select_all();
            self.touch_caret_activity();
            true
        } else if self.focus == FocusTarget::Editor {
            self.active_buffer_mut().select_all();
            self.touch_caret_activity();
            true
        } else if self.focus == FocusTarget::TerminalInput {
            self.terminal.active_pty_mut().select_all();
            self.touch_caret_activity();
            true
        } else if self.focus == FocusTarget::AgentChatStream {
            self.select_all_agent_chat()
        } else {
            false
        }
    }

    pub fn copy_focused_selection(&self) -> Option<String> {
        if let Some(field) = self.active_text_input() {
            field.selected_text()
        } else if self.focus == FocusTarget::Editor {
            self.active_buffer().selected_text()
        } else if self.focus == FocusTarget::TerminalInput {
            self.terminal.active_pty().selected_text()
        } else if self.focus == FocusTarget::AgentChatStream {
            self.copy_agent_chat_selection()
        } else {
            None
        }
    }

    pub fn clear_active_input_selection(&mut self) -> bool {
        if let Some(field) = self.active_text_input_mut() {
            if field.selected_range().is_some() {
                field.clear_selection();
                self.touch_caret_activity();
                return true;
            }
        } else if self.focus == FocusTarget::Editor {
            if self.active_buffer().has_selection() {
                self.active_buffer_mut().clear_selection();
                self.touch_caret_activity();
                return true;
            }
        } else if self.focus == FocusTarget::TerminalInput {
            if self.terminal.active_pty().selection_range().is_some() {
                self.terminal.active_pty_mut().clear_selection();
                self.touch_caret_activity();
                return true;
            }
        } else if self.focus == FocusTarget::AgentChatStream {
            if self.clear_agent_chat_selection() {
                return true;
            }
        }
        false
    }

    pub fn cut_focused_selection(&mut self) -> Option<String> {
        if let Some(field) = self.active_text_input_mut() {
            let cut = field.cut_selected()?;
            self.on_active_input_mutated();
            self.touch_caret_activity();
            return Some(cut);
        }
        if self.focus == FocusTarget::Editor {
            let txt = self.active_buffer().selected_text()?;
            self.active_buffer_mut().delete_selection();
            self.ensure_editor_cursor_visible();
            self.touch_caret_activity();
            return Some(txt);
        }
        None
    }

    pub fn move_agent_input_vertical(
        &mut self,
        delta: isize,
        extend: bool,
        canvas: &SurfaceCanvas,
    ) -> bool {
        if self.focus != FocusTarget::AgentChatInput {
            return false;
        }
        let max_w = self.agent_input_text_rect.w.max(120);
        let moved = self.agent_chat_input.move_vertical_in_wrapped(
            delta,
            extend,
            canvas,
            max_w,
            AGENT_INPUT_FONT,
        );
        if !moved && !extend {
            if delta < 0 {
                self.agent_chat_input.history_prev();
            } else if delta > 0 {
                self.agent_chat_input.history_next();
            }
        }
        self.touch_caret_activity();
        true
    }

    pub fn hit_test_editor(
        &self,
        x: i32,
        y: i32,
        canvas: &SurfaceCanvas,
    ) -> Option<(usize, usize)> {
        if self.editor_code_rect.w <= 0 || self.editor_code_rect.h <= 0 {
            return None;
        }
        let buf = self.active_buffer();
        let total = buf.line_count().max(1);
        let line_h = 22i32;
        let rel_y = (y - self.editor_code_rect.y).clamp(0, (self.editor_code_rect.h - 1).max(0));
        let line_idx = (self.scroll_line + (rel_y / line_h) as usize).min(total - 1);
        let line_str = buf.line_text(line_idx);
        let rel_x = (x - self.editor_code_start_x + self.editor_scroll_x).max(0);
        let col = canvas.measure_caret_col_in_line(&line_str, rel_x, FontStyle::CodeMono);
        Some((line_idx, col))
    }

    pub fn hit_test_terminal(
        &self,
        x: i32,
        y: i32,
        canvas: &SurfaceCanvas,
    ) -> Option<(usize, usize)> {
        if self.terminal_content_rect.w <= 0 || self.terminal_content_rect.h <= 0 {
            return None;
        }
        let snap = self.terminal.active_pty().snapshot();
        if snap.lines.is_empty() {
            return Some((0, 0));
        }
        let line_h = self.terminal_line_h.max(16);
        let top_y = self.terminal_content_rect.y + 8;
        let rel_y = (y - top_y).clamp(0, (self.terminal_content_rect.h - 12).max(0));
        let line_idx =
            (self.terminal_scroll_anchor_line + (rel_y / line_h) as usize).min(snap.lines.len() - 1);
        let line_str = snap.lines[line_idx].raw_text();
        let rel_x = (x - (self.terminal_content_rect.x + 12)).max(0);
        let col = canvas.measure_caret_col_in_line(&line_str, rel_x, FontStyle::CodeMono);
        Some((line_idx, col))
    }

    pub fn end_mouse_text_drag(&mut self) {
        self.mouse_drag_target = None;
    }

    pub fn handle_input_mouse_drag(
        &mut self,
        x: i32,
        y: i32,
        canvas: &SurfaceCanvas,
    ) -> bool {
        let Some(target) = self.mouse_drag_target else {
            return false;
        };
        match target {
            FocusTarget::Editor => {
                if let Some((line, col)) = self.hit_test_editor(x, y, canvas) {
                    self.active_buffer_mut().set_cursor(line, col, true);
                    self.ensure_editor_cursor_visible();
                    self.touch_caret_activity();
                    return true;
                }
            }
            FocusTarget::TerminalInput => {
                if let Some((line, col)) = self.hit_test_terminal(x, y, canvas) {
                    self.terminal.active_pty_mut().sel_cursor = Some((line, col));
                    self.touch_caret_activity();
                    return true;
                }
            }
            FocusTarget::AgentChatInput => {
                let r = self.agent_input_text_rect;
                if r.w > 0 {
                    let vis_lines = (r.h / AGENT_INPUT_LINE_H).max(1) as usize;
                    let idx = self.agent_chat_input.hit_test_multiline(
                        canvas,
                        x,
                        y,
                        r.x,
                        r.y,
                        r.w,
                        AGENT_INPUT_LINE_H,
                        vis_lines,
                        AGENT_INPUT_FONT,
                    );
                    self.agent_chat_input.set_cursor_at(idx, true);
                    self.touch_caret_activity();
                    return true;
                }
            }
            FocusTarget::AgentChatStream => {
                if let Some(pos) = self.hit_test_agent_chat_stream(x, y, canvas) {
                    if self.agent_chat_sel_anchor.is_none() {
                        self.agent_chat_sel_anchor = Some(pos);
                    }
                    self.agent_chat_sel_cursor = Some(pos);
                    return true;
                }
            }
            FocusTarget::CtrlKPrompt => {
                let start_x = self.ctrl_k_input_box_rect.x + 28;
                let idx = self
                    .ctrl_k_input
                    .hit_test_singleline(canvas, x, start_x, FontStyle::UiSmall);
                self.ctrl_k_input.set_cursor_at(idx, true);
                self.touch_caret_activity();
                return true;
            }
            FocusTarget::GitCommitInput => {
                let start_x = self.git_commit_input_box_rect.x + 8;
                let idx = self
                    .git_commit_input
                    .hit_test_singleline(canvas, x, start_x, FontStyle::UiSmall);
                self.git_commit_input.set_cursor_at(idx, true);
                self.touch_caret_activity();
                return true;
            }
            FocusTarget::GitLogSearchInput => {
                let start_x = self.git_log_search_rect.x + 22;
                let idx = self
                    .git_log_search_input
                    .hit_test_singleline(canvas, x, start_x, FontStyle::UiSmall);
                self.git_log_search_input.set_cursor_at(idx, true);
                self.touch_caret_activity();
                return true;
            }
            FocusTarget::SearchEverywhereModal | FocusTarget::FindInFilesModal => {
                let idx = self.search_query_input.hit_test_singleline(
                    canvas,
                    x,
                    self.search_modal_text_start_x,
                    FontStyle::CodeMono,
                );
                self.search_query_input.set_cursor_at(idx, true);
                self.touch_caret_activity();
                return true;
            }
            FocusTarget::NewFileModal
            | FocusTarget::NewFolderModal
            | FocusTarget::NewGitBranchModal
            | FocusTarget::RenamePathModal => {
                let start_x = self.prompt_modal_input_rect.x + 8;
                let idx = self.modal_text_input.hit_test_singleline(
                    canvas,
                    x,
                    start_x,
                    FontStyle::CodeMono,
                );
                self.modal_text_input.set_cursor_at(idx, true);
                self.touch_caret_activity();
                return true;
            }
            FocusTarget::AiConfigModal(fi) if fi < 3 => {
                let rect = self.ai_cfg_field_rects[fi];
                let start_x = rect.x + 10;
                let masked = fi == 1 && !self.ai_cfg_show_key;
                let field = match fi {
                    0 => &mut self.ai_cfg_url,
                    1 => &mut self.ai_cfg_key,
                    _ => &mut self.ai_cfg_model,
                };
                let idx = field.hit_test_singleline_masked(
                    canvas,
                    x,
                    start_x,
                    FontStyle::CodeMono,
                    masked,
                );
                field.set_cursor_at(idx, true);
                self.touch_caret_activity();
                return true;
            }
            _ => {}
        }
        false
    }

    pub fn handle_input_double_click(
        &mut self,
        x: i32,
        y: i32,
        canvas: &SurfaceCanvas,
    ) -> bool {
        match self.focus {
            FocusTarget::Editor if self.editor_code_rect.contains(x, y) => {
                if let Some((line, col)) = self.hit_test_editor(x, y, canvas) {
                    self.active_buffer_mut().select_word_at(line, col);
                    self.touch_caret_activity();
                    return true;
                }
                false
            }
            FocusTarget::TerminalInput if self.terminal_content_rect.contains(x, y) => {
                if let Some((line, col)) = self.hit_test_terminal(x, y, canvas) {
                    self.terminal.active_pty_mut().select_word_at(line, col);
                    self.touch_caret_activity();
                    return true;
                }
                false
            }
            FocusTarget::AgentChatInput if self.agent_input_text_rect.contains(x, y) => {
                let r = self.agent_input_text_rect;
                let vis_lines = (r.h / AGENT_INPUT_LINE_H).max(1) as usize;
                let idx = self.agent_chat_input.hit_test_multiline(
                    canvas,
                    x,
                    y,
                    r.x,
                    r.y,
                    r.w,
                    AGENT_INPUT_LINE_H,
                    vis_lines,
                    AGENT_INPUT_FONT,
                );
                self.agent_chat_input.select_word_at(idx);
                self.touch_caret_activity();
                true
            }
            FocusTarget::AgentChatStream if self.agent_chat_area_rect.contains(x, y) => {
                self.select_word_in_agent_chat(x, y, canvas)
            }
            FocusTarget::CtrlKPrompt if self.ctrl_k_input_box_rect.contains(x, y) => {
                let idx = self.ctrl_k_input.hit_test_singleline(
                    canvas,
                    x,
                    self.ctrl_k_input_box_rect.x + 28,
                    FontStyle::UiSmall,
                );
                self.ctrl_k_input.select_word_at(idx);
                self.touch_caret_activity();
                true
            }
            FocusTarget::GitCommitInput if self.git_commit_input_box_rect.contains(x, y) => {
                let idx = self.git_commit_input.hit_test_singleline(
                    canvas,
                    x,
                    self.git_commit_input_box_rect.x + 8,
                    FontStyle::UiSmall,
                );
                self.git_commit_input.select_word_at(idx);
                self.touch_caret_activity();
                true
            }
            FocusTarget::GitLogSearchInput if self.git_log_search_rect.contains(x, y) => {
                let idx = self.git_log_search_input.hit_test_singleline(
                    canvas,
                    x,
                    self.git_log_search_rect.x + 22,
                    FontStyle::UiSmall,
                );
                self.git_log_search_input.select_word_at(idx);
                self.touch_caret_activity();
                true
            }
            FocusTarget::AiConfigModal(fi)
                if fi < 3 && self.ai_cfg_field_rects[fi].contains(x, y) =>
            {
                let start_x = self.ai_cfg_field_rects[fi].x + 10;
                let masked = fi == 1 && !self.ai_cfg_show_key;
                let field = match fi {
                    0 => &mut self.ai_cfg_url,
                    1 => &mut self.ai_cfg_key,
                    _ => &mut self.ai_cfg_model,
                };
                let idx = field.hit_test_singleline_masked(
                    canvas,
                    x,
                    start_x,
                    FontStyle::CodeMono,
                    masked,
                );
                field.select_word_at(idx);
                self.touch_caret_activity();
                true
            }
            _ => false,
        }
    }

    pub fn handle_input_triple_click(
        &mut self,
        x: i32,
        y: i32,
        canvas: &SurfaceCanvas,
    ) -> bool {
        match self.focus {
            FocusTarget::Editor if self.editor_code_rect.contains(x, y) => {
                if let Some((line, _)) = self.hit_test_editor(x, y, canvas) {
                    self.active_buffer_mut().select_line(line);
                    self.touch_caret_activity();
                    return true;
                }
                false
            }
            FocusTarget::TerminalInput if self.terminal_content_rect.contains(x, y) => {
                if let Some((line, _)) = self.hit_test_terminal(x, y, canvas) {
                    self.terminal.active_pty_mut().select_line(line);
                    self.touch_caret_activity();
                    return true;
                }
                false
            }
            FocusTarget::AgentChatInput if self.agent_input_text_rect.contains(x, y) => {
                let r = self.agent_input_text_rect;
                let vis_lines = (r.h / AGENT_INPUT_LINE_H).max(1) as usize;
                let idx = self.agent_chat_input.hit_test_multiline(
                    canvas,
                    x,
                    y,
                    r.x,
                    r.y,
                    r.w,
                    AGENT_INPUT_LINE_H,
                    vis_lines,
                    AGENT_INPUT_FONT,
                );
                self.agent_chat_input.select_line_at(idx);
                self.touch_caret_activity();
                true
            }
            FocusTarget::AgentChatStream if self.agent_chat_area_rect.contains(x, y) => {
                self.select_line_in_agent_chat(x, y, canvas)
            }
            FocusTarget::CtrlKPrompt
            | FocusTarget::GitCommitInput
            | FocusTarget::GitLogSearchInput
            | FocusTarget::SearchEverywhereModal
            | FocusTarget::FindInFilesModal
            | FocusTarget::NewFileModal
            | FocusTarget::NewFolderModal
            | FocusTarget::NewGitBranchModal
            | FocusTarget::RenamePathModal
            | FocusTarget::AiConfigModal(_) => self.select_all_at_focus(),
            _ => false,
        }
    }

    /// 通用鼠标滚轮滚动：自动识别当前指针所在面板（项目树 / Git更改 / 代码编辑器 / AI 控制台 / 原生终端 / Git 历史库 / 弹窗）
    #[allow(dead_code)]
    pub fn handle_mouse_wheel(&mut self, x: i32, y: i32, lines_delta: i32) {
        self.handle_mouse_wheel_2d(x, y, lines_delta, 0);
    }

    /// 二维平滑滚轮滚动：支持垂直加速滚动与代码编辑器水平滚动 (`Shift+滚轮` 或触控板横向滑动)
    pub fn handle_mouse_wheel_2d(
        &mut self,
        x: i32,
        y: i32,
        lines_delta: i32,
        horiz_px_delta: i32,
    ) {
        if lines_delta == 0 && horiz_px_delta == 0 {
            return;
        }
        match self.focus {
            FocusTarget::SettingsModal
            | FocusTarget::SearchEverywhereModal
            | FocusTarget::FindInFilesModal => {
                if lines_delta < 0 {
                    self.modal_scroll = self.modal_scroll.saturating_sub((-lines_delta) as usize);
                } else if lines_delta > 0 {
                    self.modal_scroll = (self.modal_scroll + lines_delta as usize).min(40);
                }
                return;
            }
            _ => {}
        }

        if self.left_tool.is_some() && self.left_pane_rect.contains(x, y) {
            if self.left_tool == Some(ToolWindowId::Commit) {
                let max_s = (self.git_engine.changed_files.len() + 4).saturating_sub(4);
                if lines_delta < 0 {
                    self.git_changes_scroll =
                        self.git_changes_scroll.saturating_sub((-lines_delta) as usize);
                } else if lines_delta > 0 {
                    self.git_changes_scroll =
                        (self.git_changes_scroll + lines_delta as usize).min(max_s);
                }
            } else {
                let max_t = self.project_tree.visible_entries.len().saturating_sub(6);
                if lines_delta < 0 {
                    self.tree_scroll = self.tree_scroll.saturating_sub((-lines_delta) as usize);
                } else if lines_delta > 0 {
                    self.tree_scroll = (self.tree_scroll + lines_delta as usize).min(max_t);
                }
            }
        } else if self.right_agent_open && self.right_pane_rect.contains(x, y) {
            let max_r = self
                .right_scroll_info
                .total
                .saturating_sub(self.right_scroll_info.visible.max(1));
            if self.agent_scroll == usize::MAX {
                self.agent_scroll = max_r;
            }
            if lines_delta < 0 {
                self.agent_scroll = self.agent_scroll.saturating_sub((-lines_delta) as usize);
            } else if lines_delta > 0 {
                self.agent_scroll = (self.agent_scroll + lines_delta as usize).min(max_r);
            }
        } else if self.bottom_tool.is_some() && self.bottom_pane_rect.contains(x, y) {
            if self.bottom_tool == Some(ToolWindowId::GitLog) {
                if self.git_log_diff_rect.contains(x, y) {
                    let max_d = self
                        .git_log_diff_scroll_info
                        .total
                        .saturating_sub(self.git_log_diff_scroll_info.visible.max(1));
                    if lines_delta < 0 {
                        self.git_diff_scroll =
                            self.git_diff_scroll.saturating_sub((-lines_delta) as usize);
                    } else if lines_delta > 0 {
                        self.git_diff_scroll =
                            (self.git_diff_scroll + lines_delta as usize).min(max_d);
                    }
                } else if self.git_log_files_rect.contains(x, y) {
                    if lines_delta < 0 {
                        self.git_files_scroll =
                            self.git_files_scroll.saturating_sub((-lines_delta) as usize);
                    } else if lines_delta > 0 {
                        self.git_files_scroll =
                            (self.git_files_scroll + lines_delta as usize).min(120);
                    }
                } else if self.git_log_branches_rect.contains(x, y) {
                    if lines_delta < 0 {
                        self.git_branches_scroll = self
                            .git_branches_scroll
                            .saturating_sub((-lines_delta) as usize);
                    } else if lines_delta > 0 {
                        self.git_branches_scroll =
                            (self.git_branches_scroll + lines_delta as usize).min(60);
                    }
                } else {
                    let max_c = self
                        .git_log_commits_scroll_info
                        .total
                        .saturating_sub(self.git_log_commits_scroll_info.visible.max(1));
                    if lines_delta < 0 {
                        self.git_log_scroll =
                            self.git_log_scroll.saturating_sub((-lines_delta) as usize);
                    } else if lines_delta > 0 {
                        self.git_log_scroll =
                            (self.git_log_scroll + lines_delta as usize).min(max_c);
                    }
                }
            } else {
                let pty = self.terminal.active_pty_mut();
                let total = pty.snapshot().lines.len();
                let max_up = total.saturating_sub(4);
                // scroll_offset 表示从底部向上回看多少行
                if lines_delta < 0 {
                    pty.scroll_offset = (pty.scroll_offset + (-lines_delta) as usize).min(max_up);
                } else if lines_delta > 0 {
                    pty.scroll_offset = pty.scroll_offset.saturating_sub(lines_delta as usize);
                }
            }
        } else {
            let max_line = self.active_buffer().line_count().saturating_sub(5);
            if lines_delta < 0 {
                self.scroll_line = self.scroll_line.saturating_sub((-lines_delta) as usize);
            } else if lines_delta > 0 {
                self.scroll_line = (self.scroll_line + lines_delta as usize).min(max_line);
            }
            if horiz_px_delta != 0 {
                self.editor_scroll_x = (self.editor_scroll_x + horiz_px_delta).clamp(0, 2400);
            }
        }
    }

    /// 检测鼠标按下位置是否命中任意面板的垂直滚动条轨道
    pub fn try_begin_scrollbar_drag(&mut self, x: i32, y: i32) -> bool {
        let hit_padding = 4;
        let check = |info: ScrollTrackInfo| -> bool {
            info.total > info.visible
                && info.track.h > 0
                && x >= info.track.x - hit_padding
                && x < info.track.x + info.track.w + hit_padding
                && y >= info.track.y
                && y < info.track.y + info.track.h
        };
        if check(self.left_scroll_info) {
            self.active_scrollbar_drag = Some(DragScrollbar::LeftTree);
            self.update_scrollbar_drag(y);
            return true;
        }
        if check(self.editor_scroll_info) {
            self.active_scrollbar_drag = Some(DragScrollbar::CenterEditor);
            self.update_scrollbar_drag(y);
            return true;
        }
        if check(self.right_scroll_info) {
            self.active_scrollbar_drag = Some(DragScrollbar::RightPanel);
            self.update_scrollbar_drag(y);
            return true;
        }
        if self.bottom_tool == Some(ToolWindowId::GitLog) {
            if check(self.git_log_commits_scroll_info) {
                self.active_scrollbar_drag = Some(DragScrollbar::GitLogCommits);
                self.update_scrollbar_drag(y);
                return true;
            }
            if check(self.git_log_diff_scroll_info) {
                self.active_scrollbar_drag = Some(DragScrollbar::GitLogDiff);
                self.update_scrollbar_drag(y);
                return true;
            }
        } else if check(self.terminal_scroll_info) {
            self.active_scrollbar_drag = Some(DragScrollbar::BottomTerminal);
            self.update_scrollbar_drag(y);
            return true;
        }
        false
    }

    pub fn update_scrollbar_drag(&mut self, y: i32) -> bool {
        let Some(sb) = self.active_scrollbar_drag else {
            return false;
        };
        let ratio_for = |info: ScrollTrackInfo| -> usize {
            let max_s = info.total.saturating_sub(info.visible).max(1);
            let rel = ((y - info.track.y) as f32 / (info.track.h.max(1) as f32)).clamp(0.0, 1.0);
            (rel * max_s as f32).round() as usize
        };
        match sb {
            DragScrollbar::LeftTree => {
                if self.left_tool == Some(ToolWindowId::Commit) {
                    self.git_changes_scroll = ratio_for(self.left_scroll_info);
                } else {
                    self.tree_scroll = ratio_for(self.left_scroll_info);
                }
            }
            DragScrollbar::CenterEditor => {
                self.scroll_line = ratio_for(self.editor_scroll_info);
            }
            DragScrollbar::RightPanel => {
                self.agent_scroll = ratio_for(self.right_scroll_info);
            }
            DragScrollbar::BottomTerminal => {
                let info = self.terminal_scroll_info;
                let max_s = info.total.saturating_sub(info.visible);
                let top_offset = ratio_for(info).min(max_s);
                self.terminal.active_pty_mut().scroll_offset = max_s.saturating_sub(top_offset);
            }
            DragScrollbar::GitLogCommits => {
                self.git_log_scroll = ratio_for(self.git_log_commits_scroll_info);
            }
            DragScrollbar::GitLogDiff => {
                self.git_diff_scroll = ratio_for(self.git_log_diff_scroll_info);
            }
        }
        true
    }

    pub fn end_scrollbar_drag(&mut self) {
        self.active_scrollbar_drag = None;
    }

    /// 检测鼠标按下位置是否命中左右或底部面板的可拖拽分割线
    pub fn try_begin_splitter_drag(&mut self, x: i32, y: i32, win_w: i32, win_h: i32) -> bool {
        if let Some(splitter) = self.splitter_at(x, y, win_w, win_h) {
            self.active_splitter_drag = Some(splitter);
            return true;
        }
        false
    }

    pub fn update_splitter_drag(&mut self, x: i32, y: i32, win_w: i32, win_h: i32) -> bool {
        let Some(drag) = self.active_splitter_drag else {
            return false;
        };
        let stripe_w = 42;
        let status_h = 26;
        match drag {
            DragSplitter::LeftPanel => {
                self.left_panel_width = (x - stripe_w).clamp(170, 540);
            }
            DragSplitter::RightPanel => {
                self.right_panel_width = (win_w - stripe_w - x).clamp(280, 680);
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

    pub fn handle_mouse_click_with_mods(
        &mut self,
        x: i32,
        y: i32,
        shift: bool,
        canvas: &SurfaceCanvas,
    ) {
        self.mouse_drag_target = None;
        let hit_match = self
            .hit_regions
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (rect, _))| rect.contains(x, y));

        if let Some((idx, _)) = hit_match {
            if idx < self.overlay_hit_start
                && self.active_overlay_rects.iter().any(|r| r.contains(x, y))
            {
                return;
            }
        }
        let action = hit_match.map(|(_, (_, act))| act.clone());

        let Some(act) = action else {
            self.active_menu_category = None;
            self.git_branch_popup_open = false;
            self.agent_provider_popup_open = false;
            self.agent_model_popup_open = false;
            self.agent_mode_popup_open = false;
            self.agent_sessions_popup_open = false;
            self.run_config_popup_open = false;
            self.project_popup_open = false;
            self.more_menu_popup_open = false;
            self.context_menu = None;
            if self.focus == FocusTarget::ImagePreviewModal {
                self.close_image_preview();
            }
            return;
        };

        if !matches!(&act, HitAction::TriggerContextMenuAction(_)) {
            self.context_menu = None;
        }

        // 点击非弹窗区时自动收起对应下拉框
        match &act {
            HitAction::OpenMenuCategory(_) | HitAction::ToggleHamburgerMenu => {}
            _ => self.active_menu_category = None,
        }
        match &act {
            HitAction::ToggleGitBranchPopup => {}
            _ => self.git_branch_popup_open = false,
        }
        match &act {
            HitAction::ToggleAgentProviderPopup | HitAction::SelectAgentProvider(_) => {}
            _ => self.agent_provider_popup_open = false,
        }
        match &act {
            HitAction::ToggleAgentModelPopup | HitAction::SelectAgentModel(_) => {}
            _ => self.agent_model_popup_open = false,
        }
        match &act {
            HitAction::ToggleAgentModePopup | HitAction::SelectAgentMode(_) => {}
            _ => self.agent_mode_popup_open = false,
        }
        match &act {
            HitAction::ToggleAgentSessionsPopup | HitAction::CloseAgentSession(_) => {}
            _ => self.agent_sessions_popup_open = false,
        }
        match &act {
            HitAction::ToggleRunConfigPopup => {}
            _ => self.run_config_popup_open = false,
        }
        match &act {
            HitAction::ToggleProjectPopup => {}
            _ => self.project_popup_open = false,
        }
        match &act {
            HitAction::ToggleMoreMenuPopup => {}
            _ => self.more_menu_popup_open = false,
        }

        match act {
            HitAction::WindowControl(cmd) => {
                self.pending_window_cmd = Some(cmd);
            }
            HitAction::ToggleHamburgerMenu => {
                self.hamburger_menu_expanded = !self.hamburger_menu_expanded;
                if !self.hamburger_menu_expanded {
                    self.active_menu_category = None;
                }
            }
            HitAction::OpenMenuCategory(cat) => {
                if self.active_menu_category == Some(cat) {
                    self.active_menu_category = None;
                } else {
                    self.active_menu_category = Some(cat);
                }
            }
            HitAction::ExecuteIdeAction(ide_act) => {
                self.execute_ide_action(ide_act);
            }
            HitAction::TriggerContextMenuAction(cm_act) => {
                self.context_menu = None;
                self.execute_context_menu_action(cm_act);
            }
            HitAction::ToggleProjectPopup => {
                self.project_popup_open = !self.project_popup_open;
            }
            HitAction::ToggleRunConfigPopup => {
                self.run_config_popup_open = !self.run_config_popup_open;
            }
            HitAction::SelectRunConfig(idx) => {
                if idx < self.run_configs.len() {
                    self.active_run_config = idx;
                    let name = self.run_configs[idx].name.clone();
                    self.status_notification = format!("已切换运行配置: {name}");
                }
                self.run_config_popup_open = false;
            }
            HitAction::ToggleMoreMenuPopup => {
                self.more_menu_popup_open = !self.more_menu_popup_open;
            }
            HitAction::ToggleGitBranchPopup => {
                self.git_branch_popup_open = !self.git_branch_popup_open;
                if self.git_branch_popup_open {
                    self.git_engine.request_refresh();
                }
            }
            HitAction::CheckoutGitBranch(branch) => {
                self.git_branch_popup_open = false;
                self.git_engine.request_checkout_branch(&branch);
                self.status_notification = format!("正在切换分支 '{branch}'...");
            }
            HitAction::CreateNewGitBranchPrompt => {
                self.git_branch_popup_open = false;
                self.git_new_branch_from_ref = None;
                self.modal_text_input.set_text("feature/new-agent");
                self.modal_text_input.select_all();
                self.focus = FocusTarget::NewGitBranchModal;
                self.touch_caret_activity();
            }
            HitAction::SelectTab(idx) => {
                self.active_tab = idx;
                self.scroll_line = 0;
                self.editor_scroll_x = 0;
                self.focus = FocusTarget::Editor;
                self.touch_caret_activity();
            }
            HitAction::CloseTab(idx) => self.close_tab(idx),
            HitAction::OpenFile(p) => {
                self.open_file(p);
                self.focus = FocusTarget::Editor;
                self.touch_caret_activity();
            }
            HitAction::JumpToLine(line) => {
                self.active_buffer_mut().set_cursor(line, 0, false);
                self.ensure_editor_cursor_visible();
                self.focus = FocusTarget::Editor;
                self.touch_caret_activity();
            }
            HitAction::ClickEditorLine | HitAction::FocusEditorBody => {
                if let Some((line, col)) = self.hit_test_editor(x, y, canvas) {
                    self.active_buffer_mut().set_cursor(line, col, shift);
                    self.ensure_editor_cursor_visible();
                    self.mouse_drag_target = Some(FocusTarget::Editor);
                }
                self.focus = FocusTarget::Editor;
                self.touch_caret_activity();
            }
            HitAction::ToggleDirectory(dir) => {
                self.project_tree.toggle_dir(&dir);
                self.status_notification = format!("已切换目录: {}", dir.display());
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
                        self.git_engine.request_refresh();
                    }
                    self.left_tool = Some(tool);
                }
            }
            HitAction::ToggleRightAgentHub => {
                self.right_agent_open = !self.right_agent_open;
            }
            HitAction::SwitchRightPanelTab(tab) => {
                self.right_panel_tab = tab;
                self.right_agent_open = true;
                if tab == RightPanelTab::Changes {
                    self.git_engine.request_refresh();
                }
            }
            HitAction::SelectBottomTool(tool) => {
                if self.bottom_tool == Some(tool) {
                    self.bottom_tool = None;
                } else {
                    self.bottom_tool = Some(tool);
                    if tool == ToolWindowId::Terminal {
                        self.focus = FocusTarget::TerminalInput;
                        self.touch_caret_activity();
                    } else if tool == ToolWindowId::GitLog {
                        self.git_engine.request_refresh();
                        if self.selected_git_commit_hash.is_none() {
                            if let Some(first) = self.git_engine.recent_commits.first() {
                                let h = first.hash.clone();
                                self.git_engine.request_load_commit_detail(&h);
                                self.selected_git_commit_hash = Some(h);
                            }
                        }
                    }
                }
            }
            HitAction::ToggleStageFile(path) => {
                self.git_engine.request_toggle_stage_file(&path);
            }
            HitAction::StageAllGitChanges => {
                self.git_engine.request_stage_all();
                self.status_notification =
                    self.tr("正在后台暂存全部变更...", "Staging all changes...").to_string();
            }
            HitAction::UnstageAllGitChanges => {
                self.git_engine.request_unstage_all();
                self.status_notification = self
                    .tr("正在后台取消全部暂存...", "Unstaging all changes...")
                    .to_string();
            }
            HitAction::CommitAllGitChanges => {
                let msg = self.git_commit_input.as_str().to_string();
                self.git_engine
                    .request_commit_with_options(&msg, self.git_amend_commit, false);
                self.status_notification = self
                    .tr("正在后台提交 Git 变更...", "Committing Git changes...")
                    .to_string();
            }
            HitAction::CommitAndPushGitChanges => {
                let msg = self.git_commit_input.as_str().to_string();
                self.git_engine
                    .request_commit_with_options(&msg, self.git_amend_commit, true);
                self.status_notification = self
                    .tr(
                        "正在后台提交并推送 Git 变更...",
                        "Committing and pushing Git changes...",
                    )
                    .to_string();
            }
            HitAction::ToggleGitAmend => {
                self.git_amend_commit = !self.git_amend_commit;
                if self.git_amend_commit {
                    if let Some(last) = self.git_engine.recent_commits.first() {
                        let cur = self.git_commit_input.as_str().trim();
                        if cur.is_empty() || cur == "feat: update Rust AI IDE workspace" {
                            self.git_commit_input.set_text(&last.subject);
                            self.git_engine.commit_message_draft = last.subject.clone();
                        }
                    }
                    self.status_notification = self
                        .tr(
                            "已启用 Amend 修正上次提交模式",
                            "Enabled Amend last commit mode",
                        )
                        .to_string();
                } else {
                    self.status_notification = self
                        .tr("已关闭 Amend 修正模式", "Disabled Amend mode")
                        .to_string();
                }
            }
            HitAction::GenerateAiCommitMessage => {
                self.git_engine.request_ai_commit_diff();
                self.status_notification = "正在后台分析 Git Diff 并生成 Commit 信息...".to_string();
            }
            HitAction::RollbackGitFile(path) => {
                self.git_engine.request_rollback_file(&path);
                self.status_notification = format!("正在回滚 {}...", path.display());
            }
            HitAction::RollbackAllGitChanges => {
                self.git_engine.request_rollback_all();
                self.status_notification = self
                    .tr("正在回滚工作区全部未提交变更...", "Rolling back all changes...")
                    .to_string();
            }
            HitAction::RefreshGitStatus => {
                self.git_engine.request_refresh();
                self.status_notification = "正在后台刷新 Git 状态...".to_string();
            }
            HitAction::GitFetchAll => {
                self.git_branch_popup_open = false;
                self.git_engine.request_fetch();
                self.status_notification = self
                    .tr("正在后台执行 Git Fetch...", "Fetching all remotes...")
                    .to_string();
            }
            HitAction::FocusGitCommitMessageInput => {
                self.focus = FocusTarget::GitCommitInput;
                let start_x = self.git_commit_input_box_rect.x + 8;
                let idx = self.git_commit_input.hit_test_singleline(
                    canvas,
                    x,
                    start_x,
                    FontStyle::UiSmall,
                );
                self.git_commit_input.set_cursor_at(idx, shift);
                self.mouse_drag_target = Some(FocusTarget::GitCommitInput);
                self.touch_caret_activity();
            }
            HitAction::InspectGitWorkingFileDiff(path) => {
                self.git_working_diff_target = Some(path.clone());
                self.git_diff_scroll = 0;
                self.bottom_tool = Some(ToolWindowId::GitLog);
                self.open_file(path.clone());
                self.status_notification = format!(
                    "{}: {}",
                    self.tr("正在查看工作区文件 Diff", "Inspecting working tree diff"),
                    path.display()
                );
            }
            HitAction::SelectGitLogBranchFilter(branch_opt) => {
                self.git_log_branch_filter = branch_opt.clone();
                self.git_log_scroll = 0;
                self.status_notification = match branch_opt {
                    Some(b) => format!(
                        "{}: {b}",
                        self.tr("已按分支筛选提交历史", "Filtered Git log by branch")
                    ),
                    None => self
                        .tr("显示全部分支提交历史", "Showing all branches in Git log")
                        .to_string(),
                };
            }
            HitAction::SelectGitCommit(hash) => {
                self.git_working_diff_target = None;
                self.selected_git_commit_hash = Some(hash.clone());
                self.selected_git_commit_file = 0;
                self.git_files_scroll = 0;
                self.git_diff_scroll = 0;
                self.git_engine.request_load_commit_detail(&hash);
            }
            HitAction::SelectGitCommitFile(file_idx) => {
                self.git_working_diff_target = None;
                self.selected_git_commit_file = file_idx;
                self.git_diff_scroll = 0;
            }
            HitAction::FocusGitLogSearchInput => {
                self.focus = FocusTarget::GitLogSearchInput;
                let start_x = self.git_log_search_rect.x + 22;
                let idx = self.git_log_search_input.hit_test_singleline(
                    canvas,
                    x,
                    start_x,
                    FontStyle::UiSmall,
                );
                self.git_log_search_input.set_cursor_at(idx, shift);
                self.mouse_drag_target = Some(FocusTarget::GitLogSearchInput);
                self.touch_caret_activity();
            }
            HitAction::ClearGitLogSearch => {
                self.git_log_search_input.clear();
                self.git_log_scroll = 0;
            }
            HitAction::ClearGitWorkingDiffTarget => {
                self.git_working_diff_target = None;
                self.git_diff_scroll = 0;
            }
            HitAction::GitCheckoutSelectedCommit => {
                if let Some(hash) = self.selected_git_commit_hash.clone() {
                    self.git_engine.request_checkout_branch(&hash);
                    self.status_notification = format!(
                        "{}: {hash}",
                        self.tr("正在检出提交版本", "Checking out revision")
                    );
                }
            }
            HitAction::GitCreateBranchAtSelectedCommit => {
                let target = self
                    .selected_git_commit_hash
                    .clone()
                    .or_else(|| self.git_engine.recent_commits.first().map(|c| c.hash.clone()));
                self.git_new_branch_from_ref = target.clone();
                let default_name = match &target {
                    Some(h) => format!("branch-from-{h}"),
                    None => "feature/new-branch".to_string(),
                };
                self.modal_text_input.set_text(&default_name);
                self.modal_text_input.select_all();
                self.focus = FocusTarget::NewGitBranchModal;
                self.touch_caret_activity();
            }
            HitAction::GitCherryPickSelectedCommit => {
                if let Some(hash) = self.selected_git_commit_hash.clone() {
                    self.git_engine.request_cherry_pick(&hash);
                    self.status_notification = format!("Cherry-Pick {hash}...");
                }
            }
            HitAction::GitRevertSelectedCommit => {
                if let Some(hash) = self.selected_git_commit_hash.clone() {
                    self.git_engine.request_revert_commit(&hash);
                    self.status_notification = format!("Revert {hash}...");
                }
            }
            HitAction::GitCopySelectedCommitHash => {
                if let Some(hash) = self.selected_git_commit_hash.clone() {
                    let full = self
                        .git_engine
                        .recent_commits
                        .iter()
                        .find(|c| c.hash == hash)
                        .map(|c| c.full_hash.clone())
                        .unwrap_or(hash);
                    write_windows_clipboard_text(&full);
                    self.status_notification = format!(
                        "{}: {full}",
                        self.tr("已复制 Commit Hash", "Copied Commit Hash")
                    );
                }
            }
            HitAction::GitMergeBranch(branch) => {
                self.git_branch_popup_open = false;
                self.git_engine.request_merge_branch(&branch);
                self.status_notification = format!("Merging '{branch}'...");
            }
            HitAction::GitDeleteBranch(branch) => {
                self.git_branch_popup_open = false;
                self.git_engine.request_delete_branch(&branch);
                self.status_notification = format!("Deleting branch '{branch}'...");
            }
            HitAction::SelectAgentProvider(provider) => {
                self.agent_hub.active_provider = provider;
                self.agent_mode = self.agent_hub.normalize_mode_for_active(self.agent_mode);
                self.agent_provider_popup_open = false;
                self.agent_model_popup_open = false;
                self.agent_mode_popup_open = false;
                self.ai_cfg_model.set_text(&self.agent_hub.active_model());
                self.focus = FocusTarget::AgentChatInput;
                self.touch_caret_activity();
                self.status_notification = format!(
                    "已切换本地 CLI 智能体: {} · 当前模型: {} · 模式: {}",
                    provider.display_name(),
                    self.agent_hub.active_model(),
                    self.agent_mode.label()
                );
            }
            HitAction::ToggleAgentProviderPopup => {
                self.agent_provider_popup_open = !self.agent_provider_popup_open;
                if self.agent_provider_popup_open {
                    self.agent_model_popup_open = false;
                    self.agent_mode_popup_open = false;
                    self.agent_sessions_popup_open = false;
                }
            }
            HitAction::ToggleAgentModelPopup => {
                self.agent_model_popup_open = !self.agent_model_popup_open;
                if self.agent_model_popup_open {
                    self.agent_provider_popup_open = false;
                    self.agent_mode_popup_open = false;
                    self.agent_sessions_popup_open = false;
                }
            }
            HitAction::ToggleAgentModePopup => {
                self.agent_mode_popup_open = !self.agent_mode_popup_open;
                if self.agent_mode_popup_open {
                    self.agent_provider_popup_open = false;
                    self.agent_model_popup_open = false;
                    self.agent_sessions_popup_open = false;
                }
            }
            HitAction::SelectAgentMode(mode) => {
                let normalized = self.agent_hub.normalize_mode_for_active(mode);
                self.agent_mode = normalized;
                self.agent_mode_popup_open = false;
                self.focus = FocusTarget::AgentChatInput;
                self.touch_caret_activity();
                self.status_notification = format!(
                    "✓ {} {} ({})",
                    self.tr("已切换模式为", "Switched mode to"),
                    normalized.label(),
                    self.agent_hub.mode_cli_flag_for_active(normalized)
                );
            }
            HitAction::SelectAgentModel(model) => {
                self.agent_hub.select_model_for_active(model.clone());
                self.ai_cfg_model.set_text(&model);
                self.agent_model_popup_open = false;
                self.focus = FocusTarget::AgentChatInput;
                self.touch_caret_activity();
                self.status_notification = format!(
                    "✓ 已切换 `{}` 模型为: {model}",
                    self.agent_hub.active_provider.short_name()
                );
            }
            HitAction::ToggleAgentFastMode => {
                let _ = self.agent_hub.toggle_fast_mode_for_active();
                self.ai_cfg_model.set_text(&self.agent_hub.active_model());
                let fast = self
                    .agent_hub
                    .providers
                    .iter()
                    .find(|p| p.kind == self.agent_hub.active_provider)
                    .map(|p| p.fast_mode)
                    .unwrap_or(false);
                self.status_notification = format!(
                    "{}: {} ({})",
                    if fast {
                        self.tr("⚡ 已开启 Fast 快速模式", "⚡ Fast Mode ON")
                    } else {
                        self.tr("已关闭 Fast 快速模式", "Fast Mode OFF")
                    },
                    self.agent_hub.active_provider.short_name(),
                    self.agent_hub.active_model()
                );
            }
            HitAction::CycleAgentReasoningEffort => {
                let _ = self.agent_hub.cycle_reasoning_effort_for_active();
                self.ai_cfg_model.set_text(&self.agent_hub.active_model());
                let eff = self
                    .agent_hub
                    .providers
                    .iter()
                    .find(|p| p.kind == self.agent_hub.active_provider)
                    .map(|p| p.reasoning_effort)
                    .unwrap_or(ide_agent_hub::ReasoningEffort::Medium);
                self.status_notification = format!(
                    "🧠 {} {} {:?} ({})",
                    self.agent_hub.active_provider.short_name(),
                    self.tr("思考深度:", "Reasoning Effort:"),
                    eff,
                    self.agent_hub.active_model()
                );
            }
            HitAction::RefreshLocalCliAgents => {
                self.agent_hub.refresh_local_cli_agents();
                self.agent_mode = self.agent_hub.normalize_mode_for_active(self.agent_mode);
                self.ai_cfg_model.set_text(&self.agent_hub.active_model());
                let avail_cnt = self
                    .agent_hub
                    .providers
                    .iter()
                    .filter(|p| p.available)
                    .count();
                let models_cnt = self.agent_hub.available_models_for_active().len();
                self.status_notification = format!(
                    "↻ 已同步本机 {avail_cnt} 个 CLI 引擎，当前 `{}` 共自动发现 {models_cnt} 个模型",
                    self.agent_hub.active_provider.short_name()
                );
            }
            HitAction::ToggleAttachActiveContext => {
                self.attach_active_context = !self.attach_active_context;
                self.status_notification = if self.attach_active_context {
                    "已开启自动附加当前文件与光标上下文".to_string()
                } else {
                    "已关闭自动附加文件上下文".to_string()
                };
            }
            HitAction::AttachActiveFileToAgent => {
                let fname = self
                    .active_buffer()
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("current_file")
                    .to_string();
                let current = self.agent_chat_input.as_str().to_string();
                if !current.contains(&fname) {
                    let prefix = if !current.is_empty() && !current.ends_with(' ') && !current.ends_with('\n') {
                        " "
                    } else {
                        ""
                    };
                    self.agent_chat_input.move_end(false);
                    self.agent_chat_input.insert_str(&format!("{prefix}@{fname} "));
                }
                self.attach_active_context = true;
                self.focus = FocusTarget::AgentChatInput;
                self.touch_caret_activity();
            }
            HitAction::ClearAgentChat => {
                self.agent_hub.clear_messages();
                self.collapsed_code_blocks.clear();
                self.expanded_message_thoughts.clear();
                self.agent_scroll = 0;
                self.clear_agent_chat_selection();
                self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
                self.status_notification = "已清空当前 AI 会话流".to_string();
            }
            HitAction::NewAgentSession => {
                self.create_new_agent_session();
            }
            HitAction::SelectAgentSession(idx) => {
                self.switch_agent_session(idx);
            }
            HitAction::CloseAgentSession(idx) => {
                self.close_agent_session(idx);
            }
            HitAction::ToggleAgentSessionsPopup => {
                self.agent_sessions_popup_open = !self.agent_sessions_popup_open;
                if self.agent_sessions_popup_open {
                    self.agent_provider_popup_open = false;
                    self.agent_model_popup_open = false;
                    self.agent_mode_popup_open = false;
                }
            }
            HitAction::ToggleCodeBlockCollapse(msg_idx, blk_idx) => {
                let current_collapsed = self
                    .cached_chat_rows
                    .rows
                    .iter()
                    .find_map(|r| match r {
                        crate::agent_panel::CachedChatRow::CodeBlockHeader {
                            msg_idx: mi,
                            blk_idx: bi,
                            collapsed,
                            ..
                        } if *mi == msg_idx && *bi == blk_idx => Some(*collapsed),
                        _ => None,
                    })
                    .unwrap_or(false);
                self.collapsed_code_blocks
                    .insert((msg_idx, blk_idx), !current_collapsed);
                self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
            }
            HitAction::ToggleMessageThoughtCollapse(msg_idx) => {
                let cur = self
                    .expanded_message_thoughts
                    .get(&msg_idx)
                    .copied()
                    .unwrap_or(false);
                self.expanded_message_thoughts.insert(msg_idx, !cur);
                self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
            }
            HitAction::ToggleComposerPastedSnippet(id) => {
                if self.expanded_composer_snippet_id.as_deref() == Some(id.as_str()) {
                    self.expanded_composer_snippet_id = None;
                } else {
                    self.expanded_composer_snippet_id = Some(id);
                }
            }
            HitAction::RemoveComposerPastedSnippet(id) => {
                if self.expanded_composer_snippet_id.as_deref() == Some(id.as_str()) {
                    self.expanded_composer_snippet_id = None;
                }
                self.agent_hub.remove_pasted_snippet_from_active(&id);
                self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
            }
            HitAction::RemoveComposerAttachment(path) => {
                self.agent_hub.remove_attached_item_from_active(&path);
                self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
            }
            HitAction::OpenImagePreview(path) => {
                self.open_image_preview(path);
            }
            HitAction::CloseImagePreview => {
                self.close_image_preview();
            }
            HitAction::CopyAgentMessage(msg_idx) => {
                self.copy_agent_message_to_clipboard(msg_idx);
            }
            HitAction::CopyAgentCodeBlock(code) => {
                write_windows_clipboard_text(&code);
                self.status_notification = self
                    .tr("已复制代码块到剪贴板", "Copied code block to clipboard")
                    .to_string();
            }
            HitAction::OpenFileAtLine(path, line_opt) => {
                self.open_file_at_line(path, line_opt);
            }
            HitAction::FocusAgentChatStream => {
                self.focus = FocusTarget::AgentChatStream;
                if let Some(pos) = self.hit_test_agent_chat_stream(x, y, canvas) {
                    if !shift || self.agent_chat_sel_anchor.is_none() {
                        self.agent_chat_sel_anchor = Some(pos);
                    }
                    self.agent_chat_sel_cursor = Some(pos);
                    self.mouse_drag_target = Some(FocusTarget::AgentChatStream);
                } else if !shift {
                    self.clear_agent_chat_selection();
                }
            }
            HitAction::RunAgentQuickPrompt(prompt) => {
                self.submit_agent_prompt(prompt.to_string());
                self.focus = FocusTarget::AgentChatInput;
                self.touch_caret_activity();
            }
            HitAction::ApplyChatPatch(msg_idx) => self.apply_chat_patch(msg_idx),
            HitAction::InsertChatPatch(msg_idx) => self.insert_chat_patch(msg_idx),
            HitAction::PreviewPatchInEditor(msg_idx) => self.preview_chat_patch_in_editor(msg_idx),
            HitAction::TriggerCtrlKInlineEdit => self.trigger_ctrl_k_inline_edit(),
            HitAction::FocusCtrlKPrompt => {
                self.focus = FocusTarget::CtrlKPrompt;
                let start_x = self.ctrl_k_input_box_rect.x + 28;
                let idx = self.ctrl_k_input.hit_test_singleline(
                    canvas,
                    x,
                    start_x,
                    FontStyle::UiSmall,
                );
                self.ctrl_k_input.set_cursor_at(idx, shift);
                self.mouse_drag_target = Some(FocusTarget::CtrlKPrompt);
                self.touch_caret_activity();
            }
            HitAction::AcceptInlineDiff => self.accept_inline_diff(),
            HitAction::RejectInlineDiff => self.reject_inline_diff(),
            HitAction::SendSelectionToAgent => {
                self.execute_ide_action(IdeAction::SendSelectionToAgent)
            }
            HitAction::RevertLastCheckpoint => self.revert_last_checkpoint(),
            HitAction::FocusAgentChatInput => {
                self.focus = FocusTarget::AgentChatInput;
                let r = self.agent_input_text_rect;
                if r.w > 0 {
                    let vis_lines = (r.h / AGENT_INPUT_LINE_H).max(1) as usize;
                    let idx = self.agent_chat_input.hit_test_multiline(
                        canvas,
                        x,
                        y,
                        r.x,
                        r.y,
                        r.w,
                        AGENT_INPUT_LINE_H,
                        vis_lines,
                        AGENT_INPUT_FONT,
                    );
                    self.agent_chat_input.set_cursor_at(idx, shift);
                    self.mouse_drag_target = Some(FocusTarget::AgentChatInput);
                }
                self.touch_caret_activity();
            }
            HitAction::SubmitAgentChatPrompt => {
                let has_extras = !self.agent_hub.active_session().attached_items.is_empty()
                    || !self.agent_hub.active_session().pasted_snippets.is_empty();
                let prompt = if self.agent_chat_input.as_str().trim().is_empty() && !has_extras {
                    "请检查当前代码块的结构与错误处理，并给出修改建议".to_string()
                } else {
                    self.agent_chat_input.take_and_push_history()
                };
                self.submit_agent_prompt(prompt);
                self.focus = FocusTarget::AgentChatInput;
                self.touch_caret_activity();
            }
            HitAction::CancelAgentRequest => {
                self.agent_hub.cancel_active_request();
                self.agent_busy_started_at = None;
                self.agent_scroll = usize::MAX;
                self.status_notification = "已停止当前 AI 生成".to_string();
            }
            HitAction::OpenAiConfigModal => {
                self.open_ai_config_modal();
            }
            HitAction::SaveAiConfigModal => {
                self.save_ai_config_modal();
            }
            HitAction::FocusTerminalInput => {
                self.focus = FocusTarget::TerminalInput;
                if let Some((tl, tc)) = self.hit_test_terminal(x, y, canvas) {
                    let pty = self.terminal.active_pty_mut();
                    if !shift || pty.sel_anchor.is_none() {
                        pty.sel_anchor = Some((tl, tc));
                    }
                    pty.sel_cursor = Some((tl, tc));
                    self.mouse_drag_target = Some(FocusTarget::TerminalInput);
                }
                self.touch_caret_activity();
            }
            HitAction::NewTerminalTab => {
                self.terminal.add_new_tab();
                self.bottom_tool = Some(ToolWindowId::Terminal);
                self.focus = FocusTarget::TerminalInput;
                self.touch_caret_activity();
            }
            HitAction::SelectTerminalTab(idx) => {
                if idx < self.terminal.tabs.len() {
                    self.terminal.active_tab = idx;
                    self.focus = FocusTarget::TerminalInput;
                    self.touch_caret_activity();
                }
            }
            HitAction::CloseTerminalTab(idx) => {
                self.terminal.close_tab(idx);
            }
            HitAction::ClearTerminal => {
                self.terminal.active_pty_mut().clear();
            }
            HitAction::RunActiveConfig => self.execute_ide_action(IdeAction::RunActiveConfig),
            HitAction::DebugActiveConfig => self.execute_ide_action(IdeAction::DebugActiveConfig),
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
                self.focus = FocusTarget::TerminalInput;
                self.touch_caret_activity();
                self.status_notification = format!("已运行第 {} 行测试/入口", line + 1);
            }
            HitAction::SelectTheme(kind) => {
                self.theme = IdeaTheme::from_kind(kind);
                self.status_notification = format!(
                    "{}: {}",
                    self.tr("已应用主题", "Applied theme"),
                    kind.display_name()
                );
            }
            HitAction::SetUiLanguage(lang) => {
                self.set_ui_language(lang);
            }
            HitAction::SetUiScale(pct) => {
                self.set_ui_scale((pct as f32 / 100.0).clamp(0.85, 2.0));
            }
            HitAction::OpenFolderDialog => {
                self.request_open_folder_dialog_with_mode(FolderPickMode::AskUser);
            }
            HitAction::OpenFolderInNewWindowDialog => {
                self.request_open_folder_dialog_with_mode(FolderPickMode::NewWindow);
            }
            HitAction::PromptOpenWorkspace(path) => {
                self.project_popup_open = false;
                self.pending_open_workspace = Some(clean_workspace_path(path));
                self.focus = FocusTarget::OpenProjectChoiceModal;
            }
            HitAction::OpenWorkspaceInNewWindow(path) => {
                self.open_workspace_in_new_window(path);
            }
            HitAction::ConfirmOpenPendingInThisWindow => {
                if let Some(p) = self.pending_open_workspace.take() {
                    self.switch_workspace(p);
                }
                self.focus = FocusTarget::Editor;
            }
            HitAction::ConfirmOpenPendingInNewWindow => {
                if let Some(p) = self.pending_open_workspace.take() {
                    self.open_workspace_in_new_window(p);
                }
                self.focus = FocusTarget::Editor;
            }
            HitAction::CloseModalOrPopup => {
                self.pending_open_workspace = None;
                self.active_image_preview = None;
                self.prev_focus_before_image_preview = None;
                self.focus = FocusTarget::Editor;
                self.active_menu_category = None;
                self.git_branch_popup_open = false;
                self.agent_provider_popup_open = false;
                self.agent_model_popup_open = false;
                self.agent_sessions_popup_open = false;
                self.run_config_popup_open = false;
                self.project_popup_open = false;
                self.more_menu_popup_open = false;
                self.context_menu = None;
            }
        }
    }

    pub fn open_context_menu_at(&mut self, x: i32, y: i32, canvas: &SurfaceCanvas) {
        self.active_menu_category = None;
        self.git_branch_popup_open = false;
        self.agent_provider_popup_open = false;
        self.agent_model_popup_open = false;
        self.agent_sessions_popup_open = false;
        self.run_config_popup_open = false;
        self.project_popup_open = false;
        self.more_menu_popup_open = false;

        let item = |label: &str, shortcut: &str, action: ContextMenuAction, sep: bool| {
            ContextMenuItem {
                label: label.to_string(),
                shortcut: shortcut.to_string(),
                action,
                separator_before: sep,
            }
        };

        // 1. 编辑器标签页右键菜单
        if let Some((_, tab_idx)) = self.tab_rects.iter().find(|(r, _)| r.contains(x, y)).copied() {
            let mut items = vec![
                item(
                    self.tr("关闭当前标签页", "Close Tab"),
                    "Ctrl+F4",
                    ContextMenuAction::TabClose(tab_idx),
                    false,
                ),
                item(
                    self.tr("关闭其他标签页", "Close Other Tabs"),
                    "",
                    ContextMenuAction::TabCloseOthers(tab_idx),
                    false,
                ),
            ];
            if let Some(buf) = self.open_buffers.get(tab_idx) {
                let p = buf.path.clone();
                items.push(item(
                    self.tr("在资源管理器中显示", "Reveal in Explorer"),
                    "",
                    ContextMenuAction::TabRevealInExplorer(p.clone()),
                    true,
                ));
                items.push(item(
                    self.tr("复制完整路径", "Copy Full Path"),
                    "",
                    ContextMenuAction::TabCopyPath(p),
                    false,
                ));
            }
            self.context_menu = Some(ContextMenuState { x, y, items });
            return;
        }

        // 2. 左侧项目树右键菜单
        if self.left_tool == Some(ToolWindowId::Project) && self.left_pane_rect.contains(x, y) {
            if let Some((_, path, is_dir)) = self
                .project_entry_rects
                .iter()
                .find(|(r, _, _)| r.contains(x, y))
                .cloned()
            {
                self.project_tree.selected_path = Some(path.clone());
                let mut items = Vec::new();
                if !is_dir {
                    items.push(item(
                        self.tr("打开文件", "Open File"),
                        "Enter",
                        ContextMenuAction::TreeOpenFile(path.clone()),
                        false,
                    ));
                }
                items.push(item(
                    self.tr("新建文件...", "New File..."),
                    "Alt+Ins",
                    ContextMenuAction::TreeNewFileIn(path.clone()),
                    !is_dir,
                ));
                items.push(item(
                    self.tr("新建文件夹...", "New Folder..."),
                    "",
                    ContextMenuAction::TreeNewDirIn(path.clone()),
                    false,
                ));
                items.push(item(
                    self.tr("重命名...", "Rename..."),
                    "F2",
                    ContextMenuAction::TreeRename(path.clone()),
                    true,
                ));
                items.push(item(
                    self.tr("删除", "Delete"),
                    "Alt+Del",
                    ContextMenuAction::TreeDelete(path.clone()),
                    false,
                ));
                items.push(item(
                    self.tr("在资源管理器中显示", "Reveal in Explorer"),
                    "",
                    ContextMenuAction::TreeRevealInExplorer(path.clone()),
                    true,
                ));
                items.push(item(
                    self.tr("复制完整路径", "Copy Full Path"),
                    "",
                    ContextMenuAction::TreeCopyPath(path),
                    false,
                ));
                items.push(item(
                    self.tr("展开全部目录", "Expand All"),
                    "",
                    ContextMenuAction::TreeExpandAll,
                    true,
                ));
                items.push(item(
                    self.tr("折叠全部目录", "Collapse All"),
                    "",
                    ContextMenuAction::TreeCollapseAll,
                    false,
                ));
                items.push(item(
                    self.tr("打开其他文件夹...", "Open Folder..."),
                    "Ctrl+O",
                    ContextMenuAction::Ide(IdeAction::OpenFolder),
                    true,
                ));
                self.context_menu = Some(ContextMenuState { x, y, items });
                return;
            }
            let root = self.workspace_root.clone();
            let items = vec![
                item(
                    self.tr("新建文件...", "New File..."),
                    "Alt+Ins",
                    ContextMenuAction::TreeNewFileIn(root.clone()),
                    false,
                ),
                item(
                    self.tr("新建文件夹...", "New Folder..."),
                    "",
                    ContextMenuAction::TreeNewDirIn(root),
                    false,
                ),
                item(
                    self.tr("展开全部目录", "Expand All"),
                    "",
                    ContextMenuAction::TreeExpandAll,
                    true,
                ),
                item(
                    self.tr("折叠全部目录", "Collapse All"),
                    "",
                    ContextMenuAction::TreeCollapseAll,
                    false,
                ),
                item(
                    self.tr("打开其他文件夹...", "Open Folder..."),
                    "Ctrl+O",
                    ContextMenuAction::Ide(IdeAction::OpenFolder),
                    true,
                ),
            ];
            self.context_menu = Some(ContextMenuState { x, y, items });
            return;
        }

        // 2.5 左侧 Git 提交面板右键菜单
        if self.left_tool == Some(ToolWindowId::Commit) && self.left_pane_rect.contains(x, y) {
            if let Some((_, path, staged)) = self
                .git_change_row_rects
                .iter()
                .find(|(r, _, _)| r.contains(x, y))
                .cloned()
            {
                let items = vec![
                    item(
                        if staged {
                            self.tr("取消暂存文件 (Unstage)", "Unstage File")
                        } else {
                            self.tr("暂存文件 (Stage)", "Stage File")
                        },
                        "Space",
                        ContextMenuAction::GitToggleStage(path.clone()),
                        false,
                    ),
                    item(
                        self.tr("查看变更 Diff (Show Diff)", "Show Diff"),
                        "Ctrl+D",
                        ContextMenuAction::GitShowWorkingDiff(path.clone()),
                        false,
                    ),
                    item(
                        self.tr("打开文件 (Jump to Source)", "Jump to Source"),
                        "F4",
                        ContextMenuAction::TreeOpenFile(path.clone()),
                        false,
                    ),
                    item(
                        self.tr("回滚此文件修改 (Rollback)", "Rollback File Changes"),
                        "Ctrl+Alt+Z",
                        ContextMenuAction::GitRollbackFile(path.clone()),
                        true,
                    ),
                    item(
                        self.tr("在资源管理器中显示", "Reveal in Explorer"),
                        "",
                        ContextMenuAction::TreeRevealInExplorer(path.clone()),
                        true,
                    ),
                    item(
                        self.tr("复制完整路径", "Copy Full Path"),
                        "",
                        ContextMenuAction::TreeCopyPath(path),
                        false,
                    ),
                ];
                self.context_menu = Some(ContextMenuState { x, y, items });
                return;
            }
        }

        // 2.8 底部 Git 历史库 (Git Log) 右键菜单
        if self.bottom_tool == Some(ToolWindowId::GitLog) && self.bottom_pane_rect.contains(x, y) {
            if let Some((_, hash, subject)) = self
                .git_commit_row_rects
                .iter()
                .find(|(r, _, _)| r.contains(x, y))
                .cloned()
            {
                self.selected_git_commit_hash = Some(hash.clone());
                self.git_engine.request_load_commit_detail(&hash);
                let items = vec![
                    item(
                        self.tr("检出此提交版本 (Checkout Revision)", "Checkout Revision"),
                        "",
                        ContextMenuAction::GitCheckoutRevision(hash.clone()),
                        false,
                    ),
                    item(
                        self.tr("基于此提交新建分支...", "New Branch from Commit..."),
                        "",
                        ContextMenuAction::GitNewBranchFromCommit(hash.clone()),
                        false,
                    ),
                    item(
                        self.tr("拣选此提交 (Cherry-Pick)", "Cherry-Pick Commit"),
                        "",
                        ContextMenuAction::GitCherryPickCommit(hash.clone()),
                        true,
                    ),
                    item(
                        self.tr("还原此提交 (Revert Commit)", "Revert Commit"),
                        "",
                        ContextMenuAction::GitRevertCommit(hash.clone()),
                        false,
                    ),
                    item(
                        self.tr(
                            "软重置当前分支到此提交 (Reset Soft)",
                            "Reset Current Branch to Here (Soft)",
                        ),
                        "",
                        ContextMenuAction::GitResetSoftToCommit(hash.clone()),
                        false,
                    ),
                    item(
                        self.tr("复制 Commit Hash", "Copy Revision Number"),
                        "Ctrl+C",
                        ContextMenuAction::GitCopyText(hash),
                        true,
                    ),
                    item(
                        self.tr("复制提交说明", "Copy Commit Message"),
                        "",
                        ContextMenuAction::GitCopyText(subject),
                        false,
                    ),
                ];
                self.context_menu = Some(ContextMenuState { x, y, items });
                return;
            }
        }

        // 3. 原生终端右键菜单
        if self.bottom_tool == Some(ToolWindowId::Terminal) && self.bottom_pane_rect.contains(x, y)
        {
            self.focus = FocusTarget::TerminalInput;
            self.touch_caret_activity();
            let items = vec![
                item(
                    self.tr("复制选中文本", "Copy Selection"),
                    "Ctrl+C",
                    ContextMenuAction::TermCopy,
                    false,
                ),
                item(
                    self.tr("粘贴到终端", "Paste"),
                    "Ctrl+V",
                    ContextMenuAction::TermPaste,
                    false,
                ),
                item(
                    self.tr("全选终端缓冲", "Select All"),
                    "Ctrl+A",
                    ContextMenuAction::TermSelectAll,
                    false,
                ),
                item(
                    self.tr("发送中断信号 (SIGINT)", "Send Interrupt (SIGINT)"),
                    "Ctrl+C",
                    ContextMenuAction::TermSendInterrupt,
                    true,
                ),
                item(
                    self.tr("清空终端屏幕", "Clear Terminal"),
                    "Ctrl+L",
                    ContextMenuAction::TermClear,
                    false,
                ),
                item(
                    self.tr("新建终端标签页", "New Terminal Tab"),
                    "",
                    ContextMenuAction::TermNewTab,
                    true,
                ),
            ];
            self.context_menu = Some(ContextMenuState { x, y, items });
            return;
        }

        // 4. 各类输入框右键菜单
        let input_target_opt = if self.agent_hub.inline_edit.as_ref().is_some_and(|s| s.active)
            && self.ctrl_k_input_box_rect.contains(x, y)
        {
            Some(FocusTarget::CtrlKPrompt)
        } else if self.right_agent_open && self.agent_input_text_rect.contains(x, y) {
            Some(FocusTarget::AgentChatInput)
        } else if self.left_tool == Some(ToolWindowId::Commit)
            && self.git_commit_input_box_rect.contains(x, y)
        {
            Some(FocusTarget::GitCommitInput)
        } else if self.bottom_tool == Some(ToolWindowId::GitLog)
            && self.git_log_search_rect.contains(x, y)
        {
            Some(FocusTarget::GitLogSearchInput)
        } else if matches!(self.focus, FocusTarget::AiConfigModal(_)) {
            (0..3)
                .find(|&fi| self.ai_cfg_field_rects[fi].contains(x, y))
                .map(FocusTarget::AiConfigModal)
        } else if matches!(
            self.focus,
            FocusTarget::NewFileModal
                | FocusTarget::NewFolderModal
                | FocusTarget::NewGitBranchModal
                | FocusTarget::RenamePathModal
        ) && self.prompt_modal_input_rect.contains(x, y)
        {
            Some(self.focus)
        } else {
            None
        };

        if let Some(target) = input_target_opt {
            self.focus = target;
            self.touch_caret_activity();
            let items = vec![
                item(
                    self.tr("剪切", "Cut"),
                    "Ctrl+X",
                    ContextMenuAction::InputCut(target),
                    false,
                ),
                item(
                    self.tr("复制", "Copy"),
                    "Ctrl+C",
                    ContextMenuAction::InputCopy(target),
                    false,
                ),
                item(
                    self.tr("粘贴", "Paste"),
                    "Ctrl+V",
                    ContextMenuAction::InputPaste(target),
                    false,
                ),
                item(
                    self.tr("全选", "Select All"),
                    "Ctrl+A",
                    ContextMenuAction::InputSelectAll(target),
                    true,
                ),
            ];
            self.context_menu = Some(ContextMenuState { x, y, items });
            return;
        }

        // 5. 代码编辑器右键菜单
        if self.editor_code_rect.contains(x, y) || self.editor_pane_rect.contains(x, y) {
            self.focus = FocusTarget::Editor;
            if !self.active_buffer().has_selection() {
                if let Some((line, col)) = self.hit_test_editor(x, y, canvas) {
                    self.active_buffer_mut().set_cursor(line, col, false);
                }
            }
            self.touch_caret_activity();
            let items = vec![
                item(
                    self.tr("剪切", "Cut"),
                    "Ctrl+X",
                    ContextMenuAction::EditorCut,
                    false,
                ),
                item(
                    self.tr("复制", "Copy"),
                    "Ctrl+C",
                    ContextMenuAction::EditorCopy,
                    false,
                ),
                item(
                    self.tr("粘贴", "Paste"),
                    "Ctrl+V",
                    ContextMenuAction::EditorPaste,
                    false,
                ),
                item(
                    self.tr("全选", "Select All"),
                    "Ctrl+A",
                    ContextMenuAction::EditorSelectAll,
                    false,
                ),
                item(
                    self.tr("复制当前行", "Duplicate Line"),
                    "Ctrl+D",
                    ContextMenuAction::Ide(IdeAction::DuplicateLine),
                    true,
                ),
                item(
                    self.tr("删除当前行", "Delete Line"),
                    "Ctrl+Y",
                    ContextMenuAction::Ide(IdeAction::DeleteLine),
                    false,
                ),
                item(
                    self.tr("切换行注释", "Toggle Line Comment"),
                    "Ctrl+/",
                    ContextMenuAction::Ide(IdeAction::ToggleLineComment),
                    false,
                ),
                item(
                    self.tr("整理空白并保存", "Reformat & Save"),
                    "Ctrl+Alt+L",
                    ContextMenuAction::Ide(IdeAction::ReformatCode),
                    false,
                ),
                item(
                    self.tr("AI 行内改写", "Inline AI Edit"),
                    "Ctrl+K",
                    ContextMenuAction::Ide(IdeAction::InlineAiEdit),
                    true,
                ),
                item(
                    self.tr("发送代码至 AI", "Send to AI Assistant"),
                    "Alt+L",
                    ContextMenuAction::Ide(IdeAction::SendSelectionToAgent),
                    false,
                ),
            ];
            self.context_menu = Some(ContextMenuState { x, y, items });
            return;
        }

        // 6. AI 智能体对话流右键菜单（复制选中文本 / 全选 / 复制全部对话）
        if self.right_agent_open
            && self.right_panel_tab == RightPanelTab::Chat
            && self.agent_chat_area_rect.contains(x, y)
        {
            self.focus = FocusTarget::AgentChatStream;
            let mut items = Vec::new();
            if self.copy_agent_chat_selection().is_some() {
                items.push(item(
                    self.tr("复制选中文本", "Copy Selected Text"),
                    "Ctrl+C",
                    ContextMenuAction::AgentCopySelection,
                    false,
                ));
            }
            items.push(item(
                self.tr("全选对话文本", "Select All Chat"),
                "Ctrl+A",
                ContextMenuAction::InputSelectAll(FocusTarget::AgentChatStream),
                false,
            ));
            items.push(item(
                self.tr("复制全部对话", "Copy Entire Chat"),
                "",
                ContextMenuAction::AgentCopyAllChat,
                true,
            ));
            self.context_menu = Some(ContextMenuState { x, y, items });
            return;
        }

        self.context_menu = None;
    }

    fn reveal_path_in_os_explorer(&mut self, path: &std::path::Path) {
        #[cfg(target_os = "windows")]
        {
            let arg = format!("/select,{}", path.display());
            let _ = std::process::Command::new("explorer.exe").arg(arg).spawn();
        }
        self.status_notification = format!(
            "{}: {}",
            self.tr("已在文件管理器中定位", "Revealed in Explorer"),
            path.display()
        );
    }

    pub fn execute_context_menu_action(&mut self, action: ContextMenuAction) {
        match action {
            ContextMenuAction::Ide(ide_act) => self.execute_ide_action(ide_act),
            ContextMenuAction::EditorCut => {
                self.focus = FocusTarget::Editor;
                if let Some(cut) = self.cut_focused_selection() {
                    write_windows_clipboard_text(&cut);
                }
            }
            ContextMenuAction::EditorCopy => {
                self.focus = FocusTarget::Editor;
                if let Some(sel) = self.copy_focused_selection() {
                    write_windows_clipboard_text(&sel);
                }
            }
            ContextMenuAction::EditorPaste => {
                self.focus = FocusTarget::Editor;
                if let Some(clip) = read_windows_clipboard_text() {
                    self.insert_text_at_focus(&clip);
                }
            }
            ContextMenuAction::EditorSelectAll => {
                self.focus = FocusTarget::Editor;
                let _ = self.select_all_at_focus();
            }
            ContextMenuAction::InputCut(target) => {
                self.focus = target;
                if let Some(cut) = self.cut_focused_selection() {
                    write_windows_clipboard_text(&cut);
                }
            }
            ContextMenuAction::InputCopy(target) => {
                self.focus = target;
                if let Some(sel) = self.copy_focused_selection() {
                    write_windows_clipboard_text(&sel);
                }
            }
            ContextMenuAction::InputPaste(target) => {
                self.focus = target;
                self.paste_into_agent_or_focus();
            }
            ContextMenuAction::InputSelectAll(target) => {
                self.focus = target;
                let _ = self.select_all_at_focus();
            }
            ContextMenuAction::TreeOpenFile(path) => {
                self.open_file(path);
                self.focus = FocusTarget::Editor;
                self.touch_caret_activity();
            }
            ContextMenuAction::TreeNewFileIn(path) => {
                self.project_tree.selected_path = Some(path);
                self.execute_ide_action(IdeAction::NewFileInProject);
            }
            ContextMenuAction::TreeNewDirIn(path) => {
                self.project_tree.selected_path = Some(path);
                self.execute_ide_action(IdeAction::NewFolderInProject);
            }
            ContextMenuAction::TreeRename(path) => {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                self.rename_target_path = Some(path);
                self.modal_text_input.set_text(&name);
                self.modal_text_input.select_all();
                self.focus = FocusTarget::RenamePathModal;
                self.touch_caret_activity();
            }
            ContextMenuAction::TreeDelete(path) => {
                self.project_tree.selected_path = Some(path);
                self.execute_ide_action(IdeAction::DeleteSelectedPath);
            }
            ContextMenuAction::TreeRevealInExplorer(path)
            | ContextMenuAction::TabRevealInExplorer(path) => {
                self.reveal_path_in_os_explorer(&path);
            }
            ContextMenuAction::TreeCopyPath(path) | ContextMenuAction::TabCopyPath(path) => {
                let s = path.display().to_string();
                write_windows_clipboard_text(&s);
                self.status_notification =
                    format!("{}: {s}", self.tr("已复制路径", "Copied path"));
            }
            ContextMenuAction::TreeExpandAll => {
                self.project_tree.expand_all();
                self.status_notification = self
                    .tr("已展开全部项目目录", "Expanded all project folders")
                    .to_string();
            }
            ContextMenuAction::TreeCollapseAll => {
                self.project_tree.collapse_all();
                self.status_notification = self
                    .tr("已折叠全部项目目录", "Collapsed all project folders")
                    .to_string();
            }
            ContextMenuAction::TabClose(idx) => self.close_tab(idx),
            ContextMenuAction::TabCloseOthers(keep_idx) => {
                if self.open_buffers.len() > 1 && keep_idx < self.open_buffers.len() {
                    let kept = self.open_buffers.remove(keep_idx);
                    self.open_buffers.clear();
                    self.open_buffers.push(kept);
                    self.active_tab = 0;
                    self.status_notification = self
                        .tr("已关闭其他标签页", "Closed other tabs")
                        .to_string();
                }
            }
            ContextMenuAction::TermCopy => {
                if let Some(sel) = self.terminal.active_pty().selected_text() {
                    write_windows_clipboard_text(&sel);
                    self.status_notification = self
                        .tr("已复制终端选区文本", "Copied terminal selection")
                        .to_string();
                }
            }
            ContextMenuAction::TermPaste => {
                self.focus = FocusTarget::TerminalInput;
                if let Some(clip) = read_windows_clipboard_text() {
                    self.terminal.active_pty_mut().send_text(&clip);
                }
            }
            ContextMenuAction::TermSelectAll => {
                self.focus = FocusTarget::TerminalInput;
                self.terminal.active_pty_mut().select_all();
            }
            ContextMenuAction::TermSendInterrupt => {
                self.focus = FocusTarget::TerminalInput;
                self.terminal.active_pty_mut().send_interrupt();
            }
            ContextMenuAction::TermClear => {
                self.terminal.active_pty_mut().clear();
            }
            ContextMenuAction::TermNewTab => {
                self.terminal.add_new_tab();
                self.bottom_tool = Some(ToolWindowId::Terminal);
                self.focus = FocusTarget::TerminalInput;
            }
            ContextMenuAction::GitToggleStage(path) => {
                self.git_engine.request_toggle_stage_file(&path);
            }
            ContextMenuAction::GitShowWorkingDiff(path) => {
                self.git_working_diff_target = Some(path.clone());
                self.git_diff_scroll = 0;
                self.bottom_tool = Some(ToolWindowId::GitLog);
                self.open_file(path);
            }
            ContextMenuAction::GitRollbackFile(path) => {
                self.git_engine.request_rollback_file(&path);
                self.status_notification = format!("正在回滚 {}...", path.display());
            }
            ContextMenuAction::GitCheckoutRevision(rev) => {
                self.git_engine.request_checkout_branch(&rev);
                self.status_notification = format!("正在检出版本 '{rev}'...");
            }
            ContextMenuAction::GitNewBranchFromCommit(rev) => {
                self.git_new_branch_from_ref = Some(rev.clone());
                self.modal_text_input.set_text(&format!("branch-from-{rev}"));
                self.modal_text_input.select_all();
                self.focus = FocusTarget::NewGitBranchModal;
                self.touch_caret_activity();
            }
            ContextMenuAction::GitCherryPickCommit(hash) => {
                self.git_engine.request_cherry_pick(&hash);
                self.status_notification = format!("正在 Cherry-Pick 提交 {hash}...");
            }
            ContextMenuAction::GitRevertCommit(hash) => {
                self.git_engine.request_revert_commit(&hash);
                self.status_notification = format!("正在还原提交 {hash}...");
            }
            ContextMenuAction::GitResetSoftToCommit(hash) => {
                self.git_engine.request_reset_soft(&hash);
                self.status_notification = format!("正在软重置当前分支至 {hash}...");
            }
            ContextMenuAction::GitCopyText(txt) => {
                write_windows_clipboard_text(&txt);
                self.status_notification = format!(
                    "{}: {txt}",
                    self.tr("已复制到剪贴板", "Copied to clipboard")
                );
            }
            ContextMenuAction::AgentCopySelection => {
                if let Some(txt) = self.copy_agent_chat_selection() {
                    write_windows_clipboard_text(&txt);
                    self.status_notification = self
                        .tr("已复制选中对话文本到剪贴板", "Copied selected chat text")
                        .to_string();
                } else {
                    self.status_notification = self
                        .tr("请先拖选要复制的对话文本", "Select text in chat stream first")
                        .to_string();
                }
            }
            ContextMenuAction::AgentCopyAllChat => {
                let all = self.copy_all_agent_chat();
                if !all.is_empty() {
                    write_windows_clipboard_text(&all);
                    self.status_notification = self
                        .tr("已复制全部 AI 对话内容到剪贴板", "Copied entire AI conversation")
                        .to_string();
                }
            }
        }
    }

    fn render_context_menu_popup(&mut self, canvas: &mut SurfaceCanvas, win_w: i32, win_h: i32) {
        let Some(menu) = self.context_menu.clone() else {
            return;
        };
        if menu.items.is_empty() {
            return;
        }
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let sep_count = menu.items.iter().filter(|i| i.separator_before).count() as i32;
        let mut max_row_w = 276;
        for item in &menu.items {
            let (lw, _) = canvas.measure_text(&item.label, FontStyle::UiRegular);
            let (sw, _) = if item.shortcut.is_empty() {
                (0, 0)
            } else {
                canvas.measure_text(&item.shortcut, FontStyle::UiSmall)
            };
            max_row_w = max_row_w.max(lw + sw + 56);
        }
        let menu_w = max_row_w.min((win_w - 16).max(220));
        let menu_h = (menu.items.len() as i32) * 29 + sep_count * 8 + 10;
        let mx = menu.x.clamp(8, (win_w - menu_w - 8).max(8));
        let my = menu.y.clamp(8, (win_h - menu_h - 8).max(8));
        let rect = Rect::new(mx, my, menu_w, menu_h);
        self.context_menu_rect = rect;
        self.active_overlay_rects.push(rect);

        canvas.fill_rounded_shadow(rect, 7);
        canvas.stroke_rounded_rect(rect, 7, t.border_color, t.popup_bg);

        let mut ry = my + 5;
        for item in menu.items {
            if item.separator_before {
                canvas.fill_rect(Rect::new(mx + 8, ry + 3, menu_w - 16, 1), t.border_color);
                ry += 8;
            }
            let row = Rect::new(mx + 5, ry, menu_w - 10, 28);
            let hovered = row.contains(hx, hy);
            if hovered {
                canvas.fill_rounded_rect(row, 5, t.popup_hover_bg);
            }
            let sw = if item.shortcut.is_empty() {
                0
            } else {
                canvas.measure_text(&item.shortcut, FontStyle::UiSmall).0
            };
            let label_max_w = if sw > 0 {
                (menu_w - sw - 40).max(90)
            } else {
                (menu_w - 28).max(90)
            };
            canvas.draw_text_styled(
                mx + 14,
                ry + 4,
                &item.label,
                if hovered { 0xFFFFFF } else { t.text_primary },
                label_max_w,
                FontStyle::UiRegular,
            );
            if !item.shortcut.is_empty() {
                canvas.draw_text_styled(
                    mx + menu_w - sw - 14,
                    ry + 5,
                    &item.shortcut,
                    if hovered { 0xE2E8F0 } else { t.text_muted },
                    sw + 4,
                    FontStyle::UiSmall,
                );
            }
            self.hit_regions
                .push((row, HitAction::TriggerContextMenuAction(item.action)));
            ry += 29;
        }
    }

    /// 渲染最新 JetBrains IntelliJ IDEA (Islands New UI) 完整工作台
    pub fn render(&mut self, canvas: &mut SurfaceCanvas) {
        self.hit_regions.clear();
        self.tab_rects.clear();
        self.project_entry_rects.clear();
        self.git_commit_row_rects.clear();
        self.git_change_row_rects.clear();
        self.active_overlay_rects.clear();
        self.overlay_hit_start = usize::MAX;
        self.context_menu_rect = Rect::new(0, 0, 0, 0);
        self.search_modal_input_rect = Rect::new(0, 0, 0, 0);
        self.prompt_modal_input_rect = Rect::new(0, 0, 0, 0);
        self.git_log_search_rect = Rect::new(0, 0, 0, 0);
        let w = canvas.width as i32;
        let h = canvas.height as i32;
        self.last_win_size = (w, h);
        let t = self.theme;

        // 1. Islands 外围深色底板
        canvas.clear(t.window_bg);

        let top_bar_h = 38;
        let status_bar_h = 26;
        let left_stripe_w = 42;
        let right_stripe_w = 42;

        // 2. 顶部 Main Toolbar + 自定义无边框窗口标题控制栏
        self.render_main_toolbar(canvas, Rect::new(0, 0, w, top_bar_h));

        // 3. 底部 Status Bar
        self.render_status_bar(canvas, Rect::new(0, h - status_bar_h, w, status_bar_h));

        let body_y = top_bar_h;
        let body_h = (h - top_bar_h - status_bar_h).max(120);

        // 4. 左右 Tool Window 矢量图标停靠条 (Tool Stripes)
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
            self.left_pane_rect = island;
            self.render_left_tool_window(canvas, island, tool);
        } else {
            self.left_pane_rect = Rect::new(0, 0, 0, 0);
            self.left_scroll_info = ScrollTrackInfo::default();
        }

        // 6. 右侧 Multi-Agent 悬浮岛屿面板
        if self.right_agent_open {
            let island = Rect::new(
                w - right_stripe_w - right_w + 2,
                body_y + 2,
                right_w - 4,
                upper_h - 4,
            );
            self.right_pane_rect = island;
            self.render_right_agent_hub(canvas, island);
        } else {
            self.right_pane_rect = Rect::new(0, 0, 0, 0);
            self.right_scroll_info = ScrollTrackInfo::default();
        }

        // 7. 中央代码编辑器区域
        let editor_x = left_stripe_w + left_w + 2;
        let editor_w = (w - left_stripe_w - right_stripe_w - left_w - right_w - 4).max(220);
        let editor_rect = Rect::new(editor_x, body_y + 2, editor_w, upper_h - 4);
        self.editor_pane_rect = editor_rect;
        self.render_center_editor(canvas, editor_rect);

        // 8. 底部悬浮岛屿面板 (Terminal / Problems / Debug / Git Log)
        if let Some(btool) = self.bottom_tool {
            let bottom_island = Rect::new(
                left_stripe_w + 2,
                body_y + upper_h + 2,
                w - left_stripe_w - right_stripe_w - 4,
                bottom_h - 4,
            );
            self.bottom_pane_rect = bottom_island;
            self.render_bottom_tool_window(canvas, bottom_island, btool);
        } else {
            self.bottom_pane_rect = Rect::new(0, 0, 0, 0);
            self.terminal_scroll_info = ScrollTrackInfo::default();
        }

        // 8.5 分割线悬浮/拖拽视觉反馈线
        let active_split = self.active_splitter_drag.or_else(|| {
            let (hx, hy) = self.hover_pos;
            self.splitter_at(hx, hy, w, h)
        });
        if let Some(split) = active_split {
            match split {
                DragSplitter::LeftPanel => {
                    let sx = left_stripe_w + left_w - 1;
                    canvas.fill_rect(Rect::new(sx, body_y + 4, 2, upper_h - 8), t.accent_blue);
                }
                DragSplitter::RightPanel => {
                    let sx = w - right_stripe_w - right_w - 1;
                    canvas.fill_rect(Rect::new(sx, body_y + 4, 2, upper_h - 8), t.accent_blue);
                }
                DragSplitter::BottomPanel => {
                    let sy = body_y + upper_h - 1;
                    canvas.fill_rect(
                        Rect::new(left_stripe_w + 4, sy, w - left_stripe_w - right_stripe_w - 8, 2),
                        t.accent_blue,
                    );
                }
            }
        }

        // 8.7 温和、克制的启动工作区与后台加载悬浮指示条（自然淡入淡出，不遮挡操作）
        if self.startup_load_alpha > 0.02 {
            let a = self.startup_load_alpha;
            let pill_w = 248;
            let pill_h = 28;
            let px = (self.editor_pane_rect.x + self.editor_pane_rect.w - pill_w - 18).max(52);
            let py = (self.editor_pane_rect.y + self.editor_pane_rect.h - pill_h - 14).max(48);
            let pill = Rect::new(px, py, pill_w, pill_h);
            let bg_alpha = (a * 235.0).round() as u8;
            canvas.fill_rounded_rect_alpha(pill, 7, t.popup_bg, bg_alpha);
            canvas.draw_subtle_spinner(
                px + 15,
                py + pill_h / 2,
                5.5,
                self.spinner_phase(),
                t.accent_blue,
                t.border_color,
                a,
            );
            let txt_color = blend_rgb(t.popup_bg, t.text_secondary, a);
            canvas.draw_text_styled(
                px + 28,
                py + 5,
                self.tr("正在加载工作区索引与 Git 状态...", "Loading workspace index & Git..."),
                txt_color,
                pill_w - 36,
                FontStyle::UiSmall,
            );
        }

        // 9. 顶层弹出菜单与模态对话框
        self.overlay_hit_start = self.hit_regions.len();
        if self.hamburger_menu_expanded {
            self.render_expanded_menu_strip(canvas);
        }
        if let Some(cat) = self.active_menu_category {
            self.render_main_menu_dropdown(canvas, cat);
        }
        if self.project_popup_open {
            self.render_project_popup(canvas);
        }
        if self.git_branch_popup_open {
            self.render_git_branch_popup(canvas);
        }
        if self.run_config_popup_open {
            self.render_run_config_popup(canvas, w);
        }
        if self.more_menu_popup_open {
            self.render_more_menu_popup(canvas, w);
        }
        if self.agent_provider_popup_open && self.right_agent_open {
            self.render_agent_provider_popup(canvas);
        }
        if self.agent_model_popup_open && self.right_agent_open {
            self.render_agent_model_popup(canvas);
        }
        if self.agent_mode_popup_open && self.right_agent_open {
            self.render_agent_mode_popup(canvas);
        }
        if self.agent_sessions_popup_open && self.right_agent_open {
            self.render_agent_sessions_popup(canvas);
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
            | FocusTarget::NewGitBranchModal
            | FocusTarget::RenamePathModal => {
                self.render_input_prompt_modal(canvas, w, h);
            }
            FocusTarget::SettingsModal => {
                self.render_settings_modal(canvas, w, h);
            }
            FocusTarget::AiConfigModal(_) => {
                self.render_ai_config_modal(canvas, w, h);
            }
            FocusTarget::OpenProjectChoiceModal => {
                self.render_open_project_choice_modal(canvas, w, h);
            }
            FocusTarget::ImagePreviewModal => {
                self.render_image_preview_modal(canvas, w, h);
            }
            _ => {}
        }

        self.render_context_menu_popup(canvas, w, h);
        self.render_drag_ghost_overlay(canvas);

        // 10. 无边框自定义窗口 1px 外边框精修（非最大化时显示）
        if !self.window_maximized {
            canvas.stroke_rect_border(Rect::new(0, 0, w, h), t.border_color);
        }
    }

    fn render_main_toolbar(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        canvas.fill_rect(rect, t.main_toolbar_bg);
        canvas.fill_rect(
            Rect::new(rect.x, rect.y + rect.h - 1, rect.w, 1),
            t.border_color,
        );

        // 1. 左侧：RI 品牌徽标 + ≡ 主菜单图标（点击展开/收起 IDEA 菜单栏）
        let logo_rect = Rect::new(10, 7, 24, 24);
        canvas.draw_ri_logo_icon(logo_rect);
        self.hit_regions
            .push((logo_rect, HitAction::ToggleHamburgerMenu));

        let ham_btn = Rect::new(38, 6, 26, 26);
        if self.hamburger_menu_expanded {
            canvas.fill_rounded_rect(ham_btn, 5, t.popup_hover_bg);
        } else if ham_btn.contains(hx, hy) {
            canvas.fill_rounded_rect(ham_btn, 5, t.panel_bg);
        }
        canvas.draw_hamburger_icon(44, 13, t.text_secondary);
        self.hit_regions
            .push((ham_btn, HitAction::ToggleHamburgerMenu));

        // 2. [WS] 动态工作区徽标 + 项目名 ▾
        let ws_badge = self.workspace_badge();
        let ws_name = self.workspace_name();
        let (ws_tw, _) = canvas.measure_text(&ws_name, FontStyle::UiBold);
        let proj_w = (ws_tw + 52).clamp(118, 220);
        let mut cur_x = 68;
        let proj_box = Rect::new(cur_x, 5, proj_w, 28);
        if self.project_popup_open || proj_box.contains(hx, hy) {
            canvas.fill_rounded_rect(proj_box, 6, t.panel_bg);
        }
        let badge_rect = Rect::new(cur_x + 4, 9, 20, 20);
        canvas.fill_rounded_rect(badge_rect, 4, 0xCF6A33);
        canvas.draw_text_centered(badge_rect, &ws_badge, 0xFFFFFF, FontStyle::UiSmall);
        canvas.draw_text_styled(
            cur_x + 30,
            10,
            &ws_name,
            t.text_primary,
            proj_w - 50,
            FontStyle::UiBold,
        );
        canvas.draw_chevron_down(cur_x + proj_w - 16, 17, t.text_muted);
        self.hit_regions
            .push((proj_box, HitAction::ToggleProjectPopup));
        cur_x += proj_w + 6;

        // 3. Git 分支 Widget: ⎇ master ▾ (+ 柔和加载旋转弧)
        let branch_name = self.git_engine.current_branch.clone();
        let (btw, _) = canvas.measure_text(&branch_name, FontStyle::UiRegular);
        let git_extra = if self.git_load_alpha > 0.02 { 16 } else { 0 };
        let git_w = (btw + 44 + git_extra).clamp(84, 170);
        let git_box = Rect::new(cur_x, 5, git_w, 28);
        if self.git_branch_popup_open || git_box.contains(hx, hy) {
            canvas.fill_rounded_rect(git_box, 6, t.panel_bg);
        }
        canvas.draw_git_branch_icon(cur_x + 8, 12, t.text_secondary);
        canvas.draw_text_styled(
            cur_x + 24,
            10,
            &branch_name,
            t.text_primary,
            git_w - 40 - git_extra,
            FontStyle::UiRegular,
        );
        if self.git_load_alpha > 0.02 {
            canvas.draw_subtle_spinner(
                cur_x + git_w - 25,
                19,
                4.5,
                self.spinner_phase(),
                t.accent_blue,
                t.border_color,
                self.git_load_alpha,
            );
        }
        canvas.draw_chevron_down(cur_x + git_w - 15, 17, t.text_muted);
        self.hit_regions
            .push((git_box, HitAction::ToggleGitBranchPopup));

        // 4. 最右侧：自定义无边框窗口控制按钮 [—] [□/❐] [×] (预留 138px)
        let cap_w = 46;
        let cap_h = rect.h - 1;
        let min_rect = Rect::new(rect.w - cap_w * 3, 0, cap_w, cap_h);
        let max_rect = Rect::new(rect.w - cap_w * 2, 0, cap_w, cap_h);
        let close_rect = Rect::new(rect.w - cap_w, 0, cap_w, cap_h);

        if min_rect.contains(hx, hy) {
            canvas.fill_rect(min_rect, t.panel_bg);
        }
        canvas.draw_window_minimize_icon(
            min_rect.x + cap_w / 2,
            min_rect.y + cap_h / 2,
            t.text_secondary,
        );
        self.hit_regions
            .push((min_rect, HitAction::WindowControl(WindowControlCmd::Minimize)));

        if max_rect.contains(hx, hy) {
            canvas.fill_rect(max_rect, t.panel_bg);
        }
        if self.window_maximized {
            canvas.draw_window_restore_icon(
                max_rect.x + cap_w / 2,
                max_rect.y + cap_h / 2,
                t.text_secondary,
            );
        } else {
            canvas.draw_window_maximize_icon(
                max_rect.x + cap_w / 2,
                max_rect.y + cap_h / 2,
                t.text_secondary,
            );
        }
        self.hit_regions.push((
            max_rect,
            HitAction::WindowControl(WindowControlCmd::ToggleMaximize),
        ));

        let close_hovered = close_rect.contains(hx, hy);
        if close_hovered {
            canvas.fill_rect(close_rect, 0xC42B1C);
        }
        canvas.draw_window_close_icon(
            close_rect.x + cap_w / 2,
            close_rect.y + cap_h / 2,
            if close_hovered { 0xFFFFFF } else { t.text_secondary },
        );
        self.hit_regions
            .push((close_rect, HitAction::WindowControl(WindowControlCmd::Close)));

        // 5. IDEA 运行工具区：[运行配置 ▾] [▷] [🐞] [⋮] [🔍] [⚙]（紧邻窗口控制按钮左侧）
        let cfg_fallback = self.tr("当前文件", "Current File");
        let cfg_name = self
            .run_configs
            .get(self.active_run_config)
            .map(|c| c.name.as_str())
            .unwrap_or(cfg_fallback);
        let (ctw, _) = canvas.measure_text(cfg_name, FontStyle::UiRegular);
        let cfg_w = (ctw + 52).clamp(118, 240);
        let right_controls_total = 148 + 148; // 5 个动作图标 + 窗口控制区
        let cfg_x = (rect.w - right_controls_total - cfg_w - 8).max(cur_x + git_w + 18);

        let cfg_box = Rect::new(cfg_x, 5, cfg_w, 28);
        canvas.fill_rounded_rect(
            cfg_box,
            6,
            if self.run_config_popup_open || cfg_box.contains(hx, hy) {
                t.popup_hover_bg
            } else {
                t.panel_bg
            },
        );
        canvas.draw_file_type_icon(cfg_x + 8, 12, self.active_buffer().language);
        canvas.draw_text_styled(
            cfg_x + 26,
            10,
            cfg_name,
            t.text_primary,
            cfg_w - 42,
            FontStyle::UiRegular,
        );
        canvas.draw_chevron_down(cfg_x + cfg_w - 15, 17, t.text_muted);
        self.hit_regions
            .push((cfg_box, HitAction::ToggleRunConfigPopup));

        let mut ix = cfg_x + cfg_w + 8;

        // 空心绿色运行三角 ▷ (Shift+F10)
        let run_btn = Rect::new(ix, 6, 26, 26);
        if run_btn.contains(hx, hy) {
            canvas.fill_rounded_rect(run_btn, 5, t.panel_bg);
        }
        canvas.stroke_play_triangle(ix + 7, 13, 12, t.run_green);
        self.hit_regions.push((run_btn, HitAction::RunActiveConfig));
        ix += 28;

        // 空心绿色 Debug 虫子图标 (Shift+F9)
        let dbg_btn = Rect::new(ix, 6, 26, 26);
        if dbg_btn.contains(hx, hy) {
            canvas.fill_rounded_rect(dbg_btn, 5, t.panel_bg);
        }
        canvas.draw_debug_bug_outline_icon(ix + 6, 11, t.run_green);
        self.hit_regions
            .push((dbg_btn, HitAction::DebugActiveConfig));
        ix += 28;

        // 更多操作 ⋮
        let more_btn = Rect::new(ix, 6, 24, 26);
        if self.more_menu_popup_open || more_btn.contains(hx, hy) {
            canvas.fill_rounded_rect(more_btn, 5, t.panel_bg);
        }
        canvas.draw_more_dots_icon(ix + 7, 12, t.text_muted);
        self.hit_regions
            .push((more_btn, HitAction::ToggleMoreMenuPopup));
        ix += 26;

        // Search Everywhere 放大镜图标 🔍 (双击 Shift)
        let search_btn = Rect::new(ix, 6, 26, 26);
        if search_btn.contains(hx, hy) {
            canvas.fill_rounded_rect(search_btn, 5, t.panel_bg);
        }
        canvas.draw_search_icon(ix + 5, 11, t.text_secondary);
        self.hit_regions
            .push((search_btn, HitAction::ToggleSearchEverywhere));
        ix += 28;

        // 设置齿轮图标 ⚙ (Ctrl+Alt+S)
        let gear_btn = Rect::new(ix, 6, 26, 26);
        if gear_btn.contains(hx, hy) {
            canvas.fill_rounded_rect(gear_btn, 5, t.panel_bg);
        }
        canvas.draw_gear_icon(ix + 5, 12, t.text_secondary);
        self.hit_regions.push((
            gear_btn,
            HitAction::ExecuteIdeAction(IdeAction::OpenSettingsModal),
        ));

        // 垂直分割线分隔 IDE 工具栏与系统窗口控制区
        canvas.fill_rect(
            Rect::new(min_rect.x - 4, 9, 1, 20),
            t.border_color,
        );
    }

    fn render_expanded_menu_strip(&mut self, canvas: &mut SurfaceCanvas) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let lang = self.ui_lang;
        self.menu_category_rects.clear();

        let mut item_widths = Vec::with_capacity(MainMenuCategory::all().len());
        let mut total_w = 16;
        for &cat in MainMenuCategory::all() {
            let label = cat.label_i18n(lang);
            let (tw, _) = canvas.measure_text(label, FontStyle::UiRegular);
            let iw = tw + 20;
            item_widths.push((cat, label, iw));
            total_w += iw + 2;
        }

        let bar = Rect::new(10, 40, total_w, 32);
        self.active_overlay_rects.push(bar);
        canvas.fill_rounded_shadow(bar, 6);
        canvas.stroke_rounded_rect(bar, 6, t.border_color, t.popup_bg);

        let mut cx = bar.x + 8;
        for (cat, label, item_w) in item_widths {
            let item_rect = Rect::new(cx, bar.y + 3, item_w, 26);
            let active = self.active_menu_category == Some(cat);
            let hovered = item_rect.contains(hx, hy);
            if active || hovered {
                canvas.fill_rounded_rect(
                    item_rect,
                    5,
                    if active { t.popup_hover_bg } else { t.panel_header_bg },
                );
            }
            canvas.draw_text_centered(
                item_rect,
                label,
                if active { 0xFFFFFF } else { t.text_primary },
                if active {
                    FontStyle::UiBold
                } else {
                    FontStyle::UiRegular
                },
            );
            self.menu_category_rects.push((cat, item_rect));
            self.hit_regions
                .push((item_rect, HitAction::OpenMenuCategory(cat)));
            cx += item_w + 2;
        }
    }

    fn render_left_stripe(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        canvas.fill_rect(rect, t.tool_stripe_bg);

        let top_tools = [
            ToolWindowId::Project,
            ToolWindowId::Commit,
            ToolWindowId::Structure,
        ];
        for (idx, id) in top_tools.into_iter().enumerate() {
            let y = rect.y + 8 + (idx as i32) * 38;
            let btn = Rect::new(rect.x + 5, y, 32, 30);
            let active = self.left_tool == Some(id);
            let hovered = !active && btn.contains(hx, hy);
            if active {
                canvas.fill_rounded_rect(btn, 6, t.panel_bg);
                canvas.fill_rounded_rect(Rect::new(rect.x + 1, y + 6, 3, 18), 1, t.accent_blue);
            } else if hovered {
                canvas.fill_rounded_rect(btn, 6, t.panel_bg);
            }
            let icon_color = if active || hovered {
                t.text_primary
            } else {
                t.text_muted
            };
            let ix = btn.x + 8;
            let iy = btn.y + 7;
            match id {
                ToolWindowId::Project => canvas.draw_folder_outline_icon(ix, iy, icon_color),
                ToolWindowId::Commit => canvas.draw_commit_icon(ix, iy, icon_color),
                ToolWindowId::Structure => canvas.draw_structure_icon(ix, iy, icon_color),
                _ => {}
            }
            self.hit_regions
                .push((btn, HitAction::ToggleLeftTool(id)));
        }

        // 左下角工具窗矢量图标：终端 / Git Log / Problems / Debug
        let bottom_tools = [
            ToolWindowId::GitLog,
            ToolWindowId::Terminal,
            ToolWindowId::Problems,
            ToolWindowId::Debug,
        ];
        let base_y = rect.y + rect.h - (bottom_tools.len() as i32) * 38 - 6;
        for (idx, id) in bottom_tools.into_iter().enumerate() {
            let y = base_y + (idx as i32) * 38;
            let btn = Rect::new(rect.x + 5, y, 32, 30);
            let active = self.bottom_tool == Some(id);
            let hovered = !active && btn.contains(hx, hy);
            if active {
                canvas.fill_rounded_rect(btn, 6, t.panel_bg);
                canvas.fill_rounded_rect(Rect::new(rect.x + 1, y + 6, 3, 18), 1, t.accent_blue);
            } else if hovered {
                canvas.fill_rounded_rect(btn, 6, t.panel_bg);
            }
            let icon_color = if active || hovered {
                t.text_primary
            } else {
                t.text_muted
            };
            let ix = btn.x + 8;
            let iy = btn.y + 7;
            match id {
                ToolWindowId::GitLog => canvas.draw_git_branch_icon(ix + 2, iy + 1, icon_color),
                ToolWindowId::Terminal => canvas.draw_terminal_icon(ix, iy, icon_color),
                ToolWindowId::Problems => canvas.draw_problems_icon(ix, iy, icon_color),
                ToolWindowId::Debug => canvas.draw_debug_bug_outline_icon(ix + 1, iy, icon_color),
                _ => {}
            }
            self.hit_regions
                .push((btn, HitAction::SelectBottomTool(id)));
        }
    }

    fn render_right_stripe(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        canvas.fill_rect(rect, t.tool_stripe_bg);

        // 1. AI 助手图标
        let ai_btn = Rect::new(rect.x + 5, rect.y + 8, 32, 30);
        let ai_active = self.right_agent_open && self.right_panel_tab == RightPanelTab::Chat;
        let ai_hovered = !ai_active && ai_btn.contains(hx, hy);
        if ai_active {
            canvas.fill_rounded_rect(ai_btn, 6, t.panel_bg);
            canvas.fill_rounded_rect(
                Rect::new(rect.x + rect.w - 4, rect.y + 14, 3, 18),
                1,
                t.accent_blue,
            );
        } else if ai_hovered {
            canvas.fill_rounded_rect(ai_btn, 6, t.panel_bg);
        }
        canvas.draw_ai_sparkle_icon(
            ai_btn.x + 9,
            ai_btn.y + 7,
            if ai_active {
                t.accent_blue
            } else if ai_hovered {
                t.text_primary
            } else {
                t.text_muted
            },
        );
        self.hit_regions.push((
            ai_btn,
            if ai_active {
                HitAction::ToggleRightAgentHub
            } else {
                HitAction::SwitchRightPanelTab(RightPanelTab::Chat)
            },
        ));

        // 2. 更改 / Diff 图标
        let chg_btn = Rect::new(rect.x + 5, rect.y + 46, 32, 30);
        let chg_active = self.right_agent_open && self.right_panel_tab == RightPanelTab::Changes;
        let chg_hovered = !chg_active && chg_btn.contains(hx, hy);
        if chg_active {
            canvas.fill_rounded_rect(chg_btn, 6, t.panel_bg);
            canvas.fill_rounded_rect(
                Rect::new(rect.x + rect.w - 4, rect.y + 52, 3, 18),
                1,
                t.accent_blue,
            );
        } else if chg_hovered {
            canvas.fill_rounded_rect(chg_btn, 6, t.panel_bg);
        }
        canvas.draw_changes_diff_icon(
            chg_btn.x + 8,
            chg_btn.y + 7,
            if chg_active || chg_hovered {
                t.text_primary
            } else {
                t.text_muted
            },
        );
        self.hit_regions.push((
            chg_btn,
            if chg_active {
                HitAction::ToggleRightAgentHub
            } else {
                HitAction::SwitchRightPanelTab(RightPanelTab::Changes)
            },
        ));

        // 3. 主题切换图标
        let theme_btn = Rect::new(rect.x + 5, rect.y + 84, 32, 30);
        let theme_hov = theme_btn.contains(hx, hy);
        if theme_hov {
            canvas.fill_rounded_rect(theme_btn, 6, t.panel_bg);
        }
        canvas.draw_theme_icon(
            theme_btn.x + 8,
            theme_btn.y + 7,
            if theme_hov { t.text_primary } else { t.text_muted },
        );
        self.hit_regions.push((
            theme_btn,
            HitAction::ExecuteIdeAction(IdeAction::SwitchTheme),
        ));

        // 4. 设置与快捷键图标
        let set_btn = Rect::new(rect.x + 5, rect.y + rect.h - 40, 32, 30);
        let set_hov = set_btn.contains(hx, hy);
        if set_hov {
            canvas.fill_rounded_rect(set_btn, 6, t.panel_bg);
        }
        canvas.draw_gear_icon(
            set_btn.x + 9,
            set_btn.y + 8,
            if set_hov { t.text_primary } else { t.text_muted },
        );
        self.hit_regions.push((
            set_btn,
            HitAction::ExecuteIdeAction(IdeAction::OpenSettingsModal),
        ));
    }

    fn render_left_tool_window(
        &mut self,
        canvas: &mut SurfaceCanvas,
        rect: Rect,
        tool: ToolWindowId,
    ) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        canvas.stroke_rounded_rect(rect, 8, t.border_color, t.panel_bg);

        let title = match tool {
            ToolWindowId::Project => self.tr("项目", "Project"),
            ToolWindowId::Commit => self.tr("Git 提交", "Commit"),
            ToolWindowId::Structure => self.tr("结构", "Structure"),
            _ => self.tr("工具", "Tool"),
        };
        let (title_w, _) = canvas.measure_text(title, FontStyle::UiBold);
        canvas.draw_text_styled(
            rect.x + 12,
            rect.y + 8,
            title,
            t.text_primary,
            106,
            FontStyle::UiBold,
        );
        canvas.draw_chevron_down(rect.x + 12 + title_w + 6, rect.y + 14, t.text_muted);

        if tool == ToolWindowId::Project && self.tree_load_alpha > 0.02 {
            canvas.draw_subtle_spinner(
                rect.x + 12 + title_w + 25,
                rect.y + 16,
                4.8,
                self.spinner_phase(),
                t.accent_blue,
                t.border_color,
                self.tree_load_alpha,
            );
        } else if tool == ToolWindowId::Commit && self.git_load_alpha > 0.02 {
            canvas.draw_subtle_spinner(
                rect.x + 12 + title_w + 25,
                rect.y + 16,
                4.8,
                self.spinner_phase(),
                t.accent_blue,
                t.border_color,
                self.git_load_alpha,
            );
        }

        match tool {
            ToolWindowId::Project => {
                // 顶部矢量图标工具栏：[打开文件夹] [新建文件] [新建文件夹] [展开全部] [折叠全部]
                let btn_y = rect.y + 5;
                let title_end = rect.x + 12 + title_w + 18;
                let mut bx = (rect.x + rect.w - 144).max(title_end);

                if bx + 140 <= rect.x + rect.w {
                    let of_btn = Rect::new(bx, btn_y, 24, 22);
                    canvas.fill_rounded_rect(
                        of_btn,
                        4,
                        if of_btn.contains(hx, hy) {
                            t.popup_hover_bg
                        } else {
                            t.window_bg
                        },
                    );
                    canvas.draw_folder_outline_icon(bx + 4, btn_y + 4, t.accent_blue);
                    self.hit_regions.push((of_btn, HitAction::OpenFolderDialog));
                    bx += 28;
                }

                let nf_btn = Rect::new(bx, btn_y, 24, 22);
                canvas.fill_rounded_rect(
                    nf_btn,
                    4,
                    if nf_btn.contains(hx, hy) {
                        t.popup_hover_bg
                    } else {
                        t.window_bg
                    },
                );
                canvas.draw_new_file_icon(bx + 5, btn_y + 4, t.syn_function);
                self.hit_regions.push((nf_btn, HitAction::PromptNewFile));
                bx += 28;

                let nd_btn = Rect::new(bx, btn_y, 24, 22);
                canvas.fill_rounded_rect(
                    nd_btn,
                    4,
                    if nd_btn.contains(hx, hy) {
                        t.popup_hover_bg
                    } else {
                        t.window_bg
                    },
                );
                canvas.draw_new_folder_icon(bx + 4, btn_y + 3, 0xD9A343);
                self.hit_regions.push((nd_btn, HitAction::PromptNewFolder));
                bx += 28;

                let exp_btn = Rect::new(bx, btn_y, 24, 22);
                canvas.fill_rounded_rect(
                    exp_btn,
                    4,
                    if exp_btn.contains(hx, hy) {
                        t.popup_hover_bg
                    } else {
                        t.window_bg
                    },
                );
                canvas.draw_expand_all_icon(bx + 5, btn_y + 4, t.text_secondary);
                self.hit_regions.push((exp_btn, HitAction::ExpandAllDirs));
                bx += 28;

                let col_btn = Rect::new(bx, btn_y, 24, 22);
                canvas.fill_rounded_rect(
                    col_btn,
                    4,
                    if col_btn.contains(hx, hy) {
                        t.popup_hover_bg
                    } else {
                        t.window_bg
                    },
                );
                canvas.draw_collapse_all_icon(bx + 5, btn_y + 4, t.text_secondary);
                self.hit_regions.push((col_btn, HitAction::CollapseAllDirs));

                canvas.fill_rect(
                    Rect::new(rect.x + 1, rect.y + 32, rect.w - 2, 1),
                    t.border_color,
                );

                let list_top = rect.y + 38;
                let list_h = (rect.h - 42).max(22);
                let visible_rows = (list_h / 22).max(1) as usize;
                let total_rows = self.project_tree.visible_entries.len();
                let max_scroll = total_rows.saturating_sub(visible_rows);
                if self.tree_scroll > max_scroll {
                    self.tree_scroll = max_scroll;
                }

                let mut y = list_top;
                let entries: Vec<_> = self
                    .project_tree
                    .visible_entries
                    .iter()
                    .skip(self.tree_scroll)
                    .take(visible_rows)
                    .cloned()
                    .collect();

                for entry in entries {
                    let row_rect = Rect::new(rect.x + 6, y, rect.w - 18, 22);
                    let is_active = (!entry.is_dir && self.active_buffer().path == entry.path)
                        || (self.project_tree.selected_path.as_ref() == Some(&entry.path));
                    let row_hovered = !is_active && row_rect.contains(hx, hy);
                    if is_active {
                        canvas.fill_rounded_rect(row_rect, 5, t.editor_selection_bg);
                    } else if row_hovered {
                        canvas.fill_rounded_rect(row_rect, 5, t.panel_header_bg);
                    }
                    let indent = (entry.depth as i32) * 14;
                    let base_x = rect.x + 10 + indent;
                    let text_x = if entry.is_dir {
                        if entry.expanded {
                            canvas.draw_chevron_down(base_x, y + 9, t.text_secondary);
                        } else {
                            canvas.draw_chevron_right(base_x + 2, y + 7, t.text_secondary);
                        }
                        canvas.draw_folder_icon(base_x + 12, y + 4, 0xD9A343);
                        base_x + 30
                    } else {
                        canvas.draw_file_type_icon(base_x + 10, y + 4, entry.language);
                        base_x + 28
                    };

                    let color = if is_active || row_hovered {
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
                        rect.x + rect.w - text_x - 14,
                        if is_active {
                            FontStyle::UiBold
                        } else {
                            FontStyle::UiRegular
                        },
                    );
                    self.project_entry_rects
                        .push((row_rect, entry.path.clone(), entry.is_dir));
                    if entry.is_dir {
                        self.hit_regions
                            .push((row_rect, HitAction::ToggleDirectory(entry.path)));
                    } else {
                        self.hit_regions
                            .push((row_rect, HitAction::OpenFile(entry.path)));
                    }
                    y += 22;
                }

                let track = Rect::new(rect.x + rect.w - 9, list_top, 6, list_h);
                self.left_scroll_info = ScrollTrackInfo {
                    track,
                    visible: visible_rows,
                    total: total_rows,
                };
                canvas.draw_vertical_scrollbar(
                    track,
                    self.tree_scroll,
                    visible_rows,
                    total_rows,
                    t.border_color,
                );
            }
            ToolWindowId::Commit => {
                // 顶部右侧 IDEA Git 动作图标栏：[⎇ 分支] [↻ 刷新] [↶ 全部回滚] [日志]
                let btn_y = rect.y + 5;
                let title_end = rect.x + 12 + title_w + 18;
                let rb_all_btn = Rect::new(rect.x + rect.w - 32, btn_y, 24, 22);
                if rb_all_btn.contains(hx, hy) {
                    canvas.fill_rounded_rect(rb_all_btn, 4, t.popup_hover_bg);
                } else {
                    canvas.fill_rounded_rect(rb_all_btn, 4, t.window_bg);
                }
                canvas.draw_rollback_icon(rb_all_btn.x + 5, btn_y + 4, t.breakpoint_red);
                self.hit_regions
                    .push((rb_all_btn, HitAction::RollbackAllGitChanges));

                let ref_btn = Rect::new(rb_all_btn.x - 28, btn_y, 24, 22);
                if ref_btn.contains(hx, hy) {
                    canvas.fill_rounded_rect(ref_btn, 4, t.popup_hover_bg);
                } else {
                    canvas.fill_rounded_rect(ref_btn, 4, t.window_bg);
                }
                canvas.draw_refresh_icon(ref_btn.x + 5, btn_y + 4, t.text_secondary);
                self.hit_regions
                    .push((ref_btn, HitAction::RefreshGitStatus));

                let log_btn = Rect::new(ref_btn.x - 52, btn_y, 48, 22);
                if log_btn.x >= title_end {
                    let log_hov = log_btn.contains(hx, hy)
                        || self.bottom_tool == Some(ToolWindowId::GitLog);
                    canvas.stroke_rounded_rect(
                        log_btn,
                        4,
                        if log_hov { t.accent_blue } else { t.border_color },
                        t.window_bg,
                    );
                    canvas.draw_text_centered(
                        log_btn,
                        self.tr("历史库", "Log"),
                        if log_hov { t.accent_blue } else { t.text_secondary },
                        FontStyle::UiSmall,
                    );
                    self.hit_regions
                        .push((log_btn, HitAction::SelectBottomTool(ToolWindowId::GitLog)));
                }

                canvas.fill_rect(
                    Rect::new(rect.x + 1, rect.y + 32, rect.w - 2, 1),
                    t.border_color,
                );
                let mut y = rect.y + 37;

                // 1. 当前分支胶囊与 Ahead/Behind 指示器 + 快捷 Pull / Push / Fetch 按钮
                let branch_name = self.git_engine.current_branch.clone();
                let (ahead, behind) = self.git_engine.ahead_behind;
                let ab_suffix = if ahead > 0 || behind > 0 {
                    format!(" ↑{ahead} ↓{behind}")
                } else {
                    String::new()
                };
                let branch_label = format!("⎇ {branch_name}{ab_suffix} ▾");
                let br_pill = Rect::new(rect.x + 8, y, (rect.w - 16).max(80), 24);
                let br_hov = br_pill.contains(hx, hy);
                canvas.stroke_rounded_rect(
                    br_pill,
                    5,
                    if br_hov || self.git_branch_popup_open {
                        t.accent_blue
                    } else {
                        t.border_color
                    },
                    t.window_bg,
                );
                canvas.draw_text_styled(
                    br_pill.x + 10,
                    y + 3,
                    &branch_label,
                    t.text_primary,
                    br_pill.w - 20,
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((br_pill, HitAction::ToggleGitBranchPopup));
                y += 28;

                let gap = 4;
                let bw = ((rect.w - 16 - gap * 3) / 4).max(42);
                let mut gx = rect.x + 8;

                let fetch_btn = Rect::new(gx, y, bw, 23);
                canvas.stroke_rounded_rect(
                    fetch_btn,
                    5,
                    if fetch_btn.contains(hx, hy) {
                        t.accent_blue
                    } else {
                        t.border_color
                    },
                    t.window_bg,
                );
                canvas.draw_text_centered(
                    fetch_btn,
                    "↻ Fetch",
                    t.text_secondary,
                    FontStyle::UiSmall,
                );
                self.hit_regions.push((fetch_btn, HitAction::GitFetchAll));
                gx += bw + gap;

                let pull_btn = Rect::new(gx, y, bw, 23);
                canvas.stroke_rounded_rect(
                    pull_btn,
                    5,
                    if pull_btn.contains(hx, hy) {
                        t.accent_blue
                    } else {
                        t.border_color
                    },
                    t.window_bg,
                );
                canvas.draw_text_centered(pull_btn, "↓ Pull", t.syn_function, FontStyle::UiSmall);
                self.hit_regions.push((
                    pull_btn,
                    HitAction::ExecuteIdeAction(IdeAction::GitPull),
                ));
                gx += bw + gap;

                let push_btn = Rect::new(gx, y, bw, 23);
                canvas.stroke_rounded_rect(
                    push_btn,
                    5,
                    if push_btn.contains(hx, hy) {
                        t.run_green
                    } else {
                        t.border_color
                    },
                    t.window_bg,
                );
                canvas.draw_text_centered(push_btn, "↑ Push", t.run_green, FontStyle::UiSmall);
                self.hit_regions.push((
                    push_btn,
                    HitAction::ExecuteIdeAction(IdeAction::GitPush),
                ));
                gx += bw + gap;

                let stage_btn = Rect::new(gx, y, (rect.x + rect.w - 8 - gx).max(bw), 23);
                let all_staged = !self.git_engine.changed_files.is_empty()
                    && self.git_engine.changed_files.iter().all(|f| f.staged);
                canvas.stroke_rounded_rect(
                    stage_btn,
                    5,
                    if stage_btn.contains(hx, hy) {
                        t.accent_blue
                    } else {
                        t.border_color
                    },
                    t.window_bg,
                );
                canvas.draw_text_centered(
                    stage_btn,
                    if all_staged {
                        self.tr("- 取消", "- Unstage")
                    } else {
                        self.tr("+ 暂存", "+ Stage")
                    },
                    t.text_primary,
                    FontStyle::UiSmall,
                );
                self.hit_regions.push((
                    stage_btn,
                    if all_staged {
                        HitAction::UnstageAllGitChanges
                    } else {
                        HitAction::StageAllGitChanges
                    },
                ));
                y += 28;

                // 2. IDEA 分组变更树：已暂存 (Staged Changes) + 未暂存更改 (Changes)
                let changed_snapshot = self.git_engine.changed_files.clone();
                let staged_cnt = changed_snapshot.iter().filter(|f| f.staged).count();
                let unstaged_cnt = changed_snapshot.len().saturating_sub(staged_cnt);

                #[derive(Clone)]
                enum CommitTreeRow {
                    SectionHeader {
                        title: String,
                        is_staged_sec: bool,
                    },
                    FileEntry(ide_git::GitChangedFile),
                }

                let mut tree_rows: Vec<CommitTreeRow> = Vec::with_capacity(changed_snapshot.len() + 2);
                if staged_cnt > 0 {
                    tree_rows.push(CommitTreeRow::SectionHeader {
                        title: format!(
                            "▼ {} ({staged_cnt})",
                            self.tr("已暂存更改 Staged", "Staged Changes")
                        ),
                        is_staged_sec: true,
                    });
                    for ch in changed_snapshot.iter().filter(|f| f.staged) {
                        tree_rows.push(CommitTreeRow::FileEntry(ch.clone()));
                    }
                }
                tree_rows.push(CommitTreeRow::SectionHeader {
                    title: format!(
                        "▼ {} ({unstaged_cnt})",
                        self.tr("更改 Changes", "Changes")
                    ),
                    is_staged_sec: false,
                });
                for ch in changed_snapshot.iter().filter(|f| !f.staged) {
                    tree_rows.push(CommitTreeRow::FileEntry(ch.clone()));
                }

                let list_top = y;
                let commit_box_y = rect.y + rect.h - 114;
                let list_h = (commit_box_y - list_top - 6).max(24);
                let row_h = 23;
                let visible_rows = (list_h / row_h).max(1) as usize;
                let total_rows = tree_rows.len();
                let max_scroll = total_rows.saturating_sub(visible_rows);
                if self.git_changes_scroll > max_scroll {
                    self.git_changes_scroll = max_scroll;
                }

                if !self.git_engine.has_loaded_snapshot {
                    let phase = self.spinner_phase();
                    canvas.draw_subtle_spinner(
                        rect.x + 18,
                        y + 14,
                        5.2,
                        phase,
                        t.accent_blue,
                        t.border_color,
                        self.git_load_alpha.max(0.75),
                    );
                    canvas.draw_text_styled(
                        rect.x + 30,
                        y + 6,
                        self.tr(
                            "正在加载 Git 变更文件列表...",
                            "Loading Git changed files...",
                        ),
                        t.text_secondary,
                        rect.w - 40,
                        FontStyle::UiSmall,
                    );
                    // 极克制的轻量占位骨架条，柔和呼吸不闪烁
                    let shimmer = 0.35 + 0.25 * (phase * 0.65).sin().abs();
                    let sk_bg = blend_rgb(t.panel_bg, t.panel_header_bg, shimmer);
                    for row_i in 0..3 {
                        let sy = y + 30 + row_i * 22;
                        if sy + 14 <= commit_box_y - 4 {
                            let w = (rect.w - 36 - (row_i * 22)).max(60);
                            canvas.fill_rounded_rect(Rect::new(rect.x + 14, sy, w, 12), 4, sk_bg);
                        }
                    }
                } else if changed_snapshot.is_empty() {
                    canvas.draw_text_styled(
                        rect.x + 12,
                        y + 6,
                        self.tr(
                            "✓ 工作区干净 (修改文件后自动显示变更)",
                            "✓ Working tree clean (no local changes)",
                        ),
                        t.text_muted,
                        rect.w - 24,
                        FontStyle::UiSmall,
                    );
                } else {
                    for row_item in tree_rows
                        .into_iter()
                        .skip(self.git_changes_scroll)
                        .take(visible_rows)
                    {
                        match row_item {
                            CommitTreeRow::SectionHeader {
                                title,
                                is_staged_sec,
                            } => {
                                let hdr_rect = Rect::new(rect.x + 6, y, rect.w - 18, row_h - 1);
                                canvas.fill_rounded_rect(hdr_rect, 4, t.panel_header_bg);
                                canvas.draw_text_styled(
                                    rect.x + 10,
                                    y + 2,
                                    &title,
                                    t.text_secondary,
                                    rect.w - 76,
                                    FontStyle::UiBold,
                                );
                                let sec_btn = Rect::new(rect.x + rect.w - 64, y + 2, 50, row_h - 5);
                                let sec_hov = sec_btn.contains(hx, hy);
                                canvas.stroke_rounded_rect(
                                    sec_btn,
                                    3,
                                    if sec_hov { t.accent_blue } else { t.border_color },
                                    t.window_bg,
                                );
                                canvas.draw_text_centered(
                                    sec_btn,
                                    if is_staged_sec {
                                        self.tr("- 全退", "- All")
                                    } else {
                                        self.tr("+ 全选", "+ All")
                                    },
                                    if sec_hov { t.accent_blue } else { t.text_muted },
                                    FontStyle::UiSmall,
                                );
                                self.hit_regions.push((
                                    sec_btn,
                                    if is_staged_sec {
                                        HitAction::UnstageAllGitChanges
                                    } else {
                                        HitAction::StageAllGitChanges
                                    },
                                ));
                            }
                            CommitTreeRow::FileEntry(ch) => {
                                let full_row = Rect::new(rect.x + 6, y, rect.w - 18, row_h - 1);
                                let is_diff_selected = self
                                    .git_working_diff_target
                                    .as_ref()
                                    .is_some_and(|p| p == &ch.path);
                                let row_hov = full_row.contains(hx, hy);
                                if is_diff_selected {
                                    canvas.fill_rounded_rect(full_row, 4, t.editor_selection_bg);
                                } else if row_hov {
                                    canvas.fill_rounded_rect(full_row, 4, t.panel_header_bg);
                                }
                                self.git_change_row_rects.push((
                                    full_row,
                                    ch.path.clone(),
                                    ch.staged,
                                ));

                                // 1) IDEA 复选框 [✓] / [ ]
                                let chk_rect = Rect::new(rect.x + 10, y + 3, 15, 15);
                                if ch.staged {
                                    canvas.fill_rounded_rect(chk_rect, 3, t.accent_blue);
                                    canvas.draw_check_icon(chk_rect.x + 2, chk_rect.y + 3, 0xFFFFFF);
                                } else {
                                    canvas.stroke_rounded_rect(
                                        chk_rect,
                                        3,
                                        t.border_color,
                                        t.window_bg,
                                    );
                                }
                                let chk_hit = Rect::new(rect.x + 8, y, 22, row_h - 1);
                                self.hit_regions
                                    .push((chk_hit, HitAction::ToggleStageFile(ch.path.clone())));

                                // 2) 文件类型图标 + 文件名 + 相对目录
                                let lang = ide_core::LanguageId::from_path(&ch.path);
                                canvas.draw_file_type_icon(rect.x + 30, y + 4, lang);

                                let fname = ch
                                    .path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("file");
                                let parent_dir = ch
                                    .path
                                    .strip_prefix(&self.workspace_root)
                                    .unwrap_or(&ch.path)
                                    .parent()
                                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                                    .unwrap_or_default();
                                let status_color = match ch.status {
                                    ide_git::GitFileStatusKind::Added
                                    | ide_git::GitFileStatusKind::Untracked => t.run_green,
                                    ide_git::GitFileStatusKind::Deleted => t.breakpoint_red,
                                    ide_git::GitFileStatusKind::Modified => t.syn_function,
                                };
                                let right_actions_w = 66;
                                let text_avail_w =
                                    (rect.w - 48 - right_actions_w - 10).max(40);
                                let next_x = canvas.draw_text_styled(
                                    rect.x + 48,
                                    y + 2,
                                    fname,
                                    status_color,
                                    text_avail_w,
                                    FontStyle::UiSmall,
                                );
                                if !parent_dir.is_empty()
                                    && next_x + 24 < rect.x + rect.w - right_actions_w
                                {
                                    canvas.draw_text_styled(
                                        next_x + 6,
                                        y + 2,
                                        &parent_dir,
                                        t.text_muted,
                                        (rect.x + rect.w - right_actions_w - next_x - 8).max(16),
                                        FontStyle::UiSmall,
                                    );
                                }
                                let file_click_rect = Rect::new(
                                    rect.x + 30,
                                    y,
                                    (rect.w - 36 - right_actions_w).max(40),
                                    row_h - 1,
                                );
                                self.hit_regions.push((
                                    file_click_rect,
                                    HitAction::InspectGitWorkingFileDiff(ch.path.clone()),
                                ));

                                // 3) 状态徽章 (M / A / D / U) + Diff 按钮 + 回滚按钮
                                let badge_x = rect.x + rect.w - right_actions_w - 2;
                                canvas.draw_text_styled(
                                    badge_x,
                                    y + 2,
                                    ch.status.badge_str(),
                                    status_color,
                                    14,
                                    FontStyle::CodeMonoBold,
                                );

                                let diff_btn = Rect::new(badge_x + 15, y + 2, 24, row_h - 5);
                                canvas.stroke_rounded_rect(
                                    diff_btn,
                                    3,
                                    if diff_btn.contains(hx, hy) {
                                        t.accent_blue
                                    } else {
                                        t.border_color
                                    },
                                    t.window_bg,
                                );
                                canvas.draw_changes_diff_icon(
                                    diff_btn.x + 4,
                                    diff_btn.y + 2,
                                    t.accent_blue,
                                );
                                self.hit_regions.push((
                                    diff_btn,
                                    HitAction::InspectGitWorkingFileDiff(ch.path.clone()),
                                ));

                                let rb_btn = Rect::new(badge_x + 41, y + 2, 22, row_h - 5);
                                canvas.stroke_rounded_rect(
                                    rb_btn,
                                    3,
                                    if rb_btn.contains(hx, hy) {
                                        t.breakpoint_red
                                    } else {
                                        t.border_color
                                    },
                                    t.window_bg,
                                );
                                canvas.draw_rollback_icon(
                                    rb_btn.x + 4,
                                    rb_btn.y + 2,
                                    t.breakpoint_red,
                                );
                                self.hit_regions.push((
                                    rb_btn,
                                    HitAction::RollbackGitFile(ch.path.clone()),
                                ));
                            }
                        }
                        y += row_h;
                    }
                }

                let track = Rect::new(rect.x + rect.w - 9, list_top, 6, list_h);
                self.left_scroll_info = ScrollTrackInfo {
                    track,
                    visible: visible_rows,
                    total: total_rows,
                };
                canvas.draw_vertical_scrollbar(
                    track,
                    self.git_changes_scroll,
                    visible_rows,
                    total_rows,
                    t.border_color,
                );

                // 3. 底部 IDEA Commit 选项栏：[✓ 修正提交 Amend]  +  [✦ AI 生成提交信息]
                let opt_y = commit_box_y;
                let amend_chk = Rect::new(rect.x + 10, opt_y + 4, 15, 15);
                if self.git_amend_commit {
                    canvas.fill_rounded_rect(amend_chk, 3, t.accent_blue);
                    canvas.draw_check_icon(amend_chk.x + 2, amend_chk.y + 3, 0xFFFFFF);
                } else {
                    canvas.stroke_rounded_rect(amend_chk, 3, t.border_color, t.window_bg);
                }
                let amend_hit = Rect::new(rect.x + 8, opt_y + 1, 96, 22);
                canvas.draw_text_styled(
                    rect.x + 29,
                    opt_y + 3,
                    self.tr("修正 Amend", "Amend"),
                    if self.git_amend_commit {
                        t.accent_blue
                    } else {
                        t.text_secondary
                    },
                    76,
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((amend_hit, HitAction::ToggleGitAmend));

                let ai_msg_w = 88;
                let ai_msg_btn = Rect::new(rect.x + rect.w - ai_msg_w - 8, opt_y + 1, ai_msg_w, 22);
                canvas.stroke_rounded_rect(ai_msg_btn, 4, t.accent_blue, t.window_bg);
                canvas.draw_ai_sparkle_icon(ai_msg_btn.x + 5, ai_msg_btn.y + 3, t.accent_blue);
                canvas.draw_text_styled(
                    ai_msg_btn.x + 22,
                    ai_msg_btn.y + 2,
                    self.tr("AI 生成", "AI Msg"),
                    t.accent_blue,
                    ai_msg_w - 26,
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((ai_msg_btn, HitAction::GenerateAiCommitMessage));

                // 4. Commit Message 输入框 + 双按钮：[提交 Commit]  [提交并推送 Commit & Push…]
                let msg_box = Rect::new(rect.x + 8, commit_box_y + 26, rect.w - 16, 46);
                self.git_commit_input_box_rect = msg_box;
                if self.focus != FocusTarget::GitCommitInput
                    && self.git_engine.commit_message_draft != self.git_commit_input.as_str()
                {
                    let draft = self.git_engine.commit_message_draft.clone();
                    self.git_commit_input.set_text(&draft);
                }
                let is_git_focused = self.focus == FocusTarget::GitCommitInput;
                canvas.stroke_rounded_rect(
                    msg_box,
                    5,
                    if is_git_focused {
                        t.accent_blue
                    } else {
                        t.border_color
                    },
                    t.window_bg,
                );
                let commit_ph = self.tr(
                    "输入 Git 提交说明 (Enter 提交)...",
                    "Commit message (Enter to commit)...",
                );
                let ime = self.git_commit_input.render_singleline(
                    canvas,
                    msg_box,
                    8,
                    msg_box.y + 13,
                    commit_ph,
                    is_git_focused,
                    self.caret_visible,
                    t.text_primary,
                    t.text_muted,
                    t.editor_selection_bg,
                    t.accent_blue,
                    FontStyle::UiSmall,
                );
                if is_git_focused {
                    self.ime_spot = ime;
                }
                self.hit_regions
                    .push((msg_box, HitAction::FocusGitCommitMessageInput));

                let btn_row_y = commit_box_y + 78;
                let half_w = ((rect.w - 22) / 2).max(60);
                let commit_btn = Rect::new(rect.x + 8, btn_row_y, half_w - 6, 28);
                canvas.fill_rounded_rect(commit_btn, 6, t.accent_blue);
                canvas.draw_text_centered(
                    commit_btn,
                    if self.git_amend_commit {
                        self.tr("修正提交", "Amend")
                    } else {
                        self.tr("提交 Commit", "Commit")
                    },
                    0xFFFFFF,
                    FontStyle::UiBold,
                );
                self.hit_regions
                    .push((commit_btn, HitAction::CommitAllGitChanges));

                let cp_x = commit_btn.x + commit_btn.w + 6;
                let cp_btn = Rect::new(cp_x, btn_row_y, (rect.x + rect.w - 8 - cp_x).max(64), 28);
                let cp_hov = cp_btn.contains(hx, hy);
                canvas.stroke_rounded_rect(
                    cp_btn,
                    6,
                    t.accent_blue,
                    if cp_hov { t.popup_hover_bg } else { t.window_bg },
                );
                canvas.draw_text_centered(
                    cp_btn,
                    self.tr("提交并推送…", "Commit & Push…"),
                    if cp_hov { 0xFFFFFF } else { t.accent_blue },
                    FontStyle::UiBold,
                );
                self.hit_regions
                    .push((cp_btn, HitAction::CommitAndPushGitChanges));
            }
            ToolWindowId::Structure => {
                canvas.fill_rect(
                    Rect::new(rect.x + 1, rect.y + 32, rect.w - 2, 1),
                    t.border_color,
                );
                let list_top = rect.y + 38;
                let list_h = (rect.h - 42).max(22);
                let visible_rows = (list_h / 22).max(1) as usize;
                self.ensure_structure_symbols_cached();
                let total_rows = self
                    .cached_structure
                    .as_ref()
                    .map(|(_, _, s)| s.len())
                    .unwrap_or(0);
                let mut y = list_top;
                if let Some((_, _, symbols)) = self.cached_structure.as_ref() {
                    for sym in symbols.iter().skip(self.tree_scroll).take(visible_rows) {
                        let row = Rect::new(rect.x + 6, y, rect.w - 18, 22);
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
                            rect.w - 26,
                            FontStyle::CodeMono,
                        );
                        self.hit_regions
                            .push((row, HitAction::JumpToLine(sym.line)));
                        y += 22;
                    }
                }
                let track = Rect::new(rect.x + rect.w - 9, list_top, 6, list_h);
                self.left_scroll_info = ScrollTrackInfo {
                    track,
                    visible: visible_rows,
                    total: total_rows,
                };
                canvas.draw_vertical_scrollbar(
                    track,
                    self.tree_scroll,
                    visible_rows,
                    total_rows,
                    t.border_color,
                );
            }
            _ => {}
        }
    }

    fn render_center_editor(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        canvas.stroke_rounded_rect(rect, 8, t.border_color, t.editor_bg);

        // 1. 顶部多文件 Editor Tabs 栏（带文件图标、关闭按钮 `×` 与同名文件自动区分）
        let tab_bar = Rect::new(rect.x + 1, rect.y + 1, rect.w - 2, 34);
        canvas.fill_rounded_rect(tab_bar, 7, t.tab_inactive_bg);
        canvas.fill_rect(
            Rect::new(rect.x + 1, rect.y + 34, rect.w - 2, 1),
            t.border_color,
        );

        let mut tab_x = rect.x + 4;
        let open_snapshot: Vec<(PathBuf, bool, ide_core::LanguageId)> = self
            .open_buffers
            .iter()
            .map(|b| (b.path.clone(), b.dirty, b.language))
            .collect();

        for (idx, (path, dirty, lang)) in open_snapshot.iter().enumerate() {
            let fname = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("untitled");
            let duplicate_count = open_snapshot
                .iter()
                .filter(|(p, _, _)| p.file_name() == path.file_name())
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
            let tab_w = (tw + 58).clamp(118, 250);
            if tab_x + tab_w > rect.x + rect.w - 200 {
                break;
            }
            let tab_rect = Rect::new(tab_x, rect.y + 3, tab_w, 30);
            self.tab_rects.push((tab_rect, idx));
            let tab_hovered = !is_active && tab_rect.contains(hx, hy);

            if is_active {
                canvas.fill_rounded_rect(tab_rect, 6, t.tab_active_bg);
                canvas.fill_rounded_rect(
                    Rect::new(tab_x + 10, rect.y + 31, tab_w - 20, 3),
                    1,
                    t.tab_underline_accent,
                );
            } else if tab_hovered {
                canvas.fill_rounded_rect(tab_rect, 6, t.panel_header_bg);
            }
            canvas.draw_file_type_icon(tab_x + 10, rect.y + 11, *lang);
            canvas.draw_text_styled(
                tab_x + 28,
                rect.y + 9,
                &label,
                if is_active || tab_hovered {
                    t.text_primary
                } else {
                    t.text_secondary
                },
                tab_w - 46,
                font_style,
            );
            self.hit_regions
                .push((tab_rect, HitAction::SelectTab(idx)));

            if open_snapshot.len() > 1 {
                let close_rect = Rect::new(tab_x + tab_w - 22, rect.y + 9, 16, 16);
                let close_hov = close_rect.contains(hx, hy);
                if close_hov {
                    canvas.fill_rounded_rect(close_rect, 4, t.popup_hover_bg);
                }
                canvas.draw_text_centered(
                    close_rect,
                    "×",
                    if close_hov { 0xFFFFFF } else { t.text_muted },
                    FontStyle::UiRegular,
                );
                self.hit_regions
                    .push((close_rect, HitAction::CloseTab(idx)));
            }
            tab_x += tab_w + 2;
        }

        // 右上角 Cursor 风格 `✦ Ctrl+K` 与 `Alt+L` 图标按钮
        let send_btn_w = 96;
        let ctrl_k_btn_w = 100;
        let ctrl_k_btn = Rect::new(
            rect.x + rect.w - send_btn_w - ctrl_k_btn_w - 14,
            rect.y + 5,
            ctrl_k_btn_w,
            24,
        );
        let ctrl_k_hov = ctrl_k_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            ctrl_k_btn,
            5,
            t.accent_blue,
            if ctrl_k_hov { t.popup_hover_bg } else { t.panel_bg },
        );
        canvas.draw_ai_sparkle_icon(ctrl_k_btn.x + 7, ctrl_k_btn.y + 4, t.accent_blue);
        canvas.draw_text_styled(
            ctrl_k_btn.x + 25,
            ctrl_k_btn.y + 4,
            self.tr("Ctrl+K 改写", "Ctrl+K Edit"),
            if ctrl_k_hov { 0xFFFFFF } else { t.accent_blue },
            ctrl_k_btn_w - 30,
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((ctrl_k_btn, HitAction::TriggerCtrlKInlineEdit));

        let send_btn = Rect::new(
            rect.x + rect.w - send_btn_w - 8,
            rect.y + 5,
            send_btn_w,
            24,
        );
        let send_hov = send_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            send_btn,
            5,
            if send_hov { t.accent_blue } else { t.border_color },
            if send_hov { t.popup_hover_bg } else { t.panel_bg },
        );
        canvas.draw_text_centered(
            send_btn,
            self.tr("Alt+L 问 AI", "Alt+L Ask AI"),
            if send_hov { 0xFFFFFF } else { t.text_primary },
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((send_btn, HitAction::SendSelectionToAgent));

        // 2. 面包屑栏
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
            &active_path_str,
            t.text_muted,
            rect.w - 28,
            FontStyle::UiSmall,
        );

        // 3. Cursor `Ctrl+K` 行内编辑悬浮审批窗（支持直接点击输入指令 + Tab 接受 + Esc 拒绝）
        let mut code_top_y = rect.y + 60;
        if let Some(inline_session) = self.agent_hub.inline_edit.clone() {
            if inline_session.active {
                let banner_h = if inline_session.loading || inline_session.error.is_some() {
                    72
                } else {
                    130
                };
                let banner = Rect::new(rect.x + 66, code_top_y + 4, rect.w - 80, banner_h);
                canvas.stroke_rounded_rect(banner, 6, t.accent_blue, t.panel_bg);

                let accept_w = 108;
                let reject_w = 102;
                let prompt_box = Rect::new(
                    banner.x + 8,
                    banner.y + 5,
                    (banner.w - accept_w - reject_w - 30).max(120),
                    24,
                );
                let is_ctrl_k_focused = self.focus == FocusTarget::CtrlKPrompt;
                self.ctrl_k_input_box_rect = prompt_box;
                canvas.stroke_rounded_rect(
                    prompt_box,
                    5,
                    if is_ctrl_k_focused {
                        t.accent_blue
                    } else {
                        t.border_color
                    },
                    t.editor_bg,
                );
                canvas.draw_ai_sparkle_icon(prompt_box.x + 6, prompt_box.y + 4, t.accent_blue);
                let text_box = Rect::new(
                    prompt_box.x + 22,
                    prompt_box.y,
                    (prompt_box.w - 22).max(40),
                    prompt_box.h,
                );
                let ctrl_k_ph = self.tr(
                    "输入 Ctrl+K 改写需求，Enter 提交 AI...",
                    "Describe Ctrl+K edit, Enter to generate...",
                );
                let ime = self.ctrl_k_input.render_singleline(
                    canvas,
                    text_box,
                    6,
                    prompt_box.y + 4,
                    ctrl_k_ph,
                    is_ctrl_k_focused,
                    self.caret_visible,
                    t.text_primary,
                    t.text_muted,
                    t.editor_selection_bg,
                    t.accent_blue,
                    FontStyle::UiSmall,
                );
                if is_ctrl_k_focused {
                    self.ime_spot = ime;
                }
                self.hit_regions
                    .push((prompt_box, HitAction::FocusCtrlKPrompt));

                let accept_btn = Rect::new(
                    banner.x + banner.w - accept_w - reject_w - 16,
                    banner.y + 5,
                    accept_w,
                    24,
                );
                let can_accept = !inline_session.loading && inline_session.error.is_none();
                canvas.stroke_rounded_rect(
                    accept_btn,
                    5,
                    if can_accept { t.run_green } else { t.border_color },
                    if can_accept { t.diff_added_bg } else { t.window_bg },
                );
                canvas.draw_text_centered(
                    accept_btn,
                    if inline_session.loading {
                        self.tr("生成中...", "Generating...")
                    } else {
                        self.tr("✓ 接受 (Tab)", "✓ Accept (Tab)")
                    },
                    if can_accept { t.diff_added_fg } else { t.text_muted },
                    FontStyle::UiBold,
                );
                if can_accept {
                    self.hit_regions
                        .push((accept_btn, HitAction::AcceptInlineDiff));
                }

                let reject_btn = Rect::new(
                    banner.x + banner.w - reject_w - 8,
                    banner.y + 5,
                    reject_w,
                    24,
                );
                canvas.stroke_rounded_rect(reject_btn, 5, t.breakpoint_red, t.diff_removed_bg);
                canvas.draw_text_centered(
                    reject_btn,
                    self.tr("✕ 拒绝 (Esc)", "✕ Reject (Esc)"),
                    t.diff_removed_fg,
                    FontStyle::UiBold,
                );
                self.hit_regions
                    .push((reject_btn, HitAction::RejectInlineDiff));

                let mut diff_y = banner.y + 35;
                if inline_session.loading {
                    let row = Rect::new(banner.x + 8, diff_y, banner.w - 16, 26);
                    canvas.fill_rounded_rect(row, 4, t.editor_current_line_bg);
                    canvas.draw_text_styled(
                        banner.x + 14,
                        diff_y + 5,
                        self.tr(
                            "正在流式请求 AI 模型生成行内 Diff（按 Esc 可随时取消）...",
                            "Streaming inline AI diff (press Esc to cancel)...",
                        ),
                        t.accent_blue,
                        banner.w - 28,
                        FontStyle::UiSmall,
                    );
                } else if let Some(err) = &inline_session.error {
                    let row = Rect::new(banner.x + 8, diff_y, banner.w - 16, 26);
                    canvas.fill_rounded_rect(row, 4, t.diff_removed_bg);
                    canvas.draw_text_styled(
                        banner.x + 14,
                        diff_y + 5,
                        err,
                        t.diff_removed_fg,
                        banner.w - 28,
                        FontStyle::UiSmall,
                    );
                } else {
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
                }

                code_top_y += banner_h + 10;
            }
        }

        // 4. 代码编辑器 Gutter (动态适配 4/5/6+ 位数行号无截断 + Git 行级修改标记 + 断点 + 运行入口)
        let (buf_path, buf_lang, buf_rev, total_lines, cursor_line, cursor_col, sel_range) = {
            let buf = self.active_buffer();
            (
                buf.path.clone(),
                buf.language,
                buf.revision(),
                buf.line_count(),
                buf.cursor_line,
                buf.cursor_col,
                buf.selection_range(),
            )
        };
        self.ensure_runnables_cached(&buf_path, buf_rev, buf_lang);

        let char_w = canvas.measure_text("8", FontStyle::CodeMono).0.max(8);
        self.editor_char_w = char_w;
        let line_digits = total_lines.max(1).ilog10() as usize + 1;
        let pad_digits = line_digits.max(4);
        let sample_lineno = "8".repeat(pad_digits);
        let (sample_w, _) = canvas.measure_text(&sample_lineno, FontStyle::CodeMono);
        let lineno_col_w = (sample_w + 10).max(char_w * (pad_digits as i32) + 8).max(46);
        let gutter_w = (8 + lineno_col_w + 26).max(76);

        let code_area_h = (rect.y + rect.h - code_top_y - 4).max(24);
        let code_body_rect = Rect::new(
            rect.x + gutter_w + 1,
            code_top_y,
            rect.w - gutter_w - 12,
            code_area_h,
        );
        self.editor_code_rect = code_body_rect;
        self.hit_regions
            .push((code_body_rect, HitAction::FocusEditorBody));

        canvas.fill_rect(
            Rect::new(rect.x + 1, code_top_y, gutter_w, code_area_h),
            t.editor_gutter_bg,
        );
        canvas.fill_rect(
            Rect::new(rect.x + gutter_w, code_top_y, 1, code_area_h),
            t.border_color,
        );

        let line_h = 22;
        let max_visible_lines = (code_area_h / line_h).max(1) as usize;
        self.editor_visible_lines = max_visible_lines;
        let max_scroll = total_lines.saturating_sub(max_visible_lines);
        if self.scroll_line > max_scroll {
            self.scroll_line = max_scroll;
        }

        let visible_lines: Vec<(usize, String)> = {
            let buf = self.active_buffer();
            (0..max_visible_lines)
                .map(|vis_idx| self.scroll_line + vis_idx)
                .take_while(|&line_idx| line_idx < total_lines)
                .map(|line_idx| (line_idx, buf.line_text(line_idx)))
                .collect()
        };
        self.ensure_highlight_lines_cached(&buf_path, buf_rev, buf_lang, &visible_lines);

        let code_start_x = rect.x + gutter_w + 14;
        self.editor_code_start_x = code_start_x;
        let right_limit = rect.x + rect.w - 14;
        let code_clip_rect = Rect::new(
            code_body_rect.x,
            code_body_rect.y,
            (right_limit - code_body_rect.x).max(10),
            code_body_rect.h,
        );

        let file_working_diff = self.git_engine.working_diffs.get(&buf_path);

        for (vis_idx, (line_idx, line_str)) in visible_lines.iter().enumerate() {
            let line_idx = *line_idx;
            let row_y = code_top_y + (vis_idx as i32) * line_h;

            if line_idx == cursor_line {
                canvas.fill_rect(
                    Rect::new(
                        rect.x + gutter_w + 1,
                        row_y,
                        rect.w - gutter_w - 12,
                        line_h,
                    ),
                    t.editor_current_line_bg,
                );
            }

            let gutter_row = Rect::new(rect.x + 1, row_y, gutter_w, line_h);
            self.hit_regions
                .push((gutter_row, HitAction::ToggleBreakpointAtLine(line_idx)));

            let lineno_str = format!("{:>width$}", line_idx + 1, width = pad_digits);
            canvas.draw_text_styled(
                rect.x + 8,
                row_y + 2,
                &lineno_str,
                if line_idx == cursor_line {
                    t.text_primary
                } else {
                    t.text_muted
                },
                lineno_col_w,
                FontStyle::CodeMono,
            );

            let bp_cx = rect.x + 8 + lineno_col_w + 5;
            if self.dap_engine.has_breakpoint(&buf_path, line_idx) {
                canvas.fill_circle(bp_cx, row_y + 11, 5, t.breakpoint_red);
            }

            let is_runnable = self
                .cached_runnables
                .as_ref()
                .map(|(_, _, entries)| entries.iter().any(|r| r.line == line_idx))
                .unwrap_or(false);
            if is_runnable {
                let play_x = rect.x + 8 + lineno_col_w + 8;
                let play_rect = Rect::new(play_x, row_y + 4, 14, 14);
                canvas.fill_play_triangle(play_x + 2, row_y + 6, 10, t.run_green);
                self.hit_regions
                    .push((play_rect, HitAction::RunGutterEntry(line_idx)));
            }

            // IDEA 风格 Gutter 右边缘 Git 实时修改指示色条
            if let Some(dlines) = file_working_diff {
                let line_1based = line_idx + 1;
                if dlines
                    .iter()
                    .any(|dl| dl.kind == DiffLineKind::Added && dl.new_lineno == Some(line_1based))
                {
                    canvas.fill_rect(
                        Rect::new(rect.x + gutter_w - 3, row_y + 1, 3, line_h - 2),
                        t.run_green,
                    );
                }
            }

            let code_row = Rect::new(
                rect.x + gutter_w + 1,
                row_y,
                rect.w - gutter_w - 12,
                line_h,
            );
            self.hit_regions.push((code_row, HitAction::ClickEditorLine));

            // 字符级选区高亮渲染
            if let Some(((sl, sc), (el, ec))) = sel_range {
                if line_idx >= sl && line_idx <= el {
                    let line_chars = line_str.chars().count();
                    let col_start = if line_idx == sl { sc.min(line_chars) } else { 0 };
                    let col_end = if line_idx == el { ec.min(line_chars) } else { line_chars };
                    let p0: String = line_str.chars().take(col_start).collect();
                    let p1: String = line_str.chars().take(col_end).collect();
                    let (w0, _) = canvas.measure_text(&p0, FontStyle::CodeMono);
                    let (w1, _) = canvas.measure_text(&p1, FontStyle::CodeMono);
                    let extra_w = if line_idx < el { 8 } else { 0 };
                    let raw_x0 = code_start_x - self.editor_scroll_x + w0;
                    let raw_x1 = code_start_x - self.editor_scroll_x + w1 + extra_w;
                    let cx0 = raw_x0.clamp(code_body_rect.x, right_limit);
                    let cx1 = raw_x1.clamp(code_body_rect.x, right_limit);
                    if cx1 > cx0 {
                        canvas.fill_rect(
                            Rect::new(cx0, row_y + 1, cx1 - cx0, line_h - 2),
                            t.editor_selection_bg,
                        );
                    }
                }
            }

            if let Some(spans) = self.cached_highlight_lines.2.get(&line_idx) {
                let mut code_x = code_start_x - self.editor_scroll_x;
                for span in spans {
                    if code_x >= right_limit {
                        break;
                    }
                    let font_style = if span.kind == TokenKind::Keyword {
                        FontStyle::CodeMonoBold
                    } else {
                        FontStyle::CodeMono
                    };
                    code_x = canvas.draw_text_clipped(
                        code_x,
                        row_y + 2,
                        &span.text,
                        span.kind.color(&t),
                        code_clip_rect,
                        font_style,
                    );
                }
            }

            // 绘制编辑器光标 Caret 并更新 IME 候选框锚点
            if line_idx == cursor_line && self.focus == FocusTarget::Editor {
                let prefix: String = line_str.chars().take(cursor_col).collect();
                let (px_off, _) = canvas.measure_text(&prefix, FontStyle::CodeMono);
                let caret_x = code_start_x - self.editor_scroll_x + px_off;
                if self.caret_visible && caret_x >= code_body_rect.x && caret_x < right_limit - 1 {
                    canvas.fill_rect(Rect::new(caret_x, row_y + 3, 2, 17), t.accent_blue);
                }
                self.ime_spot = (caret_x.clamp(code_body_rect.x, right_limit - 2), row_y + line_h);
            }
        }

        // 编辑器垂直滚动条
        let track = Rect::new(rect.x + rect.w - 10, code_top_y, 7, code_area_h);
        self.editor_scroll_info = ScrollTrackInfo {
            track,
            visible: max_visible_lines,
            total: total_lines,
        };
        canvas.draw_vertical_scrollbar(
            track,
            self.scroll_line,
            max_visible_lines,
            total_lines,
            t.border_color,
        );
    }

    fn render_bottom_tool_window(
        &mut self,
        canvas: &mut SurfaceCanvas,
        rect: Rect,
        active_tool: ToolWindowId,
    ) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        canvas.stroke_rounded_rect(rect, 8, t.border_color, t.panel_bg);

        // 顶部标题栏：1:1 对齐截图终端头部 -> `终端`  `本地 ×`  `+`  `▾`   右侧切换其他底部面板
        let mut tx = rect.x + 12;
        if active_tool == ToolWindowId::Terminal {
            let term_label = self.tr("终端", "Terminal");
            let (term_lw, _) = canvas.measure_text(term_label, FontStyle::UiBold);
            canvas.draw_text_styled(
                tx,
                rect.y + 7,
                term_label,
                t.text_primary,
                term_lw + 4,
                FontStyle::UiBold,
            );
            tx += term_lw + 12;

            let tabs_meta: Vec<(usize, String)> = self
                .terminal
                .tabs
                .iter()
                .enumerate()
                .map(|(i, tab)| (i, tab.title.clone()))
                .collect();
            let active_tab_idx = self.terminal.active_tab;

            for (i, title) in tabs_meta {
                let is_sel = i == active_tab_idx;
                let (tw, _) = canvas.measure_text(&title, FontStyle::UiSmall);
                let pill_w = (tw + 34).clamp(62, 140);
                let pill = Rect::new(tx, rect.y + 4, pill_w, 24);
                let pill_hov = !is_sel && pill.contains(hx, hy);
                if is_sel {
                    canvas.fill_rounded_rect(pill, 5, t.editor_bg);
                    canvas.fill_rounded_rect(
                        Rect::new(tx + 8, rect.y + 27, pill_w - 16, 2),
                        1,
                        t.accent_blue,
                    );
                } else if pill_hov {
                    canvas.fill_rounded_rect(pill, 5, t.panel_header_bg);
                }
                canvas.draw_text_styled(
                    tx + 9,
                    rect.y + 8,
                    &title,
                    if is_sel || pill_hov {
                        t.text_primary
                    } else {
                        t.text_secondary
                    },
                    pill_w - 28,
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((pill, HitAction::SelectTerminalTab(i)));

                let close_r = Rect::new(tx + pill_w - 18, rect.y + 8, 14, 14);
                let close_hov = close_r.contains(hx, hy);
                if close_hov {
                    canvas.fill_rounded_rect(close_r, 3, t.popup_hover_bg);
                }
                canvas.draw_text_centered(
                    close_r,
                    "×",
                    if close_hov { 0xFFFFFF } else { t.text_muted },
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((close_r, HitAction::CloseTerminalTab(i)));

                tx += pill_w + 4;
            }

            // `+` 新建终端标签页
            let plus_btn = Rect::new(tx, rect.y + 5, 22, 22);
            if plus_btn.contains(hx, hy) {
                canvas.fill_rounded_rect(plus_btn, 4, t.popup_hover_bg);
            }
            canvas.draw_plus_icon(plus_btn.x + 5, plus_btn.y + 5, t.text_secondary);
            self.hit_regions
                .push((plus_btn, HitAction::NewTerminalTab));
            tx += 26;

            canvas.draw_chevron_down(tx + 2, rect.y + 13, t.text_muted);
            tx += 18;

            let clr_btn = Rect::new(rect.x + rect.w - 300, rect.y + 5, 22, 22);
            if clr_btn.x - tx > 130 {
                canvas.draw_text_styled(
                    tx + 6,
                    rect.y + 8,
                    self.tr(
                        "Ctrl+C 复制/中断 · Ctrl+V 粘贴 · Tab 补全 · 右键菜单 · Ctrl+L 清屏",
                        "Ctrl+C Copy/Interrupt · Ctrl+V Paste · Tab Complete · Context Menu · Ctrl+L Clear",
                    ),
                    t.text_muted,
                    clr_btn.x - tx - 14,
                    FontStyle::UiSmall,
                );
            }

            // 清空终端输出图标
            let clr_hov = clr_btn.contains(hx, hy);
            if clr_hov {
                canvas.fill_rounded_rect(clr_btn, 4, t.popup_hover_bg);
            }
            canvas.draw_trash_icon(
                clr_btn.x + 4,
                clr_btn.y + 4,
                if clr_hov { 0xFFFFFF } else { t.text_muted },
            );
            self.hit_regions.push((clr_btn, HitAction::ClearTerminal));
        } else {
            let label = match active_tool {
                ToolWindowId::Problems => self.tr("问题诊断", "Problems"),
                ToolWindowId::Debug => self.tr("调试器", "Debugger"),
                ToolWindowId::GitLog => self.tr("Git 提交日志", "Git Log"),
                _ => self.tr("面板", "Panel"),
            };
            canvas.draw_text_styled(tx, rect.y + 7, label, t.text_primary, 130, FontStyle::UiBold);
        }

        // 右侧底部工具快捷切换按钮
        let right_tabs = [
            (self.tr("终端", "Term"), ToolWindowId::Terminal),
            (self.tr("问题", "Prob"), ToolWindowId::Problems),
            (self.tr("调试", "Debug"), ToolWindowId::Debug),
            (self.tr("日志", "Git"), ToolWindowId::GitLog),
        ];
        let mut rx = rect.x + rect.w - 260;
        for (name, id) in right_tabs {
            let b = Rect::new(rx, rect.y + 5, 58, 22);
            let sel = id == active_tool;
            let hov = !sel && b.contains(hx, hy);
            if sel {
                canvas.fill_rounded_rect(b, 5, t.editor_bg);
            } else if hov {
                canvas.fill_rounded_rect(b, 5, t.panel_header_bg);
            }
            canvas.draw_text_centered(
                b,
                name,
                if sel {
                    t.accent_blue
                } else if hov {
                    t.text_primary
                } else {
                    t.text_muted
                },
                FontStyle::UiSmall,
            );
            self.hit_regions
                .push((b, HitAction::SelectBottomTool(id)));
            rx += 62;
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
                // 点击终端任意位置即可直接聚焦输入
                self.hit_regions
                    .push((content_rect, HitAction::FocusTerminalInput));
                self.terminal_content_rect = content_rect;

                let line_h = 19;
                self.terminal_line_h = line_h;
                let char_w = canvas.measure_text("M", FontStyle::CodeMono).0.max(8);
                self.terminal_char_w = char_w;

                let max_lines = ((content_rect.h - 14) / line_h).max(2) as usize;
                let cols = ((content_rect.w - 28) / char_w).clamp(40, 240) as u16;
                let rows = (max_lines as u16).clamp(6, 80);
                if (rows, cols) != self.last_pty_size {
                    self.last_pty_size = (rows, cols);
                    self.terminal.active_pty().resize_pty(rows, cols);
                }

                let (snap, scroll_offset, sel_range) = {
                    let pty = self.terminal.active_pty();
                    (pty.snapshot(), pty.scroll_offset, pty.selection_range())
                };
                let total_lines = snap.lines.len();
                let max_up = total_lines.saturating_sub(max_lines);
                let scroll_up = scroll_offset.min(max_up);
                let end_idx = total_lines.saturating_sub(scroll_up);
                let start_idx = end_idx.saturating_sub(max_lines);
                self.terminal_scroll_anchor_line = start_idx;

                let term_focused = self.focus == FocusTarget::TerminalInput;
                let text_left = content_rect.x + 12;
                let right_limit = content_rect.x + content_rect.w - 16;

                if snap.lines.is_empty() && term_focused {
                    if self.caret_visible {
                        canvas.fill_rect(
                            Rect::new(text_left, y + 1, 2, line_h - 3),
                            t.warning_yellow,
                        );
                    }
                    self.ime_spot = (text_left, y + line_h);
                }

                for idx in start_idx..end_idx {
                    let line = &snap.lines[idx];
                    let raw = line.raw_text();

                    // 终端鼠标选区高亮
                    if let Some(((sl, sc), (el, ec))) = sel_range {
                        if idx >= sl && idx <= el {
                            let char_len = raw.chars().count();
                            let c0 = if idx == sl { sc.min(char_len) } else { 0 };
                            let c1 = if idx == el { ec.min(char_len) } else { char_len };
                            let p0: String = raw.chars().take(c0).collect();
                            let p1: String = raw.chars().take(c1).collect();
                            let x0 = (text_left + canvas.measure_text(&p0, FontStyle::CodeMono).0)
                                .clamp(text_left, right_limit);
                            let mut x1 = (text_left
                                + canvas.measure_text(&p1, FontStyle::CodeMono).0)
                                .clamp(text_left, right_limit);
                            if idx < el && x1 < right_limit && c1 == char_len {
                                x1 = (x1 + char_w).min(right_limit);
                            }
                            if x1 > x0 {
                                canvas.fill_rect(
                                    Rect::new(x0, y, x1 - x0, line_h),
                                    t.editor_selection_bg,
                                );
                            }
                        }
                    }

                    let mut sx = text_left;
                    for (txt, fg) in &line.spans {
                        if sx >= right_limit {
                            break;
                        }
                        sx = canvas.draw_text_styled(
                            sx,
                            y,
                            txt,
                            *fg,
                            right_limit - sx,
                            FontStyle::CodeMono,
                        );
                    }

                    // 原生 ConPTY 网格光标渲染与 IME 定位
                    if idx == snap.cursor_line && term_focused {
                        let raw_chars = raw.chars().count();
                        let px_off = if snap.cursor_col <= raw_chars {
                            let prefix: String = raw.chars().take(snap.cursor_col).collect();
                            canvas.measure_text(&prefix, FontStyle::CodeMono).0
                        } else {
                            let full_w = canvas.measure_text(&raw, FontStyle::CodeMono).0;
                            full_w + (snap.cursor_col - raw_chars) as i32 * char_w
                        };
                        let caret_x = (text_left + px_off).clamp(text_left, right_limit - 3);
                        if snap.cursor_visible && self.caret_visible {
                            canvas.fill_rect(
                                Rect::new(caret_x, y + 1, 2, line_h - 3),
                                t.warning_yellow,
                            );
                        }
                        self.ime_spot = (caret_x, y + line_h);
                    }

                    y += line_h;
                }

                // 终端垂直滚动条
                let track = Rect::new(
                    content_rect.x + content_rect.w - 9,
                    content_rect.y + 4,
                    6,
                    content_rect.h - 8,
                );
                let top_offset = max_up.saturating_sub(scroll_up);
                self.terminal_scroll_info = ScrollTrackInfo {
                    track,
                    visible: max_lines,
                    total: total_lines,
                };
                canvas.draw_vertical_scrollbar(
                    track,
                    top_offset,
                    max_lines,
                    total_lines,
                    t.border_color,
                );
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
                self.git_log_branches_rect = Rect::new(0, 0, 0, 0);
                self.git_log_commits_rect = Rect::new(0, 0, 0, 0);
                self.git_log_files_rect = Rect::new(0, 0, 0, 0);
                self.git_log_diff_rect = Rect::new(0, 0, 0, 0);
                self.git_log_commits_scroll_info = ScrollTrackInfo::default();
                self.git_log_diff_scroll_info = ScrollTrackInfo::default();

                // 顶部 IDEA Git Log 工具栏：[搜索框] [分支过滤状态] [Fetch/Pull/Push/Refresh] [Checkout/New Branch/Cherry-Pick/Revert/Copy Hash]
                let tb_rect = Rect::new(content_rect.x + 6, content_rect.y + 4, content_rect.w - 12, 28);
                canvas.fill_rounded_rect(tb_rect, 5, t.panel_bg);

                let search_w = (content_rect.w / 4).clamp(170, 290);
                let search_box = Rect::new(tb_rect.x + 4, tb_rect.y + 2, search_w, 24);
                self.git_log_search_rect = search_box;
                let search_focused = self.focus == FocusTarget::GitLogSearchInput;
                canvas.stroke_rounded_rect(
                    search_box,
                    4,
                    if search_focused {
                        t.accent_blue
                    } else {
                        t.border_color
                    },
                    t.window_bg,
                );
                canvas.draw_search_icon(
                    search_box.x + 5,
                    search_box.y + 5,
                    if search_focused {
                        t.accent_blue
                    } else {
                        t.text_muted
                    },
                );
                let search_inner = Rect::new(
                    search_box.x + 22,
                    search_box.y,
                    (search_box.w - 42).max(40),
                    search_box.h,
                );
                let search_ph = self.tr("搜索提交/作者/Hash...", "Search commits, author, hash...");
                let ime = self.git_log_search_input.render_singleline(
                    canvas,
                    search_inner,
                    0,
                    search_box.y + 4,
                    search_ph,
                    search_focused,
                    self.caret_visible,
                    t.text_primary,
                    t.text_muted,
                    t.editor_selection_bg,
                    t.accent_blue,
                    FontStyle::UiSmall,
                );
                if search_focused {
                    self.ime_spot = ime;
                }
                self.hit_regions
                    .push((search_box, HitAction::FocusGitLogSearchInput));

                if !self.git_log_search_input.is_empty() {
                    let clr_s = Rect::new(search_box.x + search_box.w - 18, search_box.y + 4, 15, 16);
                    canvas.draw_text_centered(clr_s, "×", t.text_muted, FontStyle::UiSmall);
                    self.hit_regions.push((clr_s, HitAction::ClearGitLogSearch));
                }

                let mut bx = search_box.x + search_box.w + 8;
                let tool_btns: Vec<(&str, u32, HitAction)> = vec![
                    (self.tr("↻ 刷新", "↻ Refresh"), t.text_secondary, HitAction::RefreshGitStatus),
                    ("Fetch", t.text_secondary, HitAction::GitFetchAll),
                    ("↓ Pull", t.syn_function, HitAction::ExecuteIdeAction(IdeAction::GitPull)),
                    ("↑ Push", t.run_green, HitAction::ExecuteIdeAction(IdeAction::GitPush)),
                    (self.tr("检出", "Checkout"), t.accent_blue, HitAction::GitCheckoutSelectedCommit),
                    (self.tr("新建分支", "Branch"), t.syn_type, HitAction::GitCreateBranchAtSelectedCommit),
                    ("Cherry-Pick", t.warning_yellow, HitAction::GitCherryPickSelectedCommit),
                    ("Revert", t.breakpoint_red, HitAction::GitRevertSelectedCommit),
                    (self.tr("复制Hash", "Copy Hash"), t.text_muted, HitAction::GitCopySelectedCommitHash),
                ];
                for (lbl, fg, act) in tool_btns {
                    let (lw, _) = canvas.measure_text(lbl, FontStyle::UiSmall);
                    let bw = lw + 14;
                    if bx + bw > tb_rect.x + tb_rect.w - 28 {
                        break;
                    }
                    let btn = Rect::new(bx, tb_rect.y + 2, bw, 24);
                    let hov = btn.contains(hx, hy);
                    canvas.stroke_rounded_rect(
                        btn,
                        4,
                        if hov { t.accent_blue } else { t.border_color },
                        if hov { t.popup_hover_bg } else { t.window_bg },
                    );
                    canvas.draw_text_centered(
                        btn,
                        lbl,
                        if hov { 0xFFFFFF } else { fg },
                        FontStyle::UiSmall,
                    );
                    self.hit_regions.push((btn, act));
                    bx += bw + 4;
                }
                if self.git_load_alpha > 0.02 {
                    canvas.draw_subtle_spinner(
                        bx + 10,
                        tb_rect.y + 14,
                        5.0,
                        self.spinner_phase(),
                        t.accent_blue,
                        t.border_color,
                        self.git_load_alpha,
                    );
                }

                // 3 列主体区域：[左侧 Branches 树] | [中间 Commit Graph & History] | [右侧 Commit 详情 & Diff 预览]
                let body_y = tb_rect.y + tb_rect.h + 4;
                let body_h = (content_rect.y + content_rect.h - body_y - 4).max(40);
                let total_w = content_rect.w - 12;
                let branch_col_w = (total_w / 6).clamp(155, 220);
                let detail_col_w = (total_w * 36 / 100).clamp(260, 480);
                let commit_col_w = (total_w - branch_col_w - detail_col_w - 8).max(260);

                let branches_rect = Rect::new(content_rect.x + 6, body_y, branch_col_w, body_h);
                let commits_rect = Rect::new(
                    branches_rect.x + branch_col_w + 4,
                    body_y,
                    commit_col_w,
                    body_h,
                );
                let detail_rect = Rect::new(
                    commits_rect.x + commit_col_w + 4,
                    body_y,
                    (content_rect.x + content_rect.w - 6 - (commits_rect.x + commit_col_w + 4))
                        .max(160),
                    body_h,
                );
                self.git_log_branches_rect = branches_rect;
                self.git_log_commits_rect = commits_rect;

                // ===== 左列：Branches / Tags 导航树 =====
                canvas.stroke_rounded_rect(branches_rect, 5, t.border_color, t.window_bg);
                #[derive(Clone)]
                enum BranchNavRow {
                    Header(String),
                    Item {
                        label: String,
                        filter_val: Option<String>,
                        is_current: bool,
                        color: u32,
                    },
                }
                let mut b_rows: Vec<BranchNavRow> = Vec::new();
                b_rows.push(BranchNavRow::Item {
                    label: format!("★ {}", self.tr("全部分支 All", "All Branches")),
                    filter_val: None,
                    is_current: false,
                    color: t.accent_blue,
                });
                b_rows.push(BranchNavRow::Header(
                    self.tr("▼ 本地分支 Local", "▼ Local").to_string(),
                ));
                for b in &self.git_engine.local_branches {
                    let is_curr = b == &self.git_engine.current_branch;
                    let lbl = if is_curr {
                        format!("HEAD → {b}")
                    } else {
                        b.clone()
                    };
                    b_rows.push(BranchNavRow::Item {
                        label: lbl,
                        filter_val: Some(b.clone()),
                        is_current: is_curr,
                        color: if is_curr { t.run_green } else { t.text_primary },
                    });
                }
                if !self.git_engine.remote_branches.is_empty() {
                    b_rows.push(BranchNavRow::Header(
                        self.tr("▼ 远程分支 Remote", "▼ Remote").to_string(),
                    ));
                    for rb in &self.git_engine.remote_branches {
                        b_rows.push(BranchNavRow::Item {
                            label: rb.clone(),
                            filter_val: Some(rb.clone()),
                            is_current: false,
                            color: t.syn_type,
                        });
                    }
                }
                if !self.git_engine.tags.is_empty() {
                    b_rows.push(BranchNavRow::Header(
                        self.tr("▼ 标签 Tags", "▼ Tags").to_string(),
                    ));
                    for tg in &self.git_engine.tags {
                        b_rows.push(BranchNavRow::Item {
                            label: format!("🏷 {tg}"),
                            filter_val: Some(tg.clone()),
                            is_current: false,
                            color: t.warning_yellow,
                        });
                    }
                }
                let b_row_h = 21;
                let b_vis = ((branches_rect.h - 8) / b_row_h).max(1) as usize;
                let b_max = b_rows.len().saturating_sub(b_vis);
                if self.git_branches_scroll > b_max {
                    self.git_branches_scroll = b_max;
                }
                let mut by = branches_rect.y + 4;
                for item in b_rows
                    .into_iter()
                    .skip(self.git_branches_scroll)
                    .take(b_vis)
                {
                    match item {
                        BranchNavRow::Header(hdr) => {
                            canvas.draw_text_styled(
                                branches_rect.x + 8,
                                by + 2,
                                &hdr,
                                t.text_muted,
                                branches_rect.w - 16,
                                FontStyle::UiBold,
                            );
                        }
                        BranchNavRow::Item {
                            label,
                            filter_val,
                            is_current,
                            color,
                        } => {
                            let row = Rect::new(
                                branches_rect.x + 4,
                                by,
                                branches_rect.w - 8,
                                b_row_h - 1,
                            );
                            let selected = self.git_log_branch_filter == filter_val;
                            let hov = row.contains(hx, hy);
                            if selected {
                                canvas.fill_rounded_rect(row, 4, t.editor_selection_bg);
                            } else if hov {
                                canvas.fill_rounded_rect(row, 4, t.panel_header_bg);
                            }
                            canvas.draw_text_styled(
                                row.x + 8,
                                by + 2,
                                &label,
                                if selected || hov { 0xFFFFFF } else { color },
                                row.w - 14,
                                if is_current || selected {
                                    FontStyle::UiBold
                                } else {
                                    FontStyle::UiSmall
                                },
                            );
                            self.hit_regions
                                .push((row, HitAction::SelectGitLogBranchFilter(filter_val)));
                        }
                    }
                    by += b_row_h;
                }

                // ===== 中列：Commit Graph & 提交历史表 =====
                canvas.stroke_rounded_rect(commits_rect, 5, t.border_color, t.editor_bg);
                let col_hdr = Rect::new(commits_rect.x + 1, commits_rect.y + 1, commits_rect.w - 2, 22);
                canvas.fill_rect(col_hdr, t.panel_header_bg);
                canvas.draw_text_styled(
                    commits_rect.x + 10,
                    commits_rect.y + 3,
                    self.tr("提交图 / 消息", "Graph / Commit Message"),
                    t.text_muted,
                    commits_rect.w - 180,
                    FontStyle::UiSmall,
                );
                canvas.draw_text_styled(
                    commits_rect.x + commits_rect.w - 172,
                    commits_rect.y + 3,
                    self.tr("作者 · 日期", "Author · Date"),
                    t.text_muted,
                    160,
                    FontStyle::UiSmall,
                );

                let query = self.git_log_search_input.as_str().trim().to_lowercase();
                let branch_filter = self.git_log_branch_filter.clone();
                let filtered_commits: Vec<ide_git::GitLogEntry> = self
                    .git_engine
                    .recent_commits
                    .iter()
                    .filter(|c| {
                        if let Some(ref bf) = branch_filter {
                            let ref_match = c.refs.iter().any(|r| {
                                let clean = r.trim_start_matches("HEAD -> ").trim_start_matches("tag: ");
                                clean == bf || clean.ends_with(bf)
                            });
                            if !ref_match && bf != &self.git_engine.current_branch {
                                return false;
                            }
                        }
                        if query.is_empty() {
                            return true;
                        }
                        c.subject.to_lowercase().contains(&query)
                            || c.hash.to_lowercase().contains(&query)
                            || c.author.to_lowercase().contains(&query)
                            || c.refs.iter().any(|r| r.to_lowercase().contains(&query))
                    })
                    .cloned()
                    .collect();

                if self.selected_git_commit_hash.is_none() {
                    if let Some(first) = filtered_commits.first() {
                        let fh = first.hash.clone();
                        self.selected_git_commit_hash = Some(fh.clone());
                        self.git_engine.request_load_commit_detail(&fh);
                    }
                } else if let Some(ref sel_h) = self.selected_git_commit_hash {
                    if !self.git_engine.commit_details.contains_key(sel_h) {
                        self.git_engine.request_load_commit_detail(sel_h);
                    }
                }

                let list_top = commits_rect.y + 24;
                let list_h = (commits_rect.h - 28).max(24);
                let c_row_h = 24;
                let c_vis = (list_h / c_row_h).max(1) as usize;
                let c_total = filtered_commits.len();
                let c_max_s = c_total.saturating_sub(c_vis);
                if self.git_log_scroll > c_max_s {
                    self.git_log_scroll = c_max_s;
                }

                if !self.git_engine.has_loaded_snapshot {
                    canvas.draw_subtle_spinner(
                        commits_rect.x + 20,
                        list_top + 18,
                        5.5,
                        self.spinner_phase(),
                        t.accent_blue,
                        t.border_color,
                        self.git_load_alpha.max(0.75),
                    );
                    canvas.draw_text_styled(
                        commits_rect.x + 34,
                        list_top + 10,
                        self.tr("正在加载 Git 提交历史与分支拓扑...", "Loading Git commit history..."),
                        t.text_secondary,
                        commits_rect.w - 46,
                        FontStyle::UiRegular,
                    );
                } else if filtered_commits.is_empty() {
                    canvas.draw_text_styled(
                        commits_rect.x + 14,
                        list_top + 10,
                        self.tr(
                            "暂无匹配的 Git 提交记录",
                            "No matching Git commits found",
                        ),
                        t.text_muted,
                        commits_rect.w - 28,
                        FontStyle::UiRegular,
                    );
                } else {
                    let lane_colors = [
                        t.accent_blue,
                        t.run_green,
                        t.syn_function,
                        t.warning_yellow,
                        t.syn_keyword,
                        t.breakpoint_red,
                    ];
                    let graph_left = commits_rect.x + 14;
                    let lane_step = 11;
                    let mut cy = list_top;

                    for entry in filtered_commits
                        .iter()
                        .skip(self.git_log_scroll)
                        .take(c_vis)
                    {
                        let row = Rect::new(
                            commits_rect.x + 3,
                            cy,
                            commits_rect.w - 14,
                            c_row_h - 1,
                        );
                        let is_sel = self
                            .selected_git_commit_hash
                            .as_ref()
                            .is_some_and(|h| h == &entry.hash);
                        let hov = row.contains(hx, hy);
                        if is_sel {
                            canvas.fill_rounded_rect(row, 4, t.editor_selection_bg);
                        } else if hov {
                            canvas.fill_rounded_rect(row, 4, t.panel_header_bg);
                        }
                        self.git_commit_row_rects
                            .push((row, entry.hash.clone(), entry.subject.clone()));

                        // 绘制 Git Commit Graph 泳道竖线 + 合并分叉斜线 + 提交节点圆点
                        let lane = entry.graph_lane.min(4);
                        let node_x = graph_left + (lane as i32) * lane_step;
                        let node_y = cy + c_row_h / 2;
                        let lane_col = lane_colors[lane % lane_colors.len()];
                        canvas.draw_line_aa(
                            node_x as f32,
                            cy as f32,
                            node_x as f32,
                            (cy + c_row_h) as f32,
                            1.8,
                            lane_col,
                        );
                        if entry.is_merge {
                            let merge_x = graph_left + ((lane + 1).min(4) as i32) * lane_step;
                            canvas.draw_line_aa(
                                node_x as f32,
                                node_y as f32,
                                merge_x as f32,
                                (cy + c_row_h) as f32,
                                1.5,
                                lane_colors[(lane + 1) % lane_colors.len()],
                            );
                        }
                        canvas.fill_circle(node_x, node_y, 4, lane_col);

                        let meta_w = 166;
                        let mut text_x = graph_left + 44;
                        let subj_limit = (commits_rect.x + commits_rect.w - meta_w - 8).max(text_x + 40);

                        // Ref 徽章 (分支 / Tag)
                        for rf in entry.refs.iter().take(2) {
                            let badge_fg = if rf.starts_with("HEAD") {
                                0xFFFFFF
                            } else if rf.starts_with("tag:") {
                                t.warning_yellow
                            } else {
                                t.syn_type
                            };
                            let badge_bg = if rf.starts_with("HEAD") {
                                t.run_green
                            } else {
                                t.panel_header_bg
                            };
                            let short_rf = rf.trim_start_matches("HEAD -> ").trim_start_matches("tag: ");
                            let (rw, _) = canvas.measure_text(short_rf, FontStyle::UiSmall);
                            let bw = (rw + 10).min(110);
                            if text_x + bw + 20 >= subj_limit {
                                break;
                            }
                            let br = Rect::new(text_x, cy + 3, bw, 17);
                            canvas.fill_rounded_rect(br, 3, badge_bg);
                            canvas.draw_text_centered(br, short_rf, badge_fg, FontStyle::UiSmall);
                            text_x += bw + 4;
                        }

                        if text_x < subj_limit {
                            canvas.draw_text_styled(
                                text_x,
                                cy + 3,
                                &entry.subject,
                                if is_sel || hov { 0xFFFFFF } else { t.text_primary },
                                subj_limit - text_x,
                                if is_sel {
                                    FontStyle::UiBold
                                } else {
                                    FontStyle::UiSmall
                                },
                            );
                        }

                        let meta_str = format!("{} · {}", entry.author, entry.date);
                        canvas.draw_text_styled(
                            commits_rect.x + commits_rect.w - meta_w,
                            cy + 3,
                            &meta_str,
                            if is_sel { 0xE2E8F0 } else { t.text_muted },
                            meta_w - 14,
                            FontStyle::UiSmall,
                        );
                        self.hit_regions
                            .push((row, HitAction::SelectGitCommit(entry.hash.clone())));
                        cy += c_row_h;
                    }
                }

                let c_track = Rect::new(
                    commits_rect.x + commits_rect.w - 8,
                    list_top,
                    6,
                    list_h,
                );
                self.git_log_commits_scroll_info = ScrollTrackInfo {
                    track: c_track,
                    visible: c_vis,
                    total: c_total,
                };
                canvas.draw_vertical_scrollbar(
                    c_track,
                    self.git_log_scroll,
                    c_vis,
                    c_total,
                    t.border_color,
                );

                // ===== 右列：Commit 详情 / 变更文件列表 / Diff 预览区 =====
                canvas.stroke_rounded_rect(detail_rect, 5, t.border_color, t.window_bg);

                if let Some(wd_path) = self.git_working_diff_target.clone() {
                    let fname = wd_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file");
                    let hdr = Rect::new(detail_rect.x + 2, detail_rect.y + 2, detail_rect.w - 4, 24);
                    canvas.fill_rect(hdr, t.panel_header_bg);
                    canvas.draw_text_styled(
                        hdr.x + 8,
                        hdr.y + 4,
                        &format!(
                            "{}: {fname}",
                            self.tr("工作区变更 Diff", "Working Tree Diff")
                        ),
                        t.accent_blue,
                        hdr.w - 110,
                        FontStyle::UiBold,
                    );
                    let back_btn = Rect::new(hdr.x + hdr.w - 94, hdr.y + 3, 88, 18);
                    canvas.stroke_rounded_rect(back_btn, 3, t.border_color, t.window_bg);
                    canvas.draw_text_centered(
                        back_btn,
                        self.tr("← 提交详情", "← Commit"),
                        t.text_secondary,
                        FontStyle::UiSmall,
                    );
                    self.hit_regions
                        .push((back_btn, HitAction::ClearGitWorkingDiffTarget));

                    let diff_box = Rect::new(
                        detail_rect.x + 4,
                        detail_rect.y + 28,
                        detail_rect.w - 8,
                        (detail_rect.h - 32).max(20),
                    );
                    self.git_log_diff_rect = diff_box;
                    let empty_vec: Vec<ide_git::DiffLine> = Vec::new();
                    let dlines = self
                        .git_engine
                        .working_diffs
                        .get(&wd_path)
                        .unwrap_or(&empty_vec);
                    let d_row_h = 18;
                    let d_vis = (diff_box.h / d_row_h).max(1) as usize;
                    let d_total = dlines.len();
                    let d_max = d_total.saturating_sub(d_vis);
                    if self.git_diff_scroll > d_max {
                        self.git_diff_scroll = d_max;
                    }
                    let mut dy = diff_box.y + 2;
                    for dl in dlines.iter().skip(self.git_diff_scroll).take(d_vis) {
                        let (bg, fg, sign) = match dl.kind {
                            ide_git::DiffLineKind::Added => (t.diff_added_bg, t.diff_added_fg, "+"),
                            ide_git::DiffLineKind::Removed => {
                                (t.diff_removed_bg, t.diff_removed_fg, "-")
                            }
                            ide_git::DiffLineKind::Context => (t.editor_bg, t.text_secondary, " "),
                        };
                        canvas.fill_rect(Rect::new(diff_box.x, dy, diff_box.w - 10, d_row_h - 1), bg);
                        let lno = dl
                            .new_lineno
                            .or(dl.old_lineno)
                            .map(|n| format!("{n:>4}"))
                            .unwrap_or_else(|| "    ".to_string());
                        canvas.draw_text_styled(
                            diff_box.x + 6,
                            dy + 1,
                            &format!("{lno} {sign} {}", dl.content),
                            fg,
                            diff_box.w - 20,
                            FontStyle::CodeMono,
                        );
                        dy += d_row_h;
                    }
                    let d_track = Rect::new(
                        detail_rect.x + detail_rect.w - 8,
                        diff_box.y,
                        6,
                        diff_box.h,
                    );
                    self.git_log_diff_scroll_info = ScrollTrackInfo {
                        track: d_track,
                        visible: d_vis,
                        total: d_total,
                    };
                    canvas.draw_vertical_scrollbar(
                        d_track,
                        self.git_diff_scroll,
                        d_vis,
                        d_total,
                        t.border_color,
                    );
                } else if let Some(sel_hash) = self.selected_git_commit_hash.clone() {
                    if let Some(detail) = self.git_engine.commit_details.get(&sel_hash).cloned() {
                        // 1) 上部提交元数据卡
                        let meta_h = 56;
                        let meta_rect = Rect::new(detail_rect.x + 4, detail_rect.y + 4, detail_rect.w - 8, meta_h);
                        canvas.fill_rounded_rect(meta_rect, 4, t.panel_header_bg);
                        canvas.draw_text_styled(
                            meta_rect.x + 8,
                            meta_rect.y + 4,
                            &detail.subject,
                            t.text_primary,
                            meta_rect.w - 16,
                            FontStyle::UiBold,
                        );
                        canvas.draw_text_styled(
                            meta_rect.x + 8,
                            meta_rect.y + 23,
                            &format!("{}  ·  {}  ·  {}", detail.hash, detail.author, detail.date),
                            t.syn_function,
                            meta_rect.w - 16,
                            FontStyle::CodeMono,
                        );
                        canvas.draw_text_styled(
                            meta_rect.x + 8,
                            meta_rect.y + 39,
                            &format!(
                                "{} {} {}",
                                detail.files.len(),
                                self.tr("个变更文件", "changed files"),
                                if detail.parents.is_empty() {
                                    String::new()
                                } else {
                                    format!("· Parent: {}", detail.parents.join(", ").chars().take(18).collect::<String>())
                                }
                            ),
                            t.text_muted,
                            meta_rect.w - 16,
                            FontStyle::UiSmall,
                        );

                        // 2) 中部 Changed Files 列表
                        let files_top = detail_rect.y + meta_h + 8;
                        let rem_h = (detail_rect.h - meta_h - 14).max(40);
                        let files_h = (rem_h * 38 / 100).clamp(44, 140);
                        let files_box = Rect::new(detail_rect.x + 4, files_top, detail_rect.w - 8, files_h);
                        self.git_log_files_rect = files_box;
                        canvas.stroke_rounded_rect(files_box, 4, t.border_color, t.editor_bg);

                        let f_row_h = 20;
                        let f_vis = ((files_box.h - 4) / f_row_h).max(1) as usize;
                        let f_max = detail.files.len().saturating_sub(f_vis);
                        if self.git_files_scroll > f_max {
                            self.git_files_scroll = f_max;
                        }
                        if self.selected_git_commit_file >= detail.files.len() {
                            self.selected_git_commit_file = 0;
                        }
                        let mut fy = files_box.y + 2;
                        for (idx, cf) in detail
                            .files
                            .iter()
                            .enumerate()
                            .skip(self.git_files_scroll)
                            .take(f_vis)
                        {
                            let frow = Rect::new(files_box.x + 2, fy, files_box.w - 6, f_row_h - 1);
                            let fsel = idx == self.selected_git_commit_file;
                            if fsel {
                                canvas.fill_rounded_rect(frow, 3, t.editor_selection_bg);
                            } else if frow.contains(hx, hy) {
                                canvas.fill_rounded_rect(frow, 3, t.panel_header_bg);
                            }
                            let scolor = match cf.status {
                                ide_git::GitFileStatusKind::Added => t.run_green,
                                ide_git::GitFileStatusKind::Deleted => t.breakpoint_red,
                                _ => t.syn_function,
                            };
                            let stats = format!("+{} -{}", cf.additions, cf.deletions);
                            canvas.draw_text_styled(
                                frow.x + 6,
                                fy + 1,
                                &format!("[{}] {}", cf.status.badge_str(), cf.path),
                                if fsel { 0xFFFFFF } else { scolor },
                                frow.w - 82,
                                FontStyle::CodeMono,
                            );
                            canvas.draw_text_styled(
                                frow.x + frow.w - 72,
                                fy + 1,
                                &stats,
                                if fsel { 0xE2E8F0 } else { t.text_muted },
                                68,
                                FontStyle::UiSmall,
                            );
                            self.hit_regions
                                .push((frow, HitAction::SelectGitCommitFile(idx)));
                            fy += f_row_h;
                        }

                        // 3) 下部所选文件 Diff 预览区
                        let diff_top = files_top + files_h + 4;
                        let diff_h = (detail_rect.y + detail_rect.h - diff_top - 4).max(24);
                        let diff_box = Rect::new(detail_rect.x + 4, diff_top, detail_rect.w - 8, diff_h);
                        self.git_log_diff_rect = diff_box;
                        canvas.stroke_rounded_rect(diff_box, 4, t.border_color, t.editor_bg);

                        let empty_diff: Vec<ide_git::DiffLine> = Vec::new();
                        let active_diff = detail
                            .files
                            .get(self.selected_git_commit_file)
                            .and_then(|f| detail.file_diffs.get(&f.path))
                            .unwrap_or(&empty_diff);
                        let d_row_h = 18;
                        let d_vis = ((diff_box.h - 4) / d_row_h).max(1) as usize;
                        let d_total = active_diff.len();
                        let d_max = d_total.saturating_sub(d_vis);
                        if self.git_diff_scroll > d_max {
                            self.git_diff_scroll = d_max;
                        }
                        let mut dy = diff_box.y + 2;
                        for dl in active_diff.iter().skip(self.git_diff_scroll).take(d_vis) {
                            let (bg, fg, sign) = match dl.kind {
                                ide_git::DiffLineKind::Added => {
                                    (t.diff_added_bg, t.diff_added_fg, "+")
                                }
                                ide_git::DiffLineKind::Removed => {
                                    (t.diff_removed_bg, t.diff_removed_fg, "-")
                                }
                                ide_git::DiffLineKind::Context => {
                                    (t.editor_bg, t.text_secondary, " ")
                                }
                            };
                            canvas.fill_rect(
                                Rect::new(diff_box.x + 2, dy, diff_box.w - 12, d_row_h - 1),
                                bg,
                            );
                            let lno = dl
                                .new_lineno
                                .or(dl.old_lineno)
                                .map(|n| format!("{n:>4}"))
                                .unwrap_or_else(|| "    ".to_string());
                            canvas.draw_text_styled(
                                diff_box.x + 6,
                                dy + 1,
                                &format!("{lno} {sign} {}", dl.content),
                                fg,
                                diff_box.w - 20,
                                FontStyle::CodeMono,
                            );
                            dy += d_row_h;
                        }
                        let d_track = Rect::new(
                            diff_box.x + diff_box.w - 8,
                            diff_box.y + 2,
                            6,
                            diff_box.h - 4,
                        );
                        self.git_log_diff_scroll_info = ScrollTrackInfo {
                            track: d_track,
                            visible: d_vis,
                            total: d_total,
                        };
                        canvas.draw_vertical_scrollbar(
                            d_track,
                            self.git_diff_scroll,
                            d_vis,
                            d_total,
                            t.border_color,
                        );
                    } else {
                        canvas.draw_subtle_spinner(
                            detail_rect.x + 18,
                            detail_rect.y + 20,
                            5.0,
                            self.spinner_phase(),
                            t.accent_blue,
                            t.border_color,
                            0.85,
                        );
                        canvas.draw_text_styled(
                            detail_rect.x + 30,
                            detail_rect.y + 12,
                            self.tr("正在后台加载提交 Diff 详情...", "Loading commit details..."),
                            t.text_muted,
                            detail_rect.w - 40,
                            FontStyle::UiSmall,
                        );
                    }
                } else {
                    canvas.draw_text_styled(
                        detail_rect.x + 12,
                        detail_rect.y + 12,
                        self.tr("点击任意提交记录查看变更文件与 Diff", "Select a commit to inspect changed files & diff"),
                        t.text_muted,
                        detail_rect.w - 24,
                        FontStyle::UiSmall,
                    );
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
        let lsp_status = self.lsp_hub.status_summary(buf.language);
        let dpi_pct = (self.effective_scale() * 100.0).round() as i32;
        let right_info = format!(
            "Ln {}, Col {}   |   UTF-8   |   DPI {}%   |   {}   |   {}",
            buf.cursor_line + 1,
            buf.cursor_col + 1,
            dpi_pct,
            lsp_status,
            t.kind.display_name()
        );
        let (right_w, _) = canvas.measure_text(&right_info, FontStyle::UiSmall);
        let right_x = (rect.w - right_w - 16).max(rect.w / 2);

        let rel_path = buf
            .path
            .strip_prefix(&self.workspace_root)
            .unwrap_or(&buf.path)
            .to_string_lossy()
            .replace('\\', "  ›  ");
        let left_info = format!(
            "{}  ›  {}   |   {}",
            self.workspace_name(),
            rel_path,
            self.status_notification
        );
        let busy_alpha = self
            .startup_load_alpha
            .max(self.git_load_alpha)
            .max(self.tree_load_alpha);
        let left_text_x = if busy_alpha > 0.02 {
            canvas.draw_subtle_spinner(
                rect.x + 17,
                rect.y + 13,
                4.5,
                self.spinner_phase(),
                t.accent_blue,
                t.border_color,
                busy_alpha,
            );
            rect.x + 28
        } else {
            rect.x + 12
        };
        canvas.draw_text_styled(
            left_text_x,
            rect.y + 5,
            &left_info,
            t.text_muted,
            right_x - left_text_x - 12,
            FontStyle::UiSmall,
        );

        canvas.draw_text_styled(
            right_x,
            rect.y + 5,
            &right_info,
            t.text_secondary,
            right_w + 8,
            FontStyle::UiSmall,
        );
    }

    fn render_main_menu_dropdown(&mut self, canvas: &mut SurfaceCanvas, cat: MainMenuCategory) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let entries = cat.entries();
        let mut menu_w = 368;
        for entry in &entries {
            if let MenuEntry::Action(act) = entry {
                let title = act.menu_title_i18n(self.ui_lang);
                let sc = act.shortcut_label();
                let (tw, _) = canvas.measure_text(title, FontStyle::UiRegular);
                let (sw, _) = canvas.measure_text(sc, FontStyle::UiSmall);
                let needed = tw + sw + 76;
                if needed > menu_w {
                    menu_w = needed;
                }
            }
        }
        menu_w = menu_w.min((canvas.width as i32 - 16).max(320));
        let content_h: i32 = entries
            .iter()
            .map(|e| match e {
                MenuEntry::Action(_) => 29,
                MenuEntry::Separator => 9,
            })
            .sum();
        let menu_h = content_h + 14;

        let anchor_x = self
            .menu_category_rects
            .iter()
            .find(|(c, _)| *c == cat)
            .map(|(_, r)| r.x)
            .unwrap_or(12);
        let menu_x = anchor_x.min(canvas.width as i32 - menu_w - 8).max(8);
        let menu_y = 74;
        let box_rect = Rect::new(menu_x, menu_y, menu_w, menu_h);
        self.active_overlay_rects.push(box_rect);

        canvas.fill_rounded_shadow(box_rect, 8);
        canvas.stroke_rounded_rect(box_rect, 8, t.border_color, t.popup_bg);

        let mut iy = menu_y + 7;
        for entry in entries {
            match entry {
                MenuEntry::Separator => {
                    canvas.fill_rect(
                        Rect::new(menu_x + 10, iy + 4, menu_w - 20, 1),
                        t.border_color,
                    );
                    iy += 9;
                }
                MenuEntry::Action(act) => {
                    let row = Rect::new(menu_x + 6, iy, menu_w - 12, 28);
                    let hovered = row.contains(hx, hy);
                    if hovered {
                        canvas.fill_rounded_rect(row, 5, t.popup_hover_bg);
                    }
                    if self.is_action_checked(act) {
                        canvas.draw_check_icon(
                            row.x + 8,
                            iy + 8,
                            if hovered { 0xFFFFFF } else { t.accent_blue },
                        );
                    }
                    let sc = act.shortcut_label();
                    let (sw, _) = canvas.measure_text(sc, FontStyle::UiSmall);
                    canvas.draw_text_styled(
                        row.x + 26,
                        iy + 4,
                        act.menu_title_i18n(self.ui_lang),
                        if hovered { 0xFFFFFF } else { t.text_primary },
                        menu_w - sw - 48,
                        FontStyle::UiRegular,
                    );
                    canvas.draw_text_styled(
                        row.x + row.w - sw - 12,
                        iy + 5,
                        sc,
                        if hovered { 0xE2E8F0 } else { t.text_muted },
                        sw + 4,
                        FontStyle::UiSmall,
                    );
                    self.hit_regions
                        .push((row, HitAction::ExecuteIdeAction(act)));
                    iy += 29;
                }
            }
        }
    }

    fn render_git_branch_popup(&mut self, canvas: &mut SurfaceCanvas) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let popup_w = 376;
        let branches = self.git_engine.local_branches.clone();
        let remotes: Vec<String> = self
            .git_engine
            .remote_branches
            .iter()
            .take(5)
            .cloned()
            .collect();
        let remote_h = if remotes.is_empty() {
            0
        } else {
            24 + (remotes.len() as i32) * 26
        };
        let (ahead, behind) = self.git_engine.ahead_behind;
        let popup_h = (branches.len() as i32) * 28 + remote_h + 236;
        let popup_x = if self.git_pane_popup_anchor_x() > 0 {
            self.git_pane_popup_anchor_x()
        } else {
            184
        };
        let popup_y = if self.hamburger_menu_expanded { 74 } else { 40 };
        let popup_rect = Rect::new(popup_x, popup_y, popup_w, popup_h);
        self.active_overlay_rects.push(popup_rect);

        canvas.fill_rounded_shadow(popup_rect, 8);
        canvas.stroke_rounded_rect(popup_rect, 8, t.border_color, t.popup_bg);
        let hdr_text = if ahead > 0 || behind > 0 {
            format!(
                "{}  (↑{ahead} ↓{behind})",
                self.tr("Git 分支与版本控制", "Git Branches & VCS")
            )
        } else {
            self.tr("Git 分支与版本控制", "Git Branches & VCS")
                .to_string()
        };
        canvas.draw_text_styled(
            popup_rect.x + 12,
            popup_rect.y + 8,
            &hdr_text,
            t.text_primary,
            popup_w - 24,
            FontStyle::UiBold,
        );

        let mut y = popup_rect.y + 34;
        let actions = [
            (
                self.tr("+ 新建分支...", "+ New Branch..."),
                HitAction::CreateNewGitBranchPrompt,
            ),
            (
                self.tr("打开 Git 历史库 (Log)", "Open Git History (Log)"),
                HitAction::SelectBottomTool(ToolWindowId::GitLog),
            ),
            (
                self.tr("提交更改... (Alt+0)", "Commit Changes... (Alt+0)"),
                HitAction::ExecuteIdeAction(IdeAction::GitCommitPopup),
            ),
            (
                self.tr("获取全部远程 (Fetch)", "Fetch All Remotes"),
                HitAction::GitFetchAll,
            ),
            (
                self.tr("更新项目 / 拉取 (Ctrl+T)", "Update Project / Pull (Ctrl+T)"),
                HitAction::ExecuteIdeAction(IdeAction::GitPull),
            ),
            (
                self.tr("推送提交... (Ctrl+Shift+K)", "Push Commits... (Ctrl+Shift+K)"),
                HitAction::ExecuteIdeAction(IdeAction::GitPush),
            ),
        ];
        for (label, act) in actions {
            let row = Rect::new(popup_rect.x + 6, y, popup_w - 12, 26);
            let hovered = row.contains(hx, hy);
            canvas.fill_rounded_rect(
                row,
                5,
                if hovered { t.popup_hover_bg } else { t.window_bg },
            );
            canvas.draw_text_styled(
                row.x + 10,
                y + 3,
                label,
                if hovered { 0xFFFFFF } else { t.syn_function },
                popup_w - 28,
                FontStyle::UiRegular,
            );
            self.hit_regions.push((row, act));
            y += 28;
        }

        canvas.fill_rect(
            Rect::new(popup_rect.x + 10, y + 2, popup_w - 20, 1),
            t.border_color,
        );
        y += 8;
        canvas.draw_text_styled(
            popup_rect.x + 12,
            y,
            self.tr("本地分支 (点击切换 / 右侧合并或删除)", "Local Branches (Click to checkout)"),
            t.text_muted,
            popup_w - 24,
            FontStyle::UiSmall,
        );
        y += 20;

        let current_suffix = self.tr(" (当前)", " (current)");
        for b in branches {
            let row_w = if b == self.git_engine.current_branch {
                popup_w - 12
            } else {
                popup_w - 116
            };
            let row = Rect::new(popup_rect.x + 6, y, row_w, 26);
            let is_curr = b == self.git_engine.current_branch;
            let hovered = row.contains(hx, hy);
            if is_curr || hovered {
                canvas.fill_rounded_rect(row, 5, t.popup_hover_bg);
            }
            if is_curr {
                canvas.draw_check_icon(row.x + 8, y + 7, 0xFFFFFF);
            } else {
                canvas.draw_git_branch_icon(row.x + 8, y + 6, t.accent_blue);
            }
            canvas.draw_text_styled(
                row.x + 26,
                y + 3,
                &format!("{}{}", b, if is_curr { current_suffix } else { "" }),
                if is_curr || hovered {
                    0xFFFFFF
                } else {
                    t.text_primary
                },
                row_w - 32,
                FontStyle::UiRegular,
            );
            self.hit_regions
                .push((row, HitAction::CheckoutGitBranch(b.clone())));

            if !is_curr {
                let merge_btn = Rect::new(popup_rect.x + popup_w - 104, y + 2, 48, 22);
                let mhov = merge_btn.contains(hx, hy);
                canvas.stroke_rounded_rect(
                    merge_btn,
                    4,
                    if mhov { t.accent_blue } else { t.border_color },
                    t.window_bg,
                );
                canvas.draw_text_centered(
                    merge_btn,
                    self.tr("合并", "Merge"),
                    if mhov { t.accent_blue } else { t.text_secondary },
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((merge_btn, HitAction::GitMergeBranch(b.clone())));

                let del_btn = Rect::new(popup_rect.x + popup_w - 52, y + 2, 44, 22);
                let dhov = del_btn.contains(hx, hy);
                canvas.stroke_rounded_rect(
                    del_btn,
                    4,
                    if dhov { t.breakpoint_red } else { t.border_color },
                    t.window_bg,
                );
                canvas.draw_text_centered(
                    del_btn,
                    self.tr("删除", "Del"),
                    if dhov { t.breakpoint_red } else { t.text_muted },
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((del_btn, HitAction::GitDeleteBranch(b)));
            }
            y += 28;
        }

        if !remotes.is_empty() {
            canvas.draw_text_styled(
                popup_rect.x + 12,
                y + 2,
                self.tr("远程分支", "Remote Branches"),
                t.text_muted,
                popup_w - 24,
                FontStyle::UiSmall,
            );
            y += 22;
            for rb in remotes {
                let row = Rect::new(popup_rect.x + 6, y, popup_w - 12, 24);
                let hovered = row.contains(hx, hy);
                if hovered {
                    canvas.fill_rounded_rect(row, 5, t.popup_hover_bg);
                }
                canvas.draw_git_branch_icon(row.x + 8, y + 5, t.syn_type);
                canvas.draw_text_styled(
                    row.x + 26,
                    y + 2,
                    &rb,
                    if hovered { 0xFFFFFF } else { t.text_secondary },
                    popup_w - 38,
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((row, HitAction::CheckoutGitBranch(rb)));
                y += 26;
            }
        }
    }

    fn git_pane_popup_anchor_x(&self) -> i32 {
        210
    }

    fn render_project_popup(&mut self, canvas: &mut SurfaceCanvas) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let recent: Vec<PathBuf> = self
            .recent_workspaces
            .iter()
            .filter(|p| *p != &self.workspace_root)
            .take(4)
            .cloned()
            .collect();

        let popup_w = 388;
        let recent_h = if recent.is_empty() {
            0
        } else {
            28 + (recent.len() as i32) * 28
        };
        let popup_h = 56 + 5 * 28 + recent_h + 28 + (ThemeKind::all().len() as i32) * 28 + 10;
        let popup_x = 68;
        let popup_y = if self.hamburger_menu_expanded { 74 } else { 40 };
        let popup_rect = Rect::new(popup_x, popup_y, popup_w, popup_h);
        self.active_overlay_rects.push(popup_rect);

        canvas.fill_rounded_shadow(popup_rect, 8);
        canvas.stroke_rounded_rect(popup_rect, 8, t.border_color, t.popup_bg);

        let title = format!("{} ({})", self.workspace_name(), self.tr("工作区", "Workspace"));
        canvas.draw_text_styled(
            popup_x + 12,
            popup_y + 8,
            &title,
            t.text_primary,
            popup_w - 24,
            FontStyle::UiBold,
        );
        let ws_text = self.workspace_root.display().to_string();
        canvas.draw_text_styled(
            popup_x + 12,
            popup_y + 28,
            &ws_text,
            t.text_muted,
            popup_w - 24,
            FontStyle::UiSmall,
        );

        let mut y = popup_y + 50;
        canvas.fill_rect(
            Rect::new(popup_x + 10, y, popup_w - 20, 1),
            t.border_color,
        );
        y += 6;

        let quick_actions = [
            (
                self.tr("打开文件夹...", "Open Folder..."),
                "Ctrl+O",
                HitAction::OpenFolderDialog,
            ),
            (
                self.tr("在新窗口中打开文件夹...", "Open Folder in New Window..."),
                "",
                HitAction::OpenFolderInNewWindowDialog,
            ),
            (
                self.tr("全局搜索...", "Search Everywhere..."),
                "Double Shift",
                HitAction::ExecuteIdeAction(IdeAction::SearchEverywhere),
            ),
            (
                self.tr("全局文件内容搜索...", "Find in Files..."),
                "Ctrl+Shift+F",
                HitAction::ExecuteIdeAction(IdeAction::FindInFiles),
            ),
            (
                self.tr("设置...", "Settings..."),
                "Ctrl+Alt+S",
                HitAction::ExecuteIdeAction(IdeAction::OpenSettingsModal),
            ),
        ];
        for (label, sc, act) in quick_actions {
            let row = Rect::new(popup_x + 6, y, popup_w - 12, 27);
            let hovered = row.contains(hx, hy);
            if hovered {
                canvas.fill_rounded_rect(row, 5, t.popup_hover_bg);
            }
            let (sw, _) = canvas.measure_text(sc, FontStyle::UiSmall);
            canvas.draw_text_styled(
                row.x + 10,
                y + 4,
                label,
                if hovered { 0xFFFFFF } else { t.text_primary },
                popup_w - sw - 38,
                FontStyle::UiRegular,
            );
            canvas.draw_text_styled(
                row.x + row.w - sw - 10,
                y + 5,
                sc,
                if hovered { 0xE2E8F0 } else { t.text_muted },
                sw + 4,
                FontStyle::UiSmall,
            );
            self.hit_regions.push((row, act));
            y += 28;
        }

        if !recent.is_empty() {
            canvas.fill_rect(
                Rect::new(popup_x + 10, y + 2, popup_w - 20, 1),
                t.border_color,
            );
            y += 8;
            canvas.draw_text_styled(
                popup_x + 12,
                y,
                self.tr("最近打开的工作区 (点击切换或在新窗口开启)", "Recent Workspaces"),
                t.text_muted,
                popup_w - 24,
                FontStyle::UiSmall,
            );
            y += 20;
            for path in recent {
                let new_win_w = 66;
                let row = Rect::new(popup_x + 6, y, popup_w - new_win_w - 18, 26);
                let hovered = row.contains(hx, hy);
                if hovered {
                    canvas.fill_rounded_rect(row, 5, t.popup_hover_bg);
                }
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| path.display().to_string());
                let label = format!("{}  —  {}", name, path.display());
                canvas.draw_folder_icon(
                    row.x + 8,
                    y + 6,
                    if hovered { 0xFFFFFF } else { t.accent_blue },
                );
                canvas.draw_text_styled(
                    row.x + 26,
                    y + 4,
                    &label,
                    if hovered { 0xFFFFFF } else { t.text_primary },
                    row.w - 32,
                    FontStyle::UiRegular,
                );
                self.hit_regions
                    .push((row, HitAction::PromptOpenWorkspace(path.clone())));

                let nw_btn = Rect::new(popup_x + popup_w - new_win_w - 8, y + 2, new_win_w, 22);
                let nw_hov = nw_btn.contains(hx, hy);
                canvas.stroke_rounded_rect(
                    nw_btn,
                    4,
                    if nw_hov { t.accent_blue } else { t.border_color },
                    if nw_hov { t.popup_hover_bg } else { t.window_bg },
                );
                canvas.draw_text_centered(
                    nw_btn,
                    self.tr("↗ 新窗口", "↗ New Win"),
                    if nw_hov { 0xFFFFFF } else { t.accent_blue },
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((nw_btn, HitAction::OpenWorkspaceInNewWindow(path)));
                y += 28;
            }
        }

        canvas.fill_rect(
            Rect::new(popup_x + 10, y + 2, popup_w - 20, 1),
            t.border_color,
        );
        y += 8;
        canvas.draw_text_styled(
            popup_x + 12,
            y,
            self.tr("外观主题", "Appearance Theme"),
            t.text_muted,
            popup_w - 24,
            FontStyle::UiSmall,
        );
        y += 20;

        for &kind in ThemeKind::all() {
            let row = Rect::new(popup_x + 6, y, popup_w - 12, 27);
            let active = self.theme.kind == kind;
            let hovered = row.contains(hx, hy);
            if active || hovered {
                canvas.fill_rounded_rect(row, 5, t.popup_hover_bg);
            }
            if active {
                canvas.draw_check_icon(row.x + 8, y + 7, 0xFFFFFF);
            }
            canvas.draw_text_styled(
                row.x + 26,
                y + 4,
                kind.display_name(),
                if active || hovered {
                    0xFFFFFF
                } else {
                    t.text_primary
                },
                popup_w - 38,
                FontStyle::UiRegular,
            );
            self.hit_regions.push((row, HitAction::SelectTheme(kind)));
            y += 28;
        }
    }

    fn render_run_config_popup(&mut self, canvas: &mut SurfaceCanvas, win_w: i32) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let cfgs = self.run_configs.clone();
        let popup_w = 348;
        let popup_h = (cfgs.len().max(1) as i32) * 40 + 104;
        let popup_x = (win_w - 460).clamp(180, win_w - popup_w - 8);
        let popup_y = 40;
        let popup_rect = Rect::new(popup_x, popup_y, popup_w, popup_h);
        self.active_overlay_rects.push(popup_rect);

        canvas.fill_rounded_shadow(popup_rect, 8);
        canvas.stroke_rounded_rect(popup_rect, 8, t.border_color, t.popup_bg);

        canvas.draw_text_styled(
            popup_x + 12,
            popup_y + 8,
            self.tr("运行 / 调试配置", "Run / Debug Configurations"),
            t.text_primary,
            popup_w - 24,
            FontStyle::UiBold,
        );

        let mut y = popup_y + 32;
        for (idx, cfg) in cfgs.iter().enumerate() {
            let row = Rect::new(popup_x + 6, y, popup_w - 12, 36);
            let active = idx == self.active_run_config;
            let hovered = row.contains(hx, hy);
            if active || hovered {
                canvas.fill_rounded_rect(row, 5, t.popup_hover_bg);
            }
            if active {
                canvas.draw_check_icon(row.x + 8, y + 11, 0xFFFFFF);
            } else {
                canvas.draw_file_type_icon(row.x + 6, y + 10, cfg.language);
            }
            canvas.draw_text_styled(
                row.x + 26,
                y + 2,
                &cfg.name,
                if active || hovered {
                    0xFFFFFF
                } else {
                    t.text_primary
                },
                popup_w - 38,
                FontStyle::UiRegular,
            );
            let cmd_preview = format!("{} {}", cfg.command, cfg.args.join(" "));
            canvas.draw_text_styled(
                row.x + 26,
                y + 19,
                &cmd_preview,
                if active || hovered {
                    0xE2E8F0
                } else {
                    t.text_muted
                },
                popup_w - 38,
                FontStyle::CodeMono,
            );
            self.hit_regions
                .push((row, HitAction::SelectRunConfig(idx)));
            y += 40;
        }

        canvas.fill_rect(
            Rect::new(popup_x + 10, y + 2, popup_w - 20, 1),
            t.border_color,
        );
        y += 7;

        let run_row = Rect::new(popup_x + 6, y, popup_w - 12, 26);
        let run_hovered = run_row.contains(hx, hy);
        if run_hovered {
            canvas.fill_rounded_rect(run_row, 5, t.popup_hover_bg);
        }
        canvas.stroke_play_triangle(run_row.x + 9, y + 7, 10, t.run_green);
        canvas.draw_text_styled(
            run_row.x + 26,
            y + 4,
            self.tr("运行当前配置 (Shift+F10)", "Run Active Configuration (Shift+F10)"),
            if run_hovered { 0xFFFFFF } else { t.text_primary },
            popup_w - 36,
            FontStyle::UiRegular,
        );
        self.hit_regions.push((run_row, HitAction::RunActiveConfig));
        y += 28;

        let dbg_row = Rect::new(popup_x + 6, y, popup_w - 12, 26);
        let dbg_hovered = dbg_row.contains(hx, hy);
        if dbg_hovered {
            canvas.fill_rounded_rect(dbg_row, 5, t.popup_hover_bg);
        }
        canvas.draw_debug_bug_outline_icon(dbg_row.x + 8, y + 6, t.run_green);
        canvas.draw_text_styled(
            dbg_row.x + 26,
            y + 4,
            self.tr("调试当前配置 (Shift+F9)", "Debug Active Configuration (Shift+F9)"),
            if dbg_hovered { 0xFFFFFF } else { t.text_primary },
            popup_w - 36,
            FontStyle::UiRegular,
        );
        self.hit_regions
            .push((dbg_row, HitAction::DebugActiveConfig));
    }

    fn render_more_menu_popup(&mut self, canvas: &mut SurfaceCanvas, win_w: i32) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let items = [
            (
                self.tr("打开文件夹... (Ctrl+O)", "Open Folder... (Ctrl+O)"),
                HitAction::OpenFolderDialog,
                false,
            ),
            (
                if self.hamburger_menu_expanded {
                    self.tr("隐藏主菜单栏", "Hide Main Menu Bar")
                } else {
                    self.tr("显示主菜单栏", "Show Main Menu Bar")
                },
                HitAction::ToggleHamburgerMenu,
                self.hamburger_menu_expanded,
            ),
            (
                self.tr("运行 Cargo Check (/check)", "Run Cargo Check (/check)"),
                HitAction::RunAgentQuickPrompt("/check"),
                false,
            ),
            (
                self.tr("切换终端面板 (Alt+F12)", "Toggle Terminal (Alt+F12)"),
                HitAction::ExecuteIdeAction(IdeAction::ToggleTerminalWindow),
                self.bottom_tool == Some(ToolWindowId::Terminal),
            ),
            (
                self.tr("切换 AI 助手 (Alt+8)", "Toggle AI Assistant (Alt+8)"),
                HitAction::ExecuteIdeAction(IdeAction::ToggleAgentHubWindow),
                self.right_agent_open,
            ),
            (
                self.tr("放大界面与字体 (Ctrl+=)", "Zoom In UI & Fonts (Ctrl+=)"),
                HitAction::ExecuteIdeAction(IdeAction::ZoomInUi),
                false,
            ),
            (
                self.tr("缩小界面与字体 (Ctrl+-)", "Zoom Out UI & Fonts (Ctrl+-)"),
                HitAction::ExecuteIdeAction(IdeAction::ZoomOutUi),
                false,
            ),
            (
                self.tr("快速切换主题 (Ctrl+`)", "Quick Switch Theme (Ctrl+`)"),
                HitAction::ExecuteIdeAction(IdeAction::SwitchTheme),
                false,
            ),
            (
                self.tr(
                    "界面语言: 中文 / EN (Ctrl+Alt+U)",
                    "Language: EN / 中文 (Ctrl+Alt+U)",
                ),
                HitAction::ExecuteIdeAction(IdeAction::SwitchUiLanguage),
                false,
            ),
            (
                self.tr("设置... (Ctrl+Alt+S)", "Settings... (Ctrl+Alt+S)"),
                HitAction::ExecuteIdeAction(IdeAction::OpenSettingsModal),
                false,
            ),
        ];

        let popup_w = 338;
        let popup_h = 8 + (items.len() as i32) * 28 + 6;
        let popup_x = (win_w - 376).clamp(180, win_w - popup_w - 8);
        let popup_y = 40;
        let popup_rect = Rect::new(popup_x, popup_y, popup_w, popup_h);
        self.active_overlay_rects.push(popup_rect);

        canvas.fill_rounded_shadow(popup_rect, 8);
        canvas.stroke_rounded_rect(popup_rect, 8, t.border_color, t.popup_bg);

        let mut y = popup_y + 8;
        for (label, action, checked) in items {
            let row = Rect::new(popup_x + 6, y, popup_w - 12, 27);
            let hovered = row.contains(hx, hy);
            if hovered {
                canvas.fill_rounded_rect(row, 5, t.popup_hover_bg);
            }
            if checked {
                canvas.draw_check_icon(
                    row.x + 8,
                    y + 7,
                    if hovered { 0xFFFFFF } else { t.accent_blue },
                );
            }
            canvas.draw_text_styled(
                row.x + 26,
                y + 4,
                label,
                if hovered { 0xFFFFFF } else { t.text_primary },
                popup_w - 36,
                FontStyle::UiRegular,
            );
            self.hit_regions.push((row, action));
            y += 28;
        }
    }

    fn render_search_everywhere_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let mw = 680.min(w - 40);
        let mh = 390.min(h - 60);
        let mx = (w - mw) / 2;
        let my = (h - mh) / 4;
        let modal = Rect::new(mx, my, mw, mh);
        self.active_overlay_rects.push(modal);

        canvas.stroke_rounded_rect(modal, 8, t.accent_blue, t.popup_bg);

        let header = Rect::new(mx + 2, my + 2, mw - 4, 34);
        canvas.fill_rounded_rect(header, 6, t.panel_header_bg);
        canvas.draw_search_icon(mx + 14, my + 11, t.accent_blue);
        canvas.draw_text_styled(
            mx + 34,
            my + 9,
            self.tr(
                "全局搜索  [全部 | 类 | 文件 | 符号 | 动作]  (Esc 关闭)",
                "Search Everywhere  [All | Classes | Files | Symbols | Actions]  (Esc to close)",
            ),
            t.text_primary,
            mw - 48,
            FontStyle::UiBold,
        );

        let input_box = Rect::new(mx + 12, my + 42, mw - 24, 30);
        self.search_modal_input_rect = input_box;
        canvas.stroke_rounded_rect(input_box, 5, t.accent_blue, t.editor_bg);
        let prefix_end_x = canvas.draw_text_styled(
            mx + 18,
            my + 48,
            "> ",
            t.accent_blue,
            30,
            FontStyle::CodeMono,
        );
        self.search_modal_text_start_x = prefix_end_x;
        let field_box = Rect::new(
            prefix_end_x,
            my + 42,
            (input_box.x + input_box.w - prefix_end_x).max(60),
            30,
        );
        let placeholder = self.tr(
            "输入文件名、符号或动作命令...",
            "Type file name, symbol, or action...",
        );
        let ime = self.search_query_input.render_singleline(
            canvas,
            field_box,
            0,
            my + 48,
            placeholder,
            true,
            self.caret_visible,
            t.text_primary,
            t.text_muted,
            t.editor_selection_bg,
            t.accent_blue,
            FontStyle::CodeMono,
        );
        self.ime_spot = ime;

        let mut ry = my + 80;
        let results = self.search_results.clone();
        if results.is_empty() && self.search_loading {
            canvas.draw_subtle_spinner(
                mx + 22,
                ry + 13,
                5.0,
                self.spinner_phase(),
                t.accent_blue,
                t.border_color,
                0.9,
            );
            canvas.draw_text_styled(
                mx + 34,
                ry + 4,
                self.tr("正在后台索引工作区文件...", "Indexing workspace files in background..."),
                t.text_muted,
                mw - 54,
                FontStyle::UiRegular,
            );
        }
        let file_tag = self.tr("文件", "File");
        let action_tag = self.tr("动作", "Action");
        let total = results.len();
        let max_rows = 11usize;
        for item in results.into_iter().skip(self.modal_scroll).take(max_rows) {
            if ry + 24 > my + mh - 8 {
                break;
            }
            let row = Rect::new(mx + 12, ry, mw - 30, 24);
            let hovered = row.contains(hx, hy)
                && !matches!(item, SearchEverywhereItem::AgentCommand { .. });
            canvas.fill_rounded_rect(
                row,
                4,
                if hovered {
                    t.popup_hover_bg
                } else {
                    t.editor_current_line_bg
                },
            );
            match item {
                SearchEverywhereItem::File {
                    name,
                    relative_path,
                    full_path,
                } => {
                    canvas.draw_text_styled(
                        mx + 18,
                        ry + 3,
                        &format!("[{}]  {}   ({})", file_tag, name, relative_path),
                        if hovered { 0xFFFFFF } else { t.text_primary },
                        mw - 42,
                        FontStyle::UiRegular,
                    );
                    self.hit_regions
                        .push((row, HitAction::OpenFile(full_path)));
                }
                SearchEverywhereItem::Action {
                    action,
                    title: _,
                    shortcut,
                } => {
                    canvas.draw_text_styled(
                        mx + 18,
                        ry + 3,
                        &format!(
                            "[{}]  {}     [{}]",
                            action_tag,
                            action.title_i18n(self.ui_lang),
                            shortcut
                        ),
                        if hovered { 0xFFFFFF } else { t.syn_function },
                        mw - 42,
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
                        mw - 42,
                        FontStyle::UiRegular,
                    );
                }
            }
            ry += 26;
        }
        canvas.draw_vertical_scrollbar(
            Rect::new(mx + mw - 12, my + 80, 6, mh - 90),
            self.modal_scroll,
            max_rows,
            total,
            t.border_color,
        );
    }

    fn render_find_in_files_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let mw = 700.min(w - 40);
        let mh = 390.min(h - 60);
        let mx = (w - mw) / 2;
        let my = (h - mh) / 4;
        let modal = Rect::new(mx, my, mw, mh);
        self.active_overlay_rects.push(modal);

        canvas.stroke_rounded_rect(modal, 8, t.accent_blue, t.popup_bg);
        canvas.draw_search_icon(mx + 14, my + 11, t.accent_blue);
        canvas.draw_text_styled(
            mx + 34,
            my + 9,
            self.tr(
                "全局文件内容搜索 (Ripgrep 全文检索 Ctrl+Shift+F)",
                "Find in Files (Ripgrep Full-Text Search Ctrl+Shift+F)",
            ),
            t.text_primary,
            mw - 48,
            FontStyle::UiBold,
        );

        let input_box = Rect::new(mx + 12, my + 40, mw - 24, 30);
        self.search_modal_input_rect = input_box;
        canvas.stroke_rounded_rect(input_box, 5, t.accent_blue, t.editor_bg);
        let prefix_end_x = canvas.draw_text_styled(
            mx + 18,
            my + 46,
            "Regex/Text > ",
            t.accent_blue,
            130,
            FontStyle::CodeMono,
        );
        self.search_modal_text_start_x = prefix_end_x;
        let field_box = Rect::new(
            prefix_end_x,
            my + 40,
            (input_box.x + input_box.w - prefix_end_x).max(60),
            30,
        );
        let placeholder = self.tr("输入代码关键字...", "Enter code keyword or regex...");
        let ime = self.search_query_input.render_singleline(
            canvas,
            field_box,
            0,
            my + 46,
            placeholder,
            true,
            self.caret_visible,
            t.text_primary,
            t.text_muted,
            t.editor_selection_bg,
            t.accent_blue,
            FontStyle::CodeMono,
        );
        self.ime_spot = ime;

        let mut ry = my + 78;
        let hits = self.content_search_hits.clone();
        if hits.is_empty() && self.search_loading {
            canvas.draw_subtle_spinner(
                mx + 22,
                ry + 13,
                5.0,
                self.spinner_phase(),
                t.accent_blue,
                t.border_color,
                0.9,
            );
            canvas.draw_text_styled(
                mx + 34,
                ry + 4,
                self.tr("正在后台执行 Ripgrep 全文检索...", "Running Ripgrep search in background..."),
                t.text_muted,
                mw - 54,
                FontStyle::UiRegular,
            );
        }
        let total = hits.len();
        let max_rows = 11usize;
        for hit in hits.into_iter().skip(self.modal_scroll).take(max_rows) {
            let row = Rect::new(mx + 12, ry, mw - 30, 24);
            let hovered = row.contains(hx, hy);
            canvas.fill_rounded_rect(
                row,
                4,
                if hovered {
                    t.popup_hover_bg
                } else {
                    t.editor_current_line_bg
                },
            );
            let fname = hit
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file");
            canvas.draw_text_styled(
                mx + 18,
                ry + 3,
                &format!("{}:{}  |  {}", fname, hit.line_number, hit.line_text),
                if hovered { 0xFFFFFF } else { t.text_primary },
                mw - 42,
                FontStyle::CodeMono,
            );
            self.hit_regions
                .push((row, HitAction::OpenFile(hit.path)));
            ry += 26;
        }
        canvas.draw_vertical_scrollbar(
            Rect::new(mx + mw - 12, my + 78, 6, mh - 88),
            self.modal_scroll,
            max_rows,
            total,
            t.border_color,
        );
    }

    fn render_open_project_choice_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let Some(target_path) = self.pending_open_workspace.clone() else {
            self.focus = FocusTarget::Editor;
            return;
        };
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let mw = 500.min(w - 40);
        let mh = 164;
        let mx = (w - mw) / 2;
        let my = (h - mh) / 3;
        let modal = Rect::new(mx, my, mw, mh);
        self.active_overlay_rects.push(modal);

        canvas.fill_rounded_shadow(modal, 8);
        canvas.stroke_rounded_rect(modal, 8, t.accent_blue, t.popup_bg);

        let header = Rect::new(mx + 2, my + 2, mw - 4, 34);
        canvas.fill_rounded_rect(header, 6, t.panel_header_bg);
        canvas.draw_folder_outline_icon(mx + 14, my + 11, t.accent_blue);
        canvas.draw_text_styled(
            mx + 36,
            my + 9,
            self.tr("打开项目  (Open Project)", "Open Project"),
            t.text_primary,
            mw - 50,
            FontStyle::UiBold,
        );

        let folder_name = target_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Project");
        canvas.draw_text_styled(
            mx + 18,
            my + 48,
            &format!(
                "{}: {folder_name}",
                self.tr("选择打开方式", "How would you like to open")
            ),
            t.text_primary,
            mw - 36,
            FontStyle::UiRegular,
        );
        canvas.draw_text_styled(
            mx + 18,
            my + 70,
            &target_path.display().to_string(),
            t.text_muted,
            mw - 36,
            FontStyle::CodeMono,
        );
        canvas.draw_text_styled(
            mx + 18,
            my + 92,
            self.tr(
                "可在当前窗口切换，或保留当前项目并在全新独立窗口开启：",
                "Open in current window or launch an independent new window:",
            ),
            t.text_secondary,
            mw - 36,
            FontStyle::UiSmall,
        );

        let btn_y = my + mh - 42;
        let cancel_btn = Rect::new(mx + mw - 104, btn_y, 88, 28);
        let cancel_hov = cancel_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            cancel_btn,
            6,
            t.border_color,
            if cancel_hov { t.panel_header_bg } else { t.window_bg },
        );
        canvas.draw_text_centered(
            cancel_btn,
            self.tr("取消 (Esc)", "Cancel"),
            t.text_secondary,
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((cancel_btn, HitAction::CloseModalOrPopup));

        let this_btn = Rect::new(cancel_btn.x - 134, btn_y, 126, 28);
        let this_hov = this_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            this_btn,
            6,
            if this_hov { t.accent_blue } else { t.border_color },
            if this_hov { t.popup_hover_bg } else { t.editor_bg },
        );
        canvas.draw_text_centered(
            this_btn,
            self.tr("当前窗口打开", "This Window"),
            if this_hov { 0xFFFFFF } else { t.text_primary },
            FontStyle::UiRegular,
        );
        self.hit_regions
            .push((this_btn, HitAction::ConfirmOpenPendingInThisWindow));

        let new_btn = Rect::new(this_btn.x - 140, btn_y, 132, 28);
        canvas.fill_rounded_rect(new_btn, 6, t.accent_blue);
        canvas.draw_text_centered(
            new_btn,
            self.tr("新窗口打开 (Enter)", "New Window"),
            0xFFFFFF,
            FontStyle::UiBold,
        );
        self.hit_regions
            .push((new_btn, HitAction::ConfirmOpenPendingInNewWindow));
    }

    fn render_input_prompt_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let t = self.theme;
        let mw = 460;
        let mh = 135;
        let mx = (w - mw) / 2;
        let my = (h - mh) / 3;
        let modal = Rect::new(mx, my, mw, mh);
        self.active_overlay_rects.push(modal);

        canvas.stroke_rounded_rect(modal, 8, t.accent_blue, t.popup_bg);

        let (title, footer) = match self.focus {
            FocusTarget::NewFileModal => (
                match self.ui_lang {
                    UiLanguage::ZhCn => format!(
                        "在 {} 新建文件 (Enter 创建 / Esc 取消)",
                        self.project_tree.active_target_dir().display()
                    ),
                    UiLanguage::EnUs => format!(
                        "New File in {} (Enter to create / Esc to cancel)",
                        self.project_tree.active_target_dir().display()
                    ),
                },
                self.tr("按 Enter 确认创建，按 Esc 关闭弹窗", "Press Enter to create, Esc to cancel"),
            ),
            FocusTarget::NewFolderModal => (
                match self.ui_lang {
                    UiLanguage::ZhCn => format!(
                        "在 {} 新建目录 (Enter 创建 / Esc 取消)",
                        self.project_tree.active_target_dir().display()
                    ),
                    UiLanguage::EnUs => format!(
                        "New Directory in {} (Enter to create / Esc to cancel)",
                        self.project_tree.active_target_dir().display()
                    ),
                },
                self.tr("按 Enter 确认创建，按 Esc 关闭弹窗", "Press Enter to create, Esc to cancel"),
            ),
            FocusTarget::NewGitBranchModal => (
                self.tr(
                    "创建并检出新 Git 分支 (Enter 创建)",
                    "Create & Checkout New Git Branch (Enter to create)",
                )
                .to_string(),
                self.tr("按 Enter 确认创建，按 Esc 关闭弹窗", "Press Enter to create, Esc to cancel"),
            ),
            FocusTarget::RenamePathModal => (
                match self.ui_lang {
                    UiLanguage::ZhCn => format!(
                        "重命名 {} (Enter 重命名 / Esc 取消)",
                        self.rename_target_path
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                    ),
                    UiLanguage::EnUs => format!(
                        "Rename {} (Enter to rename / Esc to cancel)",
                        self.rename_target_path
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                    ),
                },
                self.tr("按 Enter 确认重命名，按 Esc 关闭弹窗", "Press Enter to rename, Esc to cancel"),
            ),
            _ => (
                String::new(),
                self.tr("按 Enter 确认，按 Esc 关闭弹窗", "Press Enter to confirm, Esc to cancel"),
            ),
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
        self.prompt_modal_input_rect = input_box;
        canvas.stroke_rounded_rect(input_box, 5, t.accent_blue, t.editor_bg);
        let placeholder = self.tr("输入名称...", "Enter name...");
        let ime = self.modal_text_input.render_singleline(
            canvas,
            input_box,
            8,
            my + 55,
            placeholder,
            true,
            self.caret_visible,
            t.text_primary,
            t.text_muted,
            t.editor_selection_bg,
            t.accent_blue,
            FontStyle::CodeMono,
        );
        self.ime_spot = ime;

        canvas.draw_text_styled(
            mx + 16,
            my + 92,
            footer,
            t.text_muted,
            mw - 32,
            FontStyle::UiSmall,
        );
    }

    fn render_settings_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let mw = 740.min(w - 40);
        let mh = 540.min(h - 40);
        let mx = (w - mw) / 2;
        let my = (h - mh) / 4;
        let modal = Rect::new(mx, my, mw, mh);
        self.active_overlay_rects.push(modal);

        canvas.stroke_rounded_rect(modal, 8, t.accent_blue, t.popup_bg);
        canvas.draw_text_styled(
            mx + 16,
            my + 12,
            self.tr(
                "设置 — 界面语言、DPI 缩放、外观主题与 IntelliJ IDEA 快捷键 (Ctrl+Alt+S)",
                "Settings — Language, DPI Scale, Theme & IntelliJ IDEA Keymap (Ctrl+Alt+S)",
            ),
            t.text_primary,
            mw - 100,
            FontStyle::UiBold,
        );

        let close_btn = Rect::new(mx + mw - 72, my + 8, 58, 24);
        let close_hov = close_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            close_btn,
            5,
            if close_hov { t.accent_blue } else { t.border_color },
            if close_hov { t.popup_hover_bg } else { t.window_bg },
        );
        canvas.draw_text_centered(
            close_btn,
            self.tr("关闭", "Close"),
            if close_hov { 0xFFFFFF } else { t.text_primary },
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((close_btn, HitAction::CloseModalOrPopup));

        // 1. UI Language
        canvas.draw_text_styled(
            mx + 16,
            my + 42,
            self.tr(
                "1. 界面语言 / UI Language (点击即刻切换):",
                "1. UI Language (Click to switch immediately):",
            ),
            t.syn_function,
            mw - 32,
            FontStyle::UiBold,
        );

        let mut lx = mx + 16;
        for &lang in UiLanguage::all() {
            let btn = Rect::new(lx, my + 65, 200, 28);
            let active = self.ui_lang == lang;
            let hov = !active && btn.contains(hx, hy);
            canvas.stroke_rounded_rect(
                btn,
                6,
                if active || hov { t.accent_blue } else { t.border_color },
                if active {
                    t.popup_hover_bg
                } else if hov {
                    t.panel_header_bg
                } else {
                    t.editor_bg
                },
            );
            canvas.draw_text_centered(
                btn,
                lang.display_name(),
                if active || hov { 0xFFFFFF } else { t.text_primary },
                if active {
                    FontStyle::UiBold
                } else {
                    FontStyle::UiRegular
                },
            );
            self.hit_regions.push((btn, HitAction::SetUiLanguage(lang)));
            lx += 212;
        }

        // 2. IDE Theme
        canvas.draw_text_styled(
            mx + 16,
            my + 102,
            self.tr(
                "2. 外观主题 / IDE Theme (点击即刻切换配色):",
                "2. IDE Theme (Click to switch color scheme):",
            ),
            t.syn_function,
            mw - 32,
            FontStyle::UiBold,
        );

        let mut tx = mx + 16;
        for &kind in ThemeKind::all() {
            let btn = Rect::new(tx, my + 125, 215, 28);
            let active = self.theme.kind == kind;
            let hov = !active && btn.contains(hx, hy);
            canvas.stroke_rounded_rect(
                btn,
                6,
                if active || hov { t.accent_blue } else { t.border_color },
                if active {
                    t.popup_hover_bg
                } else if hov {
                    t.panel_header_bg
                } else {
                    t.editor_bg
                },
            );
            canvas.draw_text_centered(
                btn,
                kind.display_name(),
                if active || hov { 0xFFFFFF } else { t.text_primary },
                if active {
                    FontStyle::UiBold
                } else {
                    FontStyle::UiRegular
                },
            );
            self.hit_regions.push((btn, HitAction::SelectTheme(kind)));
            tx += 225;
        }

        // 3. System DPI & UI Font Zoom
        let sys_dpi_pct = (self.system_dpi_scale * 100.0).round() as i32;
        let eff_dpi_pct = (self.effective_scale() * 100.0).round() as i32;
        let cur_ui_pct = (self.ui_scale * 100.0).round() as u16;
        let dpi_header = match self.ui_lang {
            UiLanguage::ZhCn => format!(
                "3. 界面与字体缩放 (自动适配系统 DPI {sys_dpi_pct}% · 当前总缩放 {eff_dpi_pct}% · Ctrl+= / Ctrl+- / Ctrl+0):"
            ),
            UiLanguage::EnUs => format!(
                "3. UI & Font Scale (Auto System DPI {sys_dpi_pct}% · Effective {eff_dpi_pct}% · Ctrl+= / Ctrl+- / Ctrl+0):"
            ),
        };
        canvas.draw_text_styled(
            mx + 16,
            my + 162,
            &dpi_header,
            t.syn_function,
            mw - 32,
            FontStyle::UiBold,
        );

        let scale_presets: [(u16, &str); 5] = [
            (90, "90%"),
            (100, self.tr("100% (标准 DPI)", "100% (Default)")),
            (110, "110%"),
            (125, "125%"),
            (150, "150%"),
        ];
        let mut sx = mx + 16;
        for (pct, label) in scale_presets {
            let bw = if pct == 100 { 142 } else { 112 };
            let btn = Rect::new(sx, my + 185, bw, 28);
            let active = (cur_ui_pct as i32 - pct as i32).abs() <= 3;
            let hov = !active && btn.contains(hx, hy);
            canvas.stroke_rounded_rect(
                btn,
                6,
                if active || hov { t.accent_blue } else { t.border_color },
                if active {
                    t.popup_hover_bg
                } else if hov {
                    t.panel_header_bg
                } else {
                    t.editor_bg
                },
            );
            canvas.draw_text_centered(
                btn,
                label,
                if active || hov { 0xFFFFFF } else { t.text_primary },
                if active {
                    FontStyle::UiBold
                } else {
                    FontStyle::UiRegular
                },
            );
            self.hit_regions.push((btn, HitAction::SetUiScale(pct)));
            sx += bw + 10;
        }

        // 4. Keymap Reference
        canvas.draw_text_styled(
            mx + 16,
            my + 222,
            self.tr(
                "4. IntelliJ IDEA 快捷键清单 (滚轮可上下滚动，点击可直接执行):",
                "4. IntelliJ IDEA Keymap Reference (Scroll to browse, click to execute):",
            ),
            t.syn_function,
            mw - 32,
            FontStyle::UiBold,
        );

        let all_actions = IdeAction::all();
        let max_rows = ((mh - 256) / 26).max(4) as usize;
        let max_s = all_actions.len().saturating_sub(max_rows);
        if self.modal_scroll > max_s {
            self.modal_scroll = max_s;
        }
        let mut ky = my + 246;
        for &act in all_actions.iter().skip(self.modal_scroll).take(max_rows) {
            if ky + 25 > my + mh - 10 {
                break;
            }
            let row = Rect::new(mx + 16, ky, mw - 38, 24);
            let hov = row.contains(hx, hy);
            canvas.fill_rounded_rect(
                row,
                4,
                if hov { t.popup_hover_bg } else { t.editor_bg },
            );
            canvas.draw_text_styled(
                mx + 24,
                ky + 2,
                act.title_i18n(self.ui_lang),
                if hov { 0xFFFFFF } else { t.text_primary },
                mw - 180,
                FontStyle::UiRegular,
            );
            canvas.draw_text_styled(
                mx + mw - 150,
                ky + 3,
                act.shortcut_label(),
                if hov { 0xE2E8F0 } else { t.syn_keyword },
                120,
                FontStyle::CodeMonoBold,
            );
            self.hit_regions
                .push((row, HitAction::ExecuteIdeAction(act)));
            ky += 26;
        }
        canvas.draw_vertical_scrollbar(
            Rect::new(mx + mw - 16, my + 246, 6, (mh - 256).max(40)),
            self.modal_scroll,
            max_rows,
            all_actions.len(),
            t.border_color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn cursor_kind_and_splitter_hit_test_cover_all_ide_regions() {
        let root = PathBuf::from(".");
        let mut state = IdeApplicationState::new(root);
        let mut canvas = SurfaceCanvas::new(1560, 940);
        state.render(&mut canvas);

        let w = 1560;
        let h = 940;

        // 1. 编辑器文本区与终端内容区 -> I-beam Text
        let ed_cx = state.editor_code_rect.x + state.editor_code_rect.w / 2;
        let ed_cy = state.editor_code_rect.y + state.editor_code_rect.h / 2;
        assert_eq!(state.cursor_kind_at(ed_cx, ed_cy, w, h), IdeCursorKind::Text);

        let tm_cx = state.terminal_content_rect.x + state.terminal_content_rect.w / 2;
        let tm_cy = state.terminal_content_rect.y + state.terminal_content_rect.h / 2;
        assert_eq!(state.cursor_kind_at(tm_cx, tm_cy, w, h), IdeCursorKind::Text);

        // 2. 右侧 AI 对话输入框 -> Text
        let ai_cx = state.agent_input_text_rect.x + 20;
        let ai_cy = state.agent_input_text_rect.y + 10;
        assert_eq!(state.cursor_kind_at(ai_cx, ai_cy, w, h), IdeCursorKind::Text);

        // 3. 左右分割线 -> EwResize；底部分割线 -> NsResize
        let left_split_x = 42 + state.left_panel_width;
        assert_eq!(
            state.splitter_at(left_split_x, 120, w, h),
            Some(DragSplitter::LeftPanel)
        );
        assert_eq!(
            state.cursor_kind_at(left_split_x, 120, w, h),
            IdeCursorKind::EwResize
        );

        let right_split_x = w - 42 - state.right_panel_width;
        assert_eq!(
            state.cursor_kind_at(right_split_x, 120, w, h),
            IdeCursorKind::EwResize
        );

        let body_h = h - 38 - 26;
        let upper_bottom = 38 + (body_h - state.bottom_panel_height);
        assert_eq!(
            state.splitter_at(w / 2, upper_bottom, w, h),
            Some(DragSplitter::BottomPanel)
        );
        assert_eq!(
            state.cursor_kind_at(w / 2, upper_bottom, w, h),
            IdeCursorKind::NsResize
        );

        // 4. 顶部主工具栏图标/窗口按钮 & 标签页 -> Pointer
        assert_eq!(state.cursor_kind_at(20, 18, w, h), IdeCursorKind::Pointer);
        assert_eq!(
            state.cursor_kind_at(w - 20, 18, w, h),
            IdeCursorKind::Pointer
        );

        // 5. 底部状态栏空白区 -> Default
        assert_eq!(
            state.cursor_kind_at(w / 2, h - 10, w, h),
            IdeCursorKind::Default
        );

        // 6. 打开 SearchEverywhere 模态框后：外部与分割线隔离为 Default，输入框为 Text，标题空白为 Default
        state.execute_ide_action(IdeAction::SearchEverywhere);
        state.render(&mut canvas);
        let in_cx = state.search_modal_input_rect.x + 30;
        let in_cy = state.search_modal_input_rect.y + 15;
        assert_eq!(state.cursor_kind_at(in_cx, in_cy, w, h), IdeCursorKind::Text);
        // 模态框标题栏空白处不穿透底层面板 -> Default
        assert_eq!(
            state.cursor_kind_at(in_cx, state.search_modal_input_rect.y - 20, w, h),
            IdeCursorKind::Default
        );
        // 模态框外侧的分割线、编辑器边缘、右侧面板 -> Default
        assert_eq!(
            state.cursor_kind_at(left_split_x, 120, w, h),
            IdeCursorKind::Default
        );
        assert_eq!(
            state.cursor_kind_at(state.editor_code_rect.x + 12, ed_cy, w, h),
            IdeCursorKind::Default
        );
    }

    #[test]
    fn render_caches_invalidate_on_buffer_edit_and_async_search_wakes_ui() {
        let root = PathBuf::from(".");
        let mut state = IdeApplicationState::new(root);
        let mut canvas = SurfaceCanvas::new(1280, 800);
        state.left_tool = Some(ToolWindowId::Structure);
        state.render(&mut canvas);

        let initial_rev = state.active_buffer().revision();
        assert_eq!(
            state.cached_runnables.as_ref().map(|(_, r, _)| *r),
            Some(initial_rev)
        );
        assert_eq!(
            state.cached_structure.as_ref().map(|(_, r, _)| *r),
            Some(initial_rev)
        );
        assert_eq!(state.cached_highlight_lines.1, initial_rev);

        // 修改缓冲区后 revision 递增，下次渲染自动失效并重建缓存
        state.active_buffer_mut().insert_str_at_cursor("\npub fn cache_probe_fn() {}\n");
        let new_rev = state.active_buffer().revision();
        assert!(new_rev > initial_rev);

        state.render(&mut canvas);
        assert_eq!(
            state.cached_runnables.as_ref().map(|(_, r, _)| *r),
            Some(new_rev)
        );
        assert_eq!(
            state.cached_structure.as_ref().map(|(_, r, _)| *r),
            Some(new_rev)
        );
        assert_eq!(state.cached_highlight_lines.1, new_rev);

        // 撤销恢复后同样精确匹配 revision
        assert!(state.active_buffer_mut().undo());
        state.render(&mut canvas);
        assert_eq!(
            state.cached_structure.as_ref().map(|(_, r, _)| *r),
            Some(initial_rev)
        );
    }

    #[test]
    fn switch_workspace_updates_project_tree_buffers_recent_and_title_dirty() {
        let unique = format!(
            "rust-ai-ide-ws-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let temp_root = std::env::temp_dir().join(&unique);
        let src_dir = temp_root.join("src");
        std::fs::create_dir_all(&src_dir).expect("create temp workspace");
        std::fs::write(
            temp_root.join("Cargo.toml"),
            "[package]\nname = \"demo_ws\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write Cargo.toml");
        let main_rs = src_dir.join("main.rs");
        std::fs::write(&main_rs, "fn main() { println!(\"hello\"); }\n")
            .expect("write main.rs");

        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let initial_root = state.workspace_root.clone();
        state.switch_workspace(temp_root.clone());

        let expected_clean = clean_workspace_path(temp_root.clone());
        assert_eq!(state.workspace_root, expected_clean);
        assert_eq!(state.workspace_name(), unique);
        assert!(!state.workspace_badge().is_empty());
        assert!(state.take_workspace_title_dirty());
        assert!(!state.take_workspace_title_dirty());
        assert!(state.recent_workspaces.contains(&expected_clean));
        assert!(state.recent_workspaces.contains(&initial_root));
        assert!(state
            .open_buffers
            .iter()
            .any(|b| b.path.ends_with("main.rs")));

        let mut canvas = SurfaceCanvas::new(1280, 800);
        state.render(&mut canvas);

        let _ = std::fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn ui_language_toggle_and_open_folder_hit_regions_work() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1440, 900);

        assert_eq!(state.ui_lang, UiLanguage::ZhCn);
        state.execute_ide_action(IdeAction::OpenSettingsModal);
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::SetUiLanguage(UiLanguage::ZhCn))));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::SetUiLanguage(UiLanguage::EnUs))));

        state.execute_ide_action(IdeAction::SwitchUiLanguage);
        assert_eq!(state.ui_lang, UiLanguage::EnUs);
        assert!(state.status_notification.contains("English"));

        state.focus = FocusTarget::Editor;
        state.project_popup_open = true;
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::OpenFolderDialog)));

        state.project_popup_open = false;
        state.more_menu_popup_open = true;
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::OpenFolderDialog)));

        state.more_menu_popup_open = false;
        state.active_menu_category = Some(MainMenuCategory::File);
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ExecuteIdeAction(IdeAction::OpenFolder))));
    }

    #[test]
    fn hidpi_scaling_and_ui_zoom_render_and_map_coordinates_accurately() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        state.system_dpi_scale = 1.5;
        let mut canvas = SurfaceCanvas::new_with_scale(2340, 1410, state.effective_scale());
        assert_eq!(canvas.phys_width, 2340);
        assert_eq!(canvas.phys_height, 1410);
        assert_eq!(canvas.width, 1560);
        assert_eq!(canvas.height, 940);

        // Physical <-> Logical coordinate conversion
        assert_eq!(canvas.to_logical_pos(300.0, 150.0), (200, 100));
        assert_eq!(canvas.to_physical_pos(200, 100), (300, 150));

        // Render menus and settings at 150% DPI
        state.hamburger_menu_expanded = true;
        state.active_menu_category = Some(MainMenuCategory::View);
        state.render(&mut canvas);
        assert_eq!(canvas.pixels().len(), 2340 * 1410);

        // UI Zoom In / Out / Reset
        state.execute_ide_action(IdeAction::ZoomInUi);
        assert!((state.ui_scale - 1.10).abs() < 0.01);
        assert!((state.effective_scale() - 1.65).abs() < 0.01);
        canvas.set_scale_factor(state.effective_scale());
        state.render(&mut canvas);

        state.execute_ide_action(IdeAction::ResetUiZoom);
        assert!((state.ui_scale - 1.0).abs() < 0.01);
        canvas.set_scale_factor(state.effective_scale());
        assert_eq!(canvas.width, 1560);
        assert_eq!(canvas.height, 940);
    }

    #[test]
    fn fast_scrolling_and_glyph_mask_cache_achieve_high_fps() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1560, 940);
        // Warm up initial glyph mask and syntax caches
        state.render(&mut canvas);

        let ed_x = state.editor_code_rect.x + 100;
        let ed_y = state.editor_code_rect.y + 100;

        // Verify O(1) HitTestPoint caret column accuracy
        let col0 = canvas.measure_caret_col_in_line("fn hello_world() {}", 0, FontStyle::CodeMono);
        let col_end =
            canvas.measure_caret_col_in_line("fn hello_world() {}", 500, FontStyle::CodeMono);
        assert_eq!(col0, 0);
        assert_eq!(col_end, 19);

        // Simulate 20 rapid scroll frames + horizontal scroll
        let t0 = Instant::now();
        for i in 0..20 {
            let dy = if i % 2 == 0 { 6 } else { -4 };
            let dx = if i % 4 == 0 { 24 } else { 0 };
            state.handle_mouse_wheel_2d(ed_x, ed_y, dy, dx);
            state.render(&mut canvas);
        }
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let avg_frame_ms = elapsed_ms / 20.0;
        println!(
            "20 editor scroll frames rendered in {:.2} ms ({:.2} ms/frame, ~{:.0} FPS in debug build)",
            elapsed_ms,
            avg_frame_ms,
            1000.0 / avg_frame_ms.max(0.1)
        );
        assert!(avg_frame_ms < 50.0);
    }

    #[test]
    fn editor_gutter_expands_for_five_digit_line_numbers_without_truncation() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut big_text = String::with_capacity(120_000);
        for i in 1..=12500 {
            big_text.push_str(&format!("let line_{i} = {i};\n"));
        }
        let big_buf = TextBuffer::from_text(PathBuf::from("big_file.rs"), &big_text);
        state.open_buffers.push(big_buf);
        state.active_tab = state.open_buffers.len() - 1;
        state.scroll_line = 12450;

        let mut canvas = SurfaceCanvas::new(1560, 940);
        state.render(&mut canvas);

        // 5-digit line number (12500) must widen the gutter well beyond the old hardcoded 62px
        let gutter_width = state.editor_code_rect.x - 1 - state.editor_pane_rect.x;
        assert!(
            gutter_width >= 78,
            "expected expanded gutter width >= 78 for 5-digit line numbers, got {gutter_width}"
        );
        // Code text starts cleanly to the right of gutter with padding
        assert!(state.editor_code_start_x > state.editor_code_rect.x + 8);
    }

    #[test]
    fn idea_git_commit_panel_and_history_log_render_and_filter() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        state.left_tool = Some(ToolWindowId::Commit);
        state.bottom_tool = Some(ToolWindowId::GitLog);
        let mut canvas = SurfaceCanvas::new(1560, 940);
        state.render(&mut canvas);

        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ToggleGitAmend)));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::CommitAndPushGitChanges)));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::FocusGitLogSearchInput)));
        assert!(state.git_log_commits_rect.w > 200);
        assert!(state.git_log_branches_rect.w >= 150);

        if let Some(first_hash) = state
            .git_engine
            .recent_commits
            .first()
            .map(|c| c.hash.clone())
        {
            assert_eq!(state.selected_git_commit_hash.as_deref(), Some(first_hash.as_str()));
            assert!(state.git_engine.commit_details.contains_key(&first_hash));
        }

        // Toggle amend and render branch popup
        state.execute_ide_action(IdeAction::ToggleCommitWindow);
        state.git_branch_popup_open = true;
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::CreateNewGitBranchPrompt)));
    }

    #[test]
    fn project_switch_offers_new_window_and_loading_animations_ease_smoothly() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1560, 940);

        // 1. Project popup exposes Open Folder in New Window + Recent Workspace actions
        state.recent_workspaces.push(PathBuf::from("."));
        let second_ws = std::env::temp_dir();
        state.recent_workspaces.push(second_ws.clone());
        state.project_popup_open = true;
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::OpenFolderInNewWindowDialog)));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::PromptOpenWorkspace(_))));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::OpenWorkspaceInNewWindow(_))));

        // 2. Clicking a workspace opens OpenProjectChoiceModal with This Window / New Window buttons
        state.project_popup_open = false;
        state.pending_open_workspace = Some(second_ws);
        state.focus = FocusTarget::OpenProjectChoiceModal;
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ConfirmOpenPendingInThisWindow)));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ConfirmOpenPendingInNewWindow)));

        // 3. Git Commit & Git Log loading state before initial snapshot arrives + smooth fade easing
        state.focus = FocusTarget::Editor;
        state.pending_open_workspace = None;
        state.git_engine.has_loaded_snapshot = false;
        state.startup_load_alpha = 0.85;
        state.git_load_alpha = 0.85;
        state.left_tool = Some(ToolWindowId::Commit);
        state.bottom_tool = Some(ToolWindowId::GitLog);
        state.render(&mut canvas);

        // Once snapshot arrives (wakeup_enabled=false), step_animations eases alphas cleanly to 0
        state.git_engine.has_loaded_snapshot = true;
        for _ in 0..35 {
            state.last_anim_tick = Instant::now() - Duration::from_millis(25);
            state.step_animations();
        }
        assert_eq!(state.startup_load_alpha, 0.0);
        assert_eq!(state.git_load_alpha, 0.0);
        assert!(!state.has_active_animations());
    }

    #[test]
    fn agent_panel_fast_thinking_copy_and_clickable_file_links_work() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1560, 940);
        state.right_agent_open = true;
        state.right_panel_tab = RightPanelTab::Chat;
        state.agent_hub.active_provider = AgentProviderKind::CursorCli;

        let cargo_toml = state.workspace_root.join("Cargo.toml");
        state.agent_hub.messages.push(ide_agent_hub::AgentChatMessage {
            role: "assistant".to_string(),
            agent: AgentProviderKind::CursorCli,
            content: "已更新 Cargo.toml:3 的工作区成员：\n```rust\nfn verify_agent_ui() -> bool { true }\n```\n请检查实现。".to_string(),
            thought: Some("Checked workspace Cargo.toml lines".to_string()),
            checkpoint_id: None,
            patch: None,
            modified_files: vec![cargo_toml.clone()],
            attachments: Vec::new(),
        });

        state.render(&mut canvas);

        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ToggleAgentFastMode)));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::CycleAgentReasoningEffort)));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::CopyAgentMessage(_))));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::CopyAgentCodeBlock(code) if code.contains("verify_agent_ui"))));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::OpenFileAtLine(p, _) if p.ends_with("Cargo.toml"))));

        // 点击 Fast Mode 与 思考深度胶囊切换状态
        if let Some((rect, _)) = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::ToggleAgentFastMode))
            .cloned()
        {
            let prev_fast = state
                .agent_hub
                .providers
                .iter()
                .find(|p| p.kind == AgentProviderKind::CursorCli)
                .map(|p| p.fast_mode)
                .unwrap_or(false);
            state.handle_mouse_click_with_mods(rect.x + 2, rect.y + 2, false, &canvas);
            let next_fast = state
                .agent_hub
                .providers
                .iter()
                .find(|p| p.kind == AgentProviderKind::CursorCli)
                .map(|p| p.fast_mode)
                .unwrap_or(false);
            assert_ne!(prev_fast, next_fast);
        }

        // 点击关联文件链并跳转到第 3 行（0-based 行号为 2）
        state.open_file_at_line(cargo_toml.clone(), Some(3));
        assert!(state.active_buffer().path.ends_with("Cargo.toml"));
        assert_eq!(state.active_buffer().cursor_line, 2);

        // 对话流全选与复制选区 + 右键菜单动作
        state.focus = FocusTarget::AgentChatStream;
        assert!(state.select_all_at_focus());
        let copied = state.copy_focused_selection().expect("chat stream selection");
        assert!(copied.contains("Cargo.toml:3") || copied.contains("verify_agent_ui"));

        let chat_rect = state.agent_chat_area_rect;
        state.open_context_menu_at(chat_rect.x + 12, chat_rect.y + 12, &canvas);
        let menu = state.context_menu.as_ref().expect("agent chat context menu");
        assert!(menu
            .items
            .iter()
            .any(|i| matches!(i.action, ContextMenuAction::AgentCopySelection)));
        assert!(menu
            .items
            .iter()
            .any(|i| matches!(i.action, ContextMenuAction::AgentCopyAllChat)));
    }

    #[test]
    fn agent_sessions_drag_drop_image_preview_and_collapsible_code_work() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1560, 940);
        state.right_agent_open = true;
        state.right_panel_tab = RightPanelTab::Chat;
        state.render(&mut canvas);

        // 1. 新建会话 + 切换会话弹窗
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::NewAgentSession)));
        state.create_new_agent_session();
        assert_eq!(state.agent_hub.sessions.len(), 2);
        assert_eq!(state.agent_hub.active_session_idx, 1);
        assert!(state.agent_hub.messages.is_empty());

        state.agent_sessions_popup_open = true;
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::SelectAgentSession(0))));
        state.agent_sessions_popup_open = false;

        // 2. 外部拖拽图片 + 内部拖拽工作区文件到智能体面板，并验证图片直接预览与弹窗放大
        let mut rgba = Vec::with_capacity(24 * 18 * 4);
        for y in 0..18u8 {
            for x in 0..24u8 {
                rgba.extend_from_slice(&[x * 10, y * 12, 210, 255]);
            }
        }
        let png_path = crate::image_preview::encode_rgba_to_temp_png(24, 18, &rgba)
            .expect("create temp test png");
        let drop_x = state.right_pane_rect.x + 24;
        let drop_y = state.right_pane_rect.y + 120;
        state.handle_os_dropped_file(png_path.clone(), drop_x, drop_y);

        let cargo_toml = state.workspace_root.join("Cargo.toml");
        state.pending_ide_drag = Some((cargo_toml.clone(), (20, 20)));
        assert!(state.update_ide_drag(drop_x, drop_y));
        assert!(state.finish_ide_drag_at(drop_x, drop_y));

        assert_eq!(state.agent_hub.active_session().attached_items.len(), 2);
        assert!(state.image_preview_cache.contains_key(&clean_workspace_path(png_path.clone())));

        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::OpenImagePreview(_))));

        // 打开图片放大预览弹窗并渲染关闭
        state.open_image_preview(clean_workspace_path(png_path.clone()));
        assert_eq!(state.focus, FocusTarget::ImagePreviewModal);
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::CloseImagePreview)));
        state.close_image_preview();
        assert_ne!(state.focus, FocusTarget::ImagePreviewModal);

        // 3. 粘贴多行代码自动折叠 -> 展开预览 -> 发送后聊天区默认折叠 -> 点击展开高亮显示
        let code = "fn sample_demo(a: i32, b: i32) -> i32 {\n    let sum = a + b;\n    let mul = a * b;\n    println!(\"res={}\", sum + mul);\n    sum + mul\n}";
        let (title, lang, lc) =
            detect_pasted_code_snippet(code).expect("detect multi-line pasted rust code");
        state
            .agent_hub
            .add_pasted_snippet_to_active_session(PastedCodeSnippet {
                id: "test-snip-1".to_string(),
                title,
                language: lang,
                content: code.to_string(),
                line_count: lc,
            });
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ToggleComposerPastedSnippet(id) if id == "test-snip-1")));

        state.expanded_composer_snippet_id = Some("test-snip-1".to_string());
        state.render(&mut canvas);

        state.submit_agent_prompt("/search ide_core".to_string());
        state.render(&mut canvas);

        assert!(state
            .cached_chat_rows
            .rows
            .iter()
            .any(|r| matches!(r, crate::agent_panel::CachedChatRow::UserImageCard { .. })));
        assert!(state
            .cached_chat_rows
            .rows
            .iter()
            .any(|r| matches!(r, crate::agent_panel::CachedChatRow::UserFilesRow { .. })));
        assert!(state
            .cached_chat_rows
            .rows
            .iter()
            .any(|r| matches!(r, crate::agent_panel::CachedChatRow::CodeBlockCollapsedSummary { .. })));

        // 向上滚动至顶部消息，直接点击展开折叠的代码块，确认语法高亮行渲染出来
        state.agent_scroll = 0;
        state.render(&mut canvas);
        let (btn_rect, _) = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::ToggleCodeBlockCollapse(0, 0)))
            .cloned()
            .expect("collapsed code block header hit region");
        state.handle_mouse_click_with_mods(btn_rect.x + 4, btn_rect.y + 4, false, &canvas);
        state.render(&mut canvas);
        assert!(state
            .cached_chat_rows
            .rows
            .iter()
            .any(|r| matches!(r, crate::agent_panel::CachedChatRow::CodeBlockLine { .. })));

        let _ = std::fs::remove_file(&png_path);
    }

    #[test]
    fn agent_mode_dropdown_adapts_to_cli_support_and_markdown_bubbles_render() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1560, 940);
        state.right_agent_open = true;
        state.right_panel_tab = RightPanelTab::Chat;

        // 1. Antigravity CLI 支持 Agent / Edit / Ask 三种模式，点击模式按钮应打开下拉菜单而非仅循环
        state.agent_hub.active_provider = AgentProviderKind::GoogleAntigravityAcp;
        state.render(&mut canvas);
        let (mode_btn_rect, _) = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::ToggleAgentModePopup))
            .cloned()
            .expect("mode dropdown button");
        state.handle_mouse_click_with_mods(
            mode_btn_rect.x + 4,
            mode_btn_rect.y + 4,
            false,
            &canvas,
        );
        assert!(state.agent_mode_popup_open);

        state.render(&mut canvas);
        let agy_modes: Vec<AgentWorkbenchMode> = state
            .hit_regions
            .iter()
            .filter_map(|(_, a)| match a {
                HitAction::SelectAgentMode(m) => Some(*m),
                _ => None,
            })
            .collect();
        assert_eq!(
            agy_modes,
            vec![
                AgentWorkbenchMode::Agent,
                AgentWorkbenchMode::Edit,
                AgentWorkbenchMode::Ask,
            ]
        );

        // 点击 Edit 项并确认选中且下拉关闭
        let (edit_row, _) = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::SelectAgentMode(AgentWorkbenchMode::Edit)))
            .cloned()
            .expect("Edit option in Antigravity mode dropdown");
        state.handle_mouse_click_with_mods(edit_row.x + 6, edit_row.y + 6, false, &canvas);
        assert_eq!(state.agent_mode, AgentWorkbenchMode::Edit);
        assert!(!state.agent_mode_popup_open);

        // 2. 切换至不支持 Edit 的 Codex CLI 时，自动回落为 Agent，且下拉中仅提供 Agent / Ask
        state.handle_mouse_click_with_mods(
            mode_btn_rect.x + 4,
            mode_btn_rect.y + 4,
            false,
            &canvas,
        );
        state.agent_provider_popup_open = true;
        state.render(&mut canvas);
        let (codex_row, _) = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::SelectAgentProvider(AgentProviderKind::OpenAiCodexCli)))
            .cloned()
            .expect("Codex CLI provider option");
        state.handle_mouse_click_with_mods(codex_row.x + 6, codex_row.y + 6, false, &canvas);
        assert_eq!(state.agent_hub.active_provider, AgentProviderKind::OpenAiCodexCli);
        assert_eq!(state.agent_mode, AgentWorkbenchMode::Agent);

        state.render(&mut canvas);
        let (codex_mode_btn, _) = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::ToggleAgentModePopup))
            .cloned()
            .expect("Codex mode button");
        state.handle_mouse_click_with_mods(
            codex_mode_btn.x + 4,
            codex_mode_btn.y + 4,
            false,
            &canvas,
        );
        assert!(state.agent_mode_popup_open);
        state.render(&mut canvas);
        let codex_modes: Vec<AgentWorkbenchMode> = state
            .hit_regions
            .iter()
            .filter_map(|(_, a)| match a {
                HitAction::SelectAgentMode(m) => Some(*m),
                _ => None,
            })
            .collect();
        assert_eq!(
            codex_modes,
            vec![AgentWorkbenchMode::Agent, AgentWorkbenchMode::Ask]
        );

        // 3. 验证用户气泡、行内代码胶囊与助手 Markdown 排版可正确构建行缓存与选中文本
        state.agent_mode_popup_open = false;
        state.agent_hub.messages.push(ide_agent_hub::AgentChatMessage {
            role: "user".to_string(),
            agent: AgentProviderKind::CursorCli,
            content: "后台短剧编辑，提审方 appid 默认改为 `wxae3b81ca05d7b0e0`".to_string(),
            thought: None,
            checkpoint_id: None,
            patch: None,
            modified_files: Vec::new(),
            attachments: Vec::new(),
        });
        state.agent_hub.messages.push(ide_agent_hub::AgentChatMessage {
            role: "assistant".to_string(),
            agent: AgentProviderKind::CursorCli,
            content: "请重新导入 `Cargo.toml` 查看 **配置项**。".to_string(),
            thought: Some("Checked appid field".to_string()),
            checkpoint_id: None,
            patch: None,
            modified_files: Vec::new(),
            attachments: Vec::new(),
        });
        state.render(&mut canvas);
        assert!(state
            .cached_chat_rows
            .rows
            .iter()
            .any(|r| matches!(r, crate::agent_panel::CachedChatRow::UserBubbleTop { .. })));
        assert!(state.cached_chat_rows.rows.iter().any(|r| match r {
            crate::agent_panel::CachedChatRow::UserLine { text, spans, .. } => {
                text.contains("wxae3b81ca05d7b0e0")
                    && spans
                        .iter()
                        .any(|s| matches!(s.kind, crate::agent_panel::ChatSpanKind::InlineCode))
            }
            _ => false,
        }));
        assert!(state.cached_chat_rows.rows.iter().any(|r| match r {
            crate::agent_panel::CachedChatRow::AgentLine { text, spans, .. } => {
                text.contains("配置项")
                    && spans
                        .iter()
                        .any(|s| matches!(s.kind, crate::agent_panel::ChatSpanKind::Bold))
            }
            _ => false,
        }));
    }
}
