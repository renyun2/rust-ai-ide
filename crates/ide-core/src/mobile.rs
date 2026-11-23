use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Detected mobile development project type in the current workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MobileProjectKind {
    #[default]
    None,
    Flutter,
    AndroidNative,
    FlutterAndAndroid,
}

impl MobileProjectKind {
    pub fn is_mobile(self) -> bool {
        self != Self::None
    }

    pub fn uses_flutter(self) -> bool {
        matches!(self, Self::Flutter | Self::FlutterAndAndroid)
    }

    pub fn uses_android(self) -> bool {
        matches!(
            self,
            Self::AndroidNative | Self::FlutterAndAndroid | Self::Flutter
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MobileDeviceState {
    Online,
    Unauthorized,
    Offline,
    EmulatorAvd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MobileDeviceInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub state: MobileDeviceState,
    pub is_emulator: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkToolStatus {
    pub id: String,
    pub label: String,
    pub installed: bool,
    pub required: bool,
    pub version_or_detail: String,
    pub path: Option<PathBuf>,
    pub install_hint: String,
    pub install_cmd: Option<String>,
    pub download_url: String,
}

#[derive(Debug, Clone, Default)]
pub struct MobileEnvironmentReport {
    pub project_kind: MobileProjectKind,
    pub sdk_tools: Vec<SdkToolStatus>,
    pub devices: Vec<MobileDeviceInfo>,
    pub flutter_bin: Option<PathBuf>,
    pub adb_bin: Option<PathBuf>,
    pub android_sdk_root: Option<PathBuf>,
    pub emulator_bin: Option<PathBuf>,
    pub probing: bool,
    pub last_checked_note: String,
}

impl MobileEnvironmentReport {
    pub fn missing_required_tools(&self) -> Vec<&SdkToolStatus> {
        self.sdk_tools
            .iter()
            .filter(|t| t.required && !t.installed)
            .collect()
    }

    pub fn online_devices(&self) -> Vec<&MobileDeviceInfo> {
        self.devices
            .iter()
            .filter(|d| d.state == MobileDeviceState::Online)
            .collect()
    }
}

/// Detects whether `workspace_root` (or immediate subdirectories) is a Flutter / Android mobile project.
pub fn detect_mobile_project(workspace_root: &Path) -> MobileProjectKind {
    let mut has_flutter = dir_has_flutter_pubspec(workspace_root);
    let mut has_android = dir_has_android_project(workspace_root);

    if let Ok(entries) = std::fs::read_dir(workspace_root) {
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(
                name.as_str(),
                ".git" | "node_modules" | "target" | "build" | ".dart_tool" | ".gradle"
            ) {
                continue;
            }
            if !has_flutter && dir_has_flutter_pubspec(&p) {
                has_flutter = true;
            }
            if !has_android && dir_has_android_project(&p) {
                has_android = true;
            }
        }
    }

    match (has_flutter, has_android) {
        (true, true) => MobileProjectKind::FlutterAndAndroid,
        (true, false) => MobileProjectKind::Flutter,
        (false, true) => MobileProjectKind::AndroidNative,
        (false, false) => MobileProjectKind::None,
    }
}

pub fn dir_has_flutter_pubspec(dir: &Path) -> bool {
    let pubspec = dir.join("pubspec.yaml");
    if let Ok(raw) = std::fs::read_to_string(pubspec) {
        let lower = raw.to_ascii_lowercase();
        return lower.contains("sdk: flutter") || lower.contains("\nflutter:") || lower.starts_with("flutter:");
    }
    false
}

pub fn dir_has_android_project(dir: &Path) -> bool {
    let manifests = [
        dir.join("AndroidManifest.xml"),
        dir.join("src/main/AndroidManifest.xml"),
        dir.join("app/src/main/AndroidManifest.xml"),
        dir.join("android/app/src/main/AndroidManifest.xml"),
    ];
    if manifests.iter().any(|p| p.is_file()) {
        return true;
    }
    for gradle_file in [
        dir.join("build.gradle"),
        dir.join("build.gradle.kts"),
        dir.join("app/build.gradle"),
        dir.join("app/build.gradle.kts"),
        dir.join("android/build.gradle"),
        dir.join("android/build.gradle.kts"),
    ] {
        if let Ok(raw) = std::fs::read_to_string(gradle_file) {
            if raw.contains("com.android.application")
                || raw.contains("com.android.library")
                || raw.contains("android {")
            {
                return true;
            }
        }
    }
    false
}

/// Parses `adb devices -l` stdout into structured device descriptors.
pub fn parse_adb_devices_output(output: &str) -> Vec<MobileDeviceInfo> {
    let mut devices = Vec::new();
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty()
            || line.starts_with("List of devices")
            || line.starts_with("* daemon")
        {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(id) = parts.next() else {
            continue;
        };
        let Some(state_tok) = parts.next() else {
            continue;
        };
        let state = match state_tok {
            "device" => MobileDeviceState::Online,
            "unauthorized" => MobileDeviceState::Unauthorized,
            "offline" => MobileDeviceState::Offline,
            _ => continue,
        };
        let rest_tokens: Vec<&str> = parts.collect();
        let detail = rest_tokens.join(" ");
        let model = rest_tokens
            .iter()
            .find_map(|tok| tok.strip_prefix("model:"))
            .map(|m| m.replace('_', " "))
            .filter(|m| !m.is_empty());
        let is_emulator = id.starts_with("emulator-")
            || model
                .as_deref()
                .is_some_and(|m| m.contains("sdk_gphone") || m.contains("Emulator"));
        let name = match (model, state) {
            (Some(m), _) => m,
            (None, MobileDeviceState::Unauthorized) => format!("{id} (未授权 USB 调试)"),
            (None, _) => id.to_string(),
        };
        devices.push(MobileDeviceInfo {
            id: id.to_string(),
            name,
            platform: "Android".to_string(),
            state,
            is_emulator,
            detail,
        });
    }
    devices
}

/// Parses `flutter devices --machine` JSON array output.
pub fn parse_flutter_devices_json(output: &str) -> Vec<MobileDeviceInfo> {
    let trimmed = output.trim();
    let json_start = trimmed.find('[');
    let Some(start) = json_start else {
        return Vec::new();
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&trimmed[start..]) else {
        return Vec::new();
    };
    let Some(arr) = val.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr {
        let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(id)
            .to_string();
        let platform = item
            .get("targetPlatform")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let is_emulator = item
            .get("emulator")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        out.push(MobileDeviceInfo {
            id: id.to_string(),
            name,
            platform,
            state: MobileDeviceState::Online,
            is_emulator,
            detail: String::new(),
        });
    }
    out
}

/// Parses `emulator -list-avds` stdout into bootable AVD entries.
pub fn parse_emulator_avds_output(output: &str) -> Vec<MobileDeviceInfo> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("INFO") && !l.starts_with("ERROR"))
        .map(|avd| MobileDeviceInfo {
            id: avd.to_string(),
            name: format!("AVD: {avd}"),
            platform: "Android Emulator".to_string(),
            state: MobileDeviceState::EmulatorAvd,
            is_emulator: true,
            detail: "Android Virtual Device".to_string(),
        })
        .collect()
}

/// Install directories chosen in Settings. Empty fields fall back to auto-detection.
/// A non-empty path that is missing or incomplete is reported as `SDK 未安装` and is not replaced by PATH.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MobileSdkLocations {
    /// Flutter SDK root (contains `bin/flutter` or `bin/flutter.bat`).
    pub flutter_sdk: String,
    /// Android SDK root (`ANDROID_HOME`).
    pub android_sdk: String,
    /// JDK home (`JAVA_HOME`).
    pub jdk_home: String,
}

