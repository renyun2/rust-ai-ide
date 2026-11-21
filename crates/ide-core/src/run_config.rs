use crate::LanguageId;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Origin of a Run/Debug Configuration entry shown in the IDE toolbar list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RunConfigSource {
    #[default]
    Detected,
    Custom,
}

/// IDEA-style Run/Debug configuration parsed from project manifests, files, or CLI commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunConfiguration {
    pub name: String,
    pub language: LanguageId,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub source: RunConfigSource,
    #[serde(default)]
    pub target_file: Option<PathBuf>,
}

const IGNORED_SUBDIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".venv",
    "venv",
    "__pycache__",
    ".gradle",
    ".idea",
    ".vscode",
    ".next",
    ".nuxt",
    "coverage",
    "vendor",
];

const TRIVIAL_TERMINAL_CMDS: &[&str] = &[
    "cd",
    "sl",
    "set-location",
    "pushd",
    "popd",
    "cls",
    "clear",
    "ls",
    "dir",
    "ll",
    "la",
    "pwd",
    "exit",
    "quit",
    "echo",
    "write-output",
    "cat",
    "type",
    "get-content",
    "head",
    "tail",
    "mkdir",
    "md",
    "rmdir",
    "rd",
    "rm",
    "del",
    "erase",
    "remove-item",
    "cp",
    "copy",
    "mv",
    "move",
    "ren",
    "rename-item",
    "touch",
    "new-item",
    "which",
    "where",
    "where.exe",
    "get-command",
    "whoami",
    "hostname",
    "date",
    "history",
    "h",
    "help",
    "man",
    "get-help",
    "true",
    "false",
    "y",
    "n",
    "yes",
    "no",
];

impl RunConfiguration {
    /// Canonical identity key binding this RunConfiguration to a dedicated terminal/process slot.
    pub fn identity_key(&self) -> String {
        let norm_cwd = std::fs::canonicalize(&self.cwd)
            .unwrap_or_else(|_| self.cwd.clone())
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        format!(
            "{}|{}|{}",
            normalize_cmd_token(&self.command),
            self.args.join("\x1f"),
            norm_cwd
        )
    }

    /// Returns true if two configurations represent the same command, arguments, and working directory.
    pub fn same_identity(&self, other: &Self) -> bool {
        normalize_cmd_token(&self.command) == normalize_cmd_token(&other.command)
            && self.args == other.args
            && paths_equivalent(&self.cwd, &other.cwd)
    }

