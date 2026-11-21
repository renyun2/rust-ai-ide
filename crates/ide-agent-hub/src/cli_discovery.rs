use crate::types::{AgentProviderKind, AgentRegistration, ReasoningEffort};
use ide_lsp::find_executable_in_path;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

pub fn push_unique_model(list: &mut Vec<String>, model: impl AsRef<str>) {
    let trimmed = model.as_ref().trim();
    if trimmed.is_empty() {
        return;
    }
    if !list.iter().any(|m| m.eq_ignore_ascii_case(trimmed)) {
        list.push(trimmed.to_string());
    }
}

/// 将 Cursor CLI 的带后缀模型 ID（如 `claude-opus-5-5-high-fast`、`gemini-4-argon-medium`）归纳为基础模型族名
pub fn normalize_cursor_base_model(id: &str) -> String {
    let mut s = id.trim().to_string();
    if s.eq_ignore_ascii_case("auto") || s.eq_ignore_ascii_case("default") {
        return "auto".to_string();
    }
    if let Some(rest) = s.strip_suffix("-fast") {
        s = rest.to_string();
    }
    for suf in [
        "-extra-high",
        "-minimal",
        "-medium",
        "-xhigh",
        "-high",
        "-none",
        "-low",
        "-max",
    ] {
        if let Some(rest) = s.strip_suffix(suf) {
            s = rest.to_string();
            break;
        }
    }
    s
}

/// 依据基础模型名 + 思考深度 + 快速模式，在 Cursor CLI 真实支持模型目录中精确匹配最终 `--model` 参数
pub fn resolve_cursor_model_id(
    selected: &str,
    effort: ReasoningEffort,
    fast_mode: bool,
    exact_catalog: &[String],
) -> String {
    let trimmed = selected.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") || trimmed.eq_ignore_ascii_case("default") {
        return "auto".to_string();
    }
    if exact_catalog.is_empty() {
        return if fast_mode && !trimmed.ends_with("-fast") {
            format!("{trimmed}-fast")
        } else {
            trimmed.to_string()
        };
    }

    let base = normalize_cursor_base_model(trimmed);
    let eff = effort.as_cli_str();
    let mut candidates = Vec::new();
    let effort_fallbacks: Vec<&str> = match effort {
        ReasoningEffort::Max => vec!["max", "xhigh", "high", "medium", "low"],
        ReasoningEffort::ExtraHigh => vec!["xhigh", "extra-high", "high", "medium", "low"],
        ReasoningEffort::High => vec!["high", "medium", "xhigh", "low"],
        ReasoningEffort::Medium => vec!["medium", "high", "low"],
        ReasoningEffort::Low => vec!["low", "medium", "high"],
    };

    if fast_mode {
        for e in &effort_fallbacks {
            candidates.push(format!("{base}-{e}-fast"));
            candidates.push(format!("{base}-thinking-{e}-fast"));
        }
        candidates.push(format!("{base}-fast"));
    }
    for e in &effort_fallbacks {
        candidates.push(format!("{base}-{e}"));
        candidates.push(format!("{base}-thinking-{e}"));
    }
    candidates.push(base.clone());
    candidates.push(trimmed.to_string());

    for cand in &candidates {
        if let Some(hit) = exact_catalog.iter().find(|m| m.eq_ignore_ascii_case(cand)) {
            return hit.clone();
        }
    }
    if let Some(hit) = exact_catalog
        .iter()
        .find(|m| m.eq_ignore_ascii_case(&format!("{base}-{eff}")))
    {
        return hit.clone();
    }
    // 前缀模糊匹配（比如 `claude-opus-5-thinking` -> `claude-opus-5-thinking-high-fast`）
    if fast_mode {
        if let Some(hit) = exact_catalog
            .iter()
            .find(|m| m.starts_with(&base) && m.ends_with("-fast"))
        {
            return hit.clone();
        }
    }
    exact_catalog
        .iter()
        .find(|m| m.starts_with(&base))
        .cloned()
        .unwrap_or_else(|| trimmed.to_string())
}