pub const SDK_NOT_INSTALLED_LABEL: &str = "SDK 未安装";

enum ConfiguredSdk {
    Auto,
    Found(PathBuf),
    Missing(PathBuf),
}

fn nonempty_configured_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn missing_sdk_detail(path: Option<&Path>) -> String {
    match path {
        Some(p) => format!("{SDK_NOT_INSTALLED_LABEL}：{}", p.display()),
        None => SDK_NOT_INSTALLED_LABEL.to_string(),
    }
}

fn missing_sdk_hint(path: Option<&Path>) -> String {
    match path {
        Some(p) => format!(
            "{SDK_NOT_INSTALLED_LABEL}：{}。可在设置 → 语言和框架 → 移动端 SDK 修改安装目录",
            p.display()
        ),
        None => format!(
            "{SDK_NOT_INSTALLED_LABEL}。请在设置 → 语言和框架 → 移动端 SDK 中指定安装目录"
        ),
    }
}

fn flutter_executable_in(root: &Path) -> Option<PathBuf> {
    if root.is_file() {
        let name = root.file_name()?.to_string_lossy().to_ascii_lowercase();
        if matches!(
            name.as_str(),
            "flutter" | "flutter.bat" | "flutter.cmd" | "flutter.exe"
        ) {
            return Some(root.to_path_buf());
        }
        return None;
    }
    for rel in [
        "bin/flutter.bat",
        "bin/flutter.cmd",
        "bin/flutter.exe",
        "bin/flutter",
        "flutter.bat",
        "flutter.cmd",
        "flutter.exe",
        "flutter",
    ] {
        let candidate = root.join(rel);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn flutter_sdk_root_from_bin(bin: &Path) -> PathBuf {
    bin.parent()
        .filter(|dir| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("bin"))
        })
        .and_then(|dir| dir.parent())
        .unwrap_or(bin)
        .to_path_buf()
}