    /// Scans `workspace_root` (and direct 1-level subprojects) to detect launch commands for
    /// Cargo, npm/pnpm/yarn/bun, Docker/Compose, Gradle, Maven, Python, Go, .NET, Make/Just.
    pub fn detect_for_workspace(workspace_root: &Path) -> Vec<Self> {
        let mut configs = Vec::new();
        let mut cargo_member_dirs: Vec<PathBuf> = Vec::new();

        detect_cargo_configs(workspace_root, None, &mut configs, &mut cargo_member_dirs);
        detect_in_directory(workspace_root, workspace_root, None, &mut configs);

        // Scan immediate subdirectories (e.g. frontend/, backend/, docker/, service/) for polyglot setups.
        if let Ok(entries) = std::fs::read_dir(workspace_root) {
            let mut subdirs: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_dir()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| {
                                !n.starts_with('.')
                                    && !IGNORED_SUBDIRS
                                        .iter()
                                        .any(|ig| ig.eq_ignore_ascii_case(n))
                            })
                })
                .collect();
            subdirs.sort();

            for subdir in subdirs {
                if configs.len() >= 60 {
                    break;
                }
                let sub_label = subdir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string());
                let is_root_cargo_member = cargo_member_dirs
                    .iter()
                    .any(|m| paths_equivalent(m, &subdir));
                if !is_root_cargo_member && !workspace_root.join("Cargo.toml").exists() {
                    let mut ignored = Vec::new();
                    detect_cargo_configs(&subdir, sub_label.as_deref(), &mut configs, &mut ignored);
                }
                detect_in_directory(&subdir, workspace_root, sub_label.as_deref(), &mut configs);
            }
        }

        if configs.is_empty() {
            configs.push(Self {
                name: "Default Shell Task".to_string(),
                language: LanguageId::PlainText,
                command: "cargo".to_string(),
                args: vec!["--version".to_string()],
                cwd: workspace_root.to_path_buf(),
                source: RunConfigSource::Detected,
                target_file: None,
            });
        }

        dedup_preserve_order(configs)
    }

    /// Builds a custom RunConfiguration for running a specific source or build/container file.
    pub fn for_file(file_path: &Path, workspace_root: &Path) -> Option<Self> {
        let file_name = file_path.file_name()?.to_str()?;
        let lower_name = file_name.to_ascii_lowercase();
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let stem = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(file_name);
        let rel_display = relative_display_path(file_path, workspace_root);
        let parent_dir = file_path.parent().unwrap_or(workspace_root).to_path_buf();

        // Special container / manifest files
        if lower_name == "dockerfile" || lower_name.starts_with("dockerfile.") {
            let tag = sanitize_docker_tag(
                parent_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("app"),
            );
            let rel_from_parent = file_name.to_string();
            return Some(Self {
                name: format!("Docker: build ({file_name})"),
                language: LanguageId::PlainText,
                command: "docker".to_string(),
                args: vec![
                    "build".to_string(),
                    "-f".to_string(),
                    rel_from_parent,
                    "-t".to_string(),
                    format!("{tag}:latest"),
                    ".".to_string(),
                ],
                cwd: parent_dir,
                source: RunConfigSource::Custom,
                target_file: Some(file_path.to_path_buf()),
            });
        }
        if lower_name == "compose.yml"
            || lower_name == "compose.yaml"
            || (lower_name.starts_with("docker-compose")
                && (lower_name.ends_with(".yml") || lower_name.ends_with(".yaml")))
        {
            return Some(Self {
                name: format!("Docker Compose: {file_name}"),
                language: LanguageId::PlainText,
                command: "docker".to_string(),
                args: vec![
                    "compose".to_string(),
                    "-f".to_string(),
                    file_name.to_string(),
                    "up".to_string(),
                ],
                cwd: parent_dir,
                source: RunConfigSource::Custom,
                target_file: Some(file_path.to_path_buf()),
            });
        }
        if lower_name == "package.json" {
            let mut detected = Vec::new();
            detect_node_configs(&parent_dir, workspace_root, None, &mut detected);
            if let Some(mut first) = detected.into_iter().next() {
                first.source = RunConfigSource::Custom;
                first.target_file = Some(file_path.to_path_buf());
                return Some(first);
            }
            return None;
        }
        if lower_name == "build.gradle" || lower_name == "build.gradle.kts" {
            let mut detected = Vec::new();
            detect_gradle_configs(&parent_dir, workspace_root, None, &mut detected);
            if let Some(mut first) = detected.into_iter().next() {
                first.source = RunConfigSource::Custom;
                first.target_file = Some(file_path.to_path_buf());
                return Some(first);
            }
            return None;
        }
        if lower_name == "cargo.toml" {
            let mut detected = Vec::new();
            let mut ignored = Vec::new();
            detect_cargo_configs(&parent_dir, None, &mut detected, &mut ignored);
            if let Some(mut first) = detected.into_iter().next() {
                first.source = RunConfigSource::Custom;
                first.target_file = Some(file_path.to_path_buf());
                return Some(first);
            }
            return None;
        }

        match ext.as_str() {
            "py" => {
                let is_test = lower_name.starts_with("test_") || lower_name.ends_with("_test.py");
                let (cmd, mut args) = detect_python_runner(workspace_root);
                if is_test {
                    if cmd == "python" {
                        return Some(Self {
                            name: format!("Pytest: {file_name}"),
                            language: LanguageId::Python,
                            command: "pytest".to_string(),
                            args: vec![rel_display],
                            cwd: workspace_root.to_path_buf(),
                            source: RunConfigSource::Custom,
                            target_file: Some(file_path.to_path_buf()),
                        });
                    }
                    args.push("pytest".to_string());
                    args.push(rel_display);
                    return Some(Self {
                        name: format!("Pytest: {file_name}"),
                        language: LanguageId::Python,
                        command: cmd,
                        args,
                        cwd: workspace_root.to_path_buf(),
                        source: RunConfigSource::Custom,
                        target_file: Some(file_path.to_path_buf()),
                    });
                }
                args.push(rel_display);
                Some(Self {
                    name: format!("Run {file_name}"),
                    language: LanguageId::Python,
                    command: cmd,
                    args,
                    cwd: workspace_root.to_path_buf(),
                    source: RunConfigSource::Custom,
                    target_file: Some(file_path.to_path_buf()),
                })
            }
            "js" | "mjs" | "cjs" => Some(Self {
                name: format!("Node: {file_name}"),
                language: LanguageId::JavaScript,
                command: "node".to_string(),
                args: vec![rel_display],
                cwd: workspace_root.to_path_buf(),
                source: RunConfigSource::Custom,
                target_file: Some(file_path.to_path_buf()),
            }),
            "ts" | "mts" => {
                let use_bun = workspace_root.join("bun.lock").exists()
                    || workspace_root.join("bun.lockb").exists();
                let (command, args) = if use_bun {
                    ("bun".to_string(), vec!["run".to_string(), rel_display])
                } else {
                    (
                        "npx".to_string(),
                        vec!["tsx".to_string(), rel_display],
                    )
                };
                Some(Self {
                    name: format!("TS: {file_name}"),
                    language: LanguageId::TypeScript,
                    command,
                    args,
                    cwd: workspace_root.to_path_buf(),
                    source: RunConfigSource::Custom,
                    target_file: Some(file_path.to_path_buf()),
                })
            }
            "rs" => Some(cargo_config_for_rust_file(file_path, workspace_root)),
            "go" => {
                if lower_name.ends_with("_test.go") {
                    let pkg_rel = relative_display_path(&parent_dir, workspace_root);
                    let target = if pkg_rel == "." {
                        ".".to_string()
                    } else if pkg_rel.starts_with('.') {
                        pkg_rel
                    } else {
                        format!("./{pkg_rel}")
                    };
                    Some(Self {
                        name: format!("Go Test: {file_name}"),
                        language: LanguageId::PlainText,
                        command: "go".to_string(),
                        args: vec!["test".to_string(), target],
                        cwd: workspace_root.to_path_buf(),
                        source: RunConfigSource::Custom,
                        target_file: Some(file_path.to_path_buf()),
                    })
                } else {
                    Some(Self {
                        name: format!("Go: {file_name}"),
                        language: LanguageId::PlainText,
                        command: "go".to_string(),
                        args: vec!["run".to_string(), rel_display],
                        cwd: workspace_root.to_path_buf(),
                        source: RunConfigSource::Custom,
                        target_file: Some(file_path.to_path_buf()),
                    })
                }
            }
            "java" => Some(Self {
                name: format!("Java: {file_name}"),
                language: LanguageId::PlainText,
                command: "java".to_string(),
                args: vec![rel_display],
                cwd: workspace_root.to_path_buf(),
                source: RunConfigSource::Custom,
                target_file: Some(file_path.to_path_buf()),
            }),
            "dart" => {
                let is_flutter = crate::mobile::dir_has_flutter_pubspec(workspace_root);
                let is_test = lower_name.ends_with("_test.dart");
                if is_flutter {
                    let (name, args) = if is_test {
                        (
                            format!("Flutter Test: {file_name}"),
                            vec!["test".to_string(), rel_display],
                        )
                    } else {
                        (
                            format!("Flutter: {file_name}"),
                            vec!["run".to_string(), "-t".to_string(), rel_display],
                        )
                    };
                    Some(Self {
                        name,
                        language: LanguageId::PlainText,
                        command: "flutter".to_string(),
                        args,
                        cwd: workspace_root.to_path_buf(),
                        source: RunConfigSource::Custom,
                        target_file: Some(file_path.to_path_buf()),
                    })
                } else {
                    Some(Self {
                        name: format!("Dart: {file_name}"),
                        language: LanguageId::PlainText,
                        command: "dart".to_string(),
                        args: vec![
                            if is_test { "test" } else { "run" }.to_string(),
                            rel_display,
                        ],
                        cwd: workspace_root.to_path_buf(),
                        source: RunConfigSource::Custom,
                        target_file: Some(file_path.to_path_buf()),
                    })
                }
            }
            "ps1" => Some(Self {
                name: format!("PowerShell: {file_name}"),
                language: LanguageId::PlainText,
                command: "powershell".to_string(),
                args: vec![
                    "-ExecutionPolicy".to_string(),
                    "Bypass".to_string(),
                    "-File".to_string(),
                    rel_display,
                ],
                cwd: workspace_root.to_path_buf(),
                source: RunConfigSource::Custom,
                target_file: Some(file_path.to_path_buf()),
            }),
            "bat" | "cmd" => {
                let invocation = if cfg!(windows) && !rel_display.starts_with('.') {
                    format!(".\\{}", rel_display.replace('/', "\\"))
                } else {
                    rel_display
                };
                Some(Self {
                    name: format!("Script: {file_name}"),
                    language: LanguageId::PlainText,
                    command: invocation,
                    args: Vec::new(),
                    cwd: workspace_root.to_path_buf(),
                    source: RunConfigSource::Custom,
                    target_file: Some(file_path.to_path_buf()),
                })
            }
            "sh" => Some(Self {
                name: format!("Shell: {file_name}"),
                language: LanguageId::PlainText,
                command: "bash".to_string(),
                args: vec![rel_display],
                cwd: workspace_root.to_path_buf(),
                source: RunConfigSource::Custom,
                target_file: Some(file_path.to_path_buf()),
            }),
            "rb" => Some(Self {
                name: format!("Ruby: {file_name}"),
                language: LanguageId::PlainText,
                command: "ruby".to_string(),
                args: vec![rel_display],
                cwd: workspace_root.to_path_buf(),
                source: RunConfigSource::Custom,
                target_file: Some(file_path.to_path_buf()),
            }),
            "php" => Some(Self {
                name: format!("PHP: {file_name}"),
                language: LanguageId::PlainText,
                command: "php".to_string(),
                args: vec![rel_display],
                cwd: workspace_root.to_path_buf(),
                source: RunConfigSource::Custom,
                target_file: Some(file_path.to_path_buf()),
            }),
            "lua" => Some(Self {
                name: format!("Lua: {file_name}"),
                language: LanguageId::PlainText,
                command: "lua".to_string(),
                args: vec![rel_display],
                cwd: workspace_root.to_path_buf(),
                source: RunConfigSource::Custom,
                target_file: Some(file_path.to_path_buf()),
            }),
            "rs_standalone" => {
                let _ = stem;
                None
            }
            _ => None,
        }
    }

    /// Parses an interactive terminal command line into a custom RunConfiguration, skipping
    /// trivial shell navigation/housekeeping commands like `cd`, `cls`, `ls`, `dir`, `pwd`.
    pub fn from_terminal_command(cmd_line: &str, cwd: &Path) -> Option<Self> {
        let trimmed = cmd_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return None;
        }
        let parts = parse_shell_command_parts(trimmed);
        let command = parts.first()?.clone();
        let base = normalize_cmd_token(&command);
        if TRIVIAL_TERMINAL_CMDS.contains(&base.as_str()) {
            return None;
        }
        Self::build_from_parts(trimmed, parts, cwd)
    }

    /// Parses an explicitly entered command line (e.g. from the "+ Add Run Command" modal)
    /// into a custom RunConfiguration without filtering shell built-ins.
    pub fn from_command_line_explicit(cmd_line: &str, cwd: &Path) -> Option<Self> {
        let trimmed = cmd_line.trim();
        if trimmed.is_empty() {
            return None;
        }
        let parts = parse_shell_command_parts(trimmed);
        if parts.is_empty() {
            return None;
        }
        Self::build_from_parts(trimmed, parts, cwd)
    }

    fn build_from_parts(trimmed: &str, mut parts: Vec<String>, cwd: &Path) -> Option<Self> {
        let command = parts.remove(0);
        let args = parts;
        let language = infer_language_from_command(&command, &args);
        let name = truncate_display(trimmed, 38);
        Some(Self {
            name,
            language,
            command,
            args,
            cwd: cwd.to_path_buf(),
            source: RunConfigSource::Custom,
            target_file: None,
        })
    }

    /// Returns a copy bound to `device_id` when running `flutter run` or `adb logcat`.
    pub fn effective_with_mobile_device(&self, device_id: Option<&str>) -> Self {
        let Some(dev_id) = device_id.filter(|s| !s.trim().is_empty() && !s.starts_with("avd:"))
        else {
            return self.clone();
        };
        let mut out = self.clone();
        let cmd = normalize_cmd_token(&out.command);
        if cmd == "flutter"
            && out.args.first().map(|s| s.as_str()) == Some("run")
            && !out.args.iter().any(|a| a == "-d" || a == "--device-id")
        {
            out.args.push("-d".to_string());
            out.args.push(dev_id.to_string());
        } else if cmd == "adb" && !out.args.iter().any(|a| a == "-s") {
            out.args.insert(0, "-s".to_string());
            out.args.insert(1, dev_id.to_string());
        }
        out
    }
}