/// 解析 Antigravity (`agy.exe`) 的模型名与 `--effort`（注意：带 `(Medium)`/`(High)` 后缀的模型不能再额外传 `--effort`）
pub fn resolve_agy_model_and_effort(
    selected: &str,
    effort: ReasoningEffort,
    exact_catalog: &[String],
) -> (String, Option<&'static str>) {
    let trimmed = selected.trim();
    if trimmed.is_empty() {
        return ("Gemini 4 Argon (Medium)".to_string(), None);
    }

    let effort_paren = match effort {
        ReasoningEffort::Low => "Low",
        ReasoningEffort::Medium => "Medium",
        ReasoningEffort::High => "High",
        ReasoningEffort::ExtraHigh | ReasoningEffort::Max => "XHigh",
    };

    // 剥离尾部括号 `(Medium)` 提取纯模型族名
    let family = if let Some(idx) = trimmed.rfind(" (") {
        if trimmed.ends_with(')') {
            trimmed[..idx].trim()
        } else {
            trimmed
        }
    } else {
        trimmed
    };

    if !exact_catalog.is_empty() {
        let mut try_suffixes = vec![effort_paren];
        if matches!(effort, ReasoningEffort::ExtraHigh | ReasoningEffort::Max) {
            try_suffixes.push("High");
        }
        try_suffixes.extend_from_slice(&["Medium", "High", "Low"]);
        for suf in try_suffixes {
            let cand = format!("{family} ({suf})");
            if let Some(hit) = exact_catalog.iter().find(|m| m.eq_ignore_ascii_case(&cand)) {
                return (hit.clone(), None);
            }
        }
        if let Some(hit) = exact_catalog.iter().find(|m| m.eq_ignore_ascii_case(trimmed)) {
            let has_paren = hit.ends_with(')') && hit.contains(" (");
            return (
                hit.clone(),
                if has_paren {
                    None
                } else {
                    Some(effort.as_cli_str())
                },
            );
        }
    }

    let has_paren = trimmed.ends_with(')') && trimmed.contains(" (");
    (
        trimmed.to_string(),
        if has_paren {
            None
        } else {
            Some(effort.as_cli_str())
        },
    )
}

/// 解析 Cursor CLI 安装目录下的最新 `versions/<ver>/node.exe` 与 `index.js`，绕过 `.cmd` -> `.ps1` 双层 Shell 转义问题
pub fn resolve_cursor_node_entry(bin: &Path) -> Option<(PathBuf, PathBuf)> {
    let base_dir = bin.parent()?;
    let direct_node = base_dir.join("node.exe");
    let direct_js = base_dir.join("index.js");
    if direct_node.exists() && direct_js.exists() {
        return Some((direct_node, direct_js));
    }
    let versions_dir = base_dir.join("versions");
    let rd = fs::read_dir(&versions_dir).ok()?;
    let mut dirs: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
        .filter(|(name, p)| {
            !name.eq_ignore_ascii_case("staged")
                && p.join("node.exe").exists()
                && p.join("index.js").exists()
        })
        .collect();
    dirs.sort_by(|a, b| b.0.cmp(&a.0));
    dirs.into_iter()
        .next()
        .map(|(_, p)| (p.join("node.exe"), p.join("index.js")))
}

/// 扫描 `~/.codex/ccr-model-catalog.json` 与 `~/.codex/cc-switch-model-catalog.json` 中的模型列表
fn read_shared_local_model_catalogs(home: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let codex_dir = home.join(".codex");
    for fname in ["ccr-model-catalog.json", "cc-switch-model-catalog.json"] {
        let p = codex_dir.join(fname);
        if let Ok(raw) = fs::read_to_string(&p) {
            for line in raw.lines() {
                let t = line.trim();
                for key in ["\"slug\"", "\"model\""] {
                    if let Some(rest) = t.strip_prefix(key) {
                        if let Some(colon) = rest.find(':') {
                            let val = rest[colon + 1..]
                                .trim()
                                .trim_end_matches(',')
                                .trim()
                                .trim_matches('"');
                            push_unique_model(&mut out, val);
                        }
                    }
                }
            }
        }
    }
    out
}

