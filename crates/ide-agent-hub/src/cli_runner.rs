use crate::cli_discovery::{
    resolve_agy_model_and_effort, resolve_cursor_model_id, resolve_cursor_node_entry,
    resolve_native_agy_binary, strip_ansi_basic, suppress_agy_background_updater_window,
    user_home_dir,
};
use crate::tools::resolve_safe_workspace_path;
use crate::types::{
    AgentProviderKind, AgentWakeupCallback, AgentWorkbenchMode, ReasoningEffort,
    StreamingAgentProgress,
};
use ide_git::{compute_line_diff, normalize_diff_text, DiffLineKind};
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
// `creation_flags` replaces the default, so keep CREATE_UNICODE_ENVIRONMENT (0x400).
// Without it, Windows reads the environment as ANSI and `git status` fails on CI.
const CREATE_NO_WINDOW: u32 = 0x0800_0000 | 0x0000_0400;

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
    pub resume_session_id: Option<String>,
    pub fork_session: bool,
}

#[derive(Debug, Clone)]
pub struct CliRunOutput {
    pub reply: String,
    pub thought: Option<String>,
    pub modified_files: Vec<PathBuf>,
    pub resolved_model: String,
    pub cli_session_id: Option<String>,
    pub checkpoint_backups: HashMap<PathBuf, Option<String>>,
    pub file_diff_stats: HashMap<PathBuf, (usize, usize)>,
}

fn strip_verbatim_and_clean(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    let stripped = raw.strip_prefix(r"\\?\").unwrap_or(raw.as_ref());
    PathBuf::from(stripped).components().collect()
}

pub(crate) fn normalize_abs_path(path: &Path) -> PathBuf {
    let normalized = strip_verbatim_and_clean(path);
    // Git on Windows often returns the long path while Rust's temp dir is the
    // 8.3 form (`RUNNER~1`) or a junction. Compare the resolved path, falling
    // back to the nearest existing ancestor for deleted or new files.
    if normalized.is_absolute() {
        if let Ok(canon) = normalized.canonicalize() {
            return strip_verbatim_and_clean(&canon);
        }
        let mut suffix = Vec::new();
        let mut cur = normalized.as_path();
        while let Some(name) = cur.file_name() {
            suffix.push(name.to_os_string());
            let Some(parent) = cur.parent() else {
                break;
            };
            if let Ok(canon_parent) = parent.canonicalize() {
                let mut resolved = strip_verbatim_and_clean(&canon_parent);
                for part in suffix.into_iter().rev() {
                    resolved.push(part);
                }
                return resolved.components().collect();
            }
            cur = parent;
        }
    }
    normalized
}

pub(crate) fn paths_same_file(a: &Path, b: &Path) -> bool {
    let na = normalize_abs_path(a);
    let nb = normalize_abs_path(b);
    #[cfg(target_os = "windows")]
    {
        na.to_string_lossy()
            .replace('/', "\\")
            .eq_ignore_ascii_case(&nb.to_string_lossy().replace('/', "\\"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        na == nb
    }
}

fn strip_prefix_norm<'a>(path: &Path, base: &Path) -> Option<String> {
    let np = normalize_abs_path(path);
    let nb = normalize_abs_path(base);
    if let Ok(rel) = np.strip_prefix(&nb) {
        return Some(rel.to_string_lossy().replace('\\', "/"));
    }
    #[cfg(target_os = "windows")]
    {
        let ps = np.to_string_lossy().replace('/', "\\");
        let bs = nb.to_string_lossy().replace('/', "\\");
        let bs_trimmed = bs.trim_end_matches('\\');
        if ps.len() > bs_trimmed.len()
            && ps[..bs_trimmed.len()].eq_ignore_ascii_case(bs_trimmed)
            && ps.as_bytes().get(bs_trimmed.len()) == Some(&b'\\')
        {
            return Some(ps[bs_trimmed.len() + 1..].replace('\\', "/"));
        }
    }
    None
}

fn git_repo_toplevel(root: &Path) -> Option<PathBuf> {
    let mut cmd = Command::new("git");
    cmd.args(["rev-parse", "--show-toplevel"])
        .current_dir(root)
        .stdin(Stdio::null());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(normalize_abs_path(Path::new(&s)))
    }
}

pub(crate) fn is_mutating_tool_name(raw_name: &str) -> bool {
    let lower = raw_name.to_ascii_lowercase();
    if lower.contains("read")
        || lower.contains("view")
        || lower.contains("grep")
        || lower.contains("search")
        || lower.contains("find")
        || lower.contains("glob")
        || lower.contains("list")
        || lower == "ls"
        || lower.starts_with("lstool")
        || lower.contains("web")
        || lower.contains("fetch")
        || lower.contains("todo")
    {
        return false;
    }
    lower.contains("write")
        || lower.contains("edit")
        || lower.contains("replace")
        || lower.contains("create")
        || lower.contains("delete")
        || lower.contains("remove")
        || lower.contains("patch")
        || lower.contains("insert")
        || lower.contains("append")
        || lower.contains("file_change")
        || lower.contains("notebook")
}

/// 单个文件快照最大允许字节数（512 KB）：防止将二进制、压缩包、大型构建产物或 minified bundle 写入内存与 `.agent.json`
pub(crate) const MAX_SNAPSHOT_FILE_BYTES: usize = 512 * 1024;
const MAX_RAW_STDOUT_BYTES: usize = 256 * 1024;
const MAX_STDERR_BYTES: usize = 128 * 1024;

pub(crate) fn is_snapshot_eligible_path(path: &Path) -> bool {
    for comp in path.components() {
        if let Some(name) = comp.as_os_str().to_str() {
            let lower = name.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                ".git"
                    | "target"
                    | "node_modules"
                    | "dist"
                    | "build"
                    | "out"
                    | ".next"
                    | ".nuxt"
                    | "__pycache__"
                    | ".venv"
                    | "venv"
                    | ".gradle"
                    | ".dart_tool"
                    | ".cache"
                    | ".turbo"
                    | "coverage"
                    | "vendor"
            ) {
                return false;
            }
        }
    }
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if file_name.ends_with(".min.js")
        || file_name.ends_with("-min.js")
        || file_name.ends_with(".min.css")
        || file_name.ends_with("-min.css")
        || file_name.ends_with(".bundle.js")
        || file_name.ends_with("-bundle.js")
        || file_name.ends_with(".chunk.js")
        || file_name.ends_with("-lock.json")
        || file_name.ends_with("-lock.yaml")
        || matches!(
            file_name.as_str(),
            "cargo.lock" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "poetry.lock"
        )
    {
        return false;
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext_low = ext.to_ascii_lowercase();
        if matches!(
            ext_low.as_str(),
            "exe"
                | "dll"
                | "so"
                | "dylib"
                | "pdb"
                | "rlib"
                | "rmeta"
                | "o"
                | "obj"
                | "a"
                | "lib"
                | "class"
                | "wasm"
                | "bin"
                | "msi"
                | "dmg"
                | "iso"
                | "zip"
                | "tar"
                | "gz"
                | "tgz"
                | "bz2"
                | "xz"
                | "7z"
                | "rar"
                | "jar"
                | "war"
                | "apk"
                | "ipa"
                | "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "bmp"
                | "ico"
                | "tif"
                | "tiff"
                | "avif"
                | "heic"
                | "mp3"
                | "wav"
                | "ogg"
                | "flac"
                | "aac"
                | "m4a"
                | "mp4"
                | "webm"
                | "mov"
                | "avi"
                | "mkv"
                | "pdf"
                | "ttf"
                | "otf"
                | "woff"
                | "woff2"
                | "eot"
                | "db"
                | "sqlite"
                | "sqlite3"
                | "lock"
                | "map"
                | "pyc"
                | "pyo"
                | "ilk"
                | "exp"
                | "idb"
                | "pch"
                | "gch"
                | "node"
        ) {
            return false;
        }
    }
    true
}