fn detect_in_directory(
    dir: &Path,
    workspace_root: &Path,
    sub_label: Option<&str>,
    configs: &mut Vec<RunConfiguration>,
) {
    detect_flutter_configs(dir, sub_label, configs);
    detect_node_configs(dir, workspace_root, sub_label, configs);
    detect_docker_configs(dir, sub_label, configs);
    detect_gradle_configs(dir, workspace_root, sub_label, configs);
    detect_maven_configs(dir, workspace_root, sub_label, configs);
    detect_python_configs(dir, sub_label, configs);
    detect_go_configs(dir, sub_label, configs);
    detect_dotnet_configs(dir, sub_label, configs);
    detect_make_and_just_configs(dir, sub_label, configs);
}

fn detect_flutter_configs(
    dir: &Path,
    sub_label: Option<&str>,
    configs: &mut Vec<RunConfiguration>,
) {
    let pubspec = dir.join("pubspec.yaml");
    if !pubspec.is_file() {
        return;
    }
    if crate::mobile::dir_has_flutter_pubspec(dir) {
        let tasks: [(&str, &[&str]); 6] = [
            ("Flutter: run", &["run"]),
            ("Flutter: test", &["test"]),
            ("Flutter: build apk", &["build", "apk"]),
            ("Flutter: pub get", &["pub", "get"]),
            ("Flutter: clean", &["clean"]),
            ("Flutter: doctor", &["doctor"]),
        ];
        for (label, args) in tasks {
            configs.push(RunConfiguration {
                name: suffix_name(label.to_string(), sub_label),
                language: LanguageId::PlainText,
                command: "flutter".to_string(),
                args: args.iter().map(|s| (*s).to_string()).collect(),
                cwd: dir.to_path_buf(),
                source: RunConfigSource::Detected,
                target_file: Some(pubspec.clone()),
            });
        }
    } else {
        for (label, sub) in [("Dart: run", "run"), ("Dart: test", "test")] {
            configs.push(RunConfiguration {
                name: suffix_name(label.to_string(), sub_label),
                language: LanguageId::PlainText,
                command: "dart".to_string(),
                args: vec![sub.to_string()],
                cwd: dir.to_path_buf(),
                source: RunConfigSource::Detected,
                target_file: Some(pubspec.clone()),
            });
        }
    }
}

fn suffix_name(base: String, sub_label: Option<&str>) -> String {
    match sub_label {
        Some(lbl) => format!("{base} ({lbl})"),
        None => base,
    }
}

fn detect_node_configs(
    dir: &Path,
    workspace_root: &Path,
    sub_label: Option<&str>,
    configs: &mut Vec<RunConfiguration>,
) {
    let pkg_path = dir.join("package.json");
    let Ok(raw) = std::fs::read_to_string(&pkg_path) else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return;
    };

    let pm = detect_node_pm(dir, workspace_root, &json);
    let language = if dir.join("tsconfig.json").exists() {
        LanguageId::TypeScript
    } else {
        LanguageId::JavaScript
    };

    let priority_order = [
        "dev",
        "start",
        "serve",
        "preview",
        "watch",
        "build",
        "test",
        "lint",
        "typecheck",
        "check",
        "format",
    ];

    if let Some(scripts) = json.get("scripts").and_then(|v| v.as_object()) {
        let mut added_keys = HashSet::new();
        let mut ordered_keys: Vec<&String> = Vec::new();

        for pri in priority_order {
            if let Some((k, _)) = scripts.get_key_value(pri) {
                if added_keys.insert(k.as_str()) {
                    ordered_keys.push(k);
                }
            }
        }
        for k in scripts.keys() {
            if ordered_keys.len() >= 15 {
                break;
            }
            if added_keys.insert(k.as_str()) {
                ordered_keys.push(k);
            }
        }

        for script_name in ordered_keys {
            let args = match pm {
                "npm" if script_name == "start" || script_name == "test" => {
                    vec![script_name.clone()]
                }
                "pnpm" | "bun" if script_name == "test" => vec!["test".to_string()],
                "yarn" => vec![script_name.clone()],
                _ => vec!["run".to_string(), script_name.clone()],
            };
            configs.push(RunConfiguration {
                name: suffix_name(format!("{pm}: {script_name}"), sub_label),
                language,
                command: pm.to_string(),
                args,
                cwd: dir.to_path_buf(),
                source: RunConfigSource::Detected,
                target_file: Some(pkg_path.clone()),
            });
        }
    } else if let Some(main_entry) = json.get("main").and_then(|v| v.as_str()) {
        if dir.join(main_entry).is_file() {
            configs.push(RunConfiguration {
                name: suffix_name(format!("node: {main_entry}"), sub_label),
                language,
                command: "node".to_string(),
                args: vec![main_entry.to_string()],
                cwd: dir.to_path_buf(),
                source: RunConfigSource::Detected,
                target_file: Some(dir.join(main_entry)),
            });
        }
    }
}

fn detect_node_pm(dir: &Path, workspace_root: &Path, json: &serde_json::Value) -> &'static str {
    if let Some(pm_spec) = json.get("packageManager").and_then(|v| v.as_str()) {
        let lower = pm_spec.to_ascii_lowercase();
        if lower.starts_with("pnpm") {
            return "pnpm";
        }
        if lower.starts_with("yarn") {
            return "yarn";
        }
        if lower.starts_with("bun") {
            return "bun";
        }
        if lower.starts_with("npm") {
            return "npm";
        }
    }

    for check_dir in [dir, workspace_root] {
        if check_dir.join("pnpm-lock.yaml").exists() {
            return "pnpm";
        }
        if check_dir.join("yarn.lock").exists() {
            return "yarn";
        }
        if check_dir.join("bun.lock").exists() || check_dir.join("bun.lockb").exists() {
            return "bun";
        }
        if check_dir.join("package-lock.json").exists() {
            return "npm";
        }
    }
    "npm"
}

