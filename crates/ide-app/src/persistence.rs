use crate::ide_state::{DockPane, RightPanelTab, SettingsPage};
use ide_agent_hub::{AgentProviderKind, AgentWorkbenchMode, AiApiConfig, ReasoningEffort};
use ide_core::{KeymapScheme, MobileSdkLocations, ThemeKind, ToolWindowId, UiLanguage};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// 全局本地化存储根目录（Windows: `%APPDATA%\rust-ai-ide`，Unix: `~/.config/rust-ai-ide`）
pub fn default_storage_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .or_else(|| std::env::var_os("LOCALAPPDATA"))
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|p| PathBuf::from(p).join("rust-ai-ide"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|p| p.join("rust-ai-ide"))
    }
}

/// 生成跨重启稳定的工作区存储键（FNV-1a 64-bit，避免写入目标项目目录污染 git）
pub fn workspace_storage_key(workspace_root: &Path) -> String {
    let raw = workspace_root.to_string_lossy().replace('\\', "/");
    let trimmed = raw.trim_end_matches('/');
    #[cfg(target_os = "windows")]
    let norm = trimmed.to_ascii_lowercase();
    #[cfg(not(target_os = "windows"))]
    let norm = trimmed.to_string();

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in norm.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

pub fn global_settings_path(storage_dir: &Path) -> PathBuf {
    storage_dir.join("settings.json")
}

pub fn workspace_state_path(storage_dir: &Path, workspace_root: &Path) -> PathBuf {
    storage_dir.join("workspaces").join(format!(
        "{}.workspace.json",
        workspace_storage_key(workspace_root)
    ))
}

pub fn agent_history_path(storage_dir: &Path, workspace_root: &Path) -> PathBuf {
    storage_dir.join("workspaces").join(format!(
        "{}.agent.json",
        workspace_storage_key(workspace_root)
    ))
}

pub fn load_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<T>(&raw).ok()
}

/// Remember every workspace window so the next launch can reopen them all.
pub fn store_open_windows(storage_dir: &Path, paths: &[PathBuf]) {
    let path = global_settings_path(storage_dir);
    let mut gs = load_json::<PersistedGlobalSettings>(&path).unwrap_or_default();
    gs.open_windows = paths.to_vec();
    if let Some(first) = paths.iter().find(|p| p.is_dir()) {
        gs.last_workspace = Some(first.clone());
    }
    save_json(&path, &gs);
}

pub fn save_json<T: Serialize>(path: &Path, value: &T) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(value) {
        let _ = fs::write(path, json);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedProviderPref {
    pub kind: AgentProviderKind,
    pub active_model: String,
    pub fast_mode: bool,
    pub reasoning_effort: ReasoningEffort,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedGlobalSettings {
    pub ui_lang: UiLanguage,
    pub ui_scale: f32,
    pub theme_kind: ThemeKind,
    pub keymap_scheme: KeymapScheme,
    pub settings_page: SettingsPage,
    pub last_workspace: Option<PathBuf>,
    pub recent_workspaces: Vec<PathBuf>,
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
    pub window_maximized: bool,
    pub window_width: i32,
    pub window_height: i32,
    /// Workspaces that were open when the app last quit. Restored together on the next launch.
    pub open_windows: Vec<PathBuf>,
    pub attach_active_context: bool,
    pub agent_mode: AgentWorkbenchMode,
    pub active_provider: AgentProviderKind,
    pub provider_prefs: Vec<PersistedProviderPref>,
    pub ai_api_config: Option<AiApiConfig>,
    pub mobile_sdk: MobileSdkLocations,
}

impl Default for PersistedGlobalSettings {
    fn default() -> Self {
        Self {
            ui_lang: UiLanguage::default(),
            ui_scale: 1.0,
            theme_kind: ThemeKind::IslandsDark,
            keymap_scheme: KeymapScheme::IntelliJIdea,
            settings_page: SettingsPage::Appearance,
            last_workspace: None,
            recent_workspaces: Vec::new(),
            left_tool: Some(ToolWindowId::Project),
            left_panel_width: 248,
            right_agent_open: true,
            right_panel_width: 420,
            right_panel_tab: RightPanelTab::Chat,
            bottom_tool: None,
            bottom_panel_height: 220,
            dock_left: DockPane::Project,
            dock_right: DockPane::Agent,
            dock_bottom: DockPane::Tooling,
            window_maximized: false,
            window_width: 1560,
            window_height: 940,
            open_windows: Vec::new(),
            attach_active_context: true,
            agent_mode: AgentWorkbenchMode::Agent,
            active_provider: AgentProviderKind::CursorCli,
            provider_prefs: Vec::new(),
            ai_api_config: None,
            mobile_sdk: MobileSdkLocations::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PersistedOpenTab {
    pub path: PathBuf,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

impl Default for PersistedOpenTab {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            cursor_line: 0,
            cursor_col: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedWorkspaceState {
    pub workspace_root: PathBuf,
    pub open_files: Vec<PersistedOpenTab>,
    pub active_tab_path: Option<PathBuf>,
    pub scroll_line: usize,
    pub editor_scroll_x: i32,
    pub active_run_config: usize,
    pub active_run_config_name: Option<String>,
    pub custom_run_configs: Vec<ide_core::RunConfiguration>,
    pub git_commit_draft: String,
}