pub(crate) fn is_valid_text_snapshot(s: &str) -> bool {
    if s.len() > MAX_SNAPSHOT_FILE_BYTES {
        return false;
    }
    if s.as_bytes().contains(&0) {
        return false;
    }
    let sample_len = s.len().min(8192);
    let mut end = sample_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let sample = &s[..end];
    let mut total_chars = 0usize;
    let mut bad_chars = 0usize;
    for ch in sample.chars() {
        total_chars += 1;
        if ch == '\u{FFFD}' || (ch.is_control() && !matches!(ch, '\n' | '\r' | '\t')) {
            bad_chars += 1;
        }
    }
    if total_chars >= 32 && bad_chars * 50 > total_chars {
        return false;
    }
    true
}

pub(crate) fn read_text_snapshot_bounded(path: &Path) -> Option<String> {
    if !is_snapshot_eligible_path(path) {
        return None;
    }
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_SNAPSHOT_FILE_BYTES as u64 {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.len() > MAX_SNAPSHOT_FILE_BYTES || bytes.contains(&0) {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    if is_valid_text_snapshot(&text) {
        Some(text)
    } else {
        None
    }
}

/// 追踪单次 AI 执行前后的工作区文件增减统计（对所有 CLI 统一生效，准确区分已存在的 Git 改动）
#[derive(Debug, Clone)]
pub(crate) struct TurnDiffTracker {
    root: PathBuf,
    mode: AgentWorkbenchMode,
    before_snap: HashMap<PathBuf, Option<SystemTime>>,
    before_dirty_contents: HashMap<PathBuf, Option<String>>,
}

impl TurnDiffTracker {
    pub(crate) fn capture(root: &Path, mode: AgentWorkbenchMode) -> Self {
        let norm_root = normalize_abs_path(root);
        let before_snap = if mode != AgentWorkbenchMode::Ask {
            capture_git_mtime_snapshot(&norm_root)
        } else {
            HashMap::new()
        };
        let mut before_dirty_contents = HashMap::with_capacity(before_snap.len());
        for p in before_snap.keys() {
            if is_snapshot_eligible_path(p) {
                before_dirty_contents.insert(p.clone(), read_text_snapshot_bounded(p));
            }
        }
        Self {
            root: norm_root,
            mode,
            before_snap,
            before_dirty_contents,
        }
    }

    pub(crate) fn finish(
        &self,
        extra_mutated: &[PathBuf],
    ) -> (
        Vec<PathBuf>,
        HashMap<PathBuf, Option<String>>,
        HashMap<PathBuf, (usize, usize)>,
    ) {
        let mut candidate_files: Vec<PathBuf> = Vec::new();
        let mut git_changed_files: HashSet<PathBuf> = HashSet::new();

        if self.mode != AgentWorkbenchMode::Ask {
            let after_snap = capture_git_mtime_snapshot(&self.root);
            for (p, after_t) in &after_snap {
                if !is_snapshot_eligible_path(p) {
                    continue;
                }
                let before_entry = self
                    .before_snap
                    .iter()
                    .find(|(bp, _)| paths_same_file(bp, p))
                    .map(|(_, bt)| bt);
                let changed = match before_entry {
                    None => true,
                    Some(before_t) => {
                        if before_t != after_t {
                            true
                        } else if let Some(prev_content) = self
                            .before_dirty_contents
                            .iter()
                            .find(|(bp, _)| paths_same_file(bp, p))
                            .map(|(_, c)| c)
                        {
                            let cur_content = read_text_snapshot_bounded(p);
                            prev_content.as_deref().map(normalize_diff_text)
                                != cur_content.as_deref().map(normalize_diff_text)
                        } else {
                            false
                        }
                    }
                };
                if changed {
                    git_changed_files.insert(p.clone());
                    if !candidate_files.iter().any(|cp| paths_same_file(cp, p)) {
                        candidate_files.push(p.clone());
                    }
                }
            }
            for p in self.before_snap.keys() {
                if !is_snapshot_eligible_path(p) {
                    continue;
                }
                if !after_snap.keys().any(|ap| paths_same_file(ap, p)) {
                    git_changed_files.insert(p.clone());
                    if !candidate_files.iter().any(|cp| paths_same_file(cp, p)) {
                        candidate_files.push(p.clone());
                    }
                }
            }
        }

        for raw_p in extra_mutated {
            let p = normalize_abs_path(raw_p);
            if !is_snapshot_eligible_path(&p) {
                continue;
            }
            if !candidate_files.iter().any(|cp| paths_same_file(cp, &p)) {
                candidate_files.push(p);
            }
        }

        let mut modified_files = Vec::new();
        let mut checkpoint_backups: HashMap<PathBuf, Option<String>> = HashMap::new();
        let mut file_diff_stats: HashMap<PathBuf, (usize, usize)> = HashMap::new();

        for p in candidate_files {
            if !is_snapshot_eligible_path(&p) {
                continue;
            }
            let after_content = read_text_snapshot_bounded(&p);
            let pre_dirty_entry = self
                .before_dirty_contents
                .iter()
                .find(|(bp, _)| paths_same_file(bp, &p))
                .map(|(_, c)| c.clone());
            let had_pre_dirty = pre_dirty_entry.is_some();
            let baseline = if let Some(prev) = pre_dirty_entry {
                prev
            } else if self.mode != AgentWorkbenchMode::Ask {
                read_git_head_file(&self.root, &p)
            } else {
                continue;
            };
            let b_norm = baseline.as_deref().map(normalize_diff_text);
            let a_norm = after_content.as_deref().map(normalize_diff_text);
            let content_changed = match (&b_norm, &a_norm) {
                (Some(b), Some(a)) => b != a,
                (None, Some(_)) => {
                    git_changed_files.iter().any(|gp| paths_same_file(gp, &p))
                        || had_pre_dirty
                        || extra_mutated
                            .iter()
                            .any(|ep| paths_same_file(ep, &p))
                }
                (Some(_), None) => !p.exists(),
                (None, None) => false,
            };
            if !content_changed {
                continue;
            }
            let (added, removed) = compute_file_diff_counts(
                baseline.as_deref().unwrap_or(""),
                after_content.as_deref().unwrap_or(""),
            );
            if added == 0 && removed == 0 && b_norm.is_some() && a_norm.is_some() {
                continue;
            }
            checkpoint_backups.insert(p.clone(), baseline);
            file_diff_stats.insert(p.clone(), (added, removed));
            modified_files.push(p);
        }

        (modified_files, checkpoint_backups, file_diff_stats)
    }
}

pub fn execute_cli_once(
    params: &CliRunParams,
    combined_prompt: &str,
    stream_buf: &Arc<Mutex<Option<StreamingAgentProgress>>>,
    active_pid: &Arc<Mutex<Option<u32>>>,
    modified_inbox: Option<&Arc<Mutex<Vec<PathBuf>>>>,
    wakeup: Option<&AgentWakeupCallback>,
) -> Result<CliRunOutput, String> {
    let diff_tracker = TurnDiffTracker::capture(&params.root, params.mode);

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
            | AgentProviderKind::ClaudeCodeCli
            | AgentProviderKind::OpenAiCodexCli
    );

    let stdout_raw = Arc::new(Mutex::new(String::new()));
    let stderr_acc = Arc::new(Mutex::new(String::new()));
    let assembled_reply = Arc::new(Mutex::new(String::new()));
    let assembled_thought = Arc::new(Mutex::new(String::new()));
    let stream_error = Arc::new(Mutex::new(None::<String>));
    let tool_touched_files = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let captured_session_id = Arc::new(Mutex::new(params.resume_session_id.clone()));
    let auth_blocked = Arc::new(AtomicBool::new(false));

    let out_thread = child.stdout.take().map(|out| {
        let raw_acc = stdout_raw.clone();
        let reply_acc = assembled_reply.clone();
        let thought_acc = assembled_thought.clone();
        let err_slot = stream_error.clone();
        let touched = tool_touched_files.clone();
        let sid_acc = captured_session_id.clone();
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
                if !is_json_stream || reply_acc.lock().is_empty() {
                    let mut raw = raw_acc.lock();
                    if raw.len() < MAX_RAW_STDOUT_BYTES {
                        raw.push_str(&clean);
                    }
                }

                if is_json_stream {
                    let trimmed = clean.trim();
                    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                        capture_stream_session_id(kind, &v, &sid_acc);
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
                    let reply_snapshot = g.clone();
                    drop(g);
                    update_stream_progress(&sbuf, |p| {
                        p.phase = "正在生成回复...".to_string();
                        p.reply_text = reply_snapshot;
                    });
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
                {
                    let mut err_buf = acc.lock();
                    if err_buf.len() < MAX_STDERR_BYTES {
                        err_buf.push_str(&clean);
                    }
                }
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

    let extra_mutated = tool_touched_files.lock().clone();
    let (modified_files, checkpoint_backups, file_diff_stats) =
        diff_tracker.finish(&extra_mutated);

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
    let cli_session_id = captured_session_id.lock().clone();
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
            cli_session_id,
            checkpoint_backups,
            file_diff_stats,
        })
    } else if !raw_out.is_empty() && (!is_json_stream || !raw_out.trim_start().starts_with('{')) {
        Ok(CliRunOutput {
            reply: raw_out,
            thought: thought_opt,
            modified_files,
            resolved_model,
            cli_session_id,
            checkpoint_backups,
            file_diff_stats,
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
                cli_session_id,
                checkpoint_backups,
                file_diff_stats,
            }),
            Ok(st) => Err(format!("CLI 退出状态: {st}")),
            Err(e) => Err(format!("等待 CLI 进程失败: {e}")),
        }
    }
}