fn detect_docker_configs(
    dir: &Path,
    sub_label: Option<&str>,
    configs: &mut Vec<RunConfiguration>,
) {
    let default_compose = [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
    ]
    .into_iter()
    .map(|f| dir.join(f))
    .find(|p| p.is_file());

    if let Some(compose_file) = default_compose {
        let compose_commands: [(&str, &[&str]); 4] = [
            ("Docker Compose: up", &["compose", "up"]),
            ("Docker Compose: up -d", &["compose", "up", "-d"]),
            ("Docker Compose: build", &["compose", "build"]),
            ("Docker Compose: down", &["compose", "down"]),
        ];
        for (label, args) in compose_commands {
            configs.push(RunConfiguration {
                name: suffix_name(label.to_string(), sub_label),
                language: LanguageId::PlainText,
                command: "docker".to_string(),
                args: args.iter().map(|s| (*s).to_string()).collect(),
                cwd: dir.to_path_buf(),
                source: RunConfigSource::Detected,
                target_file: Some(compose_file.clone()),
            });
        }
    }

    let dockerfile_path = ["Dockerfile", "dockerfile"]
        .into_iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file());

    if let Some(df_path) = dockerfile_path {
        let tag = sanitize_docker_tag(
            dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("app"),
        );
        let image_ref = format!("{tag}:latest");
        configs.push(RunConfiguration {
            name: suffix_name(format!("Docker: build ({image_ref})"), sub_label),
            language: LanguageId::PlainText,
            command: "docker".to_string(),
            args: vec![
                "build".to_string(),
                "-t".to_string(),
                image_ref.clone(),
                ".".to_string(),
            ],
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: Some(df_path.clone()),
        });

        let mut run_args = vec!["run".to_string(), "--rm".to_string(), "-it".to_string()];
        if let Some(port) = extract_dockerfile_exposed_port(&df_path) {
            run_args.push("-p".to_string());
            run_args.push(format!("{port}:{port}"));
        }
        run_args.push(image_ref.clone());
        configs.push(RunConfiguration {
            name: suffix_name(format!("Docker: run ({image_ref})"), sub_label),
            language: LanguageId::PlainText,
            command: "docker".to_string(),
            args: run_args,
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: Some(df_path),
        });
    }
}

fn extract_dockerfile_exposed_port(dockerfile: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dockerfile).ok()?;
    for line in raw.lines() {
        let trimmed = line.trim();
        let rest = if let Some(r) = trimmed.strip_prefix("EXPOSE ") {
            r
        } else if let Some(r) = trimmed.strip_prefix("expose ") {
            r
        } else {
            continue;
        };
        if let Some(token) = rest.split_whitespace().next() {
            let port_only = token.split('/').next().unwrap_or("");
            if !port_only.is_empty() && port_only.chars().all(|c| c.is_ascii_digit()) {
                return Some(port_only.to_string());
            }
        }
    }
    None
}

fn detect_gradle_configs(
    dir: &Path,
    workspace_root: &Path,
    sub_label: Option<&str>,
    configs: &mut Vec<RunConfiguration>,
) {
    let build_file = [
        dir.join("build.gradle.kts"),
        dir.join("build.gradle"),
        dir.join("settings.gradle.kts"),
        dir.join("settings.gradle"),
    ]
    .into_iter()
    .find(|p| p.is_file());

    let Some(gradle_manifest) = build_file else {
        return;
    };

    let gradle_cmd = detect_gradle_executable(dir, workspace_root);
    let mut build_content = std::fs::read_to_string(&gradle_manifest).unwrap_or_default();
    let app_build = [dir.join("app/build.gradle.kts"), dir.join("app/build.gradle")]
        .into_iter()
        .find(|p| p.is_file());
    if let Some(app_path) = &app_build {
        if let Ok(extra) = std::fs::read_to_string(app_path) {
            build_content.push('\n');
            build_content.push_str(&extra);
        }
    }

    let mut tasks: Vec<String> = Vec::new();
    if build_content.contains("org.springframework.boot") || build_content.contains("bootRun") {
        tasks.push("bootRun".to_string());
    }
    if build_content.contains("application") || build_content.contains("mainClass") {
        tasks.push("run".to_string());
    }
    if build_content.contains("com.android.application")
        || crate::mobile::dir_has_android_project(dir)
    {
        for task in [
            "installDebug",
            "assembleDebug",
            "testDebugUnitTest",
            "connectedAndroidTest",
        ] {
            if !tasks.iter().any(|t| t == task) {
                tasks.push(task.to_string());
            }
        }
    }

    for custom_task in extract_gradle_custom_tasks(&build_content) {
        if !tasks.contains(&custom_task) {
            tasks.push(custom_task);
        }
    }

    for std_task in ["build", "test", "clean build"] {
        if !tasks.iter().any(|t| t == std_task) {
            tasks.push(std_task.to_string());
        }
    }

    for task in tasks {
        let args: Vec<String> = task.split_whitespace().map(|s| s.to_string()).collect();
        configs.push(RunConfiguration {
            name: suffix_name(format!("Gradle: {task}"), sub_label),
            language: LanguageId::PlainText,
            command: gradle_cmd.clone(),
            args,
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: Some(gradle_manifest.clone()),
        });
    }
}

fn detect_gradle_executable(dir: &Path, workspace_root: &Path) -> String {
    for base in [dir, workspace_root] {
        if cfg!(windows) && base.join("gradlew.bat").is_file() {
            return if paths_equivalent(base, dir) {
                ".\\gradlew.bat".to_string()
            } else {
                base.join("gradlew.bat").display().to_string()
            };
        }
        if base.join("gradlew").is_file() {
            return if paths_equivalent(base, dir) {
                "./gradlew".to_string()
            } else {
                base.join("gradlew").display().to_string()
            };
        }
    }
    "gradle".to_string()
}