fn locate_flutter_sdk(configured: &str) -> ConfiguredSdk {
    let Some(path) = nonempty_configured_path(configured) else {
        return ConfiguredSdk::Auto;
    };
    match flutter_executable_in(&path) {
        Some(bin) => ConfiguredSdk::Found(flutter_sdk_root_from_bin(&bin)),
        None => ConfiguredSdk::Missing(path),
    }
}

fn android_sdk_looks_valid(root: &Path) -> bool {
    root.is_dir()
        && [
            "platform-tools",
            "platforms",
            "build-tools",
            "cmdline-tools",
            "emulator",
        ]
        .iter()
        .any(|rel| root.join(rel).is_dir())
}

fn locate_android_sdk(configured: &str) -> ConfiguredSdk {
    let Some(path) = nonempty_configured_path(configured) else {
        return ConfiguredSdk::Auto;
    };
    if android_sdk_looks_valid(&path) {
        ConfiguredSdk::Found(path)
    } else {
        ConfiguredSdk::Missing(path)
    }
}

fn jdk_java_in(root: &Path) -> Option<PathBuf> {
    if root.is_file() {
        let name = root.file_name()?.to_string_lossy().to_ascii_lowercase();
        if matches!(name.as_str(), "java" | "java.exe") {
            return Some(root.to_path_buf());
        }
        return None;
    }
    for rel in ["bin/java.exe", "bin/java"] {
        let candidate = root.join(rel);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn jdk_home_from_java(bin: &Path) -> PathBuf {
    bin.parent()
        .filter(|dir| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("bin"))
        })
        .and_then(|dir| dir.parent())
        .unwrap_or(bin)
        .to_path_buf()
}

fn locate_jdk(configured: &str) -> ConfiguredSdk {
    let Some(path) = nonempty_configured_path(configured) else {
        return ConfiguredSdk::Auto;
    };
    match jdk_java_in(&path) {
        Some(bin) => ConfiguredSdk::Found(jdk_home_from_java(&bin)),
        None => ConfiguredSdk::Missing(path),
    }
}