/// 剥离非标准 JSON 中的尾随逗号（`,}` / `,]`），兼容 `.cursor/cli-config.json` 等本地配置文件
pub(crate) fn strip_json_trailing_commas(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut in_str = false;
    let mut escaped = false;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if in_str {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            in_str = true;
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                i += 1;
                continue;
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}

/// 自动发现本机所有已安装的 AI CLI（Cursor `cursor-agent` / Antigravity `agy` / Claude `claude` / Codex `codex`）及其模型、快速模式与思考深度
pub fn discover_local_cli_registrations() -> Vec<AgentRegistration> {
    let home = user_home_dir();
    let shared_catalog_models = home
        .as_deref()
        .map(read_shared_local_model_catalogs)
        .unwrap_or_default();

    // 1. Cursor Agent CLI (`cursor-agent.cmd` / `agent.cmd`)
    let cursor_path = find_executable_in_path("cursor-agent.cmd")
        .or_else(|| find_executable_in_path("cursor-agent.exe"))
        .or_else(|| find_executable_in_path("cursor-agent"))
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(|p| {
                    PathBuf::from(p)
                        .join("cursor-agent")
                        .join("cursor-agent.cmd")
                })
                .filter(|p| p.exists())
        })
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(|p| PathBuf::from(p).join("cursor-agent").join("agent.cmd"))
                .filter(|p| p.exists())
        });
    let cursor_cfg_path = home
        .as_ref()
        .map(|h| h.join(".cursor").join("cli-config.json"))
        .filter(|p| p.exists());

    let mut cursor_models = Vec::new();
    let mut cursor_fast = true;
    let mut cursor_effort = ReasoningEffort::High;

    if let Some(cfg_p) = &cursor_cfg_path {
        if let Ok(raw) = fs::read_to_string(cfg_p) {
            let cleaned = strip_json_trailing_commas(&raw);
            if let Ok(v) = serde_json::from_str::<Value>(&cleaned) {
                if let Some(sel_id) = v["selectedModel"]["modelId"]
                    .as_str()
                    .or_else(|| v["model"]["modelId"].as_str())
                {
                    push_unique_model(&mut cursor_models, normalize_cursor_base_model(sel_id));
                }
                if let Some(params) = v["selectedModel"]["parameters"].as_array() {
                    for p in params {
                        let id = p["id"].as_str().unwrap_or("");
                        let val = p["value"].as_str().unwrap_or("");
                        if id == "fast" {
                            cursor_fast = val.eq_ignore_ascii_case("true");
                        } else if matches!(id, "effort" | "reasoning_effort" | "reasoning") {
                            cursor_effort = ReasoningEffort::from_str_lossy(val);
                        }
                    }
                }
                if let Some(hist) = v["modelSelectionHistory"].as_array() {
                    for item in hist {
                        if let Some(s) = item.as_str() {
                            if !s.eq_ignore_ascii_case("default") {
                                push_unique_model(
                                    &mut cursor_models,
                                    normalize_cursor_base_model(s),
                                );
                            }
                        }
                    }
                }
                if let Some(mp) = v["modelParameters"].as_object() {
                    for key in mp.keys() {
                        if !key.eq_ignore_ascii_case("default") {
                            push_unique_model(
                                &mut cursor_models,
                                normalize_cursor_base_model(key),
                            );
                        }
                    }
                }
            } else if let Some(m) = extract_json_string_field(&raw, "modelId") {
                push_unique_model(&mut cursor_models, normalize_cursor_base_model(&m));
            }
        }
    }
    for preset in [
        "auto",
        "composer-2.5",
        "claude-opus-5-5",
        "claude-opus-5-thinking",
        "claude-sonnet-5-thinking",
        "gpt-5.6-sol",
        "gpt-5.3-codex",
        "gemini-4-hydrogen",
        "gemini-4-argon",
        "gemini-3.8-flash",
        "grok-4.7",
    ] {
        push_unique_model(&mut cursor_models, preset);
    }
    let cursor_active_model = cursor_models
        .first()
        .cloned()
        .unwrap_or_else(|| "composer-2.5".to_string());

    // 2. Antigravity CLI (`agy.exe` / `agy`)
    let agy_path = find_executable_in_path("agy.exe")
        .or_else(|| find_executable_in_path("agy.cmd"))
        .or_else(|| find_executable_in_path("agy"))
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(|p| PathBuf::from(p).join("agy").join("bin").join("agy.exe"))
                .filter(|p| p.exists())
        })
        .or_else(|| {
            home.as_ref()
                .map(|h| {
                    h.join(".gemini")
                        .join("antigravity-cli")
                        .join("bin")
                        .join("agy.exe")
                })
                .filter(|p| p.exists())
        });
    let agy_cfg_path = home
        .as_ref()
        .map(|h| {
            h.join(".gemini")
                .join("antigravity-cli")
                .join("settings.json")
        })
        .filter(|p| p.exists());
    let mut agy_models = Vec::new();
    let mut agy_effort = ReasoningEffort::Medium;
    if let Some(cfg_p) = &agy_cfg_path {
        if let Ok(raw) = fs::read_to_string(cfg_p) {
            if let Some(m) = extract_json_string_field(&raw, "model") {
                let low = m.to_ascii_lowercase();
                if low.contains("(xhigh)") {
                    agy_effort = ReasoningEffort::ExtraHigh;
                } else if low.contains("(high)") {
                    agy_effort = ReasoningEffort::High;
                } else if low.contains("(low)") {
                    agy_effort = ReasoningEffort::Low;
                }
                push_unique_model(&mut agy_models, m);
            }
        }
    }
    for preset in [
        "Gemini 4 Argon (Medium)",
        "Gemini 4 Argon (High)",
        "Gemini 4 Hydrogen (XHigh)",
        "Gemini 4 Hydrogen (High)",
        "Gemini 3.8 Flash (High)",
        "Claude Opus 5.5 (High)",
        "Claude Sonnet 5.5 (Medium)",
    ] {
        push_unique_model(&mut agy_models, preset);
    }
    let agy_active_model = agy_models
        .first()
        .cloned()
        .unwrap_or_else(|| "Gemini 4 Argon (Medium)".to_string());

    // 3. Claude Code CLI (`claude.cmd` / `claude`)
    let claude_path =
        find_executable_in_path("claude.cmd").or_else(|| find_executable_in_path("claude"));
    let claude_cfg_path = home
        .as_ref()
        .map(|h| h.join(".claude").join("settings.json"))
        .filter(|p| p.exists());
    let mut claude_models = Vec::new();
    if let Some(cfg_p) = &claude_cfg_path {
        if let Ok(raw) = fs::read_to_string(cfg_p) {
            if let Some(m) = extract_json_string_field(&raw, "model") {
                push_unique_model(&mut claude_models, m);
            }
            for env_key in [
                "ANTHROPIC_MODEL",
                "CCR_CLAUDE_CODE_MODEL",
                "CODEXL_CLAUDE_CODE_MODEL",
                "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
                "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
                "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
                "CLAUDE_CODE_SUBAGENT_MODEL",
            ] {
                if let Some(m) = extract_json_string_field(&raw, env_key) {
                    push_unique_model(&mut claude_models, m);
                }
            }
        }
    }
    for m in &shared_catalog_models {
        push_unique_model(&mut claude_models, m);
    }
    if claude_models.is_empty() {
        for m in ["sonnet", "opus", "haiku"] {
            push_unique_model(&mut claude_models, m);
        }
    }
    let claude_active_model = claude_models
        .first()
        .cloned()
        .unwrap_or_else(|| "sonnet".to_string());

    // 4. OpenAI Codex CLI (`codex.cmd` / `codex`)
    let codex_path =
        find_executable_in_path("codex.cmd").or_else(|| find_executable_in_path("codex"));
    let codex_cfg_path = home
        .as_ref()
        .map(|h| h.join(".codex").join("config.toml"))
        .filter(|p| p.exists());
    let mut codex_models = Vec::new();
    let mut codex_effort = ReasoningEffort::Medium;
    if let Some(cfg_p) = &codex_cfg_path {
        if let Ok(raw) = fs::read_to_string(cfg_p) {
            for line in raw.lines() {
                let t = line.trim();
                if t.starts_with('#') {
                    continue;
                }
                if let Some(rest) = t.strip_prefix("model_reasoning_effort") {
                    if let Some(val_part) = rest.trim_start().strip_prefix('=') {
                        let val = val_part.trim().trim_matches('"').trim_matches('\'');
                        if !val.eq_ignore_ascii_case("none") {
                            codex_effort = ReasoningEffort::from_str_lossy(val);
                        }
                    }
                } else if let Some(rest) = t.strip_prefix("model") {
                    let rest = rest.trim_start();
                    if let Some(val_part) = rest.strip_prefix('=') {
                        let val = val_part.trim().trim_matches('"').trim_matches('\'');
                        push_unique_model(&mut codex_models, val);
                    }
                }
            }
        }
    }
    for m in &shared_catalog_models {
        push_unique_model(&mut codex_models, m);
    }
    if codex_models.is_empty() {
        for m in ["o3", "o4-mini", "gpt-5-codex"] {
            push_unique_model(&mut codex_models, m);
        }
    }
    let codex_active_model = codex_models
        .first()
        .cloned()
        .unwrap_or_else(|| "default".to_string());

    vec![
        AgentRegistration {
            kind: AgentProviderKind::CursorCli,
            status_text: match &cursor_path {
                Some(_) => format!(
                    "cursor-agent · {cursor_active_model} [{}{}]",
                    cursor_effort.label_zh(),
                    if cursor_fast { " · ⚡快速" } else { "" }
                ),
                None => "未检测到 cursor-agent CLI".to_string(),
            },
            available: cursor_path.is_some(),
            supports_session_resume: cursor_path.is_some(),
            supports_checkpoint_rewind: cursor_path.is_some(),
            binary_path: cursor_path,
            config_path: cursor_cfg_path,
            active_model: cursor_active_model,
            discovered_models: cursor_models.clone(),
            exact_model_catalog: Vec::new(),
            fast_mode: cursor_fast,
            reasoning_effort: cursor_effort,
        },
        AgentRegistration {
            kind: AgentProviderKind::GoogleAntigravityAcp,
            status_text: match &agy_path {
                Some(_) => format!(
                    "agy.exe · {agy_active_model} [{}]",
                    agy_effort.label_zh()
                ),
                None => "未检测到 agy.exe (Antigravity CLI)".to_string(),
            },
            available: agy_path.is_some(),
            supports_session_resume: agy_path.is_some(),
            supports_checkpoint_rewind: false,
            binary_path: agy_path,
            config_path: agy_cfg_path,
            active_model: agy_active_model,
            discovered_models: agy_models.clone(),
            exact_model_catalog: agy_models,
            fast_mode: false,
            reasoning_effort: agy_effort,
        },
        AgentRegistration {
            kind: AgentProviderKind::ClaudeCodeCli,
            status_text: match &claude_path {
                Some(_) => format!("claude · {claude_active_model}"),
                None => "未检测到 claude CLI".to_string(),
            },
            available: claude_path.is_some(),
            supports_session_resume: claude_path.is_some(),
            supports_checkpoint_rewind: claude_path.is_some(),
            binary_path: claude_path,
            config_path: claude_cfg_path,
            active_model: claude_active_model,
            discovered_models: claude_models.clone(),
            exact_model_catalog: claude_models,
            fast_mode: false,
            reasoning_effort: ReasoningEffort::Medium,
        },
        AgentRegistration {
            kind: AgentProviderKind::OpenAiCodexCli,
            status_text: match &codex_path {
                Some(_) => format!("codex · {codex_active_model}"),
                None => "未检测到 codex CLI".to_string(),
            },
            available: codex_path.is_some(),
            supports_session_resume: codex_path.is_some(),
            supports_checkpoint_rewind: false,
            binary_path: codex_path,
            config_path: codex_cfg_path,
            active_model: codex_active_model,
            discovered_models: codex_models.clone(),
            exact_model_catalog: codex_models,
            fast_mode: false,
            reasoning_effort: codex_effort,
        },
    ]
}