fn extract_gradle_custom_tasks(content: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(idx) = trimmed.find("tasks.register") {
            let after = &trimmed[idx..];
            if let Some(name) = extract_first_quoted(after) {
                if !name.is_empty() && !found.contains(&name) {
                    found.push(name);
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("task ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if !name.is_empty() && !found.contains(&name) {
                found.push(name);
            }
        }
        if found.len() >= 4 {
            break;
        }
    }
    found
}

fn detect_maven_configs(
    dir: &Path,
    workspace_root: &Path,
    sub_label: Option<&str>,
    configs: &mut Vec<RunConfiguration>,
) {
    let pom_path = dir.join("pom.xml");
    let Ok(content) = std::fs::read_to_string(&pom_path) else {
        return;
    };

    let mvn_cmd = if cfg!(windows) && dir.join("mvnw.cmd").is_file() {
        ".\\mvnw.cmd".to_string()
    } else if dir.join("mvnw").is_file() {
        "./mvnw".to_string()
    } else if cfg!(windows) && workspace_root.join("mvnw.cmd").is_file() {
        workspace_root.join("mvnw.cmd").display().to_string()
    } else {
        "mvn".to_string()
    };

    let mut goals: Vec<&str> = Vec::new();
    if content.contains("spring-boot-maven-plugin") {
        goals.push("spring-boot:run");
    }
    if content.contains("quarkus-maven-plugin") {
        goals.push("quarkus:dev");
    }
    if content.contains("exec-maven-plugin") || content.contains("<mainClass>") {
        goals.push("exec:java");
    }
    goals.push("test");
    goals.push("package");

    for goal in goals {
        configs.push(RunConfiguration {
            name: suffix_name(format!("Maven: {goal}"), sub_label),
            language: LanguageId::PlainText,
            command: mvn_cmd.clone(),
            args: vec![goal.to_string()],
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: Some(pom_path.clone()),
        });
    }
}

fn detect_cargo_configs(
    dir: &Path,
    sub_label: Option<&str>,
    configs: &mut Vec<RunConfiguration>,
    member_dirs_out: &mut Vec<PathBuf>,
) {
    let cargo_toml = dir.join("Cargo.toml");
    let Ok(raw) = std::fs::read_to_string(&cargo_toml) else {
        return;
    };

    let member_patterns = parse_toml_workspace_members(&raw);
    let is_workspace = raw.contains("[workspace]");

    if is_workspace {
        configs.push(RunConfiguration {
            name: suffix_name("Cargo: check (Workspace)".to_string(), sub_label),
            language: LanguageId::Rust,
            command: "cargo".to_string(),
            args: vec!["check".to_string(), "--workspace".to_string()],
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: Some(cargo_toml.clone()),
        });
        configs.push(RunConfiguration {
            name: suffix_name("Cargo: test (Workspace)".to_string(), sub_label),
            language: LanguageId::Rust,
            command: "cargo".to_string(),
            args: vec!["test".to_string(), "--workspace".to_string()],
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: Some(cargo_toml.clone()),
        });

        let resolved_members = resolve_workspace_member_dirs(dir, &member_patterns);
        for member_dir in resolved_members {
            member_dirs_out.push(member_dir.clone());
            let member_toml = member_dir.join("Cargo.toml");
            if let Ok(m_raw) = std::fs::read_to_string(&member_toml) {
                let pkg_name = parse_toml_package_name(&m_raw);
                let bins = parse_toml_bin_names(&m_raw);
                if !bins.is_empty() {
                    for bin in bins {
                        let mut args = vec!["run".to_string()];
                        if let Some(pkg) = &pkg_name {
                            args.push("-p".to_string());
                            args.push(pkg.clone());
                        }
                        args.push("--bin".to_string());
                        args.push(bin.clone());
                        configs.push(RunConfiguration {
                            name: suffix_name(format!("Cargo: run ({bin})"), sub_label),
                            language: LanguageId::Rust,
                            command: "cargo".to_string(),
                            args,
                            cwd: dir.to_path_buf(),
                            source: RunConfigSource::Detected,
                            target_file: Some(member_toml.clone()),
                        });
                    }
                } else if member_dir.join("src/main.rs").is_file() {
                    if let Some(pkg) = pkg_name {
                        configs.push(RunConfiguration {
                            name: suffix_name(format!("Cargo: run ({pkg})"), sub_label),
                            language: LanguageId::Rust,
                            command: "cargo".to_string(),
                            args: vec!["run".to_string(), "-p".to_string(), pkg],
                            cwd: dir.to_path_buf(),
                            source: RunConfigSource::Detected,
                            target_file: Some(member_dir.join("src/main.rs")),
                        });
                    }
                }
            }
        }
    } else {
        let pkg_name = parse_toml_package_name(&raw);
        let bins = parse_toml_bin_names(&raw);
        if !bins.is_empty() {
            for bin in bins {
                configs.push(RunConfiguration {
                    name: suffix_name(format!("Cargo: run ({bin})"), sub_label),
                    language: LanguageId::Rust,
                    command: "cargo".to_string(),
                    args: vec!["run".to_string(), "--bin".to_string(), bin],
                    cwd: dir.to_path_buf(),
                    source: RunConfigSource::Detected,
                    target_file: Some(cargo_toml.clone()),
                });
            }
        } else if dir.join("src/main.rs").is_file() {
            let label = match &pkg_name {
                Some(p) => format!("Cargo: run ({p})"),
                None => "Cargo: run".to_string(),
            };
            configs.push(RunConfiguration {
                name: suffix_name(label, sub_label),
                language: LanguageId::Rust,
                command: "cargo".to_string(),
                args: vec!["run".to_string()],
                cwd: dir.to_path_buf(),
                source: RunConfigSource::Detected,
                target_file: Some(dir.join("src/main.rs")),
            });
        }
        configs.push(RunConfiguration {
            name: suffix_name("Cargo: check".to_string(), sub_label),
            language: LanguageId::Rust,
            command: "cargo".to_string(),
            args: vec!["check".to_string()],
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: Some(cargo_toml.clone()),
        });
        configs.push(RunConfiguration {
            name: suffix_name("Cargo: test".to_string(), sub_label),
            language: LanguageId::Rust,
            command: "cargo".to_string(),
            args: vec!["test".to_string()],
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: Some(cargo_toml),
        });
    }
}

fn detect_python_configs(
    dir: &Path,
    sub_label: Option<&str>,
    configs: &mut Vec<RunConfiguration>,
) {
    let has_pyproject = dir.join("pyproject.toml").is_file();
    let has_reqs = dir.join("requirements.txt").is_file();
    let has_manage = dir.join("manage.py").is_file();
    let entry_file = ["main.py", "app.py", "run.py", "server.py"]
        .into_iter()
        .find(|f| dir.join(f).is_file());

    if !has_pyproject && !has_reqs && !has_manage && entry_file.is_none() {
        return;
    }

    let (runner, runner_prefix) = detect_python_runner(dir);

    if has_manage {
        let mut run_args = runner_prefix.clone();
        run_args.extend(["manage.py".to_string(), "runserver".to_string()]);
        configs.push(RunConfiguration {
            name: suffix_name("Django: runserver".to_string(), sub_label),
            language: LanguageId::Python,
            command: runner.clone(),
            args: run_args,
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: Some(dir.join("manage.py")),
        });

        let mut test_args = runner_prefix.clone();
        test_args.extend(["manage.py".to_string(), "test".to_string()]);
        configs.push(RunConfiguration {
            name: suffix_name("Django: test".to_string(), sub_label),
            language: LanguageId::Python,
            command: runner.clone(),
            args: test_args,
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: Some(dir.join("manage.py")),
        });
    }

    if let Ok(pyproject_raw) = std::fs::read_to_string(dir.join("pyproject.toml")) {
        for script in parse_toml_section_keys(&pyproject_raw, &["project.scripts", "tool.poetry.scripts"]) {
            let (cmd, args) = if runner != "python" {
                (runner.clone(), vec!["run".to_string(), script.clone()])
            } else {
                (script.clone(), Vec::new())
            };
            configs.push(RunConfiguration {
                name: suffix_name(format!("Python: {script}"), sub_label),
                language: LanguageId::Python,
                command: cmd,
                args,
                cwd: dir.to_path_buf(),
                source: RunConfigSource::Detected,
                target_file: Some(dir.join("pyproject.toml")),
            });
        }
        if (pyproject_raw.contains("uvicorn") || pyproject_raw.contains("fastapi"))
            && entry_file.is_some()
        {
            let mod_name = entry_file
                .unwrap_or("main.py")
                .trim_end_matches(".py");
            let (cmd, args) = if runner != "python" {
                (
                    runner.clone(),
                    vec![
                        "run".to_string(),
                        "uvicorn".to_string(),
                        format!("{mod_name}:app"),
                        "--reload".to_string(),
                    ],
                )
            } else {
                (
                    "uvicorn".to_string(),
                    vec![format!("{mod_name}:app"), "--reload".to_string()],
                )
            };
            configs.push(RunConfiguration {
                name: suffix_name("Uvicorn: dev".to_string(), sub_label),
                language: LanguageId::Python,
                command: cmd,
                args,
                cwd: dir.to_path_buf(),
                source: RunConfigSource::Detected,
                target_file: Some(dir.join(entry_file.unwrap_or("main.py"))),
            });
        }
    }

    if let Some(entry) = entry_file {
        let mut args = runner_prefix.clone();
        args.push(entry.to_string());
        configs.push(RunConfiguration {
            name: suffix_name(format!("Python: {entry}"), sub_label),
            language: LanguageId::Python,
            command: runner.clone(),
            args,
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: Some(dir.join(entry)),
        });
    }

    if dir.join("tests").is_dir() || has_pyproject {
        let (cmd, args) = if runner != "python" {
            (runner, vec!["run".to_string(), "pytest".to_string()])
        } else {
            ("pytest".to_string(), Vec::new())
        };
        configs.push(RunConfiguration {
            name: suffix_name("Pytest".to_string(), sub_label),
            language: LanguageId::Python,
            command: cmd,
            args,
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: None,
        });
    }
}

fn detect_python_runner(dir: &Path) -> (String, Vec<String>) {
    if dir.join("uv.lock").is_file() {
        ("uv".to_string(), vec!["run".to_string(), "python".to_string()])
    } else if dir.join("poetry.lock").is_file() {
        (
            "poetry".to_string(),
            vec!["run".to_string(), "python".to_string()],
        )
    } else {
        ("python".to_string(), Vec::new())
    }
}

fn detect_go_configs(
    dir: &Path,
    sub_label: Option<&str>,
    configs: &mut Vec<RunConfiguration>,
) {
    let has_mod = dir.join("go.mod").is_file();
    let has_main = dir.join("main.go").is_file();
    if !has_mod && !has_main {
        return;
    }

    if has_main {
        configs.push(RunConfiguration {
            name: suffix_name("Go: run .".to_string(), sub_label),
            language: LanguageId::PlainText,
            command: "go".to_string(),
            args: vec!["run".to_string(), ".".to_string()],
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: Some(dir.join("main.go")),
        });
    }

    if let Ok(cmd_entries) = std::fs::read_dir(dir.join("cmd")) {
        let mut cmd_dirs: Vec<PathBuf> = cmd_entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.join("main.go").is_file())
            .collect();
        cmd_dirs.sort();
        for cdir in cmd_dirs {
            if let Some(name) = cdir.file_name().and_then(|n| n.to_str()) {
                configs.push(RunConfiguration {
                    name: suffix_name(format!("Go: run ({name})"), sub_label),
                    language: LanguageId::PlainText,
                    command: "go".to_string(),
                    args: vec!["run".to_string(), format!("./cmd/{name}")],
                    cwd: dir.to_path_buf(),
                    source: RunConfigSource::Detected,
                    target_file: Some(cdir.join("main.go")),
                });
            }
        }
    }

    configs.push(RunConfiguration {
        name: suffix_name("Go: test ./...".to_string(), sub_label),
        language: LanguageId::PlainText,
        command: "go".to_string(),
        args: vec!["test".to_string(), "./...".to_string()],
        cwd: dir.to_path_buf(),
        source: RunConfigSource::Detected,
        target_file: Some(dir.join("go.mod")),
    });
    configs.push(RunConfiguration {
        name: suffix_name("Go: build ./...".to_string(), sub_label),
        language: LanguageId::PlainText,
        command: "go".to_string(),
        args: vec!["build".to_string(), "./...".to_string()],
        cwd: dir.to_path_buf(),
        source: RunConfigSource::Detected,
        target_file: Some(dir.join("go.mod")),
    });
}