/// Probes local SDK installations (`Flutter`, `Android SDK`, `ADB`, `JDK`) and connected phones/emulators.
/// `locations` supplies install directories from Settings. A configured path that does not contain the SDK
/// is reported as not installed and does not fall back to PATH or `ANDROID_HOME`.
pub fn probe_mobile_environment(
    workspace_root: &Path,
    project_kind: MobileProjectKind,
    locations: &MobileSdkLocations,
) -> MobileEnvironmentReport {
    let flutter_loc = locate_flutter_sdk(&locations.flutter_sdk);
    let android_loc = locate_android_sdk(&locations.android_sdk);
    let jdk_loc = locate_jdk(&locations.jdk_home);

    let flutter_bin = match &flutter_loc {
        ConfiguredSdk::Found(root) => flutter_executable_in(root),
        ConfiguredSdk::Auto => detect_flutter_binary(workspace_root),
        ConfiguredSdk::Missing(_) => None,
    };
    let android_sdk_root = match &android_loc {
        ConfiguredSdk::Found(root) => Some(root.clone()),
        ConfiguredSdk::Auto => detect_android_sdk_root(workspace_root),
        ConfiguredSdk::Missing(_) => None,
    };
    let adb_bin = detect_adb_binary(android_sdk_root.as_deref());
    let emulator_bin = detect_emulator_binary(android_sdk_root.as_deref());
    let sdkmanager_bin = detect_sdkmanager_binary(android_sdk_root.as_deref());

    let mut sdk_tools = Vec::new();

    // 1. Flutter SDK
    let flutter_missing_path = match &flutter_loc {
        ConfiguredSdk::Missing(path) => Some(path.as_path()),
        _ => None,
    };
    if project_kind.uses_flutter() || flutter_bin.is_some() || flutter_missing_path.is_some() {
        let (installed, version_or_detail, install_hint, path) = if let Some(ref fbin) = flutter_bin {
            let ver = run_version_line(fbin, &["--version", "--suppress-analytics"])
                .unwrap_or_else(|| "Flutter SDK installed".to_string());
            (
                true,
                ver,
                "运行 winget install Google.Flutter 或在设置中指定 Flutter SDK 安装目录".to_string(),
                Some(fbin.clone()),
            )
        } else {
            (
                false,
                missing_sdk_detail(flutter_missing_path),
                missing_sdk_hint(flutter_missing_path),
                flutter_missing_path.map(Path::to_path_buf),
            )
        };
        sdk_tools.push(SdkToolStatus {
            id: "flutter".to_string(),
            label: "Flutter SDK".to_string(),
            installed,
            required: project_kind.uses_flutter(),
            version_or_detail,
            path,
            install_hint,
            install_cmd: Some("winget install --id Google.Flutter -e".to_string()),
            download_url: "https://docs.flutter.dev/get-started/install/windows".to_string(),
        });
    }

    // 2. Android SDK root
    let android_installed = android_sdk_root.is_some();
    let platforms = android_sdk_root
        .as_deref()
        .map(list_installed_android_platforms)
        .unwrap_or_default();
    let build_tools = android_sdk_root
        .as_deref()
        .map(list_installed_build_tools)
        .unwrap_or_default();
    let android_missing_path = match &android_loc {
        ConfiguredSdk::Missing(path) => Some(path.as_path()),
        _ => None,
    };
    let android_detail = if let Some(ref root) = android_sdk_root {
        let p_str = if platforms.is_empty() {
            "缺少 platforms".to_string()
        } else {
            platforms.join(", ")
        };
        let bt_str = if build_tools.is_empty() {
            "缺少 build-tools".to_string()
        } else {
            build_tools.join(", ")
        };
        format!("{} ({p_str} | {bt_str})", root.display())
    } else {
        missing_sdk_detail(android_missing_path)
    };
    let android_hint = if android_installed {
        "安装 Android Studio 或 Android Command-line Tools，或在设置中指定 Android SDK 安装目录"
            .to_string()
    } else {
        missing_sdk_hint(android_missing_path)
    };
    sdk_tools.push(SdkToolStatus {
        id: "android_sdk".to_string(),
        label: "Android SDK".to_string(),
        installed: android_installed,
        required: project_kind.uses_android(),
        version_or_detail: android_detail,
        path: android_sdk_root
            .clone()
            .or_else(|| android_missing_path.map(Path::to_path_buf)),
        install_hint: android_hint,
        install_cmd: Some(if let Some(ref sm) = sdkmanager_bin {
            format!(
                "\"{}\" \"platform-tools\" \"platforms;android-34\" \"build-tools;34.0.0\"",
                sm.display()
            )
        } else {
            "winget install --id Google.AndroidStudio -e".to_string()
        }),
        download_url: "https://developer.android.com/studio".to_string(),
    });

    // 3. Android SDK Platforms & Build-Tools (when SDK folder exists but platforms missing)
    if project_kind.uses_android() && android_installed && (platforms.is_empty() || build_tools.is_empty()) {
        sdk_tools.push(SdkToolStatus {
            id: "android_platforms".to_string(),
            label: "Android Platforms & Build-Tools".to_string(),
            installed: false,
            required: true,
            version_or_detail: "Android SDK 目录存在，但未下载 platforms / build-tools".to_string(),
            path: android_sdk_root.clone(),
            install_hint: "通过 sdkmanager 下载 Android 平台与构建工具".to_string(),
            install_cmd: Some(if let Some(ref sm) = sdkmanager_bin {
                format!(
                    "\"{}\" \"platform-tools\" \"platforms;android-34\" \"build-tools;34.0.0\"",
                    sm.display()
                )
            } else {
                "sdkmanager \"platform-tools\" \"platforms;android-34\" \"build-tools;34.0.0\"".to_string()
            }),
            download_url: "https://developer.android.com/tools/sdkmanager".to_string(),
        });
    }

    // 4. ADB (Android Debug Bridge)
    let (adb_installed, adb_detail) = if let Some(ref abin) = adb_bin {
        let ver = run_version_line(abin, &["version"])
            .unwrap_or_else(|| "adb installed".to_string());
        (true, ver)
    } else {
        (
            false,
            format!("{SDK_NOT_INSTALLED_LABEL}：未找到 adb（Platform-Tools）"),
        )
    };
    sdk_tools.push(SdkToolStatus {
        id: "adb".to_string(),
        label: "ADB (Android Debug Bridge)".to_string(),
        installed: adb_installed,
        required: project_kind.uses_android(),
        version_or_detail: adb_detail,
        path: adb_bin.clone(),
        install_hint: if adb_installed {
            "安装 Android Platform-Tools 以启用手机连接、调试与 adb logcat".to_string()
        } else {
            missing_sdk_hint(android_sdk_root.as_deref().or(android_missing_path))
        },
        install_cmd: Some("winget install --id Google.PlatformTools -e".to_string()),
        download_url: "https://developer.android.com/tools/releases/platform-tools".to_string(),
    });

    // 5. JDK
    let (jdk_path, jdk_ver) = match &jdk_loc {
        ConfiguredSdk::Found(root) => {
            let bin = jdk_java_in(root);
            let ver = bin
                .as_deref()
                .and_then(|p| run_version_line(p, &["-version"]));
            (bin, ver)
        }
        ConfiguredSdk::Auto => detect_jdk_status(),
        ConfiguredSdk::Missing(_) => (None, None),
    };
    let jdk_installed = jdk_path.is_some() || jdk_ver.is_some();
    let jdk_missing_path = match &jdk_loc {
        ConfiguredSdk::Missing(path) => Some(path.as_path()),
        _ => None,
    };
    sdk_tools.push(SdkToolStatus {
        id: "jdk".to_string(),
        label: "JDK (Java)".to_string(),
        installed: jdk_installed,
        required: project_kind.uses_android(),
        version_or_detail: if jdk_installed {
            jdk_ver.unwrap_or_else(|| "JDK installed".to_string())
        } else {
            missing_sdk_detail(jdk_missing_path)
        },
        path: jdk_path.or_else(|| jdk_missing_path.map(Path::to_path_buf)),
        install_hint: if jdk_installed {
            "安装 Temurin JDK 17（Android Gradle 构建必需）".to_string()
        } else {
            missing_sdk_hint(jdk_missing_path)
        },
        install_cmd: Some("winget install --id EclipseAdoptium.Temurin.17.JDK -e".to_string()),
        download_url: "https://adoptium.net/temurin/releases/?version=17".to_string(),
    });

    // 6. Probe connected devices (adb devices -l + flutter devices --machine + emulator -list-avds)
    let mut devices: Vec<MobileDeviceInfo> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    if let Some(ref abin) = adb_bin {
        if let Some(out) = run_command_stdout(abin, &["devices", "-l"]) {
            for dev in parse_adb_devices_output(&out) {
                seen_ids.insert(dev.id.clone());
                devices.push(dev);
            }
        }
    }

    if project_kind.uses_flutter() {
        if let Some(ref fbin) = flutter_bin {
            if let Some(out) = run_command_stdout(fbin, &["devices", "--machine", "--suppress-analytics"]) {
                for dev in parse_flutter_devices_json(&out) {
                    if let Some(existing) = devices.iter_mut().find(|d| d.id == dev.id) {
                        if !dev.name.is_empty() {
                            existing.name = dev.name;
                        }
                    } else if seen_ids.insert(dev.id.clone()) {
                        devices.push(dev);
                    }
                }
            }
        }
    }

    if let Some(ref emubin) = emulator_bin {
        if let Some(out) = run_command_stdout(emubin, &["-list-avds"]) {
            for avd in parse_emulator_avds_output(&out) {
                if seen_ids.insert(avd.id.clone()) {
                    devices.push(avd);
                }
            }
        }
    }

    let online_cnt = devices
        .iter()
        .filter(|d| d.state == MobileDeviceState::Online)
        .count();
    let missing_cnt = sdk_tools.iter().filter(|t| t.required && !t.installed).count();
    let last_checked_note = format!(
        "在线设备 {online_cnt} 台 · 缺失必需 SDK {missing_cnt} 项"
    );

    MobileEnvironmentReport {
        project_kind,
        sdk_tools,
        devices,
        flutter_bin,
        adb_bin,
        android_sdk_root,
        emulator_bin,
        probing: false,
        last_checked_note,
    }
}