/// 容错提取 JSON 配置文件中的字符串字段（兼容尾随逗号与大小写重复键）
pub fn extract_json_string_field(raw: &str, field_name: &str) -> Option<String> {
    let needle = format!("\"{field_name}\"");
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&needle) {
            let rest = rest.trim_start();
            if let Some(after_colon) = rest.strip_prefix(':') {
                let val_part = after_colon.trim().trim_end_matches(',').trim();
                if val_part.starts_with('"') && val_part.ends_with('"') && val_part.len() >= 2 {
                    let inner = &val_part[1..val_part.len() - 1];
                    if !inner.trim().is_empty() {
                        return Some(inner.trim().to_string());
                    }
                }
            }
        }
    }
    None
}

/// 解析本地 CLI `--help` 文本，检测是否支持会话恢复与消息编辑回退同步（`(supports_session_resume, supports_checkpoint_rewind)`）
pub fn parse_cli_rewind_capabilities(kind: AgentProviderKind, help_text: &str) -> (bool, bool) {
    let lower = help_text.to_ascii_lowercase();
    match kind {
        AgentProviderKind::CursorCli | AgentProviderKind::CursorInlineEngine => {
            let has_resume =
                lower.contains("--resume") || lower.contains("create-chat") || lower.contains("resume");
            (has_resume, has_resume)
        }
        AgentProviderKind::ClaudeCodeCli => {
            let has_resume = lower.contains("--resume");
            let has_fork = lower.contains("--fork-session");
            (has_resume, has_resume && has_fork)
        }
        AgentProviderKind::OpenAiCodexCli => {
            let has_resume = lower.contains("resume");
            (has_resume, false)
        }
        AgentProviderKind::GoogleAntigravityAcp => {
            let has_conv = lower.contains("--conversation") || lower.contains("--continue");
            (has_conv, false)
        }
        AgentProviderKind::CustomLlmApi => (false, false),
    }
}