fn detect_dotnet_configs(
    dir: &Path,
    sub_label: Option<&str>,
    configs: &mut Vec<RunConfiguration>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let project_file = entries
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("csproj") || e.eq_ignore_ascii_case("sln"))
        });
    let Some(proj) = project_file else {
        return;
    };
    for sub in ["run", "test", "build"] {
        configs.push(RunConfiguration {
            name: suffix_name(format!("dotnet: {sub}"), sub_label),
            language: LanguageId::PlainText,
            command: "dotnet".to_string(),
            args: vec![sub.to_string()],
            cwd: dir.to_path_buf(),
            source: RunConfigSource::Detected,
            target_file: Some(proj.clone()),
        });
    }
}

fn detect_make_and_just_configs(
    dir: &Path,
    sub_label: Option<&str>,
    configs: &mut Vec<RunConfiguration>,
) {
    if let Some(mk_path) = ["Makefile", "makefile", "GNUmakefile"]
        .into_iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file())
    {
        if let Ok(raw) = std::fs::read_to_string(&mk_path) {
            for target in extract_makefile_targets(&raw, false).into_iter().take(6) {
                configs.push(RunConfiguration {
                    name: suffix_name(format!("Make: {target}"), sub_label),
                    language: LanguageId::PlainText,
                    command: "make".to_string(),
                    args: vec![target],
                    cwd: dir.to_path_buf(),
                    source: RunConfigSource::Detected,
                    target_file: Some(mk_path.clone()),
                });
            }
        }
    }

    if let Some(just_path) = ["justfile", "Justfile"]
        .into_iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file())
    {
        if let Ok(raw) = std::fs::read_to_string(&just_path) {
            for target in extract_makefile_targets(&raw, true).into_iter().take(6) {
                configs.push(RunConfiguration {
                    name: suffix_name(format!("Just: {target}"), sub_label),
                    language: LanguageId::PlainText,
                    command: "just".to_string(),
                    args: vec![target],
                    cwd: dir.to_path_buf(),
                    source: RunConfigSource::Detected,
                    target_file: Some(just_path.clone()),
                });
            }
        }
    }
}

fn extract_makefile_targets(raw: &str, is_just: bool) -> Vec<String> {
    let priority = ["dev", "run", "start", "up", "serve", "build", "test", "clean"];
    let mut all = Vec::new();
    for line in raw.lines() {
        if line.starts_with(char::is_whitespace) || line.trim().is_empty() {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.starts_with('.') {
            continue;
        }
        if is_just && trimmed.starts_with("set ") {
            continue;
        }
        let Some((lhs, _)) = trimmed.split_once(':') else {
            continue;
        };
        if lhs.contains('=') || lhs.contains('$') || lhs.contains('%') {
            continue;
        }
        let target = lhs.split_whitespace().next().unwrap_or("");
        if !target.is_empty()
            && target
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            && !all.iter().any(|t| t == target)
        {
            all.push(target.to_string());
        }
    }

    let mut ordered = Vec::new();
    for pri in priority {
        if let Some(pos) = all.iter().position(|t| t == pri) {
            ordered.push(all.remove(pos));
        }
    }
    ordered.extend(all);
    ordered
}

fn cargo_config_for_rust_file(file_path: &Path, workspace_root: &Path) -> RunConfiguration {
    let file_name = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("main.rs");
    let stem = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("bin");

    // Walk upwards to find owning Cargo.toml
    let mut cursor = file_path.parent();
    while let Some(dir) = cursor {
        let toml_path = dir.join("Cargo.toml");
        if toml_path.is_file() {
            let raw = std::fs::read_to_string(&toml_path).unwrap_or_default();
            if let Some(pkg) = parse_toml_package_name(&raw) {
                let rel_in_crate = file_path
                    .strip_prefix(dir)
                    .unwrap_or(file_path)
                    .to_string_lossy()
                    .replace('\\', "/");

                let mut args = if rel_in_crate.starts_with("tests/") {
                    vec!["test".to_string(), "-p".to_string(), pkg.clone(), "--test".to_string(), stem.to_string()]
                } else if rel_in_crate.starts_with("examples/") {
                    vec!["run".to_string(), "-p".to_string(), pkg.clone(), "--example".to_string(), stem.to_string()]
                } else if rel_in_crate.starts_with("src/bin/") {
                    vec!["run".to_string(), "-p".to_string(), pkg.clone(), "--bin".to_string(), stem.to_string()]
                } else if rel_in_crate == "src/main.rs" {
                    vec!["run".to_string(), "-p".to_string(), pkg.clone()]
                } else {
                    let src = std::fs::read_to_string(file_path).unwrap_or_default();
                    if src.contains("#[test]") || src.contains("#[tokio::test]") {
                        vec!["test".to_string(), "-p".to_string(), pkg.clone()]
                    } else if dir.join("src/main.rs").is_file() {
                        vec!["run".to_string(), "-p".to_string(), pkg.clone()]
                    } else {
                        vec!["test".to_string(), "-p".to_string(), pkg.clone()]
                    }
                };
                if paths_equivalent(dir, workspace_root) {
                    if let Some(p_idx) = args.iter().position(|a| a == "-p") {
                        args.drain(p_idx..=(p_idx + 1).min(args.len() - 1));
                    }
                }
                return RunConfiguration {
                    name: format!("Cargo: {file_name}"),
                    language: LanguageId::Rust,
                    command: "cargo".to_string(),
                    args,
                    cwd: workspace_root.to_path_buf(),
                    source: RunConfigSource::Custom,
                    target_file: Some(file_path.to_path_buf()),
                };
            }
        }
        if paths_equivalent(dir, workspace_root) {
            break;
        }
        cursor = dir.parent();
    }

    RunConfiguration {
        name: format!("Rust: {file_name}"),
        language: LanguageId::Rust,
        command: "rustc".to_string(),
        args: vec![relative_display_path(file_path, workspace_root)],
        cwd: workspace_root.to_path_buf(),
        source: RunConfigSource::Custom,
        target_file: Some(file_path.to_path_buf()),
    }
}

fn parse_toml_package_name(raw: &str) -> Option<String> {
    let mut section = String::new();
    for line in raw.lines() {
        let trimmed = strip_toml_comment(line).trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed.to_string();
            continue;
        }
        if section == "[package]" {
            if let Some((k, v)) = trimmed.split_once('=') {
                if k.trim() == "name" {
                    return extract_first_quoted(v);
                }
            }
        }
    }
    None
}