fn detect_android_sdk_root(workspace_root: &Path) -> Option<PathBuf> {
    // 1. Project local.properties `sdk.dir=...`
    for lp in [
        workspace_root.join("local.properties"),
        workspace_root.join("android/local.properties"),
    ] {
        if let Ok(raw) = std::fs::read_to_string(&lp) {
            for line in raw.lines() {
                let trimmed = line.trim();
                if let Some(val) = trimmed.strip_prefix("sdk.dir=") {
                    let unescaped = val.replace("\\:", ":").replace("\\\\", "\\");
                    let candidate = PathBuf::from(unescaped.trim());
                    if candidate.is_dir() {
                        return Some(candidate);
                    }
                }
            }
        }
    }

    // 2. Environment variables
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(val) = std::env::var_os(var) {
            let p = PathBuf::from(val);
            if p.is_dir() {
                return Some(p);
            }
        }
    }

    // 3. Common local directories
    let mut candidates = Vec::new();
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local_app_data).join("Android/Sdk"));
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        let home_p = PathBuf::from(home);
        candidates.push(home_p.join("AppData/Local/Android/Sdk"));
        candidates.push(home_p.join("Android/Sdk"));
        candidates.push(home_p.join("Library/Android/sdk"));
    }
    candidates.push(PathBuf::from(r"C:\Android\Sdk"));

    candidates.into_iter().find(|p| p.is_dir())
}