/// 在后台实际运行 `<cli> --help` 探测客户端能力
pub fn probe_cli_rewind_capabilities(kind: AgentProviderKind, bin: &Path) -> Option<(bool, bool)> {
    if kind == AgentProviderKind::CustomLlmApi {
        return Some((false, false));
    }
    let output = run_cli_probe(bin, &["--help"])?;
    let combined = format!(
        "{}\n{}",
        strip_ansi_basic(&String::from_utf8_lossy(&output.stdout)),
        strip_ansi_basic(&String::from_utf8_lossy(&output.stderr))
    );
    if combined.trim().is_empty() {
        return None;
    }
    Some(parse_cli_rewind_capabilities(kind, &combined))
}

fn run_cli_probe(bin: &Path, args: &[&str]) -> Option<std::process::Output> {
    if let Some((node_exe, index_js)) = resolve_cursor_node_entry(bin) {
        let mut cmd = Command::new(node_exe);
        cmd.arg(index_js).args(args).stdin(Stdio::null());
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.output().ok()
    } else {
        run_cli_with_args(bin, args)
    }
}

/// 后台异步调用 `cursor-agent --list-models` 动态获取本机 Cursor CLI 全部可用模型（返回 `(分组基础模型列表, 精确模型ID目录)`）
pub fn probe_cursor_cli_list_models(bin: &Path) -> Option<(Vec<String>, Vec<String>)> {
    let output = run_cli_probe(bin, &["--list-models"])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut base_models = Vec::new();
    let mut exact_models = Vec::new();
    for line in stdout.lines() {
        let clean = strip_ansi_basic(line);
        let trimmed = clean.trim();
        if trimmed.is_empty() || trimmed.starts_with("Available") {
            continue;
        }
        if let Some((slug, _desc)) = trimmed.split_once(" - ") {
            let slug = slug.trim();
            if !slug.is_empty() && !slug.contains(' ') {
                push_unique_model(&mut exact_models, slug);
                push_unique_model(&mut base_models, normalize_cursor_base_model(slug));
            }
        }
    }
    if exact_models.is_empty() {
        None
    } else {
        Some((base_models, exact_models))
    }
}

