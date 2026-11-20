use crate::agent_panel::{AgentChatCache, AGENT_INPUT_FONT, AGENT_INPUT_LINE_H};
use crate::canvas::{FontStyle, Rect, SurfaceCanvas};
use crate::image_preview::{
    detect_media_kind, detect_pasted_code_snippet, format_media_duration, format_media_file_size,
    is_image_file_path, load_image_for_preview, load_media_preview_tab, open_media_with_system_app,
    read_windows_clipboard_payload, ClipboardPastePayload, DecodedPreviewImage, MediaKind,
    MediaPreviewTab,
};
use crate::persistence::{
    self, PersistedGlobalSettings, PersistedOpenTab, PersistedProviderPref, PersistedWorkspaceState,
};
use crate::text_input::TextInputField;
use ide_agent_hub::{
    parse_path_and_line_token, AgentAttachedItem, AgentHub, AgentProviderKind, AgentWorkbenchMode,
    AiApiFormat, IdeCodeContext, PastedCodeSnippet,
};
use ide_core::{
    detect_mobile_project, probe_mobile_environment, IdeAction, IdeaTheme, MainMenuCategory,
    MenuEntry, MobileDeviceState, MobileEnvironmentReport, MobileProjectKind, MobileSdkLocations,
    RunConfigSource, RunConfiguration, ThemeKind, ToolWindowId, UiLanguage,
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
use serde::{Deserialize, Serialize};
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
    /// 0 = Flutter SDK, 1 = Android SDK, 2 = JDK.
    MobileSdk(u8),
}

fn sibling_toolchain_bin(bin: &Path, names: &[&str]) -> Option<PathBuf> {
    let dir = bin.parent()?;
    names
        .iter()
        .map(|name| dir.join(name))
        .find(|candidate| candidate.is_file())
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

pub(crate) fn workspace_paths_equal(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    #[cfg(windows)]
    {
        a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Counts repeated attempts to open a project that is already on screen.
/// The first attempt focuses the window; the second and later attempts flash it.
pub(crate) fn duplicate_open_count<K: PartialEq>(
    previous: &Option<(K, u32, Instant)>,
    key: &K,
    now: Instant,
) -> u32 {
    match previous {
        Some((prev_key, count, at))
            if prev_key == key && now.duration_since(*at) <= Duration::from_secs(8) =>
        {
            count.saturating_add(1)
        }
        _ => 1,
    }
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

/// 可在左 / 右 / 底三条边之间拖动交换的工作区。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum DockPane {
    Project = 0,
    Agent = 1,
    Tooling = 2,
}

impl Default for DockPane {
    fn default() -> Self {
        Self::Project
    }
}

impl DockPane {
    fn title(self, zh: bool) -> &'static str {
        match (self, zh) {
            (Self::Project, true) => "项目",
            (Self::Project, false) => "Project",
            (Self::Agent, true) => "AI 助手",
            (Self::Agent, false) => "AI Assistant",
            (Self::Tooling, true) => "工具",
            (Self::Tooling, false) => "Tools",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DockEdge {
    Left,
    Right,
    Bottom,
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
    CenterEditorHoriz,
    RightPanel,
    BottomTerminal,
    GitLogCommits,
    GitLogDiff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone)]
pub struct ActiveEditorDiff {
    pub path: PathBuf,
    pub subtitle: String,
    pub diff_lines: Vec<ide_git::DiffLine>,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub scroll: usize,
    pub scroll_x: i32,
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
    RunFile(PathBuf),
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
    TermAddLastCommandToRunList,
    OpenNewRunConfigModal,
    // IDEA Git 上下文菜单操作
    GitToggleCommitCheck(PathBuf),
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
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
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

/// Settings 对话框页面，结构对齐 IntelliJ IDEA `Ctrl+Alt+S`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingsPage {
    Appearance,
    MenusAndToolbars,
    SystemSettings,
    EditorGeneral,
    EditorFont,
    ColorScheme,
    CodeStyle,
    Keymap,
    Plugins,
    Git,
    Debugger,
    Terminal,
    Languages,
    MobileSdk,
    Advanced,
}

impl SettingsPage {
    fn title(self, lang: UiLanguage) -> &'static str {
        match (self, lang) {
            (Self::Appearance, UiLanguage::ZhCn) => "外观",
            (Self::Appearance, UiLanguage::EnUs) => "Appearance",
            (Self::MenusAndToolbars, UiLanguage::ZhCn) => "菜单和工具栏",
            (Self::MenusAndToolbars, UiLanguage::EnUs) => "Menus and Toolbars",
            (Self::SystemSettings, UiLanguage::ZhCn) => "系统设置",
            (Self::SystemSettings, UiLanguage::EnUs) => "System Settings",
            (Self::EditorGeneral, UiLanguage::ZhCn) => "常规",
            (Self::EditorGeneral, UiLanguage::EnUs) => "General",
            (Self::EditorFont, UiLanguage::ZhCn) => "字体",
            (Self::EditorFont, UiLanguage::EnUs) => "Font",
            (Self::ColorScheme, UiLanguage::ZhCn) => "配色方案",
            (Self::ColorScheme, UiLanguage::EnUs) => "Color Scheme",
            (Self::CodeStyle, UiLanguage::ZhCn) => "代码样式",
            (Self::CodeStyle, UiLanguage::EnUs) => "Code Style",
            (Self::Keymap, UiLanguage::ZhCn) => "按键映射",
            (Self::Keymap, UiLanguage::EnUs) => "Keymap",
            (Self::Plugins, UiLanguage::ZhCn) => "插件",
            (Self::Plugins, UiLanguage::EnUs) => "Plugins",
            (Self::Git, UiLanguage::ZhCn) => "Git",
            (Self::Git, UiLanguage::EnUs) => "Git",
            (Self::Debugger, UiLanguage::ZhCn) => "调试器",
            (Self::Debugger, UiLanguage::EnUs) => "Debugger",
            (Self::Terminal, UiLanguage::ZhCn) => "终端",
            (Self::Terminal, UiLanguage::EnUs) => "Terminal",
            (Self::Languages, UiLanguage::ZhCn) => "语言和框架",
            (Self::Languages, UiLanguage::EnUs) => "Languages & Frameworks",
            (Self::MobileSdk, UiLanguage::ZhCn) => "移动端 SDK",
            (Self::MobileSdk, UiLanguage::EnUs) => "Mobile SDKs",
            (Self::Advanced, UiLanguage::ZhCn) => "高级设置",
            (Self::Advanced, UiLanguage::EnUs) => "Advanced Settings",
        }
    }

    fn breadcrumb(self, lang: UiLanguage) -> &'static str {
        match (self, lang) {
            (Self::Appearance, UiLanguage::ZhCn) => "外观与行为  /  外观",
            (Self::Appearance, UiLanguage::EnUs) => "Appearance & Behavior  /  Appearance",
            (Self::MenusAndToolbars, UiLanguage::ZhCn) => "外观与行为  /  菜单和工具栏",
            (Self::MenusAndToolbars, UiLanguage::EnUs) => {
                "Appearance & Behavior  /  Menus and Toolbars"
            }
            (Self::SystemSettings, UiLanguage::ZhCn) => "外观与行为  /  系统设置",
            (Self::SystemSettings, UiLanguage::EnUs) => "Appearance & Behavior  /  System Settings",
            (Self::EditorGeneral, UiLanguage::ZhCn) => "编辑器  /  常规",
            (Self::EditorGeneral, UiLanguage::EnUs) => "Editor  /  General",
            (Self::EditorFont, UiLanguage::ZhCn) => "编辑器  /  字体",
            (Self::EditorFont, UiLanguage::EnUs) => "Editor  /  Font",
            (Self::ColorScheme, UiLanguage::ZhCn) => "编辑器  /  配色方案",
            (Self::ColorScheme, UiLanguage::EnUs) => "Editor  /  Color Scheme",
            (Self::CodeStyle, UiLanguage::ZhCn) => "编辑器  /  代码样式",
            (Self::CodeStyle, UiLanguage::EnUs) => "Editor  /  Code Style",
            (Self::Keymap, UiLanguage::ZhCn) => "按键映射",
            (Self::Keymap, UiLanguage::EnUs) => "Keymap",
            (Self::Plugins, UiLanguage::ZhCn) => "插件",
            (Self::Plugins, UiLanguage::EnUs) => "Plugins",
            (Self::Git, UiLanguage::ZhCn) => "版本控制  /  Git",
            (Self::Git, UiLanguage::EnUs) => "Version Control  /  Git",
            (Self::Debugger, UiLanguage::ZhCn) => "构建、执行、部署  /  调试器",
            (Self::Debugger, UiLanguage::EnUs) => "Build, Execution, Deployment  /  Debugger",
            (Self::Terminal, UiLanguage::ZhCn) => "工具  /  终端",
            (Self::Terminal, UiLanguage::EnUs) => "Tools  /  Terminal",
            (Self::Languages, UiLanguage::ZhCn) => "语言和框架",
            (Self::Languages, UiLanguage::EnUs) => "Languages & Frameworks",
            (Self::MobileSdk, UiLanguage::ZhCn) => "语言和框架  /  移动端 SDK",
            (Self::MobileSdk, UiLanguage::EnUs) => "Languages & Frameworks  /  Mobile SDKs",
            (Self::Advanced, UiLanguage::ZhCn) => "高级设置",
            (Self::Advanced, UiLanguage::EnUs) => "Advanced Settings",
        }
    }
}

#[derive(Clone, Copy)]
struct SettingsNavItem {
    page: Option<SettingsPage>,
    depth: u8,
    zh: &'static str,
    en: &'static str,
    keywords: &'static str,
}

const SETTINGS_NAV: &[SettingsNavItem] = &[
    SettingsNavItem {
        page: None,
        depth: 0,
        zh: "外观与行为",
        en: "Appearance & Behavior",
        keywords: "appearance behavior 外观 行为",
    },
    SettingsNavItem {
        page: Some(SettingsPage::Appearance),
        depth: 1,
        zh: "外观",
        en: "Appearance",
        keywords: "appearance theme zoom scale language ui dpi 外观 主题 缩放 语言 字体",
    },
    SettingsNavItem {
        page: Some(SettingsPage::MenusAndToolbars),
        depth: 1,
        zh: "菜单和工具栏",
        en: "Menus and Toolbars",
        keywords: "menu toolbar 菜单 工具栏",
    },
    SettingsNavItem {
        page: Some(SettingsPage::SystemSettings),
        depth: 1,
        zh: "系统设置",
        en: "System Settings",
        keywords: "system project workspace recent 系统 项目 工作区 最近打开",
    },
    SettingsNavItem {
        page: None,
        depth: 0,
        zh: "编辑器",
        en: "Editor",
        keywords: "editor 编辑器",
    },
    SettingsNavItem {
        page: Some(SettingsPage::EditorGeneral),
        depth: 1,
        zh: "常规",
        en: "General",
        keywords: "editor general indent comment caret 常规 缩进 注释 光标",
    },
    SettingsNavItem {
        page: Some(SettingsPage::EditorFont),
        depth: 1,
        zh: "字体",
        en: "Font",
        keywords: "font size zoom cascadia yahei 字体 字号 缩放",
    },
    SettingsNavItem {
        page: Some(SettingsPage::ColorScheme),
        depth: 1,
        zh: "配色方案",
        en: "Color Scheme",
        keywords: "color scheme syntax highlight darcula islands 配色 语法 高亮 颜色",
    },
    SettingsNavItem {
        page: Some(SettingsPage::CodeStyle),
        depth: 1,
        zh: "代码样式",
        en: "Code Style",
        keywords: "code style indent tab reformat 代码样式 缩进 制表符 格式化",
    },
    SettingsNavItem {
        page: Some(SettingsPage::Keymap),
        depth: 0,
        zh: "按键映射",
        en: "Keymap",
        keywords: "keymap shortcut key binding 按键 快捷键 intellij",
    },
    SettingsNavItem {
        page: Some(SettingsPage::Plugins),
        depth: 0,
        zh: "插件",
        en: "Plugins",
        keywords: "plugin extension lsp 插件 扩展",
    },
    SettingsNavItem {
        page: None,
        depth: 0,
        zh: "版本控制",
        en: "Version Control",
        keywords: "version control vcs 版本控制",
    },
    SettingsNavItem {
        page: Some(SettingsPage::Git),
        depth: 1,
        zh: "Git",
        en: "Git",
        keywords: "git branch commit push pull checkout 分支 提交 推送 拉取",
    },
    SettingsNavItem {
        page: None,
        depth: 0,
        zh: "构建、执行、部署",
        en: "Build, Execution, Deployment",
        keywords: "build execution deployment 构建 执行 部署",
    },
    SettingsNavItem {
        page: Some(SettingsPage::Debugger),
        depth: 1,
        zh: "调试器",
        en: "Debugger",
        keywords: "debug breakpoint dap codelldb 调试 断点",
    },
    SettingsNavItem {
        page: None,
        depth: 0,
        zh: "工具",
        en: "Tools",
        keywords: "tools 工具",
    },
    SettingsNavItem {
        page: Some(SettingsPage::Terminal),
        depth: 1,
        zh: "终端",
        en: "Terminal",
        keywords: "terminal shell console pty 终端 控制台",
    },
    SettingsNavItem {
        page: Some(SettingsPage::Languages),
        depth: 0,
        zh: "语言和框架",
        en: "Languages & Frameworks",
        keywords: "language rust typescript javascript python framework 语言 框架",
    },
    SettingsNavItem {
        page: Some(SettingsPage::MobileSdk),
        depth: 0,
        zh: "移动端 SDK",
        en: "Mobile SDKs",
        keywords: "mobile sdk flutter android jdk adb install directory path 移动端 sdk 安装目录 flutter android",
    },
    SettingsNavItem {
        page: Some(SettingsPage::Advanced),
        depth: 0,
        zh: "高级设置",
        en: "Advanced Settings",
        keywords: "advanced dpi scale new ui 高级 缩放",
    },
];

struct SettingsFlow {
    origin_y: i32,
    view_h: i32,
    scroll_px: i32,
    cursor: i32,
}

impl SettingsFlow {
    fn place(&mut self, h: i32, gap: i32) -> Option<i32> {
        let top = self.cursor;
        self.cursor += h + gap;
        let sy = self.origin_y + top - self.scroll_px;
        if sy < self.origin_y || sy + h > self.origin_y + self.view_h {
            None
        } else {
            Some(sy)
        }
    }
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
    ToggleRunConfigAt(usize),
    RemoveRunConfig(usize),
    OpenNewRunConfigModal,
    ToggleMobilePopup,
    RefreshMobileEnv,
    SelectMobileDevice(String),
    LaunchMobileEmulator(String),
    RunMobileCommand(String),
    InstallMobileSdkCmd(String, String),
    OpenMobileDownloadUrl(String),
    OpenMobileSdkSettings,
    FocusMobileSdkPath(u8),
    BrowseMobileSdkDir(u8),
    ToggleMoreMenuPopup,
    ToggleGitBranchPopup,
    CheckoutGitBranch(String),
    CreateNewGitBranchPrompt,
    // 编辑器与标签页
    SelectTab(usize),
    CloseTab(usize),
    DragDockPane(DockPane),
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
    ToggleCommitFileCheck(PathBuf),
    ToggleCommitGroupCheck { untracked_group: bool },
    ToggleCommitGroupCollapse { untracked_group: bool },
    InspectSelectedGitDiff,
    RollbackCheckedGitChanges,
    CommitAllGitChanges,
    CommitAndPushGitChanges,
    ToggleGitAmend,
    GenerateAiCommitMessage,
    RollbackGitFile(PathBuf),
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
    GitMergeAbort,
    GitRebaseBranch(String),
    GitRebaseContinue,
    GitRebaseAbort,
    GitDeleteBranch(String),
    GitStashPop(String),
    GitStashApply(String),
    GitStashDrop(String),
    GitResetTo {
        target: String,
        mode: ide_git::GitResetMode,
    },
    SelectGitResetMode(ide_git::GitResetMode),
    GitRemoveRemote(String),
    OpenAddRemoteModal,
    GitConfirmRollbackAll,
    CloseModalToEditor,
    ClearGitFileHistoryFilter,
    InspectBlameCommit(String),
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
    ToggleMessageFileChangesCollapse(usize),
    InspectAgentFileDiff {
        path: PathBuf,
        msg_idx: Option<usize>,
    },
    CloseEditorDiff,
    OpenDiffSourceFile(PathBuf),
    SelectMediaTab(usize),
    CloseMediaTab(usize),
    OpenMediaAsText(PathBuf),
    OpenExternalMedia(PathBuf),
    AdjustMediaZoom(i32),
    ToggleComposerPastedSnippet(String),
    RemoveComposerPastedSnippet(String),
    RemoveComposerAttachment(PathBuf),
    OpenImagePreview(PathBuf),
    CloseImagePreview,
    CopyAgentMessage(usize),
    CopyAgentCodeBlock(String),
    OpenFileAtLine(PathBuf, Option<usize>),
    OpenExternalUrl(String),
    FocusAgentChatStream,
    RunAgentQuickPrompt(&'static str),
    ApplyChatPatch(usize),
    InsertChatPatch(usize),
    PreviewPatchInEditor(usize),
    FocusCtrlKPrompt,
    AcceptInlineDiff,
    RejectInlineDiff,
    RevertLastCheckpoint,
    ToggleAgentChangedFilesDrawer,
    KeepAllAgentChangedFiles,
    RevertAllAgentChangedFiles,
    RevertSingleAgentChangedFile(PathBuf),
    StartEditSentUserMessage(usize),
    CancelEditSentUserMessage,
    ConfirmEditMessageRewind { revert_files: bool },
    CancelEditMessageRewindModal,
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
    SelectSettingsPage(SettingsPage),
    FocusSettingsSearch,
    ApplySettings,
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
    NewRunConfigModal,
    NewGitBranchModal,
    NewGitTagModal,
    NewGitStashModal,
    GitAddRemoteModal,
    GitMergeModal,
    GitRebaseModal,
    GitUnstashModal,
    GitResetModal,
    GitRollbackModal,
    GitRemotesModal,
    RenamePathModal,
    SettingsModal,
    AiConfigModal(usize),
    OpenProjectChoiceModal,
    EditMessageRewindModal,
    ImagePreviewModal,
}

#[derive(Debug, Clone)]
pub struct PendingEditMessageRewind {
    pub msg_idx: usize,
    pub new_prompt: String,
    pub dropped_msgs: usize,
    pub affected_files: Vec<PathBuf>,
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
    /// 没有打开文件时给状态栏和动作一个空缓冲，避免下标崩溃。不写入磁盘。
    idle_editor: TextBuffer,
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
    pub selected_git_working_file: Option<PathBuf>,
    pub git_changes_group_collapsed: bool,
    pub git_unversioned_group_collapsed: bool,
    pub git_new_branch_from_ref: Option<String>,
    pub git_blame_visible: bool,
    pub git_file_history_filter: Option<PathBuf>,
    pub git_reset_mode: ide_git::GitResetMode,
    pub modal_scroll: usize,
    pub settings_page: SettingsPage,
    pub mobile_sdk: MobileSdkLocations,
    pub settings_nav_scroll: usize,
    pub settings_nav_max_scroll: usize,
    pub settings_content_max_scroll: usize,
    pub settings_nav_rect: Rect,
    pub settings_content_rect: Rect,
    pub settings_search_rect: Rect,
    pub settings_search_text_x: i32,
    pub settings_query_input: TextInputField,
    pub settings_sdk_focus: Option<u8>,
    mobile_sdk_flutter: TextInputField,
    mobile_sdk_android: TextInputField,
    mobile_sdk_jdk: TextInputField,
    mobile_sdk_field_rects: [Rect; 3],
    // 自定义窗口与悬浮态
    pub hover_pos: (i32, i32),
    pub last_hover_hit_idx: Option<usize>,
    pub last_hover_splitter: Option<DragSplitter>,
    pub last_win_size: (i32, i32),
    pub window_maximized: bool,
    pub pending_window_cmd: Option<WindowControlCmd>,
    pub armed_window_control: Option<WindowControlCmd>,
    // 可拖拽调节的停靠面板尺寸
    pub left_tool: Option<ToolWindowId>,
    pub left_panel_width: i32,
    pub right_agent_open: bool,
    pub right_panel_width: i32,
    pub right_panel_tab: RightPanelTab,
    pub bottom_tool: Option<ToolWindowId>,
    pub bottom_panel_height: i32,
    pub dock_left: DockPane,
    pub dock_right: DockPane,
    pub dock_bottom: DockPane,
    pending_dock_drag: Option<(DockPane, i32, i32)>,
    active_dock_drag: Option<DockPane>,
    dock_drag_pos: (i32, i32),
    dock_drop_edge: Option<DockEdge>,
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
    pub editor_horiz_scroll_info: ScrollTrackInfo,
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
    pub menu_trigger_and_bar_rect: Rect,
    pub menu_dropdown_rect: Rect,
    pub project_popup_open: bool,
    pub run_config_popup_open: bool,
    pub mobile_popup_open: bool,
    pub more_menu_popup_open: bool,
    pub git_branch_popup_open: bool,
    pub agent_provider_popup_open: bool,
    pub agent_model_popup_open: bool,
    pub agent_mode_popup_open: bool,
    pub agent_sessions_popup_open: bool,
    pub agent_changed_files_expanded: bool,
    pub agent_mode: AgentWorkbenchMode,
    pub agent_busy_started_at: Option<Instant>,
    pub agent_provider_pill_rect: Rect,
    pub agent_model_pill_rect: Rect,
    pub agent_mode_pill_rect: Rect,
    pub attach_active_context: bool,
    pub editing_sent_msg_idx: Option<usize>,
    pub editing_msg_saved_draft: String,
    pub pending_edit_rewind: Option<PendingEditMessageRewind>,
    pub context_menu: Option<ContextMenuState>,
    pub context_menu_rect: Rect,
    pub active_overlay_rects: Vec<Rect>,
    pub overlay_hit_start: usize,
    pub rename_target_path: Option<PathBuf>,
    // 子系统引擎
    pub run_configs: Vec<RunConfiguration>,
    pub active_run_config: usize,
    pub run_config_popup_scroll: usize,
    pub run_config_popup_rect: Rect,
    pub mobile_popup_rect: Rect,
    pub mobile_env: MobileEnvironmentReport,
    pub selected_mobile_device_id: Option<String>,
    pub mobile_missing_prompted: bool,
    mobile_probe_rx: Option<mpsc::Receiver<MobileEnvironmentReport>>,
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
    pub expanded_message_file_changes: HashMap<usize, bool>,
    pub expanded_composer_snippet_id: Option<String>,
    pub active_editor_diff: Option<ActiveEditorDiff>,
    pub media_tabs: Vec<MediaPreviewTab>,
    pub active_media_tab: Option<usize>,
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
    pub pending_new_windows: Vec<PathBuf>,
    /// Load into this window unless another window already has the project.
    pub pending_switch_workspaces: Vec<PathBuf>,
    /// Repeating an open of this project pulses the window frame until this instant.
    pub window_attention_until: Option<Instant>,
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
    pub persistence_dir: Option<PathBuf>,
    folder_pick_tx: mpsc::Sender<PathBuf>,
    folder_pick_rx: mpsc::Receiver<PathBuf>,
    workspace_files_cache: Arc<RwLock<Option<Vec<WorkspaceFileEntry>>>>,
    search_seq: Arc<AtomicU64>,
    search_cmd_tx: mpsc::Sender<SearchWorkerCmd>,
    search_event_rx: mpsc::Receiver<AsyncSearchEvent>,
    cached_runnables: Option<(PathBuf, usize, Vec<RunnableGutterEntry>)>,
    cached_structure: Option<(PathBuf, usize, Vec<StructureSymbol>)>,
    cached_highlight_lines: (PathBuf, usize, HashMap<usize, Vec<HighlightSpan>>),
    cached_max_editor_line_w: (PathBuf, usize, i32, i32),
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

fn ensure_search_index(root: &Path, files_cache: &Arc<RwLock<Option<Vec<WorkspaceFileEntry>>>>) {
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
) -> (
    mpsc::Sender<SearchWorkerCmd>,
    mpsc::Receiver<AsyncSearchEvent>,
) {
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
                        let _ =
                            event_tx.send(AsyncSearchEvent::SearchEverywhereReady { seq, items });
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
                if detect_media_kind(&entry.path).is_some() {
                    continue;
                }
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

struct TerminalTextLink {
    start_char: usize,
    end_char: usize,
    action: HitAction,
}

fn terminal_links_in_line(text: &str, workspace: &Path, cwd: &Path) -> Vec<TerminalTextLink> {
    let chars: Vec<char> = text.chars().collect();
    let mut covered = vec![false; chars.len()];
    let mut links = Vec::new();

    let mut i = 0;
    while i < chars.len() {
        let Some(scheme_len) = url_scheme_len(&chars, i) else {
            i += 1;
            continue;
        };
        let mut end = i + scheme_len;
        while end < chars.len() && !is_url_stop(chars[end]) {
            end += 1;
        }
        while end > i + scheme_len && is_url_trailing(chars[end - 1]) {
            end -= 1;
        }
        if end > i + scheme_len {
            let url: String = chars[i..end].iter().collect();
            let lower = url.to_ascii_lowercase();
            if lower.starts_with("https://") || lower.starts_with("http://") {
                links.push(TerminalTextLink {
                    start_char: i,
                    end_char: end,
                    action: HitAction::OpenExternalUrl(url),
                });
                for slot in &mut covered[i..end] {
                    *slot = true;
                }
            }
        }
        i = end.max(i + 1);
    }

    let mut index = 0;
    while index < chars.len() {
        if is_link_token_sep(chars[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len() && !is_link_token_sep(chars[index]) {
            index += 1;
        }
        let mut end = index;
        while end > start && matches!(chars[end - 1], '.' | '。' | '，') {
            end -= 1;
        }
        if end <= start || covered[start..end].iter().any(|flag| *flag) {
            continue;
        }
        let token: String = chars[start..end].iter().collect();
        if let Some(action) = terminal_token_action(&token, workspace, cwd) {
            links.push(TerminalTextLink {
                start_char: start,
                end_char: end,
                action,
            });
        }
    }
    links
}

fn url_scheme_len(chars: &[char], start: usize) -> Option<usize> {
    let rest: String = chars[start..]
        .iter()
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    if rest.starts_with("https://") {
        Some(8)
    } else if rest.starts_with("http://") {
        Some(7)
    } else {
        None
    }
}

fn is_url_stop(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'' | '`' | '|' | ' ')
}

fn is_url_trailing(ch: char) -> bool {
    matches!(
        ch,
        '.' | ','
            | ';'
            | ':'
            | '!'
            | '?'
            | ')'
            | ']'
            | '}'
            | '。'
            | '，'
            | '；'
            | '）'
            | '】'
            | '》'
    )
}

fn is_link_token_sep(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';' | '"' | '\'' | '`' | '|'
        )
}

fn looks_like_path_token(path: &str) -> bool {
    path.contains('.') || path.contains('/') || path.contains('\\')
}

fn terminal_token_action(token: &str, workspace: &Path, cwd: &Path) -> Option<HitAction> {
    if token.len() < 3 {
        return None;
    }
    if let Some(path) = file_url_to_path(token) {
        return Some(HitAction::OpenFileAtLine(path, None));
    }
    let (path_str, line) = parse_path_and_line_token(token)?;
    if !looks_like_path_token(path_str) {
        return None;
    }
    resolve_terminal_link_path(workspace, cwd, path_str)
        .map(|path| HitAction::OpenFileAtLine(path, line))
}

fn file_url_to_path(token: &str) -> Option<PathBuf> {
    let lower = token.to_ascii_lowercase();
    let rest = lower.strip_prefix("file://")?;
    let raw = &token[token.len() - rest.len()..];
    let rest = raw.trim_start_matches('/');
    let rest = rest
        .strip_prefix("localhost/")
        .or_else(|| rest.strip_prefix("localhost\\"))
        .unwrap_or(rest);
    if rest.is_empty() {
        return None;
    }
    let path = PathBuf::from(rest);
    path.is_file().then_some(path)
}

fn resolve_terminal_link_path(workspace: &Path, cwd: &Path, raw: &str) -> Option<PathBuf> {
    let rel = Path::new(raw);
    let candidates = if rel.is_absolute() {
        vec![rel.to_path_buf()]
    } else {
        vec![cwd.join(rel), workspace.join(rel)]
    };
    candidates.into_iter().find(|path| path.is_file())
}

fn open_http_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        use windows::core::PCWSTR;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let op: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
        let file: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let result = ShellExecuteW(
                None,
                PCWSTR(op.as_ptr()),
                PCWSTR(file.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
            (result.0 as isize) > 32
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = url;
        false
    }
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
        let mobile_kind = detect_mobile_project(&workspace_root);
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
        let terminal =
            TerminalSession::new_with_wakeup(term_title, &workspace_root, wakeup.clone());
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
            idle_editor: TextBuffer::from_text(PathBuf::new(), ""),
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
            selected_git_working_file: None,
            git_changes_group_collapsed: false,
            git_unversioned_group_collapsed: false,
            git_new_branch_from_ref: None,
            git_blame_visible: false,
            git_file_history_filter: None,
            git_reset_mode: ide_git::GitResetMode::Mixed,
            modal_scroll: 0,
            settings_page: SettingsPage::Appearance,
            settings_nav_scroll: 0,
            settings_nav_max_scroll: 0,
            settings_content_max_scroll: 0,
            settings_nav_rect: Rect::new(0, 0, 0, 0),
            settings_content_rect: Rect::new(0, 0, 0, 0),
            settings_search_rect: Rect::new(0, 0, 0, 0),
            settings_search_text_x: 0,
            settings_query_input: TextInputField::single_line(),
            mobile_sdk: MobileSdkLocations::default(),
            settings_sdk_focus: None,
            mobile_sdk_flutter: TextInputField::single_line(),
            mobile_sdk_android: TextInputField::single_line(),
            mobile_sdk_jdk: TextInputField::single_line(),
            mobile_sdk_field_rects: [Rect::new(0, 0, 0, 0); 3],
            hover_pos: (-1, -1),
            last_hover_hit_idx: None,
            last_hover_splitter: None,
            last_win_size: (1560, 940),
            window_maximized: false,
            pending_window_cmd: None,
            armed_window_control: None,
            left_tool: Some(ToolWindowId::Project),
            left_panel_width: 268,
            right_agent_open: true,
            right_panel_width: 440,
            right_panel_tab: RightPanelTab::Chat,
            bottom_tool: Some(ToolWindowId::Terminal),
            bottom_panel_height: 240,
            dock_left: DockPane::Project,
            dock_right: DockPane::Agent,
            dock_bottom: DockPane::Tooling,
            pending_dock_drag: None,
            active_dock_drag: None,
            dock_drag_pos: (0, 0),
            dock_drop_edge: None,
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
            editor_horiz_scroll_info: ScrollTrackInfo::default(),
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
            menu_trigger_and_bar_rect: Rect::new(0, 0, 0, 0),
            menu_dropdown_rect: Rect::new(0, 0, 0, 0),
            project_popup_open: false,
            run_config_popup_open: false,
            mobile_popup_open: false,
            more_menu_popup_open: false,
            git_branch_popup_open: false,
            agent_provider_popup_open: false,
            agent_model_popup_open: false,
            agent_mode_popup_open: false,
            agent_sessions_popup_open: false,
            agent_changed_files_expanded: false,
            agent_mode: agent_hub.normalize_mode_for_active(AgentWorkbenchMode::Agent),
            agent_busy_started_at: None,
            agent_provider_pill_rect: Rect::new(0, 0, 0, 0),
            agent_model_pill_rect: Rect::new(0, 0, 0, 0),
            agent_mode_pill_rect: Rect::new(0, 0, 0, 0),
            attach_active_context: true,
            editing_sent_msg_idx: None,
            editing_msg_saved_draft: String::new(),
            pending_edit_rewind: None,
            context_menu: None,
            context_menu_rect: Rect::new(0, 0, 0, 0),
            active_overlay_rects: Vec::new(),
            overlay_hit_start: usize::MAX,
            rename_target_path: None,
            run_configs,
            active_run_config: 0,
            run_config_popup_scroll: 0,
            run_config_popup_rect: Rect::new(0, 0, 0, 0),
            mobile_popup_rect: Rect::new(0, 0, 0, 0),
            mobile_env: MobileEnvironmentReport {
                project_kind: mobile_kind,
                ..Default::default()
            },
            selected_mobile_device_id: None,
            mobile_missing_prompted: false,
            mobile_probe_rx: None,
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
            expanded_message_file_changes: HashMap::new(),
            expanded_composer_snippet_id: None,
            active_editor_diff: None,
            media_tabs: Vec::new(),
            active_media_tab: None,
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
            pending_new_windows: Vec::new(),
            pending_switch_workspaces: Vec::new(),
            window_attention_until: None,
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
            persistence_dir: None,
            folder_pick_tx,
            folder_pick_rx,
            workspace_files_cache,
            search_seq,
            search_cmd_tx,
            search_event_rx,
            cached_runnables: None,
            cached_structure: None,
            cached_highlight_lines: (PathBuf::new(), usize::MAX, HashMap::new()),
            cached_max_editor_line_w: (PathBuf::new(), usize::MAX, 0, 0),
            cached_chat_rows: AgentChatCache::default(),
            hit_regions: Vec::new(),
            status_notification:
                "IDEA Islands UI 就绪 | 真实文档选区拖拽 · 原生 ConPTY VT100 终端 · AI Agent 工具链"
                    .to_string(),
        };

        let first_path = state.active_buffer().path.clone();
        state.dap_engine.toggle_breakpoint(&first_path, 17);
        if wakeup_enabled && mobile_kind.is_mobile() {
            state.refresh_mobile_environment_async();
        }
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
        self.save_global_settings();
    }

    pub fn restore_from_persistence_dir(&mut self, dir: PathBuf) {
        self.persistence_dir = Some(dir.clone());

        if let Some(gs) = persistence::load_json::<PersistedGlobalSettings>(
            &persistence::global_settings_path(&dir),
        ) {
            self.ui_lang = gs.ui_lang;
            self.ui_scale = gs.ui_scale.clamp(0.85, 2.0);
            self.theme = IdeaTheme::from_kind(gs.theme_kind);
            self.settings_page = gs.settings_page;
            self.left_tool = gs.left_tool;
            self.left_panel_width = gs.left_panel_width.clamp(48, 2000);
            self.right_agent_open = gs.right_agent_open;
            self.right_panel_width = gs.right_panel_width.clamp(64, 2000);
            self.right_panel_tab = gs.right_panel_tab;
            self.bottom_tool = gs.bottom_tool;
            self.bottom_panel_height = gs.bottom_panel_height.clamp(40, 1400);
            self.dock_left = gs.dock_left;
            self.dock_right = gs.dock_right;
            self.dock_bottom = gs.dock_bottom;
            self.normalize_docks();
            self.window_maximized = gs.window_maximized;
            if gs.window_width >= 800 && gs.window_height >= 540 {
                self.last_win_size = (gs.window_width, gs.window_height);
            }
            self.attach_active_context = gs.attach_active_context;

            let mut merged_recent = vec![self.workspace_root.clone()];
            for p in gs.recent_workspaces {
                let cp = clean_workspace_path(p);
                if cp.is_dir() && !merged_recent.contains(&cp) {
                    merged_recent.push(cp);
                }
            }
            merged_recent.truncate(10);
            self.recent_workspaces = merged_recent;

            for pref in &gs.provider_prefs {
                if let Some(reg) = self
                    .agent_hub
                    .providers
                    .iter_mut()
                    .find(|p| p.kind == pref.kind)
                {
                    if !pref.active_model.trim().is_empty() {
                        reg.active_model = pref.active_model.clone();
                        ide_agent_hub::cli_discovery::push_unique_model(
                            &mut reg.discovered_models,
                            &pref.active_model,
                        );
                    }
                    reg.fast_mode = pref.fast_mode;
                    reg.reasoning_effort = pref.reasoning_effort;
                }
            }
            if let Some(cfg) = gs.ai_api_config {
                let _ = self.agent_hub.update_api_config(cfg);
            }
            self.mobile_sdk = gs.mobile_sdk;
            self.sync_mobile_sdk_inputs();
            self.agent_hub.active_provider = gs.active_provider;
            self.agent_mode = self.agent_hub.normalize_mode_for_active(gs.agent_mode);
        }

        let hist_path = persistence::agent_history_path(&dir, &self.workspace_root);
        self.agent_hub.set_history_file_path(Some(hist_path));
        self.agent_mode = self.agent_hub.normalize_mode_for_active(self.agent_mode);
        self.ai_cfg_model.set_text(&self.agent_hub.active_model());
        let sess_draft = self.agent_hub.active_session().draft_prompt.clone();
        self.agent_chat_input.set_text(&sess_draft);
        self.agent_scroll = usize::MAX;
        self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);

        self.restore_workspace_state(&dir);
        self.save_global_settings();
    }

    pub fn save_global_settings(&self) {
        let Some(dir) = &self.persistence_dir else {
            return;
        };
        let provider_prefs = self
            .agent_hub
            .providers
            .iter()
            .map(|p| PersistedProviderPref {
                kind: p.kind,
                active_model: p.active_model.clone(),
                fast_mode: p.fast_mode,
                reasoning_effort: p.reasoning_effort,
            })
            .collect();
        let (win_w, win_h) = if self.last_win_size.0 >= 800 && self.last_win_size.1 >= 540 {
            self.last_win_size
        } else {
            (1560, 940)
        };
        let mut recent = Vec::with_capacity(self.recent_workspaces.len().min(10));
        for p in &self.recent_workspaces {
            let cp = clean_workspace_path(p.clone());
            if !recent.contains(&cp) {
                recent.push(cp);
            }
        }
        recent.truncate(10);
        let gs = PersistedGlobalSettings {
            ui_lang: self.ui_lang,
            ui_scale: self.ui_scale,
            theme_kind: self.theme.kind,
            settings_page: self.settings_page,
            last_workspace: Some(self.workspace_root.clone()),
            recent_workspaces: recent,
            left_tool: self.left_tool,
            left_panel_width: self.left_panel_width,
            right_agent_open: self.right_agent_open,
            right_panel_width: self.right_panel_width,
            right_panel_tab: self.right_panel_tab,
            bottom_tool: self.bottom_tool,
            bottom_panel_height: self.bottom_panel_height,
            dock_left: self.dock_left,
            dock_right: self.dock_right,
            dock_bottom: self.dock_bottom,
            window_maximized: self.window_maximized,
            window_width: win_w,
            window_height: win_h,
            attach_active_context: self.attach_active_context,
            agent_mode: self.agent_mode,
            active_provider: self.agent_hub.active_provider,
            provider_prefs,
            ai_api_config: Some(self.agent_hub.api_config.clone()),
            mobile_sdk: self.mobile_sdk.clone(),
        };
        persistence::save_json(&persistence::global_settings_path(dir), &gs);
    }

    pub fn save_workspace_state(&self) {
        let Some(dir) = &self.persistence_dir else {
            return;
        };
        let open_files = self
            .open_buffers
            .iter()
            .filter(|b| b.path.is_file())
            .map(|b| PersistedOpenTab {
                path: b.path.clone(),
                cursor_line: b.cursor_line,
                cursor_col: b.cursor_col,
            })
            .collect();
        let active_tab_path = self
            .open_buffers
            .get(self.active_tab)
            .filter(|b| b.path.is_file())
            .map(|b| b.path.clone());
        let custom_run_configs: Vec<RunConfiguration> = self
            .run_configs
            .iter()
            .filter(|c| c.source == RunConfigSource::Custom)
            .cloned()
            .collect();
        let active_run_config_name = self
            .run_configs
            .get(self.active_run_config)
            .map(|c| c.name.clone());
        let ws = PersistedWorkspaceState {
            workspace_root: self.workspace_root.clone(),
            open_files,
            active_tab_path,
            scroll_line: self.scroll_line,
            editor_scroll_x: self.editor_scroll_x,
            active_run_config: self.active_run_config,
            active_run_config_name,
            custom_run_configs,
            git_commit_draft: self.git_commit_input.as_str().to_string(),
        };
        persistence::save_json(
            &persistence::workspace_state_path(dir, &self.workspace_root),
            &ws,
        );
    }

    pub fn restore_workspace_state(&mut self, dir: &Path) {
        let Some(ws) = persistence::load_json::<PersistedWorkspaceState>(
            &persistence::workspace_state_path(dir, &self.workspace_root),
        ) else {
            return;
        };
        let mut restored_buffers = Vec::new();
        let saved_no_files = ws.open_files.is_empty();
        for tab in &ws.open_files {
            if tab.path.is_file() {
                if let Ok(mut buf) = TextBuffer::open(&tab.path) {
                    let max_line = buf.line_count().saturating_sub(1);
                    buf.set_cursor(tab.cursor_line.min(max_line), tab.cursor_col, false);
                    restored_buffers.push(buf);
                }
            }
        }
        if saved_no_files {
            self.open_buffers.clear();
            self.active_tab = 0;
            self.scroll_line = 0;
            self.editor_scroll_x = 0;
        } else if !restored_buffers.is_empty() {
            self.open_buffers = restored_buffers;
            self.active_tab = ws
                .active_tab_path
                .and_then(|ap| self.open_buffers.iter().position(|b| b.path == ap))
                .unwrap_or(0);
            self.scroll_line = ws.scroll_line;
            self.editor_scroll_x = ws.editor_scroll_x;
            if let Some(active_p) = self
                .open_buffers
                .get(self.active_tab)
                .map(|b| b.path.clone())
            {
                self.project_tree.selected_path = Some(active_p);
            }
        }
        for custom_cfg in ws.custom_run_configs.into_iter().rev() {
            let mut cfg = custom_cfg;
            cfg.source = RunConfigSource::Custom;
            self.upsert_run_configuration_internal(cfg, false, false);
        }
        if !self.run_configs.is_empty() {
            if let Some(target_name) = ws.active_run_config_name.as_deref() {
                if let Some(pos) = self.run_configs.iter().position(|c| c.name == target_name) {
                    self.active_run_config = pos;
                } else {
                    self.active_run_config = ws.active_run_config.min(self.run_configs.len() - 1);
                }
            } else {
                self.active_run_config = ws.active_run_config.min(self.run_configs.len() - 1);
            }
        }
        if !ws.git_commit_draft.is_empty() {
            self.git_commit_input.set_text(&ws.git_commit_draft);
            self.git_engine.commit_message_draft = ws.git_commit_draft;
        }
    }

    pub fn save_all_persisted_state(&self) {
        if self.persistence_dir.is_none() {
            return;
        }
        self.save_global_settings();
        self.save_workspace_state();
        self.agent_hub.save_history();
    }

    pub fn adapt_panels_for_logical_width(&mut self, log_w: i32) {
        let win_h = self.last_win_size.1.max(640);
        self.clamp_dock_spans(log_w, win_h);
    }

    pub fn workspace_name(&self) -> String {
        Self::folder_display_name(&self.workspace_root)
    }

    /// Windows 任务栏标题：项目名称在前，软件名称在后。
    pub fn taskbar_title(&self) -> String {
        format!("{} — Rust AI IDE", self.workspace_name())
    }

    fn folder_display_name(root: &Path) -> String {
        root.file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| root.display().to_string())
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
            let chars: Vec<char> = name
                .chars()
                .filter(|c| c.is_alphanumeric())
                .take(2)
                .collect();
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

    fn folder_pick_initial_dir(&self) -> PathBuf {
        let configured = match self.folder_pick_mode {
            FolderPickMode::MobileSdk(0) => self.mobile_sdk.flutter_sdk.as_str(),
            FolderPickMode::MobileSdk(1) => self.mobile_sdk.android_sdk.as_str(),
            FolderPickMode::MobileSdk(_) => self.mobile_sdk.jdk_home.as_str(),
            _ => "",
        };
        let path = PathBuf::from(configured.trim());
        if path.is_dir() {
            path
        } else if path.parent().is_some_and(|parent| parent.is_dir()) {
            path.parent().unwrap().to_path_buf()
        } else {
            self.workspace_root.clone()
        }
    }

    fn mobile_sdk_input_mut(&mut self, slot: u8) -> &mut TextInputField {
        match slot {
            0 => &mut self.mobile_sdk_flutter,
            1 => &mut self.mobile_sdk_android,
            _ => &mut self.mobile_sdk_jdk,
        }
    }

    pub fn request_open_folder_dialog(&mut self) {
        self.request_open_folder_dialog_with_mode(FolderPickMode::AskUser);
    }

    pub fn request_open_folder_dialog_with_mode(&mut self, mode: FolderPickMode) {
        self.folder_pick_mode = mode;
        self.project_popup_open = false;
        self.more_menu_popup_open = false;
        self.close_top_menu();
        self.context_menu = None;
        if FOLDER_PICKER_OPEN.swap(true, Ordering::SeqCst) {
            self.status_notification = self
                .tr(
                    "文件夹选择对话框已打开...",
                    "Folder picker dialog is already open...",
                )
                .to_string();
            return;
        }
        let initial = self.folder_pick_initial_dir();
        let tx = self.folder_pick_tx.clone();
        let wakeup = self.wakeup.clone();
        self.status_notification = self
            .tr(
                "正在打开系统文件夹选择对话框...",
                "Opening folder picker dialog...",
            )
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
        self.recent_workspaces.truncate(10);
        self.save_global_settings();
        self.project_popup_open = false;
        self.pending_open_workspace = None;
        if self.focus == FocusTarget::OpenProjectChoiceModal {
            self.focus = FocusTarget::Editor;
        }

        // Keep repeated clicks so the event loop can focus an existing window,
        // then flash it when the same project is requested again.
        self.pending_new_windows.push(clean.clone());
        if let Some(cb) = &self.wakeup {
            cb();
        }
        self.status_notification = format!(
            "{}: {}",
            self.tr("正在打开项目", "Opening project"),
            clean.display()
        );
    }

    /// Open `path` in this window, unless that project is already open elsewhere.
    pub fn request_switch_workspace(&mut self, path: PathBuf) {
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
        self.recent_workspaces.truncate(10);
        self.save_global_settings();
        self.project_popup_open = false;
        self.pending_open_workspace = None;
        if self.focus == FocusTarget::OpenProjectChoiceModal {
            self.focus = FocusTarget::Editor;
        }
        self.pending_switch_workspaces.push(clean.clone());
        if let Some(cb) = &self.wakeup {
            cb();
        }
        self.status_notification = format!(
            "{}: {}",
            self.tr("正在打开项目", "Opening project"),
            clean.display()
        );
    }

    pub fn start_window_attention_flash(&mut self) {
        self.window_attention_until = Some(Instant::now() + Duration::from_millis(980));
    }

    /// Trims heavy per-window transient caches when a window is minimized.
    pub fn trim_caches_when_minimized(&mut self) {
        self.cached_highlight_lines.2.clear();
        let active_media: std::collections::HashSet<PathBuf> = self
            .media_tabs
            .iter()
            .map(|m| m.path.clone())
            .collect();
        self.image_preview_cache
            .retain(|p, _| active_media.contains(p));
    }

    fn cache_decoded_image_preview(&mut self, path: PathBuf, img: Arc<DecodedPreviewImage>) {
        const MAX_IMAGE_PREVIEW_CACHE: usize = 24;
        if self.image_preview_cache.len() >= MAX_IMAGE_PREVIEW_CACHE
            && !self.image_preview_cache.contains_key(&path)
        {
            let keep: std::collections::HashSet<PathBuf> = self
                .media_tabs
                .iter()
                .map(|m| m.path.clone())
                .collect();
            self.image_preview_cache.retain(|p, _| keep.contains(p));
            if self.image_preview_cache.len() >= MAX_IMAGE_PREVIEW_CACHE {
                if let Some(drop_key) = self
                    .image_preview_cache
                    .keys()
                    .find(|k| !keep.contains(*k))
                    .cloned()
                    .or_else(|| self.image_preview_cache.keys().next().cloned())
                {
                    self.image_preview_cache.remove(&drop_key);
                }
            }
        }
        self.image_preview_cache.insert(path, img);
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
        let git_busy = self.git_engine.is_busy()
            || (self.wakeup_enabled && !self.git_engine.has_loaded_snapshot);
        let search_busy = self.search_loading;
        let agent_busy = self.agent_hub.is_busy.load(Ordering::Relaxed);
        let attention = self
            .window_attention_until
            .is_some_and(|until| now < until);
        startup_busy
            || git_busy
            || search_busy
            || agent_busy
            || attention
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

        let attention_frame = if let Some(until) = self.window_attention_until {
            if now >= until {
                self.window_attention_until = None;
            }
            true
        } else {
            false
        };

        attention_frame || self.has_active_animations()
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
        self.save_all_persisted_state();

        self.recent_workspaces.retain(|p| p != &clean);
        self.recent_workspaces.insert(0, clean.clone());
        self.recent_workspaces.truncate(10);
        self.save_global_settings();

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
        self.selected_git_working_file = None;
        self.active_editor_diff = None;
        self.media_tabs.clear();
        self.active_media_tab = None;
        self.expanded_message_file_changes.clear();
        self.git_changes_group_collapsed = false;
        self.git_unversioned_group_collapsed = false;
        self.git_new_branch_from_ref = None;
        self.git_blame_visible = false;
        self.git_file_history_filter = None;
        self.git_reset_mode = ide_git::GitResetMode::Mixed;
        self.git_log_search_input.clear();
        self.agent_scroll = usize::MAX;
        self.modal_scroll = 0;
        self.cached_runnables = None;
        self.cached_structure = None;
        self.cached_highlight_lines = (PathBuf::new(), usize::MAX, HashMap::new());
        self.project_popup_open = false;
        self.git_branch_popup_open = false;
        self.run_config_popup_open = false;
        self.run_config_popup_scroll = 0;
        self.mobile_popup_open = false;
        self.mobile_popup_rect = Rect::new(0, 0, 0, 0);
        self.selected_mobile_device_id = None;
        self.mobile_missing_prompted = false;
        self.mobile_probe_rx = None;
        let mobile_kind = detect_mobile_project(&clean);
        self.mobile_env = MobileEnvironmentReport {
            project_kind: mobile_kind,
            ..Default::default()
        };
        self.more_menu_popup_open = false;
        self.close_top_menu();
        self.context_menu = None;
        self.focus = FocusTarget::Editor;
        self.touch_caret_activity();

        if let Some(first) = self.open_buffers.first().map(|b| b.path.clone()) {
            self.project_tree.selected_path = Some(first);
        }
        if let Some(dir) = self.persistence_dir.clone() {
            let hist_path = persistence::agent_history_path(&dir, &clean);
            self.agent_hub.set_history_file_path(Some(hist_path));
            self.agent_mode = self.agent_hub.normalize_mode_for_active(self.agent_mode);
            self.ai_cfg_model.set_text(&self.agent_hub.active_model());
            let sess_draft = self.agent_hub.active_session().draft_prompt.clone();
            self.agent_chat_input.set_text(&sess_draft);
            self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
            self.restore_workspace_state(&dir);
            self.save_global_settings();
        }
        if self.wakeup_enabled && mobile_kind.is_mobile() {
            self.refresh_mobile_environment_async();
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

    fn ensure_runnables_cached(&mut self, path: &PathBuf, rev: usize, lang: ide_core::LanguageId) {
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

    fn ensure_editor_max_line_width_cached(
        &mut self,
        path: &PathBuf,
        rev: usize,
        char_w: i32,
        canvas: &SurfaceCanvas,
    ) -> i32 {
        if self.cached_max_editor_line_w.0 == *path
            && self.cached_max_editor_line_w.1 == rev
            && self.cached_max_editor_line_w.2 == char_w
        {
            return self.cached_max_editor_line_w.3;
        }
        let mut line_estimates: Vec<(usize, i32)> = {
            let buf = self.active_buffer();
            (0..buf.line_count())
                .map(|idx| {
                    let s = buf.line_text(idx);
                    let est: i32 = s
                        .chars()
                        .map(|c| if c.is_ascii() { char_w } else { char_w * 2 })
                        .sum();
                    (idx, est)
                })
                .collect()
        };
        let max_est = line_estimates
            .iter()
            .map(|(_, est)| *est)
            .max()
            .unwrap_or(0);
        // Only measure exact glyphs for the widest candidates to stay O(1) on huge files.
        let threshold = (max_est - char_w * 16).max(0);
        line_estimates.retain(|(_, est)| *est >= threshold);
        line_estimates.sort_by(|a, b| b.1.cmp(&a.1));
        let max_px = {
            let buf = self.active_buffer();
            line_estimates
                .into_iter()
                .take(12)
                .map(|(idx, _)| {
                    let s = buf.line_text(idx);
                    canvas.measure_text(&s, FontStyle::CodeMono).0
                })
                .max()
                .unwrap_or(0)
        };
        self.cached_max_editor_line_w = (path.clone(), rev, char_w, max_px);
        max_px
    }

    pub fn active_buffer(&self) -> &TextBuffer {
        if self.open_buffers.is_empty() {
            return &self.idle_editor;
        }
        &self.open_buffers[self.active_tab.min(self.open_buffers.len() - 1)]
    }

    pub fn active_buffer_mut(&mut self) -> &mut TextBuffer {
        if self.open_buffers.is_empty() {
            return &mut self.idle_editor;
        }
        let idx = self.active_tab.min(self.open_buffers.len() - 1);
        &mut self.open_buffers[idx]
    }

    fn abort_without_open_file(&mut self) -> bool {
        if !self.open_buffers.is_empty() {
            return false;
        }
        self.status_notification = self
            .tr("当前没有打开的文件", "No file is open")
            .to_string();
        true
    }

    pub fn open_file(&mut self, path: PathBuf) {
        self.active_editor_diff = None;
        self.project_tree.selected_path = Some(path.clone());
        if let Some(existing_idx) = self.open_buffers.iter().position(|b| b.path == path) {
            self.active_media_tab = None;
            self.active_tab = existing_idx;
            self.scroll_line = 0;
            self.editor_scroll_x = 0;
            return;
        }
        if detect_media_kind(&path).is_some() {
            self.open_media_file(path);
            return;
        }
        self.open_file_as_text(path);
    }

    /// 强制以文本缓冲区打开指定文件（例如查看 `.svg` XML 源码）
    fn ensure_active_blame_loaded(&self) {
        if self.git_blame_visible {
            let p = &self.active_buffer().path;
            if !self.git_engine.file_blames.contains_key(p) {
                self.git_engine.request_load_file_blame(p);
            }
        }
    }

    pub fn open_file_as_text(&mut self, path: PathBuf) {
        self.active_editor_diff = None;
        self.active_media_tab = None;
        self.project_tree.selected_path = Some(path.clone());
        if let Some(existing_idx) = self.open_buffers.iter().position(|b| b.path == path) {
            self.active_tab = existing_idx;
            self.scroll_line = 0;
            self.editor_scroll_x = 0;
            self.ensure_active_blame_loaded();
            return;
        }
        if let Ok(buf) = TextBuffer::open(&path) {
            self.open_buffers.push(buf);
            self.active_tab = self.open_buffers.len() - 1;
            self.scroll_line = 0;
            self.editor_scroll_x = 0;
            self.status_notification = format!("已打开: {}", path.display());
            self.save_workspace_state();
            self.ensure_active_blame_loaded();
        }
    }

    /// 在中央编辑器标签栏打开图片 / SVG / 音频 / 视频媒体预览标签
    pub fn open_media_file(&mut self, path: PathBuf) {
        self.active_editor_diff = None;
        self.project_tree.selected_path = Some(path.clone());
        if let Some(idx) = self.media_tabs.iter().position(|m| m.path == path) {
            self.active_media_tab = Some(idx);
            return;
        }
        let cached = self.image_preview_cache.get(&path).cloned();
        let tab = load_media_preview_tab(&path, cached);
        if let Some(ref img) = tab.preview_image {
            self.cache_decoded_image_preview(path.clone(), img.clone());
        }
        self.media_tabs.push(tab);
        self.active_media_tab = Some(self.media_tabs.len() - 1);
        self.status_notification = format!(
            "{}: {}",
            self.tr("已打开媒体预览", "Opened media preview"),
            path.display()
        );
    }

    /// 关闭指定索引的媒体预览标签页
    pub fn close_media_tab(&mut self, idx: usize) {
        if idx >= self.media_tabs.len() {
            return;
        }
        let closed = self.media_tabs.remove(idx);
        match self.active_media_tab {
            Some(cur) if cur == idx => {
                if self.media_tabs.is_empty() {
                    self.active_media_tab = None;
                } else {
                    self.active_media_tab = Some(idx.min(self.media_tabs.len() - 1));
                }
            }
            Some(cur) if cur > idx => {
                self.active_media_tab = Some(cur - 1);
            }
            _ => {}
        }
        self.status_notification = format!(
            "{}: {}",
            self.tr("已关闭媒体标签", "Closed media tab"),
            closed.path.display()
        );
    }

    fn read_git_head_text_for_path(&self, path: &Path) -> String {
        let rel = path
            .strip_prefix(&self.workspace_root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if rel.is_empty() {
            return String::new();
        }
        let mut cmd = std::process::Command::new("git");
        cmd.args(["show", &format!("HEAD:{rel}")])
            .current_dir(&self.workspace_root)
            .stdin(std::process::Stdio::null());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        cmd.output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    pub fn open_editor_diff_from_lines(
        &mut self,
        path: PathBuf,
        subtitle: String,
        diff_lines: Vec<ide_git::DiffLine>,
    ) {
        let mut added_lines = 0usize;
        let mut removed_lines = 0usize;
        for dl in &diff_lines {
            match dl.kind {
                DiffLineKind::Added => added_lines += 1,
                DiffLineKind::Removed => removed_lines += 1,
                DiffLineKind::Context => {}
            }
        }
        self.active_editor_diff = Some(ActiveEditorDiff {
            path: path.clone(),
            subtitle,
            diff_lines,
            added_lines,
            removed_lines,
            scroll: 0,
            scroll_x: 0,
        });
        self.focus = FocusTarget::Editor;
        self.touch_caret_activity();
        self.status_notification = format!(
            "{}: {}",
            self.tr("已在文件窗口打开 Diff", "Opened diff in editor"),
            path.display()
        );
    }

    pub fn open_editor_diff_from_texts(
        &mut self,
        path: PathBuf,
        subtitle: String,
        old_text: &str,
        new_text: &str,
    ) {
        let diff_lines = ide_git::compute_line_diff(old_text, new_text);
        self.open_editor_diff_from_lines(path, subtitle, diff_lines);
    }

    pub fn open_git_working_diff_in_editor(&mut self, path: &Path) {
        self.selected_git_working_file = Some(path.to_path_buf());
        if let Some(dlines) = self.git_engine.working_diffs.get(path).cloned() {
            let subtitle = self
                .tr("Git 工作区变更 (HEAD ↔ 当前)", "Git Working Tree (HEAD ↔ Local)")
                .to_string();
            self.open_editor_diff_from_lines(path.to_path_buf(), subtitle, dlines);
        } else {
            let old_text = self.read_git_head_text_for_path(path);
            let new_text = std::fs::read_to_string(path).unwrap_or_default();
            let subtitle = self
                .tr("Git 工作区变更 (HEAD ↔ 当前)", "Git Working Tree (HEAD ↔ Local)")
                .to_string();
            self.open_editor_diff_from_texts(path.to_path_buf(), subtitle, &old_text, &new_text);
        }
    }

    pub fn open_agent_file_diff_in_editor(&mut self, path: &Path, msg_idx: Option<usize>) {
        let old_from_msg = msg_idx.and_then(|idx| {
            self.agent_hub.messages.get(idx).and_then(|m| {
                m.file_changes
                    .iter()
                    .find(|fc| fc.path == path)
                    .and_then(|fc| fc.before_content.clone())
            })
        });
        let old_from_sess = self
            .agent_hub
            .active_session_changed_files()
            .iter()
            .find(|sf| sf.path == path)
            .and_then(|sf| sf.original_content.clone());
        let old_text = old_from_msg
            .or(old_from_sess)
            .unwrap_or_else(|| self.read_git_head_text_for_path(path));
        let new_text = std::fs::read_to_string(path).unwrap_or_default();
        let subtitle = self
            .tr("AI 对话文件变更 (修改前 ↔ 修改后)", "AI Agent Diff (Before ↔ After)")
            .to_string();
        self.open_editor_diff_from_texts(path.to_path_buf(), subtitle, &old_text, &new_text);
    }

    pub fn open_file_at_line(&mut self, path: PathBuf, line_1_based: Option<usize>) {
        if line_1_based.is_some()
            && detect_media_kind(&path).is_some_and(|k| k == MediaKind::Svg)
        {
            self.open_file_as_text(path.clone());
        } else {
            self.open_file(path.clone());
        }
        if self.active_media_tab.is_none() {
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
                return;
            }
        }
        self.focus = FocusTarget::Editor;
        self.touch_caret_activity();
    }

    pub fn close_tab(&mut self, idx: usize) {
        if idx >= self.open_buffers.len() {
            return;
        }
        let closed = self.open_buffers.remove(idx);
        if idx < self.active_tab {
            self.active_tab -= 1;
        }
        if self.open_buffers.is_empty() {
            self.active_tab = 0;
            self.scroll_line = 0;
            self.editor_scroll_x = 0;
            self.active_editor_diff = None;
            self.status_notification = self
                .tr("已关闭最后一个文件", "Closed the last file")
                .to_string();
        } else {
            if self.active_tab >= self.open_buffers.len() {
                self.active_tab = self.open_buffers.len() - 1;
            }
            self.status_notification = format!(
                "{}: {}",
                self.tr("已关闭标签页", "Closed tab"),
                closed.path.display()
            );
        }
        self.save_workspace_state();
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
        self.save_global_settings();
    }

    pub fn execute_ide_action(&mut self, action: IdeAction) {
        self.close_top_menu();
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
                        self.open_buffers
                            .retain(|b| b.path != sel && !b.path.starts_with(&sel));
                        if self.open_buffers.is_empty() {
                            self.active_tab = 0;
                        } else if self.active_tab >= self.open_buffers.len() {
                            self.active_tab = self.open_buffers.len() - 1;
                        }
                        self.git_engine.request_refresh();
                        self.refresh_workspace_file_index_async();
                        self.status_notification =
                            format!("{}: {}", self.tr("已删除", "Deleted"), sel.display());
                    }
                }
            }
            IdeAction::SaveActiveFile => {
                if self.abort_without_open_file() {
                    return;
                }
                if self.active_buffer_mut().save().is_ok() {
                    self.git_engine.request_refresh();
                    self.status_notification =
                        self.tr("已保存当前文档", "Saved current file").to_string();
                }
            }
            IdeAction::CloseActiveTab => {
                let idx = self.active_tab;
                self.close_tab(idx);
            }
            IdeAction::UndoEdit => {
                if self.abort_without_open_file() {
                    return;
                }
                self.active_buffer_mut().undo();
                self.ensure_editor_cursor_visible();
                self.status_notification = self
                    .tr("已撤销编辑 (Ctrl+Z)", "Undid edit (Ctrl+Z)")
                    .to_string();
            }
            IdeAction::RedoEdit => {
                if self.abort_without_open_file() {
                    return;
                }
                self.active_buffer_mut().redo();
                self.ensure_editor_cursor_visible();
                self.status_notification = self
                    .tr("已重做编辑 (Ctrl+Shift+Z)", "Redid edit (Ctrl+Shift+Z)")
                    .to_string();
            }
            IdeAction::DuplicateLine => {
                if self.abort_without_open_file() {
                    return;
                }
                self.active_buffer_mut().duplicate_current_line();
                self.ensure_editor_cursor_visible();
                self.status_notification = self
                    .tr("已复制当前行 (IDEA Ctrl+D)", "Duplicated line (Ctrl+D)")
                    .to_string();
            }
            IdeAction::DeleteLine => {
                if self.abort_without_open_file() {
                    return;
                }
                self.active_buffer_mut().delete_current_line();
                self.ensure_editor_cursor_visible();
                self.status_notification = self
                    .tr("已删除当前行 (IDEA Ctrl+Y)", "Deleted line (Ctrl+Y)")
                    .to_string();
            }
            IdeAction::ToggleLineComment => {
                if self.abort_without_open_file() {
                    return;
                }
                self.active_buffer_mut().toggle_line_comment();
                self.status_notification = self
                    .tr(
                        "已切换行注释 (IDEA Ctrl+/)",
                        "Toggled line comment (Ctrl+/)",
                    )
                    .to_string();
            }
            IdeAction::ReformatCode => {
                if self.abort_without_open_file() {
                    return;
                }
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
            IdeAction::RenameSymbol
            | IdeAction::ShowIntentionActions
            | IdeAction::InlineAiEdit
            | IdeAction::SendSelectionToAgent => {}
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
                self.toggle_run_config_execution(self.active_run_config);
            }
            IdeAction::RunCurrentFile => {
                if self.abort_without_open_file() {
                    return;
                }
                let path = self.active_buffer().path.clone();
                if !self.run_file_in_terminal(&path) {
                    self.status_notification = self
                        .tr(
                            "当前文件类型不支持直接运行",
                            "Current file type cannot be executed directly",
                        )
                        .to_string();
                }
            }
            IdeAction::DebugActiveConfig => {
                if self.abort_without_open_file() {
                    return;
                }
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
                if self.abort_without_open_file() {
                    return;
                }
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
                    .tr(
                        "正在后台执行 Git Pull...",
                        "Running Git Pull in background...",
                    )
                    .to_string();
            }
            IdeAction::GitPush => {
                self.git_engine.request_push();
                self.status_notification = self
                    .tr(
                        "正在后台执行 Git Push...",
                        "Running Git Push in background...",
                    )
                    .to_string();
            }
            IdeAction::GitFetch => {
                self.git_engine.request_fetch();
                self.status_notification = self
                    .tr("正在后台执行 Git Fetch...", "Fetching all remotes...")
                    .to_string();
            }
            IdeAction::GitBranches => {
                self.git_engine.request_refresh();
                self.git_branch_popup_open = true;
            }
            IdeAction::GitNewBranch => {
                self.git_new_branch_from_ref = None;
                self.modal_text_input.set_text("feature/ai-agent-flow");
                self.modal_text_input.select_all();
                self.focus = FocusTarget::NewGitBranchModal;
                self.touch_caret_activity();
            }
            IdeAction::GitNewTag => {
                self.modal_text_input.set_text("v0.1.0");
                self.modal_text_input.select_all();
                self.focus = FocusTarget::NewGitTagModal;
                self.touch_caret_activity();
            }
            IdeAction::GitMerge => {
                self.git_engine.request_refresh();
                self.modal_scroll = 0;
                self.focus = FocusTarget::GitMergeModal;
            }
            IdeAction::GitRebase => {
                self.git_engine.request_refresh();
                self.modal_scroll = 0;
                self.focus = FocusTarget::GitRebaseModal;
            }
            IdeAction::GitStashChanges => {
                let default_msg = format!("WIP on {}", self.git_engine.current_branch);
                self.modal_text_input.set_text(&default_msg);
                self.modal_text_input.select_all();
                self.focus = FocusTarget::NewGitStashModal;
                self.touch_caret_activity();
            }
            IdeAction::GitUnstashChanges => {
                self.git_engine.request_refresh();
                self.modal_scroll = 0;
                self.focus = FocusTarget::GitUnstashModal;
            }
            IdeAction::GitResetHead => {
                self.git_engine.request_refresh();
                self.focus = FocusTarget::GitResetModal;
            }
            IdeAction::GitRollback => {
                self.git_engine.request_refresh();
                self.modal_scroll = 0;
                self.focus = FocusTarget::GitRollbackModal;
            }
            IdeAction::GitShowDiff => {
                if self.abort_without_open_file() {
                    return;
                }
                let p = self.active_buffer().path.clone();
                self.open_git_working_diff_in_editor(&p);
            }
            IdeAction::GitAnnotateBlame => {
                self.git_blame_visible = !self.git_blame_visible;
                if self.git_blame_visible {
                    let p = self.active_buffer().path.clone();
                    self.git_engine.request_load_file_blame(&p);
                    self.status_notification = self
                        .tr("已开启 Git Blame 逐行注释", "Enabled Git Blame annotations")
                        .to_string();
                } else {
                    self.status_notification = self
                        .tr("已隐藏 Git Blame 注释", "Disabled Git Blame annotations")
                        .to_string();
                }
            }
            IdeAction::GitFileHistory => {
                if self.abort_without_open_file() {
                    return;
                }
                let p = self.active_buffer().path.clone();
                self.git_file_history_filter = Some(p.clone());
                self.git_log_scroll = 0;
                self.selected_git_commit_hash = None;
                self.git_engine.request_load_file_history(&p);
                self.bottom_tool = Some(ToolWindowId::GitLog);
                self.status_notification = format!(
                    "{}: {}",
                    self.tr("正在加载文件提交历史", "Loading file Git history"),
                    p.display()
                );
            }
            IdeAction::GitRollbackCurrentFile => {
                if self.abort_without_open_file() {
                    return;
                }
                let p = self.active_buffer().path.clone();
                self.git_engine.request_rollback_file(&p);
                self.status_notification = format!(
                    "{}: {}",
                    self.tr("正在回滚当前文件", "Rolling back active file"),
                    p.display()
                );
            }
            IdeAction::GitShowLog => {
                self.git_file_history_filter = None;
                self.bottom_tool = Some(ToolWindowId::GitLog);
                self.git_engine.request_refresh();
            }
            IdeAction::GitManageRemotes => {
                self.git_engine.request_refresh();
                self.modal_scroll = 0;
                self.focus = FocusTarget::GitRemotesModal;
            }
            IdeAction::CheckMobileDevicesAndSdk => {
                self.mobile_popup_open = true;
                self.mobile_missing_prompted = false;
                self.refresh_mobile_environment_async();
                self.status_notification = self
                    .tr(
                        "正在扫描连接手机 (ADB / Flutter) 与本地 SDK...",
                        "Scanning mobile devices (ADB / Flutter) & local SDKs...",
                    )
                    .to_string();
            }
            IdeAction::ToggleProjectWindow => {
                self.left_tool = if self.left_tool == Some(ToolWindowId::Project) {
                    None
                } else {
                    Some(ToolWindowId::Project)
                };
                self.save_global_settings();
            }
            IdeAction::ToggleStructureWindow => {
                self.left_tool = if self.left_tool == Some(ToolWindowId::Structure) {
                    None
                } else {
                    Some(ToolWindowId::Structure)
                };
                self.save_global_settings();
            }
            IdeAction::ToggleAgentHubWindow => {
                self.right_agent_open = !self.right_agent_open;
                self.save_global_settings();
            }
            IdeAction::ToggleTerminalWindow => {
                self.bottom_tool = if self.bottom_tool == Some(ToolWindowId::Terminal) {
                    None
                } else {
                    self.focus = FocusTarget::TerminalInput;
                    self.touch_caret_activity();
                    Some(ToolWindowId::Terminal)
                };
                self.save_global_settings();
            }
            IdeAction::OpenSettingsModal => {
                self.focus = FocusTarget::SettingsModal;
                self.settings_query_input.clear();
                self.settings_sdk_focus = None;
                self.sync_mobile_sdk_inputs();
                self.settings_nav_scroll = 0;
                self.modal_scroll = 0;
                self.touch_caret_activity();
            }
            IdeAction::SwitchTheme => {
                let next_kind = self.theme.kind.next();
                self.theme = IdeaTheme::from_kind(next_kind);
                self.status_notification = format!(
                    "{}: {}",
                    self.tr("已切换主题为", "Switched theme to"),
                    next_kind.display_name()
                );
                self.save_global_settings();
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
                let (_, max) = self.span_limits_for_drag(
                    DockEdge::Left,
                    self.last_win_size.0,
                    self.last_win_size.1,
                );
                self.left_panel_width = (self.left_panel_width + 56).min(max);
            }
            IdeAction::NarrowLeftPanel => {
                self.left_panel_width = (self.left_panel_width - 56).max(48);
            }
            IdeAction::WidenRightPanel => {
                let (_, max) = self.span_limits_for_drag(
                    DockEdge::Right,
                    self.last_win_size.0,
                    self.last_win_size.1,
                );
                self.right_panel_width = (self.right_panel_width + 56).min(max);
            }
            IdeAction::RaiseBottomPanel => {
                let (_, max) = self.span_limits_for_drag(
                    DockEdge::Bottom,
                    self.last_win_size.0,
                    self.last_win_size.1,
                );
                self.bottom_panel_height = (self.bottom_panel_height + 48).min(max);
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

    fn strip_ide_context_tag(raw: &str) -> String {
        let trimmed = raw.trim_end();
        if let Some(pos) = trimmed.rfind("  [@") {
            if trimmed[pos..].ends_with(']') {
                return trimmed[..pos].to_string();
            }
        }
        raw.to_string()
    }

    pub fn reload_paths_after_revert(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        for path in paths {
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(path) {
                    if let Some(buf) = self.open_buffers.iter_mut().find(|b| &b.path == path) {
                        *buf = TextBuffer::from_text(path.clone(), &content);
                    }
                }
            } else {
                self.open_buffers.retain(|b| &b.path != path);
            }
        }
        if self.open_buffers.is_empty() {
            self.active_tab = 0;
        } else if self.active_tab >= self.open_buffers.len() {
            self.active_tab = self.open_buffers.len() - 1;
        }
        self.project_tree.rebuild();
        self.git_engine.request_refresh();
        self.refresh_workspace_file_index_async();
    }

    pub fn start_edit_sent_user_message(&mut self, msg_idx: usize) {
        if !self.agent_hub.active_cli_supports_checkpoint_rewind()
            || self.agent_hub.is_active_session_busy()
        {
            return;
        }
        let Some(msg) = self.agent_hub.messages.get(msg_idx) else {
            return;
        };
        if msg.role != "user" {
            return;
        }
        if self.editing_sent_msg_idx.is_none() {
            self.editing_msg_saved_draft = self.agent_chat_input.as_str().to_string();
        }
        let clean = Self::strip_ide_context_tag(&msg.content);
        self.editing_sent_msg_idx = Some(msg_idx);
        self.agent_chat_input.set_text(&clean);
        self.focus = FocusTarget::AgentChatInput;
        self.touch_caret_activity();
        self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
    }

    pub fn cancel_edit_sent_user_message(&mut self) {
        if self.editing_sent_msg_idx.take().is_some() {
            let draft = std::mem::take(&mut self.editing_msg_saved_draft);
            self.agent_chat_input.set_text(&draft);
            self.pending_edit_rewind = None;
            self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
            if self.focus == FocusTarget::EditMessageRewindModal {
                self.focus = FocusTarget::AgentChatInput;
            }
        }
    }

    pub fn open_edit_message_rewind_modal(&mut self, msg_idx: usize, new_prompt: String) {
        let (dropped_msgs, affected_files) = self.agent_hub.count_rewind_impact(msg_idx);
        self.pending_edit_rewind = Some(PendingEditMessageRewind {
            msg_idx,
            new_prompt,
            dropped_msgs,
            affected_files,
        });
        self.focus = FocusTarget::EditMessageRewindModal;
    }

    pub fn confirm_edit_message_rewind(&mut self, revert_files: bool) {
        let Some(pending) = self.pending_edit_rewind.take() else {
            self.focus = FocusTarget::AgentChatInput;
            return;
        };
        self.editing_sent_msg_idx = None;
        let restored_draft = std::mem::take(&mut self.editing_msg_saved_draft);
        self.agent_chat_input.set_text(&restored_draft);
        self.agent_mode = self.agent_hub.normalize_mode_for_active(self.agent_mode);
        let ctx = if self.attach_active_context {
            self.build_active_ide_code_context()
        } else {
            None
        };
        self.expanded_composer_snippet_id = None;
        let restored_paths = self.agent_hub.edit_message_and_rewind(
            pending.msg_idx,
            pending.new_prompt,
            revert_files,
            self.agent_mode,
            ctx.as_ref(),
        );
        self.reload_paths_after_revert(&restored_paths);
        self.agent_busy_started_at = if self.agent_hub.is_active_session_busy() {
            Some(Instant::now())
        } else {
            None
        };
        self.right_agent_open = true;
        self.right_panel_tab = RightPanelTab::Chat;
        self.focus = FocusTarget::AgentChatInput;
        self.agent_scroll = usize::MAX;
        self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
        self.status_notification = if revert_files && !restored_paths.is_empty() {
            format!(
                "已回退 {} 个文件并同步 CLI 重发编辑后的消息",
                restored_paths.len()
            )
        } else {
            "已保留文件修改并与 CLI 客户端同步重发消息".to_string()
        };
    }

    fn upsert_run_configuration_internal(
        &mut self,
        cfg: RunConfiguration,
        set_active: bool,
        persist: bool,
    ) -> usize {
        if let Some(pos) = self.run_configs.iter().position(|c| c.same_identity(&cfg)) {
            if set_active {
                self.active_run_config = pos;
            }
            if persist {
                self.save_workspace_state();
            }
            return pos;
        }
        if self.run_configs.len() == 1
            && self.run_configs[0].name == "Default Shell Task"
            && self.run_configs[0].source == RunConfigSource::Detected
        {
            self.run_configs.clear();
            self.active_run_config = 0;
        }
        let custom_count = self
            .run_configs
            .iter()
            .filter(|c| c.source == RunConfigSource::Custom)
            .count();
        if cfg.source == RunConfigSource::Custom && custom_count >= 20 {
            if let Some(oldest_custom) = self
                .run_configs
                .iter()
                .rposition(|c| c.source == RunConfigSource::Custom)
            {
                self.run_configs.remove(oldest_custom);
                if oldest_custom < self.active_run_config {
                    self.active_run_config = self.active_run_config.saturating_sub(1);
                } else if self.active_run_config >= self.run_configs.len() {
                    self.active_run_config = self.run_configs.len().saturating_sub(1);
                }
            }
        }
        let idx = if cfg.source == RunConfigSource::Custom {
            self.run_configs.insert(0, cfg);
            if !set_active && self.run_configs.len() > 1 {
                self.active_run_config = (self.active_run_config + 1).min(self.run_configs.len() - 1);
            }
            0
        } else {
            self.run_configs.push(cfg);
            self.run_configs.len() - 1
        };
        if set_active {
            self.active_run_config = idx;
        }
        if persist {
            self.save_workspace_state();
        }
        idx
    }

    pub fn is_run_config_running(&self, idx: usize) -> bool {
        let Some(cfg) = self.run_configs.get(idx) else {
            return false;
        };
        self.terminal.is_run_config_running(&cfg.identity_key())
    }

    pub fn active_run_config_is_running(&self) -> bool {
        self.is_run_config_running(self.active_run_config)
    }

    fn sync_mobile_sdk_inputs(&mut self) {
        self.mobile_sdk_flutter
            .set_text(&self.mobile_sdk.flutter_sdk);
        self.mobile_sdk_android
            .set_text(&self.mobile_sdk.android_sdk);
        self.mobile_sdk_jdk.set_text(&self.mobile_sdk.jdk_home);
    }

    fn apply_picked_mobile_sdk_dir(&mut self, slot: u8, path: PathBuf) {
        let text = clean_workspace_path(path).display().to_string();
        match slot {
            0 => {
                self.mobile_sdk.flutter_sdk = text.clone();
                self.mobile_sdk_flutter.set_text(&text);
            }
            1 => {
                self.mobile_sdk.android_sdk = text.clone();
                self.mobile_sdk_android.set_text(&text);
            }
            _ => {
                self.mobile_sdk.jdk_home = text.clone();
                self.mobile_sdk_jdk.set_text(&text);
            }
        }
        self.save_global_settings();
        self.mobile_missing_prompted = false;
        self.refresh_mobile_environment_async();
        self.status_notification =
            format!("{}: {text}", self.tr("已设置 SDK 安装目录", "SDK install directory set"));
    }

    pub fn commit_mobile_sdk_locations_from_inputs(&mut self) -> bool {
        let next = MobileSdkLocations {
            flutter_sdk: self.mobile_sdk_flutter.as_str().trim().to_string(),
            android_sdk: self.mobile_sdk_android.as_str().trim().to_string(),
            jdk_home: self.mobile_sdk_jdk.as_str().trim().to_string(),
        };
        if next == self.mobile_sdk {
            return false;
        }
        self.mobile_sdk = next;
        self.save_global_settings();
        self.mobile_missing_prompted = false;
        self.refresh_mobile_environment_async();
        true
    }

    pub fn refresh_mobile_environment_async(&mut self) {
        if self.mobile_env.probing {
            return;
        }
        self.mobile_env.probing = true;
        let root = self.workspace_root.clone();
        let kind = self.mobile_env.project_kind;
        let locations = self.mobile_sdk.clone();
        let wakeup = self.wakeup.clone();
        let (tx, rx) = mpsc::channel();
        self.mobile_probe_rx = Some(rx);
        thread::spawn(move || {
            let report = probe_mobile_environment(&root, kind, &locations);
            let _ = tx.send(report);
            if let Some(w) = wakeup {
                w();
            }
        });
    }

    pub fn apply_mobile_environment_report(&mut self, report: MobileEnvironmentReport) {
        self.mobile_env = report;
        if let Some(cur_id) = self.selected_mobile_device_id.as_ref() {
            if !self.mobile_env.devices.iter().any(|d| &d.id == cur_id) {
                self.selected_mobile_device_id = None;
            }
        }
        if self.selected_mobile_device_id.is_none() {
            self.selected_mobile_device_id = self
                .mobile_env
                .devices
                .iter()
                .find(|d| d.state == MobileDeviceState::Online)
                .map(|d| d.id.clone());
        }

        let missing = self.mobile_env.missing_required_tools();
        if self.mobile_env.project_kind.is_mobile() && !missing.is_empty() {
            let names: Vec<&str> = missing.iter().map(|t| t.label.as_str()).collect();
            if !self.mobile_missing_prompted {
                self.mobile_missing_prompted = true;
                self.mobile_popup_open = true;
            }
            self.status_notification = format!(
                "⚠ {}: {}（{}）",
                self.tr("SDK 未安装", "SDK not installed"),
                names.join(", "),
                self.tr(
                    "可在设置 → 语言和框架 → 移动端 SDK 指定安装目录",
                    "Set the install directory under Settings → Languages & Frameworks → Mobile SDKs",
                )
            );
        } else if let Some(dev) = self.selected_or_first_online_device() {
            self.status_notification = format!(
                "📱 {}: {} ({})",
                self.tr("移动设备已就绪", "Mobile device ready"),
                dev.name,
                dev.id
            );
        } else if self.mobile_env.project_kind.is_mobile() {
            self.status_notification = self
                .tr(
                    "移动端环境 SDK 已就绪，当前未检测到在线手机或模拟器 (可连接 USB 调试或启动 AVD)",
                    "Mobile SDK ready; no online device/emulator connected yet",
                )
                .to_string();
        }
    }

    pub fn selected_or_first_online_device(&self) -> Option<&ide_core::MobileDeviceInfo> {
        if let Some(sel_id) = self.selected_mobile_device_id.as_deref() {
            if let Some(dev) = self.mobile_env.devices.iter().find(|d| d.id == sel_id) {
                return Some(dev);
            }
        }
        self.mobile_env
            .devices
            .iter()
            .find(|d| d.state == MobileDeviceState::Online)
    }

    fn warn_if_missing_mobile_sdk_for_cmd(&mut self, cfg: &RunConfiguration) {
        if self.mobile_env.sdk_tools.is_empty() || self.mobile_env.probing {
            return;
        }
        let is_mobile_cmd = cfg.command.eq_ignore_ascii_case("flutter")
            || cfg.command.eq_ignore_ascii_case("dart")
            || cfg.command.eq_ignore_ascii_case("adb")
            || (cfg.command.eq_ignore_ascii_case("gradle")
                && self.mobile_env.project_kind.is_mobile());
        if !is_mobile_cmd {
            return;
        }
        let missing = self.mobile_env.missing_required_tools();
        if !missing.is_empty() {
            let names: Vec<&str> = missing.iter().map(|t| t.label.as_str()).collect();
            self.mobile_popup_open = true;
            self.status_notification = format!(
                "⚠ {}: {} — {}",
                self.tr("SDK 未安装", "SDK not installed"),
                names.join(", "),
                self.tr(
                    "请在设置 → 语言和框架 → 移动端 SDK 指定安装目录",
                    "Set the install directory under Settings → Languages & Frameworks → Mobile SDKs",
                )
            );
        }
    }

    fn start_run_config_with_mobile_device(&mut self, cfg: &RunConfiguration) {
        let mut exec_cfg =
            cfg.effective_with_mobile_device(self.selected_mobile_device_id.as_deref());
        self.apply_resolved_mobile_toolchain(&mut exec_cfg);
        self.terminal
            .start_bound_run_configuration_as(&cfg.identity_key(), &cfg.name, &exec_cfg);
    }

    fn apply_resolved_mobile_toolchain(&self, cfg: &mut RunConfiguration) {
        if cfg.command.contains('\\') || cfg.command.contains('/') {
            return;
        }
        let bare = cfg
            .command
            .trim()
            .trim_end_matches(".exe")
            .trim_end_matches(".bat")
            .trim_end_matches(".cmd")
            .to_ascii_lowercase();
        let replacement = match bare.as_str() {
            "flutter" => self.mobile_env.flutter_bin.clone(),
            "dart" => self
                .mobile_env
                .flutter_bin
                .as_ref()
                .and_then(|bin| sibling_toolchain_bin(bin, &["dart.bat", "dart.exe", "dart"])),
            "adb" => self.mobile_env.adb_bin.clone(),
            _ => None,
        };
        if let Some(bin) = replacement {
            cfg.command = bin.display().to_string();
        }
    }

    pub fn run_mobile_shell_command(&mut self, cmd_line: &str) {
        self.bottom_tool = Some(ToolWindowId::Terminal);
        self.focus = FocusTarget::TerminalInput;
        self.terminal.active_pty_mut().ensure_spawned();
        let line = format!("{cmd_line}\r\n");
        self.terminal.active_pty_mut().send_raw_bytes(line.as_bytes());
        self.touch_caret_activity();
    }

    pub fn launch_mobile_emulator(&mut self, avd: &str) {
        let emulator_bin = self.mobile_env.emulator_bin.clone();
        if let Some(bin) = emulator_bin {
            let mut cmd = std::process::Command::new(&bin);
            cmd.args(["-avd", avd]);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
            if cmd.spawn().is_ok() {
                self.status_notification = format!(
                    "📱 {}: {avd}",
                    self.tr("正在启动 Android 模拟器", "Launching Android Emulator")
                );
                return;
            }
        }
        self.run_mobile_shell_command(&format!("emulator -avd \"{avd}\""));
        self.status_notification = format!(
            "📱 {}: {avd}",
            self.tr("正在终端启动模拟器", "Launching emulator in terminal")
        );
    }

    pub fn toggle_run_config_execution(&mut self, idx: usize) {
        let Some(cfg) = self.run_configs.get(idx).cloned() else {
            return;
        };
        let key = cfg.identity_key();
        if self.terminal.is_run_config_running(&key) {
            self.terminal.stop_bound_run_configuration(&key);
            self.touch_caret_activity();
            self.status_notification = format!(
                "{}: {}",
                self.tr("已停止配置", "Stopped configuration"),
                cfg.name
            );
        } else {
            self.active_run_config = idx;
            self.warn_if_missing_mobile_sdk_for_cmd(&cfg);
            self.bottom_tool = Some(ToolWindowId::Terminal);
            self.focus = FocusTarget::TerminalInput;
            self.start_run_config_with_mobile_device(&cfg);
            self.touch_caret_activity();
            self.status_notification = format!(
                "{}: {}",
                self.tr("已运行配置", "Running configuration"),
                cfg.name
            );
        }
    }

    pub fn run_file_in_terminal(&mut self, path: &Path) -> bool {
        let Some(cfg) = RunConfiguration::for_file(path, &self.workspace_root) else {
            return false;
        };
        let label = cfg.name.clone();
        self.warn_if_missing_mobile_sdk_for_cmd(&cfg);
        self.upsert_run_configuration_internal(cfg.clone(), true, true);
        self.bottom_tool = Some(ToolWindowId::Terminal);
        self.focus = FocusTarget::TerminalInput;
        self.start_run_config_with_mobile_device(&cfg);
        self.touch_caret_activity();
        self.status_notification = format!(
            "{}: {} ({})",
            self.tr("已运行并加入列表", "Running & added to Run list"),
            label,
            cfg.command
        );
        true
    }

    pub fn run_gutter_entry_in_terminal(&mut self, line: usize) {
        let buf = self.active_buffer().clone();
        let runnable = self
            .cached_runnables
            .as_ref()
            .and_then(|(_, _, entries)| entries.iter().find(|r| r.line == line).cloned());
        let line_text = buf.line_text(line);

        let cfg = match runnable {
            Some(entry) if entry.command == "cargo" && entry.args.iter().any(|a| a == "test") => {
                let mut base = RunConfiguration::for_file(&buf.path, &self.workspace_root)
                    .unwrap_or_else(|| RunConfiguration {
                        name: entry.label.clone(),
                        language: buf.language,
                        command: "cargo".to_string(),
                        args: vec!["test".to_string()],
                        cwd: self.workspace_root.clone(),
                        source: RunConfigSource::Custom,
                        target_file: Some(buf.path.clone()),
                    });
                base.args.retain(|a| a != "run");
                if !base.args.iter().any(|a| a == "test") {
                    base.args.insert(0, "test".to_string());
                }
                if let Some(fn_name) = extract_identifier_after(&line_text, "fn ") {
                    base.name = format!("Test: {fn_name}");
                    base.args.push(fn_name);
                }
                base
            }
            Some(entry) if entry.command == "pytest" => {
                let mut base = RunConfiguration::for_file(&buf.path, &self.workspace_root)
                    .unwrap_or_else(|| RunConfiguration {
                        name: entry.label.clone(),
                        language: buf.language,
                        command: "pytest".to_string(),
                        args: Vec::new(),
                        cwd: self.workspace_root.clone(),
                        source: RunConfigSource::Custom,
                        target_file: Some(buf.path.clone()),
                    });
                if let Some(fn_name) = extract_identifier_after(&line_text, "def ") {
                    base.name = format!("Pytest: {fn_name}");
                    base.args.push("-k".to_string());
                    base.args.push(fn_name);
                }
                base
            }
            Some(entry) => RunConfiguration::for_file(&buf.path, &self.workspace_root)
                .unwrap_or_else(|| RunConfiguration {
                    name: entry.label,
                    language: buf.language,
                    command: entry.command,
                    args: entry.args,
                    cwd: self.workspace_root.clone(),
                    source: RunConfigSource::Custom,
                    target_file: Some(buf.path.clone()),
                }),
            None => match RunConfiguration::for_file(&buf.path, &self.workspace_root) {
                Some(c) => c,
                None => return,
            },
        };

        let name = cfg.name.clone();
        self.upsert_run_configuration_internal(cfg.clone(), true, true);
        self.bottom_tool = Some(ToolWindowId::Terminal);
        self.focus = FocusTarget::TerminalInput;
        self.terminal.execute_run_configuration(&cfg);
        self.touch_caret_activity();
        self.status_notification = format!(
            "{}: {} (第 {} 行)",
            self.tr("已运行并加入列表", "Running & added"),
            name,
            line + 1
        );
    }

    pub fn record_terminal_prompt_command_before_enter(&mut self) -> bool {
        let Some((cwd, cmd_line)) = self.terminal.active_pty_mut().record_prompt_command() else {
            return false;
        };
        let Some(cfg) = RunConfiguration::from_terminal_command(&cmd_line, &cwd) else {
            return false;
        };
        self.upsert_run_configuration_internal(cfg, false, true);
        true
    }

    pub fn open_new_run_config_modal(&mut self, initial_cmd: Option<&str>) {
        self.run_config_popup_open = false;
        self.run_config_popup_scroll = 0;
        self.modal_text_input.set_text(initial_cmd.unwrap_or(""));
        self.focus = FocusTarget::NewRunConfigModal;
        self.touch_caret_activity();
    }

    pub fn add_and_run_command_line(&mut self, cmd_line: &str) {
        let cwd = self.workspace_root.clone();
        if let Some(cfg) = RunConfiguration::from_command_line_explicit(cmd_line, &cwd) {
            let name = cfg.name.clone();
            self.upsert_run_configuration_internal(cfg.clone(), true, true);
            self.bottom_tool = Some(ToolWindowId::Terminal);
            self.focus = FocusTarget::TerminalInput;
            self.terminal.execute_run_configuration(&cfg);
            self.touch_caret_activity();
            self.status_notification = format!(
                "{}: {}",
                self.tr("已添加并运行命令", "Added & running command"),
                name
            );
        } else {
            self.focus = FocusTarget::Editor;
        }
    }

    pub fn remove_custom_run_config(&mut self, idx: usize) {
        if idx >= self.run_configs.len() || self.run_configs[idx].source != RunConfigSource::Custom {
            return;
        }
        let removed_name = self.run_configs[idx].name.clone();
        self.run_configs.remove(idx);
        if self.run_configs.is_empty() {
            self.run_configs = RunConfiguration::detect_for_workspace(&self.workspace_root);
            self.active_run_config = 0;
        } else if self.active_run_config >= self.run_configs.len() {
            self.active_run_config = self.run_configs.len() - 1;
        } else if idx < self.active_run_config {
            self.active_run_config -= 1;
        }
        self.save_workspace_state();
        self.status_notification = format!(
            "{}: {}",
            self.tr("已移除运行配置", "Removed run configuration"),
            removed_name
        );
    }

    pub fn cancel_edit_message_rewind_modal(&mut self) {
        if let Some(pending) = self.pending_edit_rewind.take() {
            self.editing_sent_msg_idx = Some(pending.msg_idx);
            self.agent_chat_input.set_text(&pending.new_prompt);
        }
        self.focus = FocusTarget::AgentChatInput;
        self.touch_caret_activity();
    }

    pub fn submit_agent_prompt(&mut self, prompt: String) {
        if let Some(msg_idx) = self.editing_sent_msg_idx {
            if !prompt.trim().is_empty() {
                self.open_edit_message_rewind_modal(msg_idx, prompt);
            } else {
                self.cancel_edit_sent_user_message();
            }
            return;
        }
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
        self.cancel_edit_sent_user_message();
        let draft = self.agent_chat_input.as_str().to_string();
        self.agent_hub.active_session_mut().draft_prompt = draft;
        let idx = self.agent_hub.new_session();
        let title = self.agent_hub.sessions[idx].title.clone();
        self.agent_chat_input.clear();
        self.clear_agent_chat_selection();
        self.collapsed_code_blocks.clear();
        self.expanded_message_thoughts.clear();
        self.expanded_message_file_changes.clear();
        self.expanded_composer_snippet_id = None;
        self.agent_sessions_popup_open = false;
        self.agent_mode_popup_open = false;
        self.agent_changed_files_expanded = false;
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
        self.cancel_edit_sent_user_message();
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
            self.expanded_message_file_changes.clear();
            self.expanded_composer_snippet_id = None;
            self.agent_sessions_popup_open = false;
            self.agent_mode_popup_open = false;
            self.agent_changed_files_expanded = false;
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
            self.expanded_message_file_changes.clear();
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
        self.cache_decoded_image_preview(path.to_path_buf(), Arc::clone(&arc));
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
        let in_agent_zone =
            (self.right_agent_open && self.right_pane_rect.contains(lx, ly)) || is_img;
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
        self.save_global_settings();
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
        if let Some(pos) = self
            .open_buffers
            .iter()
            .position(|b| b.path == patch.file_path)
        {
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
        if let Some(pos) = self
            .open_buffers
            .iter()
            .position(|b| b.path == patch.file_path)
        {
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

    /// 命中标题栏上的最小化 / 最大化 / 关闭按钮。
    pub fn window_control_at(&self, x: i32, y: i32) -> Option<WindowControlCmd> {
        self.hit_regions
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(x, y))
            .and_then(|(_, action)| match action {
                HitAction::WindowControl(cmd) => Some(*cmd),
                _ => None,
            })
    }

    pub fn arm_window_control(&mut self, cmd: WindowControlCmd) {
        self.armed_window_control = Some(cmd);
    }

    /// 鼠标抬起时，只有指针仍在按下的那个按钮上才执行。
    pub fn release_armed_window_control(&mut self, x: i32, y: i32) -> Option<WindowControlCmd> {
        let armed = self.armed_window_control.take()?;
        match self.window_control_at(x, y) {
            Some(hit) if hit == armed => Some(armed),
            _ => None,
        }
    }

    pub fn is_point_in_toolbar_blank(&self, x: i32, y: i32) -> bool {
        if y < 0 || y >= 38 {
            return false;
        }
        !self.hit_regions.iter().any(|(r, _)| r.contains(x, y))
    }

    /// 收起顶部主菜单并恢复项目名称与分支显示
    pub fn close_top_menu(&mut self) -> bool {
        let was_open = self.hamburger_menu_expanded || self.active_menu_category.is_some();
        self.hamburger_menu_expanded = false;
        self.active_menu_category = None;
        self.menu_trigger_and_bar_rect = Rect::new(0, 0, 0, 0);
        self.menu_dropdown_rect = Rect::new(0, 0, 0, 0);
        was_open
    }

    /// 鼠标移动悬浮追踪：支持菜单栏划动切换展开子菜单 + 移出菜单区域自动恢复标题 + 分割线/控件悬浮高亮按需重绘
    pub fn handle_mouse_move(&mut self, x: i32, y: i32) -> bool {
        self.hover_pos = (x, y);
        let mut changed = false;

        // 展开主菜单后，鼠标移动经过任意一级菜单分类立即展开对应子菜单；
        // 当鼠标移出菜单条及当前下拉子菜单区域后，自动收起菜单并恢复标题显示
        if self.hamburger_menu_expanded {
            if let Some((cat, _)) = self
                .menu_category_rects
                .iter()
                .find(|(_, r)| r.contains(x, y))
            {
                if self.active_menu_category != Some(*cat) {
                    self.active_menu_category = Some(*cat);
                    changed = true;
                }
            } else if !self.menu_category_rects.is_empty() {
                let in_bar = self.menu_trigger_and_bar_rect.w > 0
                    && self.menu_trigger_and_bar_rect.contains(x, y);
                let in_dropdown = self.active_menu_category.is_some()
                    && self.menu_dropdown_rect.w > 0
                    && self.menu_dropdown_rect.contains(x, y);
                if !in_bar && !in_dropdown && self.close_top_menu() {
                    changed = true;
                }
            }
        }

        let hover_idx = self.hit_regions.iter().rposition(|(r, _)| r.contains(x, y));
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
        let bottom_h = if self.pane_is_open(self.dock_bottom) {
            self.bottom_panel_height
        } else {
            0
        };
        let upper_h = (body_h - bottom_h).max(56);
        let upper_bottom = top_bar_h + upper_h;

        // 遮罩层/菜单激活时不穿透命中底层分割线
        if self.context_menu.is_some()
            || matches!(
                self.focus,
                FocusTarget::SearchEverywhereModal
                    | FocusTarget::FindInFilesModal
                    | FocusTarget::NewFileModal
                    | FocusTarget::NewFolderModal
                    | FocusTarget::NewRunConfigModal
                    | FocusTarget::NewGitBranchModal
                    | FocusTarget::NewGitTagModal
                    | FocusTarget::NewGitStashModal
                    | FocusTarget::GitAddRemoteModal
                    | FocusTarget::GitMergeModal
                    | FocusTarget::GitRebaseModal
                    | FocusTarget::GitUnstashModal
                    | FocusTarget::GitResetModal
                    | FocusTarget::GitRollbackModal
                    | FocusTarget::GitRemotesModal
                    | FocusTarget::RenamePathModal
                    | FocusTarget::SettingsModal
                    | FocusTarget::AiConfigModal(_)
            )
            || self.active_overlay_rects.iter().any(|r| r.contains(x, y))
        {
            return None;
        }

        if self.pane_is_open(self.dock_left) && y > top_bar_h && y < upper_bottom {
            let split_x = stripe_w + self.left_panel_width;
            if (x - split_x).abs() <= 8 {
                return Some(DragSplitter::LeftPanel);
            }
        }
        if self.pane_is_open(self.dock_right) && y > top_bar_h && y < upper_bottom {
            let split_x = win_w - stripe_w - self.right_panel_width;
            if (x - split_x).abs() <= 8 {
                return Some(DragSplitter::RightPanel);
            }
        }
        if self.pane_is_open(self.dock_bottom) && x > stripe_w && x < win_w - stripe_w {
            if (y - upper_bottom).abs() <= 8 {
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
        if self.active_dock_drag.is_some() {
            return IdeCursorKind::Pointer;
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

        let overlay_hits: &[(Rect, HitAction)] = if self.overlay_hit_start <= self.hit_regions.len()
        {
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
                | FocusTarget::NewRunConfigModal
                | FocusTarget::NewGitBranchModal
                | FocusTarget::NewGitTagModal
                | FocusTarget::NewGitStashModal
                | FocusTarget::GitAddRemoteModal
                | FocusTarget::GitMergeModal
                | FocusTarget::GitRebaseModal
                | FocusTarget::GitUnstashModal
                | FocusTarget::GitResetModal
                | FocusTarget::GitRollbackModal
                | FocusTarget::GitRemotesModal
                | FocusTarget::RenamePathModal
                | FocusTarget::SettingsModal
                | FocusTarget::AiConfigModal(_)
                | FocusTarget::OpenProjectChoiceModal
                | FocusTarget::EditMessageRewindModal
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
                | FocusTarget::NewRunConfigModal
                | FocusTarget::NewGitBranchModal
                | FocusTarget::NewGitTagModal
                | FocusTarget::NewGitStashModal
                | FocusTarget::GitAddRemoteModal
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
                FocusTarget::SettingsModal => {
                    if self.settings_search_rect.contains(x, y)
                        || self
                            .mobile_sdk_field_rects
                            .iter()
                            .any(|r| r.w > 0 && r.contains(x, y))
                    {
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
            IdeAction::GitAnnotateBlame => self.git_blame_visible,
            IdeAction::GitShowLog => {
                self.bottom_tool == Some(ToolWindowId::GitLog)
                    && self.git_file_history_filter.is_none()
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
                buf.replace_line_range(
                    session.start_line,
                    session.end_line,
                    &session.proposed_code,
                );
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
            let mut restored = Vec::with_capacity(ckpt.file_backups.len());
            for (path, backup) in ckpt.file_backups {
                match backup {
                    Some(content) => {
                        let _ = std::fs::write(&path, &content);
                    }
                    None => {
                        let _ = std::fs::remove_file(&path);
                    }
                }
                restored.push(path);
            }
            self.reload_paths_after_revert(&restored);
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
        let mobile_report = self
            .mobile_probe_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(report) = mobile_report {
            self.mobile_probe_rx = None;
            self.apply_mobile_environment_report(report);
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
                FolderPickMode::MobileSdk(slot) => {
                    self.folder_pick_mode = FolderPickMode::AskUser;
                    self.apply_picked_mobile_sdk_dir(slot, picked);
                }
            }
            changed = true;
        }
        changed
    }

    pub fn active_terminal_revision(&self) -> u64 {
        self.terminal.total_revision()
    }

    pub fn ensure_editor_cursor_visible(&mut self) {
        let (cursor_line, cursor_col, line_str) = {
            let buf = self.active_buffer();
            (
                buf.cursor_line,
                buf.cursor_col,
                buf.line_text(buf.cursor_line),
            )
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
            | FocusTarget::NewRunConfigModal
            | FocusTarget::NewGitBranchModal
            | FocusTarget::NewGitTagModal
            | FocusTarget::NewGitStashModal
            | FocusTarget::GitAddRemoteModal
            | FocusTarget::RenamePathModal => Some(&self.modal_text_input),
            FocusTarget::SettingsModal => match self.settings_sdk_focus {
                Some(0) => Some(&self.mobile_sdk_flutter),
                Some(1) => Some(&self.mobile_sdk_android),
                Some(2) => Some(&self.mobile_sdk_jdk),
                _ => Some(&self.settings_query_input),
            },
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
            | FocusTarget::NewRunConfigModal
            | FocusTarget::NewGitBranchModal
            | FocusTarget::NewGitTagModal
            | FocusTarget::NewGitStashModal
            | FocusTarget::GitAddRemoteModal
            | FocusTarget::RenamePathModal => Some(&mut self.modal_text_input),
            FocusTarget::SettingsModal => match self.settings_sdk_focus {
                Some(0) => Some(&mut self.mobile_sdk_flutter),
                Some(1) => Some(&mut self.mobile_sdk_android),
                Some(2) => Some(&mut self.mobile_sdk_jdk),
                _ => Some(&mut self.settings_query_input),
            },
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
            FocusTarget::SettingsModal => {
                if self.settings_sdk_focus.is_some() {
                    return;
                }
                self.settings_nav_scroll = 0;
                let query = self.settings_query_input.as_str().trim();
                if query.is_empty() {
                    return;
                }
                if let Some(page) = self
                    .visible_settings_nav()
                    .into_iter()
                    .find_map(|item| item.page)
                {
                    if self.settings_page != page {
                        self.settings_page = page;
                        self.modal_scroll = 0;
                    }
                }
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
            if self.open_buffers.is_empty() {
                return;
            }
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
        let line_idx = (self.terminal_scroll_anchor_line + (rel_y / line_h) as usize)
            .min(snap.lines.len() - 1);
        let line_str = snap.lines[line_idx].raw_text();
        let rel_x = (x - (self.terminal_content_rect.x + 12)).max(0);
        let col = canvas.measure_caret_col_in_line(&line_str, rel_x, FontStyle::CodeMono);
        Some((line_idx, col))
    }

    pub fn end_mouse_text_drag(&mut self) {
        self.mouse_drag_target = None;
    }

    pub fn handle_input_mouse_drag(&mut self, x: i32, y: i32, canvas: &SurfaceCanvas) -> bool {
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
                let idx =
                    self.ctrl_k_input
                        .hit_test_singleline(canvas, x, start_x, FontStyle::UiSmall);
                self.ctrl_k_input.set_cursor_at(idx, true);
                self.touch_caret_activity();
                return true;
            }
            FocusTarget::GitCommitInput => {
                let start_x = self.git_commit_input_box_rect.x + 8;
                let idx = self.git_commit_input.hit_test_singleline(
                    canvas,
                    x,
                    start_x,
                    FontStyle::UiSmall,
                );
                self.git_commit_input.set_cursor_at(idx, true);
                self.touch_caret_activity();
                return true;
            }
            FocusTarget::GitLogSearchInput => {
                let start_x = self.git_log_search_rect.x + 22;
                let idx = self.git_log_search_input.hit_test_singleline(
                    canvas,
                    x,
                    start_x,
                    FontStyle::UiSmall,
                );
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
            | FocusTarget::NewRunConfigModal
            | FocusTarget::NewGitBranchModal
            | FocusTarget::NewGitTagModal
            | FocusTarget::NewGitStashModal
            | FocusTarget::GitAddRemoteModal
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
            FocusTarget::SettingsModal => {
                if let Some(slot) = self.settings_sdk_focus {
                    let rect = self.mobile_sdk_field_rects[slot as usize];
                    let start_x = rect.x + 8;
                    let field = self.mobile_sdk_input_mut(slot);
                    let idx = field.hit_test_singleline(canvas, x, start_x, FontStyle::UiRegular);
                    field.set_cursor_at(idx, true);
                } else {
                    let idx = self.settings_query_input.hit_test_singleline(
                        canvas,
                        x,
                        self.settings_search_text_x,
                        FontStyle::UiRegular,
                    );
                    self.settings_query_input.set_cursor_at(idx, true);
                }
                self.touch_caret_activity();
                return true;
            }
            _ => {}
        }
        false
    }

    pub fn handle_input_double_click(&mut self, x: i32, y: i32, canvas: &SurfaceCanvas) -> bool {
        if self.left_tool == Some(ToolWindowId::Commit) && self.left_pane_rect.contains(x, y) {
            if let Some((row_rect, path, _)) = self
                .git_change_row_rects
                .iter()
                .find(|(r, _, _)| r.contains(x, y))
                .cloned()
            {
                if x < row_rect.x + row_rect.w - 86 {
                    self.open_git_working_diff_in_editor(&path);
                    return true;
                }
            }
        }
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
            FocusTarget::SettingsModal
                if self
                    .settings_sdk_focus
                    .is_some_and(|slot| {
                        self.mobile_sdk_field_rects
                            .get(slot as usize)
                            .is_some_and(|r| r.w > 0 && r.contains(x, y))
                    }) =>
            {
                let slot = self.settings_sdk_focus.unwrap_or(0);
                let start_x = self.mobile_sdk_field_rects[slot as usize].x + 8;
                let field = self.mobile_sdk_input_mut(slot);
                let idx = field.hit_test_singleline(canvas, x, start_x, FontStyle::UiRegular);
                field.select_word_at(idx);
                self.touch_caret_activity();
                true
            }
            FocusTarget::SettingsModal if self.settings_search_rect.contains(x, y) => {
                let idx = self.settings_query_input.hit_test_singleline(
                    canvas,
                    x,
                    self.settings_search_text_x,
                    FontStyle::UiRegular,
                );
                self.settings_query_input.select_word_at(idx);
                self.touch_caret_activity();
                true
            }
            _ => false,
        }
    }

    pub fn handle_input_triple_click(&mut self, x: i32, y: i32, canvas: &SurfaceCanvas) -> bool {
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
            | FocusTarget::NewRunConfigModal
            | FocusTarget::NewGitBranchModal
            | FocusTarget::NewGitTagModal
            | FocusTarget::NewGitStashModal
            | FocusTarget::GitAddRemoteModal
            | FocusTarget::RenamePathModal
            | FocusTarget::AiConfigModal(_)
            | FocusTarget::SettingsModal => self.select_all_at_focus(),
            _ => false,
        }
    }

    /// 通用鼠标滚轮滚动：自动识别当前指针所在面板（项目树 / Git更改 / 代码编辑器 / AI 控制台 / 原生终端 / Git 历史库 / 弹窗）
    #[allow(dead_code)]
    pub fn handle_mouse_wheel(&mut self, x: i32, y: i32, lines_delta: i32) {
        self.handle_mouse_wheel_2d(x, y, lines_delta, 0);
    }

    /// 二维平滑滚轮滚动：支持垂直加速滚动与代码编辑器水平滚动 (`Shift+滚轮` 或触控板横向滑动)
    pub fn handle_mouse_wheel_2d(&mut self, x: i32, y: i32, lines_delta: i32, horiz_px_delta: i32) {
        if lines_delta == 0 && horiz_px_delta == 0 {
            return;
        }
        if self.run_config_popup_open && self.run_config_popup_rect.contains(x, y) {
            let max_scroll = self.run_configs.len().saturating_sub(9);
            if lines_delta < 0 {
                self.run_config_popup_scroll = self
                    .run_config_popup_scroll
                    .saturating_sub((-lines_delta) as usize);
            } else if lines_delta > 0 {
                self.run_config_popup_scroll =
                    (self.run_config_popup_scroll + lines_delta as usize).min(max_scroll);
            }
            return;
        }
        match self.focus {
            FocusTarget::SettingsModal => {
                if self.settings_nav_rect.h > 0 && self.settings_nav_rect.contains(x, y) {
                    if lines_delta < 0 {
                        self.settings_nav_scroll = self
                            .settings_nav_scroll
                            .saturating_sub((-lines_delta) as usize);
                    } else if lines_delta > 0 {
                        self.settings_nav_scroll = (self.settings_nav_scroll
                            + lines_delta as usize)
                            .min(self.settings_nav_max_scroll);
                    }
                } else if lines_delta < 0 {
                    self.modal_scroll = self.modal_scroll.saturating_sub((-lines_delta) as usize);
                } else if lines_delta > 0 {
                    self.modal_scroll = (self.modal_scroll + lines_delta as usize)
                        .min(self.settings_content_max_scroll);
                }
                return;
            }
            FocusTarget::SearchEverywhereModal | FocusTarget::FindInFilesModal => {
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
                    self.git_changes_scroll = self
                        .git_changes_scroll
                        .saturating_sub((-lines_delta) as usize);
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
                        self.git_files_scroll = self
                            .git_files_scroll
                            .saturating_sub((-lines_delta) as usize);
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
        } else if let Some(ref mut ed_diff) = self.active_editor_diff {
            let max_line = ed_diff.diff_lines.len().saturating_sub(5);
            if lines_delta < 0 {
                ed_diff.scroll = ed_diff.scroll.saturating_sub((-lines_delta) as usize);
            } else if lines_delta > 0 {
                ed_diff.scroll = (ed_diff.scroll + lines_delta as usize).min(max_line);
            }
            if horiz_px_delta != 0 {
                let limit_hx = if self.editor_horiz_scroll_info.visible > 0 {
                    self.editor_horiz_scroll_info
                        .total
                        .saturating_sub(self.editor_horiz_scroll_info.visible)
                        as i32
                } else {
                    2400
                };
                ed_diff.scroll_x = (ed_diff.scroll_x + horiz_px_delta).clamp(0, limit_hx);
            }
        } else {
            let max_line = self.active_buffer().line_count().saturating_sub(5);
            if lines_delta < 0 {
                self.scroll_line = self.scroll_line.saturating_sub((-lines_delta) as usize);
            } else if lines_delta > 0 {
                self.scroll_line = (self.scroll_line + lines_delta as usize).min(max_line);
            }
            if horiz_px_delta != 0 {
                let limit_hx = if self.editor_horiz_scroll_info.visible > 0 {
                    self.editor_horiz_scroll_info
                        .total
                        .saturating_sub(self.editor_horiz_scroll_info.visible)
                        as i32
                } else {
                    2400
                };
                self.editor_scroll_x = (self.editor_scroll_x + horiz_px_delta).clamp(0, limit_hx);
            }
        }
    }

    /// 检测鼠标按下位置是否命中任意面板的垂直/水平滚动条轨道
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
        let check_horiz = |info: ScrollTrackInfo| -> bool {
            info.total > info.visible
                && info.track.w > 0
                && x >= info.track.x
                && x < info.track.x + info.track.w
                && y >= info.track.y - hit_padding
                && y < info.track.y + info.track.h + hit_padding
        };
        if check(self.left_scroll_info) {
            self.active_scrollbar_drag = Some(DragScrollbar::LeftTree);
            self.update_scrollbar_drag(x, y);
            return true;
        }
        if check(self.editor_scroll_info) {
            self.active_scrollbar_drag = Some(DragScrollbar::CenterEditor);
            self.update_scrollbar_drag(x, y);
            return true;
        }
        if check_horiz(self.editor_horiz_scroll_info) {
            self.active_scrollbar_drag = Some(DragScrollbar::CenterEditorHoriz);
            self.update_scrollbar_drag(x, y);
            return true;
        }
        if check(self.right_scroll_info) {
            self.active_scrollbar_drag = Some(DragScrollbar::RightPanel);
            self.update_scrollbar_drag(x, y);
            return true;
        }
        if self.bottom_tool == Some(ToolWindowId::GitLog) {
            if check(self.git_log_commits_scroll_info) {
                self.active_scrollbar_drag = Some(DragScrollbar::GitLogCommits);
                self.update_scrollbar_drag(x, y);
                return true;
            }
            if check(self.git_log_diff_scroll_info) {
                self.active_scrollbar_drag = Some(DragScrollbar::GitLogDiff);
                self.update_scrollbar_drag(x, y);
                return true;
            }
        } else if check(self.terminal_scroll_info) {
            self.active_scrollbar_drag = Some(DragScrollbar::BottomTerminal);
            self.update_scrollbar_drag(x, y);
            return true;
        }
        false
    }

    pub fn update_scrollbar_drag(&mut self, x: i32, y: i32) -> bool {
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
                if let Some(ref mut ed_diff) = self.active_editor_diff {
                    ed_diff.scroll = ratio_for(self.editor_scroll_info);
                } else {
                    self.scroll_line = ratio_for(self.editor_scroll_info);
                }
            }
            DragScrollbar::CenterEditorHoriz => {
                let info = self.editor_horiz_scroll_info;
                let max_s = info.total.saturating_sub(info.visible).max(1);
                let rel =
                    ((x - info.track.x) as f32 / (info.track.w.max(1) as f32)).clamp(0.0, 1.0);
                let next_x = (rel * max_s as f32).round() as i32;
                if let Some(ref mut ed_diff) = self.active_editor_diff {
                    ed_diff.scroll_x = next_x;
                } else {
                    self.editor_scroll_x = next_x;
                }
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

    pub fn submit_git_commit(&mut self, push_after: bool) {
        let msg = self.git_commit_input.as_str().trim().to_string();
        if msg.is_empty() {
            self.focus = FocusTarget::GitCommitInput;
            self.touch_caret_activity();
            self.status_notification = self
                .tr(
                    "请先输入 Git Commit 提交说明",
                    "Please enter a Git commit message",
                )
                .to_string();
            return;
        }
        let checked = self.git_engine.checked_commit_files();
        if checked.is_empty() && !self.git_amend_commit {
            self.status_notification = self
                .tr(
                    "请先勾选需要提交的文件",
                    "Please check at least one file to commit",
                )
                .to_string();
            return;
        }
        let count = checked.len();
        self.git_engine.request_commit_selected_files(
            &msg,
            self.git_amend_commit,
            push_after,
            checked,
        );
        self.status_notification = if push_after {
            format!(
                "{} ({count})...",
                self.tr(
                    "正在后台提交并推送勾选的 Git 变更",
                    "Committing and pushing selected changes"
                )
            )
        } else {
            format!(
                "{} ({count})...",
                self.tr("正在后台提交勾选的 Git 变更", "Committing selected changes")
            )
        };
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
                let (min, max) = self.span_limits_for_drag(DockEdge::Left, win_w, win_h);
                self.left_panel_width = (x - stripe_w).clamp(min, max);
            }
            DragSplitter::RightPanel => {
                let (min, max) = self.span_limits_for_drag(DockEdge::Right, win_w, win_h);
                self.right_panel_width = (win_w - stripe_w - x).clamp(min, max);
            }
            DragSplitter::BottomPanel => {
                let (min, max) = self.span_limits_for_drag(DockEdge::Bottom, win_w, win_h);
                self.bottom_panel_height = (win_h - status_h - y).clamp(min, max);
            }
        }
        true
    }

    fn pane_is_open(&self, pane: DockPane) -> bool {
        match pane {
            DockPane::Project => self.left_tool.is_some(),
            DockPane::Agent => self.right_agent_open,
            DockPane::Tooling => self.bottom_tool.is_some(),
        }
    }

    fn pane_on(&self, edge: DockEdge) -> DockPane {
        match edge {
            DockEdge::Left => self.dock_left,
            DockEdge::Right => self.dock_right,
            DockEdge::Bottom => self.dock_bottom,
        }
    }

    fn set_pane_on(&mut self, edge: DockEdge, pane: DockPane) {
        match edge {
            DockEdge::Left => self.dock_left = pane,
            DockEdge::Right => self.dock_right = pane,
            DockEdge::Bottom => self.dock_bottom = pane,
        }
    }

    fn normalize_docks(&mut self) {
        let all = [self.dock_left, self.dock_right, self.dock_bottom];
        let mut seen = [false; 3];
        let mut ok = true;
        for pane in all {
            let idx = pane as usize;
            if idx > 2 || seen[idx] {
                ok = false;
                break;
            }
            seen[idx] = true;
        }
        if !ok {
            self.dock_left = DockPane::Project;
            self.dock_right = DockPane::Agent;
            self.dock_bottom = DockPane::Tooling;
        }
    }

    fn swap_dock(&mut self, a: DockEdge, b: DockEdge) {
        if a == b {
            return;
        }
        let pa = self.pane_on(a);
        let pb = self.pane_on(b);
        self.set_pane_on(a, pb);
        self.set_pane_on(b, pa);
    }

    fn span_limits_for_drag(&self, edge: DockEdge, win_w: i32, win_h: i32) -> (i32, i32) {
        let stripe = 84;
        let min_editor_w = 72;
        let min_editor_h = 56;
        match edge {
            DockEdge::Left => {
                let right = if self.pane_is_open(self.dock_right) {
                    self.right_panel_width
                } else {
                    0
                };
                let min = 48;
                let max = (win_w - stripe - right - min_editor_w).max(min);
                (min, max)
            }
            DockEdge::Right => {
                let left = if self.pane_is_open(self.dock_left) {
                    self.left_panel_width
                } else {
                    0
                };
                let min = 64;
                let max = (win_w - stripe - left - min_editor_w).max(min);
                (min, max)
            }
            DockEdge::Bottom => {
                let body = (win_h - 38 - 26).max(min_editor_h + 40);
                let min = 40;
                let max = (body - min_editor_h).max(min);
                (min, max)
            }
        }
    }

    fn clamp_dock_spans(&mut self, win_w: i32, win_h: i32) {
        let (lmin, lmax) = self.span_limits_for_drag(DockEdge::Left, win_w, win_h);
        self.left_panel_width = self.left_panel_width.clamp(lmin, lmax);
        let (rmin, rmax) = self.span_limits_for_drag(DockEdge::Right, win_w, win_h);
        self.right_panel_width = self.right_panel_width.clamp(rmin, rmax);
        let (lmin, lmax) = self.span_limits_for_drag(DockEdge::Left, win_w, win_h);
        self.left_panel_width = self.left_panel_width.clamp(lmin, lmax);
        let (bmin, bmax) = self.span_limits_for_drag(DockEdge::Bottom, win_w, win_h);
        self.bottom_panel_height = self.bottom_panel_height.clamp(bmin, bmax);
    }

    fn edge_of_pane(&self, pane: DockPane) -> DockEdge {
        if self.dock_left == pane {
            DockEdge::Left
        } else if self.dock_right == pane {
            DockEdge::Right
        } else {
            DockEdge::Bottom
        }
    }

    fn dock_edge_at(&self, x: i32, y: i32, win_w: i32, win_h: i32) -> Option<DockEdge> {
        let top = 38;
        let status = 26;
        let stripe = 42;
        if y <= top || y >= win_h - status || x <= stripe || x >= win_w - stripe {
            return None;
        }
        if self.left_pane_rect.w > 20 && self.left_pane_rect.contains(x, y) {
            return Some(self.edge_of_pane(DockPane::Project));
        }
        if self.right_pane_rect.w > 20 && self.right_pane_rect.contains(x, y) {
            return Some(self.edge_of_pane(DockPane::Agent));
        }
        if self.bottom_pane_rect.w > 20 && self.bottom_pane_rect.contains(x, y) {
            return Some(self.edge_of_pane(DockPane::Tooling));
        }
        let body_x = stripe;
        let body_r = win_w - stripe;
        let body_b = win_h - status;
        let dl = x - body_x;
        let dr = body_r - x;
        let db = body_b - y;
        let mut best = DockEdge::Left;
        let mut dist = dl;
        if dr < dist {
            best = DockEdge::Right;
            dist = dr;
        }
        if db < dist {
            best = DockEdge::Bottom;
            dist = db;
        }
        if dist <= 220 {
            Some(best)
        } else {
            None
        }
    }

    pub fn update_dock_drag(&mut self, x: i32, y: i32, win_w: i32, win_h: i32) -> bool {
        if let Some((pane, ox, oy)) = self.pending_dock_drag {
            if (x - ox).abs() + (y - oy).abs() >= 6 {
                self.pending_dock_drag = None;
                self.pending_ide_drag = None;
                self.active_ide_drag = None;
                self.active_dock_drag = Some(pane);
                self.dock_drag_pos = (x, y);
                self.dock_drop_edge = self.dock_edge_at(x, y, win_w, win_h);
                self.status_notification = self
                    .tr(
                        "拖到左、右或底边松开，即可移动该分区",
                        "Release on the left, right, or bottom edge to move this pane",
                    )
                    .to_string();
                return true;
            }
            return false;
        }
        if self.active_dock_drag.is_some() {
            self.dock_drag_pos = (x, y);
            self.dock_drop_edge = self.dock_edge_at(x, y, win_w, win_h);
            return true;
        }
        false
    }

    pub fn end_dock_drag(&mut self, x: i32, y: i32, win_w: i32, win_h: i32) {
        let pane = self.active_dock_drag.take();
        self.pending_dock_drag = None;
        let Some(pane) = pane else {
            self.dock_drop_edge = None;
            return;
        };
        let target = self
            .dock_edge_at(x, y, win_w, win_h)
            .or(self.dock_drop_edge);
        self.dock_drop_edge = None;
        let Some(target) = target else {
            return;
        };
        let from = self.edge_of_pane(pane);
        if from == target {
            return;
        }
        let displaced = self.pane_on(target);
        self.swap_dock(from, target);
        let zh = self.ui_lang == UiLanguage::ZhCn;
        self.status_notification = format!(
            "{}: {} ↔ {}",
            self.tr("已移动分区", "Moved pane"),
            pane.title(zh),
            displaced.title(zh)
        );
        self.save_global_settings();
    }

    pub(crate) fn arm_pane_header_drag(&mut self, canvas: &mut SurfaceCanvas, rect: Rect, pane: DockPane) {
        if rect.w < 24 || rect.h < 24 {
            return;
        }
        let strip = Rect::new(rect.x + 2, rect.y + 2, (rect.w - 4).max(8), 28);
        self.hit_regions
            .push((strip, HitAction::DragDockPane(pane)));
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let grip = Rect::new(rect.x + 6, rect.y + 8, 12, 16);
        let hot = grip.contains(hx, hy) || self.active_dock_drag == Some(pane);
        let col = if hot { t.accent_blue } else { t.text_muted };
        for row in 0..3 {
            for col_i in 0..2 {
                canvas.fill_rect(
                    Rect::new(grip.x + 1 + col_i * 5, grip.y + 1 + row * 5, 2, 2),
                    col,
                );
            }
        }
    }

    fn place_dock_pane(&mut self, canvas: &mut SurfaceCanvas, pane: DockPane, rect: Rect) {
        if rect.w <= 8 || rect.h <= 8 {
            return;
        }
        match pane {
            DockPane::Project => {
                self.left_pane_rect = rect;
                if let Some(tool) = self.left_tool {
                    self.render_left_tool_window(canvas, rect, tool);
                }
            }
            DockPane::Agent => {
                self.right_pane_rect = rect;
                self.render_right_agent_hub(canvas, rect);
            }
            DockPane::Tooling => {
                self.bottom_pane_rect = rect;
                if let Some(tool) = self.bottom_tool {
                    self.render_bottom_tool_window(canvas, rect, tool);
                }
            }
        }
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
            self.close_top_menu();
            self.git_branch_popup_open = false;
            self.agent_provider_popup_open = false;
            self.agent_model_popup_open = false;
            self.agent_mode_popup_open = false;
            self.agent_sessions_popup_open = false;
            self.run_config_popup_open = false;
        self.run_config_popup_scroll = 0;
            self.mobile_popup_open = false;
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

        // 点击非菜单控制区（含菜单项、编辑器等）时自动收起顶部菜单并恢复标题显示
        match &act {
            HitAction::OpenMenuCategory(_) | HitAction::ToggleHamburgerMenu => {}
            _ => {
                self.close_top_menu();
            }
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
            HitAction::ToggleRunConfigPopup
            | HitAction::ToggleRunConfigAt(_)
            | HitAction::RemoveRunConfig(_) => {}
            _ => {
                self.run_config_popup_open = false;
                self.run_config_popup_scroll = 0;
            }
        }
        match &act {
            HitAction::ToggleMobilePopup
            | HitAction::RefreshMobileEnv
            | HitAction::SelectMobileDevice(_)
            | HitAction::LaunchMobileEmulator(_)
            | HitAction::RunMobileCommand(_)
            | HitAction::InstallMobileSdkCmd(_, _)
            | HitAction::OpenMobileDownloadUrl(_) => {}
            _ => {
                self.mobile_popup_open = false;
            }
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
                self.armed_window_control = Some(cmd);
            }
            HitAction::ToggleHamburgerMenu => {
                self.hamburger_menu_expanded = !self.hamburger_menu_expanded;
                if self.hamburger_menu_expanded {
                    self.project_popup_open = false;
                    self.git_branch_popup_open = false;
                } else {
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
                if self.run_config_popup_open && self.active_run_config >= 9 {
                    let max_scroll = self.run_configs.len().saturating_sub(9);
                    self.run_config_popup_scroll =
                        self.active_run_config.saturating_sub(4).min(max_scroll);
                } else if !self.run_config_popup_open {
                    self.run_config_popup_scroll = 0;
                }
            }
            HitAction::SelectRunConfig(idx) => {
                if idx < self.run_configs.len() {
                    self.active_run_config = idx;
                    let name = self.run_configs[idx].name.clone();
                    self.save_workspace_state();
                    self.status_notification = format!("已切换运行配置: {name}");
                }
                self.run_config_popup_open = false;
                self.run_config_popup_scroll = 0;
            }
            HitAction::RemoveRunConfig(idx) => {
                self.remove_custom_run_config(idx);
            }
            HitAction::ToggleRunConfigAt(idx) => {
                self.toggle_run_config_execution(idx);
            }
            HitAction::OpenNewRunConfigModal => {
                self.open_new_run_config_modal(None);
            }
            HitAction::ToggleMobilePopup => {
                self.mobile_popup_open = !self.mobile_popup_open;
                if self.mobile_popup_open
                    && self.mobile_env.sdk_tools.is_empty()
                    && !self.mobile_env.probing
                {
                    self.refresh_mobile_environment_async();
                }
            }
            HitAction::RefreshMobileEnv => {
                self.commit_mobile_sdk_locations_from_inputs();
                self.mobile_missing_prompted = false;
                self.refresh_mobile_environment_async();
                self.status_notification = self
                    .tr(
                        "正在刷新手机设备列表 (ADB / Flutter) 与本地 SDK...",
                        "Refreshing mobile devices (ADB / Flutter) & local SDKs...",
                    )
                    .to_string();
            }
            HitAction::SelectMobileDevice(id) => {
                self.selected_mobile_device_id = Some(id.clone());
                let name = self
                    .mobile_env
                    .devices
                    .iter()
                    .find(|d| d.id == id)
                    .map(|d| d.name.as_str())
                    .unwrap_or(id.as_str());
                self.status_notification = format!(
                    "📱 {}: {} ({})",
                    self.tr("已选定目标移动设备", "Selected target mobile device"),
                    name,
                    id
                );
                self.mobile_popup_open = false;
            }
            HitAction::LaunchMobileEmulator(avd) => {
                self.launch_mobile_emulator(&avd);
                self.mobile_popup_open = false;
            }
            HitAction::RunMobileCommand(cmd) => {
                self.run_mobile_shell_command(&cmd);
                self.status_notification =
                    format!("▶ {}: {cmd}", self.tr("已在终端执行", "Executed in terminal"));
                self.mobile_popup_open = false;
            }
            HitAction::InstallMobileSdkCmd(label, cmd) => {
                self.run_mobile_shell_command(&cmd);
                self.status_notification = format!(
                    "⚙ {}: {label} ({cmd})",
                    self.tr("已启动 SDK 安装命令", "Started SDK install command")
                );
                self.mobile_popup_open = false;
            }
            HitAction::OpenMobileDownloadUrl(url) => {
                if open_http_url(&url) {
                    self.status_notification = format!(
                        "🌐 {}: {url}",
                        self.tr("已打开官方下载页面", "Opened official download page")
                    );
                } else {
                    self.status_notification = format!(
                        "{}: {url}",
                        self.tr("无法打开下载链接", "Could not open download URL")
                    );
                }
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
                self.active_editor_diff = None;
                self.active_media_tab = None;
                self.active_tab = idx;
                self.scroll_line = 0;
                self.editor_scroll_x = 0;
                self.focus = FocusTarget::Editor;
                self.touch_caret_activity();
                self.ensure_active_blame_loaded();
            }
            HitAction::CloseTab(idx) => self.close_tab(idx),
            HitAction::DragDockPane(pane) => {
                self.pending_dock_drag = Some((pane, x, y));
                self.mouse_drag_target = None;
            }
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
            HitAction::ToggleCommitFileCheck(path) => {
                let checked = self.git_engine.toggle_commit_file_checked(&path);
                self.selected_git_working_file = Some(path.clone());
                let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
                self.status_notification = if checked {
                    format!(
                        "{}: {fname}",
                        self.tr("已勾选纳入提交", "Included in commit")
                    )
                } else {
                    format!("{}: {fname}", self.tr("已取消勾选", "Excluded from commit"))
                };
            }
            HitAction::ToggleCommitGroupCheck { untracked_group } => {
                let (checked, total) = self.git_engine.checked_counts(untracked_group);
                let next = checked < total;
                self.git_engine.set_group_checked(untracked_group, next);
            }
            HitAction::ToggleCommitGroupCollapse { untracked_group } => {
                if untracked_group {
                    self.git_unversioned_group_collapsed = !self.git_unversioned_group_collapsed;
                } else {
                    self.git_changes_group_collapsed = !self.git_changes_group_collapsed;
                }
            }
            HitAction::InspectSelectedGitDiff => {
                let target = self
                    .selected_git_working_file
                    .clone()
                    .filter(|p| self.git_engine.changed_files.iter().any(|c| &c.path == p))
                    .or_else(|| self.git_engine.checked_commit_files().into_iter().next())
                    .or_else(|| {
                        self.git_engine
                            .changed_files
                            .first()
                            .map(|c| c.path.clone())
                    });
                if let Some(path) = target {
                    self.open_git_working_diff_in_editor(&path);
                } else {
                    self.status_notification = self
                        .tr("当前工作区没有可对比的变更文件", "No changed files to diff")
                        .to_string();
                }
            }
            HitAction::RollbackCheckedGitChanges => {
                let targets = self.git_engine.checked_commit_files();
                if targets.is_empty() {
                    if let Some(sel) = self
                        .selected_git_working_file
                        .clone()
                        .filter(|p| self.git_engine.changed_files.iter().any(|c| &c.path == p))
                    {
                        self.git_engine.request_rollback_file(&sel);
                        self.status_notification = format!("正在回滚 {}...", sel.display());
                    } else {
                        self.status_notification = self
                            .tr(
                                "请先勾选或选中需要回滚的文件",
                                "Select or check files to rollback first",
                            )
                            .to_string();
                    }
                } else {
                    let cnt = targets.len();
                    self.git_engine.request_rollback_files(targets);
                    self.status_notification = format!(
                        "{} ({cnt})...",
                        self.tr("正在回滚已勾选的变更文件", "Rolling back checked files")
                    );
                }
            }
            HitAction::CommitAllGitChanges => {
                self.submit_git_commit(false);
            }
            HitAction::CommitAndPushGitChanges => {
                self.submit_git_commit(true);
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
                self.status_notification =
                    "正在后台分析 Git Diff 并生成 Commit 信息...".to_string();
            }
            HitAction::RollbackGitFile(path) => {
                self.git_engine.request_rollback_file(&path);
                self.status_notification = format!("正在回滚 {}...", path.display());
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
                self.open_git_working_diff_in_editor(&path);
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
                let target = self.selected_git_commit_hash.clone().or_else(|| {
                    self.git_engine
                        .recent_commits
                        .first()
                        .map(|c| c.hash.clone())
                });
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
                self.focus = FocusTarget::Editor;
                self.git_engine.request_merge_branch(&branch);
                self.status_notification = format!("Merging '{branch}'...");
            }
            HitAction::GitMergeAbort => {
                self.git_branch_popup_open = false;
                self.focus = FocusTarget::Editor;
                self.git_engine.request_merge_abort();
                self.status_notification = self
                    .tr("正在中止当前 Merge...", "Aborting active merge...")
                    .to_string();
            }
            HitAction::GitRebaseBranch(branch) => {
                self.git_branch_popup_open = false;
                self.focus = FocusTarget::Editor;
                self.git_engine.request_rebase_branch(&branch);
                self.status_notification = format!("Rebasing onto '{branch}'...");
            }
            HitAction::GitRebaseContinue => {
                self.git_branch_popup_open = false;
                self.focus = FocusTarget::Editor;
                self.git_engine.request_rebase_continue();
                self.status_notification = self
                    .tr("正在继续 Git Rebase...", "Continuing Git Rebase...")
                    .to_string();
            }
            HitAction::GitRebaseAbort => {
                self.git_branch_popup_open = false;
                self.focus = FocusTarget::Editor;
                self.git_engine.request_rebase_abort();
                self.status_notification = self
                    .tr("正在中止 Git Rebase...", "Aborting Git Rebase...")
                    .to_string();
            }
            HitAction::GitDeleteBranch(branch) => {
                self.git_branch_popup_open = false;
                self.git_engine.request_delete_branch(&branch);
                self.status_notification = format!("Deleting branch '{branch}'...");
            }
            HitAction::GitStashPop(sref) => {
                self.git_engine.request_stash_pop(&sref);
                self.focus = FocusTarget::Editor;
                self.status_notification = format!("Popping stash {sref}...");
            }
            HitAction::GitStashApply(sref) => {
                self.git_engine.request_stash_apply(&sref);
                self.focus = FocusTarget::Editor;
                self.status_notification = format!("Applying stash {sref}...");
            }
            HitAction::GitStashDrop(sref) => {
                self.git_engine.request_stash_drop(&sref);
                self.status_notification = format!("Dropping stash {sref}...");
            }
            HitAction::SelectGitResetMode(mode) => {
                self.git_reset_mode = mode;
            }
            HitAction::GitResetTo { target, mode } => {
                self.git_engine.request_reset(&target, mode);
                self.focus = FocusTarget::Editor;
                self.status_notification = format!("Reset ({}) -> {target}...", mode.as_flag());
            }
            HitAction::GitRemoveRemote(name) => {
                self.git_engine.request_remove_remote(&name);
                self.status_notification = format!("Removing remote '{name}'...");
            }
            HitAction::OpenAddRemoteModal => {
                self.modal_text_input
                    .set_text("origin https://github.com/org/repo.git");
                self.modal_text_input.select_all();
                self.focus = FocusTarget::GitAddRemoteModal;
                self.touch_caret_activity();
            }
            HitAction::GitConfirmRollbackAll => {
                self.git_engine.request_rollback_all();
                self.focus = FocusTarget::Editor;
                self.status_notification = self
                    .tr(
                        "正在回滚工作区全部未提交变更...",
                        "Rolling back all uncommitted changes...",
                    )
                    .to_string();
            }
            HitAction::CloseModalToEditor => {
                self.focus = FocusTarget::Editor;
            }
            HitAction::ClearGitFileHistoryFilter => {
                self.git_file_history_filter = None;
                self.git_log_scroll = 0;
            }
            HitAction::InspectBlameCommit(hash) => {
                self.git_file_history_filter = None;
                self.selected_git_commit_hash = Some(hash.clone());
                self.git_engine.request_load_commit_detail(&hash);
                self.bottom_tool = Some(ToolWindowId::GitLog);
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
                self.save_global_settings();
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
                self.save_global_settings();
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
                self.save_global_settings();
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
                self.save_global_settings();
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
                self.save_global_settings();
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
                self.save_global_settings();
            }
            HitAction::ToggleAttachActiveContext => {
                self.attach_active_context = !self.attach_active_context;
                self.status_notification = if self.attach_active_context {
                    "已开启自动附加当前文件与光标上下文".to_string()
                } else {
                    "已关闭自动附加文件上下文".to_string()
                };
                self.save_global_settings();
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
                    let prefix = if !current.is_empty()
                        && !current.ends_with(' ')
                        && !current.ends_with('\n')
                    {
                        " "
                    } else {
                        ""
                    };
                    self.agent_chat_input.move_end(false);
                    self.agent_chat_input
                        .insert_str(&format!("{prefix}@{fname} "));
                }
                self.attach_active_context = true;
                self.focus = FocusTarget::AgentChatInput;
                self.touch_caret_activity();
            }
            HitAction::ClearAgentChat => {
                self.agent_hub.clear_messages();
                self.collapsed_code_blocks.clear();
                self.expanded_message_thoughts.clear();
                self.expanded_message_file_changes.clear();
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
            HitAction::ToggleMessageFileChangesCollapse(msg_idx) => {
                let cur = self
                    .expanded_message_file_changes
                    .get(&msg_idx)
                    .copied()
                    .unwrap_or(false);
                self.expanded_message_file_changes.insert(msg_idx, !cur);
                self.agent_chat_signature_salt = self.agent_chat_signature_salt.wrapping_add(1);
            }
            HitAction::InspectAgentFileDiff { path, msg_idx } => {
                self.open_agent_file_diff_in_editor(&path, msg_idx);
            }
            HitAction::CloseEditorDiff => {
                self.active_editor_diff = None;
                self.focus = FocusTarget::Editor;
            }
            HitAction::OpenDiffSourceFile(path) => {
                self.open_file(path);
                self.focus = FocusTarget::Editor;
                self.touch_caret_activity();
            }
            HitAction::SelectMediaTab(idx) => {
                self.active_editor_diff = None;
                if idx < self.media_tabs.len() {
                    self.active_media_tab = Some(idx);
                }
                self.focus = FocusTarget::Editor;
                self.touch_caret_activity();
            }
            HitAction::CloseMediaTab(idx) => {
                self.close_media_tab(idx);
            }
            HitAction::OpenMediaAsText(path) => {
                self.open_file_as_text(path);
                self.focus = FocusTarget::Editor;
                self.touch_caret_activity();
            }
            HitAction::OpenExternalMedia(path) => {
                if open_media_with_system_app(&path) {
                    self.status_notification = format!(
                        "{}: {}",
                        self.tr("已在系统默认应用中打开", "Opened in system player"),
                        path.display()
                    );
                }
            }
            HitAction::AdjustMediaZoom(delta) => {
                if let Some(idx) = self.active_media_tab {
                    if let Some(tab) = self.media_tabs.get_mut(idx) {
                        tab.zoom_pct = if delta == 0 {
                            100
                        } else {
                            ((tab.zoom_pct as i32) + delta).clamp(25, 300) as u32
                        };
                    }
                }
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
            HitAction::OpenExternalUrl(url) => {
                if open_http_url(&url) {
                    self.status_notification = format!(
                        "{}: {url}",
                        self.tr("已在浏览器中打开", "Opened in browser")
                    );
                } else {
                    self.status_notification =
                        format!("{}: {url}", self.tr("无法打开链接", "Could not open link"));
                }
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
            HitAction::FocusCtrlKPrompt => {
                self.focus = FocusTarget::CtrlKPrompt;
                let start_x = self.ctrl_k_input_box_rect.x + 28;
                let idx =
                    self.ctrl_k_input
                        .hit_test_singleline(canvas, x, start_x, FontStyle::UiSmall);
                self.ctrl_k_input.set_cursor_at(idx, shift);
                self.mouse_drag_target = Some(FocusTarget::CtrlKPrompt);
                self.touch_caret_activity();
            }
            HitAction::AcceptInlineDiff => self.accept_inline_diff(),
            HitAction::RejectInlineDiff => self.reject_inline_diff(),
            HitAction::RevertLastCheckpoint => self.revert_last_checkpoint(),
            HitAction::ToggleAgentChangedFilesDrawer => {
                self.agent_changed_files_expanded = !self.agent_changed_files_expanded;
            }
            HitAction::KeepAllAgentChangedFiles => {
                self.agent_hub.keep_all_session_changed_files();
                self.agent_changed_files_expanded = false;
                self.status_notification = self
                    .tr("已保留本轮全部文件更改", "Kept all changed files")
                    .to_string();
            }
            HitAction::RevertAllAgentChangedFiles => {
                let restored = self.agent_hub.revert_all_session_changed_files();
                self.reload_paths_after_revert(&restored);
                self.agent_changed_files_expanded = false;
                self.status_notification = format!(
                    "{} ({})",
                    self.tr("已回退全部文件更改", "Reverted all changed files"),
                    restored.len()
                );
            }
            HitAction::RevertSingleAgentChangedFile(path) => {
                if self.agent_hub.revert_session_changed_file(&path) {
                    self.reload_paths_after_revert(&[path.clone()]);
                    self.status_notification = format!(
                        "{}: {}",
                        self.tr("已回退文件", "Reverted file"),
                        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
                    );
                }
            }
            HitAction::StartEditSentUserMessage(msg_idx) => {
                self.start_edit_sent_user_message(msg_idx);
            }
            HitAction::CancelEditSentUserMessage => {
                self.cancel_edit_sent_user_message();
            }
            HitAction::ConfirmEditMessageRewind { revert_files } => {
                self.confirm_edit_message_rewind(revert_files);
            }
            HitAction::CancelEditMessageRewindModal => {
                self.cancel_edit_message_rewind_modal();
            }
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
                self.terminal.active_pty_mut().ensure_spawned();
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
                    format!(
                        "已在 {}:{} 添加红点断点 (Ctrl+F8)",
                        path.display(),
                        line + 1
                    )
                } else {
                    format!("已移除 {}:{} 的断点", path.display(), line + 1)
                };
            }
            HitAction::RunGutterEntry(line) => {
                self.run_gutter_entry_in_terminal(line);
            }
            HitAction::SelectTheme(kind) => {
                self.theme = IdeaTheme::from_kind(kind);
                self.status_notification = format!(
                    "{}: {}",
                    self.tr("已应用主题", "Applied theme"),
                    kind.display_name()
                );
                self.save_global_settings();
            }
            HitAction::SetUiLanguage(lang) => {
                self.set_ui_language(lang);
            }
            HitAction::SetUiScale(pct) => {
                self.set_ui_scale((pct as f32 / 100.0).clamp(0.85, 2.0));
            }
            HitAction::SelectSettingsPage(page) => {
                self.settings_sdk_focus = None;
                if self.settings_page != page {
                    self.settings_page = page;
                    self.modal_scroll = 0;
                    self.save_global_settings();
                }
            }
            HitAction::FocusSettingsSearch => {
                self.settings_sdk_focus = None;
                let idx = self.settings_query_input.hit_test_singleline(
                    canvas,
                    x,
                    self.settings_search_text_x,
                    FontStyle::UiRegular,
                );
                self.settings_query_input.set_cursor_at(idx, shift);
                self.mouse_drag_target = Some(FocusTarget::SettingsModal);
                self.touch_caret_activity();
            }
            HitAction::ApplySettings => {
                let rechecked = self.commit_mobile_sdk_locations_from_inputs();
                self.save_all_persisted_state();
                self.status_notification = if rechecked {
                    self.tr(
                        "已保存移动端 SDK 目录并重新检测",
                        "Saved mobile SDK directories and rechecked",
                    )
                    .to_string()
                } else {
                    self.tr("设置已应用", "Settings applied").to_string()
                };
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
                    self.request_switch_workspace(p);
                }
                self.focus = FocusTarget::Editor;
            }
            HitAction::ConfirmOpenPendingInNewWindow => {
                if let Some(p) = self.pending_open_workspace.take() {
                    self.open_workspace_in_new_window(p);
                }
                self.focus = FocusTarget::Editor;
            }
            HitAction::OpenMobileSdkSettings => {
                self.settings_page = SettingsPage::MobileSdk;
                self.execute_ide_action(IdeAction::OpenSettingsModal);
            }
            HitAction::FocusMobileSdkPath(slot) => {
                let slot = slot.min(2);
                self.settings_page = SettingsPage::MobileSdk;
                if self.focus != FocusTarget::SettingsModal {
                    self.execute_ide_action(IdeAction::OpenSettingsModal);
                }
                self.settings_sdk_focus = Some(slot);
                let start_x = self.mobile_sdk_field_rects[slot as usize].x + 8;
                let field = self.mobile_sdk_input_mut(slot);
                let idx = field.hit_test_singleline(canvas, x, start_x, FontStyle::UiRegular);
                field.set_cursor_at(idx, shift);
                self.mouse_drag_target = Some(FocusTarget::SettingsModal);
                self.touch_caret_activity();
            }
            HitAction::BrowseMobileSdkDir(slot) => {
                self.commit_mobile_sdk_locations_from_inputs();
                self.request_open_folder_dialog_with_mode(FolderPickMode::MobileSdk(slot.min(2)));
            }
            HitAction::CloseModalOrPopup => {
                if self.focus == FocusTarget::SettingsModal {
                    self.commit_mobile_sdk_locations_from_inputs();
                }
                self.pending_open_workspace = None;
                self.active_image_preview = None;
                self.prev_focus_before_image_preview = None;
                self.focus = FocusTarget::Editor;
                self.close_top_menu();
                self.git_branch_popup_open = false;
                self.agent_provider_popup_open = false;
                self.agent_model_popup_open = false;
                self.agent_sessions_popup_open = false;
                self.run_config_popup_open = false;
        self.run_config_popup_scroll = 0;
                self.project_popup_open = false;
                self.more_menu_popup_open = false;
                self.context_menu = None;
            }
        }
    }

    pub fn open_context_menu_at(&mut self, x: i32, y: i32, canvas: &SurfaceCanvas) {
        self.close_top_menu();
        self.git_branch_popup_open = false;
        self.agent_provider_popup_open = false;
        self.agent_model_popup_open = false;
        self.agent_sessions_popup_open = false;
        self.run_config_popup_open = false;
        self.run_config_popup_scroll = 0;
        self.project_popup_open = false;
        self.more_menu_popup_open = false;

        let item =
            |label: &str, shortcut: &str, action: ContextMenuAction, sep: bool| ContextMenuItem {
                label: label.to_string(),
                shortcut: shortcut.to_string(),
                action,
                separator_before: sep,
            };

        // 1. 编辑器标签页右键菜单
        if let Some((_, tab_idx)) = self
            .tab_rects
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .copied()
        {
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
                if RunConfiguration::for_file(&p, &self.workspace_root).is_some() {
                    let fname = p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file");
                    let label = match self.ui_lang {
                        UiLanguage::ZhCn => format!("运行 '{fname}'"),
                        UiLanguage::EnUs => format!("Run '{fname}'"),
                    };
                    items.push(item(
                        &label,
                        "Ctrl+Shift+F10",
                        ContextMenuAction::RunFile(p.clone()),
                        true,
                    ));
                }
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
                    if RunConfiguration::for_file(&path, &self.workspace_root).is_some() {
                        let fname = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("file");
                        let label = match self.ui_lang {
                            UiLanguage::ZhCn => format!("运行 '{fname}'"),
                            UiLanguage::EnUs => format!("Run '{fname}'"),
                        };
                        items.push(item(
                            &label,
                            "Ctrl+Shift+F10",
                            ContextMenuAction::RunFile(path.clone()),
                            false,
                        ));
                    }
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

        // 2.5 左侧 Git 提交面板右键菜单 (IDEA 风格操作)
        if self.left_tool == Some(ToolWindowId::Commit) && self.left_pane_rect.contains(x, y) {
            if let Some((_, path, is_checked)) = self
                .git_change_row_rects
                .iter()
                .find(|(r, _, _)| r.contains(x, y))
                .cloned()
            {
                let items = vec![
                    item(
                        if is_checked {
                            self.tr("取消勾选 (Exclude from Commit)", "Exclude from Commit")
                        } else {
                            self.tr("勾选纳入提交 (Include in Commit)", "Include in Commit")
                        },
                        "Space",
                        ContextMenuAction::GitToggleCommitCheck(path.clone()),
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
                item(
                    self.tr("将上一条命令加入运行列表", "Add Last Command to Run List"),
                    "",
                    ContextMenuAction::TermAddLastCommandToRunList,
                    true,
                ),
                item(
                    self.tr("添加运行命令...", "Add Run Command..."),
                    "",
                    ContextMenuAction::OpenNewRunConfigModal,
                    false,
                ),
            ];
            self.context_menu = Some(ContextMenuState { x, y, items });
            return;
        }

        // 4. 各类输入框右键菜单
        let input_target_opt = if self
            .agent_hub
            .inline_edit
            .as_ref()
            .is_some_and(|s| s.active)
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
                | FocusTarget::NewRunConfigModal
                | FocusTarget::NewGitBranchModal
                | FocusTarget::NewGitTagModal
                | FocusTarget::NewGitStashModal
                | FocusTarget::GitAddRemoteModal
                | FocusTarget::RenamePathModal
        ) && self.prompt_modal_input_rect.contains(x, y)
        {
            Some(self.focus)
        } else if self.focus == FocusTarget::SettingsModal
            && (self.settings_search_rect.contains(x, y)
                || self
                    .mobile_sdk_field_rects
                    .iter()
                    .any(|r| r.w > 0 && r.contains(x, y)))
        {
            if let Some(slot) = (0..3u8).find(|slot| {
                self.mobile_sdk_field_rects
                    .get(*slot as usize)
                    .is_some_and(|r| r.w > 0 && r.contains(x, y))
            }) {
                self.settings_sdk_focus = Some(slot);
            } else {
                self.settings_sdk_focus = None;
            }
            Some(FocusTarget::SettingsModal)
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
            let mut items = vec![
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
            ];
            let active_path = self.active_buffer().path.clone();
            if RunConfiguration::for_file(&active_path, &self.workspace_root).is_some() {
                let fname = active_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file");
                let run_lbl = match self.ui_lang {
                    UiLanguage::ZhCn => format!("运行 '{fname}'"),
                    UiLanguage::EnUs => format!("Run '{fname}'"),
                };
                items.push(item(
                    &run_lbl,
                    "Ctrl+Shift+F10",
                    ContextMenuAction::RunFile(active_path),
                    true,
                ));
            }
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
                self.status_notification = format!("{}: {s}", self.tr("已复制路径", "Copied path"));
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
            ContextMenuAction::RunFile(path) => {
                self.run_file_in_terminal(&path);
            }
            ContextMenuAction::TabClose(idx) => self.close_tab(idx),
            ContextMenuAction::TabCloseOthers(keep_idx) => {
                if self.open_buffers.len() > 1 && keep_idx < self.open_buffers.len() {
                    let kept = self.open_buffers.remove(keep_idx);
                    self.open_buffers.clear();
                    self.open_buffers.push(kept);
                    self.active_tab = 0;
                    self.status_notification =
                        self.tr("已关闭其他标签页", "Closed other tabs").to_string();
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
            ContextMenuAction::TermAddLastCommandToRunList => {
                let last_cmd = self.terminal.active_pty().history.last().cloned();
                if let Some(cmd) = last_cmd {
                    let cwd = self.terminal.active_pty().cwd.clone();
                    if let Some(cfg) = RunConfiguration::from_command_line_explicit(&cmd, &cwd) {
                        let name = cfg.name.clone();
                        self.upsert_run_configuration_internal(cfg, true, true);
                        self.status_notification = format!(
                            "{}: {}",
                            self.tr("已将命令加入运行列表", "Added command to Run list"),
                            name
                        );
                    }
                } else {
                    self.open_new_run_config_modal(None);
                }
            }
            ContextMenuAction::OpenNewRunConfigModal => {
                let last_cmd = self.terminal.active_pty().history.last().cloned();
                self.open_new_run_config_modal(last_cmd.as_deref());
            }
            ContextMenuAction::GitToggleCommitCheck(path) => {
                let checked = self.git_engine.toggle_commit_file_checked(&path);
                self.selected_git_working_file = Some(path.clone());
                let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
                self.status_notification = if checked {
                    format!(
                        "{}: {fname}",
                        self.tr("已勾选纳入提交", "Included in commit")
                    )
                } else {
                    format!("{}: {fname}", self.tr("已取消勾选", "Excluded from commit"))
                };
            }
            ContextMenuAction::GitShowWorkingDiff(path) => {
                self.open_git_working_diff_in_editor(&path);
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
                self.modal_text_input
                    .set_text(&format!("branch-from-{rev}"));
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
                        .tr(
                            "请先拖选要复制的对话文本",
                            "Select text in chat stream first",
                        )
                        .to_string();
                }
            }
            ContextMenuAction::AgentCopyAllChat => {
                let all = self.copy_all_agent_chat();
                if !all.is_empty() {
                    write_windows_clipboard_text(&all);
                    self.status_notification = self
                        .tr(
                            "已复制全部 AI 对话内容到剪贴板",
                            "Copied entire AI conversation",
                        )
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

        self.clamp_dock_spans(w, h);
        let left_w = if self.pane_is_open(self.dock_left) {
            self.left_panel_width
        } else {
            0
        };
        let right_w = if self.pane_is_open(self.dock_right) {
            self.right_panel_width
        } else {
            0
        };
        let bottom_h = if self.pane_is_open(self.dock_bottom) {
            self.bottom_panel_height
        } else {
            0
        };
        let upper_h = (body_h - bottom_h).max(56);

        self.left_pane_rect = Rect::new(0, 0, 0, 0);
        self.right_pane_rect = Rect::new(0, 0, 0, 0);
        self.bottom_pane_rect = Rect::new(0, 0, 0, 0);
        if !self.pane_is_open(DockPane::Project) {
            self.left_scroll_info = ScrollTrackInfo::default();
        }
        if !self.pane_is_open(DockPane::Agent) {
            self.right_scroll_info = ScrollTrackInfo::default();
        }
        if !self.pane_is_open(DockPane::Tooling) {
            self.terminal_scroll_info = ScrollTrackInfo::default();
        }

        let left_island = if left_w > 8 {
            Rect::new(
                left_stripe_w + 2,
                body_y + 2,
                (left_w - 4).max(8),
                (upper_h - 4).max(8),
            )
        } else {
            Rect::new(0, 0, 0, 0)
        };
        let right_island = if right_w > 8 {
            Rect::new(
                w - right_stripe_w - right_w + 2,
                body_y + 2,
                (right_w - 4).max(8),
                (upper_h - 4).max(8),
            )
        } else {
            Rect::new(0, 0, 0, 0)
        };
        let bottom_island = if bottom_h > 8 {
            Rect::new(
                left_stripe_w + 2,
                body_y + upper_h + 2,
                (w - left_stripe_w - right_stripe_w - 4).max(8),
                (bottom_h - 4).max(8),
            )
        } else {
            Rect::new(0, 0, 0, 0)
        };
        self.place_dock_pane(canvas, self.dock_left, left_island);
        self.place_dock_pane(canvas, self.dock_right, right_island);
        self.place_dock_pane(canvas, self.dock_bottom, bottom_island);

        // 7. 中央代码编辑器区域
        let editor_x = left_stripe_w + left_w + 2;
        let editor_w = (w - left_stripe_w - right_stripe_w - left_w - right_w - 4).max(64);
        let editor_rect = Rect::new(editor_x, body_y + 2, editor_w, (upper_h - 4).max(48));
        self.editor_pane_rect = editor_rect;
        self.render_center_editor(canvas, editor_rect);

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
                        Rect::new(
                            left_stripe_w + 4,
                            sy,
                            w - left_stripe_w - right_stripe_w - 8,
                            2,
                        ),
                        t.accent_blue,
                    );
                }
            }
        }

        if self.active_dock_drag.is_some() {
            if let Some(edge) = self.dock_drop_edge {
                let mark = match edge {
                    DockEdge::Left => {
                        Rect::new(left_stripe_w + 2, body_y + 6, 4, (upper_h - 12).max(8))
                    }
                    DockEdge::Right => Rect::new(
                        w - right_stripe_w - 6,
                        body_y + 6,
                        4,
                        (upper_h - 12).max(8),
                    ),
                    DockEdge::Bottom => Rect::new(
                        left_stripe_w + 8,
                        (body_y + body_h - 8).max(body_y),
                        (w - left_stripe_w - right_stripe_w - 16).max(8),
                        4,
                    ),
                };
                canvas.fill_rect(mark, t.accent_blue);
            }
            if let Some(pane) = self.active_dock_drag {
                let (mx, my) = self.dock_drag_pos;
                let card = Rect::new((mx + 14).min(w - 180), (my + 16).min(h - 48), 164, 32);
                canvas.fill_rounded_rect_alpha(card, 8, t.popup_bg, 232);
                canvas.stroke_rounded_rect(card, 8, t.accent_blue, t.popup_bg);
                canvas.draw_text_centered(
                    card,
                    pane.title(self.ui_lang == UiLanguage::ZhCn),
                    t.text_primary,
                    FontStyle::UiBold,
                );
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
                self.tr(
                    "正在加载工作区索引与 Git 状态...",
                    "Loading workspace index & Git...",
                ),
                txt_color,
                pill_w - 36,
                FontStyle::UiSmall,
            );
        }

        // 9. 顶层弹出菜单与模态对话框
        self.overlay_hit_start = self.hit_regions.len();
        self.menu_dropdown_rect = Rect::new(0, 0, 0, 0);
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
        if self.mobile_popup_open {
            self.render_mobile_env_popup(canvas, w);
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
            | FocusTarget::NewRunConfigModal
            | FocusTarget::NewGitBranchModal
            | FocusTarget::NewGitTagModal
            | FocusTarget::NewGitStashModal
            | FocusTarget::GitAddRemoteModal
            | FocusTarget::RenamePathModal => {
                self.render_input_prompt_modal(canvas, w, h);
            }
            FocusTarget::GitMergeModal
            | FocusTarget::GitRebaseModal
            | FocusTarget::GitUnstashModal
            | FocusTarget::GitResetModal
            | FocusTarget::GitRollbackModal
            | FocusTarget::GitRemotesModal => {
                self.render_git_operation_modal(canvas, w, h);
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
            FocusTarget::EditMessageRewindModal => {
                self.render_edit_message_rewind_modal(canvas, w, h);
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
        self.render_window_attention_flash(canvas, w, h);
    }

    fn render_window_attention_flash(&self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let Some(until) = self.window_attention_until else {
            return;
        };
        let now = Instant::now();
        if now >= until {
            return;
        }
        let total = 0.98_f32;
        let left = until.duration_since(now).as_secs_f32();
        let elapsed = (total - left).clamp(0.0, total);
        let on = ((elapsed / total) * 6.0) as i32 % 2 == 0;
        if !on {
            return;
        }
        let color = self.theme.accent_blue;
        let thickness = 4;
        canvas.fill_rect_alpha(Rect::new(0, 0, w, thickness), color, 220);
        canvas.fill_rect_alpha(Rect::new(0, h - thickness, w, thickness), color, 220);
        canvas.fill_rect_alpha(Rect::new(0, 0, thickness, h), color, 220);
        canvas.fill_rect_alpha(Rect::new(w - thickness, 0, thickness, h), color, 220);
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

        // 2. 顶部主工具栏左侧内容：
        // 展开主菜单时隐藏项目名称和 Git 分支，直接在顶部工具栏原位展示主菜单栏
        self.menu_category_rects.clear();
        let right_controls_total = 148 + 148; // 5 个动作图标 + 窗口控制区
        let left_content_end_x = if self.hamburger_menu_expanded {
            let lang = self.ui_lang;
            let menu_right_limit = (rect.w - right_controls_total - 8).max(68);
            let mut mx = 68;
            for &cat in MainMenuCategory::all() {
                let label = cat.label_i18n(lang);
                let (tw, _) = canvas.measure_text(label, FontStyle::UiRegular);
                let item_w = tw + 18;
                if mx + item_w > menu_right_limit && mx > 68 {
                    break;
                }
                let item_rect = Rect::new(mx, 6, item_w, 26);
                let active = self.active_menu_category == Some(cat);
                let hovered = item_rect.contains(hx, hy);
                if active || hovered {
                    canvas.fill_rounded_rect(
                        item_rect,
                        5,
                        if active { t.popup_hover_bg } else { t.panel_bg },
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
                mx += item_w + 2;
            }
            self.menu_trigger_and_bar_rect = Rect::new(8, 0, (mx - 8 + 8).max(60), 42);
            mx
        } else {
            self.menu_trigger_and_bar_rect = Rect::new(0, 0, 0, 0);
            // [WS] 动态工作区徽标 + 项目名 ▾
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
            cur_x + git_w
        };

        // 4. 最右侧：自定义无边框窗口控制按钮 [—] [□/❐] [×] (预留 138px)
        let cap_w = 46;
        let cap_h = rect.h - 1;
        let min_rect = Rect::new(rect.w - cap_w * 3, 0, cap_w, cap_h);
        let max_rect = Rect::new(rect.w - cap_w * 2, 0, cap_w, cap_h);
        let close_rect = Rect::new(rect.w - cap_w, 0, cap_w, cap_h);

        let min_hot = min_rect.contains(hx, hy);
        let min_pressed = self.armed_window_control == Some(WindowControlCmd::Minimize) && min_hot;
        if min_pressed {
            canvas.fill_rect(min_rect, blend_rgb(t.panel_bg, 0x000000, 0.28));
        } else if min_hot {
            canvas.fill_rect(min_rect, t.panel_bg);
        }
        canvas.draw_window_minimize_icon(
            min_rect.x + cap_w / 2,
            min_rect.y + cap_h / 2,
            t.text_secondary,
        );
        self.hit_regions.push((
            min_rect,
            HitAction::WindowControl(WindowControlCmd::Minimize),
        ));

        let max_hot = max_rect.contains(hx, hy);
        let max_pressed =
            self.armed_window_control == Some(WindowControlCmd::ToggleMaximize) && max_hot;
        if max_pressed {
            canvas.fill_rect(max_rect, blend_rgb(t.panel_bg, 0x000000, 0.28));
        } else if max_hot {
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
        let close_pressed = self.armed_window_control == Some(WindowControlCmd::Close) && close_hovered;
        if close_hovered {
            canvas.fill_rect(close_rect, if close_pressed { 0xA32016 } else { 0xC42B1C });
        }
        canvas.draw_window_close_icon(
            close_rect.x + cap_w / 2,
            close_rect.y + cap_h / 2,
            if close_hovered {
                0xFFFFFF
            } else {
                t.text_secondary
            },
        );
        self.hit_regions.push((
            close_rect,
            HitAction::WindowControl(WindowControlCmd::Close),
        ));

        // 5. IDEA 运行工具区：[📱 移动设备/SDK ▾] [运行配置 ▾] [▷] [🐞] [⋮] [🔍] [⚙]（紧邻窗口控制按钮左侧）
        let cfg_fallback = self.tr("当前文件", "Current File");
        let cfg_name = self
            .run_configs
            .get(self.active_run_config)
            .map(|c| c.name.as_str())
            .unwrap_or(cfg_fallback);
        let (ctw, _) = canvas.measure_text(cfg_name, FontStyle::UiRegular);
        let cfg_w = (ctw + 52).clamp(118, 232);
        let right_actions_start_x = rect.w - right_controls_total;
        let cfg_x = (right_actions_start_x - cfg_w - 8).max(left_content_end_x + 12);

        if self.mobile_env.project_kind.is_mobile() || self.mobile_popup_open {
            let missing_cnt = if !self.mobile_env.sdk_tools.is_empty() {
                self.mobile_env.missing_required_tools().len()
            } else {
                0
            };
            let online_dev = self.selected_or_first_online_device().cloned();
            let (mob_label, status_dot_color): (String, u32) = if self.mobile_env.probing {
                (
                    self.tr("检测设备中...", "Probing SDK...").to_string(),
                    t.accent_blue,
                )
            } else if missing_cnt > 0 {
                (
                    match self.ui_lang {
                        UiLanguage::ZhCn => format!("缺少 {missing_cnt} 项SDK"),
                        UiLanguage::EnUs => format!("{missing_cnt} SDK missing"),
                    },
                    t.breakpoint_red,
                )
            } else if let Some(dev) = &online_dev {
                (dev.name.clone(), t.run_green)
            } else {
                (
                    self.tr("未连接设备", "No Device").to_string(),
                    t.text_muted,
                )
            };
            let (mtw, _) = canvas.measure_text(&mob_label, FontStyle::UiSmall);
            let mob_w = (mtw + 48).clamp(112, 176);
            let mob_x = cfg_x - mob_w - 6;
            if mob_x >= left_content_end_x + 8 {
                let mob_box = Rect::new(mob_x, 5, mob_w, 28);
                canvas.fill_rounded_rect(
                    mob_box,
                    6,
                    if self.mobile_popup_open || mob_box.contains(hx, hy) {
                        t.popup_hover_bg
                    } else {
                        t.panel_bg
                    },
                );
                canvas.draw_mobile_phone_icon(mob_x + 7, 12, status_dot_color);
                canvas.fill_circle(mob_x + 19, 23, 3, status_dot_color);
                canvas.draw_text_styled(
                    mob_x + 25,
                    11,
                    &mob_label,
                    if missing_cnt > 0 {
                        0xF78166
                    } else {
                        t.text_primary
                    },
                    mob_w - 40,
                    FontStyle::UiSmall,
                );
                canvas.draw_chevron_down(mob_x + mob_w - 14, 17, t.text_muted);
                self.hit_regions
                    .push((mob_box, HitAction::ToggleMobilePopup));
            }
        }

        let active_running = self.active_run_config_is_running();
        if cfg_x + cfg_w + 8 <= right_actions_start_x {
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
            if let Some(cfg) = self.run_configs.get(self.active_run_config) {
                canvas.draw_run_config_icon(cfg_x + 8, 12, cfg);
            } else {
                let icon_path = self.active_buffer().path.clone();
                canvas.draw_path_file_icon(cfg_x + 8, 12, &icon_path);
            }
            if active_running {
                canvas.fill_circle(cfg_x + 21, 24, 3, t.run_green);
            }
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
        }

        let mut ix = right_actions_start_x;

        // 绑定当前选中配置：空闲显示绿色运行三角 ▷，当前配置运行中变为暂停/停止 ❚❚ (Shift+F10)
        let run_btn = Rect::new(ix, 6, 26, 26);
        if run_btn.contains(hx, hy) {
            canvas.fill_rounded_rect(run_btn, 5, t.panel_bg);
        }
        if active_running {
            canvas.draw_pause_icon(ix + 7, 13, 12, t.breakpoint_red);
        } else {
            canvas.stroke_play_triangle(ix + 7, 13, 12, t.run_green);
        }
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
        canvas.fill_rect(Rect::new(min_rect.x - 4, 9, 1, 20), t.border_color);
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
                ToolWindowId::Commit => {
                    canvas.draw_commit_icon(ix, iy, icon_color);
                    let chg_cnt = self.git_engine.changed_files.len();
                    if chg_cnt > 0 {
                        let badge_cutout = if active || hovered {
                            t.panel_bg
                        } else {
                            t.tool_stripe_bg
                        };
                        canvas.draw_count_badge(
                            btn,
                            chg_cnt,
                            t.accent_blue,
                            0xFFFFFF,
                            badge_cutout,
                        );
                    }
                }
                ToolWindowId::Structure => canvas.draw_structure_icon(ix, iy, icon_color),
                _ => {}
            }
            self.hit_regions.push((btn, HitAction::ToggleLeftTool(id)));
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
            if theme_hov {
                t.text_primary
            } else {
                t.text_muted
            },
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
            if set_hov {
                t.text_primary
            } else {
                t.text_muted
            },
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
        self.arm_pane_header_drag(canvas, rect, DockPane::Project);

        let title = match tool {
            ToolWindowId::Project => self.tr("项目", "Project"),
            ToolWindowId::Commit => self.tr("Git 提交", "Commit"),
            ToolWindowId::Structure => self.tr("结构", "Structure"),
            _ => self.tr("工具", "Tool"),
        };
        let (title_w, _) = canvas.measure_text(title, FontStyle::UiBold);
        canvas.draw_text_styled(
            rect.x + 22,
            rect.y + 8,
            title,
            t.text_primary,
            106,
            FontStyle::UiBold,
        );
        canvas.draw_chevron_down(rect.x + 22 + title_w + 6, rect.y + 14, t.text_muted);

        if tool == ToolWindowId::Project && self.tree_load_alpha > 0.02 {
            canvas.draw_subtle_spinner(
                rect.x + 22 + title_w + 25,
                rect.y + 16,
                4.8,
                self.spinner_phase(),
                t.accent_blue,
                t.border_color,
                self.tree_load_alpha,
            );
        } else if tool == ToolWindowId::Commit && self.git_load_alpha > 0.02 {
            canvas.draw_subtle_spinner(
                rect.x + 22 + title_w + 25,
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
                let title_end = rect.x + 22 + title_w + 18;
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
                    let is_active = (!self.open_buffers.is_empty()
                        && !entry.is_dir
                        && self.active_buffer().path == entry.path)
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
                        canvas.draw_path_file_icon(base_x + 10, y + 4, &entry.path);
                        base_x + 28
                    };

                    let color = if is_active || row_hovered {
                        0xFFFFFF
                    } else if entry.is_dir {
                        t.text_primary
                    } else {
                        t.text_secondary
                    };
                    let entry_style = if is_active {
                        FontStyle::UiBold
                    } else {
                        FontStyle::UiRegular
                    };
                    let max_name_w = (rect.x + rect.w - text_x - 14).max(20);
                    if entry.is_dir {
                        canvas.draw_text_ellipsis(
                            text_x,
                            y + 2,
                            &entry.name,
                            color,
                            max_name_w,
                            entry_style,
                        );
                    } else {
                        let shown_name =
                            canvas.ellipsize_filename(&entry.name, max_name_w, entry_style);
                        canvas.draw_text_styled(
                            text_x,
                            y + 2,
                            &shown_name,
                            color,
                            max_name_w,
                            entry_style,
                        );
                    }
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
                // 顶部右侧 IDEA Commit 工具栏：[历史库 Log] [↻ 刷新] [⇄ Diff] [↶ 回滚勾选项]
                let btn_y = rect.y + 5;
                let title_end = rect.x + 22 + title_w + 18;
                let rb_all_btn = Rect::new(rect.x + rect.w - 32, btn_y, 24, 22);
                if rb_all_btn.contains(hx, hy) {
                    canvas.fill_rounded_rect(rb_all_btn, 4, t.popup_hover_bg);
                } else {
                    canvas.fill_rounded_rect(rb_all_btn, 4, t.window_bg);
                }
                canvas.draw_rollback_icon(rb_all_btn.x + 5, btn_y + 4, t.breakpoint_red);
                self.hit_regions
                    .push((rb_all_btn, HitAction::RollbackCheckedGitChanges));

                let diff_hdr_btn = Rect::new(rb_all_btn.x - 28, btn_y, 24, 22);
                if diff_hdr_btn.contains(hx, hy) {
                    canvas.fill_rounded_rect(diff_hdr_btn, 4, t.popup_hover_bg);
                } else {
                    canvas.fill_rounded_rect(diff_hdr_btn, 4, t.window_bg);
                }
                canvas.draw_changes_diff_icon(diff_hdr_btn.x + 4, btn_y + 3, t.accent_blue);
                self.hit_regions
                    .push((diff_hdr_btn, HitAction::InspectSelectedGitDiff));

                let ref_btn = Rect::new(diff_hdr_btn.x - 28, btn_y, 24, 22);
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
                    let log_hov =
                        log_btn.contains(hx, hy) || self.bottom_tool == Some(ToolWindowId::GitLog);
                    canvas.stroke_rounded_rect(
                        log_btn,
                        4,
                        if log_hov {
                            t.accent_blue
                        } else {
                            t.border_color
                        },
                        t.window_bg,
                    );
                    canvas.draw_text_centered(
                        log_btn,
                        self.tr("历史库", "Log"),
                        if log_hov {
                            t.accent_blue
                        } else {
                            t.text_secondary
                        },
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

                // 1. 当前分支胶囊与 Ahead/Behind 指示器 + 快捷 Fetch / Pull / Push 按钮
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

                let gap = 5;
                let bw = ((rect.w - 16 - gap * 2) / 3).max(52);
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
                self.hit_regions
                    .push((pull_btn, HitAction::ExecuteIdeAction(IdeAction::GitPull)));
                gx += bw + gap;

                let push_btn = Rect::new(gx, y, (rect.x + rect.w - 8 - gx).max(bw), 23);
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
                self.hit_regions
                    .push((push_btn, HitAction::ExecuteIdeAction(IdeAction::GitPush)));
                y += 28;

                // 2. IDEA 风格 Changelist 树：Changes (已跟踪变更) + Unversioned Files (未跟踪文件)
                //    勾选/取消勾选仅维护待提交清单，不触发 git add / git reset 暂存！
                let changed_snapshot = self.git_engine.changed_files.clone();
                let tracked_files: Vec<ide_git::GitChangedFile> = changed_snapshot
                    .iter()
                    .filter(|f| f.status != ide_git::GitFileStatusKind::Untracked)
                    .cloned()
                    .collect();
                let untracked_files: Vec<ide_git::GitChangedFile> = changed_snapshot
                    .iter()
                    .filter(|f| f.status == ide_git::GitFileStatusKind::Untracked)
                    .cloned()
                    .collect();

                #[derive(Clone)]
                enum CommitTreeRow {
                    SectionHeader {
                        title: String,
                        untracked_group: bool,
                        collapsed: bool,
                        checked_cnt: usize,
                        total_cnt: usize,
                    },
                    FileEntry(ide_git::GitChangedFile),
                }

                let mut tree_rows: Vec<CommitTreeRow> =
                    Vec::with_capacity(changed_snapshot.len() + 2);
                if !tracked_files.is_empty() || untracked_files.is_empty() {
                    let (c_chk, c_tot) = self.git_engine.checked_counts(false);
                    tree_rows.push(CommitTreeRow::SectionHeader {
                        title: format!(
                            "{} ({}/{})",
                            self.tr("更改 Changes", "Changes"),
                            c_chk,
                            c_tot
                        ),
                        untracked_group: false,
                        collapsed: self.git_changes_group_collapsed,
                        checked_cnt: c_chk,
                        total_cnt: c_tot,
                    });
                    if !self.git_changes_group_collapsed {
                        for ch in tracked_files {
                            tree_rows.push(CommitTreeRow::FileEntry(ch));
                        }
                    }
                }
                if !untracked_files.is_empty() {
                    let (u_chk, u_tot) = self.git_engine.checked_counts(true);
                    tree_rows.push(CommitTreeRow::SectionHeader {
                        title: format!(
                            "{} ({}/{})",
                            self.tr("未跟踪 Unversioned", "Unversioned Files"),
                            u_chk,
                            u_tot
                        ),
                        untracked_group: true,
                        collapsed: self.git_unversioned_group_collapsed,
                        checked_cnt: u_chk,
                        total_cnt: u_tot,
                    });
                    if !self.git_unversioned_group_collapsed {
                        for ch in untracked_files {
                            tree_rows.push(CommitTreeRow::FileEntry(ch));
                        }
                    }
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
                                untracked_group,
                                collapsed,
                                checked_cnt,
                                total_cnt,
                            } => {
                                let hdr_rect = Rect::new(rect.x + 6, y, rect.w - 18, row_h - 1);
                                canvas.fill_rounded_rect(hdr_rect, 4, t.panel_header_bg);

                                // 折叠三角 `▼` / `▶`
                                let chev_rect = Rect::new(rect.x + 8, y, 16, row_h - 1);
                                canvas.draw_text_styled(
                                    rect.x + 9,
                                    y + 2,
                                    if collapsed { "▶" } else { "▼" },
                                    t.text_muted,
                                    14,
                                    FontStyle::UiSmall,
                                );
                                self.hit_regions.push((
                                    chev_rect,
                                    HitAction::ToggleCommitGroupCollapse { untracked_group },
                                ));

                                // 分组主复选框：[✓] 全选 / [—] 部分选中 / [ ] 未选
                                let grp_chk = Rect::new(rect.x + 25, y + 3, 15, 15);
                                if total_cnt > 0 && checked_cnt == total_cnt {
                                    canvas.fill_rounded_rect(grp_chk, 3, t.accent_blue);
                                    canvas.draw_check_icon(grp_chk.x + 2, grp_chk.y + 3, 0xFFFFFF);
                                } else if checked_cnt > 0 {
                                    canvas.fill_rounded_rect(grp_chk, 3, t.accent_blue);
                                    canvas.draw_dash_icon(grp_chk.x + 2, grp_chk.y + 2, 0xFFFFFF);
                                } else {
                                    canvas.stroke_rounded_rect(
                                        grp_chk,
                                        3,
                                        t.border_color,
                                        t.window_bg,
                                    );
                                }
                                let grp_chk_hit = Rect::new(rect.x + 23, y, 22, row_h - 1);
                                self.hit_regions.push((
                                    grp_chk_hit,
                                    HitAction::ToggleCommitGroupCheck { untracked_group },
                                ));

                                canvas.draw_text_styled(
                                    rect.x + 45,
                                    y + 2,
                                    &title,
                                    t.text_secondary,
                                    rect.w - 64,
                                    FontStyle::UiBold,
                                );
                                let hdr_title_hit =
                                    Rect::new(rect.x + 45, y, (rect.w - 60).max(40), row_h - 1);
                                self.hit_regions.push((
                                    hdr_title_hit,
                                    HitAction::ToggleCommitGroupCheck { untracked_group },
                                ));
                            }
                            CommitTreeRow::FileEntry(ch) => {
                                let full_row = Rect::new(rect.x + 6, y, rect.w - 18, row_h - 1);
                                let is_checked =
                                    self.git_engine.is_file_checked_for_commit(&ch.path);
                                let is_selected = self
                                    .selected_git_working_file
                                    .as_ref()
                                    .is_some_and(|p| p == &ch.path)
                                    || self
                                        .git_working_diff_target
                                        .as_ref()
                                        .is_some_and(|p| p == &ch.path);
                                let row_hov = full_row.contains(hx, hy);
                                if is_selected {
                                    canvas.fill_rounded_rect(full_row, 4, t.editor_selection_bg);
                                } else if row_hov {
                                    canvas.fill_rounded_rect(full_row, 4, t.panel_header_bg);
                                }
                                self.git_change_row_rects.push((
                                    full_row,
                                    ch.path.clone(),
                                    is_checked,
                                ));

                                // 1) IDEA 复选框 [✓] / [ ]
                                let chk_rect = Rect::new(rect.x + 14, y + 3, 15, 15);
                                if is_checked {
                                    canvas.fill_rounded_rect(chk_rect, 3, t.accent_blue);
                                    canvas.draw_check_icon(
                                        chk_rect.x + 2,
                                        chk_rect.y + 3,
                                        0xFFFFFF,
                                    );
                                } else {
                                    canvas.stroke_rounded_rect(
                                        chk_rect,
                                        3,
                                        t.border_color,
                                        t.window_bg,
                                    );
                                }

                                // 2) 文件类型图标 + 文件名 + 相对目录
                                canvas.draw_path_file_icon(rect.x + 34, y + 4, &ch.path);

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
                                let right_actions_w = 88;
                                let text_avail_w = (rect.w - 52 - right_actions_w - 10).max(40);
                                let fname_shown = canvas.ellipsize_filename(
                                    fname,
                                    text_avail_w,
                                    FontStyle::UiSmall,
                                );
                                let next_x = canvas.draw_text_styled(
                                    rect.x + 52,
                                    y + 2,
                                    &fname_shown,
                                    status_color,
                                    text_avail_w,
                                    FontStyle::UiSmall,
                                );
                                if !parent_dir.is_empty()
                                    && next_x + 24 < rect.x + rect.w - right_actions_w
                                {
                                    canvas.draw_text_ellipsis(
                                        next_x + 6,
                                        y + 2,
                                        &parent_dir,
                                        t.text_muted,
                                        (rect.x + rect.w - right_actions_w - next_x - 8).max(16),
                                        FontStyle::UiSmall,
                                    );
                                }

                                // 点击左侧复选框区域仅勾选/取消勾选（不直接暂存或提交）；
                                // 点击文件名区域直接在中央文件窗口打开该文件的 Diff 对比视图
                                let chk_hit_rect = Rect::new(rect.x + 8, y, 25, row_h - 1);
                                self.hit_regions.push((
                                    chk_hit_rect,
                                    HitAction::ToggleCommitFileCheck(ch.path.clone()),
                                ));
                                let name_diff_rect = Rect::new(
                                    rect.x + 34,
                                    y,
                                    (rect.w - 42 - right_actions_w).max(24),
                                    row_h - 1,
                                );
                                self.hit_regions.push((
                                    name_diff_rect,
                                    HitAction::InspectGitWorkingFileDiff(ch.path.clone()),
                                ));

                                // 3) 右侧操作区：状态徽章 (M / A / D / U) + Diff + 打开文件 + 回滚
                                let badge_x = rect.x + rect.w - right_actions_w - 4;
                                canvas.draw_text_styled(
                                    badge_x,
                                    y + 2,
                                    ch.status.badge_str(),
                                    status_color,
                                    14,
                                    FontStyle::CodeMonoBold,
                                );

                                let diff_btn = Rect::new(badge_x + 15, y + 2, 22, row_h - 5);
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
                                    diff_btn.x + 3,
                                    diff_btn.y + 2,
                                    t.accent_blue,
                                );
                                self.hit_regions.push((
                                    diff_btn,
                                    HitAction::InspectGitWorkingFileDiff(ch.path.clone()),
                                ));

                                let open_btn = Rect::new(badge_x + 39, y + 2, 22, row_h - 5);
                                canvas.stroke_rounded_rect(
                                    open_btn,
                                    3,
                                    if open_btn.contains(hx, hy) {
                                        t.accent_blue
                                    } else {
                                        t.border_color
                                    },
                                    t.window_bg,
                                );
                                canvas.draw_jump_to_source_icon(
                                    open_btn.x + 5,
                                    open_btn.y + 3,
                                    t.text_secondary,
                                );
                                self.hit_regions
                                    .push((open_btn, HitAction::OpenFile(ch.path.clone())));

                                let rb_btn = Rect::new(badge_x + 63, y + 2, 22, row_h - 5);
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
                                self.hit_regions
                                    .push((rb_btn, HitAction::RollbackGitFile(ch.path.clone())));
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

                // 3. 底部 IDEA Commit 选项栏：[✓ Amend]  +  已选 N/M  +  [✦ AI 生成]
                let opt_y = commit_box_y;
                let amend_chk = Rect::new(rect.x + 10, opt_y + 4, 15, 15);
                if self.git_amend_commit {
                    canvas.fill_rounded_rect(amend_chk, 3, t.accent_blue);
                    canvas.draw_check_icon(amend_chk.x + 2, amend_chk.y + 3, 0xFFFFFF);
                } else {
                    canvas.stroke_rounded_rect(amend_chk, 3, t.border_color, t.window_bg);
                }
                let amend_hit = Rect::new(rect.x + 8, opt_y + 1, 78, 22);
                canvas.draw_text_styled(
                    rect.x + 29,
                    opt_y + 3,
                    self.tr("修正", "Amend"),
                    if self.git_amend_commit {
                        t.accent_blue
                    } else {
                        t.text_secondary
                    },
                    56,
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((amend_hit, HitAction::ToggleGitAmend));

                let checked_total = self.git_engine.total_checked_count();
                let changed_total = self.git_engine.changed_files.len();
                let sel_label = format!("{checked_total}/{changed_total}");
                canvas.draw_text_styled(
                    rect.x + 88,
                    opt_y + 3,
                    &sel_label,
                    if checked_total > 0 {
                        t.accent_blue
                    } else {
                        t.text_muted
                    },
                    48,
                    FontStyle::UiSmall,
                );

                let ai_msg_w = 82;
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

                // 4. Commit Message 输入框 + 双按钮：[提交 Commit (N)]  [提交并推送…]
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
                let can_commit = checked_total > 0 || self.git_amend_commit;
                canvas.fill_rounded_rect(
                    commit_btn,
                    6,
                    if can_commit {
                        t.accent_blue
                    } else {
                        t.panel_header_bg
                    },
                );
                let commit_btn_label = if self.git_amend_commit {
                    if checked_total > 0 {
                        format!("{} ({checked_total})", self.tr("修正提交", "Amend"))
                    } else {
                        self.tr("修正提交", "Amend").to_string()
                    }
                } else if checked_total > 0 {
                    format!("{} ({checked_total})", self.tr("提交", "Commit"))
                } else {
                    self.tr("提交 Commit", "Commit").to_string()
                };
                canvas.draw_text_centered(
                    commit_btn,
                    &commit_btn_label,
                    if can_commit { 0xFFFFFF } else { t.text_muted },
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
                    if can_commit {
                        t.accent_blue
                    } else {
                        t.border_color
                    },
                    if cp_hov && can_commit {
                        t.popup_hover_bg
                    } else {
                        t.window_bg
                    },
                );
                canvas.draw_text_centered(
                    cp_btn,
                    self.tr("提交并推送…", "Commit & Push…"),
                    if !can_commit {
                        t.text_muted
                    } else if cp_hov {
                        0xFFFFFF
                    } else {
                        t.accent_blue
                    },
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

    fn render_editor_welcome(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        self.editor_code_rect = Rect::new(0, 0, 0, 0);
        self.editor_scroll_info = ScrollTrackInfo::default();
        self.editor_horiz_scroll_info = ScrollTrackInfo::default();

        let title = self.tr("没有打开的文件", "No files open");
        let hint = self.tr(
            "在项目视图中打开，或按两次 Shift 搜索",
            "Open a file from the Project view, or press Shift twice to search",
        );
        let text_w = (rect.w - 32).max(24);
        let x = rect.x + (rect.w - text_w) / 2;
        let block_h = if rect.h >= 72 { 40 } else { 20 };
        let y = rect.y + ((rect.h - block_h) / 2).max(4);
        canvas.draw_text_centered(
            Rect::new(x, y, text_w, 20),
            title,
            t.text_secondary,
            FontStyle::UiRegular,
        );
        if rect.h >= 72 {
            canvas.draw_text_centered(
                Rect::new(x, y + 22, text_w, 16),
                hint,
                t.text_muted,
                FontStyle::UiSmall,
            );
        }
    }

    fn render_center_editor(&mut self, canvas: &mut SurfaceCanvas, rect: Rect) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        canvas.stroke_rounded_rect(rect, 8, t.border_color, t.editor_bg);

        if self.open_buffers.is_empty()
            && self.media_tabs.is_empty()
            && self.active_editor_diff.is_none()
        {
            self.render_editor_welcome(canvas, rect);
            return;
        }

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

        let has_diff_tab = self.active_editor_diff.is_some();
        let active_media_idx = if has_diff_tab {
            None
        } else {
            self.active_media_tab
                .filter(|&i| i < self.media_tabs.len())
        };
        let tabs_right_limit = if has_diff_tab {
            rect.x + rect.w - 196
        } else {
            rect.x + rect.w - 12
        };

        struct EditorBarItem {
            path: PathBuf,
            label: String,
            is_active: bool,
            font_style: FontStyle,
            tab_w: i32,
            select_action: HitAction,
            close_action: Option<HitAction>,
            buffer_idx: Option<usize>,
        }

        let mut bar_items: Vec<EditorBarItem> = Vec::with_capacity(open_snapshot.len() + self.media_tabs.len());
        for (idx, (path, dirty, _lang)) in open_snapshot.iter().enumerate() {
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

            let is_active = !has_diff_tab && active_media_idx.is_none() && idx == self.active_tab;
            let font_style = if is_active {
                FontStyle::UiBold
            } else {
                FontStyle::UiRegular
            };
            let (tw, _) = canvas.measure_text(&label, font_style);
            let tab_w = (tw + 58).clamp(108, 220);
            bar_items.push(EditorBarItem {
                path: path.clone(),
                label,
                is_active,
                font_style,
                tab_w,
                select_action: HitAction::SelectTab(idx),
                close_action: Some(HitAction::CloseTab(idx)),
                buffer_idx: Some(idx),
            });
        }

        for (m_idx, mtab) in self.media_tabs.iter().enumerate() {
            let fname = mtab
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("media")
                .to_string();
            let is_active = !has_diff_tab && active_media_idx == Some(m_idx);
            let font_style = if is_active {
                FontStyle::UiBold
            } else {
                FontStyle::UiRegular
            };
            let (tw, _) = canvas.measure_text(&fname, font_style);
            let tab_w = (tw + 58).clamp(108, 220);
            bar_items.push(EditorBarItem {
                path: mtab.path.clone(),
                label: fname,
                is_active,
                font_style,
                tab_w,
                select_action: HitAction::SelectMediaTab(m_idx),
                close_action: Some(HitAction::CloseMediaTab(m_idx)),
                buffer_idx: None,
            });
        }

        // 确保当前激活的文件/媒体标签在多标签溢出时始终保持可见
        let avail_tab_strip_w = (tabs_right_limit - tab_x).max(120);
        let mut first_visible = 0usize;
        if let Some(active_pos) = bar_items.iter().position(|it| it.is_active) {
            let mut prefix_w: i32 = bar_items[..=active_pos]
                .iter()
                .map(|it| it.tab_w + 2)
                .sum();
            while first_visible < active_pos && prefix_w > avail_tab_strip_w {
                prefix_w -= bar_items[first_visible].tab_w + 2;
                first_visible += 1;
            }
        }

        for item in bar_items.into_iter().skip(first_visible) {
            if tab_x + item.tab_w > tabs_right_limit {
                break;
            }
            let tab_rect = Rect::new(tab_x, rect.y + 3, item.tab_w, 30);
            if let Some(b_idx) = item.buffer_idx {
                self.tab_rects.push((tab_rect, b_idx));
            }
            let tab_hovered = !item.is_active && tab_rect.contains(hx, hy);

            if item.is_active {
                canvas.fill_rounded_rect(tab_rect, 6, t.tab_active_bg);
                canvas.fill_rounded_rect(
                    Rect::new(tab_x + 10, rect.y + 31, item.tab_w - 20, 3),
                    1,
                    t.tab_underline_accent,
                );
            } else if tab_hovered {
                canvas.fill_rounded_rect(tab_rect, 6, t.panel_header_bg);
            }
            canvas.draw_path_file_icon(tab_x + 10, rect.y + 11, &item.path);
            let label_max_w = (item.tab_w - 46).max(32);
            let shown_label =
                canvas.ellipsize_filename(&item.label, label_max_w, item.font_style);
            canvas.draw_text_styled(
                tab_x + 28,
                rect.y + 9,
                &shown_label,
                if item.is_active || tab_hovered {
                    t.text_primary
                } else {
                    t.text_secondary
                },
                label_max_w,
                item.font_style,
            );
            self.hit_regions.push((tab_rect, item.select_action));

            if let Some(close_act) = item.close_action {
                let close_rect = Rect::new(tab_x + item.tab_w - 22, rect.y + 9, 16, 16);
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
                self.hit_regions.push((close_rect, close_act));
            }
            tab_x += item.tab_w + 2;
        }

        if let Some(diff_tab_info) = self
            .active_editor_diff
            .as_ref()
            .map(|d| d.path.clone())
        {
            let fname = diff_tab_info
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("diff");
            let diff_label = format!("Diff: {fname}");
            let (tw, _) = canvas.measure_text(&diff_label, FontStyle::UiBold);
            let dtab_w = (tw + 52)
                .clamp(124, 240)
                .min((rect.x + rect.w - tab_x - 6).max(110));
            let dtab_x = tab_x.min(rect.x + rect.w - dtab_w - 6);
            let dtab_rect = Rect::new(dtab_x, rect.y + 3, dtab_w, 30);
            canvas.fill_rounded_rect(dtab_rect, 6, t.tab_active_bg);
            canvas.fill_rounded_rect(
                Rect::new(dtab_x + 10, rect.y + 31, dtab_w - 20, 3),
                1,
                t.tab_underline_accent,
            );
            canvas.draw_changes_diff_icon(dtab_x + 8, rect.y + 10, t.accent_blue);
            let diff_max_w = (dtab_w - 50).max(40);
            let shown_diff =
                canvas.ellipsize_filename(&diff_label, diff_max_w, FontStyle::UiBold);
            canvas.draw_text_styled(
                dtab_x + 26,
                rect.y + 9,
                &shown_diff,
                t.text_primary,
                diff_max_w,
                FontStyle::UiBold,
            );
            let close_diff_r = Rect::new(dtab_x + dtab_w - 22, rect.y + 9, 16, 16);
            let cd_hov = close_diff_r.contains(hx, hy);
            if cd_hov {
                canvas.fill_rounded_rect(close_diff_r, 4, t.popup_hover_bg);
            }
            canvas.draw_text_centered(
                close_diff_r,
                "×",
                if cd_hov { 0xFFFFFF } else { t.text_muted },
                FontStyle::UiRegular,
            );
            self.hit_regions
                .push((close_diff_r, HitAction::CloseEditorDiff));
        }

        // 2. 面包屑栏
        let crumb_bar = Rect::new(rect.x + 1, rect.y + 35, rect.w - 2, 24);
        canvas.fill_rect(crumb_bar, t.editor_bg);
        canvas.fill_rect(
            Rect::new(rect.x + 1, rect.y + 58, rect.w - 2, 1),
            t.border_color,
        );

        if self.active_editor_diff.is_some() {
            self.render_active_editor_diff(canvas, rect, rect.y + 60);
            return;
        }

        if let Some(m_idx) = active_media_idx {
            self.render_active_media_preview(canvas, rect, m_idx, rect.y + 60);
            return;
        }

        if self.open_buffers.is_empty() {
            self.render_editor_welcome(
                canvas,
                Rect::new(rect.x, rect.y + 60, rect.w, (rect.h - 64).max(24)),
            );
            return;
        }

        let active_path_str = self
            .active_buffer()
            .path
            .strip_prefix(&self.workspace_root)
            .unwrap_or(&self.active_buffer().path)
            .to_string_lossy()
            .replace('\\', "  ›  ");
        canvas.draw_text_ellipsis(
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
                    if can_accept {
                        t.run_green
                    } else {
                        t.border_color
                    },
                    if can_accept {
                        t.diff_added_bg
                    } else {
                        t.window_bg
                    },
                );
                canvas.draw_text_centered(
                    accept_btn,
                    if inline_session.loading {
                        self.tr("生成中...", "Generating...")
                    } else {
                        self.tr("✓ 接受 (Tab)", "✓ Accept (Tab)")
                    },
                    if can_accept {
                        t.diff_added_fg
                    } else {
                        t.text_muted
                    },
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
        let lineno_col_w = (sample_w + 10)
            .max(char_w * (pad_digits as i32) + 8)
            .max(46);
        let blame_col_w = if self.git_blame_visible { 172 } else { 0 };
        let gutter_w = (8 + lineno_col_w + blame_col_w + 26).max(76);

        let code_start_x = rect.x + gutter_w + 14;
        self.editor_code_start_x = code_start_x;
        let right_limit = rect.x + rect.w - 14;
        let horiz_avail_w = (right_limit - code_start_x).max(24);
        let max_line_px =
            self.ensure_editor_max_line_width_cached(&buf_path, buf_rev, char_w, canvas);
        let content_total_w = max_line_px + 56;
        let has_horiz_overflow = content_total_w > horiz_avail_w;
        let max_scroll_x = (content_total_w - horiz_avail_w).max(0);
        self.editor_scroll_x = self.editor_scroll_x.clamp(0, max_scroll_x);
        let horiz_bar_h = if has_horiz_overflow { 11 } else { 0 };

        let gutter_full_h = (rect.y + rect.h - code_top_y - 2).max(24);
        let code_area_h = (rect.y + rect.h - code_top_y - 4 - horiz_bar_h).max(24);
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
            Rect::new(rect.x + 1, code_top_y, gutter_w, gutter_full_h),
            t.editor_gutter_bg,
        );
        canvas.fill_rect(
            Rect::new(rect.x + gutter_w, code_top_y, 1, gutter_full_h),
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

        let code_clip_rect = Rect::new(
            code_body_rect.x,
            code_body_rect.y,
            (right_limit - code_body_rect.x).max(10),
            code_body_rect.h,
        );

        let file_working_diff = self.git_engine.working_diffs.get(&buf_path);
        let file_blame_lines = if self.git_blame_visible {
            self.git_engine.file_blames.get(&buf_path).cloned()
        } else {
            None
        };
        let blame_sep_x = rect.x + 8 + lineno_col_w + 24;
        if self.git_blame_visible {
            canvas.fill_rect(
                Rect::new(blame_sep_x, code_top_y, 1, gutter_full_h),
                blend_rgb(t.border_color, t.editor_gutter_bg, 0.55),
            );
        }

        for (vis_idx, (line_idx, line_str)) in visible_lines.iter().enumerate() {
            let line_idx = *line_idx;
            let row_y = code_top_y + (vis_idx as i32) * line_h;

            if line_idx == cursor_line {
                canvas.fill_rect(
                    Rect::new(rect.x + gutter_w + 1, row_y, rect.w - gutter_w - 12, line_h),
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

            if self.git_blame_visible {
                if let Some(blame) = file_blame_lines.as_ref().and_then(|v| v.get(line_idx)) {
                    let blame_rect = Rect::new(blame_sep_x + 4, row_y, blame_col_w - 8, line_h);
                    let bhov = blame_rect.contains(hx, hy);
                    let author_short: String = blame.author.chars().take(11).collect();
                    let label = format!("{} {}", blame.date, author_short);
                    canvas.draw_text_ellipsis(
                        blame_rect.x + 2,
                        row_y + 3,
                        &label,
                        if bhov { t.accent_blue } else { t.text_muted },
                        blame_rect.w - 4,
                        FontStyle::UiSmall,
                    );
                    if !blame.commit_short.is_empty() {
                        self.hit_regions.push((
                            blame_rect,
                            HitAction::InspectBlameCommit(blame.commit_short.clone()),
                        ));
                    }
                } else {
                    canvas.draw_text_styled(
                        blame_sep_x + 6,
                        row_y + 3,
                        "…",
                        t.text_muted,
                        24,
                        FontStyle::UiSmall,
                    );
                }
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

            let code_row = Rect::new(rect.x + gutter_w + 1, row_y, rect.w - gutter_w - 12, line_h);
            self.hit_regions
                .push((code_row, HitAction::ClickEditorLine));

            // 字符级选区高亮渲染
            if let Some(((sl, sc), (el, ec))) = sel_range {
                if line_idx >= sl && line_idx <= el {
                    let line_chars = line_str.chars().count();
                    let col_start = if line_idx == sl {
                        sc.min(line_chars)
                    } else {
                        0
                    };
                    let col_end = if line_idx == el {
                        ec.min(line_chars)
                    } else {
                        line_chars
                    };
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
                let max_ime_x = (right_limit - 2).max(code_body_rect.x);
                self.ime_spot = (
                    caret_x.clamp(code_body_rect.x, max_ime_x),
                    row_y + line_h,
                );
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

        // 编辑器水平滚动条（当存在超长行超出可视区时显示）
        if has_horiz_overflow {
            let horiz_track = Rect::new(
                code_body_rect.x + 2,
                rect.y + rect.h - 10,
                (code_body_rect.w - 4).max(16),
                7,
            );
            self.editor_horiz_scroll_info = ScrollTrackInfo {
                track: horiz_track,
                visible: horiz_avail_w.max(1) as usize,
                total: content_total_w.max(1) as usize,
            };
            canvas.draw_horizontal_scrollbar(
                horiz_track,
                self.editor_scroll_x as usize,
                horiz_avail_w.max(1) as usize,
                content_total_w.max(1) as usize,
                t.border_color,
            );
        } else {
            self.editor_horiz_scroll_info = ScrollTrackInfo::default();
        }
    }

    /// 在中央文件编辑器窗口内渲染常见媒体格式（位图图片 / SVG 矢量 / 音频波形 / 视频缩略图与属性）预览视图
    fn render_active_media_preview(
        &mut self,
        canvas: &mut SurfaceCanvas,
        rect: Rect,
        tab_idx: usize,
        body_top_y: i32,
    ) {
        let Some(tab) = self.media_tabs.get(tab_idx).cloned() else {
            return;
        };
        self.editor_scroll_info = ScrollTrackInfo::default();
        self.editor_horiz_scroll_info = ScrollTrackInfo::default();

        let t = self.theme;
        let (hx, hy) = self.hover_pos;

        // 1. 面包屑栏右侧操作按钮（系统打开、SVG 查看源码、图片缩放 +/-/100%）
        let mut right_x = rect.x + rect.w - 8;
        let ext_w = 78;
        right_x -= ext_w;
        let ext_btn = Rect::new(right_x, rect.y + 37, ext_w, 19);
        let ext_hov = ext_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            ext_btn,
            4,
            if ext_hov { t.accent_blue } else { t.border_color },
            if ext_hov { t.popup_hover_bg } else { t.panel_bg },
        );
        canvas.draw_text_centered(
            ext_btn,
            self.tr("系统打开", "Open Ext"),
            if ext_hov { 0xFFFFFF } else { t.text_primary },
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((ext_btn, HitAction::OpenExternalMedia(tab.path.clone())));

        if tab.kind == MediaKind::Svg {
            let src_w = 74;
            right_x -= src_w + 6;
            let src_btn = Rect::new(right_x, rect.y + 37, src_w, 19);
            let src_hov = src_btn.contains(hx, hy);
            canvas.stroke_rounded_rect(
                src_btn,
                4,
                if src_hov { t.accent_blue } else { t.border_color },
                if src_hov { t.popup_hover_bg } else { t.panel_bg },
            );
            canvas.draw_text_centered(
                src_btn,
                self.tr("查看源码", "Source"),
                if src_hov { 0xFFFFFF } else { t.text_primary },
                FontStyle::UiSmall,
            );
            self.hit_regions
                .push((src_btn, HitAction::OpenMediaAsText(tab.path.clone())));
        }

        if matches!(tab.kind, MediaKind::Image | MediaKind::Svg) {
            let plus_btn = Rect::new(right_x - 28, rect.y + 37, 22, 19);
            let pct_btn = Rect::new(plus_btn.x - 46, rect.y + 37, 44, 19);
            let minus_btn = Rect::new(pct_btn.x - 24, rect.y + 37, 22, 19);
            right_x = minus_btn.x;
            for (btn, label, delta) in [
                (minus_btn, "-", -25),
                (pct_btn, &format!("{}%", tab.zoom_pct) as &str, 0),
                (plus_btn, "+", 25),
            ] {
                let bhov = btn.contains(hx, hy);
                canvas.stroke_rounded_rect(
                    btn,
                    4,
                    if bhov { t.accent_blue } else { t.border_color },
                    if bhov { t.popup_hover_bg } else { t.panel_bg },
                );
                canvas.draw_text_centered(
                    btn,
                    label,
                    if bhov { 0xFFFFFF } else { t.text_secondary },
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((btn, HitAction::AdjustMediaZoom(delta)));
            }
        }

        // 面包屑摘要：路径 · 格式 · 大小 · 分辨率/时长
        let rel_path = tab
            .path
            .strip_prefix(&self.workspace_root)
            .unwrap_or(&tab.path)
            .to_string_lossy()
            .replace('\\', "  ›  ");
        let mut meta_parts = vec![
            rel_path,
            tab.format_label.clone(),
            format_media_file_size(tab.file_size_bytes),
        ];
        if let Some((w, h)) = tab.orig_dimensions {
            meta_parts.push(format!("{w} × {h} px"));
        }
        if let Some(dur) = tab.duration_secs {
            meta_parts.push(format_media_duration(dur));
        }
        let crumb_txt = meta_parts.join("  ·  ");
        canvas.draw_text_ellipsis(
            rect.x + 14,
            rect.y + 39,
            &crumb_txt,
            t.text_secondary,
            (right_x - rect.x - 22).max(48),
            FontStyle::UiSmall,
        );

        let body = Rect::new(
            rect.x + 14,
            body_top_y + 10,
            (rect.w - 28).max(80),
            (rect.y + rect.h - body_top_y - 22).max(80),
        );
        match tab.kind {
            MediaKind::Image | MediaKind::Svg => self.render_media_image_body(canvas, body, &tab),
            MediaKind::Audio => self.render_media_audio_body(canvas, body, &tab),
            MediaKind::Video => self.render_media_video_body(canvas, body, &tab),
        }
    }

    fn render_media_image_body(
        &self,
        canvas: &mut SurfaceCanvas,
        body: Rect,
        tab: &MediaPreviewTab,
    ) {
        let t = self.theme;
        canvas.stroke_rounded_rect(body, 8, t.border_color, t.window_bg);

        let inner = Rect::new(
            body.x + 14,
            body.y + 14,
            (body.w - 28).max(32),
            (body.h - 28).max(32),
        );
        // 透明通道棋盘格底纹，方便识别透明 PNG/SVG 边界
        let tile = 16;
        let c1 = t.window_bg;
        let c2 = blend_rgb(t.window_bg, t.text_primary, 0.045);
        for row in 0..((inner.h / tile) + 1) {
            let ty = inner.y + row * tile;
            let th = tile.min(inner.y + inner.h - ty);
            if th <= 0 {
                break;
            }
            for col in 0..((inner.w / tile) + 1) {
                let tx = inner.x + col * tile;
                let tw = tile.min(inner.x + inner.w - tx);
                if tw <= 0 {
                    break;
                }
                let col_rgb = if (row + col) % 2 == 0 { c1 } else { c2 };
                canvas.fill_rect(Rect::new(tx, ty, tw, th), col_rgb);
            }
        }

        if let Some(ref im) = tab.preview_image {
            let scale = (tab.zoom_pct as f32 / 100.0).clamp(0.25, 3.0);
            let target_w = ((inner.w as f32 * scale).round() as i32).clamp(24, inner.w);
            let target_h = ((inner.h as f32 * scale).round() as i32).clamp(24, inner.h);
            let target = Rect::new(
                inner.x + (inner.w - target_w) / 2,
                inner.y + (inner.h - target_h) / 2,
                target_w,
                target_h,
            );
            canvas.draw_rgba_image_fit(
                target,
                6,
                im.width,
                im.height,
                &im.rgba,
                t.window_bg,
                t.border_color,
            );
        } else {
            let msg = if tab.kind == MediaKind::Svg {
                self.tr(
                    "无法光栅化此复杂 SVG，可点击右上角「查看源码」或「系统打开」",
                    "Unable to rasterize this SVG. Click Source or Open Ext above.",
                )
            } else {
                self.tr(
                    "无法解码此图片缩略图，可点击右上角「系统打开」查看",
                    "Unable to decode this image format. Click Open Ext above.",
                )
            };
            canvas.draw_text_centered(inner, msg, t.warning_yellow, FontStyle::UiRegular);
        }
    }

    fn render_media_audio_body(
        &mut self,
        canvas: &mut SurfaceCanvas,
        body: Rect,
        tab: &MediaPreviewTab,
    ) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        canvas.stroke_rounded_rect(body, 8, t.border_color, t.window_bg);

        let card_w = (body.w - 36).clamp(220, 620);
        let card_h = (body.h - 32).clamp(180, 320);
        let card = Rect::new(
            body.x + (body.w - card_w) / 2,
            body.y + (body.h - card_h) / 2,
            card_w,
            card_h,
        );
        canvas.stroke_rounded_rect(card, 10, t.border_color, t.panel_bg);

        let fname = tab
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio");
        let title_str = canvas.ellipsize_filename(
            &format!("♪  {fname}"),
            card.w - 110,
            FontStyle::UiBold,
        );
        canvas.draw_text_styled(
            card.x + 18,
            card.y + 16,
            &title_str,
            t.text_primary,
            card.w - 110,
            FontStyle::UiBold,
        );

        let fmt_badge = Rect::new(card.x + card.w - 76, card.y + 15, 58, 22);
        canvas.fill_rounded_rect(fmt_badge, 5, blend_rgb(t.panel_bg, t.accent_blue, 0.22));
        canvas.draw_text_centered(fmt_badge, &tab.format_label, t.accent_blue, FontStyle::UiSmall);

        let mut details = vec![format_media_file_size(tab.file_size_bytes)];
        if let Some(d) = tab.duration_secs {
            details.push(format!("{} {}", self.tr("时长", "Duration"), format_media_duration(d)));
        }
        if let Some(sr) = tab.sample_rate_hz {
            details.push(format!("{sr} Hz"));
        }
        if let Some(ch) = tab.channels {
            details.push(format!("{ch} ch"));
        }
        canvas.draw_text_ellipsis(
            card.x + 18,
            card.y + 42,
            &details.join("  ·  "),
            t.text_secondary,
            card.w - 36,
            FontStyle::UiSmall,
        );

        // 音频波形可视化区
        let wave_box = Rect::new(
            card.x + 18,
            card.y + 72,
            (card.w - 36).max(60),
            (card.h - 134).max(48),
        );
        canvas.stroke_rounded_rect(wave_box, 8, t.border_color, t.window_bg);
        let mid_y = wave_box.y + wave_box.h / 2;
        canvas.fill_rect(
            Rect::new(wave_box.x + 10, mid_y, (wave_box.w - 20).max(8), 1),
            blend_rgb(t.border_color, t.window_bg, 0.5),
        );

        let bars = 64usize;
        let inner_w = (wave_box.w - 24).max(bars as i32);
        let step_w = (inner_w as f32 / bars as f32).max(2.0);
        let bar_w = (step_w * 0.62).round().max(2.0) as i32;
        let max_half_h = ((wave_box.h - 18) / 2).max(6);
        for i in 0..bars {
            let amp = tab
                .waveform_peaks
                .get(i)
                .copied()
                .unwrap_or_else(|| {
                    let t1 = ((i as f32 * 0.37).sin().abs() + 0.25) * 0.65;
                    t1.clamp(0.12, 0.92)
                });
            let bh = ((amp * max_half_h as f32).round() as i32).max(2);
            let bx = wave_box.x + 12 + (i as f32 * step_w).round() as i32;
            if bx + bar_w > wave_box.x + wave_box.w - 8 {
                break;
            }
            let bar_col = if tab.waveform_peaks.is_empty() {
                blend_rgb(t.text_muted, t.accent_blue, 0.45)
            } else {
                t.accent_blue
            };
            canvas.fill_rounded_rect(
                Rect::new(bx, mid_y - bh, bar_w, bh * 2),
                1,
                bar_col,
            );
        }

        let play_btn = Rect::new(
            card.x + (card.w - 190) / 2,
            card.y + card.h - 46,
            190,
            28,
        );
        let phov = play_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            play_btn,
            6,
            t.accent_blue,
            if phov {
                t.accent_blue
            } else {
                blend_rgb(t.panel_bg, t.accent_blue, 0.20)
            },
        );
        canvas.draw_text_centered(
            play_btn,
            self.tr("▶ 在系统播放器中播放", "▶ Play in System Player"),
            if phov { 0xFFFFFF } else { t.text_primary },
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((play_btn, HitAction::OpenExternalMedia(tab.path.clone())));
    }

    fn render_media_video_body(
        &mut self,
        canvas: &mut SurfaceCanvas,
        body: Rect,
        tab: &MediaPreviewTab,
    ) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        canvas.stroke_rounded_rect(body, 8, t.border_color, t.window_bg);

        let card_w = (body.w - 36).clamp(240, 720);
        let card_h = (body.h - 32).clamp(200, 440);
        let card = Rect::new(
            body.x + (body.w - card_w) / 2,
            body.y + (body.h - card_h) / 2,
            card_w,
            card_h,
        );
        canvas.stroke_rounded_rect(card, 10, t.border_color, t.panel_bg);

        let fname = tab
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("video");
        let title_str = canvas.ellipsize_filename(
            &format!("▶  {fname}"),
            card.w - 110,
            FontStyle::UiBold,
        );
        canvas.draw_text_styled(
            card.x + 18,
            card.y + 14,
            &title_str,
            t.text_primary,
            card.w - 110,
            FontStyle::UiBold,
        );

        let fmt_badge = Rect::new(card.x + card.w - 76, card.y + 13, 58, 22);
        canvas.fill_rounded_rect(fmt_badge, 5, blend_rgb(t.panel_bg, t.accent_blue, 0.22));
        canvas.draw_text_centered(fmt_badge, &tab.format_label, t.accent_blue, FontStyle::UiSmall);

        let frame_rect = Rect::new(
            card.x + 18,
            card.y + 44,
            (card.w - 36).max(80),
            (card.h - 102).max(80),
        );
        if let Some(ref thumb) = tab.preview_image {
            canvas.draw_rgba_image_fit(
                frame_rect,
                8,
                thumb.width,
                thumb.height,
                &thumb.rgba,
                t.window_bg,
                t.border_color,
            );
        } else {
            canvas.stroke_rounded_rect(frame_rect, 8, t.border_color, t.window_bg);
            canvas.draw_text_centered(
                frame_rect,
                self.tr("▶ 视频预览帧", "▶ Video Preview"),
                t.text_secondary,
                FontStyle::UiBold,
            );
        }

        // 底部播放栏与文件元数据
        let mut info = vec![format_media_file_size(tab.file_size_bytes)];
        if let Some(d) = tab.duration_secs {
            info.push(format_media_duration(d));
        }
        if let Some((w, h)) = tab.orig_dimensions {
            info.push(format!("{w}×{h}"));
        }
        let open_btn = Rect::new(card.x + card.w - 208, card.y + card.h - 42, 190, 26);
        canvas.draw_text_ellipsis(
            card.x + 18,
            card.y + card.h - 38,
            &info.join("  ·  "),
            t.text_secondary,
            (open_btn.x - card.x - 28).max(40),
            FontStyle::UiSmall,
        );
        let ohov = open_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            open_btn,
            6,
            t.accent_blue,
            if ohov {
                t.accent_blue
            } else {
                blend_rgb(t.panel_bg, t.accent_blue, 0.20)
            },
        );
        canvas.draw_text_centered(
            open_btn,
            self.tr("▶ 使用系统播放器播放", "▶ Play in System Player"),
            if ohov { 0xFFFFFF } else { t.text_primary },
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((open_btn, HitAction::OpenExternalMedia(tab.path.clone())));
    }

    /// 在中央文件编辑器窗口内渲染 IDEA 风格工作区 / AI 会话文件 Diff 对比视图
    fn render_active_editor_diff(
        &mut self,
        canvas: &mut SurfaceCanvas,
        rect: Rect,
        code_top_y: i32,
    ) {
        let Some(diff_snap) = self.active_editor_diff.clone() else {
            return;
        };
        let t = self.theme;
        let (hx, hy) = self.hover_pos;

        // 面包屑 / Diff 头部栏操作按钮：[打开源文件] [× 关闭 Diff]
        let rel_path = diff_snap
            .path
            .strip_prefix(&self.workspace_root)
            .unwrap_or(&diff_snap.path)
            .to_string_lossy()
            .replace('\\', "  ›  ");
        let close_btn = Rect::new(rect.x + rect.w - 78, rect.y + 37, 70, 19);
        let open_btn = Rect::new(close_btn.x - 84, rect.y + 37, 78, 19);
        let stats_str = format!("+{}  -{}", diff_snap.added_lines, diff_snap.removed_lines);
        let crumb_txt = format!("{rel_path}  ·  {}   ({stats_str})", diff_snap.subtitle);
        canvas.draw_text_ellipsis(
            rect.x + 14,
            rect.y + 39,
            &crumb_txt,
            t.text_secondary,
            (open_btn.x - rect.x - 22).max(60),
            FontStyle::UiSmall,
        );

        let open_hov = open_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            open_btn,
            4,
            if open_hov { t.accent_blue } else { t.border_color },
            if open_hov { t.popup_hover_bg } else { t.panel_bg },
        );
        canvas.draw_text_centered(
            open_btn,
            self.tr("打开文件", "Open File"),
            if open_hov { 0xFFFFFF } else { t.text_primary },
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((open_btn, HitAction::OpenDiffSourceFile(diff_snap.path.clone())));

        let close_hov = close_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            close_btn,
            4,
            if close_hov { t.accent_blue } else { t.border_color },
            if close_hov { t.popup_hover_bg } else { t.panel_bg },
        );
        canvas.draw_text_centered(
            close_btn,
            self.tr("× 关闭", "× Close"),
            if close_hov { 0xFFFFFF } else { t.text_secondary },
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((close_btn, HitAction::CloseEditorDiff));

        // 双行号列 Gutter (Old | New | +/-)
        let char_w = canvas.measure_text("8", FontStyle::CodeMono).0.max(8);
        self.editor_char_w = char_w;
        let max_digits = diff_snap.diff_lines.len().max(1).ilog10() as usize + 1;
        let pad_digits = max_digits.max(4);
        let col_w = (char_w * (pad_digits as i32) + 8).max(40);
        let gutter_w = col_w * 2 + 22;

        let code_start_x = rect.x + gutter_w + 14;
        self.editor_code_start_x = code_start_x;
        let right_limit = rect.x + rect.w - 14;
        let horiz_avail_w = (right_limit - code_start_x).max(24);
        let max_line_px = diff_snap
            .diff_lines
            .iter()
            .map(|dl| canvas.measure_text(&dl.content, FontStyle::CodeMono).0)
            .max()
            .unwrap_or(0);
        let content_total_w = max_line_px + 48;
        let has_horiz_overflow = content_total_w > horiz_avail_w;
        let max_scroll_x = (content_total_w - horiz_avail_w).max(0);
        let clamped_scroll_x = diff_snap.scroll_x.clamp(0, max_scroll_x);
        let horiz_bar_h = if has_horiz_overflow { 11 } else { 0 };

        let gutter_full_h = (rect.y + rect.h - code_top_y - 2).max(24);
        let code_area_h = (rect.y + rect.h - code_top_y - 4 - horiz_bar_h).max(24);
        let code_body_rect = Rect::new(
            rect.x + gutter_w + 1,
            code_top_y,
            (rect.w - gutter_w - 12).max(24),
            code_area_h,
        );
        self.editor_code_rect = code_body_rect;

        canvas.fill_rect(
            Rect::new(rect.x + 1, code_top_y, gutter_w, gutter_full_h),
            t.editor_gutter_bg,
        );
        canvas.fill_rect(
            Rect::new(rect.x + gutter_w, code_top_y, 1, gutter_full_h),
            t.border_color,
        );

        let line_h = 22;
        let max_visible_lines = (code_area_h / line_h).max(1) as usize;
        self.editor_visible_lines = max_visible_lines;
        let total_lines = diff_snap.diff_lines.len().max(1);
        let max_scroll = diff_snap.diff_lines.len().saturating_sub(max_visible_lines);
        let clamped_scroll = diff_snap.scroll.min(max_scroll);

        if let Some(ref mut ed_diff) = self.active_editor_diff {
            ed_diff.scroll = clamped_scroll;
            ed_diff.scroll_x = clamped_scroll_x;
        }

        if diff_snap.diff_lines.is_empty() {
            canvas.draw_text_styled(
                code_start_x,
                code_top_y + 18,
                self.tr("当前文件没有代码差异。", "No diff changes for this file."),
                t.text_muted,
                horiz_avail_w,
                FontStyle::UiRegular,
            );
        } else {
            let code_clip_rect = Rect::new(
                code_body_rect.x,
                code_body_rect.y,
                (right_limit - code_body_rect.x).max(10),
                code_body_rect.h,
            );
            for (vis_idx, dl) in diff_snap
                .diff_lines
                .iter()
                .skip(clamped_scroll)
                .take(max_visible_lines)
                .enumerate()
            {
                let row_y = code_top_y + (vis_idx as i32) * line_h;
                let (bg, fg, sign, stripe_opt) = match dl.kind {
                    DiffLineKind::Added => {
                        (t.diff_added_bg, t.diff_added_fg, "+", Some(t.run_green))
                    }
                    DiffLineKind::Removed => (
                        t.diff_removed_bg,
                        t.diff_removed_fg,
                        "-",
                        Some(t.breakpoint_red),
                    ),
                    DiffLineKind::Context => (t.editor_bg, t.text_primary, " ", None),
                };

                if dl.kind != DiffLineKind::Context {
                    canvas.fill_rect(
                        Rect::new(rect.x + gutter_w + 1, row_y, code_body_rect.w, line_h),
                        bg,
                    );
                }
                if let Some(sc) = stripe_opt {
                    canvas.fill_rect(Rect::new(rect.x + gutter_w - 3, row_y + 1, 3, line_h - 2), sc);
                }

                let old_s = dl
                    .old_lineno
                    .map(|n| format!("{:>width$}", n + 1, width = pad_digits))
                    .unwrap_or_else(|| " ".repeat(pad_digits));
                let new_s = dl
                    .new_lineno
                    .map(|n| format!("{:>width$}", n + 1, width = pad_digits))
                    .unwrap_or_else(|| " ".repeat(pad_digits));
                canvas.draw_text_styled(
                    rect.x + 6,
                    row_y + 2,
                    &old_s,
                    t.text_muted,
                    col_w,
                    FontStyle::CodeMono,
                );
                canvas.draw_text_styled(
                    rect.x + 6 + col_w,
                    row_y + 2,
                    &new_s,
                    t.text_muted,
                    col_w,
                    FontStyle::CodeMono,
                );
                canvas.draw_text_styled(
                    rect.x + gutter_w - 16,
                    row_y + 2,
                    sign,
                    fg,
                    14,
                    FontStyle::CodeMonoBold,
                );

                canvas.draw_text_clipped(
                    code_start_x - clamped_scroll_x,
                    row_y + 2,
                    &dl.content,
                    fg,
                    code_clip_rect,
                    FontStyle::CodeMono,
                );
            }
        }

        let track = Rect::new(rect.x + rect.w - 10, code_top_y, 7, code_area_h);
        self.editor_scroll_info = ScrollTrackInfo {
            track,
            visible: max_visible_lines,
            total: total_lines,
        };
        canvas.draw_vertical_scrollbar(
            track,
            clamped_scroll,
            max_visible_lines,
            total_lines,
            t.border_color,
        );

        if has_horiz_overflow {
            let horiz_track = Rect::new(
                code_body_rect.x + 2,
                rect.y + rect.h - 10,
                (code_body_rect.w - 4).max(16),
                7,
            );
            self.editor_horiz_scroll_info = ScrollTrackInfo {
                track: horiz_track,
                visible: horiz_avail_w.max(1) as usize,
                total: content_total_w.max(1) as usize,
            };
            canvas.draw_horizontal_scrollbar(
                horiz_track,
                clamped_scroll_x as usize,
                horiz_avail_w.max(1) as usize,
                content_total_w.max(1) as usize,
                t.border_color,
            );
        } else {
            self.editor_horiz_scroll_info = ScrollTrackInfo::default();
        }
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
        self.arm_pane_header_drag(canvas, rect, DockPane::Tooling);

        // 顶部标题栏：1:1 对齐截图终端头部 -> `终端`  `本地 ×`  `+`  `▾`   右侧切换其他底部面板
        let mut tx = rect.x + 22;
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
            self.hit_regions.push((plus_btn, HitAction::NewTerminalTab));
            tx += 26;

            canvas.draw_chevron_down(tx + 2, rect.y + 13, t.text_muted);
            tx += 18;

            let clr_btn = Rect::new(rect.x + rect.w - 300, rect.y + 5, 22, 22);
            if clr_btn.x - tx > 130 {
                canvas.draw_text_styled(
                    tx + 6,
                    rect.y + 8,
                    self.tr(
                        "Ctrl+C 复制 · Ctrl+V 粘贴 · 点击链接或文件路径打开",
                        "Ctrl+C Copy · Ctrl+V Paste · Click a link or file path to open",
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
            canvas.draw_text_styled(
                tx,
                rect.y + 7,
                label,
                t.text_primary,
                130,
                FontStyle::UiBold,
            );
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
            self.hit_regions.push((b, HitAction::SelectBottomTool(id)));
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
                    self.terminal.active_pty_mut().resize_pty(rows, cols);
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
                let link_root = self.workspace_root.clone();
                let link_cwd = self.terminal.cwd.clone();

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
                            let c1 = if idx == el {
                                ec.min(char_len)
                            } else {
                                char_len
                            };
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

                    let links = terminal_links_in_line(&raw, &link_root, &link_cwd);
                    let mut link_geom = Vec::new();
                    for link in links {
                        let prefix: String = raw.chars().take(link.start_char).collect();
                        let slice: String = raw
                            .chars()
                            .skip(link.start_char)
                            .take(link.end_char.saturating_sub(link.start_char))
                            .collect();
                        if slice.is_empty() {
                            continue;
                        }
                        let x0 = text_left + canvas.measure_text(&prefix, FontStyle::CodeMono).0;
                        if x0 >= right_limit {
                            continue;
                        }
                        let width = canvas.measure_text(&slice, FontStyle::CodeMono).0.max(4);
                        let rect = Rect::new(x0, y, (width).min(right_limit - x0).max(4), line_h);
                        let hot = rect.contains(hx, hy);
                        link_geom.push((rect, slice, link.action, hot));
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

                    for (rect, slice, action, hot) in link_geom {
                        if hot {
                            canvas.draw_text_styled(
                                rect.x,
                                y,
                                &slice,
                                t.syn_function,
                                rect.w + 4,
                                FontStyle::CodeMono,
                            );
                        }
                        canvas.fill_rect(
                            Rect::new(rect.x, y + line_h - 3, rect.w.max(2), 1),
                            if hot { t.syn_function } else { t.accent_blue },
                        );
                        self.hit_regions.push((rect, action));
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
                let tb_rect = Rect::new(
                    content_rect.x + 6,
                    content_rect.y + 4,
                    content_rect.w - 12,
                    28,
                );
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
                    let clr_s =
                        Rect::new(search_box.x + search_box.w - 18, search_box.y + 4, 15, 16);
                    canvas.draw_text_centered(clr_s, "×", t.text_muted, FontStyle::UiSmall);
                    self.hit_regions.push((clr_s, HitAction::ClearGitLogSearch));
                }

                let mut bx = search_box.x + search_box.w + 8;
                if let Some(ref fpath) = self.git_file_history_filter.clone() {
                    let fname = fpath
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file");
                    let chip_txt = format!("File: {fname} ×");
                    let (cw, _) = canvas.measure_text(&chip_txt, FontStyle::UiSmall);
                    let chip_w = (cw + 14).min(180);
                    let chip_r = Rect::new(bx, tb_rect.y + 2, chip_w, 24);
                    canvas.stroke_rounded_rect(
                        chip_r,
                        4,
                        t.accent_blue,
                        blend_rgb(t.window_bg, t.accent_blue, 0.16),
                    );
                    canvas.draw_text_ellipsis(
                        chip_r.x + 6,
                        chip_r.y + 4,
                        &chip_txt,
                        t.text_primary,
                        chip_w - 12,
                        FontStyle::UiSmall,
                    );
                    self.hit_regions
                        .push((chip_r, HitAction::ClearGitFileHistoryFilter));
                    bx += chip_w + 6;
                }
                let tool_btns: Vec<(&str, u32, HitAction)> = vec![
                    (
                        self.tr("↻ 刷新", "↻ Refresh"),
                        t.text_secondary,
                        HitAction::RefreshGitStatus,
                    ),
                    ("Fetch", t.text_secondary, HitAction::GitFetchAll),
                    (
                        "↓ Pull",
                        t.syn_function,
                        HitAction::ExecuteIdeAction(IdeAction::GitPull),
                    ),
                    (
                        "↑ Push",
                        t.run_green,
                        HitAction::ExecuteIdeAction(IdeAction::GitPush),
                    ),
                    (
                        self.tr("检出", "Checkout"),
                        t.accent_blue,
                        HitAction::GitCheckoutSelectedCommit,
                    ),
                    (
                        self.tr("新建分支", "Branch"),
                        t.syn_type,
                        HitAction::GitCreateBranchAtSelectedCommit,
                    ),
                    (
                        "Cherry-Pick",
                        t.warning_yellow,
                        HitAction::GitCherryPickSelectedCommit,
                    ),
                    (
                        "Revert",
                        t.breakpoint_red,
                        HitAction::GitRevertSelectedCommit,
                    ),
                    (
                        self.tr("复制Hash", "Copy Hash"),
                        t.text_muted,
                        HitAction::GitCopySelectedCommitHash,
                    ),
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
                let col_hdr = Rect::new(
                    commits_rect.x + 1,
                    commits_rect.y + 1,
                    commits_rect.w - 2,
                    22,
                );
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
                let source_commits = if let Some(ref fpath) = self.git_file_history_filter {
                    self.git_engine
                        .file_histories
                        .get(fpath)
                        .unwrap_or(&self.git_engine.recent_commits)
                } else {
                    &self.git_engine.recent_commits
                };
                let filtered_commits: Vec<ide_git::GitLogEntry> = source_commits
                    .iter()
                    .filter(|c| {
                        if let Some(ref bf) = branch_filter {
                            let ref_match = c.refs.iter().any(|r| {
                                let clean =
                                    r.trim_start_matches("HEAD -> ").trim_start_matches("tag: ");
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
                        self.tr(
                            "正在加载 Git 提交历史与分支拓扑...",
                            "Loading Git commit history...",
                        ),
                        t.text_secondary,
                        commits_rect.w - 46,
                        FontStyle::UiRegular,
                    );
                } else if filtered_commits.is_empty() {
                    canvas.draw_text_styled(
                        commits_rect.x + 14,
                        list_top + 10,
                        self.tr("暂无匹配的 Git 提交记录", "No matching Git commits found"),
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
                        let row =
                            Rect::new(commits_rect.x + 3, cy, commits_rect.w - 14, c_row_h - 1);
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
                        self.git_commit_row_rects.push((
                            row,
                            entry.hash.clone(),
                            entry.subject.clone(),
                        ));

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
                        let subj_limit =
                            (commits_rect.x + commits_rect.w - meta_w - 8).max(text_x + 40);

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
                            let short_rf = rf
                                .trim_start_matches("HEAD -> ")
                                .trim_start_matches("tag: ");
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
                                if is_sel || hov {
                                    0xFFFFFF
                                } else {
                                    t.text_primary
                                },
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

                let c_track = Rect::new(commits_rect.x + commits_rect.w - 8, list_top, 6, list_h);
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
                    let hdr =
                        Rect::new(detail_rect.x + 2, detail_rect.y + 2, detail_rect.w - 4, 24);
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
                        canvas
                            .fill_rect(Rect::new(diff_box.x, dy, diff_box.w - 10, d_row_h - 1), bg);
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
                    let d_track =
                        Rect::new(detail_rect.x + detail_rect.w - 8, diff_box.y, 6, diff_box.h);
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
                        let meta_rect = Rect::new(
                            detail_rect.x + 4,
                            detail_rect.y + 4,
                            detail_rect.w - 8,
                            meta_h,
                        );
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
                                    format!(
                                        "· Parent: {}",
                                        detail
                                            .parents
                                            .join(", ")
                                            .chars()
                                            .take(18)
                                            .collect::<String>()
                                    )
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
                        let files_box =
                            Rect::new(detail_rect.x + 4, files_top, detail_rect.w - 8, files_h);
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
                        let diff_box =
                            Rect::new(detail_rect.x + 4, diff_top, detail_rect.w - 8, diff_h);
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
                        self.tr(
                            "点击任意提交记录查看变更文件与 Diff",
                            "Select a commit to inspect changed files & diff",
                        ),
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
        let editor_open = !self.open_buffers.is_empty();
        let lsp_status = if editor_open {
            self.lsp_hub.status_summary(buf.language)
        } else {
            self.tr("未打开文件", "No file").to_string()
        };
        let dpi_pct = (self.effective_scale() * 100.0).round() as i32;
        let right_info = if editor_open {
            format!(
                "Ln {}, Col {}   |   UTF-8   |   DPI {}%   |   {}   |   {}",
                buf.cursor_line + 1,
                buf.cursor_col + 1,
                dpi_pct,
                lsp_status,
                t.kind.display_name()
            )
        } else {
            format!(
                "DPI {}%   |   {}   |   {}",
                dpi_pct,
                lsp_status,
                t.kind.display_name()
            )
        };
        let (right_w, _) = canvas.measure_text(&right_info, FontStyle::UiSmall);
        let right_x = (rect.w - right_w - 16).max(rect.w / 2);

        let left_info = if editor_open {
            let rel_path = buf
                .path
                .strip_prefix(&self.workspace_root)
                .unwrap_or(&buf.path)
                .to_string_lossy()
                .replace('\\', "  ›  ");
            format!(
                "{}  ›  {}   |   {}",
                self.workspace_name(),
                rel_path,
                self.status_notification
            )
        } else {
            format!(
                "{}   |   {}   |   {}",
                self.workspace_name(),
                self.tr("没有打开的文件", "No file open"),
                self.status_notification
            )
        };
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
        let avail_h = (canvas.height as i32 - 52).max(200);
        let default_h: i32 = entries
            .iter()
            .map(|e| match e {
                MenuEntry::Action(_) => 29,
                MenuEntry::Separator => 9,
            })
            .sum();
        let (row_step, row_h, sep_step) = if default_h + 14 > avail_h {
            (23, 22, 7)
        } else {
            (29, 28, 9)
        };
        let content_h: i32 = entries
            .iter()
            .map(|e| match e {
                MenuEntry::Action(_) => row_step,
                MenuEntry::Separator => sep_step,
            })
            .sum();
        let menu_h = (content_h + 14).min(avail_h);

        let anchor_x = self
            .menu_category_rects
            .iter()
            .find(|(c, _)| *c == cat)
            .map(|(_, r)| r.x)
            .unwrap_or(12);
        let menu_x = anchor_x.min(canvas.width as i32 - menu_w - 8).max(8);
        let menu_y = 38;
        let box_rect = Rect::new(menu_x, menu_y, menu_w, menu_h);
        self.menu_dropdown_rect = box_rect;
        self.active_overlay_rects.push(box_rect);

        canvas.fill_rounded_shadow(box_rect, 8);
        canvas.stroke_rounded_rect(box_rect, 8, t.border_color, t.popup_bg);

        let mut iy = menu_y + 7;
        for entry in entries {
            if iy + 18 > menu_y + menu_h {
                break;
            }
            match entry {
                MenuEntry::Separator => {
                    canvas.fill_rect(
                        Rect::new(menu_x + 10, iy + sep_step / 2, menu_w - 20, 1),
                        t.border_color,
                    );
                    iy += sep_step;
                }
                MenuEntry::Action(act) => {
                    let row = Rect::new(menu_x + 6, iy, menu_w - 12, row_h);
                    let hovered = row.contains(hx, hy);
                    if hovered {
                        canvas.fill_rounded_rect(row, 5, t.popup_hover_bg);
                    }
                    if self.is_action_checked(act) {
                        canvas.draw_check_icon(
                            row.x + 8,
                            iy + (row_h - 12).max(2) / 2,
                            if hovered { 0xFFFFFF } else { t.accent_blue },
                        );
                    }
                    let sc = act.shortcut_label();
                    let (sw, _) = canvas.measure_text(sc, FontStyle::UiSmall);
                    let text_y = iy + (row_h - 18).max(2) / 2;
                    canvas.draw_text_ellipsis(
                        row.x + 26,
                        text_y,
                        act.menu_title_i18n(self.ui_lang),
                        if hovered { 0xFFFFFF } else { t.text_primary },
                        menu_w - sw - 48,
                        FontStyle::UiRegular,
                    );
                    canvas.draw_text_styled(
                        row.x + row.w - sw - 12,
                        text_y + 1,
                        sc,
                        if hovered { 0xE2E8F0 } else { t.text_muted },
                        sw + 4,
                        FontStyle::UiSmall,
                    );
                    self.hit_regions
                        .push((row, HitAction::ExecuteIdeAction(act)));
                    iy += row_step;
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
        let popup_y = 40;
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
                self.tr(
                    "推送提交... (Ctrl+Shift+K)",
                    "Push Commits... (Ctrl+Shift+K)",
                ),
                HitAction::ExecuteIdeAction(IdeAction::GitPush),
            ),
        ];
        for (label, act) in actions {
            let row = Rect::new(popup_rect.x + 6, y, popup_w - 12, 26);
            let hovered = row.contains(hx, hy);
            canvas.fill_rounded_rect(
                row,
                5,
                if hovered {
                    t.popup_hover_bg
                } else {
                    t.window_bg
                },
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
            self.tr(
                "本地分支 (点击切换 / 右侧合并或删除)",
                "Local Branches (Click to checkout)",
            ),
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
                popup_w - 170
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
            canvas.draw_text_ellipsis(
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
                let rebase_btn = Rect::new(popup_rect.x + popup_w - 158, y + 2, 50, 22);
                let rhov = rebase_btn.contains(hx, hy);
                canvas.stroke_rounded_rect(
                    rebase_btn,
                    4,
                    if rhov { t.syn_type } else { t.border_color },
                    t.window_bg,
                );
                canvas.draw_text_centered(
                    rebase_btn,
                    self.tr("变基", "Rebase"),
                    if rhov { t.syn_type } else { t.text_secondary },
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((rebase_btn, HitAction::GitRebaseBranch(b.clone())));

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
                    if mhov {
                        t.accent_blue
                    } else {
                        t.text_secondary
                    },
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((merge_btn, HitAction::GitMergeBranch(b.clone())));

                let del_btn = Rect::new(popup_rect.x + popup_w - 52, y + 2, 44, 22);
                let dhov = del_btn.contains(hx, hy);
                canvas.stroke_rounded_rect(
                    del_btn,
                    4,
                    if dhov {
                        t.breakpoint_red
                    } else {
                        t.border_color
                    },
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
        let popup_y = 40;
        let popup_rect = Rect::new(popup_x, popup_y, popup_w, popup_h);
        self.active_overlay_rects.push(popup_rect);

        canvas.fill_rounded_shadow(popup_rect, 8);
        canvas.stroke_rounded_rect(popup_rect, 8, t.border_color, t.popup_bg);

        let title = format!(
            "{} ({})",
            self.workspace_name(),
            self.tr("工作区", "Workspace")
        );
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
        canvas.fill_rect(Rect::new(popup_x + 10, y, popup_w - 20, 1), t.border_color);
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
                self.tr(
                    "最近打开的工作区 (点击切换或在新窗口开启)",
                    "Recent Workspaces",
                ),
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
                    if nw_hov {
                        t.accent_blue
                    } else {
                        t.border_color
                    },
                    if nw_hov {
                        t.popup_hover_bg
                    } else {
                        t.window_bg
                    },
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
        let max_visible = 9usize;
        let total = cfgs.len();
        let visible_rows = total.clamp(1, max_visible);
        let max_scroll = total.saturating_sub(max_visible);
        if self.run_config_popup_scroll > max_scroll {
            self.run_config_popup_scroll = max_scroll;
        }
        let scroll = self.run_config_popup_scroll;

        let active_path = self.active_buffer().path.clone();
        let can_run_current_file = RunConfiguration::for_file(&active_path, &self.workspace_root).is_some();
        let footer_h = if can_run_current_file { 132 } else { 104 };
        let popup_w = 378;
        let popup_h = (visible_rows as i32) * 40 + footer_h;
        let popup_x = (win_w - 490).clamp(180, win_w - popup_w - 8);
        let popup_y = 40;
        let popup_rect = Rect::new(popup_x, popup_y, popup_w, popup_h);
        self.run_config_popup_rect = popup_rect;
        self.active_overlay_rects.push(popup_rect);

        canvas.fill_rounded_shadow(popup_rect, 8);
        canvas.stroke_rounded_rect(popup_rect, 8, t.border_color, t.popup_bg);

        let header_right = if total > max_visible {
            format!("{} ({total})", self.tr("运行 / 调试配置", "Run / Debug Configurations"))
        } else {
            self.tr("运行 / 调试配置", "Run / Debug Configurations").to_string()
        };
        canvas.draw_text_styled(
            popup_x + 12,
            popup_y + 8,
            &header_right,
            t.text_primary,
            popup_w - 24,
            FontStyle::UiBold,
        );

        let list_top = popup_y + 32;
        let list_h = (visible_rows as i32) * 40;
        let has_scrollbar = total > max_visible;
        let row_w = if has_scrollbar { popup_w - 20 } else { popup_w - 12 };

        let mut y = list_top;
        for (idx, cfg) in cfgs.iter().enumerate().skip(scroll).take(max_visible) {
            let row = Rect::new(popup_x + 6, y, row_w, 36);
            let active = idx == self.active_run_config;
            let row_running = self.terminal.is_run_config_running(&cfg.identity_key());
            let hovered = row.contains(hx, hy);
            if active || hovered {
                canvas.fill_rounded_rect(row, 5, t.popup_hover_bg);
            }
            if active {
                canvas.draw_check_icon(row.x + 8, y + 11, 0xFFFFFF);
            } else {
                canvas.draw_run_config_icon(row.x + 6, y + 10, cfg);
            }
            if row_running {
                canvas.fill_circle(row.x + 21, y + 24, 3, t.run_green);
            }
            let is_custom = cfg.source == RunConfigSource::Custom;
            let right_reserve = if is_custom { 88 } else { 34 };
            canvas.draw_text_styled(
                row.x + 26,
                y + 2,
                &cfg.name,
                if active || hovered {
                    0xFFFFFF
                } else {
                    t.text_primary
                },
                row_w - 26 - right_reserve,
                FontStyle::UiRegular,
            );
            let cmd_preview = if cfg.args.is_empty() {
                cfg.command.clone()
            } else {
                format!("{} {}", cfg.command, cfg.args.join(" "))
            };
            canvas.draw_text_styled(
                row.x + 26,
                y + 19,
                &cmd_preview,
                if active || hovered {
                    0xE2E8F0
                } else {
                    t.text_muted
                },
                row_w - 26 - right_reserve,
                FontStyle::CodeMono,
            );

            // 每个脚本各自独立的 运行▷ / 暂停❚❚ 按钮
            let toggle_btn_x = if is_custom {
                row.x + row.w - 82
            } else {
                row.x + row.w - 26
            };
            let toggle_btn = Rect::new(toggle_btn_x, y + 7, 22, 22);
            if toggle_btn.contains(hx, hy) {
                canvas.fill_rounded_rect(toggle_btn, 4, t.panel_bg);
            }
            if row_running {
                canvas.draw_pause_icon(toggle_btn.x + 6, toggle_btn.y + 6, 10, t.breakpoint_red);
            } else {
                canvas.stroke_play_triangle(toggle_btn.x + 6, toggle_btn.y + 6, 10, t.run_green);
            }

            if is_custom {
                let badge_txt = if cfg.target_file.is_some() { "File" } else { "CLI" };
                let badge = Rect::new(row.x + row.w - 56, y + 9, 30, 18);
                canvas.fill_rounded_rect(
                    badge,
                    4,
                    if active || hovered { 0x355C9A } else { t.panel_bg },
                );
                canvas.draw_text_styled(
                    badge.x + 5,
                    badge.y + 2,
                    badge_txt,
                    if active || hovered { 0xFFFFFF } else { t.text_muted },
                    24,
                    FontStyle::UiSmall,
                );
                let del_btn = Rect::new(row.x + row.w - 24, y + 8, 20, 20);
                if del_btn.contains(hx, hy) {
                    canvas.fill_rounded_rect(del_btn, 4, t.breakpoint_red);
                }
                canvas.draw_text_styled(
                    del_btn.x + 6,
                    del_btn.y + 2,
                    "×",
                    if del_btn.contains(hx, hy) || active || hovered {
                        0xFFFFFF
                    } else {
                        t.text_muted
                    },
                    14,
                    FontStyle::UiRegular,
                );
                self.hit_regions.push((
                    Rect::new(row.x, row.y, toggle_btn_x - row.x, row.h),
                    HitAction::SelectRunConfig(idx),
                ));
                self.hit_regions
                    .push((toggle_btn, HitAction::ToggleRunConfigAt(idx)));
                self.hit_regions
                    .push((del_btn, HitAction::RemoveRunConfig(idx)));
            } else {
                self.hit_regions.push((
                    Rect::new(row.x, row.y, toggle_btn_x - row.x, row.h),
                    HitAction::SelectRunConfig(idx),
                ));
                self.hit_regions
                    .push((toggle_btn, HitAction::ToggleRunConfigAt(idx)));
            }
            y += 40;
        }

        if has_scrollbar {
            canvas.draw_vertical_scrollbar(
                Rect::new(popup_x + popup_w - 10, list_top + 2, 5, (list_h - 6).max(16)),
                scroll,
                max_visible,
                total,
                t.border_color,
            );
        }

        canvas.fill_rect(
            Rect::new(popup_x + 10, y + 2, popup_w - 20, 1),
            t.border_color,
        );
        y += 7;

        let active_running = self.active_run_config_is_running();
        let run_row = Rect::new(popup_x + 6, y, popup_w - 12, 26);
        let run_hovered = run_row.contains(hx, hy);
        if run_hovered {
            canvas.fill_rounded_rect(run_row, 5, t.popup_hover_bg);
        }
        if active_running {
            canvas.draw_pause_icon(run_row.x + 9, y + 7, 10, t.breakpoint_red);
        } else {
            canvas.stroke_play_triangle(run_row.x + 9, y + 7, 10, t.run_green);
        }
        canvas.draw_text_styled(
            run_row.x + 26,
            y + 4,
            if active_running {
                self.tr(
                    "暂停/停止当前配置 (Shift+F10)",
                    "Stop Active Configuration (Shift+F10)",
                )
            } else {
                self.tr(
                    "运行当前配置 (Shift+F10)",
                    "Run Active Configuration (Shift+F10)",
                )
            },
            if run_hovered {
                0xFFFFFF
            } else {
                t.text_primary
            },
            popup_w - 36,
            FontStyle::UiRegular,
        );
        self.hit_regions.push((run_row, HitAction::RunActiveConfig));
        y += 28;

        if can_run_current_file {
            let fname = active_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file");
            let file_label = match self.ui_lang {
                UiLanguage::ZhCn => format!("运行当前文件 '{fname}' (Ctrl+Shift+F10)"),
                UiLanguage::EnUs => format!("Run '{fname}' (Ctrl+Shift+F10)"),
            };
            let file_row = Rect::new(popup_x + 6, y, popup_w - 12, 26);
            let file_hovered = file_row.contains(hx, hy);
            if file_hovered {
                canvas.fill_rounded_rect(file_row, 5, t.popup_hover_bg);
            }
            canvas.draw_path_file_icon(file_row.x + 6, y + 5, &active_path);
            canvas.draw_text_styled(
                file_row.x + 26,
                y + 4,
                &file_label,
                if file_hovered {
                    0xFFFFFF
                } else {
                    t.text_primary
                },
                popup_w - 36,
                FontStyle::UiRegular,
            );
            self.hit_regions
                .push((file_row, HitAction::ExecuteIdeAction(IdeAction::RunCurrentFile)));
            y += 28;
        }

        let add_row = Rect::new(popup_x + 6, y, popup_w - 12, 26);
        let add_hovered = add_row.contains(hx, hy);
        if add_hovered {
            canvas.fill_rounded_rect(add_row, 5, t.popup_hover_bg);
        }
        canvas.draw_text_styled(
            add_row.x + 10,
            y + 3,
            "+",
            t.accent_blue,
            16,
            FontStyle::UiBold,
        );
        canvas.draw_text_styled(
            add_row.x + 26,
            y + 4,
            self.tr(
                "添加命令行配置...",
                "Add Run Command...",
            ),
            if add_hovered {
                0xFFFFFF
            } else {
                t.text_primary
            },
            popup_w - 36,
            FontStyle::UiRegular,
        );
        self.hit_regions
            .push((add_row, HitAction::OpenNewRunConfigModal));
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
            self.tr(
                "调试当前配置 (Shift+F9)",
                "Debug Active Configuration (Shift+F9)",
            ),
            if dbg_hovered {
                0xFFFFFF
            } else {
                t.text_primary
            },
            popup_w - 36,
            FontStyle::UiRegular,
        );
        self.hit_regions
            .push((dbg_row, HitAction::DebugActiveConfig));
    }

    fn render_mobile_env_popup(&mut self, canvas: &mut SurfaceCanvas, win_w: i32) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let devices = self.mobile_env.devices.clone();
        let tools = self.mobile_env.sdk_tools.clone();
        let has_flutter = matches!(
            self.mobile_env.project_kind,
            MobileProjectKind::Flutter | MobileProjectKind::FlutterAndAndroid
        );

        let dev_rows = devices.len().clamp(1, 5);
        let tool_rows = tools.len().clamp(1, 6);
        let missing_cnt = self.mobile_env.missing_required_tools().len();

        let popup_w = 436;
        let header_h = 38;
        let warn_h = if missing_cnt > 0 { 26 } else { 0 };
        let dev_section_h = 26 + (dev_rows as i32) * 34;
        let sdk_section_h = 28 + (tool_rows as i32) * 44;
        let quick_h = 38;
        let popup_h = (header_h + warn_h + dev_section_h + sdk_section_h + quick_h).min(640);
        let popup_x = (win_w - 640).clamp(140, win_w - popup_w - 8);
        let popup_y = 40;
        let popup_rect = Rect::new(popup_x, popup_y, popup_w, popup_h);
        self.mobile_popup_rect = popup_rect;
        self.active_overlay_rects.push(popup_rect);

        canvas.fill_rounded_shadow(popup_rect, 8);
        canvas.stroke_rounded_rect(popup_rect, 8, t.border_color, t.popup_bg);

        let kind_tag = match self.mobile_env.project_kind {
            MobileProjectKind::Flutter => "Flutter",
            MobileProjectKind::AndroidNative => "Android",
            MobileProjectKind::FlutterAndAndroid => "Flutter + Android",
            MobileProjectKind::None => "Mobile SDK",
        };
        let title_str = format!(
            "{} · {kind_tag}",
            self.tr("移动设备与 SDK 环境", "Mobile Devices & SDK")
        );
        canvas.draw_mobile_phone_icon(popup_x + 12, popup_y + 12, t.accent_blue);
        canvas.draw_text_styled(
            popup_x + 30,
            popup_y + 9,
            &title_str,
            t.text_primary,
            popup_w - 190,
            FontStyle::UiBold,
        );

        let path_rect = Rect::new(popup_x + popup_w - 150, popup_y + 7, 60, 24);
        let path_hot = path_rect.contains(hx, hy);
        canvas.fill_rounded_rect(
            path_rect,
            5,
            if path_hot { t.accent_blue } else { t.panel_bg },
        );
        canvas.draw_text_styled(
            path_rect.x + 10,
            path_rect.y + 4,
            self.tr("目录", "Path"),
            if path_hot { 0xFFFFFF } else { t.text_primary },
            path_rect.w - 12,
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((path_rect, HitAction::OpenMobileSdkSettings));

        let refresh_rect = Rect::new(popup_x + popup_w - 84, popup_y + 7, 74, 24);
        let refresh_hot = refresh_rect.contains(hx, hy);
        canvas.fill_rounded_rect(
            refresh_rect,
            5,
            if refresh_hot {
                t.accent_blue
            } else {
                t.panel_bg
            },
        );
        canvas.draw_text_styled(
            refresh_rect.x + 8,
            refresh_rect.y + 4,
            if self.mobile_env.probing {
                self.tr("检测中...", "Probing...")
            } else {
                self.tr("↻ 刷新", "↻ Refresh")
            },
            if refresh_hot { 0xFFFFFF } else { t.text_primary },
            refresh_rect.w - 12,
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((refresh_rect, HitAction::RefreshMobileEnv));

        let mut y = popup_y + 36;
        if missing_cnt > 0 {
            let warn_box = Rect::new(popup_x + 10, y, popup_w - 20, 22);
            canvas.fill_rounded_rect(warn_box, 4, blend_rgb(t.popup_bg, t.breakpoint_red, 0.22));
            let warn_msg = match self.ui_lang {
                UiLanguage::ZhCn => format!(
                    "⚠ {missing_cnt} 个移动端 SDK 未安装。可点「目录」指定安装路径，或下方安装"
                ),
                UiLanguage::EnUs => format!(
                    "⚠ {missing_cnt} mobile SDK(s) not installed. Set Path or install below"
                ),
            };
            canvas.draw_text_styled(
                warn_box.x + 8,
                warn_box.y + 4,
                &warn_msg,
                0xF78166,
                warn_box.w - 16,
                FontStyle::UiSmall,
            );
            y += 26;
        }

        // Section 1: Connected phones & AVD emulators
        canvas.draw_text_styled(
            popup_x + 12,
            y,
            self.tr("可用真机与模拟器 (ADB / Flutter / AVD)", "Devices & Emulators (ADB / Flutter / AVD)"),
            t.text_muted,
            popup_w - 24,
            FontStyle::UiSmall,
        );
        y += 20;

        if devices.is_empty() {
            let empty_msg = if self.mobile_env.probing {
                self.tr("正在执行 adb devices / flutter devices 检测...", "Running adb devices / flutter devices...")
            } else {
                self.tr(
                    "未发现设备：请用 USB 连接手机并开启「USB 调试」，或先安装 ADB/AVD",
                    "No device found: connect phone via USB Debugging or launch an AVD",
                )
            };
            canvas.draw_text_styled(
                popup_x + 14,
                y + 6,
                empty_msg,
                t.text_secondary,
                popup_w - 28,
                FontStyle::UiSmall,
            );
            y += 32;
        } else {
            for dev in devices.iter().take(5) {
                let row = Rect::new(popup_x + 8, y, popup_w - 16, 30);
                let is_sel = self.selected_mobile_device_id.as_deref() == Some(dev.id.as_str());
                let row_hot = row.contains(hx, hy);
                if is_sel || row_hot {
                    canvas.fill_rounded_rect(
                        row,
                        5,
                        if is_sel {
                            blend_rgb(t.popup_bg, t.accent_blue, 0.25)
                        } else {
                            t.popup_hover_bg
                        },
                    );
                }
                let state_color = match dev.state {
                    MobileDeviceState::Online => t.run_green,
                    MobileDeviceState::Unauthorized => 0xE5C07B,
                    MobileDeviceState::Offline => t.breakpoint_red,
                    MobileDeviceState::EmulatorAvd => t.accent_blue,
                };
                canvas.fill_circle(row.x + 10, row.y + 15, 4, state_color);
                let title_line = if is_sel {
                    format!("✓ {}  ({})", dev.name, dev.detail)
                } else {
                    format!("{}  ({})", dev.name, dev.detail)
                };
                canvas.draw_text_styled(
                    row.x + 22,
                    row.y + 6,
                    &title_line,
                    if row_hot && !is_sel { 0xFFFFFF } else { t.text_primary },
                    row.w - 92,
                    FontStyle::UiSmall,
                );
                if dev.state == MobileDeviceState::EmulatorAvd {
                    let avd_name = dev.id.strip_prefix("avd:").unwrap_or(&dev.name).to_string();
                    let btn = Rect::new(row.x + row.w - 64, row.y + 4, 58, 22);
                    let btn_hot = btn.contains(hx, hy);
                    canvas.fill_rounded_rect(
                        btn,
                        4,
                        if btn_hot { t.accent_blue } else { t.panel_bg },
                    );
                    canvas.draw_text_styled(
                        btn.x + 8,
                        btn.y + 3,
                        self.tr("启动", "Launch"),
                        if btn_hot { 0xFFFFFF } else { t.text_primary },
                        btn.w - 12,
                        FontStyle::UiSmall,
                    );
                    self.hit_regions
                        .push((btn, HitAction::LaunchMobileEmulator(avd_name)));
                } else if dev.state == MobileDeviceState::Unauthorized {
                    canvas.draw_text_styled(
                        row.x + row.w - 76,
                        row.y + 6,
                        self.tr("需手机授权", "Authorize"),
                        0xE5C07B,
                        72,
                        FontStyle::UiSmall,
                    );
                } else if dev.state == MobileDeviceState::Online {
                    self.hit_regions
                        .push((row, HitAction::SelectMobileDevice(dev.id.clone())));
                }
                y += 34;
            }
        }

        canvas.fill_rect(
            Rect::new(popup_x + 10, y + 2, popup_w - 20, 1),
            t.border_color,
        );
        y += 8;

        // Section 2: Local SDK & Toolchain Status
        canvas.draw_text_styled(
            popup_x + 12,
            y,
            self.tr("本机 SDK / 编译工具链检测", "Local SDK & Toolchain Check"),
            t.text_muted,
            popup_w - 24,
            FontStyle::UiSmall,
        );
        y += 20;

        for tool in tools.iter().take(6) {
            let row = Rect::new(popup_x + 8, y, popup_w - 16, 40);
            if row.contains(hx, hy) {
                canvas.fill_rounded_rect(row, 5, blend_rgb(t.popup_bg, t.panel_bg, 0.65));
            }
            let dot = if tool.installed {
                t.run_green
            } else if tool.required {
                t.breakpoint_red
            } else {
                t.text_muted
            };
            canvas.fill_circle(row.x + 10, row.y + 12, 4, dot);

            let right_reserve = if !tool.installed {
                if tool.install_cmd.is_some() { 142 } else { 72 }
            } else {
                12
            };
            let status_head = if tool.installed {
                let ver = if tool.version_or_detail.trim().is_empty() {
                    self.tr("已安装", "Installed")
                } else {
                    tool.version_or_detail.as_str()
                };
                format!("{} · {ver}", tool.label)
            } else if tool.required {
                format!(
                    "{} · {}",
                    tool.label,
                    self.tr("SDK 未安装", "SDK not installed")
                )
            } else {
                format!(
                    "{} · {}",
                    tool.label,
                    self.tr("SDK 未安装", "SDK not installed")
                )
            };
            canvas.draw_text_styled(
                row.x + 20,
                row.y + 3,
                &status_head,
                if tool.installed {
                    t.text_primary
                } else if tool.required {
                    0xF78166
                } else {
                    t.text_secondary
                },
                row.w - right_reserve - 20,
                FontStyle::UiSmall,
            );
            let sub_info_owned: String;
            let sub_info = if tool.installed {
                sub_info_owned = tool
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                sub_info_owned.as_str()
            } else {
                tool.install_hint.as_str()
            };
            canvas.draw_text_styled(
                row.x + 20,
                row.y + 20,
                sub_info,
                t.text_muted,
                row.w - right_reserve - 20,
                FontStyle::UiSmall,
            );

            if !tool.installed {
                let mut bx = row.x + row.w - 64;
                let dl_btn = Rect::new(bx, row.y + 9, 58, 22);
                let dl_hot = dl_btn.contains(hx, hy);
                canvas.fill_rounded_rect(
                    dl_btn,
                    4,
                    if dl_hot { t.accent_blue } else { t.panel_bg },
                );
                canvas.draw_text_styled(
                    dl_btn.x + 10,
                    dl_btn.y + 3,
                    self.tr("下载", "Get"),
                    if dl_hot { 0xFFFFFF } else { t.text_primary },
                    dl_btn.w - 12,
                    FontStyle::UiSmall,
                );
                self.hit_regions.push((
                    dl_btn,
                    HitAction::OpenMobileDownloadUrl(tool.download_url.clone()),
                ));

                if let Some(cmd) = &tool.install_cmd {
                    bx -= 68;
                    let inst_btn = Rect::new(bx, row.y + 9, 64, 22);
                    let inst_hot = inst_btn.contains(hx, hy);
                    canvas.fill_rounded_rect(
                        inst_btn,
                        4,
                        if inst_hot {
                            t.run_green
                        } else {
                            blend_rgb(t.panel_bg, t.run_green, 0.25)
                        },
                    );
                    canvas.draw_text_styled(
                        inst_btn.x + 6,
                        inst_btn.y + 3,
                        self.tr("一键安装", "Install"),
                        if inst_hot { 0x0B1510 } else { t.text_primary },
                        inst_btn.w - 10,
                        FontStyle::UiSmall,
                    );
                    self.hit_regions.push((
                        inst_btn,
                        HitAction::InstallMobileSdkCmd(tool.label.clone(), cmd.clone()),
                    ));
                }
            }
            y += 44;
        }

        canvas.fill_rect(
            Rect::new(popup_x + 10, y + 2, popup_w - 20, 1),
            t.border_color,
        );
        y += 8;

        // Section 3: Quick ADB / Flutter diagnostics
        let mut qx = popup_x + 10;
        let logcat_cmd = if let Some(dev_id) = self.selected_mobile_device_id.as_deref() {
            if !dev_id.starts_with("avd:") {
                format!("adb -s {dev_id} logcat -v time")
            } else {
                "adb logcat -v time".to_string()
            }
        } else {
            "adb logcat -v time".to_string()
        };
        let quick_actions: Vec<(&str, String)> = {
            let mut v = vec![(self.tr("查看 ADB Logcat", "ADB Logcat"), logcat_cmd)];
            if has_flutter {
                v.push(("flutter doctor", "flutter doctor -v".to_string()));
            }
            v.push((
                self.tr("重启 ADB", "Restart ADB"),
                "adb kill-server && adb start-server && adb devices -l".to_string(),
            ));
            v
        };
        for (label, cmd) in quick_actions {
            let (tw, _) = canvas.measure_text(label, FontStyle::UiSmall);
            let bw = (tw + 16).min(148);
            if qx + bw > popup_x + popup_w - 8 {
                break;
            }
            let btn = Rect::new(qx, y, bw, 24);
            let bhot = btn.contains(hx, hy);
            canvas.fill_rounded_rect(btn, 5, if bhot { t.popup_hover_bg } else { t.panel_bg });
            canvas.draw_text_styled(
                btn.x + 8,
                btn.y + 4,
                label,
                if bhot { 0xFFFFFF } else { t.text_secondary },
                btn.w - 12,
                FontStyle::UiSmall,
            );
            self.hit_regions
                .push((btn, HitAction::RunMobileCommand(cmd)));
            qx += bw + 6;
        }
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
                self.tr(
                    "正在后台索引工作区文件...",
                    "Indexing workspace files in background...",
                ),
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
            let hovered =
                row.contains(hx, hy) && !matches!(item, SearchEverywhereItem::AgentCommand { .. });
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
                    let row_max_w = (mw - 42).max(60);
                    let prefix = format!("[{file_tag}]  ");
                    let (pw, _) = canvas.measure_text(&prefix, FontStyle::UiRegular);
                    let name_max_w = ((row_max_w - pw) / 2).max(110);
                    let shown_name =
                        canvas.ellipsize_filename(&name, name_max_w, FontStyle::UiRegular);
                    let display_row = format!("{prefix}{shown_name}   ({relative_path})");
                    canvas.draw_text_ellipsis(
                        mx + 18,
                        ry + 3,
                        &display_row,
                        if hovered { 0xFFFFFF } else { t.text_primary },
                        row_max_w,
                        FontStyle::UiRegular,
                    );
                    self.hit_regions.push((row, HitAction::OpenFile(full_path)));
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
                self.tr(
                    "正在后台执行 Ripgrep 全文检索...",
                    "Running Ripgrep search in background...",
                ),
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
            self.hit_regions.push((row, HitAction::OpenFile(hit.path)));
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
            if cancel_hov {
                t.panel_header_bg
            } else {
                t.window_bg
            },
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
            if this_hov {
                t.accent_blue
            } else {
                t.border_color
            },
            if this_hov {
                t.popup_hover_bg
            } else {
                t.editor_bg
            },
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

    fn render_edit_message_rewind_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let Some(pending) = self.pending_edit_rewind.clone() else {
            self.focus = FocusTarget::AgentChatInput;
            return;
        };
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let has_files = !pending.affected_files.is_empty();
        let mw = 540.min(w - 40);
        let mh = if has_files { 198 } else { 158 };
        let mx = (w - mw) / 2;
        let my = (h - mh) / 3;
        let modal = Rect::new(mx, my, mw, mh);
        self.active_overlay_rects.push(modal);

        canvas.fill_rounded_shadow(modal, 8);
        canvas.stroke_rounded_rect(modal, 8, t.accent_blue, t.popup_bg);

        let header = Rect::new(mx + 2, my + 2, mw - 4, 34);
        canvas.fill_rounded_rect(header, 6, t.panel_header_bg);
        canvas.draw_text_styled(
            mx + 16,
            my + 9,
            self.tr(
                "编辑已发送消息并同步 CLI 客户端",
                "Edit Sent Message & Sync CLI",
            ),
            t.text_primary,
            mw - 32,
            FontStyle::UiBold,
        );

        let cli_name = self.agent_hub.active_provider.compact_name();
        let summary_line = match self.ui_lang {
            UiLanguage::ZhCn => format!(
                "将替换此消息之后的 {} 条对话，并与 {} 客户端会话同步状态。",
                pending.dropped_msgs, cli_name
            ),
            UiLanguage::EnUs => format!(
                "This replaces {} subsequent message(s) and syncs the {} CLI session.",
                pending.dropped_msgs, cli_name
            ),
        };
        canvas.draw_text_styled(
            mx + 16,
            my + 48,
            &summary_line,
            t.text_primary,
            mw - 32,
            FontStyle::UiRegular,
        );

        let preview_one_line: String = pending
            .new_prompt
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(56)
            .collect();
        canvas.draw_text_styled(
            mx + 16,
            my + 72,
            &format!("› {preview_one_line}"),
            t.text_muted,
            mw - 32,
            FontStyle::ChatMeta,
        );

        if has_files {
            let names: Vec<String> = pending
                .affected_files
                .iter()
                .take(3)
                .map(|p| {
                    p.strip_prefix(&self.workspace_root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .replace('\\', "/")
                })
                .collect();
            let extra = pending.affected_files.len().saturating_sub(names.len());
            let files_str = if extra > 0 {
                format!("{} (+{})", names.join(", "), extra)
            } else {
                names.join(", ")
            };
            canvas.draw_text_styled(
                mx + 16,
                my + 98,
                &format!(
                    "{}: {files_str}",
                    self.tr("涉及工作区文件修改", "Modified workspace files")
                ),
                t.accent_blue,
                mw - 32,
                FontStyle::ChatMeta,
            );
            canvas.draw_text_styled(
                mx + 16,
                my + 118,
                self.tr(
                    "请选择回退这些文件修改后再发送，或保留当前磁盘修改继续发送：",
                    "Choose whether to revert file changes or keep them before resending:",
                ),
                t.text_secondary,
                mw - 32,
                FontStyle::UiSmall,
            );
        }

        let btn_y = my + mh - 42;
        let cancel_btn = Rect::new(mx + mw - 102, btn_y, 86, 28);
        let cancel_hov = cancel_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            cancel_btn,
            6,
            t.border_color,
            if cancel_hov {
                t.panel_header_bg
            } else {
                t.window_bg
            },
        );
        canvas.draw_text_centered(
            cancel_btn,
            self.tr("取消 (Esc)", "Cancel"),
            t.text_secondary,
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((cancel_btn, HitAction::CancelEditMessageRewindModal));

        if has_files {
            let keep_btn = Rect::new(cancel_btn.x - 134, btn_y, 126, 28);
            let keep_hov = keep_btn.contains(hx, hy);
            canvas.stroke_rounded_rect(
                keep_btn,
                6,
                if keep_hov {
                    t.accent_blue
                } else {
                    t.border_color
                },
                if keep_hov {
                    t.popup_hover_bg
                } else {
                    t.editor_bg
                },
            );
            canvas.draw_text_centered(
                keep_btn,
                self.tr("保留文件并重发", "Keep & Resend"),
                if keep_hov { 0xFFFFFF } else { t.text_primary },
                FontStyle::UiSmall,
            );
            self.hit_regions.push((
                keep_btn,
                HitAction::ConfirmEditMessageRewind {
                    revert_files: false,
                },
            ));

            let rev_btn = Rect::new(keep_btn.x - 152, btn_y, 144, 28);
            canvas.fill_rounded_rect(rev_btn, 6, t.accent_blue);
            canvas.draw_text_centered(
                rev_btn,
                self.tr("回退更改并重发 (Enter)", "Revert & Resend"),
                0xFFFFFF,
                FontStyle::UiBold,
            );
            self.hit_regions.push((
                rev_btn,
                HitAction::ConfirmEditMessageRewind { revert_files: true },
            ));
        } else {
            let ok_btn = Rect::new(cancel_btn.x - 152, btn_y, 144, 28);
            canvas.fill_rounded_rect(ok_btn, 6, t.accent_blue);
            canvas.draw_text_centered(
                ok_btn,
                self.tr("同步并重发 (Enter)", "Sync & Resend"),
                0xFFFFFF,
                FontStyle::UiBold,
            );
            self.hit_regions.push((
                ok_btn,
                HitAction::ConfirmEditMessageRewind {
                    revert_files: false,
                },
            ));
        }
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
                self.tr(
                    "按 Enter 确认创建，按 Esc 关闭弹窗",
                    "Press Enter to create, Esc to cancel",
                ),
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
                self.tr(
                    "按 Enter 确认创建，按 Esc 关闭弹窗",
                    "Press Enter to create, Esc to cancel",
                ),
            ),
            FocusTarget::NewGitBranchModal => (
                self.tr(
                    "创建并检出新 Git 分支 (Enter 创建)",
                    "Create & Checkout New Git Branch (Enter to create)",
                )
                .to_string(),
                self.tr(
                    "按 Enter 确认创建，按 Esc 关闭弹窗",
                    "Press Enter to create, Esc to cancel",
                ),
            ),
            FocusTarget::NewRunConfigModal => (
                self.tr(
                    "添加并运行命令行配置 (例如 npm run dev / docker compose up / ./gradlew bootRun)",
                    "Add & Run Command Configuration (e.g. npm run dev / docker compose up)",
                )
                .to_string(),
                self.tr(
                    "输入命令后按 Enter 加入运行列表并执行，Esc 取消",
                    "Enter command then press Enter to add & run, Esc to cancel",
                ),
            ),
            FocusTarget::NewGitTagModal => (
                self.tr(
                    "创建新 Git Tag 标签 (Enter 创建)",
                    "Create New Git Tag at HEAD (Enter to create)",
                )
                .to_string(),
                self.tr(
                    "按 Enter 确认创建标签，按 Esc 取消",
                    "Press Enter to create tag, Esc to cancel",
                ),
            ),
            FocusTarget::NewGitStashModal => (
                self.tr(
                    "暂存当前工作区修改 Stash Changes",
                    "Stash Uncommitted Changes (includes untracked)",
                )
                .to_string(),
                self.tr(
                    "输入 Stash 说明后按 Enter 暂存，按 Esc 取消",
                    "Enter stash message and press Enter, Esc to cancel",
                ),
            ),
            FocusTarget::GitAddRemoteModal => (
                self.tr(
                    "添加 Git 远程仓库 (格式: <名称> <URL>)",
                    "Add Git Remote (format: <name> <url>)",
                )
                .to_string(),
                self.tr(
                    "例如 origin https://... 后按 Enter 确认，Esc 取消",
                    "Example: origin https://... then Enter, Esc to cancel",
                ),
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
                self.tr(
                    "按 Enter 确认重命名，按 Esc 关闭弹窗",
                    "Press Enter to rename, Esc to cancel",
                ),
            ),
            _ => (
                String::new(),
                self.tr(
                    "按 Enter 确认，按 Esc 关闭弹窗",
                    "Press Enter to confirm, Esc to cancel",
                ),
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

    /// 渲染 IntelliJ IDEA 风格 Git 模态操作弹窗（合并 / 变基 / 恢复 Stash / Reset HEAD / 回滚 / 远程仓库）
    fn render_git_operation_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let mw = 540.min((w - 36).max(320));
        let mh = 360.min((h - 48).max(240));
        let mx = (w - mw) / 2;
        let my = (h - mh) / 3;
        let modal = Rect::new(mx, my, mw, mh);
        self.active_overlay_rects.push(modal);

        canvas.fill_rounded_shadow(modal, 10);
        canvas.stroke_rounded_rect(modal, 10, t.accent_blue, t.popup_bg);

        let header = Rect::new(mx + 2, my + 2, mw - 4, 38);
        canvas.fill_rounded_rect(header, 8, t.panel_header_bg);

        let title = match self.focus {
            FocusTarget::GitMergeModal => self.tr("Git 合并分支 (Merge)", "Git Merge Branch"),
            FocusTarget::GitRebaseModal => self.tr("Git 变基 (Rebase)", "Git Rebase Branch"),
            FocusTarget::GitUnstashModal => {
                self.tr("恢复工作区暂存 (Unstash Changes)", "Unstash Changes (Stash List)")
            }
            FocusTarget::GitResetModal => self.tr("重置当前分支 (Reset HEAD)", "Reset HEAD"),
            FocusTarget::GitRollbackModal => {
                self.tr("回滚未提交变更 (Rollback Changes)", "Rollback Uncommitted Changes")
            }
            FocusTarget::GitRemotesModal => {
                self.tr("管理远程仓库 (Git Remotes)", "Manage Git Remotes")
            }
            _ => "",
        };
        canvas.draw_text_ellipsis(
            mx + 16,
            my + 11,
            title,
            t.text_primary,
            mw - 104,
            FontStyle::UiBold,
        );

        let close_btn = Rect::new(mx + mw - 76, my + 8, 64, 24);
        let chov = close_btn.contains(hx, hy);
        canvas.stroke_rounded_rect(
            close_btn,
            5,
            if chov { t.accent_blue } else { t.border_color },
            if chov { t.popup_hover_bg } else { t.window_bg },
        );
        canvas.draw_text_centered(
            close_btn,
            self.tr("关闭 (Esc)", "Close"),
            if chov { 0xFFFFFF } else { t.text_secondary },
            FontStyle::UiSmall,
        );
        self.hit_regions
            .push((close_btn, HitAction::CloseModalToEditor));

        let mut y = my + 48;
        let content_bottom = my + mh - 14;
        match self.focus {
            FocusTarget::GitMergeModal | FocusTarget::GitRebaseModal => {
                let is_rebase = self.focus == FocusTarget::GitRebaseModal;
                if is_rebase && self.git_engine.in_rebase {
                    let cont_btn = Rect::new(mx + 16, y, 130, 26);
                    let abort_btn = Rect::new(mx + 154, y, 130, 26);
                    for (btn, lbl, act, col) in [
                        (
                            cont_btn,
                            self.tr("继续 Rebase", "Continue Rebase"),
                            HitAction::GitRebaseContinue,
                            t.run_green,
                        ),
                        (
                            abort_btn,
                            self.tr("中止 Rebase", "Abort Rebase"),
                            HitAction::GitRebaseAbort,
                            t.breakpoint_red,
                        ),
                    ] {
                        let hov = btn.contains(hx, hy);
                        canvas.stroke_rounded_rect(btn, 5, col, t.window_bg);
                        canvas.draw_text_centered(btn, lbl, if hov { 0xFFFFFF } else { col }, FontStyle::UiSmall);
                        self.hit_regions.push((btn, act));
                    }
                    y += 34;
                } else if !is_rebase && self.git_engine.in_merge {
                    let abort_btn = Rect::new(mx + 16, y, 160, 26);
                    canvas.stroke_rounded_rect(abort_btn, 5, t.breakpoint_red, t.window_bg);
                    canvas.draw_text_centered(
                        abort_btn,
                        self.tr("中止当前 Merge", "Abort Active Merge"),
                        t.breakpoint_red,
                        FontStyle::UiSmall,
                    );
                    self.hit_regions.push((abort_btn, HitAction::GitMergeAbort));
                    y += 34;
                }

                let sub = if is_rebase {
                    format!(
                        "{}: {}",
                        self.tr("选择要变基到的目标分支，当前分支", "Select target branch to rebase"),
                        self.git_engine.current_branch
                    )
                } else {
                    format!(
                        "{}: {}",
                        self.tr("选择要合并入当前分支的来源分支", "Select branch to merge into"),
                        self.git_engine.current_branch
                    )
                };
                canvas.draw_text_ellipsis(
                    mx + 16,
                    y,
                    &sub,
                    t.text_muted,
                    mw - 32,
                    FontStyle::UiSmall,
                );
                y += 22;

                let candidates: Vec<String> = self
                    .git_engine
                    .local_branches
                    .iter()
                    .filter(|b| *b != &self.git_engine.current_branch)
                    .chain(self.git_engine.remote_branches.iter())
                    .cloned()
                    .collect();
                if candidates.is_empty() {
                    canvas.draw_text_centered(
                        Rect::new(mx + 16, y + 20, mw - 32, 60),
                        self.tr("当前没有其他可操作的本地或远程分支", "No other local or remote branches available"),
                        t.text_muted,
                        FontStyle::UiRegular,
                    );
                } else {
                    for br in candidates {
                        if y + 28 > content_bottom {
                            break;
                        }
                        let row = Rect::new(mx + 16, y, mw - 32, 26);
                        let hov = row.contains(hx, hy);
                        canvas.stroke_rounded_rect(
                            row,
                            5,
                            if hov { t.accent_blue } else { t.border_color },
                            if hov { t.popup_hover_bg } else { t.window_bg },
                        );
                        canvas.draw_git_branch_icon(row.x + 8, row.y + 6, t.accent_blue);
                        canvas.draw_text_ellipsis(
                            row.x + 28,
                            row.y + 4,
                            &br,
                            if hov { 0xFFFFFF } else { t.text_primary },
                            row.w - 110,
                            FontStyle::UiRegular,
                        );
                        let act_lbl = if is_rebase {
                            self.tr("变基至此", "Rebase")
                        } else {
                            self.tr("合并入当前", "Merge")
                        };
                        canvas.draw_text_styled(
                            row.x + row.w - 78,
                            row.y + 5,
                            act_lbl,
                            if hov { 0xFFFFFF } else { t.accent_blue },
                            72,
                            FontStyle::UiSmall,
                        );
                        let hit = if is_rebase {
                            HitAction::GitRebaseBranch(br)
                        } else {
                            HitAction::GitMergeBranch(br)
                        };
                        self.hit_regions.push((row, hit));
                        y += 30;
                    }
                }
            }
            FocusTarget::GitUnstashModal => {
                let stashes = self.git_engine.stashes.clone();
                if stashes.is_empty() {
                    canvas.draw_text_centered(
                        Rect::new(mx + 16, y + 40, mw - 32, 80),
                        self.tr("当前没有已保存的 Git Stash 暂存记录", "No Git stashes saved in this repository"),
                        t.text_muted,
                        FontStyle::UiRegular,
                    );
                } else {
                    for st in stashes {
                        if y + 32 > content_bottom {
                            break;
                        }
                        let row = Rect::new(mx + 16, y, mw - 32, 28);
                        canvas.stroke_rounded_rect(row, 5, t.border_color, t.window_bg);
                        let label = format!("{}  ·  {}", st.index_ref, st.message);
                        canvas.draw_text_ellipsis(
                            row.x + 10,
                            row.y + 5,
                            &label,
                            t.text_primary,
                            (row.w - 186).max(40),
                            FontStyle::UiSmall,
                        );
                        let drop_b = Rect::new(row.x + row.w - 54, row.y + 3, 48, 22);
                        let apply_b = Rect::new(drop_b.x - 56, row.y + 3, 52, 22);
                        let pop_b = Rect::new(apply_b.x - 54, row.y + 3, 50, 22);
                        for (btn, lbl, act, col) in [
                            (pop_b, "Pop", HitAction::GitStashPop(st.index_ref.clone()), t.run_green),
                            (
                                apply_b,
                                "Apply",
                                HitAction::GitStashApply(st.index_ref.clone()),
                                t.accent_blue,
                            ),
                            (
                                drop_b,
                                "Drop",
                                HitAction::GitStashDrop(st.index_ref.clone()),
                                t.breakpoint_red,
                            ),
                        ] {
                            let bhov = btn.contains(hx, hy);
                            canvas.stroke_rounded_rect(btn, 4, col, if bhov { col } else { t.panel_bg });
                            canvas.draw_text_centered(btn, lbl, if bhov { 0xFFFFFF } else { col }, FontStyle::UiSmall);
                            self.hit_regions.push((btn, act));
                        }
                        y += 32;
                    }
                }
            }
            FocusTarget::GitResetModal => {
                canvas.draw_text_styled(
                    mx + 16,
                    y,
                    self.tr("1. 选择 Reset 模式:", "1. Select Reset Mode:"),
                    t.text_secondary,
                    mw - 32,
                    FontStyle::UiSmall,
                );
                y += 22;
                let modes = [
                    (ide_git::GitResetMode::Soft, "Soft (--soft)", self.tr("保留更改在暂存区", "Keep staged")),
                    (ide_git::GitResetMode::Mixed, "Mixed (--mixed)", self.tr("保留在工作区", "Keep in worktree")),
                    (ide_git::GitResetMode::Hard, "Hard (--hard)", self.tr("丢弃全部本地修改", "Discard all changes")),
                ];
                let bw = ((mw - 48) / 3).max(90);
                for (i, (mode, title_m, _desc)) in modes.iter().enumerate() {
                    let btn = Rect::new(mx + 16 + (i as i32) * (bw + 8), y, bw, 28);
                    let active = self.git_reset_mode == *mode;
                    canvas.stroke_rounded_rect(
                        btn,
                        5,
                        if active { t.accent_blue } else { t.border_color },
                        if active {
                            blend_rgb(t.window_bg, t.accent_blue, 0.20)
                        } else {
                            t.window_bg
                        },
                    );
                    canvas.draw_text_centered(
                        btn,
                        title_m,
                        if active { t.accent_blue } else { t.text_primary },
                        FontStyle::UiSmall,
                    );
                    self.hit_regions.push((btn, HitAction::SelectGitResetMode(*mode)));
                }
                y += 40;
                canvas.draw_text_styled(
                    mx + 16,
                    y,
                    self.tr("2. 选择目标提交 (点击即执行 Reset):", "2. Select Target Revision:"),
                    t.text_secondary,
                    mw - 32,
                    FontStyle::UiSmall,
                );
                y += 22;
                let mut targets = vec![
                    ("HEAD".to_string(), self.tr("当前提交 HEAD", "Current HEAD").to_string()),
                    ("HEAD~1".to_string(), self.tr("上一个提交 HEAD~1", "Parent commit HEAD~1").to_string()),
                ];
                for c in self.git_engine.recent_commits.iter().take(5) {
                    targets.push((c.hash.clone(), format!("{}  {}", c.hash, c.subject)));
                }
                for (tgt, label) in targets {
                    if y + 28 > content_bottom {
                        break;
                    }
                    let row = Rect::new(mx + 16, y, mw - 32, 26);
                    let hov = row.contains(hx, hy);
                    canvas.stroke_rounded_rect(
                        row,
                        5,
                        if hov { t.accent_blue } else { t.border_color },
                        if hov { t.popup_hover_bg } else { t.window_bg },
                    );
                    canvas.draw_text_ellipsis(
                        row.x + 10,
                        row.y + 4,
                        &label,
                        if hov { 0xFFFFFF } else { t.text_primary },
                        row.w - 20,
                        FontStyle::UiSmall,
                    );
                    self.hit_regions.push((
                        row,
                        HitAction::GitResetTo {
                            target: tgt,
                            mode: self.git_reset_mode,
                        },
                    ));
                    y += 30;
                }
            }
            FocusTarget::GitRollbackModal => {
                let files = self.git_engine.changed_files.clone();
                if files.is_empty() {
                    canvas.draw_text_centered(
                        Rect::new(mx + 16, y + 40, mw - 32, 80),
                        self.tr("当前工作区干净，没有可回滚的文件", "Working tree is clean (nothing to rollback)"),
                        t.text_muted,
                        FontStyle::UiRegular,
                    );
                } else {
                    let rb_chk = Rect::new(mx + 16, y, 180, 26);
                    let rb_all = Rect::new(mx + 206, y, 180, 26);
                    canvas.stroke_rounded_rect(rb_chk, 5, t.accent_blue, t.window_bg);
                    canvas.draw_text_centered(
                        rb_chk,
                        self.tr("回滚已勾选变更", "Rollback Checked"),
                        t.accent_blue,
                        FontStyle::UiSmall,
                    );
                    self.hit_regions
                        .push((rb_chk, HitAction::RollbackCheckedGitChanges));

                    canvas.stroke_rounded_rect(rb_all, 5, t.breakpoint_red, t.window_bg);
                    canvas.draw_text_centered(
                        rb_all,
                        self.tr("放弃全部本地修改", "Discard All Changes"),
                        t.breakpoint_red,
                        FontStyle::UiSmall,
                    );
                    self.hit_regions
                        .push((rb_all, HitAction::GitConfirmRollbackAll));
                    y += 34;

                    for ch in files {
                        if y + 24 > content_bottom {
                            break;
                        }
                        let row = Rect::new(mx + 16, y, mw - 32, 22);
                        canvas.draw_path_file_icon(row.x + 4, row.y + 3, &ch.path);
                        let rel = ch
                            .path
                            .strip_prefix(&self.workspace_root)
                            .unwrap_or(&ch.path)
                            .to_string_lossy()
                            .replace('\\', "/");
                        canvas.draw_text_ellipsis(
                            row.x + 24,
                            row.y + 3,
                            &rel,
                            t.text_primary,
                            row.w - 92,
                            FontStyle::UiSmall,
                        );
                        let one_rb = Rect::new(row.x + row.w - 60, row.y + 1, 56, 20);
                        canvas.stroke_rounded_rect(one_rb, 4, t.border_color, t.window_bg);
                        canvas.draw_text_centered(
                            one_rb,
                            self.tr("回滚", "Revert"),
                            t.breakpoint_red,
                            FontStyle::UiSmall,
                        );
                        self.hit_regions
                            .push((one_rb, HitAction::RollbackGitFile(ch.path)));
                        y += 24;
                    }
                }
            }
            FocusTarget::GitRemotesModal => {
                let add_btn = Rect::new(mx + 16, y, 160, 26);
                let ahov = add_btn.contains(hx, hy);
                canvas.stroke_rounded_rect(
                    add_btn,
                    5,
                    t.accent_blue,
                    if ahov {
                        t.accent_blue
                    } else {
                        blend_rgb(t.window_bg, t.accent_blue, 0.16)
                    },
                );
                canvas.draw_text_centered(
                    add_btn,
                    self.tr("+ 添加远程仓库...", "+ Add Remote..."),
                    if ahov { 0xFFFFFF } else { t.accent_blue },
                    FontStyle::UiSmall,
                );
                self.hit_regions.push((add_btn, HitAction::OpenAddRemoteModal));
                y += 34;

                let remotes = self.git_engine.remotes.clone();
                if remotes.is_empty() {
                    canvas.draw_text_centered(
                        Rect::new(mx + 16, y + 30, mw - 32, 60),
                        self.tr("当前仓库尚未配置任何远程仓库", "No Git remotes configured"),
                        t.text_muted,
                        FontStyle::UiRegular,
                    );
                } else {
                    for rm in remotes {
                        if y + 30 > content_bottom {
                            break;
                        }
                        let row = Rect::new(mx + 16, y, mw - 32, 26);
                        canvas.stroke_rounded_rect(row, 5, t.border_color, t.window_bg);
                        let txt = format!("{}   {}", rm.name, rm.url);
                        canvas.draw_text_ellipsis(
                            row.x + 10,
                            row.y + 4,
                            &txt,
                            t.text_primary,
                            row.w - 76,
                            FontStyle::UiSmall,
                        );
                        let del_b = Rect::new(row.x + row.w - 60, row.y + 2, 54, 22);
                        canvas.stroke_rounded_rect(del_b, 4, t.breakpoint_red, t.panel_bg);
                        canvas.draw_text_centered(
                            del_b,
                            self.tr("移除", "Remove"),
                            t.breakpoint_red,
                            FontStyle::UiSmall,
                        );
                        self.hit_regions
                            .push((del_b, HitAction::GitRemoveRemote(rm.name)));
                        y += 30;
                    }
                }
            }
            _ => {}
        }
    }

    fn visible_settings_nav(&self) -> Vec<SettingsNavItem> {
        let query = self
            .settings_query_input
            .as_str()
            .trim()
            .to_ascii_lowercase();
        if query.is_empty() {
            return SETTINGS_NAV.to_vec();
        }
        let mut out = Vec::new();
        let mut index = 0;
        while index < SETTINGS_NAV.len() {
            let item = SETTINGS_NAV[index];
            if item.page.is_none() {
                let mut end = index + 1;
                while end < SETTINGS_NAV.len() && SETTINGS_NAV[end].depth > item.depth {
                    end += 1;
                }
                let group_hit = self.settings_nav_matches(&item, &query);
                let mut children = Vec::new();
                for child in &SETTINGS_NAV[index + 1..end] {
                    if group_hit || self.settings_nav_matches(child, &query) {
                        children.push(*child);
                    }
                }
                if group_hit || !children.is_empty() {
                    out.push(item);
                    out.extend(children);
                }
                index = end;
            } else {
                if self.settings_nav_matches(&item, &query) {
                    out.push(item);
                }
                index += 1;
            }
        }
        out
    }

    fn settings_nav_matches(&self, item: &SettingsNavItem, query: &str) -> bool {
        let label = match self.ui_lang {
            UiLanguage::ZhCn => item.zh,
            UiLanguage::EnUs => item.en,
        };
        label.to_ascii_lowercase().contains(query)
            || item.keywords.to_ascii_lowercase().contains(query)
    }

    fn settings_row_fg(&self, hot: bool) -> u32 {
        if hot && self.theme.kind != ThemeKind::IslandsLight {
            0xFFFFFF
        } else {
            self.theme.text_primary
        }
    }

    fn settings_note(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        text: &str,
    ) {
        if let Some(y) = flow.place(18, 6) {
            canvas.draw_text_styled(x, y, text, self.theme.text_muted, w, FontStyle::UiSmall);
        }
    }

    fn settings_section(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        text: &str,
    ) {
        flow.cursor += 6;
        if let Some(y) = flow.place(22, 6) {
            canvas.draw_text_styled(x, y, text, self.theme.text_primary, w, FontStyle::UiBold);
        }
    }

    fn settings_kv(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        label: &str,
        value: &str,
    ) {
        if let Some(y) = flow.place(26, 2) {
            let t = self.theme;
            canvas.draw_text_styled(x, y + 3, label, t.text_primary, 210, FontStyle::UiRegular);
            canvas.draw_text_styled(
                x + 218,
                y + 4,
                value,
                t.text_secondary,
                (w - 218).max(48),
                FontStyle::UiSmall,
            );
        }
    }

    fn settings_status(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        label: &str,
        on: bool,
    ) {
        if let Some(y) = flow.place(28, 3) {
            let t = self.theme;
            canvas.draw_text_styled(
                x,
                y + 4,
                label,
                t.text_primary,
                (w - 96).max(40),
                FontStyle::UiRegular,
            );
            let badge = if on {
                self.tr("已启用", "On")
            } else {
                self.tr("未检测到", "Not found")
            };
            let (bw, _) = canvas.measure_text(badge, FontStyle::UiSmall);
            let badge_w = bw + 16;
            let rect = Rect::new(x + w - badge_w, y + 2, badge_w, 22);
            canvas.fill_rounded_rect(rect, 6, if on { t.diff_added_bg } else { t.editor_bg });
            canvas.draw_text_centered(
                rect,
                badge,
                if on { t.diff_added_fg } else { t.text_muted },
                FontStyle::UiSmall,
            );
        }
    }

    fn draw_settings_button(
        &mut self,
        canvas: &mut SurfaceCanvas,
        rect: Rect,
        label: &str,
        primary: bool,
        hx: i32,
        hy: i32,
        action: HitAction,
    ) {
        let t = self.theme;
        let hot = rect.contains(hx, hy);
        if primary {
            canvas.fill_rounded_rect(rect, 6, t.accent_blue);
            canvas.draw_text_centered(rect, label, 0xFFFFFF, FontStyle::UiBold);
        } else {
            let fill = if hot { t.popup_hover_bg } else { t.editor_bg };
            let border = if hot { t.accent_blue } else { t.border_color };
            canvas.stroke_rounded_rect(rect, 6, border, fill);
            canvas.draw_text_centered(rect, label, self.settings_row_fg(hot), FontStyle::UiRegular);
        }
        self.hit_regions.push((rect, action));
    }

    fn draw_settings_theme_cards(
        &mut self,
        canvas: &mut SurfaceCanvas,
        x: i32,
        y: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let kinds = ThemeKind::all();
        let gap = 10;
        let count = kinds.len() as i32;
        let card_w = ((w - gap * (count - 1)) / count).max(1);
        let mut cx = x;
        for &kind in kinds {
            let theme = IdeaTheme::from_kind(kind);
            let card = Rect::new(cx, y, card_w, 86);
            let active = self.theme.kind == kind;
            let hot = card.contains(hx, hy);
            let border = if active || hot {
                self.theme.accent_blue
            } else {
                self.theme.border_color
            };
            canvas.stroke_rounded_rect(card, 8, border, theme.editor_bg);
            canvas.fill_rect(Rect::new(cx + 1, y + 1, card_w - 2, 16), theme.panel_bg);
            canvas.fill_rounded_rect(Rect::new(cx + 12, y + 26, 26, 4), 2, theme.syn_keyword);
            canvas.fill_rounded_rect(Rect::new(cx + 42, y + 26, 34, 4), 2, theme.syn_function);
            canvas.fill_rounded_rect(Rect::new(cx + 20, y + 36, 18, 4), 2, theme.syn_keyword);
            canvas.fill_rounded_rect(Rect::new(cx + 42, y + 36, 40, 4), 2, theme.syn_string);
            canvas.fill_rounded_rect(Rect::new(cx + 20, y + 46, 46, 4), 2, theme.syn_comment);
            let name = match kind {
                ThemeKind::IslandsDark => "Islands Dark",
                ThemeKind::DarculaClassic => "Darcula",
                ThemeKind::IslandsLight => "Light",
            };
            canvas.draw_text_styled(
                cx + 12,
                y + 60,
                name,
                theme.text_primary,
                card_w - 36,
                FontStyle::UiSmall,
            );
            if active {
                canvas.draw_check_icon(cx + card_w - 22, y + 62, theme.accent_blue);
            }
            self.hit_regions.push((card, HitAction::SelectTheme(kind)));
            cx += card_w + gap;
        }
    }

    fn draw_settings_scale(
        &mut self,
        canvas: &mut SurfaceCanvas,
        x: i32,
        y: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let presets: [(u16, &str); 5] = [
            (90, "90%"),
            (100, "100%"),
            (110, "110%"),
            (125, "125%"),
            (150, "150%"),
        ];
        let current = (self.ui_scale * 100.0).round() as i32;
        let seg_w = ((w - 32) / 5).clamp(40, 108);
        let mut sx = x;
        for (pct, label) in presets {
            let btn = Rect::new(sx, y, seg_w, 30);
            let active = (current - pct as i32).abs() <= 3;
            let hot = btn.contains(hx, hy);
            let fill = if active {
                self.theme.accent_blue
            } else if hot {
                self.theme.popup_hover_bg
            } else {
                self.theme.editor_bg
            };
            let border = if active || hot {
                self.theme.accent_blue
            } else {
                self.theme.border_color
            };
            canvas.stroke_rounded_rect(btn, 6, border, fill);
            let fg = if active {
                0xFFFFFF
            } else {
                self.settings_row_fg(hot)
            };
            canvas.draw_text_centered(
                btn,
                label,
                fg,
                if active {
                    FontStyle::UiBold
                } else {
                    FontStyle::UiRegular
                },
            );
            self.hit_regions.push((btn, HitAction::SetUiScale(pct)));
            sx += seg_w + 8;
        }
    }

    fn draw_settings_languages(
        &mut self,
        canvas: &mut SurfaceCanvas,
        x: i32,
        y: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let mut ly = y;
        let row_w = w.min(460);
        for &lang in UiLanguage::all() {
            let row = Rect::new(x, ly, row_w, 32);
            let active = self.ui_lang == lang;
            let hot = row.contains(hx, hy);
            if active {
                canvas.fill_rounded_rect(row, 6, self.theme.editor_selection_bg);
            } else if hot {
                canvas.fill_rounded_rect(row, 6, self.theme.panel_header_bg);
            }
            let ring = Rect::new(x + 10, ly + 8, 16, 16);
            canvas.stroke_rounded_rect(
                ring,
                8,
                if active {
                    self.theme.accent_blue
                } else {
                    self.theme.border_color
                },
                self.theme.editor_bg,
            );
            if active {
                canvas.fill_circle(x + 18, ly + 16, 4, self.theme.accent_blue);
            }
            canvas.draw_text_styled(
                x + 36,
                ly + 6,
                lang.display_name(),
                self.settings_row_fg(active || hot),
                row_w - 48,
                if active {
                    FontStyle::UiBold
                } else {
                    FontStyle::UiRegular
                },
            );
            self.hit_regions.push((row, HitAction::SetUiLanguage(lang)));
            ly += 36;
        }
    }

    fn draw_settings_action_row(
        &mut self,
        canvas: &mut SurfaceCanvas,
        x: i32,
        y: i32,
        w: i32,
        action: IdeAction,
        hx: i32,
        hy: i32,
        clickable: bool,
    ) {
        let t = self.theme;
        let row = Rect::new(x, y, w, 26);
        let hot = clickable && row.contains(hx, hy);
        if hot {
            canvas.fill_rounded_rect(row, 5, t.popup_hover_bg);
        }
        let fg = self.settings_row_fg(hot);
        canvas.draw_text_styled(
            x + 8,
            y + 3,
            action.title_i18n(self.ui_lang),
            fg,
            (w - 156).max(40),
            FontStyle::UiRegular,
        );
        let shortcut = action.shortcut_label();
        if !shortcut.is_empty() {
            let (sw, _) = canvas.measure_text(shortcut, FontStyle::CodeMono);
            canvas.draw_text_styled(
                x + w - sw - 10,
                y + 4,
                shortcut,
                if hot { fg } else { t.syn_keyword },
                sw + 4,
                FontStyle::CodeMono,
            );
        }
        if clickable {
            self.hit_regions
                .push((row, HitAction::ExecuteIdeAction(action)));
        }
    }

    fn render_settings_appearance(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let intro = self.tr(
            "主题、界面缩放和显示语言。更改立即生效。",
            "Theme, zoom, and UI language. Changes apply immediately.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        let theme_title = self.tr("主题", "Theme");
        self.settings_section(canvas, flow, x, w, theme_title);
        if let Some(y) = flow.place(86, 8) {
            self.draw_settings_theme_cards(canvas, x, y, w, hx, hy);
        }
        let scheme = self.theme.kind.display_name();
        let scheme_note = match self.ui_lang {
            UiLanguage::ZhCn => format!("当前方案：{scheme}"),
            UiLanguage::EnUs => format!("Current scheme: {scheme}"),
        };
        self.settings_note(canvas, flow, x, w, &scheme_note);

        let zoom_title = self.tr("缩放", "Zoom");
        self.settings_section(canvas, flow, x, w, zoom_title);
        if let Some(y) = flow.place(30, 8) {
            self.draw_settings_scale(canvas, x, y, w, hx, hy);
        }
        let sys = (self.system_dpi_scale * 100.0).round() as i32;
        let ui = (self.ui_scale * 100.0).round() as i32;
        let eff = (self.effective_scale() * 100.0).round() as i32;
        let zoom_note = match self.ui_lang {
            UiLanguage::ZhCn => format!(
                "系统 DPI {sys}%  ×  界面 {ui}%  =  有效缩放 {eff}%     Ctrl+=   Ctrl+-   Ctrl+0"
            ),
            UiLanguage::EnUs => format!(
                "System DPI {sys}%  ×  UI {ui}%  =  effective {eff}%     Ctrl+=   Ctrl+-   Ctrl+0"
            ),
        };
        self.settings_note(canvas, flow, x, w, &zoom_note);

        let lang_title = self.tr("语言和区域", "Language and Region");
        self.settings_section(canvas, flow, x, w, lang_title);
        if let Some(y) = flow.place(68, 4) {
            self.draw_settings_languages(canvas, x, y, w, hx, hy);
        }
    }

    fn render_settings_menus(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let intro = self.tr(
            "主菜单与 IntelliJ IDEA 对齐。要执行命令，请打开「按键映射」。",
            "Main menu matches IntelliJ IDEA. Run a command from Keymap.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        for cat in MainMenuCategory::all() {
            let title = cat.label_i18n(self.ui_lang).to_string();
            self.settings_section(canvas, flow, x, w, &title);
            for entry in cat.entries() {
                let MenuEntry::Action(action) = entry else {
                    continue;
                };
                if let Some(y) = flow.place(26, 2) {
                    self.draw_settings_action_row(canvas, x, y, w, action, hx, hy, false);
                }
            }
        }
    }

    fn render_settings_system(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let intro = self.tr(
            "当前项目与最近打开的工作区。",
            "Current project and recently opened workspaces.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        let project_title = self.tr("项目", "Project");
        self.settings_section(canvas, flow, x, w, project_title);
        let root = self.workspace_root.display().to_string();
        let path_label = self.tr("项目路径", "Project path");
        self.settings_kv(canvas, flow, x, w, path_label, &root);
        let recent_title = self.tr("最近的项目", "Recent Projects");
        self.settings_section(canvas, flow, x, w, recent_title);
        let current = self.workspace_root.clone();
        let recent: Vec<_> = self.recent_workspaces.iter().take(6).cloned().collect();
        let mut any = false;
        for path in recent {
            if path == current {
                continue;
            }
            any = true;
            if let Some(y) = flow.place(26, 2) {
                let row = Rect::new(x, y, w, 26);
                let hot = row.contains(hx, hy);
                if hot {
                    canvas.fill_rounded_rect(row, 5, self.theme.popup_hover_bg);
                }
                let text = path.display().to_string();
                canvas.draw_text_styled(
                    x + 8,
                    y + 3,
                    &text,
                    self.settings_row_fg(hot),
                    w - 16,
                    FontStyle::UiSmall,
                );
                self.hit_regions
                    .push((row, HitAction::PromptOpenWorkspace(path)));
            }
        }
        if !any {
            let empty = self.tr("没有其他最近的项目", "No other recent projects");
            self.settings_note(canvas, flow, x, w, empty);
        }
        let lang_title = self.tr("语言和区域", "Language and Region");
        self.settings_section(canvas, flow, x, w, lang_title);
        if let Some(y) = flow.place(68, 4) {
            self.draw_settings_languages(canvas, x, y, w, hx, hy);
        }
    }

    fn render_settings_editor_general(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let intro = self.tr(
            "编辑器行为与 IntelliJ IDEA 快捷键保持一致。",
            "Editor behavior follows the IntelliJ IDEA keymap.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        let title = self.tr("编辑器", "Editor");
        self.settings_section(canvas, flow, x, w, title);
        let rows = [
            (self.tr("显示行号", "Show line numbers"), true),
            (self.tr("高亮当前行", "Highlight current line"), true),
            (self.tr("回车自动缩进", "Auto-indent on Enter"), true),
            (self.tr("Tab 插入 4 个空格", "Tab inserts 4 spaces"), true),
        ];
        for (label, on) in rows {
            self.settings_status(canvas, flow, x, w, label, on);
        }
        let keys = self.tr("快捷键", "Shortcuts");
        self.settings_section(canvas, flow, x, w, keys);
        for action in [
            IdeAction::ToggleLineComment,
            IdeAction::DuplicateLine,
            IdeAction::DeleteLine,
            IdeAction::ReformatCode,
            IdeAction::UndoEdit,
            IdeAction::RedoEdit,
        ] {
            if let Some(y) = flow.place(26, 2) {
                self.draw_settings_action_row(canvas, x, y, w, action, hx, hy, true);
            }
        }
    }

    fn render_settings_font(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let intro = self.tr(
            "界面与编辑器共用同一缩放，和 IDEA 的 Zoom 一致。",
            "UI and editor share one zoom, matching IDEA Zoom.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        let font_title = self.tr("字体", "Font");
        self.settings_section(canvas, flow, x, w, font_title);
        let ui_font = self.tr("界面字体", "UI font");
        self.settings_kv(canvas, flow, x, w, ui_font, "Microsoft YaHei UI");
        let editor_font = self.tr("编辑器字体", "Editor font");
        self.settings_kv(canvas, flow, x, w, editor_font, "Cascadia Mono");
        let zoom_title = self.tr("字号缩放", "Font zoom");
        self.settings_section(canvas, flow, x, w, zoom_title);
        if let Some(y) = flow.place(30, 8) {
            self.draw_settings_scale(canvas, x, y, w, hx, hy);
        }
    }

    fn render_settings_scheme(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let intro = self.tr(
            "编辑器配色与界面主题联动，预览使用当前方案。",
            "Editor colors follow the UI theme. The preview uses the current scheme.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        let title = self.tr("配色方案", "Color Scheme");
        self.settings_section(canvas, flow, x, w, title);
        if let Some(y) = flow.place(86, 8) {
            self.draw_settings_theme_cards(canvas, x, y, w, hx, hy);
        }
        if let Some(y) = flow.place(78, 10) {
            let t = self.theme;
            let preview = Rect::new(x, y, w.min(520), 78);
            canvas.stroke_rounded_rect(preview, 6, t.border_color, t.editor_bg);
            let x1 = canvas.draw_text_styled(
                x + 14,
                y + 10,
                "fn ",
                t.syn_keyword,
                36,
                FontStyle::CodeMono,
            );
            let x2 = canvas.draw_text_styled(
                x1,
                y + 10,
                "main",
                t.syn_function,
                64,
                FontStyle::CodeMono,
            );
            canvas.draw_text_styled(x2, y + 10, "() {", t.text_primary, 48, FontStyle::CodeMono);
            let x3 = canvas.draw_text_styled(
                x + 30,
                y + 30,
                "let ",
                t.syn_keyword,
                40,
                FontStyle::CodeMono,
            );
            let x4 = canvas.draw_text_styled(
                x3,
                y + 30,
                "theme",
                t.syn_field_or_property,
                70,
                FontStyle::CodeMono,
            );
            let x5 =
                canvas.draw_text_styled(x4, y + 30, " = ", t.text_primary, 28, FontStyle::CodeMono);
            canvas.draw_text_styled(
                x5,
                y + 30,
                "\"islands\"",
                t.syn_string,
                110,
                FontStyle::CodeMono,
            );
            canvas.draw_text_styled(
                x + 30,
                y + 50,
                "// Color Scheme",
                t.syn_comment,
                220,
                FontStyle::CodeMono,
            );
        }
        if let Some(y) = flow.place(24, 4) {
            let t = self.theme;
            let chips = [
                (self.tr("关键字", "Keyword"), t.syn_keyword),
                (self.tr("函数", "Function"), t.syn_function),
                (self.tr("字符串", "String"), t.syn_string),
                (self.tr("注释", "Comment"), t.syn_comment),
                (self.tr("类型", "Type"), t.syn_type),
            ];
            let mut cx = x;
            for (name, color) in chips {
                canvas.stroke_rounded_rect(Rect::new(cx, y + 4, 14, 14), 3, t.border_color, color);
                canvas.draw_text_styled(
                    cx + 18,
                    y + 2,
                    name,
                    t.text_secondary,
                    70,
                    FontStyle::UiSmall,
                );
                cx += 96;
            }
        }
    }

    fn render_settings_codestyle(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let intro = self.tr(
            "Rust、TypeScript、JavaScript 与 Python 使用同一套缩进。",
            "Rust, TypeScript, JavaScript, and Python share one indent.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        let title = self.tr("缩进", "Tabs and Indents");
        self.settings_section(canvas, flow, x, w, title);
        let tab = self.tr("制表符宽度", "Tab size");
        self.settings_kv(canvas, flow, x, w, tab, "4");
        let indent = self.tr("缩进", "Indent");
        self.settings_kv(canvas, flow, x, w, indent, "4");
        let use_tab = self.tr("使用制表符", "Use tab character");
        self.settings_kv(
            canvas,
            flow,
            x,
            w,
            use_tab,
            self.tr("关（空格）", "Off (spaces)"),
        );
        let fmt = self.tr("重新格式化代码", "Reformat Code");
        self.settings_section(canvas, flow, x, w, fmt);
        let fmt_note = self.tr(
            "Ctrl+Alt+L 去除行尾空白，并保证文件以换行结束。",
            "Ctrl+Alt+L trims trailing whitespace and keeps a final newline.",
        );
        self.settings_note(canvas, flow, x, w, fmt_note);
        if let Some(y) = flow.place(26, 2) {
            self.draw_settings_action_row(canvas, x, y, w, IdeAction::ReformatCode, hx, hy, true);
        }
    }

    fn render_settings_keymap(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let intro = self.tr(
            "当前方案为 IntelliJ IDEA。点击条目会直接执行。",
            "Active keymap: IntelliJ IDEA. Click an entry to run it.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        if let Some(y) = flow.place(32, 10) {
            let label = self.tr("按键映射", "Keymap");
            canvas.draw_text_styled(
                x,
                y + 6,
                label,
                self.theme.text_primary,
                120,
                FontStyle::UiRegular,
            );
            let box_r = Rect::new(x + 128, y, (w - 128).min(360), 30);
            canvas.stroke_rounded_rect(box_r, 6, self.theme.border_color, self.theme.editor_bg);
            canvas.draw_text_styled(
                box_r.x + 10,
                box_r.y + 6,
                "IntelliJ IDEA",
                self.theme.text_primary,
                box_r.w - 20,
                FontStyle::UiRegular,
            );
        }
        let mut seen = Vec::new();
        for cat in MainMenuCategory::all() {
            let mut actions = Vec::new();
            for entry in cat.entries() {
                let MenuEntry::Action(action) = entry else {
                    continue;
                };
                if !seen.contains(&action) {
                    seen.push(action);
                    actions.push(action);
                }
            }
            if actions.is_empty() {
                continue;
            }
            let title = cat.label_i18n(self.ui_lang).to_string();
            self.settings_section(canvas, flow, x, w, &title);
            for action in actions {
                if let Some(y) = flow.place(26, 2) {
                    self.draw_settings_action_row(canvas, x, y, w, action, hx, hy, true);
                }
            }
        }
        let mut rest = Vec::new();
        for &action in IdeAction::all() {
            if !seen.contains(&action) {
                seen.push(action);
                rest.push(action);
            }
        }
        if !rest.is_empty() {
            let other = self.tr("其他", "Other");
            self.settings_section(canvas, flow, x, w, other);
            for action in rest {
                if let Some(y) = flow.place(26, 2) {
                    self.draw_settings_action_row(canvas, x, y, w, action, hx, hy, true);
                }
            }
        }
    }

    fn render_settings_plugins(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
    ) {
        let intro = self.tr(
            "已捆绑的语言服务与工具窗口。",
            "Bundled language servers and tool windows.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        let installed = self.tr("已安装", "Installed");
        self.settings_section(canvas, flow, x, w, installed);
        let servers: Vec<(String, bool)> = self
            .lsp_hub
            .servers
            .iter()
            .map(|server| {
                (
                    format!(
                        "{}   {}",
                        server.language.display_name(),
                        server.binary_name
                    ),
                    server.resolved_path.is_some(),
                )
            })
            .collect();
        for (label, ready) in &servers {
            self.settings_status(canvas, flow, x, w, label, *ready);
        }
        let bundled = self.tr("已捆绑", "Bundled");
        self.settings_section(canvas, flow, x, w, bundled);
        let items = [
            self.tr("Git 集成", "Git Integration"),
            self.tr("终端 (ConPTY)", "Terminal (ConPTY)"),
            self.tr(
                "调试器 (codelldb / js-debug / debugpy)",
                "Debugger (codelldb / js-debug / debugpy)",
            ),
            self.tr("中文语言包", "Chinese Language Pack"),
        ];
        for label in items {
            self.settings_status(canvas, flow, x, w, label, true);
        }
    }

    fn render_settings_git(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let intro = self.tr(
            "Git 工具窗口与快捷键对齐 IntelliJ IDEA。",
            "Git tool window and shortcuts follow IntelliJ IDEA.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        let branch = if self.git_engine.current_branch.is_empty() {
            self.tr("（无）", "(none)").to_string()
        } else {
            self.git_engine.current_branch.clone()
        };
        let branch_label = self.tr("当前分支", "Current branch");
        self.settings_kv(canvas, flow, x, w, branch_label, &branch);
        let title = self.tr("操作", "Actions");
        self.settings_section(canvas, flow, x, w, title);
        for action in [
            IdeAction::GitCommitPopup,
            IdeAction::GitPush,
            IdeAction::GitPull,
            IdeAction::GitFetch,
            IdeAction::GitMerge,
            IdeAction::GitRebase,
            IdeAction::GitBranches,
            IdeAction::GitNewBranch,
            IdeAction::GitNewTag,
            IdeAction::GitStashChanges,
            IdeAction::GitUnstashChanges,
            IdeAction::GitResetHead,
            IdeAction::GitRollback,
            IdeAction::GitShowDiff,
            IdeAction::GitAnnotateBlame,
            IdeAction::GitFileHistory,
            IdeAction::GitShowLog,
            IdeAction::GitManageRemotes,
            IdeAction::RevertLastCheckpoint,
        ] {
            if let Some(y) = flow.place(26, 2) {
                self.draw_settings_action_row(canvas, x, y, w, action, hx, hy, true);
            }
        }
    }

    fn render_settings_debugger(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let intro = self.tr(
            "Rust 使用 codelldb，TypeScript / JavaScript 使用 js-debug，Python 使用 debugpy。",
            "Rust uses codelldb, TypeScript / JavaScript uses js-debug, Python uses debugpy.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        let adapter_label = self.tr("调试器", "Debugger");
        let adapter = self.dap_engine.active_adapter.clone();
        self.settings_kv(canvas, flow, x, w, adapter_label, &adapter);
        let bp_count: usize = self
            .dap_engine
            .breakpoints
            .iter()
            .map(|(_, lines)| lines.len())
            .sum();
        let bp_label = self.tr("断点", "Breakpoints");
        let bp_value = bp_count.to_string();
        self.settings_kv(canvas, flow, x, w, bp_label, &bp_value);
        let title = self.tr("操作", "Actions");
        self.settings_section(canvas, flow, x, w, title);
        for action in [
            IdeAction::RunActiveConfig,
            IdeAction::DebugActiveConfig,
            IdeAction::ToggleBreakpoint,
        ] {
            if let Some(y) = flow.place(26, 2) {
                self.draw_settings_action_row(canvas, x, y, w, action, hx, hy, true);
            }
        }
    }

    fn render_settings_terminal_page(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let intro = self.tr(
            "内嵌终端使用 ConPTY。Alt+F12 打开或关闭终端窗口。",
            "The embedded terminal uses ConPTY. Alt+F12 shows or hides it.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        let shell = self.tr("Shell", "Shell");
        self.settings_kv(canvas, flow, x, w, shell, "ConPTY");
        let tabs = self.terminal.tabs.len().to_string();
        let tabs_label = self.tr("标签页", "Tabs");
        self.settings_kv(canvas, flow, x, w, tabs_label, &tabs);
        let title = self
            .terminal
            .tabs
            .get(self.terminal.active_tab)
            .map(|tab| tab.title.clone())
            .unwrap_or_default();
        let active = self.tr("当前标签", "Active tab");
        self.settings_kv(canvas, flow, x, w, active, &title);
        if let Some(y) = flow.place(26, 2) {
            self.draw_settings_action_row(
                canvas,
                x,
                y,
                w,
                IdeAction::ToggleTerminalWindow,
                hx,
                hy,
                true,
            );
        }
    }

    fn render_settings_languages_page(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
    ) {
        let intro = self.tr(
            "首发语言与 IDEA 一样走 Language Server 和调试适配器。",
            "First-tier languages use a language server and a debug adapter, as in IDEA.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        let rows = [
            ("Rust", "rust-analyzer", "codelldb"),
            ("TypeScript", "vtsls", "js-debug"),
            ("JavaScript", "vtsls", "js-debug"),
            ("Python", "pyright-langserver", "debugpy"),
            (
                "JSON / TOML / Markdown",
                self.tr("内置语法", "Built-in syntax"),
                "—",
            ),
        ];
        for (lang, lsp, dap) in rows {
            let title = lang.to_string();
            self.settings_section(canvas, flow, x, w, &title);
            let lsp_label = self.tr("语言服务", "Language server");
            self.settings_kv(canvas, flow, x, w, lsp_label, lsp);
            let dap_label = self.tr("调试器", "Debugger");
            self.settings_kv(canvas, flow, x, w, dap_label, dap);
        }
    }

    fn render_settings_advanced(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
    ) {
        let intro = self.tr(
            "这些值对应当前窗口。主题、缩放和语言在「外观」中修改。",
            "These values describe this window. Change theme, zoom, and language under Appearance.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        let ui = format!("{}%", (self.ui_scale * 100.0).round() as i32);
        let sys = format!("{}%", (self.system_dpi_scale * 100.0).round() as i32);
        let eff = format!("{}%", (self.effective_scale() * 100.0).round() as i32);
        let ui_label = self.tr("界面缩放", "UI zoom");
        self.settings_kv(canvas, flow, x, w, ui_label, &ui);
        let dpi_label = self.tr("系统 DPI", "System DPI");
        self.settings_kv(canvas, flow, x, w, dpi_label, &sys);
        let eff_label = self.tr("有效缩放", "Effective scale");
        self.settings_kv(canvas, flow, x, w, eff_label, &eff);
        let theme_label = self.tr("主题", "Theme");
        self.settings_kv(
            canvas,
            flow,
            x,
            w,
            theme_label,
            self.theme.kind.display_name(),
        );
        let lang_label = self.tr("语言", "Language");
        self.settings_kv(canvas, flow, x, w, lang_label, self.ui_lang.display_name());
        let map_label = self.tr("按键映射", "Keymap");
        self.settings_kv(canvas, flow, x, w, map_label, "IntelliJ IDEA");
        let ui_mode = self.tr("界面", "UI");
        self.settings_kv(canvas, flow, x, w, ui_mode, "New UI / Islands");
    }

    fn mobile_sdk_badge(&self, id: &str, configured: &str) -> (String, bool) {
        if let Some(tool) = self.mobile_env.sdk_tools.iter().find(|t| t.id == id) {
            if tool.installed {
                return (self.tr("已安装", "Installed").to_string(), true);
            }
            return (self.tr("SDK 未安装", "SDK not installed").to_string(), false);
        }
        if self.mobile_env.probing {
            return (self.tr("检测中", "Checking").to_string(), false);
        }
        if configured.trim().is_empty() {
            (
                self.tr("留空则自动检测", "Auto-detect when empty").to_string(),
                false,
            )
        } else {
            (self.tr("SDK 未安装", "SDK not installed").to_string(), false)
        }
    }

    fn render_mobile_sdk_path_row(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        hx: i32,
        hy: i32,
        slot: u8,
        title: &str,
        tool_id: &str,
        placeholder: &str,
    ) {
        let configured = match slot {
            0 => self.mobile_sdk_flutter.as_str().to_string(),
            1 => self.mobile_sdk_android.as_str().to_string(),
            _ => self.mobile_sdk_jdk.as_str().to_string(),
        };
        let (badge, installed) = self.mobile_sdk_badge(tool_id, &configured);
        let heading = format!("{title}  ·  {badge}");
        self.settings_section(canvas, flow, x, w, &heading);
        if let Some(detail) = self
            .mobile_env
            .sdk_tools
            .iter()
            .find(|tool| tool.id == tool_id)
            .map(|tool| tool.version_or_detail.clone())
        {
            if !detail.is_empty() {
                let color_note = if installed {
                    detail
                } else if !detail.contains("SDK 未安装") {
                    format!("SDK 未安装：{detail}")
                } else {
                    detail
                };
                self.settings_note(canvas, flow, x, w, &color_note);
            }
        }
        if let Some(y) = flow.place(32, 8) {
            let t = self.theme;
            let browse_w = 78;
            let field = Rect::new(x, y, (w - browse_w - 8).max(48), 32);
            self.mobile_sdk_field_rects[slot as usize] = field;
            let focused =
                self.settings_sdk_focus == Some(slot) && self.focus == FocusTarget::SettingsModal;
            let border = if focused { t.accent_blue } else { t.border_color };
            let editor_bg = t.editor_bg;
            let caret_visible = self.caret_visible;
            let text_primary = t.text_primary;
            let text_muted = t.text_muted;
            let selection_bg = t.editor_selection_bg;
            let caret_color = t.accent_blue;
            canvas.stroke_rounded_rect(field, 6, border, editor_bg);
            let ime = self.mobile_sdk_input_mut(slot).render_singleline(
                canvas,
                Rect::new(field.x + 6, field.y, (field.w - 10).max(20), field.h),
                2,
                field.y + 7,
                placeholder,
                focused,
                caret_visible,
                text_primary,
                text_muted,
                selection_bg,
                caret_color,
                FontStyle::UiRegular,
            );
            if focused {
                self.ime_spot = ime;
            }
            self.hit_regions
                .push((field, HitAction::FocusMobileSdkPath(slot)));
            let browse = Rect::new(field.x + field.w + 8, y, browse_w, 32);
            let browse_label = self.tr("浏览…", "Browse…");
            self.draw_settings_button(
                canvas,
                browse,
                browse_label,
                false,
                hx,
                hy,
                HitAction::BrowseMobileSdkDir(slot),
            );
        }
    }

    fn render_settings_mobile_sdk(
        &mut self,
        canvas: &mut SurfaceCanvas,
        flow: &mut SettingsFlow,
        x: i32,
        w: i32,
        hx: i32,
        hy: i32,
    ) {
        let intro = self.tr(
            "指定 Flutter、Android SDK 与 JDK 的安装目录。留空时自动检测 PATH 和环境变量；目录无效时，运行和检测会提示 SDK 未安装。",
            "Set the Flutter, Android SDK, and JDK install directories. Empty fields are auto-detected. An invalid directory is reported as SDK not installed when you run or probe a mobile project.",
        );
        self.settings_note(canvas, flow, x, w, intro);
        let flutter_ph = self.tr("例如 D:\\src\\flutter", "e.g. D:\\src\\flutter");
        self.render_mobile_sdk_path_row(
            canvas, flow, x, w, hx, hy, 0, "Flutter SDK", "flutter", flutter_ph,
        );
        let android_ph = self.tr(
            "例如 %LOCALAPPDATA%\\Android\\Sdk",
            "e.g. %LOCALAPPDATA%\\Android\\Sdk",
        );
        self.render_mobile_sdk_path_row(
            canvas, flow, x, w, hx, hy, 1, "Android SDK", "android_sdk", android_ph,
        );
        let jdk_ph = self.tr(
            "例如 C:\\Program Files\\Java\\jdk-17",
            "e.g. C:\\Program Files\\Java\\jdk-17",
        );
        self.render_mobile_sdk_path_row(
            canvas, flow, x, w, hx, hy, 2, "JDK", "jdk", jdk_ph,
        );
        if let Some(y) = flow.place(32, 8) {
            let label = if self.mobile_env.probing {
                self.tr("检测中…", "Checking…")
            } else {
                self.tr("重新检测", "Recheck")
            };
            self.draw_settings_button(
                canvas,
                Rect::new(x, y, 120, 30),
                label,
                true,
                hx,
                hy,
                HitAction::RefreshMobileEnv,
            );
        }
    }

    /// IntelliJ IDEA Settings (`Ctrl+Alt+S`)：左侧树、搜索、右侧页面、确定 / 应用 / 取消。
    fn render_settings_modal(&mut self, canvas: &mut SurfaceCanvas, w: i32, h: i32) {
        self.mobile_sdk_field_rects = [Rect::new(0, 0, 0, 0); 3];
        if self.settings_page != SettingsPage::MobileSdk {
            self.settings_sdk_focus = None;
        }
        let t = self.theme;
        let (hx, hy) = self.hover_pos;
        let mw = (w - 36).clamp(560, 1080).min(w - 8);
        let mh = (h - 28).clamp(420, 760).min(h - 8);
        let mx = (w - mw) / 2;
        let my = ((h - mh) / 2).max(8);
        let modal = Rect::new(mx, my, mw, mh);

        canvas.fill_rect_alpha(Rect::new(0, 0, w, h), 0x000000, 88);
        self.active_overlay_rects.push(Rect::new(0, 0, w, h));
        self.active_overlay_rects.push(modal);
        canvas.fill_rounded_shadow(modal, 10);
        canvas.stroke_rounded_rect(modal, 10, t.border_color, t.panel_bg);

        canvas.draw_text_styled(
            mx + 20,
            my + 14,
            self.tr("设置", "Settings"),
            t.text_primary,
            mw - 80,
            FontStyle::UiBold,
        );
        let close = Rect::new(mx + mw - 42, my + 10, 26, 26);
        let close_hot = close.contains(hx, hy);
        if close_hot {
            canvas.fill_rounded_rect(close, 6, t.popup_hover_bg);
        }
        canvas.draw_window_close_icon(
            close.x + 13,
            close.y + 13,
            if close_hot {
                t.text_primary
            } else {
                t.text_secondary
            },
        );
        self.hit_regions.push((close, HitAction::CloseModalOrPopup));

        let search = Rect::new(mx + 16, my + 46, mw - 32, 32);
        self.settings_search_rect = search;
        canvas.stroke_rounded_rect(search, 6, t.accent_blue, t.editor_bg);
        canvas.draw_search_icon(search.x + 10, search.y + 8, t.text_muted);
        self.settings_search_text_x = search.x + 32;
        let field = Rect::new(search.x + 30, search.y, (search.w - 40).max(40), search.h);
        let placeholder = match self.ui_lang {
            UiLanguage::ZhCn => "搜索设置",
            UiLanguage::EnUs => "Search settings",
        };
        let ime = self.settings_query_input.render_singleline(
            canvas,
            field,
            2,
            search.y + 6,
            placeholder,
            true,
            self.caret_visible,
            t.text_primary,
            t.text_muted,
            t.editor_selection_bg,
            t.accent_blue,
            FontStyle::UiRegular,
        );
        self.ime_spot = ime;
        self.hit_regions
            .push((search, HitAction::FocusSettingsSearch));

        canvas.fill_rect(Rect::new(mx + 1, my + 90, mw - 2, 1), t.border_color);

        let nav_w = if mw >= 900 { 252 } else { 200 };
        let body_top = my + 91;
        let footer_h = 52;
        let body_h = (mh - 91 - footer_h).max(80);
        let nav_rect = Rect::new(mx, body_top, nav_w, body_h);
        self.settings_nav_rect = nav_rect;
        self.settings_content_rect = Rect::new(mx + nav_w, body_top, mw - nav_w, body_h);
        canvas.fill_rect(Rect::new(mx + nav_w, body_top, 1, body_h), t.border_color);

        let nav = self.visible_settings_nav();
        let query_active = !self.settings_query_input.as_str().trim().is_empty();
        if query_active {
            let pages: Vec<SettingsPage> = nav.iter().filter_map(|item| item.page).collect();
            if let Some(first) = pages.first().copied() {
                if !pages.contains(&self.settings_page) {
                    self.settings_page = first;
                    self.modal_scroll = 0;
                }
            }
        }
        let page = self.settings_page;

        let row_h = 28;
        let nav_visible = (nav_rect.h / row_h).max(1) as usize;
        self.settings_nav_max_scroll = nav.len().saturating_sub(nav_visible);
        if self.settings_nav_scroll > self.settings_nav_max_scroll {
            self.settings_nav_scroll = self.settings_nav_max_scroll;
        }
        if nav.is_empty() {
            canvas.draw_text_styled(
                nav_rect.x + 16,
                nav_rect.y + 16,
                self.tr("没有匹配的设置", "No settings found"),
                t.text_muted,
                nav_rect.w - 28,
                FontStyle::UiSmall,
            );
        } else {
            let mut y = nav_rect.y + 8;
            for item in nav.iter().skip(self.settings_nav_scroll).take(nav_visible) {
                let row = Rect::new(nav_rect.x + 6, y, nav_rect.w - 20, 24);
                let selected = item.page == Some(page);
                let hot = item.page.is_some() && row.contains(hx, hy);
                if selected {
                    canvas.fill_rounded_rect(row, 6, t.editor_selection_bg);
                } else if hot {
                    canvas.fill_rounded_rect(row, 6, t.panel_header_bg);
                }
                let indent = item.depth as i32 * 14;
                if item.page.is_none() {
                    canvas.draw_chevron_down(row.x + 6 + indent, y + 8, t.text_muted);
                }
                let label = match self.ui_lang {
                    UiLanguage::ZhCn => item.zh,
                    UiLanguage::EnUs => item.en,
                };
                let fg = if item.page.is_none() {
                    t.text_secondary
                } else {
                    self.settings_row_fg(selected || hot)
                };
                canvas.draw_text_styled(
                    row.x + 22 + indent,
                    y + 3,
                    label,
                    fg,
                    row.w - 28 - indent,
                    if item.page.is_none() || selected {
                        FontStyle::UiBold
                    } else {
                        FontStyle::UiRegular
                    },
                );
                if let Some(target) = item.page {
                    self.hit_regions
                        .push((row, HitAction::SelectSettingsPage(target)));
                }
                y += row_h;
            }
        }
        canvas.draw_vertical_scrollbar(
            Rect::new(
                nav_rect.x + nav_rect.w - 10,
                nav_rect.y + 6,
                6,
                nav_rect.h - 12,
            ),
            self.settings_nav_scroll,
            nav_visible,
            nav.len(),
            t.border_color,
        );

        let content_x = mx + nav_w + 20;
        let content_w = (mw - nav_w - 40).max(120);
        canvas.draw_text_styled(
            content_x,
            body_top + 12,
            page.breadcrumb(self.ui_lang),
            t.text_muted,
            content_w,
            FontStyle::UiSmall,
        );
        canvas.draw_text_styled(
            content_x,
            body_top + 30,
            page.title(self.ui_lang),
            t.text_primary,
            content_w,
            FontStyle::UiBold,
        );

        let view_y = body_top + 56;
        let view_h = (body_h - 56).max(40);
        if self.modal_scroll > self.settings_content_max_scroll {
            self.modal_scroll = self.settings_content_max_scroll;
        }
        let mut flow = SettingsFlow {
            origin_y: view_y,
            view_h,
            scroll_px: self.modal_scroll as i32 * 28,
            cursor: 0,
        };
        let no_hits = query_active && nav.iter().all(|item| item.page.is_none());
        if no_hits {
            let empty = self.tr("没有匹配的设置", "No settings found");
            self.settings_note(canvas, &mut flow, content_x, content_w, empty);
        } else {
            match page {
                SettingsPage::Appearance => {
                    self.render_settings_appearance(canvas, &mut flow, content_x, content_w, hx, hy)
                }
                SettingsPage::MenusAndToolbars => {
                    self.render_settings_menus(canvas, &mut flow, content_x, content_w, hx, hy)
                }
                SettingsPage::SystemSettings => {
                    self.render_settings_system(canvas, &mut flow, content_x, content_w, hx, hy)
                }
                SettingsPage::EditorGeneral => self.render_settings_editor_general(
                    canvas, &mut flow, content_x, content_w, hx, hy,
                ),
                SettingsPage::EditorFont => {
                    self.render_settings_font(canvas, &mut flow, content_x, content_w, hx, hy)
                }
                SettingsPage::ColorScheme => {
                    self.render_settings_scheme(canvas, &mut flow, content_x, content_w, hx, hy)
                }
                SettingsPage::CodeStyle => {
                    self.render_settings_codestyle(canvas, &mut flow, content_x, content_w, hx, hy)
                }
                SettingsPage::Keymap => {
                    self.render_settings_keymap(canvas, &mut flow, content_x, content_w, hx, hy)
                }
                SettingsPage::Plugins => {
                    self.render_settings_plugins(canvas, &mut flow, content_x, content_w)
                }
                SettingsPage::Git => {
                    self.render_settings_git(canvas, &mut flow, content_x, content_w, hx, hy)
                }
                SettingsPage::Debugger => {
                    self.render_settings_debugger(canvas, &mut flow, content_x, content_w, hx, hy)
                }
                SettingsPage::Terminal => self
                    .render_settings_terminal_page(canvas, &mut flow, content_x, content_w, hx, hy),
                SettingsPage::Languages => {
                    self.render_settings_languages_page(canvas, &mut flow, content_x, content_w)
                }
                SettingsPage::MobileSdk => {
                    self.render_settings_mobile_sdk(
                        canvas, &mut flow, content_x, content_w, hx, hy,
                    )
                }
                SettingsPage::Advanced => {
                    self.render_settings_advanced(canvas, &mut flow, content_x, content_w)
                }
            }
        }
        let total_rows = ((flow.cursor.max(1) + 27) / 28) as usize;
        let visible_rows = (view_h / 28).max(1) as usize;
        self.settings_content_max_scroll = total_rows.saturating_sub(visible_rows);
        if self.modal_scroll > self.settings_content_max_scroll {
            self.modal_scroll = self.settings_content_max_scroll;
        }
        canvas.draw_vertical_scrollbar(
            Rect::new(mx + mw - 14, view_y, 6, view_h),
            self.modal_scroll,
            visible_rows,
            total_rows.max(1),
            t.border_color,
        );

        let footer_y = my + mh - footer_h;
        canvas.fill_rect(Rect::new(mx + 1, footer_y, mw - 2, 1), t.border_color);
        let ok = Rect::new(mx + mw - 92, footer_y + 11, 76, 30);
        let apply = Rect::new(ok.x - 84, footer_y + 11, 76, 30);
        let cancel = Rect::new(apply.x - 84, footer_y + 11, 76, 30);
        let hint = self.tr("更改立即生效", "Changes apply immediately");
        canvas.draw_text_styled(
            mx + 16,
            footer_y + 16,
            hint,
            t.text_muted,
            (cancel.x - mx - 24).max(40),
            FontStyle::UiSmall,
        );
        self.draw_settings_button(
            canvas,
            cancel,
            self.tr("取消", "Cancel"),
            false,
            hx,
            hy,
            HitAction::CloseModalOrPopup,
        );
        self.draw_settings_button(
            canvas,
            apply,
            self.tr("应用", "Apply"),
            false,
            hx,
            hy,
            HitAction::ApplySettings,
        );
        self.draw_settings_button(
            canvas,
            ok,
            self.tr("确定", "OK"),
            true,
            hx,
            hy,
            HitAction::CloseModalOrPopup,
        );
    }
}

fn extract_identifier_after(line: &str, keyword: &str) -> Option<String> {
    let idx = line.find(keyword)?;
    let rest = line[idx + keyword.len()..].trim_start();
    let ident: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if ident.is_empty() {
        None
    } else {
        Some(ident)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

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
        assert_eq!(
            state.cursor_kind_at(ed_cx, ed_cy, w, h),
            IdeCursorKind::Text
        );

        let tm_cx = state.terminal_content_rect.x + state.terminal_content_rect.w / 2;
        let tm_cy = state.terminal_content_rect.y + state.terminal_content_rect.h / 2;
        assert_eq!(
            state.cursor_kind_at(tm_cx, tm_cy, w, h),
            IdeCursorKind::Text
        );

        // 2. 右侧 AI 对话输入框 -> Text
        let ai_cx = state.agent_input_text_rect.x + 20;
        let ai_cy = state.agent_input_text_rect.y + 10;
        assert_eq!(
            state.cursor_kind_at(ai_cx, ai_cy, w, h),
            IdeCursorKind::Text
        );

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
        assert_eq!(
            state.cursor_kind_at(in_cx, in_cy, w, h),
            IdeCursorKind::Text
        );
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
    fn last_file_closes_and_panes_can_move_with_loose_limits() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        assert!(!state.open_buffers.is_empty());
        while !state.open_buffers.is_empty() {
            let before = state.open_buffers.len();
            state.close_tab(0);
            assert!(state.open_buffers.len() < before);
        }
        assert!(state.open_buffers.is_empty());
        state.execute_ide_action(IdeAction::SaveActiveFile);
        assert!(state.status_notification.contains("没有打开") || state.status_notification.contains("No file"));

        let (min_l, max_l) = state.span_limits_for_drag(DockEdge::Left, 1800, 1100);
        assert!(min_l <= 48);
        assert!(max_l >= 900);
        let (min_b, max_b) = state.span_limits_for_drag(DockEdge::Bottom, 1800, 1100);
        assert!(min_b <= 40);
        assert!(max_b >= 800);

        assert_eq!(state.dock_left, DockPane::Project);
        state.swap_dock(DockEdge::Left, DockEdge::Bottom);
        assert_eq!(state.dock_left, DockPane::Tooling);
        assert_eq!(state.dock_bottom, DockPane::Project);
        assert_eq!(state.dock_right, DockPane::Agent);
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
        state
            .active_buffer_mut()
            .insert_str_at_cursor("\npub fn cache_probe_fn() {}\n");
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
        std::fs::write(&main_rs, "fn main() { println!(\"hello\"); }\n").expect("write main.rs");

        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let initial_root = state.workspace_root.clone();
        state.switch_workspace(temp_root.clone());

        let expected_clean = clean_workspace_path(temp_root.clone());
        assert_eq!(state.workspace_root, expected_clean);
        assert_eq!(state.workspace_name(), unique);
        assert_eq!(state.taskbar_title(), format!("{unique} — Rust AI IDE"));
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
    fn window_caption_buttons_activate_only_when_released_on_the_same_button() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1440, 900);
        state.render(&mut canvas);

        let close = state
            .hit_regions
            .iter()
            .find_map(|(rect, action)| match action {
                HitAction::WindowControl(WindowControlCmd::Close) => Some(*rect),
                _ => None,
            })
            .expect("close caption button");
        state.arm_window_control(WindowControlCmd::Close);
        assert_eq!(
            state.release_armed_window_control(close.x + 4, close.y + 4),
            Some(WindowControlCmd::Close)
        );
        assert!(state.armed_window_control.is_none());

        state.arm_window_control(WindowControlCmd::Minimize);
        assert_eq!(state.release_armed_window_control(20, 20), None);
        assert!(state.armed_window_control.is_none());
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
    fn mobile_sdk_settings_page_exposes_install_directories() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1440, 900);
        state.execute_ide_action(IdeAction::OpenSettingsModal);
        state.settings_page = SettingsPage::MobileSdk;
        state.mobile_sdk_flutter.set_text(r"D:\missing-flutter");
        state
            .mobile_sdk_android
            .set_text(r"D:\Android\Sdk-missing");
        state.mobile_sdk_jdk.set_text(r"D:\missing-jdk");
        state.render(&mut canvas);

        assert!(state.hit_regions.iter().any(|(_, action)| {
            matches!(action, HitAction::SelectSettingsPage(SettingsPage::MobileSdk))
        }));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, action)| matches!(action, HitAction::BrowseMobileSdkDir(0))));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, action)| matches!(action, HitAction::BrowseMobileSdkDir(1))));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, action)| matches!(action, HitAction::FocusMobileSdkPath(2))));
        assert!(state.mobile_sdk_field_rects[1].w > 0);
        assert!(state.commit_mobile_sdk_locations_from_inputs());
        assert_eq!(state.mobile_sdk.android_sdk, r"D:\Android\Sdk-missing");
        assert!(!state.commit_mobile_sdk_locations_from_inputs());
    }

    #[test]
    fn settings_dialog_follows_idea_navigation() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1440, 900);
        state.execute_ide_action(IdeAction::OpenSettingsModal);
        assert_eq!(state.settings_page, SettingsPage::Appearance);
        assert_eq!(state.focus, FocusTarget::SettingsModal);
        state.render(&mut canvas);

        assert!(state.hit_regions.iter().any(|(_, action)| {
            matches!(action, HitAction::SelectSettingsPage(SettingsPage::Keymap))
        }));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, action)| matches!(action, HitAction::SelectTheme(_))));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, action)| matches!(action, HitAction::SetUiScale(100))));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, action)| matches!(action, HitAction::ApplySettings)));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, action)| matches!(action, HitAction::CloseModalOrPopup)));

        state.settings_query_input.set_text("终端");
        state.on_active_input_mutated();
        assert_eq!(state.settings_page, SettingsPage::Terminal);
        state.render(&mut canvas);
        assert!(state.hit_regions.iter().any(|(_, action)| {
            matches!(
                action,
                HitAction::SelectSettingsPage(SettingsPage::Terminal)
            )
        }));
        assert!(!state.hit_regions.iter().any(|(_, action)| {
            matches!(action, HitAction::SelectSettingsPage(SettingsPage::Keymap))
        }));
        assert!(state.hit_regions.iter().any(|(_, action)| {
            matches!(
                action,
                HitAction::ExecuteIdeAction(IdeAction::ToggleTerminalWindow)
            )
        }));
    }

    #[test]
    fn terminal_output_links_and_file_paths_are_clickable() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let rel = if std::path::Path::new("src/main.rs").is_file() {
            "src/main.rs"
        } else {
            "crates/ide-app/src/main.rs"
        };
        assert!(
            std::path::Path::new(rel).is_file(),
            "fixture file missing: {rel}"
        );
        let line = format!("docs https://example.com/docs. and {rel}:8");
        {
            let pty = state.terminal.active_pty_mut();
            pty.scroll_offset = 0;
            let mut grid = pty.grid.lock();
            grid.clear_all();
            grid.push_fallback_line(&line, 0xA9B7C6);
        }
        state.bottom_tool = Some(ToolWindowId::Terminal);
        let mut canvas = SurfaceCanvas::new(1440, 900);
        state.render(&mut canvas);

        assert!(state.hit_regions.iter().any(|(_, action)| {
            matches!(action, HitAction::OpenExternalUrl(url) if url == "https://example.com/docs")
        }));
        assert!(state.hit_regions.iter().any(|(_, action)| {
            matches!(
                action,
                HitAction::OpenFileAtLine(path, Some(8)) if path.ends_with("main.rs")
            )
        }));
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
            assert_eq!(
                state.selected_git_commit_hash.as_deref(),
                Some(first_hash.as_str())
            );
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
    fn left_git_stripe_badge_and_idea_checkbox_toggle_without_staging() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1560, 940);
        state.left_tool = Some(ToolWindowId::Commit);
        state.git_engine.has_loaded_snapshot = true;
        state.git_engine.changed_files = vec![
            ide_git::GitChangedFile {
                path: state.workspace_root.join("src/main.rs"),
                status: ide_git::GitFileStatusKind::Modified,
                staged: false,
            },
            ide_git::GitChangedFile {
                path: state.workspace_root.join("notes.txt"),
                status: ide_git::GitFileStatusKind::Untracked,
                staged: false,
            },
        ];
        state
            .git_engine
            .commit_checked_files
            .insert(state.workspace_root.join("src/main.rs"));

        // Capture pixels with 2 changed files vs 0 changed files to verify left stripe badge renders
        state.render(&mut canvas);
        let badge_region_with_changes: Vec<u32> = canvas.pixels()[..1560 * 140].to_vec();

        // IDEA actions & row operations are registered; clicking file row toggles check without staging
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ToggleCommitFileCheck(_))));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::InspectGitWorkingFileDiff(_))));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::RollbackGitFile(_))));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::InspectSelectedGitDiff)));

        // Clicking notes.txt hit region checks it without modifying git `staged`
        let notes_path = state.workspace_root.join("notes.txt");
        let notes_rect = state
            .hit_regions
            .iter()
            .find_map(|(r, a)| match a {
                HitAction::ToggleCommitFileCheck(p) if p == &notes_path => Some(*r),
                _ => None,
            })
            .expect("notes.txt check hit region");
        state.handle_mouse_click_with_mods(notes_rect.x + 10, notes_rect.y + 8, false, &canvas);
        assert!(state.git_engine.is_file_checked_for_commit(&notes_path));
        assert!(state.git_engine.changed_files.iter().all(|f| !f.staged));

        // Clicking again unchecks it
        state.handle_mouse_click_with_mods(notes_rect.x + 10, notes_rect.y + 8, false, &canvas);
        assert!(!state.git_engine.is_file_checked_for_commit(&notes_path));

        // With 0 changed files, the left stripe badge disappears
        state.git_engine.changed_files.clear();
        state.render(&mut canvas);
        assert_ne!(
            badge_region_with_changes,
            canvas.pixels()[..1560 * 140].to_vec()
        );
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
            file_changes: Vec::new(),
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
        assert!(state.hit_regions.iter().any(
            |(_, a)| matches!(a, HitAction::OpenFileAtLine(p, _) if p.ends_with("Cargo.toml"))
        ));

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
        let copied = state
            .copy_focused_selection()
            .expect("chat stream selection");
        assert!(copied.contains("Cargo.toml:3") || copied.contains("verify_agent_ui"));

        let chat_rect = state.agent_chat_area_rect;
        state.open_context_menu_at(chat_rect.x + 12, chat_rect.y + 12, &canvas);
        let menu = state
            .context_menu
            .as_ref()
            .expect("agent chat context menu");
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
        assert!(state
            .image_preview_cache
            .contains_key(&clean_workspace_path(png_path.clone())));

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
        assert!(state.hit_regions.iter().any(
            |(_, a)| matches!(a, HitAction::ToggleComposerPastedSnippet(id) if id == "test-snip-1")
        ));

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
        assert!(state.cached_chat_rows.rows.iter().any(|r| matches!(
            r,
            crate::agent_panel::CachedChatRow::CodeBlockCollapsedSummary { .. }
        )));

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
            .find(|(_, a)| {
                matches!(
                    a,
                    HitAction::SelectAgentProvider(AgentProviderKind::OpenAiCodexCli)
                )
            })
            .cloned()
            .expect("Codex CLI provider option");
        state.handle_mouse_click_with_mods(codex_row.x + 6, codex_row.y + 6, false, &canvas);
        assert_eq!(
            state.agent_hub.active_provider,
            AgentProviderKind::OpenAiCodexCli
        );
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
        state
            .agent_hub
            .messages
            .push(ide_agent_hub::AgentChatMessage {
                role: "user".to_string(),
                agent: AgentProviderKind::CursorCli,
                content: "后台短剧编辑，提审方 appid 默认改为 `wxae3b81ca05d7b0e0`".to_string(),
                thought: None,
                checkpoint_id: None,
                patch: None,
                modified_files: Vec::new(),
                file_changes: Vec::new(),
                attachments: Vec::new(),
            });
        state
            .agent_hub
            .messages
            .push(ide_agent_hub::AgentChatMessage {
                role: "assistant".to_string(),
                agent: AgentProviderKind::CursorCli,
                content: "请重新导入 `Cargo.toml` 查看 **配置项**。".to_string(),
                thought: Some("Checked appid field".to_string()),
                checkpoint_id: None,
                patch: None,
                modified_files: Vec::new(),
                file_changes: Vec::new(),
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

    #[test]
    fn busy_streaming_shows_small_thought_and_execution_rows() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1280, 820);
        state.right_agent_open = true;
        state.right_panel_tab = RightPanelTab::Chat;

        let mut prog = ide_agent_hub::StreamingAgentProgress::with_phase("正在执行工具...");
        prog.thought_text = "The user wants to default the appid field in drama edit.".to_string();
        prog.reply_text = "我先看看编辑页现在怎么填这个字段。".to_string();
        prog.push_execution_step("读取 crates/ide-app/src/agent_panel.rs");
        prog.push_execution_step("搜索 BusyIndicator");
        state
            .agent_hub
            .set_active_streaming_progress(Some(prog.clone()));

        state.render(&mut canvas);
        let rows = &state.cached_chat_rows.rows;
        let thought_row = rows
            .iter()
            .find(|r| matches!(r, crate::agent_panel::CachedChatRow::LiveThoughtLine(t) if t.contains("appid")))
            .expect("small live thought row should be rendered while busy");
        let step_row = rows
            .iter()
            .find(|r| matches!(r, crate::agent_panel::CachedChatRow::LiveStepLine(s) if s.contains("搜索 BusyIndicator")))
            .expect("small live execution step row should be rendered while busy");
        let reply_row = rows
            .iter()
            .find(|r| matches!(r, crate::agent_panel::CachedChatRow::StreamingLine { text, .. } if text.contains("编辑页")))
            .expect("streaming reply row should be rendered alongside thought and steps");
        assert!(rows
            .iter()
            .any(|r| matches!(r, crate::agent_panel::CachedChatRow::BusyIndicator)));
        assert!(thought_row.row_height() < reply_row.row_height());
        assert!(step_row.row_height() < reply_row.row_height());

        // 追加新的执行步骤时应使行缓存失效并刷新展示
        prog.push_execution_step("运行命令 cargo test");
        state.agent_hub.set_active_streaming_progress(Some(prog));
        state.render(&mut canvas);
        assert!(state.cached_chat_rows.rows.iter().any(|r| matches!(
            r,
            crate::agent_panel::CachedChatRow::LiveStepLine(s) if s.contains("运行命令 cargo test")
        )));
    }

    #[test]
    fn cursor_changed_files_drawer_and_edit_rewind_modal_respect_cli_support() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1440, 900);
        state.right_agent_open = true;
        state.right_panel_tab = RightPanelTab::Chat;
        state.agent_hub.active_provider = AgentProviderKind::CursorCli;
        state
            .agent_hub
            .set_provider_capabilities(AgentProviderKind::CursorCli, true, true, true);

        let changed_path = state.workspace_root.join("Cargo.toml");
        let user_msg_idx = state.agent_hub.messages.len();
        state
            .agent_hub
            .messages
            .push(ide_agent_hub::AgentChatMessage {
                role: "user".to_string(),
                agent: AgentProviderKind::CursorCli,
                content: "修改 Cargo.toml  [@main.rs:L1]".to_string(),
                thought: None,
                checkpoint_id: None,
                patch: None,
                modified_files: Vec::new(),
                file_changes: Vec::new(),
                attachments: Vec::new(),
            });
        state
            .agent_hub
            .messages
            .push(ide_agent_hub::AgentChatMessage {
                role: "assistant".to_string(),
                agent: AgentProviderKind::CursorCli,
                content: "已更新依赖配置。".to_string(),
                thought: None,
                checkpoint_id: None,
                patch: None,
                modified_files: vec![changed_path.clone()],
                file_changes: Vec::new(),
                attachments: Vec::new(),
            });
        state
            .agent_hub
            .active_session_mut()
            .changed_files
            .push(ide_agent_hub::SessionFileChange {
                path: changed_path.clone(),
                added_lines: 4,
                removed_lines: 1,
                original_content: Some("orig".to_string()),
                first_turn_msg_idx: user_msg_idx,
            });

        // 1. 默认折叠在输入框上方，支持编辑已发送消息，且不在气泡中重复显示 ModifiedFileChip
        assert!(!state.agent_changed_files_expanded);
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ToggleAgentChangedFilesDrawer)));
        assert!(state.hit_regions.iter().any(
            |(_, a)| matches!(a, HitAction::StartEditSentUserMessage(idx) if *idx == user_msg_idx)
        ));
        assert!(!state.cached_chat_rows.rows.iter().any(|r| matches!(
            r,
            crate::agent_panel::CachedChatRow::TurnChangedFileRow { .. }
        )));
        assert!(!state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::RevertSingleAgentChangedFile(_))));

        // 2. 点击折叠条展开文件更改明细
        let (toggle_r, _) = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::ToggleAgentChangedFilesDrawer))
            .cloned()
            .expect("changed files drawer toggle");
        state.handle_mouse_click_with_mods(toggle_r.x + 6, toggle_r.y + 6, false, &canvas);
        assert!(state.agent_changed_files_expanded);
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::RevertSingleAgentChangedFile(_))));

        // 3. 点击编辑已发送消息 -> 提交后弹出“回退/保留并同步 CLI”弹窗
        let (edit_r, _) = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::StartEditSentUserMessage(idx) if *idx == user_msg_idx))
            .cloned()
            .expect("edit sent user message button");
        state.handle_mouse_click_with_mods(edit_r.x + 4, edit_r.y + 4, false, &canvas);
        assert_eq!(state.editing_sent_msg_idx, Some(user_msg_idx));
        assert_eq!(state.agent_chat_input.as_str(), "修改 Cargo.toml");

        state.submit_agent_prompt("改为仅检查 Cargo.toml".to_string());
        assert_eq!(state.focus, FocusTarget::EditMessageRewindModal);
        state.render(&mut canvas);
        assert!(state.hit_regions.iter().any(|(_, a)| matches!(
            a,
            HitAction::ConfirmEditMessageRewind { revert_files: true }
        )));
        assert!(state.hit_regions.iter().any(|(_, a)| matches!(
            a,
            HitAction::ConfirmEditMessageRewind {
                revert_files: false
            }
        )));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::CancelEditMessageRewindModal)));

        // 点击取消保留当前编辑内容回到输入框
        let (cancel_r, _) = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::CancelEditMessageRewindModal))
            .cloned()
            .expect("cancel edit rewind modal");
        state.handle_mouse_click_with_mods(cancel_r.x + 6, cancel_r.y + 6, false, &canvas);
        assert_eq!(state.focus, FocusTarget::AgentChatInput);
        assert_eq!(state.editing_sent_msg_idx, Some(user_msg_idx));
        assert_eq!(state.agent_chat_input.as_str(), "改为仅检查 Cargo.toml");
        state.cancel_edit_sent_user_message();

        // 4. 所有 CLI 均统一显示文件更改折叠栏（含统计），但不支持回退的 CLI（如 Codex）不显示回退与编辑重发按钮
        state.agent_hub.active_provider = AgentProviderKind::OpenAiCodexCli;
        state.agent_hub.set_provider_capabilities(
            AgentProviderKind::OpenAiCodexCli,
            true,
            true,
            false,
        );
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ToggleAgentChangedFilesDrawer)));
        assert!(!state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::RevertAllAgentChangedFiles)));
        assert!(!state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::RevertSingleAgentChangedFile(_))));
        assert!(!state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::StartEditSentUserMessage(_))));
    }

    #[test]
    fn persistence_restores_projects_tabs_settings_and_ai_history_across_restart() {
        let base_tmp = std::env::temp_dir().join(format!(
            "rust_ai_ide_persist_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let persist_dir = base_tmp.join("appdata");
        let ws1 = base_tmp.join("proj_alpha");
        let ws2 = base_tmp.join("proj_beta");
        std::fs::create_dir_all(ws1.join("src")).unwrap();
        std::fs::create_dir_all(ws2.join("src")).unwrap();
        let ws1_cargo = ws1.join("Cargo.toml");
        let ws1_lib = ws1.join("src").join("lib.rs");
        let ws2_main = ws2.join("src").join("main.rs");
        std::fs::write(&ws1_cargo, "[package]\nname = \"alpha\"\n").unwrap();
        std::fs::write(&ws1_lib, "pub fn a() {}\npub fn b() {}\npub fn c() {}\n").unwrap();
        std::fs::write(&ws2_main, "fn main() {}\n").unwrap();

        let mut state1 = IdeApplicationState::new(ws1.clone());
        state1.restore_from_persistence_dir(persist_dir.clone());
        state1.set_ui_language(UiLanguage::EnUs);
        state1.set_ui_scale(1.25);
        state1.theme = IdeaTheme::from_kind(ThemeKind::IslandsLight);
        state1.attach_active_context = false;
        state1.left_panel_width = 290;
        state1.agent_mode = AgentWorkbenchMode::Ask;
        state1.open_file(ws1_lib.clone());
        state1.active_buffer_mut().set_cursor(2, 4, false);
        state1.scroll_line = 1;
        state1
            .agent_hub
            .messages
            .push(ide_agent_hub::AgentChatMessage {
                role: "user".to_string(),
                agent: AgentProviderKind::CursorCli,
                content: "alpha session question".to_string(),
                thought: None,
                checkpoint_id: None,
                patch: None,
                modified_files: Vec::new(),
                file_changes: Vec::new(),
                attachments: Vec::new(),
            });
        state1.agent_hub.active_session_mut().draft_prompt = "alpha draft".to_string();
        state1.save_all_persisted_state();

        // 切换到第二个项目并保存独立对话历史与打开标签
        state1.switch_workspace(ws2.clone());
        state1.open_file(ws2_main.clone());
        state1
            .agent_hub
            .messages
            .push(ide_agent_hub::AgentChatMessage {
                role: "user".to_string(),
                agent: AgentProviderKind::CursorCli,
                content: "beta session question".to_string(),
                thought: None,
                checkpoint_id: None,
                patch: None,
                modified_files: Vec::new(),
                file_changes: Vec::new(),
                attachments: Vec::new(),
            });
        state1.save_all_persisted_state();

        // 模拟应用重启：先读取全局设置恢复最后打开的项目、UI 设置、布局和最近项目列表
        let saved_gs: PersistedGlobalSettings =
            persistence::load_json(&persistence::global_settings_path(&persist_dir))
                .expect("global settings saved");
        assert_eq!(saved_gs.ui_lang, UiLanguage::EnUs);
        assert!((saved_gs.ui_scale - 1.25).abs() < 0.01);
        assert_eq!(saved_gs.theme_kind, ThemeKind::IslandsLight);
        assert_eq!(
            saved_gs.last_workspace,
            Some(clean_workspace_path(ws2.clone()))
        );
        assert!(saved_gs
            .recent_workspaces
            .contains(&clean_workspace_path(ws1.clone())));

        let mut restarted = IdeApplicationState::new(saved_gs.last_workspace.clone().unwrap());
        restarted.restore_from_persistence_dir(persist_dir.clone());
        assert_eq!(restarted.ui_lang, UiLanguage::EnUs);
        assert!((restarted.ui_scale - 1.25).abs() < 0.01);
        assert_eq!(restarted.theme.kind, ThemeKind::IslandsLight);
        assert!(!restarted.attach_active_context);
        assert_eq!(restarted.left_panel_width, 290);
        assert_eq!(restarted.agent_mode, AgentWorkbenchMode::Ask);
        assert!(restarted
            .agent_hub
            .messages
            .iter()
            .any(|m| m.content == "beta session question"));

        // 切回 alpha 项目应恢复 alpha 的打开文件、光标行列、草稿与 AI 对话历史
        restarted.switch_workspace(ws1.clone());
        assert!(restarted
            .agent_hub
            .messages
            .iter()
            .any(|m| m.content == "alpha session question"));
        assert_eq!(restarted.agent_chat_input.as_str(), "alpha draft");
        assert_eq!(
            restarted.active_buffer().path,
            clean_workspace_path(ws1_lib)
        );
        assert_eq!(restarted.active_buffer().cursor_line, 2);
        assert_eq!(restarted.active_buffer().cursor_col, 4);

        let _ = std::fs::remove_dir_all(&base_tmp);
    }

    #[test]
    fn top_inline_menu_replaces_project_name_and_opens_submenu_on_hover() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1560, 940);
        state.render(&mut canvas);

        assert!(state
            .hit_regions
            .iter()
            .any(|(_, act)| matches!(act, HitAction::ToggleProjectPopup)));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, act)| matches!(act, HitAction::ToggleGitBranchPopup)));
        assert!(state.menu_category_rects.is_empty());

        // Clicking the hamburger icon expands the menu directly into the top toolbar
        // and hides the project / git branch widgets.
        let ham_rect = state
            .hit_regions
            .iter()
            .find(|(_, act)| matches!(act, HitAction::ToggleHamburgerMenu))
            .map(|(r, _)| *r)
            .expect("hamburger button rect");
        state.handle_mouse_click_with_mods(ham_rect.x + 4, ham_rect.y + 4, false, &canvas);
        assert!(state.hamburger_menu_expanded);
        assert!(state.active_menu_category.is_none());

        state.render(&mut canvas);
        assert!(!state
            .hit_regions
            .iter()
            .any(|(_, act)| matches!(act, HitAction::ToggleProjectPopup)));
        assert!(!state
            .hit_regions
            .iter()
            .any(|(_, act)| matches!(act, HitAction::ToggleGitBranchPopup)));
        assert!(!state.menu_category_rects.is_empty());

        for &(_, r) in &state.menu_category_rects {
            assert!(
                r.y >= 0 && r.y + r.h <= 38,
                "expected inline top menu item inside 38px toolbar, got {:?}",
                r
            );
        }

        // Moving the mouse over a category opens its submenu immediately without clicking.
        let (first_cat, first_rect) = state.menu_category_rects[0];
        let (second_cat, second_rect) = state.menu_category_rects[1];
        assert!(state.handle_mouse_move(first_rect.x + 4, first_rect.y + 4));
        assert_eq!(state.active_menu_category, Some(first_cat));

        state.render(&mut canvas);
        assert!(state
            .active_overlay_rects
            .iter()
            .any(|r| r.y == 38 && r.h > 80));

        assert!(state.handle_mouse_move(second_rect.x + 4, second_rect.y + 4));
        assert_eq!(state.active_menu_category, Some(second_cat));
        state.render(&mut canvas);

        // Moving inside the opened dropdown keeps the menu active
        let dd = state.menu_dropdown_rect;
        assert!(dd.w > 100 && dd.h > 80);
        state.handle_mouse_move(dd.x + 20, dd.y + 20);
        assert!(state.hamburger_menu_expanded);
        assert_eq!(state.active_menu_category, Some(second_cat));

        // Moving mouse outside both top menu bar and dropdown restores project & branch title
        assert!(state.handle_mouse_move(dd.x + dd.w + 40, dd.y + 40));
        assert!(!state.hamburger_menu_expanded);
        assert!(state.active_menu_category.is_none());
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, act)| matches!(act, HitAction::ToggleProjectPopup)));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, act)| matches!(act, HitAction::ToggleGitBranchPopup)));

        // Re-open menu, hover first category to open dropdown, then click an editor/menu target -> title restores
        let ham_rect2 = state
            .hit_regions
            .iter()
            .find(|(_, act)| matches!(act, HitAction::ToggleHamburgerMenu))
            .map(|(r, _)| *r)
            .expect("hamburger button rect");
        state.handle_mouse_click_with_mods(ham_rect2.x + 4, ham_rect2.y + 4, false, &canvas);
        state.render(&mut canvas);
        let first_r = state.menu_category_rects[0].1;
        state.handle_mouse_move(first_r.x + 4, first_r.y + 4);
        state.render(&mut canvas);
        assert!(state.hamburger_menu_expanded);

        let ed_x = state.editor_code_rect.x + 20;
        let ed_y = state.editor_code_rect.y + 20;
        state.handle_mouse_click_with_mods(ed_x, ed_y, false, &canvas);
        assert!(!state.hamburger_menu_expanded);
        assert!(state.active_menu_category.is_none());
    }

    #[test]
    fn editor_shows_and_drags_horizontal_scrollbar_when_lines_overflow() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1200, 800);

        // Short lines should not reserve or show a horizontal scrollbar
        let short_buf = TextBuffer::from_text(PathBuf::from("short.rs"), "fn ok() {}\n");
        state.open_buffers.push(short_buf);
        state.active_tab = state.open_buffers.len() - 1;
        state.render(&mut canvas);
        assert_eq!(state.editor_horiz_scroll_info.track.w, 0);

        // A very long line triggers horizontal overflow and renders a draggable track
        let long_line = format!("let s = \"{}\";\n", "x".repeat(320));
        let long_buf = TextBuffer::from_text(PathBuf::from("long.rs"), &long_line);
        state.open_buffers.push(long_buf);
        state.active_tab = state.open_buffers.len() - 1;
        state.render(&mut canvas);

        let info = state.editor_horiz_scroll_info;
        assert!(
            info.total > info.visible && info.track.w > 40 && info.track.h > 0,
            "expected visible horizontal scrollbar info, got {:?}",
            info
        );
        assert!(
            info.track.y >= state.editor_pane_rect.y
                && info.track.y + info.track.h
                    <= state.editor_pane_rect.y + state.editor_pane_rect.h
        );

        let drag_y = info.track.y + info.track.h / 2;
        assert!(state.try_begin_scrollbar_drag(info.track.x + info.track.w / 2, drag_y));
        assert_eq!(
            state.active_scrollbar_drag,
            Some(DragScrollbar::CenterEditorHoriz)
        );
        assert!(state.update_scrollbar_drag(info.track.x + info.track.w - 4, drag_y));
        assert!(
            state.editor_scroll_x > 200,
            "expected editor_scroll_x > 200 after dragging horizontal scrollbar, got {}",
            state.editor_scroll_x
        );
        state.end_scrollbar_drag();

        // Compact scrollbar tracks (e.g. 24px where max_thumb = 20px < 24px) must never panic
        let thumb_v = canvas.draw_vertical_scrollbar(Rect::new(0, 0, 7, 24), 0, 10, 100, 0xFFFFFF);
        assert!(thumb_v.h > 0 && thumb_v.h <= 20);
        let thumb_h =
            canvas.draw_horizontal_scrollbar(Rect::new(0, 0, 24, 7), 0, 10, 100, 0xFFFFFF);
        assert!(thumb_h.w > 0 && thumb_h.w <= 20);
        let mut tiny_canvas = SurfaceCanvas::new(420, 260);
        state.render(&mut tiny_canvas);
    }

    #[test]
    fn sent_user_bubble_preserves_bottom_pixels_and_disables_ctrl_k_and_alt_l_ai_actions() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        state
            .agent_hub
            .messages
            .push(ide_agent_hub::AgentChatMessage {
                role: "user".to_string(),
                agent: AgentProviderKind::CursorCli,
                content: "测试发送文字底部不被截断 gypqj 测试".to_string(),
                thought: None,
                checkpoint_id: None,
                patch: None,
                modified_files: Vec::new(),
                file_changes: Vec::new(),
                attachments: Vec::new(),
            });

        let mut canvas = SurfaceCanvas::new(1400, 900);
        state.render(&mut canvas);

        // User line height and bottom cap height prevent bottom clipping
        let user_line_idx = state
            .cached_chat_rows
            .rows
            .iter()
            .position(|r| matches!(r, crate::agent_panel::CachedChatRow::UserLine { .. }))
            .expect("user line cached row");
        let user_line = &state.cached_chat_rows.rows[user_line_idx];
        let bubble_bottom = &state.cached_chat_rows.rows[user_line_idx + 1];
        assert!(matches!(
            bubble_bottom,
            crate::agent_panel::CachedChatRow::UserBubbleBottom { .. }
        ));
        assert!(user_line.row_height() >= 24);
        assert!(bubble_bottom.row_height() >= 9);

        // Sample pixels inside the bottom 5px band of the user line and confirm
        // glyph pixels exist (not painted over by UserBubbleBottom).
        let (line_rect, ..) = state
            .agent_chat_line_rects
            .iter()
            .find(|(_, _, gidx, _, _)| *gidx == user_line_idx)
            .cloned()
            .expect("user line rect recorded");
        let t = state.theme;
        let bg = blend_rgb(t.panel_bg, t.text_primary, 0.06);
        let bg_r = ((bg >> 16) & 0xFF) as u8;
        let bg_g = ((bg >> 8) & 0xFF) as u8;
        let bg_b = (bg & 0xFF) as u8;
        let mut non_bg_bottom_pixels = 0usize;
        let py_start = line_rect.y + line_rect.h - 6;
        let py_end = line_rect.y + line_rect.h - 1;
        for py in py_start..py_end {
            for px in (line_rect.x + 12)..(line_rect.x + line_rect.w - 12) {
                let idx = (py as usize) * canvas.phys_width + (px as usize);
                if let Some(&p) = canvas.pixels().get(idx) {
                    let pr = ((p >> 16) & 0xFF) as u8;
                    let pg = ((p >> 8) & 0xFF) as u8;
                    let pb = (p & 0xFF) as u8;
                    if pr.abs_diff(bg_r) > 6 || pg.abs_diff(bg_g) > 6 || pb.abs_diff(bg_b) > 6 {
                        non_bg_bottom_pixels += 1;
                    }
                }
            }
        }
        assert!(
            non_bg_bottom_pixels > 0,
            "expected glyph pixels preserved in bottom strip of UserLine"
        );

        // Ctrl+K AI edit & Alt+L ask AI actions and menu entries are disabled
        let before_msgs = state.agent_hub.messages.len();
        state.execute_ide_action(IdeAction::InlineAiEdit);
        state.execute_ide_action(IdeAction::SendSelectionToAgent);
        assert!(state.agent_hub.inline_edit.is_none());
        assert_eq!(state.agent_hub.messages.len(), before_msgs);
        for &cat in MainMenuCategory::all() {
            assert!(!cat.entries().into_iter().any(|entry| matches!(
                entry,
                MenuEntry::Action(IdeAction::InlineAiEdit | IdeAction::SendSelectionToAgent)
            )));
        }
    }

    #[test]
    fn all_clis_show_expandable_turn_file_changes_and_git_diff_opens_in_editor() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let mut canvas = SurfaceCanvas::new(1560, 940);
        state.right_agent_open = true;
        state.right_panel_tab = RightPanelTab::Chat;
        // Use a non-Cursor CLI (Claude Code) where checkpoint rewind is disabled
        state.agent_hub.active_provider = AgentProviderKind::ClaudeCodeCli;
        state.agent_hub.set_provider_capabilities(
            AgentProviderKind::ClaudeCodeCli,
            true,
            true,
            false,
        );

        let target_path = state.workspace_root.join("Cargo.toml");
        let turn_msg_idx = state.agent_hub.messages.len();
        state
            .agent_hub
            .messages
            .push(ide_agent_hub::AgentChatMessage {
                role: "assistant".to_string(),
                agent: AgentProviderKind::ClaudeCodeCli,
                content: "已完成修改。".to_string(),
                thought: None,
                checkpoint_id: None,
                patch: None,
                modified_files: vec![target_path.clone()],
                file_changes: vec![ide_agent_hub::TurnFileChange {
                    path: target_path.clone(),
                    added_lines: 3,
                    removed_lines: 1,
                    before_content: Some("[package]\nname = \"old\"\n".to_string()),
                }],
                attachments: Vec::new(),
            });
        state
            .agent_hub
            .active_session_mut()
            .changed_files
            .push(ide_agent_hub::SessionFileChange {
                path: target_path.clone(),
                added_lines: 3,
                removed_lines: 1,
                original_content: Some("[package]\nname = \"old\"\n".to_string()),
                first_turn_msg_idx: 0,
            });

        state.render(&mut canvas);
        // 1. Unified changed-files bar appears above composer even for non-Cursor CLI
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ToggleAgentChangedFilesDrawer)));
        // 2. Collapsible small-text per-turn file changes toggle appears after assistant reply
        let (toggle_r, _) = state
            .hit_regions
            .iter()
            .find(|(_, a)| {
                matches!(a, HitAction::ToggleMessageFileChangesCollapse(i) if *i == turn_msg_idx)
            })
            .cloned()
            .expect("per-turn file changes collapse toggle");
        assert!(!state.cached_chat_rows.rows.iter().any(|r| matches!(
            r,
            crate::agent_panel::CachedChatRow::TurnChangedFileRow { .. }
        )));

        // Expanding reveals the file row with diff click target
        state.handle_mouse_click_with_mods(toggle_r.x + 8, toggle_r.y + 6, false, &canvas);
        state.render(&mut canvas);
        let (diff_row_r, _) = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(
                a,
                HitAction::InspectAgentFileDiff { path, msg_idx: Some(i) } if path == &target_path && *i == turn_msg_idx
            ))
            .cloned()
            .expect("expanded per-turn file row hit region");

        // Clicking the per-turn file row opens the Diff view directly in the central editor window
        state.handle_mouse_click_with_mods(diff_row_r.x + 12, diff_row_r.y + 6, false, &canvas);
        assert!(state.active_editor_diff.is_some());
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::CloseEditorDiff)));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::OpenDiffSourceFile(_))));

        // Closing Diff returns cleanly to regular editor view
        let (close_r, _) = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::CloseEditorDiff))
            .cloned()
            .expect("close diff button");
        state.handle_mouse_click_with_mods(close_r.x + 4, close_r.y + 4, false, &canvas);
        assert!(state.active_editor_diff.is_none());

        // 3. Clicking a Git working file Diff in Left Commit Panel opens editor diff WITHOUT jumping bottom_tool to GitLog
        state.left_tool = Some(ToolWindowId::Commit);
        state.bottom_tool = Some(ToolWindowId::Terminal);
        state.git_engine.changed_files = vec![ide_git::GitChangedFile {
            path: target_path.clone(),
            status: ide_git::GitFileStatusKind::Modified,
            staged: false,
        }];
        state.git_engine.working_diffs.insert(
            target_path.clone(),
            ide_git::compute_line_diff("a\nb\n", "a\nc\nd\n"),
        );
        state.render(&mut canvas);
        let (git_diff_r, _) = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::InspectGitWorkingFileDiff(p) if p == &target_path))
            .cloned()
            .expect("git working file diff hit region");
        state.handle_mouse_click_with_mods(git_diff_r.x + 6, git_diff_r.y + 6, false, &canvas);
        assert_eq!(state.bottom_tool, Some(ToolWindowId::Terminal));
        assert!(state
            .active_editor_diff
            .as_ref()
            .is_some_and(|d| d.path == target_path && d.added_lines == 2 && d.removed_lines == 1));
    }

    #[test]
    fn test_long_filename_ellipsize_in_tabs_and_tree() {
        let mut canvas = SurfaceCanvas::new(1280, 800);
        let long_name =
            "extremely_long_service_configuration_provider_implementation_details_v2.rs";
        let ell_file = canvas.ellipsize_filename(long_name, 150, FontStyle::UiBold);
        assert!(ell_file.contains('…'));
        assert!(ell_file.ends_with(".rs"));
        assert!(canvas.measure_text(&ell_file, FontStyle::UiBold).0 <= 150);

        let ell_plain = canvas.ellipsize_text(
            "src/very/deep/nested/directory/module_with_long_name/sub_handler.ts",
            140,
            FontStyle::UiSmall,
        );
        assert!(ell_plain.ends_with('…'));
        assert!(canvas.measure_text(&ell_plain, FontStyle::UiSmall).0 <= 140);

        let root = std::env::temp_dir().join(format!(
            "ide_app_ellipsis_test_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&root);
        let long_path = root.join(long_name);
        std::fs::write(&long_path, "pub fn ok() {}\n").unwrap();

        let mut state = IdeApplicationState::new(root.clone());
        state.open_file(long_path);
        state.render(&mut canvas);
        assert!(state.tab_rects.iter().all(|(r, _)| r.w <= 220));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_open_media_files_preview_zoom_source_and_close() {
        let root = std::env::temp_dir().join(format!(
            "ide_app_media_tabs_test_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(root.join("src"));
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

        let svg_path = root.join("extremely_long_vector_icon_asset_preview.svg");
        std::fs::write(
            &svg_path,
            "<svg viewBox=\"0 0 24 24\"><rect x=\"2\" y=\"2\" width=\"20\" height=\"20\" fill=\"#3B82F6\"/></svg>",
        )
        .unwrap();

        let wav_path = root.join("sample_notification_chime.wav");
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&44u32.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&16u32.to_le_bytes());
        for s in [0i16, 10000, -10000, 20000, -20000, 15000, -15000, 5000] {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(&wav_path, &wav).unwrap();

        let mp4_path = root.join("demo_clip_recording.mp4");
        std::fs::write(&mp4_path, b"ftypmp42\0\0\0\0").unwrap();

        let mut state = IdeApplicationState::new(root.clone());
        let mut canvas = SurfaceCanvas::new(1720, 900);

        // 1. Open SVG as visual preview tab
        state.open_file(svg_path.clone());
        assert_eq!(state.active_media_tab, Some(0));
        assert_eq!(state.media_tabs.len(), 1);
        assert!(state.media_tabs[0].preview_image.is_some());
        state.render(&mut canvas);

        let zoom_in_r = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::AdjustMediaZoom(d) if *d > 0))
            .map(|(r, _)| *r)
            .expect("media zoom in hit region");
        state.handle_mouse_click_with_mods(zoom_in_r.x + 4, zoom_in_r.y + 4, false, &canvas);
        assert_eq!(state.media_tabs[0].zoom_pct, 125);

        // SVG offers View Source action that opens its XML in TextBuffer
        let source_r = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::OpenMediaAsText(p) if p == &svg_path))
            .map(|(r, _)| *r)
            .expect("svg view source button");
        state.handle_mouse_click_with_mods(source_r.x + 6, source_r.y + 4, false, &canvas);
        assert!(state.active_media_tab.is_none());
        assert_eq!(state.active_buffer().path, svg_path);

        // 2. Open WAV and MP4 media preview tabs and render both cleanly
        state.open_file(wav_path.clone());
        assert_eq!(state.active_media_tab, Some(1));
        assert!(!state.media_tabs[1].waveform_peaks.is_empty());
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::OpenExternalMedia(p) if p == &wav_path)));

        state.open_file(mp4_path.clone());
        assert_eq!(state.active_media_tab, Some(2));
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::OpenExternalMedia(p) if p == &mp4_path)));

        // Closing a media tab updates selection cleanly
        let close_r = state
            .hit_regions
            .iter()
            .find(|(_, a)| matches!(a, HitAction::CloseMediaTab(2)))
            .map(|(r, _)| *r)
            .expect("close media tab button");
        state.handle_mouse_click_with_mods(close_r.x + 4, close_r.y + 4, false, &canvas);
        assert_eq!(state.media_tabs.len(), 2);
        assert_eq!(state.active_media_tab, Some(1));

        // Selecting a text tab switches back out of media preview
        state.handle_mouse_click_with_mods(
            state.tab_rects[0].0.x + 8,
            state.tab_rects[0].0.y + 8,
            false,
            &canvas,
        );
        assert!(state.active_media_tab.is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_idea_full_git_menu_and_modals_work() {
        let root = std::env::temp_dir().join(format!(
            "ide_app_full_git_menu_test_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("lib.rs"), "pub fn x() -> i32 { 1 }\n").unwrap();

        let mut state = IdeApplicationState::new(root.clone());
        let mut canvas = SurfaceCanvas::new(1400, 860);

        // 1. Full IDEA Git menu renders all operations cleanly
        state.hamburger_menu_expanded = true;
        state.active_menu_category = Some(MainMenuCategory::Git);
        state.render(&mut canvas);
        let expected_actions = [
            IdeAction::GitCommitPopup,
            IdeAction::GitPush,
            IdeAction::GitPull,
            IdeAction::GitFetch,
            IdeAction::GitMerge,
            IdeAction::GitRebase,
            IdeAction::GitBranches,
            IdeAction::GitNewBranch,
            IdeAction::GitNewTag,
            IdeAction::GitStashChanges,
            IdeAction::GitUnstashChanges,
            IdeAction::GitResetHead,
            IdeAction::GitRollback,
            IdeAction::GitShowDiff,
            IdeAction::GitAnnotateBlame,
            IdeAction::GitFileHistory,
            IdeAction::GitRollbackCurrentFile,
            IdeAction::GitShowLog,
            IdeAction::GitManageRemotes,
        ];
        for act in expected_actions {
            assert!(
                state
                    .hit_regions
                    .iter()
                    .any(|(_, a)| matches!(a, HitAction::ExecuteIdeAction(x) if *x == act)),
                "Git menu should expose {act:?}"
            );
        }

        // 2. Git Blame expands gutter and File History sets active file filter in GitLog
        state.execute_ide_action(IdeAction::GitAnnotateBlame);
        assert!(state.git_blame_visible);
        let normal_gutter_start = state.editor_code_start_x;
        state.render(&mut canvas);
        assert!(state.editor_code_start_x > normal_gutter_start + 100);

        state.execute_ide_action(IdeAction::GitFileHistory);
        assert_eq!(state.bottom_tool, Some(ToolWindowId::GitLog));
        assert!(state.git_file_history_filter.is_some());
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ClearGitFileHistoryFilter)));

        // 3. Operation modals (Merge / Rebase / Stash / Unstash / Reset / Rollback / Remotes / Tag) render & close
        for (act, expected_focus) in [
            (IdeAction::GitNewTag, FocusTarget::NewGitTagModal),
            (IdeAction::GitStashChanges, FocusTarget::NewGitStashModal),
            (IdeAction::GitMerge, FocusTarget::GitMergeModal),
            (IdeAction::GitRebase, FocusTarget::GitRebaseModal),
            (IdeAction::GitUnstashChanges, FocusTarget::GitUnstashModal),
            (IdeAction::GitResetHead, FocusTarget::GitResetModal),
            (IdeAction::GitRollback, FocusTarget::GitRollbackModal),
            (IdeAction::GitManageRemotes, FocusTarget::GitRemotesModal),
        ] {
            state.execute_ide_action(act);
            assert_eq!(state.focus, expected_focus);
            state.render(&mut canvas);
        }

        state.execute_ide_action(IdeAction::GitBranches);
        assert!(state.git_branch_popup_open);
        state.render(&mut canvas);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_project_run_configs_cli_command_and_file_run_join_run_list_and_persist() {
        let unique = format!(
            "rust_ai_ide_run_configs_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let root = std::env::temp_dir().join(&unique);
        let persist_dir = root.join(".persist");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&persist_dir).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{ "scripts": { "dev": "vite", "build": "vite build", "test": "vitest" } }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("docker-compose.yml"),
            "services:\n  api:\n    image: alpine\n",
        )
        .unwrap();
        std::fs::write(
            root.join("build.gradle.kts"),
            "plugins { id(\"org.springframework.boot\") }\n",
        )
        .unwrap();
        let py_file = root.join("worker.py");
        std::fs::write(&py_file, "if __name__ == '__main__':\n    print('ok')\n").unwrap();

        let mut state = IdeApplicationState::new(root.clone());
        state.persistence_dir = Some(persist_dir.clone());

        assert!(state
            .run_configs
            .iter()
            .any(|c| c.command == "npm" && c.args == vec!["run", "dev"]));
        assert!(state
            .run_configs
            .iter()
            .any(|c| c.command == "docker" && c.args == vec!["compose", "up", "-d"]));
        assert!(state
            .run_configs
            .iter()
            .any(|c| c.command == "gradle" && c.args == vec!["bootRun"]));

        // Run a file -> joins run list and becomes active
        state.open_file(py_file.clone());
        state.execute_ide_action(IdeAction::RunCurrentFile);
        let py_idx = state
            .run_configs
            .iter()
            .position(|c| c.command == "python" && c.args == vec!["worker.py"])
            .expect("python file added to run list");
        assert_eq!(state.active_run_config, py_idx);
        assert_eq!(state.run_configs[py_idx].source, RunConfigSource::Custom);

        // Add & run a custom CLI command -> joins run list without duplicates
        state.add_and_run_command_line("docker compose up -d --build");
        let count_before = state.run_configs.len();
        state.add_and_run_command_line("docker compose up -d --build");
        assert_eq!(state.run_configs.len(), count_before);

        // Render popup exposes scroll/add/remove hit targets
        state.run_config_popup_open = true;
        let mut canvas = SurfaceCanvas::new(1440, 900);
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::OpenNewRunConfigModal)));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::RemoveRunConfig(_))));

        // Restore persisted custom configs in a fresh state
        let mut restored = IdeApplicationState::new(root.clone());
        restored.persistence_dir = Some(persist_dir.clone());
        restored.restore_workspace_state(&persist_dir);
        assert!(restored
            .run_configs
            .iter()
            .any(|c| c.command == "python" && c.args == vec!["worker.py"]));
        assert!(restored
            .run_configs
            .iter()
            .any(|c| c.command == "docker" && c.args.contains(&"--build".to_string())));

        let custom_idx = restored
            .run_configs
            .iter()
            .position(|c| c.source == RunConfigSource::Custom)
            .unwrap();
        restored.remove_custom_run_config(custom_idx);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_single_process_new_window_queue_and_cache_trimming() {
        let unique = format!(
            "rust_ai_ide_multiwin_mem_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let root1 = std::env::temp_dir().join(format!("{unique}_1"));
        let root2 = std::env::temp_dir().join(format!("{unique}_2"));
        std::fs::create_dir_all(&root1).unwrap();
        std::fs::create_dir_all(&root2).unwrap();

        let mut state = IdeApplicationState::new(root1.clone());
        state.open_workspace_in_new_window(root2.clone());
        state.open_workspace_in_new_window(root2.clone());
        assert_eq!(state.pending_new_windows.len(), 2);
        assert!(workspace_paths_equal(
            &state.pending_new_windows[0],
            &clean_workspace_path(root2.clone())
        ));
        state.request_switch_workspace(root1.clone());
        assert!(workspace_paths_equal(
            state
                .pending_switch_workspaces
                .last()
                .expect("switch queued"),
            &state.workspace_root
        ));

        let now = Instant::now();
        let mut recent: Option<(u8, u32, Instant)> = None;
        let first = duplicate_open_count(&recent, &1u8, now);
        assert_eq!(first, 1);
        recent = Some((1, first, now));
        assert_eq!(duplicate_open_count(&recent, &1u8, now), 2);
        assert_eq!(duplicate_open_count(&recent, &2u8, now), 1);
        assert_eq!(
            duplicate_open_count(&recent, &1u8, now + Duration::from_secs(9)),
            1
        );
        state.start_window_attention_flash();
        assert!(state.window_attention_until.is_some());
        assert!(state.has_active_animations());

        // Image preview cache should stay bounded (<= 24) and trim when minimized
        for i in 0..30usize {
            let fake = DecodedPreviewImage {
                orig_width: 16,
                orig_height: 16,
                width: 4,
                height: 4,
                rgba: vec![255; 64],
            };
            state.cache_decoded_image_preview(root1.join(format!("img_{i}.png")), Arc::new(fake));
        }
        assert!(state.image_preview_cache.len() <= 24);
        state.trim_caches_when_minimized();
        assert!(state.image_preview_cache.is_empty());

        let _ = std::fs::remove_dir_all(&root1);
        let _ = std::fs::remove_dir_all(&root2);
    }

    #[test]
    fn test_per_script_run_pause_isolation() {
        let unique = format!(
            "rust_ai_ide_per_script_pause_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let root = std::env::temp_dir().join(&unique);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{ "scripts": { "dev": "node -e \"setTimeout(()=>{}, 5000)\"", "build": "node -e \"setTimeout(()=>{}, 5000)\"" } }"#,
        )
        .unwrap();

        let mut state = IdeApplicationState::new(root.clone());
        let dev_idx = state
            .run_configs
            .iter()
            .position(|c| c.args == vec!["run", "dev"])
            .expect("npm dev");
        let build_idx = state
            .run_configs
            .iter()
            .position(|c| c.args == vec!["build"] || c.args == vec!["run", "build"])
            .expect("npm build");

        state.toggle_run_config_execution(dev_idx);
        assert!(state.is_run_config_running(dev_idx));
        assert!(!state.is_run_config_running(build_idx));
        assert!(state.active_run_config_is_running());

        // Switching active dropdown selection to idle script must NOT show it as running
        state.active_run_config = build_idx;
        assert!(!state.active_run_config_is_running());

        // Start second script independently; both run simultaneously
        state.toggle_run_config_execution(build_idx);
        assert!(state.is_run_config_running(dev_idx));
        assert!(state.is_run_config_running(build_idx));

        // Render popup & verify independent per-row ToggleRunConfigAt buttons
        state.run_config_popup_open = true;
        let mut canvas = SurfaceCanvas::new(1440, 900);
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ToggleRunConfigAt(i) if *i == dev_idx)));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ToggleRunConfigAt(i) if *i == build_idx)));

        // Pausing dev script must NOT stop build script
        state.toggle_run_config_execution(dev_idx);
        assert!(!state.is_run_config_running(dev_idx));
        assert!(state.is_run_config_running(build_idx));

        state.toggle_run_config_execution(build_idx);
        assert!(!state.is_run_config_running(build_idx));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_mobile_flutter_android_toolbar_missing_sdk_and_device_selection() {
        let unique = format!(
            "rust_ai_ide_mobile_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let root = std::env::temp_dir().join(&unique);
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::create_dir_all(root.join("android/app/src/main")).unwrap();
        std::fs::write(
            root.join("pubspec.yaml"),
            "name: shop_mobile\ndependencies:\n  flutter:\n    sdk: flutter\n",
        )
        .unwrap();
        std::fs::write(root.join("lib/main.dart"), "void main() {}\n").unwrap();
        std::fs::write(
            root.join("android/app/src/main/AndroidManifest.xml"),
            "<manifest/>\n",
        )
        .unwrap();

        let mut state = IdeApplicationState::new(root.clone());
        assert_eq!(
            state.mobile_env.project_kind,
            MobileProjectKind::FlutterAndAndroid
        );
        assert!(state
            .run_configs
            .iter()
            .any(|c| c.command == "flutter" && c.args == vec!["run"]));

        // Simulate missing Flutter & Android SDK + 1 connected online ADB device
        let report = MobileEnvironmentReport {
            project_kind: MobileProjectKind::FlutterAndAndroid,
            sdk_tools: vec![
                ide_core::SdkToolStatus {
                    id: "flutter".into(),
                    label: "Flutter SDK".into(),
                    installed: false,
                    required: true,
                    version_or_detail: "Missing".into(),
                    path: None,
                    install_hint: "winget install Google.Flutter".into(),
                    install_cmd: Some("winget install Google.Flutter".into()),
                    download_url: "https://docs.flutter.dev/get-started/install/windows".into(),
                },
                ide_core::SdkToolStatus {
                    id: "adb".into(),
                    label: "ADB (Platform-Tools)".into(),
                    installed: true,
                    required: true,
                    version_or_detail: "35.0.0".into(),
                    path: Some(PathBuf::from("C:\\Android\\adb.exe")),
                    install_hint: String::new(),
                    install_cmd: None,
                    download_url: "https://developer.android.com/tools/releases/platform-tools"
                        .into(),
                },
            ],
            devices: vec![ide_core::MobileDeviceInfo {
                id: "R5CR80ABCDE".into(),
                name: "SM_S9180".into(),
                platform: "android".into(),
                state: MobileDeviceState::Online,
                is_emulator: false,
                detail: "Android (USB)".into(),
            }],
            flutter_bin: None,
            adb_bin: Some(PathBuf::from("C:\\Android\\adb.exe")),
            android_sdk_root: None,
            emulator_bin: None,
            probing: false,
            last_checked_note: "mock probed".into(),
        };

        state.apply_mobile_environment_report(report);
        assert!(state.mobile_popup_open);
        assert_eq!(state.selected_mobile_device_id.as_deref(), Some("R5CR80ABCDE"));

        let mut canvas = SurfaceCanvas::new(1440, 900);
        state.render(&mut canvas);
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::ToggleMobilePopup)));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::SelectMobileDevice(id) if id == "R5CR80ABCDE")));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::InstallMobileSdkCmd(n, _) if n == "Flutter SDK")));
        assert!(state
            .hit_regions
            .iter()
            .any(|(_, a)| matches!(a, HitAction::OpenMobileDownloadUrl(_))));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sent_user_bubble_widens_and_contains_pasted_code_blocks() {
        let mut state = IdeApplicationState::new(PathBuf::from("."));
        let long_code = "fn very_long_rust_helper_function_name_for_mobile_and_desktop(parameter_one: &str, parameter_two: usize, parameter_three: bool) -> String { format!(\"{parameter_one}:{parameter_two}:{parameter_three}\") }";
        let msg_idx = state.agent_hub.messages.len();
        state
            .agent_hub
            .messages
            .push(ide_agent_hub::AgentChatMessage {
                role: "user".to_string(),
                agent: AgentProviderKind::CursorCli,
                content: format!(
                    "请检查这段复制的代码  [@main.rs:L10]\n\n```rust\n{long_code}\nlet second = 42;\n```"
                ),
                thought: None,
                checkpoint_id: None,
                patch: None,
                modified_files: Vec::new(),
                file_changes: Vec::new(),
                attachments: Vec::new(),
            });

        // Expand code block inside user message (collapsed by default) to inspect wrapped code lines
        state.collapsed_code_blocks.insert((msg_idx, 0), false);
        let mut canvas = SurfaceCanvas::new(1400, 900);
        state.render(&mut canvas);

        let chat_area = state.agent_chat_area_rect;
        let expected_row_w = (chat_area.w - 20).max(136);
        let bubble_w = state
            .cached_chat_rows
            .rows
            .iter()
            .find_map(|r| match r {
                crate::agent_panel::CachedChatRow::UserBubbleTop { bubble_w } => Some(*bubble_w),
                _ => None,
            })
            .expect("user bubble top");
        assert!(
            bubble_w >= expected_row_w - 2,
            "user bubble with pasted code should widen to full chat row width (got {bubble_w}, expected >= {})",
            expected_row_w - 2
        );

        let right_edge_x = chat_area.x + chat_area.w - 14;
        let bubble_left = (right_edge_x - bubble_w).max(chat_area.x + 6);
        let bubble_right = bubble_left + bubble_w;

        let mut code_line_count = 0usize;
        for (idx, row) in state.cached_chat_rows.rows.iter().enumerate() {
            if let crate::agent_panel::CachedChatRow::CodeBlockLine {
                text,
                user_bubble_w,
                ..
            } = row
            {
                code_line_count += 1;
                assert_eq!(*user_bubble_w, Some(bubble_w));
                let (crect, text_x, ..) = state
                    .agent_chat_line_rects
                    .iter()
                    .find(|(_, _, gidx, _, _)| *gidx == idx)
                    .cloned()
                    .expect("code line rect recorded");
                assert!(
                    crect.x >= bubble_left + 8 && crect.x + crect.w <= bubble_right - 8,
                    "code card {crect:?} overflowed user bubble [{bubble_left}..{bubble_right}]"
                );
                let (tw, _) = canvas.measure_text(text, crate::canvas::FontStyle::ChatCode);
                assert!(
                    text_x + tw <= crect.x + crect.w - 8,
                    "wrapped code segment `{text}` width {tw} overflowed code card right edge"
                );
            }
        }
        assert!(code_line_count >= 2, "long code line should wrap neatly inside wide bubble");
    }
}