fn detect_adb_binary(sdk_root: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = sdk_root {
        for rel in ["platform-tools/adb.exe", "platform-tools/adb"] {
            let p = root.join(rel);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    find_executable_in_path(&["adb.exe", "adb"])
}

fn detect_emulator_binary(sdk_root: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = sdk_root {
        for rel in ["emulator/emulator.exe", "emulator/emulator"] {
            let p = root.join(rel);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    find_executable_in_path(&["emulator.exe", "emulator"])
}

fn detect_sdkmanager_binary(sdk_root: Option<&Path>) -> Option<PathBuf> {
    let root = sdk_root?;
    let cmdline_dir = root.join("cmdline-tools");
    if let Ok(entries) = std::fs::read_dir(&cmdline_dir) {
        for e in entries.flatten() {
            for name in ["bin/sdkmanager.bat", "bin/sdkmanager"] {
                let candidate = e.path().join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    for rel in [
        "tools/bin/sdkmanager.bat",
        "tools/bin/sdkmanager",
    ] {
        let p = root.join(rel);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn detect_flutter_binary(workspace_root: &Path) -> Option<PathBuf> {
    // 1. Project local.properties `flutter.sdk=...`
    for lp in [
        workspace_root.join("local.properties"),
        workspace_root.join("android/local.properties"),
    ] {
        if let Ok(raw) = std::fs::read_to_string(&lp) {
            for line in raw.lines() {
                if let Some(val) = line.trim().strip_prefix("flutter.sdk=") {
                    let unescaped = val.replace("\\:", ":").replace("\\\\", "\\");
                    let sdk = PathBuf::from(unescaped.trim());
                    for bin_rel in ["bin/flutter.bat", "bin/flutter"] {
                        let p = sdk.join(bin_rel);
                        if p.is_file() {
                            return Some(p);
                        }
                    }
                }
            }
        }
    }

    if let Some(in_path) = find_executable_in_path(&["flutter.bat", "flutter"]) {
        return Some(in_path);
    }

    if let Some(froot) = std::env::var_os("FLUTTER_ROOT") {
        let base = PathBuf::from(froot);
        for bin_rel in ["bin/flutter.bat", "bin/flutter"] {
            let p = base.join(bin_rel);
            if p.is_file() {
                return Some(p);
            }
        }
    }

    let mut candidates = vec![
        PathBuf::from(r"C:\src\flutter\bin\flutter.bat"),
        PathBuf::from(r"C:\flutter\bin\flutter.bat"),
    ];
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        let hp = PathBuf::from(home);
        candidates.push(hp.join("flutter/bin/flutter.bat"));
        candidates.push(hp.join("flutter/bin/flutter"));
        candidates.push(hp.join("fvm/default/bin/flutter.bat"));
    }
    if let Some(lad) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(lad).join("flutter/bin/flutter.bat"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn detect_jdk_status() -> (Option<PathBuf>, Option<String>) {
    let mut java_bin = None;
    if let Some(jh) = std::env::var_os("JAVA_HOME") {
        let base = PathBuf::from(jh);
        for rel in ["bin/java.exe", "bin/java"] {
            let p = base.join(rel);
            if p.is_file() {
                java_bin = Some(p);
                break;
            }
        }
    }
    if java_bin.is_none() {
        java_bin = find_executable_in_path(&["java.exe", "java"]);
    }
    if java_bin.is_none() {
        let studio_jbr = PathBuf::from(r"C:\Program Files\Android\Android Studio\jbr\bin\java.exe");
        if studio_jbr.is_file() {
            java_bin = Some(studio_jbr);
        }
    }
    let ver = java_bin
        .as_deref()
        .and_then(|p| run_version_line(p, &["-version"]));
    (java_bin, ver)
}

fn list_installed_android_platforms(sdk_root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(sdk_root.join("platforms")) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                if let Some(name) = e.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
        }
    }
    out.sort();
    out
}

fn list_installed_build_tools(sdk_root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(sdk_root.join("build-tools")) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                if let Some(name) = e.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
        }
    }
    out.sort();
    out
}

fn find_executable_in_path(names: &[&str]) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn run_command_stdout(bin: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = build_hidden_command(bin, args);
    let output = cmd.output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if stdout.trim().is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if stderr.trim().is_empty() {
            None
        } else {
            Some(stderr)
        }
    } else {
        Some(stdout)
    }
}

fn run_version_line(bin: &Path, args: &[&str]) -> Option<String> {
    if std::fs::metadata(bin).ok().is_some_and(|meta| meta.len() == 0) {
        return Some("installed".to_string());
    }
    let out = run_command_stdout(bin, args)?;
    out.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.to_string())
}

fn build_hidden_command(bin: &Path, args: &[&str]) -> Command {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let ext = bin
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "bat" || ext == "cmd" {
            let mut c = Command::new("cmd.exe");
            c.arg("/C").arg(bin).args(args);
            c
        } else {
            let mut c = Command::new(bin);
            c.args(args);
            c
        }
    };
    #[cfg(not(target_os = "windows"))]
    let cmd = {
        let mut c = Command::new(bin);
        c.args(args);
        c
    };
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_adb_devices_and_flutter_json() {
        let adb_out = "\
* daemon not running; starting now at tcp:5037
* daemon started successfully
List of devices attached
RFCW12345AB\tdevice product:dm1qxxx model:SM_S9110 device:dm1q transport_id:1
emulator-5554\tdevice product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 device:generic_x86_64 transport_id:2
UNAUTH99\tunauthorized transport_id:3
192.168.1.10:5555\toffline
";
        let devs = parse_adb_devices_output(adb_out);
        assert_eq!(devs.len(), 4);
        assert_eq!(devs[0].id, "RFCW12345AB");
        assert_eq!(devs[0].name, "SM S9110");
        assert_eq!(devs[0].state, MobileDeviceState::Online);
        assert!(!devs[0].is_emulator);
        assert!(devs[1].is_emulator);
        assert_eq!(devs[2].state, MobileDeviceState::Unauthorized);
        assert_eq!(devs[3].state, MobileDeviceState::Offline);

        let flutter_json = r#"[
          {"id":"RFCW12345AB","name":"Samsung Galaxy S23","targetPlatform":"android-arm64","emulator":false},
          {"id":"windows","name":"Windows (desktop)","targetPlatform":"windows-x64","emulator":false}
        ]"#;
        let fdevs = parse_flutter_devices_json(flutter_json);
        assert_eq!(fdevs.len(), 2);
        assert_eq!(fdevs[0].name, "Samsung Galaxy S23");
        assert_eq!(fdevs[1].id, "windows");

        let avds = parse_emulator_avds_output("Pixel_7_API_34\nMedium_Phone_API_35\n");
        assert_eq!(avds.len(), 2);
        assert_eq!(avds[0].state, MobileDeviceState::EmulatorAvd);
    }

    #[test]
    fn test_detect_mobile_project_kind() {
        let unique = format!(
            "ide-mobile-detect-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(root.join("android/app/src/main")).unwrap();
        std::fs::write(
            root.join("pubspec.yaml"),
            "name: my_app\ndependencies:\n  flutter:\n    sdk: flutter\n",
        )
        .unwrap();
        std::fs::write(
            root.join("android/app/src/main/AndroidManifest.xml"),
            "<manifest package=\"com.example.my_app\"/>",
        )
        .unwrap();

        assert_eq!(
            detect_mobile_project(&root),
            MobileProjectKind::FlutterAndAndroid
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn configured_missing_sdk_dir_reports_not_installed_without_path_fallback() {
        let unique = format!(
            "ide-mobile-missing-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).unwrap();
        let locations = MobileSdkLocations {
            flutter_sdk: root.join("no-flutter").display().to_string(),
            android_sdk: root.join("no-android").display().to_string(),
            jdk_home: root.join("no-jdk").display().to_string(),
        };
        let report =
            probe_mobile_environment(&root, MobileProjectKind::Flutter, &locations);
        for id in ["flutter", "android_sdk", "jdk"] {
            let tool = report
                .sdk_tools
                .iter()
                .find(|t| t.id == id)
                .unwrap_or_else(|| panic!("missing tool {id}"));
            assert!(!tool.installed, "{id} should be missing");
            assert!(
                tool.version_or_detail.contains(SDK_NOT_INSTALLED_LABEL),
                "{id} detail was {}",
                tool.version_or_detail
            );
        }
        assert!(report.flutter_bin.is_none());
        assert!(report.android_sdk_root.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn configured_sdk_dirs_are_used_when_they_contain_the_toolchain() {
        let unique = format!(
            "ide-mobile-found-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let flutter_root = root.join("flutter");
        let android_root = root.join("android");
        let jdk_root = root.join("jdk");
        std::fs::create_dir_all(flutter_root.join("bin")).unwrap();
        std::fs::create_dir_all(android_root.join("platform-tools")).unwrap();
        std::fs::create_dir_all(jdk_root.join("bin")).unwrap();
        std::fs::write(
            flutter_root.join("bin").join("flutter.bat"),
            "@echo off\r\necho Flutter 3.16.0\r\n",
        )
        .unwrap();
        std::fs::write(jdk_root.join("bin").join("java.exe"), b"").unwrap();

        let locations = MobileSdkLocations {
            flutter_sdk: flutter_root.display().to_string(),
            android_sdk: android_root.display().to_string(),
            jdk_home: jdk_root.display().to_string(),
        };
        let report = probe_mobile_environment(&root, MobileProjectKind::None, &locations);
        let flutter = report.sdk_tools.iter().find(|t| t.id == "flutter").unwrap();
        assert!(flutter.installed, "{}", flutter.version_or_detail);
        assert_eq!(report.android_sdk_root.as_deref(), Some(android_root.as_path()));
        let jdk = report.sdk_tools.iter().find(|t| t.id == "jdk").unwrap();
        assert!(jdk.installed, "{}", jdk.version_or_detail);
        let _ = std::fs::remove_dir_all(&root);
    }
}