fn parse_toml_bin_names(raw: &str) -> Vec<String> {
    let mut bins = Vec::new();
    let mut in_bin = false;
    for line in raw.lines() {
        let trimmed = strip_toml_comment(line).trim();
        if trimmed.starts_with('[') {
            in_bin = trimmed == "[[bin]]";
            continue;
        }
        if in_bin {
            if let Some((k, v)) = trimmed.split_once('=') {
                if k.trim() == "name" {
                    if let Some(name) = extract_first_quoted(v) {
                        if !name.is_empty() && !bins.contains(&name) {
                            bins.push(name);
                        }
                    }
                }
            }
        }
    }
    bins
}

fn parse_toml_section_keys(raw: &str, sections: &[&str]) -> Vec<String> {
    let mut keys = Vec::new();
    let mut current_section = String::new();
    for line in raw.lines() {
        let trimmed = strip_toml_comment(line).trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_section = trimmed.trim_matches(|c| c == '[' || c == ']').to_string();
            continue;
        }
        if sections.iter().any(|s| *s == current_section) {
            if let Some((k, _)) = trimmed.split_once('=') {
                let key = k.trim().trim_matches('"').trim_matches('\'');
                if !key.is_empty() && !keys.iter().any(|existing| existing == key) {
                    keys.push(key.to_string());
                }
            }
        }
    }
    keys
}

fn parse_toml_workspace_members(raw: &str) -> Vec<String> {
    let mut members = Vec::new();
    let mut section = String::new();
    let mut collecting = false;
    let mut buf = String::new();

    for line in raw.lines() {
        let trimmed = strip_toml_comment(line).trim();
        if !collecting && trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed.to_string();
            continue;
        }
        if section == "[workspace]" {
            if !collecting {
                if let Some((k, v)) = trimmed.split_once('=') {
                    if k.trim() == "members" {
                        buf.push_str(v);
                        if v.contains(']') {
                            collect_quoted_strings(&buf, &mut members);
                            buf.clear();
                        } else {
                            collecting = true;
                        }
                    }
                }
            } else {
                buf.push(' ');
                buf.push_str(trimmed);
                if trimmed.contains(']') {
                    collect_quoted_strings(&buf, &mut members);
                    buf.clear();
                    collecting = false;
                }
            }
        }
    }
    members
}

fn resolve_workspace_member_dirs(root: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for pat in patterns {
        let clean = pat.trim_matches('/').replace('\\', "/");
        if let Some(prefix) = clean.strip_suffix("/*") {
            let parent = if prefix.is_empty() {
                root.to_path_buf()
            } else {
                root.join(prefix)
            };
            if let Ok(entries) = std::fs::read_dir(parent) {
                let mut children: Vec<PathBuf> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir() && p.join("Cargo.toml").is_file())
                    .collect();
                children.sort();
                dirs.extend(children);
            }
        } else {
            let candidate = root.join(&clean);
            if candidate.join("Cargo.toml").is_file() {
                dirs.push(candidate);
            }
        }
    }
    dirs
}

fn strip_toml_comment(line: &str) -> &str {
    let mut in_dq = false;
    let mut in_sq = false;
    for (idx, ch) in line.char_indices() {
        match ch {
            '"' if !in_sq => in_dq = !in_dq,
            '\'' if !in_dq => in_sq = !in_sq,
            '#' if !in_dq && !in_sq => return &line[..idx],
            _ => {}
        }
    }
    line
}

fn extract_first_quoted(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'"' || b == b'\'' {
            let quote = b;
            if let Some(end_rel) = bytes[i + 1..].iter().position(|&c| c == quote) {
                return Some(s[i + 1..i + 1 + end_rel].to_string());
            }
        }
    }
    None
}