fn capture_stream_session_id(
    kind: AgentProviderKind,
    v: &Value,
    sid_slot: &Arc<Mutex<Option<String>>>,
) {
    let extracted = match kind {
        AgentProviderKind::CursorCli
        | AgentProviderKind::CursorInlineEngine
        | AgentProviderKind::ClaudeCodeCli => v["session_id"].as_str(),
        AgentProviderKind::OpenAiCodexCli => v["thread_id"].as_str().or_else(|| v["session_id"].as_str()),
        AgentProviderKind::GoogleAntigravityAcp => v["session_id"].as_str(),
        AgentProviderKind::CustomLlmApi => None,
    };
    if let Some(sid) = extracted {
        let trimmed = sid.trim();
        if !trimmed.is_empty() {
            *sid_slot.lock() = Some(trimmed.to_string());
        }
    }
}

fn read_git_head_file(root: &Path, full_path: &Path) -> Option<String> {
    if !is_snapshot_eligible_path(full_path) {
        return None;
    }
    let git_base = git_repo_toplevel(root).unwrap_or_else(|| normalize_abs_path(root));
    let rel = strip_prefix_norm(full_path, &git_base)?;
    if rel.is_empty() {
        return None;
    }
    let mut cmd = Command::new("git");
    cmd.args(["show", &format!("HEAD:{rel}")])
        .current_dir(&git_base)
        .stdin(Stdio::null());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().ok()?;
    if !out.status.success()
        || out.stdout.len() > MAX_SNAPSHOT_FILE_BYTES
        || out.stdout.contains(&0)
    {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    if is_valid_text_snapshot(&text) {
        Some(text)
    } else {
        None
    }
}

pub(crate) fn compute_file_diff_counts(before: &str, after: &str) -> (usize, usize) {
    let b_norm = normalize_diff_text(before);
    let a_norm = normalize_diff_text(after);
    if b_norm == a_norm {
        return (0, 0);
    }
    let mut added = 0usize;
    let mut removed = 0usize;
    for d in compute_line_diff(&b_norm, &a_norm) {
        match d.kind {
            DiffLineKind::Added => added += 1,
            DiffLineKind::Removed => removed += 1,
            DiffLineKind::Context => {}
        }
    }
    (added, removed)
}

#[allow(clippy::too_many_arguments)]
fn update_stream_progress(
    sbuf: &Arc<Mutex<Option<StreamingAgentProgress>>>,
    f: impl FnOnce(&mut StreamingAgentProgress),
) {
    StreamingAgentProgress::update_slot(sbuf, f);
}

fn short_rel_path(root: &Path, raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Ok(resolved) = resolve_safe_workspace_path(root, trimmed) {
        if let Ok(rel) = resolved.strip_prefix(root) {
            let s = rel.to_string_lossy().replace('\\', "/");
            if !s.is_empty() {
                return s;
            }
        }
    }
    trimmed.replace('\\', "/")
}

fn first_json_str<'a>(val: &'a Value, keys: &[&str]) -> Option<&'a str> {
    match val {
        Value::Object(map) => {
            for target in keys {
                if let Some(s) = map.get(*target).and_then(|v| v.as_str()) {
                    if !s.trim().is_empty() {
                        return Some(s);
                    }
                }
            }
            for child in map.values() {
                if let Some(found) = first_json_str(child, keys) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => arr.iter().find_map(|item| first_json_str(item, keys)),
        _ => None,
    }
}

fn compact_inline_preview(raw: &str, max_chars: usize) -> String {
    let single = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if single.chars().count() <= max_chars {
        single
    } else {
        let head: String = single.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn format_tool_execution_step(root: &Path, raw_name: &str, payload: &Value) -> String {
    let lower = raw_name.to_ascii_lowercase();
    let label = if lower.contains("read") || lower.contains("view") {
        "读取"
    } else if lower.contains("edit") || lower.contains("replace") || lower.contains("patch") {
        "编辑"
    } else if lower.contains("write") || lower.contains("create") {
        "写入"
    } else if lower.contains("grep") || lower.contains("search") || lower.contains("find") {
        "搜索"
    } else if lower.contains("glob") || lower.contains("list") || lower == "lstoolcall" {
        "浏览目录"
    } else if lower.contains("shell") || lower.contains("bash") || lower.contains("command") || lower.contains("exec") {
        "运行命令"
    } else if lower.contains("web") || lower.contains("fetch") {
        "联网检索"
    } else if lower.contains("todo") || lower.contains("task") {
        "更新任务"
    } else {
        raw_name
    };

    let detail = if lower.contains("shell") || lower.contains("bash") || lower.contains("command") || lower.contains("exec") {
        first_json_str(payload, &["command", "cmd", "script"])
            .map(|s| compact_inline_preview(s, 54))
    } else if lower.contains("grep") || lower.contains("search") {
        first_json_str(payload, &["pattern", "query", "regex", "search_term"])
            .map(|s| compact_inline_preview(s, 48))
    } else if let Some(p) = first_json_str(
        payload,
        &["path", "file_path", "target_file", "relative_workspace_path", "target_directory", "file"],
    ) {
        Some(compact_inline_preview(&short_rel_path(root, p), 56))
    } else {
        first_json_str(payload, &["glob_pattern", "pattern", "query", "url"])
            .map(|s| compact_inline_preview(s, 48))
    };

    match detail {
        Some(d) if !d.is_empty() => format!("{label} {d}"),
        _ => format!("执行 {label}"),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn handle_stream_json_event(
    kind: AgentProviderKind,
    root: &Path,
    v: &Value,
    reply_acc: &Arc<Mutex<String>>,
    thought_acc: &Arc<Mutex<String>>,
    err_slot: &Arc<Mutex<Option<String>>>,
    touched: &Arc<Mutex<Vec<PathBuf>>>,
    sbuf: &Arc<Mutex<Option<StreamingAgentProgress>>>,
) {
    match kind {
        AgentProviderKind::CursorCli | AgentProviderKind::CursorInlineEngine => {
            let ty = v["type"].as_str().unwrap_or("");
            let sub = v["subtype"].as_str().unwrap_or("");
            match ty {
                "thinking" => {
                    if let Some(txt) = v["text"].as_str() {
                        if !txt.is_empty() && (sub == "delta" || sub.is_empty() || thought_acc.lock().is_empty()) {
                            let thought_snapshot = {
                                let mut t = thought_acc.lock();
                                t.push_str(txt);
                                t.clone()
                            };
                            update_stream_progress(sbuf, |p| {
                                p.phase = "思考中...".to_string();
                                p.thought_text = thought_snapshot;
                            });
                        }
                    }
                }
                "assistant" => {
                    let mut chunk = String::new();
                    if let Some(arr) = v["message"]["content"].as_array() {
                        for item in arr {
                            if item["type"].as_str() == Some("thinking") {
                                if let Some(th) = item["thinking"].as_str().or_else(|| item["text"].as_str()) {
                                    let thought_snapshot = {
                                        let mut t = thought_acc.lock();
                                        if t.is_empty() || th.len() > t.len() {
                                            *t = th.to_string();
                                        }
                                        t.clone()
                                    };
                                    update_stream_progress(sbuf, |p| {
                                        p.phase = "思考中...".to_string();
                                        p.thought_text = thought_snapshot;
                                    });
                                }
                            } else if let Some(txt) = item["text"].as_str() {
                                chunk.push_str(txt);
                            }
                        }
                    }
                    if !chunk.is_empty() {
                        let reply_snapshot = {
                            let mut r = reply_acc.lock();
                            if v.get("timestamp_ms").is_some() {
                                r.push_str(&chunk);
                            } else if chunk.len() >= r.len() {
                                *r = chunk;
                            }
                            r.clone()
                        };
                        update_stream_progress(sbuf, |p| {
                            p.phase = "正在生成回复...".to_string();
                            p.reply_text = reply_snapshot;
                        });
                    }
                }
                "tool_call" => {
                    if let Some(obj) = v["tool_call"].as_object() {
                        if let Some(tool_key) = obj.keys().next() {
                            if is_mutating_tool_name(tool_key) {
                                extract_paths_from_json_value(
                                    root,
                                    &v["tool_call"][tool_key],
                                    touched,
                                );
                            }
                            let step = format_tool_execution_step(root, tool_key, &v["tool_call"][tool_key]);
                            update_stream_progress(sbuf, |p| {
                                p.phase = if sub == "completed" {
                                    "继续分析中...".to_string()
                                } else {
                                    "正在执行工具...".to_string()
                                };
                                p.push_execution_step(step);
                            });
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
                        let reply_snapshot = {
                            let mut r = reply_acc.lock();
                            if r.trim().is_empty() && !res_txt.trim().is_empty() {
                                *r = res_txt.to_string();
                            }
                            r.clone()
                        };
                        if !reply_snapshot.is_empty() {
                            update_stream_progress(sbuf, |p| {
                                p.reply_text = reply_snapshot;
                            });
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
                        let reply_snapshot = {
                            let mut r = reply_acc.lock();
                            r.push_str(delta);
                            r.clone()
                        };
                        update_stream_progress(sbuf, |p| {
                            p.phase = "正在生成回复...".to_string();
                            p.reply_text = reply_snapshot;
                        });
                    }
                } else if step_type.contains("thought") || step_type.contains("think") || step_type == "planner_response" {
                    if let Some(delta) = su["text_delta"]
                        .as_str()
                        .or_else(|| su["thought"].as_str())
                        .or_else(|| su["thinking"].as_str())
                        .or_else(|| su["text"].as_str())
                    {
                        let thought_snapshot = {
                            let mut t = thought_acc.lock();
                            t.push_str(delta);
                            t.clone()
                        };
                        update_stream_progress(sbuf, |p| {
                            p.phase = "思考中...".to_string();
                            p.thought_text = thought_snapshot;
                        });
                    }
                } else {
                    if !step_type.is_empty() && step_type != "user_input" {
                        let tool_name = su["tool_name"].as_str().unwrap_or(step_type);
                        if is_mutating_tool_name(tool_name) || is_mutating_tool_name(step_type) {
                            extract_paths_from_json_value(root, su, touched);
                        }
                        let step = format_tool_execution_step(root, tool_name, su);
                        update_stream_progress(sbuf, |p| {
                            p.phase = "正在执行步骤...".to_string();
                            p.push_execution_step(step);
                        });
                    }
                }
            } else if ev == "result" {
                let res = &v["result"];
                let status = res["status"].as_str().unwrap_or("");
                if status.eq_ignore_ascii_case("ERROR") {
                    let msg = res["error"].as_str().unwrap_or("Antigravity CLI 执行失败");
                    *err_slot.lock() = Some(msg.to_string());
                } else if let Some(resp_txt) = res["response"].as_str() {
                    let reply_snapshot = {
                        let mut r = reply_acc.lock();
                        if r.trim().is_empty() && !resp_txt.trim().is_empty() {
                            *r = resp_txt.to_string();
                        }
                        r.clone()
                    };
                    if !reply_snapshot.is_empty() {
                        update_stream_progress(sbuf, |p| {
                            p.reply_text = reply_snapshot;
                        });
                    }
                }
            }
        }
        AgentProviderKind::ClaudeCodeCli => {
            let ty = v["type"].as_str().unwrap_or("");
            match ty {
                "stream_event" => {
                    let ev = &v["event"];
                    match ev["type"].as_str().unwrap_or("") {
                        "content_block_start" => {
                            let cb = &ev["content_block"];
                            if cb["type"].as_str() == Some("tool_use") {
                                let name = cb["name"].as_str().unwrap_or("tool");
                                let step = format_tool_execution_step(root, name, &cb["input"]);
                                update_stream_progress(sbuf, |p| {
                                    p.phase = "正在执行工具...".to_string();
                                    p.push_execution_step(step);
                                });
                            }
                        }
                        "content_block_delta" => {
                            let delta = &ev["delta"];
                            match delta["type"].as_str().unwrap_or("") {
                                "thinking_delta" => {
                                    if let Some(txt) = delta["thinking"].as_str() {
                                        let thought_snapshot = {
                                            let mut t = thought_acc.lock();
                                            t.push_str(txt);
                                            t.clone()
                                        };
                                        update_stream_progress(sbuf, |p| {
                                            p.phase = "思考中...".to_string();
                                            p.thought_text = thought_snapshot;
                                        });
                                    }
                                }
                                "text_delta" => {
                                    if let Some(txt) = delta["text"].as_str() {
                                        let reply_snapshot = {
                                            let mut r = reply_acc.lock();
                                            r.push_str(txt);
                                            r.clone()
                                        };
                                        update_stream_progress(sbuf, |p| {
                                            p.phase = "正在生成回复...".to_string();
                                            p.reply_text = reply_snapshot;
                                        });
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                "assistant" => {
                    let mut text_chunk = String::new();
                    if let Some(arr) = v["message"]["content"].as_array() {
                        for item in arr {
                            match item["type"].as_str().unwrap_or("") {
                                "thinking" => {
                                    if let Some(th) = item["thinking"].as_str() {
                                        let thought_snapshot = {
                                            let mut t = thought_acc.lock();
                                            if t.is_empty() || th.len() > t.len() {
                                                *t = th.to_string();
                                            }
                                            t.clone()
                                        };
                                        update_stream_progress(sbuf, |p| {
                                            p.phase = "思考中...".to_string();
                                            p.thought_text = thought_snapshot;
                                        });
                                    }
                                }
                                "tool_use" => {
                                    let name = item["name"].as_str().unwrap_or("tool");
                                    if is_mutating_tool_name(name) {
                                        extract_paths_from_json_value(root, &item["input"], touched);
                                    }
                                    let step = format_tool_execution_step(root, name, &item["input"]);
                                    update_stream_progress(sbuf, |p| {
                                        p.phase = "正在执行工具...".to_string();
                                        p.push_execution_step(step);
                                    });
                                }
                                "text" => {
                                    if let Some(txt) = item["text"].as_str() {
                                        text_chunk.push_str(txt);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    if !text_chunk.is_empty() {
                        let reply_snapshot = {
                            let mut r = reply_acc.lock();
                            if r.is_empty() || text_chunk.len() >= r.len() {
                                *r = text_chunk;
                            }
                            r.clone()
                        };
                        update_stream_progress(sbuf, |p| {
                            p.phase = "正在生成回复...".to_string();
                            p.reply_text = reply_snapshot;
                        });
                    }
                }
                "result" => {
                    if v["is_error"].as_bool() == Some(true) {
                        let msg = v["result"].as_str().unwrap_or("Claude CLI 返回错误");
                        *err_slot.lock() = Some(msg.to_string());
                    } else if let Some(res_txt) = v["result"].as_str() {
                        let reply_snapshot = {
                            let mut r = reply_acc.lock();
                            if r.trim().is_empty() && !res_txt.trim().is_empty() {
                                *r = res_txt.to_string();
                            }
                            r.clone()
                        };
                        if !reply_snapshot.is_empty() {
                            update_stream_progress(sbuf, |p| {
                                p.reply_text = reply_snapshot;
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        AgentProviderKind::OpenAiCodexCli => {
            let ty = v["type"].as_str().unwrap_or("");
            match ty {
                "turn.started" => {
                    update_stream_progress(sbuf, |p| {
                        p.phase = "正在分析任务...".to_string();
                    });
                }
                "item.started" | "item.updated" | "item.completed" => {
                    let item = &v["item"];
                    match item["type"].as_str().unwrap_or("") {
                        "reasoning" => {
                            if let Some(txt) = item["text"].as_str() {
                                let thought_snapshot = {
                                    let mut t = thought_acc.lock();
                                    if txt.starts_with(t.as_str()) || t.is_empty() {
                                        *t = txt.to_string();
                                    } else {
                                        if !t.is_empty() && !t.ends_with('\n') {
                                            t.push('\n');
                                        }
                                        t.push_str(txt);
                                    }
                                    t.clone()
                                };
                                update_stream_progress(sbuf, |p| {
                                    p.phase = "思考中...".to_string();
                                    p.thought_text = thought_snapshot;
                                });
                            }
                        }
                        "command_execution" => {
                            let cmd = item["command"].as_str().unwrap_or("");
                            let step = if cmd.trim().is_empty() {
                                "运行终端命令".to_string()
                            } else {
                                format!("运行命令 {}", compact_inline_preview(cmd, 54))
                            };
                            update_stream_progress(sbuf, |p| {
                                p.phase = "正在执行命令...".to_string();
                                p.push_execution_step(step);
                            });
                        }
                        "file_change" => {
                            extract_paths_from_json_value(root, &item["changes"], touched);
                            let step = format_tool_execution_step(root, "edit", &item["changes"]);
                            update_stream_progress(sbuf, |p| {
                                p.phase = "正在修改文件...".to_string();
                                p.push_execution_step(step);
                            });
                        }
                        "mcp_tool_call" | "web_search" => {
                            let name = item["tool"].as_str().unwrap_or("tool");
                            let step = format_tool_execution_step(root, name, item);
                            update_stream_progress(sbuf, |p| {
                                p.phase = "正在执行工具...".to_string();
                                p.push_execution_step(step);
                            });
                        }
                        "agent_message" => {
                            if let Some(txt) = item["text"].as_str() {
                                let reply_snapshot = {
                                    let mut r = reply_acc.lock();
                                    if txt.len() >= r.len() || r.is_empty() {
                                        *r = txt.to_string();
                                    }
                                    r.clone()
                                };
                                update_stream_progress(sbuf, |p| {
                                    p.phase = "正在生成回复...".to_string();
                                    p.reply_text = reply_snapshot;
                                });
                            }
                        }
                        _ => {}
                    }
                }
                "error" => {
                    if let Some(msg) = v["message"].as_str().or_else(|| v["error"].as_str()) {
                        *err_slot.lock() = Some(msg.to_string());
                    }
                }
                _ => {}
            }
        }
        AgentProviderKind::CustomLlmApi => {}
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
    let norm_root = normalize_abs_path(root);
    let git_base = git_repo_toplevel(&norm_root).unwrap_or_else(|| norm_root.clone());
    let mut cmd = Command::new("git");
    cmd.args(["-c", "core.quotePath=false", "status", "--porcelain", "-uall"])
        .current_dir(&git_base)
        .stdin(Stdio::null());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    if let Ok(out) = cmd.output() {
        if out.status.success() {
            let txt = String::from_utf8_lossy(&out.stdout);
            for line in txt.lines() {
                if line.len() >= 4 {
                    let raw_path = line[3..].trim();
                    let rel = raw_path
                        .split(" -> ")
                        .last()
                        .unwrap_or(raw_path)
                        .trim()
                        .trim_matches('"');
                    let full = normalize_abs_path(&git_base.join(rel));
                    if !paths_same_file(&git_base, &norm_root)
                        && strip_prefix_norm(&full, &norm_root).is_none()
                    {
                        continue;
                    }
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
            #[cfg(not(test))]
            crate::browser_bridge::ensure_cursor_persistent_chrome_mcp(Some(&params.bin));

            let mut args = vec![
                "-p".to_string(),
                "--trust".to_string(),
                "--approve-mcps".to_string(),
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
            if let Some(sid) = params.resume_session_id.as_deref().filter(|s| !s.trim().is_empty())
            {
                args.push("--resume".to_string());
                args.push(sid.trim().to_string());
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
            suppress_agy_background_updater_window();
            let native_bin = resolve_native_agy_binary(&params.bin);
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
            if let Some(sid) = params.resume_session_id.as_deref().filter(|s| !s.trim().is_empty())
            {
                args.push("--conversation".to_string());
                args.push(sid.trim().to_string());
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
            spawn_agy_piped_cli(&native_bin, &params.root, &args)
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
                "stream-json".to_string(),
                "--verbose".to_string(),
                "--include-partial-messages".to_string(),
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
            if let Some(sid) = params.resume_session_id.as_deref().filter(|s| !s.trim().is_empty())
            {
                args.push("--resume".to_string());
                args.push(sid.trim().to_string());
                if params.fork_session {
                    args.push("--fork-session".to_string());
                }
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
                "--json".to_string(),
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
        AgentProviderKind::CustomLlmApi => {
            spawn_generic_piped_cli(&params.bin, &params.root, &[], true)
        }
    }
}

fn spawn_agy_piped_cli(bin: &Path, cwd: &Path, args: &[String]) -> std::io::Result<Child> {
    let mut cmd = build_hidden_piped_cli_command(bin, cwd, args, false);
    cmd.env("DO_NOT_TRACK", "1")
        .env("GEMINI_TELEMETRY_ENABLED", "false")
        .env_remove("GEMINI_API_KEY")
        .env_remove("GOOGLE_API_KEY")
        .env_remove("GOOGLE_GEMINI_BASE_URL")
        .env_remove("GOOGLE_GEMINI_MODEL");
    if let Some(home) = user_home_dir() {
        let env_file = home.join(".gemini").join(".env");
        if let Ok(raw) = fs::read_to_string(&env_file) {
            for line in raw.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = trimmed.split_once('=') {
                    let key = k.trim();
                    let val = v.trim().trim_matches('"').trim_matches('\'');
                    if !key.is_empty() && !val.is_empty() {
                        cmd.env(key, val);
                    }
                }
            }
        }
    }
    cmd.spawn()
}

fn build_hidden_piped_cli_command(
    bin: &Path,
    cwd: &Path,
    args: &[String],
    pipe_stdin: bool,
) -> Command {
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
                .stdin(stdin_cfg())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(CREATE_NO_WINDOW);
            return cmd;
        }
    }

    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(stdin_cfg())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

fn spawn_generic_piped_cli(
    bin: &Path,
    cwd: &Path,
    args: &[String],
    pipe_stdin: bool,
) -> std::io::Result<Child> {
    build_hidden_piped_cli_command(bin, cwd, args, pipe_stdin).spawn()
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

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn normalize_abs_path_resolves_ancestors_for_missing_files() {
        let base = std::env::temp_dir().join(format!("rust_ai_ide_norm_{}", Uuid::new_v4()));
        let sub = base.join("nested");
        fs::create_dir_all(&sub).unwrap();

        let canonical_base = normalize_abs_path(&base);
        let uncanonical = sub.join("..").join("deleted_dir").join("missing.rs");
        let expected = canonical_base.join("deleted_dir").join("missing.rs");

        assert!(paths_same_file(&uncanonical, &expected));
        assert_eq!(
            strip_prefix_norm(&uncanonical, &base).as_deref(),
            Some("deleted_dir/missing.rs")
        );

        let _ = fs::remove_dir_all(&base);
    }
}
