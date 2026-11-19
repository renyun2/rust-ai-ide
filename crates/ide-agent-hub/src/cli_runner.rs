use crate::cli_discovery::{
    resolve_agy_model_and_effort, resolve_cursor_model_id, resolve_cursor_node_entry,
    strip_ansi_basic,
};
use crate::tools::resolve_safe_workspace_path;
use crate::types::{AgentProviderKind, AgentWakeupCallback, AgentWorkbenchMode, ReasoningEffort};
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::SystemTime;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone)]
pub struct CliRunParams {
    pub kind: AgentProviderKind,
    pub bin: PathBuf,
    pub root: PathBuf,
    pub model: String,
    pub exact_catalog: Vec<String>,
    pub fast_mode: bool,
    pub effort: ReasoningEffort,
    pub mode: AgentWorkbenchMode,
}

#[derive(Debug, Clone)]
pub struct CliRunOutput {
    pub reply: String,
    pub thought: Option<String>,
    pub modified_files: Vec<PathBuf>,
    pub resolved_model: String,
}

pub fn execute_cli_once(
    params: &CliRunParams,
    combined_prompt: &str,
    stream_buf: &Arc<Mutex<Option<String>>>,
    active_pid: &Arc<Mutex<Option<u32>>>,
    modified_inbox: Option<&Arc<Mutex<Vec<PathBuf>>>>,
    wakeup: Option<&AgentWakeupCallback>,
) -> Result<CliRunOutput, String> {
    let before_snap = if params.mode != AgentWorkbenchMode::Ask {
        capture_git_mtime_snapshot(&params.root)
    } else {
        HashMap::new()
    };

    let (resolved_model, agy_effort) = match params.kind {
        AgentProviderKind::CursorCli | AgentProviderKind::CursorInlineEngine => (
            resolve_cursor_model_id(
                &params.model,
                params.effort,
                params.fast_mode,
                &params.exact_catalog,
            ),
            None,
        ),
        AgentProviderKind::GoogleAntigravityAcp => {
            resolve_agy_model_and_effort(&params.model, params.effort, &params.exact_catalog)
        }
        _ => (params.model.trim().to_string(), Some(params.effort.as_cli_str())),
    };

    let mut child = spawn_cli_process(params, &resolved_model, agy_effort, combined_prompt)
        .map_err(|e| format!("启动 CLI 失败: {e}"))?;
    let pid = child.id();
    *active_pid.lock() = Some(pid);

    // 对于非 `-p=<prompt>` 的 CLI，写入 stdin 并立即关闭以发出 EOF
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(combined_prompt.as_bytes());
        let _ = stdin.flush();
        drop(stdin);
    }

    let is_json_stream = matches!(
        params.kind,
        AgentProviderKind::CursorCli
            | AgentProviderKind::CursorInlineEngine
            | AgentProviderKind::GoogleAntigravityAcp
    );

    let stdout_raw = Arc::new(Mutex::new(String::new()));
    let stderr_acc = Arc::new(Mutex::new(String::new()));
    let assembled_reply = Arc::new(Mutex::new(String::new()));
    let assembled_thought = Arc::new(Mutex::new(String::new()));
    let stream_error = Arc::new(Mutex::new(None::<String>));
    let tool_touched_files = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let auth_blocked = Arc::new(AtomicBool::new(false));

    let out_thread = child.stdout.take().map(|out| {
        let raw_acc = stdout_raw.clone();
        let reply_acc = assembled_reply.clone();
        let thought_acc = assembled_thought.clone();
        let err_slot = stream_error.clone();
        let touched = tool_touched_files.clone();
        let sbuf = stream_buf.clone();
        let wk = wakeup.cloned();
        let auth_flag = auth_blocked.clone();
        let kind = params.kind;
        let root = params.root.clone();

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
                raw_acc.lock().push_str(&clean);

                if is_json_stream {
                    let trimmed = clean.trim();
                    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                        handle_stream_json_event(
                            kind,
                            &root,
                            &v,
                            &reply_acc,
                            &thought_acc,
                            &err_slot,
                            &touched,
                            &sbuf,
                        );
                    }
                } else {
                    let mut g = reply_acc.lock();
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
            "该本地 CLI 需要先完成一次交互式登录认证（可在底部终端运行对应 CLI 登录后重试，或切换其他本地 CLI）。"
                .to_string(),
        );
    }

    let mut modified_files = tool_touched_files.lock().clone();
    if params.mode != AgentWorkbenchMode::Ask {
        let after_snap = capture_git_mtime_snapshot(&params.root);
        for (p, after_t) in &after_snap {
            let changed = match before_snap.get(p) {
                None => true,
                Some(before_t) => before_t != after_t,
            };
            if changed && !modified_files.contains(p) {
                modified_files.push(p.clone());
            }
        }
    }
    if let Some(inbox) = modified_inbox {
        if !modified_files.is_empty() {
            let mut guard = inbox.lock();
            for p in &modified_files {
                if !guard.contains(p) {
                    guard.push(p.clone());
                }
            }
        }
    }

    if let Some(err) = stream_error.lock().clone() {
        return Err(err);
    }

    let reply_str = assembled_reply.lock().trim().to_string();
    let raw_out = stdout_raw.lock().trim().to_string();
    let err_str = stderr_acc.lock().trim().to_string();
    let thought_opt = {
        let t = assembled_thought.lock().trim().to_string();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    };

    if !reply_str.is_empty() {
        Ok(CliRunOutput {
            reply: reply_str,
            thought: thought_opt,
            modified_files,
            resolved_model,
        })
    } else if !is_json_stream && !raw_out.is_empty() {
        Ok(CliRunOutput {
            reply: raw_out,
            thought: thought_opt,
            modified_files,
            resolved_model,
        })
    } else if !err_str.is_empty() {
        Err(err_str)
    } else {
        match status {
            Ok(st) if st.success() => Ok(CliRunOutput {
                reply: "CLI 执行完成（无文本输出）。".to_string(),
                thought: thought_opt,
                modified_files,
                resolved_model,
            }),
            Ok(st) => Err(format!("CLI 退出状态: {st}")),
            Err(e) => Err(format!("等待 CLI 进程失败: {e}")),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_stream_json_event(
    kind: AgentProviderKind,
    root: &Path,
    v: &Value,
    reply_acc: &Arc<Mutex<String>>,
    thought_acc: &Arc<Mutex<String>>,
    err_slot: &Arc<Mutex<Option<String>>>,
    touched: &Arc<Mutex<Vec<PathBuf>>>,
    sbuf: &Arc<Mutex<Option<String>>>,
) {
    match kind {
        AgentProviderKind::CursorCli | AgentProviderKind::CursorInlineEngine => {
            let ty = v["type"].as_str().unwrap_or("");
            let sub = v["subtype"].as_str().unwrap_or("");
            match ty {
                "thinking" => {
                    if sub == "delta" {
                        if let Some(txt) = v["text"].as_str() {
                            let mut t = thought_acc.lock();
                            t.push_str(txt);
                            if reply_acc.lock().is_empty() {
                                let preview: String =
                                    t.chars().rev().take(240).collect::<String>().chars().rev().collect();
                                *sbuf.lock() = Some(format!("💭 思考中...\n{preview}"));
                            }
                        }
                    }
                }
                "assistant" => {
                    let mut chunk = String::new();
                    if let Some(arr) = v["message"]["content"].as_array() {
                        for item in arr {
                            if let Some(txt) = item["text"].as_str() {
                                chunk.push_str(txt);
                            }
                        }
                    }
                    if !chunk.is_empty() {
                        let mut r = reply_acc.lock();
                        if v.get("timestamp_ms").is_some() {
                            r.push_str(&chunk);
                        } else if chunk.len() >= r.len() {
                            *r = chunk;
                        }
                        *sbuf.lock() = Some(r.clone());
                    }
                }
                "tool_call" => {
                    extract_paths_from_json_value(root, &v["tool_call"], touched);
                    if sub == "started" && reply_acc.lock().is_empty() {
                        if let Some(obj) = v["tool_call"].as_object() {
                            if let Some(tool_key) = obj.keys().next() {
                                *sbuf.lock() = Some(format!("⚙ Cursor 正在执行工具 `{tool_key}`..."));
                            }
                        }
                    }
                }
                "result" => {
                    if v["is_error"].as_bool() == Some(true) || sub == "error" {
                        let msg = v["result"]
                            .as_str()
                            .or_else(|| v["error"].as_str())
                            .unwrap_or("Cursor CLI 返回错误");
                        *err_slot.lock() = Some(msg.to_string());
                    } else if let Some(res_txt) = v["result"].as_str() {
                        let mut r = reply_acc.lock();
                        if r.trim().is_empty() && !res_txt.trim().is_empty() {
                            *r = res_txt.to_string();
                        }
                    }
                }
                _ => {}
            }
        }
        AgentProviderKind::GoogleAntigravityAcp => {
            let ev = v["event"].as_str().unwrap_or("");
            if ev == "step_update" {
                let su = &v["step_update"];
                let step_type = su["step_type"].as_str().unwrap_or("");
                if step_type == "agent_response" {
                    if let Some(delta) = su["text_delta"].as_str() {
                        let mut r = reply_acc.lock();
                        r.push_str(delta);
                        *sbuf.lock() = Some(r.clone());
                    }
                } else {
                    extract_paths_from_json_value(root, su, touched);
                    if reply_acc.lock().is_empty() && !step_type.is_empty() && step_type != "user_input" {
                        *sbuf.lock() = Some(format!("⚙ Antigravity 正在执行步骤 `{step_type}`..."));
                    }
                }
            } else if ev == "result" {
                let res = &v["result"];
                let status = res["status"].as_str().unwrap_or("");
                if status.eq_ignore_ascii_case("ERROR") {
                    let msg = res["error"].as_str().unwrap_or("Antigravity CLI 执行失败");
                    *err_slot.lock() = Some(msg.to_string());
                } else if let Some(resp_txt) = res["response"].as_str() {
                    let mut r = reply_acc.lock();
                    if r.trim().is_empty() && !resp_txt.trim().is_empty() {
                        *r = resp_txt.to_string();
                    }
                }
            }
        }
        _ => {}
    }
}

fn extract_paths_from_json_value(
    root: &Path,
    val: &Value,
    touched: &Arc<Mutex<Vec<PathBuf>>>,
) {
    match val {
        Value::Object(map) => {
            for (k, v) in map {
                let kl = k.to_ascii_lowercase();
                if matches!(
                    kl.as_str(),
                    "path" | "file_path" | "target_file" | "relative_workspace_path" | "file"
                ) {
                    if let Some(s) = v.as_str() {
                        if let Ok(resolved) = resolve_safe_workspace_path(root, s) {
                            if resolved.is_file() {
                                let mut g = touched.lock();
                                if !g.contains(&resolved) {
                                    g.push(resolved);
                                }
                            }
                        }
                    }
                } else {
                    extract_paths_from_json_value(root, v, touched);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr {
                extract_paths_from_json_value(root, item, touched);
            }
        }
        _ => {}
    }
}

fn capture_git_mtime_snapshot(root: &Path) -> HashMap<PathBuf, Option<SystemTime>> {
    let mut map = HashMap::new();
    let mut cmd = Command::new("git");
    cmd.args(["status", "--porcelain", "-uall"])
        .current_dir(root)
        .stdin(Stdio::null());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    if let Ok(out) = cmd.output() {
        if out.status.success() {
            let txt = String::from_utf8_lossy(&out.stdout);
            for line in txt.lines() {
                if line.len() >= 4 {
                    let raw_path = line[3..].trim().trim_matches('"');
                    let rel = raw_path.split(" -> ").last().unwrap_or(raw_path);
                    let full = root.join(rel);
                    let mtime = fs::metadata(&full).and_then(|m| m.modified()).ok();
                    map.insert(full, mtime);
                }
            }
        }
    }
    map
}

fn spawn_cli_process(
    params: &CliRunParams,
    resolved_model: &str,
    agy_effort: Option<&'static str>,
    combined_prompt: &str,
) -> std::io::Result<Child> {
    let has_specific_model = !resolved_model.trim().is_empty()
        && !resolved_model.eq_ignore_ascii_case("default");

    match params.kind {
        AgentProviderKind::CursorCli | AgentProviderKind::CursorInlineEngine => {
            let mut args = vec![
                "-p".to_string(),
                "--trust".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
                "--stream-partial-output".to_string(),
                "--workspace".to_string(),
                params.root.to_string_lossy().to_string(),
            ];
            match params.mode {
                AgentWorkbenchMode::Ask => {
                    args.push("--mode".to_string());
                    args.push("ask".to_string());
                }
                AgentWorkbenchMode::Edit | AgentWorkbenchMode::Agent => {
                    args.push("-f".to_string());
                }
            }
            if has_specific_model {
                args.push("--model".to_string());
                args.push(resolved_model.to_string());
            }

            if let Some((node_exe, index_js)) = resolve_cursor_node_entry(&params.bin) {
                let mut cmd = Command::new(node_exe);
                cmd.arg(index_js)
                    .args(&args)
                    .current_dir(&params.root)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                #[cfg(target_os = "windows")]
                cmd.creation_flags(CREATE_NO_WINDOW);
                return cmd.spawn();
            }

            #[cfg(target_os = "windows")]
            {
                let ps1 = params.bin.with_extension("ps1");
                if ps1.exists() {
                    let mut cmd = Command::new("powershell.exe");
                    cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                        .arg(ps1)
                        .args(&args)
                        .current_dir(&params.root)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .creation_flags(CREATE_NO_WINDOW);
                    return cmd.spawn();
                }
            }
            spawn_generic_piped_cli(&params.bin, &params.root, &args, true)
        }
        AgentProviderKind::GoogleAntigravityAcp => {
            let mode_str = match params.mode {
                AgentWorkbenchMode::Ask => "plan",
                AgentWorkbenchMode::Edit => "accept-edits",
                AgentWorkbenchMode::Agent => "auto",
            };
            let mut args = vec![
                "--dangerously-skip-permissions".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
                "--mode".to_string(),
                mode_str.to_string(),
            ];
            if has_specific_model {
                args.push("--model".to_string());
                args.push(resolved_model.to_string());
            }
            if let Some(eff) = agy_effort {
                args.push("--effort".to_string());
                args.push(eff.to_string());
            }
            // Windows CreateProcessW 单命令行最大 32,767 字符，安全裁切长上下文
            let prompt_arg: String = if combined_prompt.chars().count() > 20_000 {
                combined_prompt.chars().take(20_000).collect()
            } else {
                combined_prompt.to_string()
            };
            args.push(format!("-p={prompt_arg}"));
            spawn_generic_piped_cli(&params.bin, &params.root, &args, false)
        }
        AgentProviderKind::ClaudeCodeCli => {
            let perm_mode = match params.mode {
                AgentWorkbenchMode::Ask => "plan",
                AgentWorkbenchMode::Edit => "acceptEdits",
                AgentWorkbenchMode::Agent => "bypassPermissions",
            };
            let mut args = vec![
                "-p".to_string(),
                "--output-format".to_string(),
                "text".to_string(),
                "--permission-mode".to_string(),
                perm_mode.to_string(),
                "--effort".to_string(),
                params.effort.as_cli_str().to_string(),
            ];
            if params.mode == AgentWorkbenchMode::Agent {
                args.push("--dangerously-skip-permissions".to_string());
            }
            if has_specific_model {
                args.push("--model".to_string());
                args.push(resolved_model.to_string());
            }
            spawn_generic_piped_cli(&params.bin, &params.root, &args, true)
        }
        AgentProviderKind::OpenAiCodexCli => {
            let sandbox = match params.mode {
                AgentWorkbenchMode::Ask => "read-only",
                AgentWorkbenchMode::Edit | AgentWorkbenchMode::Agent => "workspace-write",
            };
            let mut args = vec![
                "exec".to_string(),
                "--skip-git-repo-check".to_string(),
                "-s".to_string(),
                sandbox.to_string(),
                "-c".to_string(),
                format!("model_reasoning_effort=\"{}\"", params.effort.as_cli_str()),
            ];
            if has_specific_model {
                args.push("-m".to_string());
                args.push(resolved_model.to_string());
            }
            args.push("-".to_string());
            spawn_generic_piped_cli(&params.bin, &params.root, &args, true)
        }
        AgentProviderKind::GeminiCli => {
            let approval = match params.mode {
                AgentWorkbenchMode::Ask => "plan",
                AgentWorkbenchMode::Edit => "auto_edit",
                AgentWorkbenchMode::Agent => "yolo",
            };
            let mut args = vec![
                "--skip-trust".to_string(),
                "--approval-mode".to_string(),
                approval.to_string(),
                "-o".to_string(),
                "text".to_string(),
            ];
            if has_specific_model && !resolved_model.eq_ignore_ascii_case("auto") {
                args.push("-m".to_string());
                args.push(resolved_model.to_string());
            }
            args.push("-p".to_string());
            args.push(String::new());
            spawn_generic_piped_cli(&params.bin, &params.root, &args, true)
        }
        AgentProviderKind::CustomLlmApi => {
            spawn_generic_piped_cli(&params.bin, &params.root, &[], true)
        }
    }
}

fn spawn_generic_piped_cli(
    bin: &Path,
    cwd: &Path,
    args: &[String],
    pipe_stdin: bool,
) -> std::io::Result<Child> {
    let stdin_cfg = || if pipe_stdin { Stdio::piped() } else { Stdio::null() };

    #[cfg(target_os = "windows")]
    {
        let ext = bin
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "cmd" || ext == "bat" {
            let mut cmd = Command::new("cmd");
            cmd.arg("/C").arg(bin).args(args);
            cmd.current_dir(cwd)
                .env("GEMINI_CLI_TRUST_WORKSPACE", "true")
                .stdin(stdin_cfg())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(CREATE_NO_WINDOW);
            return cmd.spawn();
        }
    }

    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(cwd)
        .env("GEMINI_CLI_TRUST_WORKSPACE", "true")
        .stdin(stdin_cfg())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()
}

/// 从 AI 回复文本中提取工作区内的文件引用（支持 `` `path/file.rs:12` ``、`path/file.rs#L10` 或纯路径）
pub fn detect_workspace_file_references(root: &Path, text: &str) -> Vec<(String, PathBuf, Option<usize>)> {
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    for raw_tok in text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';')) {
        let trimmed = raw_tok
            .trim_matches(|c: char| matches!(c, '`' | '\'' | '"' | '*' | '。' | '，' | '：'))
            .trim_end_matches('.');
        if trimmed.len() < 3 {
            continue;
        }
        if let Some((path_str, line_opt)) = parse_path_and_line_token(trimmed) {
            if !path_str.contains('.') && !path_str.contains('/') && !path_str.contains('\\') {
                continue;
            }
            if let Ok(resolved) = resolve_safe_workspace_path(root, path_str) {
                if resolved.is_file() {
                    let key = (resolved.clone(), line_opt);
                    if seen.insert(key) {
                        results.push((trimmed.to_string(), resolved, line_opt));
                    }
                }
            }
        }
    }
    results
}

pub fn parse_path_and_line_token(token: &str) -> Option<(&str, Option<usize>)> {
    // 支持 file.rs#L123 或 file.rs#L123-L145
    if let Some((p, frag)) = token.split_once("#L") {
        let num_str: String = frag.chars().take_while(|c| c.is_ascii_digit()).collect();
        let line = num_str.parse::<usize>().ok();
        return Some((p, line));
    }
    // 支持 Windows 盘符路径 D:\a\b.rs:12 或相对路径 a/b.rs:12:5
    if let Some(colon_idx) = token.rfind(':') {
        // 避免把 Windows 盘符 `C:\` 的冒号当作行号
        if colon_idx > 1 {
            let after = &token[colon_idx + 1..];
            if !after.is_empty() && after.chars().all(|c| c.is_ascii_digit()) {
                let before = &token[..colon_idx];
                // 检查形如 path.rs:12:5
                if let Some(prev_colon) = before.rfind(':') {
                    if prev_colon > 1 {
                        let mid = &before[prev_colon + 1..];
                        if !mid.is_empty() && mid.chars().all(|c| c.is_ascii_digit()) {
                            return Some((&before[..prev_colon], mid.parse::<usize>().ok()));
                        }
                    }
                }
                return Some((before, after.parse::<usize>().ok()));
            }
        }
    }
    Some((token, None))
}