/// 后台异步探测 `agy.exe` 本机支持的所有模型列表
pub fn probe_agy_cli_models(bin: &Path) -> Option<Vec<String>> {
    let mut cmd = Command::new(bin);
    cmd.args(["--model", "__probe_models__", "-p=probe"])
        .stdin(Stdio::null());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let output = cmd.output().ok()?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut models = Vec::new();
    let mut in_list = false;
    for line in combined.lines() {
        let clean = strip_ansi_basic(line);
        let trimmed = clean.trim();
        if trimmed.starts_with("Available models:") {
            in_list = true;
            continue;
        }
        if in_list {
            if trimmed.is_empty() {
                break;
            }
            push_unique_model(&mut models, trimmed);
        }
    }
    if models.is_empty() {
        None
    } else {
        Some(models)
    }
}

fn run_cli_with_args(bin: &Path, args: &[&str]) -> Option<std::process::Output> {
    #[cfg(target_os = "windows")]
    {
        let ext = bin
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "cmd" || ext == "bat" {
            return Command::new("cmd")
                .arg("/C")
                .arg(bin)
                .args(args)
                .stdin(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .ok();
        }
        Command::new(bin)
            .args(args)
            .stdin(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new(bin)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .ok()
    }
}

pub fn strip_ansi_basic(input: &str) -> String {
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