fn collect_quoted_strings(s: &str, out: &mut Vec<String>) {
    let mut rest = s;
    while let Some(start) = rest.find(['"', '\'']) {
        let quote = rest.as_bytes()[start] as char;
        let after = &rest[start + 1..];
        if let Some(end) = after.find(quote) {
            out.push(after[..end].to_string());
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
}

/// Splits a shell command line into executable and arguments while preserving single/double quotes.
pub fn parse_shell_command_parts(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut has_token = false;

    for ch in input.chars() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
                has_token = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                has_token = true;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if has_token {
                    parts.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        parts.push(current);
    }
    parts
}

fn normalize_cmd_token(cmd: &str) -> String {
    let trimmed = cmd
        .trim()
        .trim_start_matches(".\\")
        .trim_start_matches("./");
    let file_part = Path::new(trimmed)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(trimmed);
    let lower = file_part.to_ascii_lowercase();
    lower
        .strip_suffix(".exe")
        .or_else(|| lower.strip_suffix(".cmd"))
        .or_else(|| lower.strip_suffix(".bat"))
        .unwrap_or(&lower)
        .to_string()
}

fn infer_language_from_command(command: &str, args: &[String]) -> LanguageId {
    match normalize_cmd_token(command).as_str() {
        "cargo" | "rustc" | "rustup" => LanguageId::Rust,
        "tsx" | "ts-node" | "tsc" | "deno" | "vite" | "next" => LanguageId::TypeScript,
        "npm" | "pnpm" | "yarn" | "bun" | "npx" | "node" | "nodemon" => {
            if args
                .iter()
                .any(|a| a.ends_with(".ts") || a.ends_with(".tsx") || a == "tsx")
            {
                LanguageId::TypeScript
            } else {
                LanguageId::JavaScript
            }
        }
        "python" | "python3" | "py" | "pytest" | "uv" | "poetry" | "uvicorn" | "flask" => {
            LanguageId::Python
        }
        _ => LanguageId::PlainText,
    }
}

fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    #[cfg(target_os = "windows")]
    if let Some(s) = path.to_str() {
        if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{stripped}"));
        }
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    path
}

fn normalize_for_prefix(path: &Path) -> PathBuf {
    if let Ok(canon) = std::fs::canonicalize(path) {
        return strip_verbatim_prefix(canon);
    }
    let mut suffix = Vec::new();
    let mut cur = path;
    while let Some(name) = cur.file_name() {
        suffix.push(name.to_os_string());
        let Some(parent) = cur.parent() else {
            break;
        };
        if let Ok(canon_parent) = std::fs::canonicalize(parent) {
            let mut resolved = strip_verbatim_prefix(canon_parent);
            for part in suffix.into_iter().rev() {
                resolved.push(part);
            }
            return resolved;
        }
        cur = parent;
    }
    path.to_path_buf()
}

fn relative_display_path(file_path: &Path, workspace_root: &Path) -> String {
    if let Ok(rel) = file_path.strip_prefix(workspace_root) {
        return rel.to_string_lossy().replace('\\', "/");
    }
    let norm_file = normalize_for_prefix(file_path);
    let norm_root = normalize_for_prefix(workspace_root);
    norm_file
        .strip_prefix(&norm_root)
        .unwrap_or(file_path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn sanitize_docker_tag(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() || lower == '-' || lower == '_' || lower == '.' {
            out.push(lower);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '.');
    if trimmed.is_empty() {
        "app".to_string()
    } else {
        trimmed.to_string()
    }
}

fn truncate_display(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let mut prefix: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        prefix.push('…');
        prefix
    }
}

fn paths_equivalent(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

fn dedup_preserve_order(configs: Vec<RunConfiguration>) -> Vec<RunConfiguration> {
    let mut out: Vec<RunConfiguration> = Vec::new();
    for cfg in configs {
        if !out.iter().any(|existing| existing.same_identity(&cfg)) {
            out.push(cfg);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_npm_pnpm_docker_gradle_maven_python_go_make() {
        let unique = format!(
            "ide-core-run-detect-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("frontend")).unwrap();

        std::fs::write(
            root.join("package.json"),
            r#"{
  "name": "demo",
  "scripts": {
    "dev": "vite --port 5173",
    "build": "vite build",
    "custom-serve": "node server.js"
  }
}"#,
        )
        .unwrap();
        std::fs::write(root.join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(
            root.join("frontend/package.json"),
            r#"{ "scripts": { "dev": "next dev" } }"#,
        )
        .unwrap();

        std::fs::write(
            root.join("docker-compose.yml"),
            "services:\n  web:\n    build: .\n",
        )
        .unwrap();
        std::fs::write(
            root.join("Dockerfile"),
            "FROM node:20\nEXPOSE 8080/tcp\nCMD [\"npm\", \"start\"]\n",
        )
        .unwrap();

        std::fs::write(
            root.join("build.gradle.kts"),
            "plugins { id(\"org.springframework.boot\") }\ntasks.register(\"seedDb\") {}\n",
        )
        .unwrap();
        std::fs::write(root.join("gradlew.bat"), "@echo off\n").unwrap();
        std::fs::write(root.join("gradlew"), "#!/bin/sh\n").unwrap();

        std::fs::write(
            root.join("pom.xml"),
            "<project><build><plugins><plugin><artifactId>spring-boot-maven-plugin</artifactId></plugin></plugins></build></project>",
        )
        .unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"polyglot\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("manage.py"), "print('django')\n").unwrap();
        std::fs::write(root.join("go.mod"), "module demo\n").unwrap();
        std::fs::write(root.join("main.go"), "package main\nfunc main() {}\n").unwrap();
        std::fs::write(root.join("Makefile"), "dev:\n\techo dev\nbuild:\n\techo build\n").unwrap();

        let detected = RunConfiguration::detect_for_workspace(&root);

        assert!(detected
            .iter()
            .any(|c| c.command == "pnpm" && c.args == vec!["run", "dev"] && c.cwd == root));
        assert!(detected
            .iter()
            .any(|c| c.command == "pnpm" && c.args == vec!["run", "custom-serve"]));
        assert!(detected.iter().any(|c| c.command == "pnpm"
            && c.args == vec!["run", "dev"]
            && c.cwd == root.join("frontend")));
        assert!(detected
            .iter()
            .any(|c| c.command == "docker" && c.args == vec!["compose", "up", "-d"]));
        assert!(detected.iter().any(|c| c.command == "docker"
            && c.args.contains(&"build".to_string())
            && c.args.iter().any(|a| a.ends_with(":latest"))));
        assert!(detected.iter().any(|c| c.command == "docker"
            && c.args.contains(&"run".to_string())
            && c.args.contains(&"8080:8080".to_string())));
        assert!(detected
            .iter()
            .any(|c| c.command.contains("gradlew") && c.args == vec!["bootRun"]));
        assert!(detected
            .iter()
            .any(|c| c.command.contains("gradlew") && c.args == vec!["seedDb"]));
        assert!(detected
            .iter()
            .any(|c| c.command == "mvn" && c.args == vec!["spring-boot:run"]));
        assert!(detected
            .iter()
            .any(|c| c.command == "cargo" && c.args == vec!["run"]));
        assert!(detected
            .iter()
            .any(|c| c.command == "python" && c.args == vec!["manage.py", "runserver"]));
        assert!(detected
            .iter()
            .any(|c| c.command == "go" && c.args == vec!["run", "."]));
        assert!(detected
            .iter()
            .any(|c| c.command == "make" && c.args == vec!["dev"]));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_for_file_and_terminal_command_parsing() {
        let unique = format!(
            "ide-core-run-file-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(root.join("crates/cli/src")).unwrap();
        std::fs::write(
            root.join("crates/cli/Cargo.toml"),
            "[package]\nname = \"my_cli\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let rs_main = root.join("crates/cli/src/main.rs");
        std::fs::write(&rs_main, "fn main() {}\n").unwrap();

        let py_cfg = RunConfiguration::for_file(&root.join("scripts/job.py"), &root).unwrap();
        assert_eq!(py_cfg.command, "python");
        assert_eq!(py_cfg.args, vec!["scripts/job.py"]);
        assert_eq!(py_cfg.source, RunConfigSource::Custom);

        let rs_cfg = RunConfiguration::for_file(&rs_main, &root).unwrap();
        assert_eq!(rs_cfg.command, "cargo");
        assert_eq!(rs_cfg.args, vec!["run", "-p", "my_cli"]);

        assert!(RunConfiguration::from_terminal_command("cls", &root).is_none());
        assert!(RunConfiguration::from_terminal_command("cd src", &root).is_none());
        assert!(RunConfiguration::from_terminal_command("dir", &root).is_none());

        let cli = RunConfiguration::from_terminal_command(
            "python \"my script.py\" --port 8080",
            &root,
        )
        .unwrap();
        assert_eq!(cli.command, "python");
        assert_eq!(cli.args, vec!["my script.py", "--port", "8080"]);
        assert_eq!(cli.language, LanguageId::Python);
        assert_eq!(cli.source, RunConfigSource::Custom);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_flutter_android_run_config_and_device_binding() {
        let unique = format!(
            "ide-core-mobile-rc-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::create_dir_all(root.join("test")).unwrap();
        std::fs::create_dir_all(root.join("android/app/src/main")).unwrap();
        std::fs::write(
            root.join("pubspec.yaml"),
            "name: my_flutter_app\ndependencies:\n  flutter:\n    sdk: flutter\n",
        )
        .unwrap();
        std::fs::write(root.join("lib/main.dart"), "void main() {}\n").unwrap();
        std::fs::write(root.join("test/widget_test.dart"), "void main() {}\n").unwrap();
        std::fs::write(
            root.join("android/app/src/main/AndroidManifest.xml"),
            "<manifest package=\"com.example.app\"/>\n",
        )
        .unwrap();
        std::fs::write(
            root.join("android/build.gradle"),
            "// root gradle\n",
        )
        .unwrap();
        std::fs::write(
            root.join("android/app/build.gradle"),
            "plugins { id 'com.android.application' }\n",
        )
        .unwrap();

        let detected = RunConfiguration::detect_for_workspace(&root);
        let flutter_run = detected
            .iter()
            .find(|c| c.command == "flutter" && c.args == vec!["run"])
            .expect("Flutter run config");
        assert!(detected
            .iter()
            .any(|c| c.command == "flutter" && c.args == vec!["test"]));
        assert!(detected
            .iter()
            .any(|c| c.command == "flutter" && c.args == vec!["build", "apk"]));
        assert!(detected
            .iter()
            .any(|c| c.command.contains("gradle") && c.args == vec!["installDebug"]));

        // Inject selected ADB / Flutter device id cleanly
        let bound = flutter_run.effective_with_mobile_device(Some("Pixel_8_API_34"));
        assert_eq!(bound.args, vec!["run", "-d", "Pixel_8_API_34"]);
        assert_eq!(
            flutter_run.effective_with_mobile_device(Some("avd:Medium_Phone")).args,
            vec!["run"]
        );

        let dart_main = RunConfiguration::for_file(&root.join("lib/main.dart"), &root).unwrap();
        assert_eq!(dart_main.command, "flutter");
        assert_eq!(dart_main.args, vec!["run", "-t", "lib/main.dart"]);

        let dart_test =
            RunConfiguration::for_file(&root.join("test/widget_test.dart"), &root).unwrap();
        assert_eq!(dart_test.command, "flutter");
        assert_eq!(dart_test.args, vec!["test", "test/widget_test.dart"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn relative_display_path_handles_uncanonical_and_canonical_mismatch() {
        let unique = format!(
            "ide-core-rel-norm-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let sub = root.join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        let py_file = root.join("worker.py");
        std::fs::write(&py_file, "print('ok')\n").unwrap();

        let canon_root = normalize_for_prefix(&root);
        let root_name = root.file_name().unwrap();
        let uncanonical_file = root.join("..").join(root_name).join("worker.py");
        let cfg = RunConfiguration::for_file(&uncanonical_file, &canon_root).unwrap();
        assert_eq!(cfg.command, "python");
        assert_eq!(cfg.args, vec!["worker.py"]);

        let _ = std::fs::remove_dir_all(&root);
    }
}

