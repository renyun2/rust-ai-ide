use anyhow::Result;
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitFileStatusKind {
    Modified,
    Added,
    Deleted,
    Untracked,
}

impl GitFileStatusKind {
    pub fn badge_str(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Untracked => "U",
        }
    }

    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Modified => "已修改",
            Self::Added => "新增",
            Self::Deleted => "已删除",
            Self::Untracked => "未跟踪",
        }
    }

    pub fn label_en(self) -> &'static str {
        match self {
            Self::Modified => "Modified",
            Self::Added => "Added",
            Self::Deleted => "Deleted",
            Self::Untracked => "Untracked",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitChangedFile {
    pub path: PathBuf,
    pub status: GitFileStatusKind,
    pub staged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_lineno: Option<usize>,
    pub new_lineno: Option<usize>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLogEntry {
    pub hash: String,
    pub full_hash: String,
    pub parents: Vec<String>,
    pub author: String,
    pub author_email: String,
    pub date: String,
    pub refs: Vec<String>,
    pub subject: String,
    pub graph_lane: usize,
    pub is_merge: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCommitFileChange {
    pub path: String,
    pub status: GitFileStatusKind,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitCommitDetail {
    pub hash: String,
    pub full_hash: String,
    pub parents: Vec<String>,
    pub author: String,
    pub author_email: String,
    pub date: String,
    pub refs: Vec<String>,
    pub subject: String,
    pub body: String,
    pub files: Vec<GitCommitFileChange>,
    pub file_diffs: HashMap<String, Vec<DiffLine>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitStashEntry {
    pub index_ref: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitRemoteEntry {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum GitResetMode {
    Soft,
    Mixed,
    Hard,
}

impl GitResetMode {
    pub fn as_flag(self) -> &'static str {
        match self {
            Self::Soft => "--soft",
            Self::Mixed => "--mixed",
            Self::Hard => "--hard",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitBlameLine {
    pub commit_short: String,
    pub author: String,
    pub date: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
struct GitSnapshot {
    git_installed: bool,
    is_git_repo: bool,
    git_binary_path: Option<PathBuf>,
    git_version: Option<String>,
    current_branch: String,
    local_branches: Vec<String>,
    remote_branches: Vec<String>,
    tags: Vec<String>,
    ahead_behind: (usize, usize),
    stashes: Vec<GitStashEntry>,
    remotes: Vec<GitRemoteEntry>,
    in_rebase: bool,
    in_merge: bool,
    changed_files: Vec<GitChangedFile>,
    working_diffs: HashMap<PathBuf, Vec<DiffLine>>,
    recent_commits: Vec<GitLogEntry>,
    head_detail: Option<GitCommitDetail>,
}

#[derive(Debug)]
enum GitWorkerCmd {
    Refresh,
    InitRepo {
        create_gitignore: bool,
    },
    StageAll,
    UnstageAll,
    ToggleStageFile {
        file: PathBuf,
        currently_staged: bool,
    },
    RollbackFile(PathBuf),
    RollbackFiles(Vec<PathBuf>),
    RollbackAll,
    Commit {
        message: String,
        amend: bool,
        push_after: bool,
        files: Vec<PathBuf>,
    },
    CheckoutBranch(String),
    CreateAndCheckoutBranch {
        name: String,
        start_point: Option<String>,
    },
    DeleteBranch(String),
    CreateTag {
        name: String,
        start_point: Option<String>,
    },
    DeleteTag(String),
    MergeBranch(String),
    MergeAbort,
    RebaseBranch(String),
    RebaseAbort,
    RebaseContinue,
    StashSave {
        message: String,
        include_untracked: bool,
    },
    StashApply(String),
    StashPop(String),
    StashDrop(String),
    AddRemote {
        name: String,
        url: String,
    },
    RemoveRemote(String),
    CherryPick(String),
    RevertCommit(String),
    Reset {
        target: String,
        mode: GitResetMode,
    },
    Fetch,
    Pull,
    Push,
    LoadCommitDetail(String),
    LoadFileHistory(PathBuf),
    LoadFileBlame(PathBuf),
    AiCommitDiff(Vec<PathBuf>),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiCommitContext {
    pub diff_text: String,
    pub changed_files: Vec<String>,
    pub recent_commit_messages: Vec<String>,
    pub style_summary: String,
    pub commit_template: Option<String>,
    pub commitlint_config: Option<String>,
}

#[derive(Debug)]
enum GitWorkerEvent {
    SnapshotUpdated {
        snapshot: GitSnapshot,
        notification: Option<String>,
        output: Option<String>,
        clear_commit_input: bool,
    },
    CommitDetailLoaded {
        detail: GitCommitDetail,
    },
    FileHistoryLoaded {
        path: PathBuf,
        entries: Vec<GitLogEntry>,
    },
    FileBlameLoaded {
        path: PathBuf,
        lines: Vec<GitBlameLine>,
    },
    AiDiffReady {
        snapshot: GitSnapshot,
        context: AiCommitContext,
    },
}

#[derive(Debug, Default)]
pub struct GitPollOutcome {
    pub changed: bool,
    pub notification: Option<String>,
    pub clear_commit_input: bool,
    pub ai_diff_ready: Option<AiCommitContext>,
}

pub struct GitEngine {
    pub root: PathBuf,
    pub git_installed: bool,
    pub is_git_repo: bool,
    pub git_binary_path: Option<PathBuf>,
    pub git_version: Option<String>,
    pub current_branch: String,
    pub local_branches: Vec<String>,
    pub remote_branches: Vec<String>,
    pub tags: Vec<String>,
    pub ahead_behind: (usize, usize),
    pub stashes: Vec<GitStashEntry>,
    pub remotes: Vec<GitRemoteEntry>,
    pub in_rebase: bool,
    pub in_merge: bool,
    pub changed_files: Vec<GitChangedFile>,
    pub commit_checked_files: HashSet<PathBuf>,
    known_changed_files: HashSet<PathBuf>,
    pub working_diffs: HashMap<PathBuf, Vec<DiffLine>>,
    pub recent_commits: Vec<GitLogEntry>,
    pub commit_details: HashMap<String, GitCommitDetail>,
    pub file_histories: HashMap<PathBuf, Vec<GitLogEntry>>,
    pub file_blames: HashMap<PathBuf, Vec<GitBlameLine>>,
    pub commit_message_draft: String,
    pub last_operation_output: String,
    pub has_loaded_snapshot: bool,
    cmd_tx: mpsc::Sender<GitWorkerCmd>,
    event_rx: mpsc::Receiver<GitWorkerEvent>,
    pending_refresh: Arc<AtomicBool>,
    is_busy: Arc<AtomicBool>,
}

pub const GIT_DOWNLOAD_URL: &str = "https://git-scm.com/downloads";

/// Detects the local `git` executable from PATH or standard OS installation directories.
pub fn detect_git_executable() -> Option<PathBuf> {
    let exe_names: &[&str] = if cfg!(target_os = "windows") {
        &["git.exe", "git.cmd", "git"]
    } else {
        &["git"]
    };
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in exe_names {
                let cand = dir.join(name);
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        let mut candidates = Vec::new();
        for env_key in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = std::env::var_os(env_key) {
                let p = PathBuf::from(base);
                candidates.push(p.join("Git").join("cmd").join("git.exe"));
                candidates.push(p.join("Git").join("bin").join("git.exe"));
            }
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let p = PathBuf::from(local);
            candidates.push(p.join("Programs").join("Git").join("cmd").join("git.exe"));
            candidates.push(p.join("Programs").join("Git").join("bin").join("git.exe"));
        }
        for cand in candidates {
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        for p in [
            "/usr/bin/git",
            "/usr/local/bin/git",
            "/opt/homebrew/bin/git",
        ] {
            let cand = PathBuf::from(p);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Returns the trimmed `git --version` string when `bin` executes cleanly.
pub fn git_version_string(bin: &Path) -> Option<String> {
    let mut cmd = Command::new(bin);
    cmd.arg("--version");
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_0400);
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Derives a local folder name from a remote Git URL (HTTPS or SSH).
pub fn derive_repo_name_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return None;
    }
    let segment = trimmed
        .rsplit(['/', ':', '\\'])
        .next()
        .map(str::trim)
        .unwrap_or("");
    let name = segment.strip_suffix(".git").unwrap_or(segment).trim();
    if name.is_empty() || name == "." || name == ".." {
        None
    } else {
        Some(name.to_string())
    }
}

/// Downloads and installs Git silently (or via platform package manager) when missing locally.
pub fn install_git_blocking() -> Result<PathBuf, String> {
    if let Some(existing) = detect_git_executable() {
        return Ok(existing);
    }

    #[cfg(target_os = "windows")]
    {
        // 1. Download latest official Git for Windows installer directly from GitHub Releases
        let arch_suffix = if cfg!(target_arch = "aarch64") {
            "arm64.exe"
        } else {
            "64-bit.exe"
        };
        let mut api_cmd = Command::new("curl.exe");
        api_cmd.args([
            "-fsSL",
            "-H",
            "User-Agent: Rust-AI-IDE",
            "https://api.github.com/repos/git-for-windows/git/releases/latest",
        ]);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            api_cmd.creation_flags(0x0800_0000);
        }
        if let Ok(api_out) = api_cmd.output() {
            if api_out.status.success() {
                let json_txt = String::from_utf8_lossy(&api_out.stdout);
                if let Some(dl_url) = extract_git_installer_download_url(&json_txt, arch_suffix) {
                    let installer_path = std::env::temp_dir().join("rust-ai-ide-git-setup.exe");
                    let mut dl_cmd = Command::new("curl.exe");
                    dl_cmd
                        .args(["-fL", "--retry", "2", "-o"])
                        .arg(&installer_path)
                        .arg(&dl_url);
                    {
                        use std::os::windows::process::CommandExt;
                        dl_cmd.creation_flags(0x0800_0000);
                    }
                    if dl_cmd.output().is_ok_and(|o| o.status.success()) && installer_path.is_file()
                    {
                        let mut installer_cmd = Command::new(&installer_path);
                        installer_cmd.args(["/VERYSILENT", "/NORESTART", "/NOCANCEL", "/SP-"]);
                        {
                            use std::os::windows::process::CommandExt;
                            installer_cmd.creation_flags(0x0800_0000);
                        }
                        let run_res = installer_cmd.output();
                        let ok = match run_res {
                            Ok(o) if o.status.success() => true,
                            _ => {
                                // Elevation may be required for Program Files install
                                let ps_script = format!(
                                    "Start-Process -FilePath '{}' -ArgumentList '/VERYSILENT','/NORESTART','/NOCANCEL','/SP-' -Verb RunAs -Wait",
                                    installer_path.display().to_string().replace('\'', "''")
                                );
                                let mut ps_cmd = Command::new("powershell.exe");
                                ps_cmd.args([
                                    "-NoProfile",
                                    "-ExecutionPolicy",
                                    "Bypass",
                                    "-Command",
                                    &ps_script,
                                ]);
                                {
                                    use std::os::windows::process::CommandExt;
                                    ps_cmd.creation_flags(0x0800_0000);
                                }
                                ps_cmd.output().is_ok_and(|o| o.status.success())
                            }
                        };
                        let _ = std::fs::remove_file(&installer_path);
                        if ok {
                            if let Some(bin) = detect_git_executable() {
                                return Ok(bin);
                            }
                        }
                    }
                }
            }
        }

        // 2. Fallback to winget
        let mut wg = Command::new("winget");
        wg.args([
            "install",
            "--id",
            "Git.Git",
            "-e",
            "--source",
            "winget",
            "--accept-package-agreements",
            "--accept-source-agreements",
        ]);
        {
            use std::os::windows::process::CommandExt;
            wg.creation_flags(0x0800_0000);
        }
        match wg.output() {
            Ok(out) if out.status.success() => {
                if let Some(bin) = detect_git_executable() {
                    return Ok(bin);
                }
                return Err("Git 安装命令已执行，但未检测到 git.exe，请重启 IDE 或手动安装".to_string());
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                return Err(if !err.is_empty() { err } else if !stdout.is_empty() { stdout } else { "Git 自动安装失败，请从官网下载安装".to_string() });
            }
            Err(e) => {
                return Err(format!("无法启动 Git 安装程序: {e}"));
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if Command::new("brew").arg("--version").output().is_ok_and(|o| o.status.success()) {
            let out = Command::new("brew")
                .args(["install", "git"])
                .output()
                .map_err(|e| format!("brew install git 失败: {e}"))?;
            if out.status.success() {
                if let Some(bin) = detect_git_executable() {
                    return Ok(bin);
                }
            }
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if err.is_empty() { "brew install git 失败".to_string() } else { err });
        }
        let _ = Command::new("xcode-select").arg("--install").spawn();
        return Err("已触发 macOS Xcode Command Line Tools 安装，请在系统弹窗中确认并在完成后刷新".to_string());
    }

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        for (mgr, args) in [
            ("apt-get", vec!["install", "-y", "git"]),
            ("dnf", vec!["install", "-y", "git"]),
            ("pacman", vec!["-S", "--noconfirm", "git"]),
        ] {
            if Command::new("which").arg(mgr).output().is_ok_and(|o| o.status.success()) {
                let out = Command::new("pkexec")
                    .arg(mgr)
                    .args(&args)
                    .output()
                    .map_err(|e| format!("安装 Git 失败: {e}"))?;
                if out.status.success() {
                    if let Some(bin) = detect_git_executable() {
                        return Ok(bin);
                    }
                }
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(if err.is_empty() { format!("{mgr} 安装 Git 失败") } else { err });
            }
        }
        Err("未找到受支持的包管理器，请访问 git-scm.com 安装 Git".to_string())
    }
}

#[cfg(target_os = "windows")]
fn extract_git_installer_download_url(json_txt: &str, arch_suffix: &str) -> Option<String> {
    for line in json_txt.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("\"browser_download_url\"") {
            if let Some(colon) = rest.find(':') {
                let url = rest[colon + 1..]
                    .trim()
                    .trim_end_matches(',')
                    .trim()
                    .trim_matches('"');
                if url.contains("/Git-") && url.ends_with(arch_suffix) {
                    return Some(url.to_string());
                }
            }
        }
    }
    None
}

/// Clones a remote Git repository into `target_dir` and returns the destination path on success.
pub fn clone_repository_blocking(url: &str, target_dir: &Path) -> Result<PathBuf, String> {
    let clean_url = url.trim();
    if clean_url.is_empty() {
        return Err("请输入 Git 远程仓库地址".to_string());
    }
    if target_dir.as_os_str().is_empty() {
        return Err("请选择本地克隆目录".to_string());
    }
    let Some(git_bin) = detect_git_executable() else {
        return Err("本机尚未安装 Git，请先点击下载安装 Git".to_string());
    };
    if target_dir.exists() {
        let has_entries = std::fs::read_dir(target_dir)
            .ok()
            .and_then(|mut rd| rd.next())
            .is_some();
        if has_entries {
            return Err(format!(
                "目标目录已存在且非空: {}",
                target_dir.display()
            ));
        }
    } else if let Some(parent) = target_dir.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("无法创建父目录 {}: {e}", parent.display()))?;
        }
    }

    let mut cmd = Command::new(git_bin);
    cmd.args(["clone", clean_url]).arg(target_dir);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_0400);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("无法执行 git clone: {e}"))?;
    if out.status.success() {
        Ok(target_dir.to_path_buf())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Err(if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            "git clone 失败".to_string()
        })
    }
}

fn git_command(root: &Path) -> Command {
    let mut cmd = match detect_git_executable() {
        Some(bin) => Command::new(bin),
        None => Command::new("git"),
    };
    cmd.current_dir(root);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_0400);
    }
    cmd
}

const MAX_DIFF_LINES_PER_FILE: usize = 1500;
const MAX_TOTAL_DIFF_LINES: usize = 8000;
const MAX_DIFF_LINE_BYTES: usize = 1024;
const MAX_UNTRACKED_DIFF_FILE_BYTES: u64 = 256 * 1024;
const MAX_COMMIT_DETAILS_CACHE: usize = 16;
const MAX_FILE_HISTORY_CACHE: usize = 8;
const MAX_FILE_BLAME_CACHE: usize = 8;

fn clip_diff_line(s: &str) -> String {
    if s.len() <= MAX_DIFF_LINE_BYTES {
        s.to_string()
    } else {
        let mut end = MAX_DIFF_LINE_BYTES;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

fn parse_unified_diff_sections(
    diff_text: &str,
) -> (Vec<GitCommitFileChange>, HashMap<String, Vec<DiffLine>>) {
    let mut files: Vec<GitCommitFileChange> = Vec::new();
    let mut file_diffs: HashMap<String, Vec<DiffLine>> = HashMap::new();

    let mut current_path: Option<String> = None;
    let mut current_status = GitFileStatusKind::Modified;
    let mut current_lines: Vec<DiffLine> = Vec::new();
    let mut current_truncated = false;
    let mut total_stored_lines = 0usize;
    let mut additions = 0usize;
    let mut deletions = 0usize;
    let mut old_ln = 1usize;
    let mut new_ln = 1usize;
    let mut in_hunk = false;

    let flush_current = |path_opt: &mut Option<String>,
                         status: GitFileStatusKind,
                         add: usize,
                         del: usize,
                         lines: &mut Vec<DiffLine>,
                         truncated: &mut bool,
                         files_out: &mut Vec<GitCommitFileChange>,
                         diffs_out: &mut HashMap<String, Vec<DiffLine>>| {
        if let Some(path) = path_opt.take() {
            if *truncated {
                lines.push(DiffLine {
                    kind: DiffLineKind::Context,
                    old_lineno: None,
                    new_lineno: None,
                    content: "... (diff truncated to save memory)".to_string(),
                });
            }
            files_out.push(GitCommitFileChange {
                path: path.clone(),
                status,
                additions: add,
                deletions: del,
            });
            diffs_out.insert(path, std::mem::take(lines));
            *truncated = false;
        }
    };

    for raw_line in diff_text.lines() {
        if let Some(rest) = raw_line.strip_prefix("diff --git ") {
            flush_current(
                &mut current_path,
                current_status,
                additions,
                deletions,
                &mut current_lines,
                &mut current_truncated,
                &mut files,
                &mut file_diffs,
            );
            current_status = GitFileStatusKind::Modified;
            additions = 0;
            deletions = 0;
            in_hunk = false;

            let extracted = rest
                .split_whitespace()
                .last()
                .unwrap_or("")
                .trim_matches('"');
            let clean = extracted.strip_prefix("b/").unwrap_or(extracted).to_string();
            if !clean.is_empty() {
                current_path = Some(clean);
            }
            continue;
        }

        if current_path.is_none() {
            continue;
        }

        if raw_line.starts_with("new file mode") {
            current_status = GitFileStatusKind::Added;
            continue;
        }
        if raw_line.starts_with("deleted file mode") {
            current_status = GitFileStatusKind::Deleted;
            continue;
        }
        if raw_line.starts_with("@@ ") {
            in_hunk = true;
            // Parse @@ -old_start,old_len +new_start,new_len @@
            let header_inner = raw_line.trim_start_matches("@@ ");
            if let Some((coords, _)) = header_inner.split_once(" @@") {
                for part in coords.split_whitespace() {
                    if let Some(old_s) = part.strip_prefix('-') {
                        let start = old_s.split(',').next().unwrap_or("1");
                        old_ln = start.parse::<usize>().unwrap_or(1);
                    } else if let Some(new_s) = part.strip_prefix('+') {
                        let start = new_s.split(',').next().unwrap_or("1");
                        new_ln = start.parse::<usize>().unwrap_or(1);
                    }
                }
            }
            if current_lines.len() < MAX_DIFF_LINES_PER_FILE
                && total_stored_lines < MAX_TOTAL_DIFF_LINES
            {
                current_lines.push(DiffLine {
                    kind: DiffLineKind::Context,
                    old_lineno: None,
                    new_lineno: None,
                    content: clip_diff_line(raw_line),
                });
                total_stored_lines += 1;
            } else {
                current_truncated = true;
            }
            continue;
        }

        if !in_hunk {
            continue;
        }

        let can_push = current_lines.len() < MAX_DIFF_LINES_PER_FILE
            && total_stored_lines < MAX_TOTAL_DIFF_LINES;
        if let Some(added) = raw_line.strip_prefix('+') {
            additions += 1;
            if can_push {
                current_lines.push(DiffLine {
                    kind: DiffLineKind::Added,
                    old_lineno: None,
                    new_lineno: Some(new_ln),
                    content: clip_diff_line(added),
                });
                total_stored_lines += 1;
            } else {
                current_truncated = true;
            }
            new_ln += 1;
        } else if let Some(removed) = raw_line.strip_prefix('-') {
            deletions += 1;
            if can_push {
                current_lines.push(DiffLine {
                    kind: DiffLineKind::Removed,
                    old_lineno: Some(old_ln),
                    new_lineno: None,
                    content: clip_diff_line(removed),
                });
                total_stored_lines += 1;
            } else {
                current_truncated = true;
            }
            old_ln += 1;
        } else if let Some(ctx) = raw_line.strip_prefix(' ') {
            if can_push {
                current_lines.push(DiffLine {
                    kind: DiffLineKind::Context,
                    old_lineno: Some(old_ln),
                    new_lineno: Some(new_ln),
                    content: clip_diff_line(ctx),
                });
                total_stored_lines += 1;
            } else {
                current_truncated = true;
            }
            old_ln += 1;
            new_ln += 1;
        }
    }

    flush_current(
        &mut current_path,
        current_status,
        additions,
        deletions,
        &mut current_lines,
        &mut current_truncated,
        &mut files,
        &mut file_diffs,
    );

    (files, file_diffs)
}

fn collect_commit_detail(root: &Path, rev: &str) -> Option<GitCommitDetail> {
    let meta_out = git_command(root)
        .args([
            "show",
            "--quiet",
            "--date=short",
            "--pretty=format:%h|%H|%P|%an|%ae|%ad|%D|%s%n%b",
            rev,
        ])
        .output()
        .ok()?;
    if !meta_out.status.success() {
        return None;
    }
    let meta_text = String::from_utf8_lossy(&meta_out.stdout);
    let mut lines = meta_text.lines();
    let header_line = lines.next()?;
    let mut parts = header_line.splitn(8, '|');
    let hash = parts.next()?.trim().to_string();
    let full_hash = parts.next().unwrap_or(&hash).trim().to_string();
    let parents: Vec<String> = parts
        .next()
        .unwrap_or("")
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let author = parts.next().unwrap_or("").trim().to_string();
    let author_email = parts.next().unwrap_or("").trim().to_string();
    let date = parts.next().unwrap_or("").trim().to_string();
    let refs: Vec<String> = parts
        .next()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let subject = parts.next().unwrap_or("").trim().to_string();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();

    let diff_out = git_command(root)
        .args(["show", "--format=", "--unified=3", rev])
        .output()
        .ok();
    let diff_text = diff_out
        .as_ref()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let (mut files, file_diffs) = parse_unified_diff_sections(&diff_text);

    if files.is_empty() {
        if let Ok(ns_out) = git_command(root)
            .args(["diff-tree", "--no-commit-id", "--name-status", "-r", "--root", rev])
            .output()
        {
            if ns_out.status.success() {
                let ns_txt = String::from_utf8_lossy(&ns_out.stdout);
                for line in ns_txt.lines() {
                    let mut cols = line.split_whitespace();
                    if let (Some(st), Some(p)) = (cols.next(), cols.next()) {
                        let status = match st.chars().next().unwrap_or('M') {
                            'A' => GitFileStatusKind::Added,
                            'D' => GitFileStatusKind::Deleted,
                            _ => GitFileStatusKind::Modified,
                        };
                        files.push(GitCommitFileChange {
                            path: p.to_string(),
                            status,
                            additions: 0,
                            deletions: 0,
                        });
                    }
                }
            }
        }
    }

    Some(GitCommitDetail {
        hash,
        full_hash,
        parents,
        author,
        author_email,
        date,
        refs,
        subject,
        body,
        files,
        file_diffs,
    })
}

fn assign_commit_graph_lanes(commits: &mut [GitLogEntry]) {
    let mut active_lanes: Vec<Option<String>> = Vec::new();

    for entry in commits.iter_mut() {
        let found_lane = active_lanes.iter().position(|slot| {
            slot.as_deref().is_some_and(|expected| {
                expected == entry.full_hash
                    || expected.starts_with(&entry.hash)
                    || entry.full_hash.starts_with(expected)
            })
        });

        let lane = if let Some(idx) = found_lane {
            idx
        } else if let Some(empty_idx) = active_lanes.iter().position(|s| s.is_none()) {
            empty_idx
        } else {
            active_lanes.push(None);
            active_lanes.len() - 1
        };

        entry.graph_lane = lane.min(5);
        entry.is_merge = entry.parents.len() > 1;

        if let Some(first_parent) = entry.parents.first() {
            active_lanes[lane] = Some(first_parent.clone());
        } else {
            active_lanes[lane] = None;
        }

        for extra_parent in entry.parents.iter().skip(1) {
            let already_tracked = active_lanes.iter().any(|slot| {
                slot.as_deref()
                    .is_some_and(|p| p == extra_parent || p.starts_with(extra_parent))
            });
            if !already_tracked {
                if let Some(empty_idx) = active_lanes.iter().position(|s| s.is_none()) {
                    active_lanes[empty_idx] = Some(extra_parent.clone());
                } else if active_lanes.len() < 6 {
                    active_lanes.push(Some(extra_parent.clone()));
                }
            }
        }
    }
}

fn collect_snapshot(root: &Path) -> GitSnapshot {
    let git_binary_path = detect_git_executable();
    let Some(ref git_bin) = git_binary_path else {
        return GitSnapshot {
            git_installed: false,
            is_git_repo: false,
            git_binary_path: None,
            git_version: None,
            current_branch: "main".to_string(),
            local_branches: vec!["main".to_string()],
            remote_branches: Vec::new(),
            tags: Vec::new(),
            ahead_behind: (0, 0),
            stashes: Vec::new(),
            remotes: Vec::new(),
            in_rebase: false,
            in_merge: false,
            changed_files: Vec::new(),
            working_diffs: HashMap::new(),
            recent_commits: Vec::new(),
            head_detail: None,
        };
    };
    let git_version = git_version_string(git_bin);
    let is_git_repo = git_command(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .eq_ignore_ascii_case("true")
        })
        .unwrap_or(false);
    if !is_git_repo {
        return GitSnapshot {
            git_installed: true,
            is_git_repo: false,
            git_binary_path,
            git_version,
            current_branch: "main".to_string(),
            local_branches: vec!["main".to_string()],
            remote_branches: Vec::new(),
            tags: Vec::new(),
            ahead_behind: (0, 0),
            stashes: Vec::new(),
            remotes: Vec::new(),
            in_rebase: false,
            in_merge: false,
            changed_files: Vec::new(),
            working_diffs: HashMap::new(),
            recent_commits: Vec::new(),
            head_detail: None,
        };
    }

    let (branch_res, status_res, log_res, diff_res) = thread::scope(|s| {
        let h_branch = s.spawn(|| {
            let mut current_branch = String::new();
            let mut local_branches = Vec::new();
            let mut remote_branches = Vec::new();
            let mut tags = Vec::new();
            let mut ahead_behind = (0usize, 0usize);

            if let Ok(output) = git_command(root)
                .args(["branch", "--format=%(HEAD)|%(refname:short)"])
                .output()
            {
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout);
                    for line in text.lines() {
                        if let Some((head, name)) = line.split_once('|') {
                            let name = name.trim().to_string();
                            if name.is_empty() {
                                continue;
                            }
                            if head.trim() == "*" {
                                current_branch = name.clone();
                            }
                            local_branches.push(name);
                        }
                    }
                }
            }

            if current_branch.is_empty() {
                if let Ok(output) = git_command(root).args(["branch", "--show-current"]).output() {
                    if output.status.success() {
                        let b = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        if !b.is_empty() {
                            current_branch = b;
                        }
                    }
                }
            }
            if current_branch.is_empty() {
                current_branch = "main".to_string();
            }
            if local_branches.is_empty() {
                local_branches.push(current_branch.clone());
            } else if !local_branches.contains(&current_branch) {
                local_branches.insert(0, current_branch.clone());
            }

            if let Ok(output) = git_command(root)
                .args(["branch", "-r", "--format=%(refname:short)"])
                .output()
            {
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout);
                    for line in text.lines() {
                        let r = line.trim();
                        if !r.is_empty() && !r.ends_with("/HEAD") && r != "origin" {
                            remote_branches.push(r.to_string());
                        }
                    }
                }
            }

            if let Ok(output) = git_command(root)
                .args(["tag", "--sort=-creatordate"])
                .output()
            {
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout);
                    for line in text.lines().take(20) {
                        let tg = line.trim();
                        if !tg.is_empty() {
                            tags.push(tg.to_string());
                        }
                    }
                }
            }

            if let Ok(output) = git_command(root)
                .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
                .output()
            {
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout);
                    let mut nums = text.split_whitespace();
                    if let (Some(a), Some(b)) = (nums.next(), nums.next()) {
                        ahead_behind = (
                            a.parse::<usize>().unwrap_or(0),
                            b.parse::<usize>().unwrap_or(0),
                        );
                    }
                }
            }

            (
                current_branch,
                local_branches,
                remote_branches,
                tags,
                ahead_behind,
            )
        });

        let h_status = s.spawn(|| {
            let mut changed_files = Vec::new();
            if let Ok(output) = git_command(root)
                .args(["-c", "core.quotePath=false", "status", "--porcelain"])
                .output()
            {
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout);
                    for line in text.lines() {
                        if line.len() < 4 {
                            continue;
                        }
                        let xy = &line[0..2];
                        let rel = line[3..].trim().trim_matches('"');
                        let status = match xy {
                            "??" => GitFileStatusKind::Untracked,
                            s if s.contains('A') => GitFileStatusKind::Added,
                            s if s.contains('D') => GitFileStatusKind::Deleted,
                            _ => GitFileStatusKind::Modified,
                        };
                        let staged = !xy.starts_with(' ') && !xy.starts_with('?');
                        changed_files.push(GitChangedFile {
                            path: root.join(rel),
                            status,
                            staged,
                        });
                    }
                }
            }
            changed_files
        });

        let h_log = s.spawn(|| {
            let mut recent_commits = if let Ok(output) = git_command(root)
                .args([
                    "log",
                    "-n",
                    "80",
                    "--all",
                    "--date=short",
                    "--pretty=format:%h|%H|%P|%an|%ae|%ad|%D|%s",
                ])
                .output()
            {
                if output.status.success() {
                    parse_git_log_lines(&String::from_utf8_lossy(&output.stdout))
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            assign_commit_graph_lanes(&mut recent_commits);
            let head_detail = recent_commits
                .first()
                .and_then(|first| collect_commit_detail(root, &first.hash));
            (recent_commits, head_detail)
        });

        let h_diff = s.spawn(|| {
            let mut working_diffs: HashMap<PathBuf, Vec<DiffLine>> = HashMap::new();
            if let Ok(output) = git_command(root)
                .args(["diff", "HEAD", "--unified=3"])
                .output()
            {
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout);
                    let (_, by_rel) = parse_unified_diff_sections(&text);
                    for (rel, lines) in by_rel {
                        working_diffs.insert(root.join(rel), lines);
                    }
                }
            }
            working_diffs
        });

        (
            h_branch.join().unwrap_or_else(|_| {
                (
                    "main".to_string(),
                    vec!["main".to_string()],
                    Vec::new(),
                    Vec::new(),
                    (0, 0),
                )
            }),
            h_status.join().unwrap_or_default(),
            h_log.join().unwrap_or_else(|_| (Vec::new(), None)),
            h_diff.join().unwrap_or_default(),
        )
    });

    let mut working_diffs = diff_res;
    for ch in &status_res {
        if ch.status == GitFileStatusKind::Untracked && !working_diffs.contains_key(&ch.path) {
            let file_ok = std::fs::metadata(&ch.path)
                .map(|m| m.is_file() && m.len() <= MAX_UNTRACKED_DIFF_FILE_BYTES)
                .unwrap_or(false);
            if !file_ok {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&ch.path) {
                let added_lines: Vec<DiffLine> = content
                    .lines()
                    .take(150)
                    .enumerate()
                    .map(|(idx, line)| DiffLine {
                        kind: DiffLineKind::Added,
                        old_lineno: None,
                        new_lineno: Some(idx + 1),
                        content: clip_diff_line(line),
                    })
                    .collect();
                working_diffs.insert(ch.path.clone(), added_lines);
            }
        }
    }

    let (stashes, remotes, in_rebase, in_merge) = collect_meta_extras(root);

    GitSnapshot {
        git_installed: true,
        is_git_repo: true,
        git_binary_path,
        git_version,
        current_branch: branch_res.0,
        local_branches: branch_res.1,
        remote_branches: branch_res.2,
        tags: branch_res.3,
        ahead_behind: branch_res.4,
        stashes,
        remotes,
        in_rebase,
        in_merge,
        changed_files: status_res,
        working_diffs,
        recent_commits: log_res.0,
        head_detail: log_res.1,
    }
}

fn parse_git_log_lines(text: &str) -> Vec<GitLogEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(8, '|');
        if let (
            Some(h),
            Some(fh),
            Some(parents_str),
            Some(a),
            Some(ae),
            Some(d),
            Some(refs_str),
            Some(subj),
        ) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) {
            let parents: Vec<String> = parents_str
                .split_whitespace()
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect();
            let refs: Vec<String> = refs_str
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect();
            entries.push(GitLogEntry {
                hash: h.to_string(),
                full_hash: fh.to_string(),
                parents,
                author: a.to_string(),
                author_email: ae.to_string(),
                date: d.to_string(),
                refs,
                subject: subj.to_string(),
                graph_lane: 0,
                is_merge: false,
            });
        }
    }
    entries
}

fn collect_meta_extras(root: &Path) -> (Vec<GitStashEntry>, Vec<GitRemoteEntry>, bool, bool) {
    let mut stashes = Vec::new();
    if let Ok(out) = git_command(root)
        .args(["stash", "list", "--pretty=format:%gd|%s"])
        .output()
    {
        if out.status.success() {
            let txt = String::from_utf8_lossy(&out.stdout);
            for line in txt.lines().take(30) {
                if let Some((idx_ref, msg)) = line.split_once('|') {
                    stashes.push(GitStashEntry {
                        index_ref: idx_ref.trim().to_string(),
                        message: msg.trim().to_string(),
                    });
                }
            }
        }
    }

    let mut remotes = Vec::new();
    if let Ok(out) = git_command(root).args(["remote", "-v"]).output() {
        if out.status.success() {
            let txt = String::from_utf8_lossy(&out.stdout);
            for line in txt.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let name = parts[0].to_string();
                    let url = parts[1].to_string();
                    if !remotes.iter().any(|r: &GitRemoteEntry| r.name == name) {
                        remotes.push(GitRemoteEntry { name, url });
                    }
                }
            }
        }
    }

    let git_dir = root.join(".git");
    let in_rebase = git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists();
    let in_merge = git_dir.join("MERGE_HEAD").exists();
    (stashes, remotes, in_rebase, in_merge)
}

fn collect_file_blame(root: &Path, file: &Path) -> Vec<GitBlameLine> {
    let rel = file
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| file.to_string_lossy().replace('\\', "/"));
    let Ok(out) = git_command(root)
        .args(["blame", "--line-porcelain", "--", &rel])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines_out = Vec::new();
    let mut commit_short = String::new();
    let mut author = String::new();
    let mut author_time: i64 = 0;
    let mut summary = String::new();
    let mut meta_cache: HashMap<String, (String, i64, String)> = HashMap::new();

    for raw in text.lines() {
        if raw.starts_with('\t') {
            if !commit_short.is_empty() {
                meta_cache
                    .entry(commit_short.clone())
                    .or_insert_with(|| (author.clone(), author_time, summary.clone()));
            }
            let date = format_unix_ymd(author_time);
            lines_out.push(GitBlameLine {
                commit_short: commit_short.clone(),
                author: author.clone(),
                date,
                summary: summary.clone(),
            });
            continue;
        }
        if let Some(rest) = raw.strip_prefix("author ") {
            author = rest.trim().to_string();
        } else if let Some(rest) = raw.strip_prefix("author-time ") {
            author_time = rest.trim().parse::<i64>().unwrap_or(0);
        } else if let Some(rest) = raw.strip_prefix("summary ") {
            summary = rest.trim().to_string();
        } else if raw.len() >= 40 && raw[..40].chars().all(|c| c.is_ascii_hexdigit()) {
            commit_short = raw[..7].to_string();
            if let Some((a, ts, s)) = meta_cache.get(&commit_short) {
                author = a.clone();
                author_time = *ts;
                summary = s.clone();
            }
        }
    }
    lines_out
}

fn format_unix_ymd(secs: i64) -> String {
    if secs <= 0 {
        return "0000-00-00".to_string();
    }
    let days = secs.div_euclid(86_400);
    // Howard Hinnant civil_from_days 算法
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let yr = y + if m <= 2 { 1 } else { 0 };
    format!("{yr:04}-{m:02}-{d:02}")
}

fn ensure_git_identity(root: &Path) {
    let has_name = git_command(root)
        .args(["config", "user.name"])
        .output()
        .map(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false);
    if !has_name {
        let _ = git_command(root)
            .args(["config", "user.name", "Rust AI IDE Developer"])
            .output();
    }
    let has_email = git_command(root)
        .args(["config", "user.email"])
        .output()
        .map(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false);
    if !has_email {
        let _ = git_command(root)
            .args(["config", "user.email", "dev@rust-ai-ide.local"])
            .output();
    }
}

fn rollback_single_file(root: &Path, file: &Path) -> String {
    let _ = git_command(root)
        .args(["reset", "HEAD", "--"])
        .arg(file)
        .output();
    let out = git_command(root)
        .args(["checkout", "--"])
        .arg(file)
        .output();
    match out {
        Ok(o) if o.status.success() => format!("Rolled back {}", file.display()),
        _ => {
            if file.is_file() {
                let _ = std::fs::remove_file(file);
                format!("Removed untracked file {}", file.display())
            } else {
                format!("Rolled back {}", file.display())
            }
        }
    }
}

fn spawn_git_worker(
    root: PathBuf,
    wakeup: Option<Arc<dyn Fn() + Send + Sync>>,
) -> (
    mpsc::Sender<GitWorkerCmd>,
    mpsc::Receiver<GitWorkerEvent>,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<GitWorkerCmd>();
    let (event_tx, event_rx) = mpsc::channel::<GitWorkerEvent>();
    let pending_refresh = Arc::new(AtomicBool::new(false));
    let is_busy = Arc::new(AtomicBool::new(false));

    let pending_ref = Arc::clone(&pending_refresh);
    let busy_ref = Arc::clone(&is_busy);

    let _ = thread::Builder::new()
        .name("ide-git-worker".to_string())
        .spawn(move || {
            let notify = |tx: &mpsc::Sender<GitWorkerEvent>, ev: GitWorkerEvent| {
                let _ = tx.send(ev);
                if let Some(cb) = &wakeup {
                    cb();
                }
            };

            while let Ok(cmd) = cmd_rx.recv() {
                busy_ref.store(true, Ordering::Release);
                match cmd {
                    GitWorkerCmd::Refresh => {
                        pending_ref.store(false, Ordering::Release);
                        let snapshot = collect_snapshot(&root);
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: None,
                                output: None,
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::InitRepo { create_gitignore } => {
                        let out = git_command(&root).arg("init").output();
                        let (ok, mut msg) = match out {
                            Ok(o) if o.status.success() => {
                                let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
                                (
                                    true,
                                    if stdout.is_empty() {
                                        format!("Initialized Git repository in {}", root.display())
                                    } else {
                                        stdout
                                    },
                                )
                            }
                            Ok(o) => {
                                let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                                (
                                    false,
                                    if err.is_empty() {
                                        "Git init failed".to_string()
                                    } else {
                                        err
                                    },
                                )
                            }
                            Err(e) => (false, format!("Git init failed: {e}")),
                        };
                        if ok && create_gitignore {
                            let gi_path = root.join(".gitignore");
                            if !gi_path.exists() {
                                if std::fs::write(&gi_path, build_default_gitignore(&root)).is_ok()
                                {
                                    msg.push_str(" (+ .gitignore)");
                                }
                            }
                        }
                        let snapshot = collect_snapshot(&root);
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::StageAll => {
                        let _ = git_command(&root).args(["add", "-A"]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = format!(
                            "Staged all changes ({} files).",
                            snapshot.changed_files.iter().filter(|f| f.staged).count()
                        );
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::UnstageAll => {
                        let _ = git_command(&root).args(["reset", "HEAD"]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = "Unstaged all changes.".to_string();
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::ToggleStageFile {
                        file,
                        currently_staged,
                    } => {
                        if currently_staged {
                            let _ = git_command(&root)
                                .args(["reset", "HEAD", "--"])
                                .arg(&file)
                                .output();
                        } else {
                            let _ = git_command(&root)
                                .args(["add", "--"])
                                .arg(&file)
                                .output();
                        }
                        let snapshot = collect_snapshot(&root);
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: None,
                                output: None,
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::RollbackFile(file) => {
                        let msg = rollback_single_file(&root, &file);
                        let snapshot = collect_snapshot(&root);
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::RollbackFiles(files) => {
                        let count = files.len();
                        for file in &files {
                            let _ = rollback_single_file(&root, file);
                        }
                        let snapshot = collect_snapshot(&root);
                        let msg = format!("Rolled back {count} selected file(s).");
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::RollbackAll => {
                        let _ = git_command(&root).args(["reset", "--hard", "HEAD"]).output();
                        let _ = git_command(&root).args(["clean", "-fd"]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = "Rolled back all uncommitted changes.".to_string();
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::Commit {
                        message,
                        amend,
                        push_after,
                        files,
                    } => {
                        let trimmed_msg = message.trim();
                        if trimmed_msg.is_empty() {
                            let snapshot = collect_snapshot(&root);
                            let msg = "Commit skipped: please enter a commit message.".to_string();
                            notify(
                                &event_tx,
                                GitWorkerEvent::SnapshotUpdated {
                                    snapshot,
                                    notification: Some(msg.clone()),
                                    output: Some(msg),
                                    clear_commit_input: false,
                                },
                            );
                            busy_ref.store(false, Ordering::Release);
                            continue;
                        }
                        if files.is_empty() && !amend {
                            let snapshot = collect_snapshot(&root);
                            let msg = "Commit skipped: no files checked to commit.".to_string();
                            notify(
                                &event_tx,
                                GitWorkerEvent::SnapshotUpdated {
                                    snapshot,
                                    notification: Some(msg.clone()),
                                    output: Some(msg),
                                    clear_commit_input: false,
                                },
                            );
                            busy_ref.store(false, Ordering::Release);
                            continue;
                        }

                        ensure_git_identity(&root);
                        let has_head = git_command(&root)
                            .args(["rev-parse", "--verify", "HEAD"])
                            .output()
                            .map(|o| o.status.success())
                            .unwrap_or(false);

                        let rel_files: Vec<PathBuf> = files
                            .iter()
                            .map(|p| p.strip_prefix(&root).unwrap_or(p).to_path_buf())
                            .collect();

                        let mut intent_added: Vec<PathBuf> = Vec::new();
                        if !rel_files.is_empty() {
                            if has_head {
                                // Only track untracked files with intent-to-add right before committing them,
                                // so `git commit -- <paths>` commits only the checked files and preserves index for others.
                                let mut untracked_selected: Vec<PathBuf> = Vec::new();
                                if let Ok(st_out) =
                                    git_command(&root).args(["status", "--porcelain"]).output()
                                {
                                    if st_out.status.success() {
                                        let untracked_set: HashSet<PathBuf> =
                                            String::from_utf8_lossy(&st_out.stdout)
                                                .lines()
                                                .filter(|l| l.len() >= 4 && &l[0..2] == "??")
                                                .map(|l| {
                                                    PathBuf::from(l[3..].trim().trim_matches('"'))
                                                })
                                                .collect();
                                        for rf in &rel_files {
                                            if untracked_set.contains(rf) {
                                                untracked_selected.push(rf.clone());
                                            }
                                        }
                                    }
                                }
                                if !untracked_selected.is_empty() {
                                    let mut add_n = git_command(&root);
                                    add_n.args(["add", "-N", "--"]);
                                    for uf in &untracked_selected {
                                        add_n.arg(uf);
                                    }
                                    if add_n.output().is_ok_and(|o| o.status.success()) {
                                        intent_added = untracked_selected;
                                    }
                                }
                            } else {
                                let mut add_cmd = git_command(&root);
                                add_cmd.args(["add", "--"]);
                                for rf in &rel_files {
                                    add_cmd.arg(rf);
                                }
                                let _ = add_cmd.output();
                            }
                        }

                        let mut commit_cmd = git_command(&root);
                        commit_cmd.arg("commit");
                        if amend {
                            commit_cmd.arg("--amend");
                        }
                        commit_cmd.args(["-m", trimmed_msg]);
                        if has_head && !rel_files.is_empty() {
                            commit_cmd.arg("--");
                            for rf in &rel_files {
                                commit_cmd.arg(rf);
                            }
                        }
                        let out = commit_cmd.output();

                        let (ok, mut summary) = match out {
                            Ok(o) if o.status.success() => (
                                true,
                                String::from_utf8_lossy(&o.stdout)
                                    .lines()
                                    .next()
                                    .unwrap_or("Committed successfully")
                                    .to_string(),
                            ),
                            Ok(o) => {
                                if !intent_added.is_empty() {
                                    let mut reset_cmd = git_command(&root);
                                    reset_cmd.args(["reset", "-q", "HEAD", "--"]);
                                    for uf in &intent_added {
                                        reset_cmd.arg(uf);
                                    }
                                    let _ = reset_cmd.output();
                                }
                                (
                                    false,
                                    String::from_utf8_lossy(&o.stderr)
                                        .lines()
                                        .next()
                                        .unwrap_or("Commit skipped (nothing to commit)")
                                        .to_string(),
                                )
                            }
                            Err(e) => {
                                if !intent_added.is_empty() {
                                    let mut reset_cmd = git_command(&root);
                                    reset_cmd.args(["reset", "-q", "HEAD", "--"]);
                                    for uf in &intent_added {
                                        reset_cmd.arg(uf);
                                    }
                                    let _ = reset_cmd.output();
                                }
                                (false, format!("Commit failed: {e}"))
                            }
                        };

                        if ok && push_after {
                            let push_out = git_command(&root).args(["push"]).output();
                            match push_out {
                                Ok(po) if po.status.success() => {
                                    summary = format!("{summary} · Pushed to remote");
                                }
                                _ => {
                                    summary = format!(
                                        "{summary} · Committed locally (no remote configured for push)"
                                    );
                                }
                            }
                        }

                        let snapshot = collect_snapshot(&root);
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(format!("Git Commit: {summary}")),
                                output: Some(summary),
                                clear_commit_input: ok,
                            },
                        );
                    }
                    GitWorkerCmd::CheckoutBranch(branch) => {
                        let out = git_command(&root).args(["checkout", &branch]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                format!("Checked out '{}'", snapshot.current_branch)
                            }
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Checkout failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::CreateAndCheckoutBranch { name, start_point } => {
                        let mut c = git_command(&root);
                        c.args(["checkout", "-b", name.trim()]);
                        if let Some(sp) = start_point.as_deref() {
                            if !sp.trim().is_empty() {
                                c.arg(sp.trim());
                            }
                        }
                        let out = c.output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                format!(
                                    "Created and switched to branch '{}'",
                                    snapshot.current_branch
                                )
                            }
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Create branch failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::DeleteBranch(branch) => {
                        let out = git_command(&root).args(["branch", "-D", &branch]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => format!("Deleted branch '{branch}'"),
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Delete branch failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::CreateTag { name, start_point } => {
                        let mut c = git_command(&root);
                        c.args(["tag", name.trim()]);
                        if let Some(sp) = start_point.as_deref() {
                            if !sp.trim().is_empty() {
                                c.arg(sp.trim());
                            }
                        }
                        let out = c.output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => format!("Created tag '{}'", name.trim()),
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Create tag failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::DeleteTag(name) => {
                        let out = git_command(&root).args(["tag", "-d", name.trim()]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => format!("Deleted tag '{}'", name.trim()),
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Delete tag failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::MergeBranch(branch) => {
                        ensure_git_identity(&root);
                        let out = git_command(&root).args(["merge", &branch]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                format!("Merged '{branch}' into '{}'", snapshot.current_branch)
                            }
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Merge failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::MergeAbort => {
                        let out = git_command(&root).args(["merge", "--abort"]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => "Aborted current merge.".to_string(),
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Merge abort failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::RebaseBranch(branch) => {
                        ensure_git_identity(&root);
                        let out = git_command(&root).args(["rebase", branch.trim()]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                format!("Rebased '{}' onto '{}'", snapshot.current_branch, branch.trim())
                            }
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Rebase failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::RebaseAbort => {
                        let out = git_command(&root).args(["rebase", "--abort"]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => "Aborted active rebase.".to_string(),
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Rebase abort failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::RebaseContinue => {
                        ensure_git_identity(&root);
                        let out = git_command(&root)
                            .env("GIT_EDITOR", "true")
                            .args(["rebase", "--continue"])
                            .output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => "Continued active rebase.".to_string(),
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Rebase continue failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::StashSave {
                        message,
                        include_untracked,
                    } => {
                        let mut c = git_command(&root);
                        c.args(["stash", "push"]);
                        if include_untracked {
                            c.arg("-u");
                        }
                        if !message.trim().is_empty() {
                            c.args(["-m", message.trim()]);
                        }
                        let out = c.output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                String::from_utf8_lossy(&o.stdout).trim().to_string()
                            }
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Stash failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::StashApply(sref) => {
                        let out = git_command(&root).args(["stash", "apply", &sref]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => format!("Applied stash {sref}"),
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Apply stash failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::StashPop(sref) => {
                        let out = git_command(&root).args(["stash", "pop", &sref]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => format!("Popped stash {sref}"),
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Pop stash failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::StashDrop(sref) => {
                        let out = git_command(&root).args(["stash", "drop", &sref]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => format!("Dropped stash {sref}"),
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Drop stash failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::AddRemote { name, url } => {
                        let out = git_command(&root)
                            .args(["remote", "add", name.trim(), url.trim()])
                            .output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                format!("Added remote '{}' -> {}", name.trim(), url.trim())
                            }
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Add remote failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::RemoveRemote(name) => {
                        let out = git_command(&root)
                            .args(["remote", "remove", name.trim()])
                            .output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                format!("Removed remote '{}'", name.trim())
                            }
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Remove remote failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::CherryPick(hash) => {
                        ensure_git_identity(&root);
                        let out = git_command(&root).args(["cherry-pick", &hash]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                format!("Cherry-picked commit {hash}")
                            }
                            Ok(o) => String::from_utf8_lossy(&o.stderr)
                                .lines()
                                .next()
                                .unwrap_or("Cherry-pick failed")
                                .to_string(),
                            Err(e) => format!("Cherry-pick failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::RevertCommit(hash) => {
                        ensure_git_identity(&root);
                        let out = git_command(&root)
                            .args(["revert", "--no-edit", &hash])
                            .output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => format!("Reverted commit {hash}"),
                            Ok(o) => String::from_utf8_lossy(&o.stderr)
                                .lines()
                                .next()
                                .unwrap_or("Revert failed")
                                .to_string(),
                            Err(e) => format!("Revert failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::Reset { target, mode } => {
                        let out = git_command(&root)
                            .args(["reset", mode.as_flag(), target.trim()])
                            .output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                format!("Reset HEAD ({}) to {}", mode.as_flag(), target.trim())
                            }
                            Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                            Err(e) => format!("Reset failed: {e}"),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::Fetch => {
                        let out = git_command(&root)
                            .args(["fetch", "--all", "--prune"])
                            .output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                "Git Fetch: Fetched all remotes.".to_string()
                            }
                            _ => "Git Fetch: Local repository (no remote configured).".to_string(),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::Pull => {
                        let out = git_command(&root).args(["pull"]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                String::from_utf8_lossy(&o.stdout).trim().to_string()
                            }
                            _ => "Git Pull: No remote configured yet for this local repository."
                                .to_string(),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::Push => {
                        let out = git_command(&root).args(["push"]).output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                String::from_utf8_lossy(&o.stdout).trim().to_string()
                            }
                            _ => "Git Push: Local commits ready (configure origin remote to push)."
                                .to_string(),
                        };
                        notify(
                            &event_tx,
                            GitWorkerEvent::SnapshotUpdated {
                                snapshot,
                                notification: Some(msg.clone()),
                                output: Some(msg),
                                clear_commit_input: false,
                            },
                        );
                    }
                    GitWorkerCmd::LoadCommitDetail(hash) => {
                        if let Some(detail) = collect_commit_detail(&root, &hash) {
                            notify(&event_tx, GitWorkerEvent::CommitDetailLoaded { detail });
                        }
                    }
                    GitWorkerCmd::LoadFileHistory(path) => {
                        let rel = path
                            .strip_prefix(&root)
                            .map(|p| p.to_string_lossy().replace('\\', "/"))
                            .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"));
                        let mut entries = if let Ok(output) = git_command(&root)
                            .args([
                                "log",
                                "-n",
                                "60",
                                "--follow",
                                "--date=short",
                                "--pretty=format:%h|%H|%P|%an|%ae|%ad|%D|%s",
                                "--",
                                &rel,
                            ])
                            .output()
                        {
                            if output.status.success() {
                                parse_git_log_lines(&String::from_utf8_lossy(&output.stdout))
                            } else {
                                Vec::new()
                            }
                        } else {
                            Vec::new()
                        };
                        assign_commit_graph_lanes(&mut entries);
                        notify(
                            &event_tx,
                            GitWorkerEvent::FileHistoryLoaded { path, entries },
                        );
                    }
                    GitWorkerCmd::LoadFileBlame(path) => {
                        let lines = collect_file_blame(&root, &path);
                        notify(&event_tx, GitWorkerEvent::FileBlameLoaded { path, lines });
                    }
                    GitWorkerCmd::AiCommitDiff(selected_files) => {
                        let (snapshot, mut diff_text, mut recent_commit_messages, (commit_template, commitlint_config)) =
                            thread::scope(|s| {
                                let h_snap = s.spawn(|| collect_snapshot(&root));
                                let h_diff = s.spawn(|| collect_ai_commit_diff(&root, &selected_files));
                                let h_hist = s.spawn(|| collect_recent_commit_messages(&root, 20));
                                let h_conv = s.spawn(|| collect_commit_conventions(&root));
                                (
                                    h_snap.join().unwrap_or_else(|_| empty_git_snapshot()),
                                    h_diff.join().unwrap_or_default(),
                                    h_hist.join().unwrap_or_default(),
                                    h_conv.join().unwrap_or((None, None)),
                                )
                            });

                        if recent_commit_messages.is_empty() {
                            recent_commit_messages = snapshot
                                .recent_commits
                                .iter()
                                .filter(|c| !c.is_merge)
                                .take(15)
                                .map(|c| c.subject.clone())
                                .filter(|s| !s.trim().is_empty())
                                .collect();
                        }

                        let selected_set: HashSet<&PathBuf> = selected_files.iter().collect();
                        let target_changes: Vec<&GitChangedFile> = if selected_set.is_empty() {
                            snapshot.changed_files.iter().collect()
                        } else {
                            let filtered: Vec<&GitChangedFile> = snapshot
                                .changed_files
                                .iter()
                                .filter(|c| selected_set.contains(&c.path))
                                .collect();
                            if filtered.is_empty() {
                                snapshot.changed_files.iter().collect()
                            } else {
                                filtered
                            }
                        };

                        let changed_files: Vec<String> = target_changes
                            .iter()
                            .map(|c| {
                                let rel = c
                                    .path
                                    .strip_prefix(&root)
                                    .unwrap_or(&c.path)
                                    .to_string_lossy()
                                    .replace('\\', "/");
                                format!("{} {}", c.status.badge_str(), rel)
                            })
                            .collect();

                        append_untracked_diff_previews(
                            &root,
                            &target_changes,
                            &snapshot.working_diffs,
                            &mut diff_text,
                        );

                        let style_summary = summarize_commit_history(
                            &recent_commit_messages,
                            commit_template.as_deref(),
                            commitlint_config.as_deref(),
                        );

                        notify(
                            &event_tx,
                            GitWorkerEvent::AiDiffReady {
                                snapshot,
                                context: AiCommitContext {
                                    diff_text,
                                    changed_files,
                                    recent_commit_messages,
                                    style_summary,
                                    commit_template,
                                    commitlint_config,
                                },
                            },
                        );
                    }
                }
                busy_ref.store(false, Ordering::Release);
            }
        });

    (cmd_tx, event_rx, pending_refresh, is_busy)
}

impl GitEngine {
    pub fn open(root: &Path) -> Self {
        let mut engine = Self::open_with_wakeup_inner(root, None, false);
        engine.refresh();
        engine
    }

    /// 异步无阻塞初始化：立即返回默认状态并在后台拉取首个 Git 快照，完成后通过 `wakeup` 唤醒 UI
    pub fn open_with_wakeup(root: &Path, wakeup: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        Self::open_with_wakeup_inner(root, wakeup, true)
    }

    fn open_with_wakeup_inner(
        root: &Path,
        wakeup: Option<Arc<dyn Fn() + Send + Sync>>,
        auto_refresh: bool,
    ) -> Self {
        let root_buf = root.to_path_buf();
        let (cmd_tx, event_rx, pending_refresh, is_busy) =
            spawn_git_worker(root_buf.clone(), wakeup);
        let is_git_repo = root_buf.join(".git").exists();
        let engine = Self {
            root: root_buf,
            git_installed: detect_git_executable().is_some(),
            is_git_repo,
            git_binary_path: detect_git_executable(),
            git_version: None,
            current_branch: "main".to_string(),
            local_branches: vec!["main".to_string()],
            remote_branches: Vec::new(),
            tags: Vec::new(),
            ahead_behind: (0, 0),
            stashes: Vec::new(),
            remotes: Vec::new(),
            in_rebase: false,
            in_merge: false,
            changed_files: Vec::new(),
            commit_checked_files: HashSet::new(),
            known_changed_files: HashSet::new(),
            working_diffs: HashMap::new(),
            recent_commits: Vec::new(),
            commit_details: HashMap::new(),
            file_histories: HashMap::new(),
            file_blames: HashMap::new(),
            commit_message_draft: String::new(),
            last_operation_output: String::new(),
            has_loaded_snapshot: false,
            cmd_tx,
            event_rx,
            pending_refresh,
            is_busy,
        };
        if auto_refresh {
            engine.request_refresh();
        }
        engine
    }

    fn apply_snapshot(&mut self, snapshot: GitSnapshot) {
        self.git_installed = snapshot.git_installed;
        self.is_git_repo = snapshot.is_git_repo;
        self.git_binary_path = snapshot.git_binary_path;
        self.git_version = snapshot.git_version;
        self.current_branch = snapshot.current_branch;
        self.local_branches = snapshot.local_branches;
        self.remote_branches = snapshot.remote_branches;
        self.tags = snapshot.tags;
        self.ahead_behind = snapshot.ahead_behind;
        self.stashes = snapshot.stashes;
        self.remotes = snapshot.remotes;
        self.in_rebase = snapshot.in_rebase;
        self.in_merge = snapshot.in_merge;
        self.file_blames.clear();
        self.file_histories.clear();

        let current_paths: HashSet<PathBuf> = snapshot
            .changed_files
            .iter()
            .map(|f| f.path.clone())
            .collect();
        self.commit_checked_files
            .retain(|p| current_paths.contains(p));
        self.known_changed_files
            .retain(|p| current_paths.contains(p));
        for ch in &snapshot.changed_files {
            if self.known_changed_files.insert(ch.path.clone())
                && ch.status != GitFileStatusKind::Untracked
            {
                self.commit_checked_files.insert(ch.path.clone());
            }
        }

        self.changed_files = snapshot.changed_files;
        self.working_diffs = snapshot.working_diffs;
        self.recent_commits = snapshot.recent_commits;
        self.has_loaded_snapshot = true;
        if let Some(detail) = snapshot.head_detail {
            self.insert_commit_detail_bounded(detail);
        }
    }

    fn insert_commit_detail_bounded(&mut self, detail: GitCommitDetail) {
        if self.commit_details.len() >= MAX_COMMIT_DETAILS_CACHE
            && !self.commit_details.contains_key(&detail.hash)
        {
            let head_hash = self.recent_commits.first().map(|c| c.hash.as_str());
            if let Some(evict) = self
                .commit_details
                .keys()
                .find(|k| Some(k.as_str()) != head_hash)
                .cloned()
                .or_else(|| self.commit_details.keys().next().cloned())
            {
                self.commit_details.remove(&evict);
            }
        }
        self.commit_details.insert(detail.hash.clone(), detail);
    }

    /// 释放后台/最小化窗口的非核心 Git 缓存，保留工作区变更状态与 HEAD 摘要
    pub fn trim_caches(&mut self) {
        let head_hash = self.recent_commits.first().map(|c| c.hash.clone());
        self.commit_details
            .retain(|k, _| head_hash.as_deref() == Some(k.as_str()));
        self.commit_details.shrink_to_fit();
        self.file_histories.clear();
        self.file_histories.shrink_to_fit();
        self.file_blames.clear();
        self.file_blames.shrink_to_fit();
    }

    pub fn is_file_checked_for_commit(&self, path: &Path) -> bool {
        self.commit_checked_files.contains(path)
    }

    /// IDEA 风格纯勾选/取消勾选（不触发 git add / git reset 暂存）
    pub fn toggle_commit_file_checked(&mut self, path: &Path) -> bool {
        if self.commit_checked_files.contains(path) {
            self.commit_checked_files.remove(path);
            false
        } else {
            self.commit_checked_files.insert(path.to_path_buf());
            true
        }
    }

    /// 批量勾选或取消勾选分组（`untracked_group == false` 为已跟踪变更，`true` 为未跟踪文件）
    pub fn set_group_checked(&mut self, untracked_group: bool, checked: bool) {
        for f in &self.changed_files {
            let is_untracked = f.status == GitFileStatusKind::Untracked;
            if is_untracked == untracked_group {
                if checked {
                    self.commit_checked_files.insert(f.path.clone());
                } else {
                    self.commit_checked_files.remove(&f.path);
                }
            }
        }
    }

    pub fn checked_counts(&self, untracked_group: bool) -> (usize, usize) {
        let mut checked = 0usize;
        let mut total = 0usize;
        for f in &self.changed_files {
            let is_untracked = f.status == GitFileStatusKind::Untracked;
            if is_untracked == untracked_group {
                total += 1;
                if self.commit_checked_files.contains(&f.path) {
                    checked += 1;
                }
            }
        }
        (checked, total)
    }

    pub fn checked_commit_files(&self) -> Vec<PathBuf> {
        self.changed_files
            .iter()
            .filter(|f| self.commit_checked_files.contains(&f.path))
            .map(|f| f.path.clone())
            .collect()
    }

    pub fn total_checked_count(&self) -> usize {
        self.changed_files
            .iter()
            .filter(|f| self.commit_checked_files.contains(&f.path))
            .count()
    }

    pub fn is_busy(&self) -> bool {
        self.is_busy.load(Ordering::Acquire) || self.pending_refresh.load(Ordering::Acquire)
    }

    /// 非阻塞合并式刷新 Git 状态（多次连续触发自动去重合并）
    pub fn request_refresh(&self) {
        if !self.pending_refresh.swap(true, Ordering::AcqRel) {
            if self.cmd_tx.send(GitWorkerCmd::Refresh).is_err() {
                self.pending_refresh.store(false, Ordering::Release);
            }
        }
    }

    /// 在当前工作区执行 `git init`（可选同步生成适配项目类型的 `.gitignore`）
    pub fn request_init_repo(&self, create_gitignore: bool) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::InitRepo { create_gitignore });
    }

    pub fn request_stage_all(&mut self) {
        for f in &mut self.changed_files {
            f.staged = true;
        }
        let _ = self.cmd_tx.send(GitWorkerCmd::StageAll);
    }

    pub fn request_unstage_all(&mut self) {
        for f in &mut self.changed_files {
            f.staged = false;
        }
        let _ = self.cmd_tx.send(GitWorkerCmd::UnstageAll);
    }

    /// 乐观更新选中状态并在后台执行 `git add` / `git reset`
    pub fn request_toggle_stage_file(&mut self, file: &Path) {
        let mut currently_staged = false;
        if let Some(entry) = self.changed_files.iter_mut().find(|f| f.path == file) {
            currently_staged = entry.staged;
            entry.staged = !currently_staged;
        }
        let _ = self.cmd_tx.send(GitWorkerCmd::ToggleStageFile {
            file: file.to_path_buf(),
            currently_staged,
        });
    }

    pub fn request_rollback_file(&self, file: &Path) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::RollbackFile(file.to_path_buf()));
    }

    pub fn request_rollback_files(&self, files: Vec<PathBuf>) {
        if !files.is_empty() {
            let _ = self.cmd_tx.send(GitWorkerCmd::RollbackFiles(files));
        }
    }

    pub fn request_rollback_all(&self) {
        let _ = self.cmd_tx.send(GitWorkerCmd::RollbackAll);
    }

    pub fn request_commit_all(&mut self, message: &str) {
        self.request_commit_with_options(message, false, false);
    }

    pub fn request_commit_with_options(&mut self, message: &str, amend: bool, push_after: bool) {
        let files = self.checked_commit_files();
        self.request_commit_selected_files(message, amend, push_after, files);
    }

    pub fn request_commit_selected_files(
        &mut self,
        message: &str,
        amend: bool,
        push_after: bool,
        files: Vec<PathBuf>,
    ) {
        self.commit_message_draft = message.to_string();
        let _ = self.cmd_tx.send(GitWorkerCmd::Commit {
            message: message.to_string(),
            amend,
            push_after,
            files,
        });
    }

    pub fn request_checkout_branch(&self, branch: &str) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::CheckoutBranch(branch.to_string()));
    }

    pub fn request_create_and_checkout_branch(&self, new_branch: &str) {
        let _ = self.cmd_tx.send(GitWorkerCmd::CreateAndCheckoutBranch {
            name: new_branch.to_string(),
            start_point: None,
        });
    }

    pub fn request_create_branch_at(&self, new_branch: &str, start_point: &str) {
        let _ = self.cmd_tx.send(GitWorkerCmd::CreateAndCheckoutBranch {
            name: new_branch.to_string(),
            start_point: Some(start_point.to_string()),
        });
    }

    pub fn request_delete_branch(&self, branch: &str) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::DeleteBranch(branch.to_string()));
    }

    pub fn request_create_tag(&self, name: &str, start_point: Option<&str>) {
        if !name.trim().is_empty() {
            let _ = self.cmd_tx.send(GitWorkerCmd::CreateTag {
                name: name.to_string(),
                start_point: start_point.map(ToOwned::to_owned),
            });
        }
    }

    pub fn request_delete_tag(&self, name: &str) {
        if !name.trim().is_empty() {
            let _ = self
                .cmd_tx
                .send(GitWorkerCmd::DeleteTag(name.to_string()));
        }
    }

    pub fn request_merge_branch(&self, branch: &str) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::MergeBranch(branch.to_string()));
    }

    pub fn request_merge_abort(&self) {
        let _ = self.cmd_tx.send(GitWorkerCmd::MergeAbort);
    }

    pub fn request_rebase_branch(&self, branch: &str) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::RebaseBranch(branch.to_string()));
    }

    pub fn request_rebase_abort(&self) {
        let _ = self.cmd_tx.send(GitWorkerCmd::RebaseAbort);
    }

    pub fn request_rebase_continue(&self) {
        let _ = self.cmd_tx.send(GitWorkerCmd::RebaseContinue);
    }

    pub fn request_stash_save(&self, message: &str, include_untracked: bool) {
        let _ = self.cmd_tx.send(GitWorkerCmd::StashSave {
            message: message.to_string(),
            include_untracked,
        });
    }

    pub fn request_stash_apply(&self, stash_ref: &str) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::StashApply(stash_ref.to_string()));
    }

    pub fn request_stash_pop(&self, stash_ref: &str) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::StashPop(stash_ref.to_string()));
    }

    pub fn request_stash_drop(&self, stash_ref: &str) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::StashDrop(stash_ref.to_string()));
    }

    pub fn request_add_remote(&self, name: &str, url: &str) {
        if !name.trim().is_empty() && !url.trim().is_empty() {
            let _ = self.cmd_tx.send(GitWorkerCmd::AddRemote {
                name: name.to_string(),
                url: url.to_string(),
            });
        }
    }

    pub fn request_remove_remote(&self, name: &str) {
        if !name.trim().is_empty() {
            let _ = self
                .cmd_tx
                .send(GitWorkerCmd::RemoveRemote(name.to_string()));
        }
    }

    pub fn request_cherry_pick(&self, commit_hash: &str) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::CherryPick(commit_hash.to_string()));
    }

    pub fn request_revert_commit(&self, commit_hash: &str) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::RevertCommit(commit_hash.to_string()));
    }

    pub fn request_reset_soft(&self, commit_hash: &str) {
        self.request_reset(commit_hash, GitResetMode::Soft);
    }

    pub fn request_reset(&self, target: &str, mode: GitResetMode) {
        if !target.trim().is_empty() {
            let _ = self.cmd_tx.send(GitWorkerCmd::Reset {
                target: target.to_string(),
                mode,
            });
        }
    }

    pub fn request_load_file_history(&self, path: &Path) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::LoadFileHistory(path.to_path_buf()));
    }

    pub fn request_load_file_blame(&self, path: &Path) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::LoadFileBlame(path.to_path_buf()));
    }

    pub fn request_fetch(&self) {
        let _ = self.cmd_tx.send(GitWorkerCmd::Fetch);
    }

    pub fn request_pull(&self) {
        let _ = self.cmd_tx.send(GitWorkerCmd::Pull);
    }

    pub fn request_push(&self) {
        let _ = self.cmd_tx.send(GitWorkerCmd::Push);
    }

    pub fn request_load_commit_detail(&self, commit_hash: &str) {
        if !commit_hash.is_empty() && !self.commit_details.contains_key(commit_hash) {
            let _ = self
                .cmd_tx
                .send(GitWorkerCmd::LoadCommitDetail(commit_hash.to_string()));
        }
    }

    pub fn request_ai_commit_diff(&self) -> bool {
        self.cmd_tx
            .send(GitWorkerCmd::AiCommitDiff(self.checked_commit_files()))
            .is_ok()
    }

    pub fn poll_updates(&mut self) -> GitPollOutcome {
        let mut outcome = GitPollOutcome::default();
        while let Ok(ev) = self.event_rx.try_recv() {
            outcome.changed = true;
            match ev {
                GitWorkerEvent::SnapshotUpdated {
                    snapshot,
                    notification,
                    output,
                    clear_commit_input,
                } => {
                    self.apply_snapshot(snapshot);
                    if let Some(out) = output {
                        self.last_operation_output = out;
                    }
                    if let Some(msg) = notification {
                        outcome.notification = Some(msg);
                    }
                    if clear_commit_input {
                        self.commit_message_draft.clear();
                        outcome.clear_commit_input = true;
                    }
                }
                GitWorkerEvent::CommitDetailLoaded { detail } => {
                    self.insert_commit_detail_bounded(detail);
                }
                GitWorkerEvent::FileHistoryLoaded { path, entries } => {
                    if self.file_histories.len() >= MAX_FILE_HISTORY_CACHE
                        && !self.file_histories.contains_key(&path)
                    {
                        if let Some(drop_k) = self.file_histories.keys().next().cloned() {
                            self.file_histories.remove(&drop_k);
                        }
                    }
                    self.file_histories.insert(path, entries);
                }
                GitWorkerEvent::FileBlameLoaded { path, lines } => {
                    if self.file_blames.len() >= MAX_FILE_BLAME_CACHE
                        && !self.file_blames.contains_key(&path)
                    {
                        if let Some(drop_k) = self.file_blames.keys().next().cloned() {
                            self.file_blames.remove(&drop_k);
                        }
                    }
                    self.file_blames.insert(path, lines);
                }
                GitWorkerEvent::AiDiffReady { snapshot, context } => {
                    self.apply_snapshot(snapshot);
                    outcome.ai_diff_ready = Some(context);
                }
            }
        }
        outcome
    }

    pub fn refresh(&mut self) {
        let snapshot = collect_snapshot(&self.root);
        self.apply_snapshot(snapshot);
    }

    pub fn stage_all(&mut self) -> Result<String> {
        let out = git_command(&self.root).args(["add", "-A"]).output()?;
        self.refresh();
        let msg = format!(
            "Staged all changes ({} files).",
            self.changed_files.iter().filter(|f| f.staged).count()
        );
        self.last_operation_output = msg.clone();
        let _ = out;
        Ok(msg)
    }

    pub fn toggle_stage_file(&mut self, file: &Path) -> Result<()> {
        let is_staged = self
            .changed_files
            .iter()
            .find(|f| f.path == file)
            .map(|f| f.staged)
            .unwrap_or(false);
        if is_staged {
            let _ = git_command(&self.root)
                .args(["reset", "HEAD", "--"])
                .arg(file)
                .output()?;
        } else {
            let _ = git_command(&self.root)
                .args(["add", "--"])
                .arg(file)
                .output()?;
        }
        self.refresh();
        Ok(())
    }

    pub fn rollback_file(&mut self, file: &Path) -> Result<String> {
        let msg = rollback_single_file(&self.root, file);
        self.refresh();
        self.last_operation_output = msg.clone();
        Ok(msg)
    }

    pub fn commit_all(&mut self, message: &str) -> Result<String> {
        let _ = git_command(&self.root).args(["add", "-A"]).output()?;
        ensure_git_identity(&self.root);

        let out = git_command(&self.root)
            .args(["commit", "-m", message])
            .output()?;
        self.refresh();
        let summary = if out.status.success() {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("Committed successfully")
                .to_string()
        } else {
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("Commit skipped (nothing to commit)")
                .to_string()
        };
        self.last_operation_output = summary.clone();
        Ok(summary)
    }

    pub fn checkout_branch(&mut self, branch: &str) -> Result<String> {
        let out = git_command(&self.root)
            .args(["checkout", branch])
            .output()?;
        self.refresh();
        let msg = if out.status.success() {
            format!("Switched to branch '{}'", self.current_branch)
        } else {
            String::from_utf8_lossy(&out.stderr).trim().to_string()
        };
        self.last_operation_output = msg.clone();
        Ok(msg)
    }

    pub fn create_and_checkout_branch(&mut self, new_branch: &str) -> Result<String> {
        let out = git_command(&self.root)
            .args(["checkout", "-b", new_branch.trim()])
            .output()?;
        self.refresh();
        let msg = if out.status.success() {
            format!("Created and switched to branch '{}'", self.current_branch)
        } else {
            String::from_utf8_lossy(&out.stderr).trim().to_string()
        };
        self.last_operation_output = msg.clone();
        Ok(msg)
    }

    pub fn pull(&mut self) -> Result<String> {
        let out = git_command(&self.root).args(["pull"]).output()?;
        self.refresh();
        let msg = if out.status.success() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            "Git Pull: No remote configured yet for this local repository.".to_string()
        };
        self.last_operation_output = msg.clone();
        Ok(msg)
    }

    pub fn push(&mut self) -> Result<String> {
        let out = git_command(&self.root).args(["push"]).output()?;
        self.refresh();
        let msg = if out.status.success() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            "Git Push: Local commits ready (configure origin remote to push).".to_string()
        };
        self.last_operation_output = msg.clone();
        Ok(msg)
    }

    /// 根据仓库历史格式与当前变更文件列表生成规范的 Commit 消息（本地快速回退）
    pub fn generate_ai_commit_message(&mut self) -> String {
        let hist: Vec<String> = self
            .recent_commits
            .iter()
            .filter(|c| !c.is_merge)
            .take(20)
            .map(|c| c.subject.clone())
            .collect();
        let profile = analyze_commit_history(&hist);
        let count = self.changed_files.len();
        let sample_names: Vec<String> = self
            .changed_files
            .iter()
            .take(3)
            .filter_map(|f| f.path.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()))
            .collect();
        let inferred_scope = infer_scope_from_changes(&self.root, &self.changed_files, &profile.top_scopes);
        let commit_type = infer_commit_type_from_changes(&self.changed_files, &profile.top_types);
        let desc = if sample_names.is_empty() {
            match profile.language {
                CommitSubjectLanguage::Chinese => "更新工作区模块变更".to_string(),
                CommitSubjectLanguage::English | CommitSubjectLanguage::Mixed => {
                    "update workspace modules".to_string()
                }
            }
        } else {
            match profile.language {
                CommitSubjectLanguage::Chinese => format!("更新 {} 等 {} 个文件", sample_names.join("、"), count),
                CommitSubjectLanguage::English | CommitSubjectLanguage::Mixed => {
                    format!("update {} ({} files changed)", sample_names.join(", "), count)
                }
            }
        };

        let msg = match profile.convention {
            CommitConventionKind::ConventionalCommits => {
                if profile.uses_scopes {
                    if let Some(scope) = inferred_scope {
                        format!("{commit_type}({scope}): {desc}")
                    } else {
                        format!("{commit_type}: {desc}")
                    }
                } else {
                    format!("{commit_type}: {desc}")
                }
            }
            CommitConventionKind::GitmojiConventional => {
                let emoji = profile.top_gitmoji.as_deref().unwrap_or("✨");
                if let Some(scope) = inferred_scope.filter(|_| profile.uses_scopes) {
                    format!("{emoji} {commit_type}({scope}): {desc}")
                } else {
                    format!("{emoji} {commit_type}: {desc}")
                }
            }
            CommitConventionKind::Gitmoji => {
                let emoji = profile.top_gitmoji.as_deref().unwrap_or("✨");
                format!("{emoji} {desc}")
            }
            CommitConventionKind::BracketTag => {
                let tag = profile
                    .top_bracket_tag
                    .as_deref()
                    .unwrap_or(commit_type.as_str());
                format!("[{tag}] {desc}")
            }
            CommitConventionKind::TicketPrefix => format!("{commit_type}: {desc}"),
            CommitConventionKind::Plain => {
                if profile.lowercase_subject {
                    desc
                } else {
                    capitalize_first_ascii(&desc)
                }
            }
        };
        self.commit_message_draft = msg.clone();
        msg
    }
}

fn build_default_gitignore(root: &Path) -> String {
    let mut lines = vec![
        "# Generated by Rust AI IDE",
        ".DS_Store",
        "Thumbs.db",
        ".idea/",
        "*.log",
        ".env",
    ];
    if root.join("Cargo.toml").is_file() {
        lines.push("/target/");
    }
    if root.join("package.json").is_file() {
        lines.push("node_modules/");
        lines.push("dist/");
    }
    if root.join("pyproject.toml").is_file() || root.join("requirements.txt").is_file() {
        lines.push("__pycache__/");
        lines.push(".venv/");
        lines.push("*.pyc");
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn empty_git_snapshot() -> GitSnapshot {
    GitSnapshot {
        git_installed: detect_git_executable().is_some(),
        is_git_repo: false,
        git_binary_path: detect_git_executable(),
        git_version: None,
        current_branch: "main".to_string(),
        local_branches: vec!["main".to_string()],
        remote_branches: Vec::new(),
        tags: Vec::new(),
        ahead_behind: (0, 0),
        stashes: Vec::new(),
        remotes: Vec::new(),
        in_rebase: false,
        in_merge: false,
        changed_files: Vec::new(),
        working_diffs: HashMap::new(),
        recent_commits: Vec::new(),
        head_detail: None,
    }
}

fn clip_chars(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let clipped: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{clipped}...")
    } else {
        clipped
    }
}

fn collect_ai_commit_diff(root: &Path, selected_files: &[PathBuf]) -> String {
    let mut cmd = git_command(root);
    cmd.args(["diff", "HEAD"]);
    if !selected_files.is_empty() && selected_files.len() <= 40 {
        cmd.arg("--");
        for path in selected_files {
            let rel = path.strip_prefix(root).unwrap_or(path);
            cmd.arg(rel);
        }
    }
    let Ok(output) = cmd.output() else {
        return String::new();
    };
    if output.status.success() {
        return String::from_utf8_lossy(&output.stdout).to_string();
    }
    // 非 Git 仓库时不再重复执行 git diff --cached；仅在无 HEAD 的新仓库回退到暂存区 diff
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.to_ascii_lowercase().contains("not a git repository") || !selected_files.is_empty() {
        return String::new();
    }
    git_command(root)
        .args(["diff", "--cached"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

fn append_untracked_diff_previews(
    root: &Path,
    target_changes: &[&GitChangedFile],
    working_diffs: &HashMap<PathBuf, Vec<DiffLine>>,
    diff_text: &mut String,
) {
    let mut untracked_added = 0usize;
    for ch in target_changes {
        if ch.status != GitFileStatusKind::Untracked {
            continue;
        }
        if untracked_added >= 6 {
            break;
        }
        let Some(lines) = working_diffs.get(&ch.path) else {
            continue;
        };
        if lines.is_empty() {
            continue;
        }
        let rel = ch
            .path
            .strip_prefix(root)
            .unwrap_or(&ch.path)
            .to_string_lossy()
            .replace('\\', "/");
        if !diff_text.is_empty() && !diff_text.ends_with('\n') {
            diff_text.push('\n');
        }
        diff_text.push_str(&format!("--- /dev/null\n+++ b/{rel}\n"));
        for dl in lines.iter().take(40) {
            diff_text.push('+');
            diff_text.push_str(&dl.content);
            diff_text.push('\n');
        }
        untracked_added += 1;
    }
}

fn collect_recent_commit_messages(root: &Path, limit: usize) -> Vec<String> {
    let limit_str = limit.max(1).to_string();
    let parse_records = |raw: &str| -> Vec<String> {
        raw.split('\x1e')
            .map(|entry| entry.replace("\r\n", "\n").replace('\r', "\n").trim().to_string())
            .filter(|entry| !entry.is_empty() && !entry.starts_with("Revert \""))
            .take(15)
            .map(|entry| clip_chars(&entry, 450))
            .collect()
    };

    if let Ok(output) = git_command(root)
        .args([
            "log",
            "-n",
            &limit_str,
            "--no-merges",
            "--pretty=format:%B%x1e",
        ])
        .output()
    {
        if output.status.success() {
            let parsed = parse_records(&String::from_utf8_lossy(&output.stdout));
            if !parsed.is_empty() {
                return parsed;
            }
            if let Ok(all_out) = git_command(root)
                .args(["log", "-n", &limit_str, "--pretty=format:%B%x1e"])
                .output()
            {
                if all_out.status.success() {
                    return parse_records(&String::from_utf8_lossy(&all_out.stdout));
                }
            }
        }
    }
    Vec::new()
}

fn read_small_text_file(path: &Path, max_bytes: u64, max_chars: usize) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > max_bytes {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(clip_chars(trimmed, max_chars))
}

fn collect_commit_conventions(root: &Path) -> (Option<String>, Option<String>) {
    let mut template_candidates: Vec<PathBuf> = Vec::new();
    if let Ok(out) = git_command(root)
        .args(["config", "--get", "commit.template"])
        .output()
    {
        if out.status.success() {
            let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !raw.is_empty() {
                let p = PathBuf::from(&raw);
                template_candidates.push(if p.is_absolute() { p } else { root.join(p) });
            }
        }
    }
    template_candidates.push(root.join(".gitmessage"));
    template_candidates.push(root.join(".gitmessage.txt"));

    let commit_template = template_candidates
        .into_iter()
        .find_map(|p| read_small_text_file(&p, 16 * 1024, 1500));

    let commitlint_candidates = [
        ".commitlintrc",
        ".commitlintrc.json",
        ".commitlintrc.yml",
        ".commitlintrc.yaml",
        ".commitlintrc.js",
        ".commitlintrc.cjs",
        "commitlint.config.js",
        "commitlint.config.cjs",
        "commitlint.config.mjs",
        "commitlint.config.ts",
    ];
    let commitlint_config = commitlint_candidates
        .iter()
        .find_map(|name| read_small_text_file(&root.join(name), 16 * 1024, 1500));

    (commit_template, commitlint_config)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitConventionKind {
    ConventionalCommits,
    GitmojiConventional,
    Gitmoji,
    BracketTag,
    TicketPrefix,
    Plain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitSubjectLanguage {
    English,
    Chinese,
    Mixed,
}

#[derive(Debug, Clone)]
pub struct CommitHistoryProfile {
    pub sample_count: usize,
    pub convention: CommitConventionKind,
    pub uses_scopes: bool,
    pub top_types: Vec<String>,
    pub top_scopes: Vec<String>,
    pub top_gitmoji: Option<String>,
    pub top_bracket_tag: Option<String>,
    pub language: CommitSubjectLanguage,
    pub lowercase_subject: bool,
    pub uses_trailing_period: bool,
    pub uses_bullet_body: bool,
    pub multiline_ratio_pct: usize,
}

fn contains_cjk(s: &str) -> bool {
    s.chars()
        .any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c) || ('\u{3400}'..='\u{4DBF}').contains(&c))
}

fn extract_leading_gitmoji(s: &str) -> Option<(&str, &str)> {
    let trimmed = s.trim_start();
    if let Some(rest) = trimmed.strip_prefix(':') {
        if let Some(end) = rest.find(':') {
            let code = &rest[..end];
            if !code.is_empty()
                && code.len() <= 24
                && code
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                let after = rest[end + 1..].trim_start();
                if !after.is_empty() {
                    return Some((&trimmed[..end + 2], after));
                }
            }
        }
    }
    let first = trimmed.chars().next()?;
    let cp = first as u32;
    let is_emoji = (0x1F300..=0x1FAFF).contains(&cp)
        || (0x2600..=0x27BF).contains(&cp)
        || matches!(first, '⚡' | '✨' | '🔥' | '✅' | '📝' | '🐛' | '🚀' | '🔧' | '♻' | '🎨' | '📦' | '🔒' | '🔖');
    if is_emoji {
        let mut idx = first.len_utf8();
        for ch in trimmed[idx..].chars() {
            if ch == '\u{FE0F}' || ch == '\u{200D}' {
                idx += ch.len_utf8();
            } else {
                break;
            }
        }
        let after = trimmed[idx..].trim_start();
        if !after.is_empty() {
            return Some((&trimmed[..idx], after));
        }
    }
    None
}

fn parse_conventional_prefix(s: &str) -> Option<(String, Option<String>, &str)> {
    let trimmed = s.trim_start();
    let colon_pos = trimmed.find(':')?;
    if colon_pos < 2 || colon_pos > 48 {
        return None;
    }
    let after_colon = &trimmed[colon_pos + 1..];
    if !after_colon.starts_with(char::is_whitespace) {
        return None;
    }
    let header = trimmed[..colon_pos].strip_suffix('!').unwrap_or(&trimmed[..colon_pos]);
    let (kind, scope) = if let Some(open) = header.find('(') {
        if !header.ends_with(')') || open < 2 {
            return None;
        }
        let kind = &header[..open];
        let scope = &header[open + 1..header.len() - 1];
        if scope.is_empty()
            || !scope
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.' | ' '))
        {
            return None;
        }
        (kind, Some(scope.to_string()))
    } else {
        (header, None)
    };
    if kind.len() < 2
        || kind.len() > 16
        || !kind.chars().all(|c| c.is_ascii_alphabetic() || c == '-' || c == '_')
    {
        return None;
    }
    Some((kind.to_ascii_lowercase(), scope, after_colon.trim_start()))
}

fn parse_bracket_tag(s: &str) -> Option<(String, &str)> {
    let trimmed = s.trim_start();
    let (open, close) = if trimmed.starts_with('[') {
        ('[', ']')
    } else if trimmed.starts_with('【') {
        ('【', '】')
    } else {
        return None;
    };
    let inner_start = open.len_utf8();
    let close_rel = trimmed[inner_start..].find(close)?;
    let tag = trimmed[inner_start..inner_start + close_rel].trim();
    let rest = trimmed[inner_start + close_rel + close.len_utf8()..].trim_start();
    if tag.is_empty() || tag.chars().count() > 24 || rest.is_empty() {
        return None;
    }
    Some((tag.to_string(), rest))
}

fn parse_ticket_prefix(s: &str) -> Option<&str> {
    let trimmed = s.trim_start();
    if let Some(rest) = trimmed.strip_prefix('#') {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let tail = rest[digits.len()..].trim_start_matches(':').trim_start();
            if !tail.is_empty() {
                return Some(tail);
            }
        }
    }
    let dash_pos = trimmed.find('-')?;
    if (2..=10).contains(&dash_pos)
        && trimmed[..dash_pos]
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        let after_dash = &trimmed[dash_pos + 1..];
        let digits: String = after_dash.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let tail = after_dash[digits.len()..].trim_start_matches(':').trim_start();
            if !tail.is_empty() {
                return Some(tail);
            }
        }
    }
    None
}

fn top_keys_by_freq(counts: HashMap<String, usize>, limit: usize) -> Vec<String> {
    let mut items: Vec<(String, usize)> = counts.into_iter().collect();
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    items.into_iter().take(limit).map(|(k, _)| k).collect()
}

pub fn analyze_commit_history(messages: &[String]) -> CommitHistoryProfile {
    if messages.is_empty() {
        return CommitHistoryProfile {
            sample_count: 0,
            convention: CommitConventionKind::ConventionalCommits,
            uses_scopes: true,
            top_types: vec!["feat".into(), "fix".into(), "refactor".into(), "chore".into()],
            top_scopes: Vec::new(),
            top_gitmoji: None,
            top_bracket_tag: None,
            language: CommitSubjectLanguage::English,
            lowercase_subject: true,
            uses_trailing_period: false,
            uses_bullet_body: false,
            multiline_ratio_pct: 0,
        };
    }

    let mut conv_count = 0usize;
    let mut gitmoji_conv_count = 0usize;
    let mut gitmoji_plain_count = 0usize;
    let mut bracket_count = 0usize;
    let mut ticket_count = 0usize;
    let mut scoped_count = 0usize;

    let mut type_freq: HashMap<String, usize> = HashMap::new();
    let mut scope_freq: HashMap<String, usize> = HashMap::new();
    let mut emoji_freq: HashMap<String, usize> = HashMap::new();
    let mut bracket_freq: HashMap<String, usize> = HashMap::new();

    let mut cjk_count = 0usize;
    let mut lower_count = 0usize;
    let mut upper_count = 0usize;
    let mut period_count = 0usize;
    let mut multiline_count = 0usize;
    let mut bullet_body_count = 0usize;

    for raw in messages {
        let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
        let mut lines = normalized.lines();
        let subject = lines.next().unwrap_or("").trim();
        if subject.is_empty() {
            continue;
        }
        let body_lines: Vec<&str> = lines
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if !body_lines.is_empty() {
            multiline_count += 1;
            if body_lines
                .iter()
                .any(|l| l.starts_with("- ") || l.starts_with("* "))
            {
                bullet_body_count += 1;
            }
        }

        if subject.ends_with('.') || subject.ends_with('。') {
            period_count += 1;
        }

        let desc_part = if let Some((emoji, after_emoji)) = extract_leading_gitmoji(subject) {
            *emoji_freq.entry(emoji.to_string()).or_insert(0) += 1;
            if let Some((kind, scope, desc)) = parse_conventional_prefix(after_emoji) {
                gitmoji_conv_count += 1;
                *type_freq.entry(kind).or_insert(0) += 1;
                if let Some(sc) = scope {
                    scoped_count += 1;
                    *scope_freq.entry(sc).or_insert(0) += 1;
                }
                desc
            } else {
                gitmoji_plain_count += 1;
                after_emoji
            }
        } else if let Some((kind, scope, desc)) = parse_conventional_prefix(subject) {
            conv_count += 1;
            *type_freq.entry(kind).or_insert(0) += 1;
            if let Some(sc) = scope {
                scoped_count += 1;
                *scope_freq.entry(sc).or_insert(0) += 1;
            }
            desc
        } else if let Some((tag, desc)) = parse_bracket_tag(subject) {
            bracket_count += 1;
            *bracket_freq.entry(tag).or_insert(0) += 1;
            desc
        } else if let Some(desc) = parse_ticket_prefix(subject) {
            ticket_count += 1;
            desc
        } else {
            subject
        };

        if contains_cjk(desc_part) {
            cjk_count += 1;
        }
        if let Some(first_alpha) = desc_part.chars().find(|c| c.is_ascii_alphabetic()) {
            if first_alpha.is_ascii_lowercase() {
                lower_count += 1;
            } else {
                upper_count += 1;
            }
        }
    }

    let sample_count = messages.len().max(1);
    let convention = {
        let candidates = [
            (CommitConventionKind::ConventionalCommits, conv_count),
            (CommitConventionKind::GitmojiConventional, gitmoji_conv_count),
            (CommitConventionKind::Gitmoji, gitmoji_plain_count),
            (CommitConventionKind::BracketTag, bracket_count),
            (CommitConventionKind::TicketPrefix, ticket_count),
        ];
        candidates
            .into_iter()
            .filter(|(_, c)| *c * 10 >= sample_count * 3)
            .max_by_key(|(_, c)| *c)
            .map(|(k, _)| k)
            .unwrap_or(CommitConventionKind::Plain)
    };

    let conv_total = conv_count + gitmoji_conv_count;
    let uses_scopes = conv_total > 0 && scoped_count * 3 >= conv_total;
    let language = if cjk_count * 10 >= sample_count * 6 {
        CommitSubjectLanguage::Chinese
    } else if cjk_count * 10 >= sample_count * 2 {
        CommitSubjectLanguage::Mixed
    } else {
        CommitSubjectLanguage::English
    };

    let top_types = top_keys_by_freq(type_freq, 6);
    let top_scopes = top_keys_by_freq(scope_freq, 8);
    let top_gitmoji = top_keys_by_freq(emoji_freq, 1).into_iter().next();
    let top_bracket_tag = top_keys_by_freq(bracket_freq, 1).into_iter().next();

    CommitHistoryProfile {
        sample_count,
        convention,
        uses_scopes,
        top_types,
        top_scopes,
        top_gitmoji,
        top_bracket_tag,
        language,
        lowercase_subject: lower_count >= upper_count,
        uses_trailing_period: period_count * 2 > sample_count,
        uses_bullet_body: bullet_body_count * 4 >= sample_count,
        multiline_ratio_pct: (multiline_count * 100) / sample_count,
    }
}

pub fn summarize_commit_history(
    messages: &[String],
    commit_template: Option<&str>,
    commitlint_config: Option<&str>,
) -> String {
    let p = analyze_commit_history(messages);
    if p.sample_count == 0 {
        let mut out = String::from(
            "No prior commit history found in repository.\n\
             - Default convention: Conventional Commits (`<type>(<scope>): <description>` or `<type>: <description>`).\n\
             - Keep the title <= 72 chars, lowercase description start, no trailing period.",
        );
        if commit_template.is_some() {
            out.push_str("\n- Follow the repository commit template provided below.");
        }
        if commitlint_config.is_some() {
            out.push_str("\n- Satisfy the repository commitlint rules provided below.");
        }
        return out;
    }

    let conv_desc = match p.convention {
        CommitConventionKind::ConventionalCommits => {
            if p.uses_scopes {
                "Conventional Commits WITH optional scope: `<type>(<scope>): <description>` (or `<type>: <description>` when cross-cutting)"
            } else {
                "Conventional Commits WITHOUT frequent scopes: `<type>: <description>`"
            }
        }
        CommitConventionKind::GitmojiConventional => {
            "Gitmoji + Conventional Commits: `<emoji> <type>(<scope>): <description>`"
        }
        CommitConventionKind::Gitmoji => "Gitmoji prefix: `<emoji> <description>`",
        CommitConventionKind::BracketTag => "Bracket tag prefix: `[tag] <description>` or `【标签】<description>`",
        CommitConventionKind::TicketPrefix => "Ticket/Issue ID prefix followed by concise description",
        CommitConventionKind::Plain => "Concise plain subject line (matching the exact style of recent commits)",
    };

    let lang_desc = match p.language {
        CommitSubjectLanguage::Chinese => "Chinese (中文描述)",
        CommitSubjectLanguage::English => "English",
        CommitSubjectLanguage::Mixed => "Match recent commit language (Chinese/English mix observed)",
    };

    let casing_desc = if p.lowercase_subject {
        "lowercase initial letter in description"
    } else {
        "capitalized initial letter in description"
    };

    let body_desc = if p.uses_bullet_body {
        format!(
            "Multi-line bullet body is common ({}% multiline): use a single line for small focused edits, or add a blank line + concise `- ` bullet points for multi-part changes.",
            p.multiline_ratio_pct
        )
    } else if p.multiline_ratio_pct >= 25 {
        format!(
            "Body paragraphs appear in {}% of commits; include a short body only for complex multi-part changes.",
            p.multiline_ratio_pct
        )
    } else {
        "Strictly single-line commit messages in history: output ONLY a single title line (no body)."
            .to_string()
    };

    let mut lines = vec![
        format!("- Analyzed {} recent non-merge commits.", p.sample_count),
        format!("- Format convention: {conv_desc}"),
    ];
    if !p.top_types.is_empty() {
        lines.push(format!("- Common commit types: {}", p.top_types.join(", ")));
    }
    if !p.top_scopes.is_empty() {
        lines.push(format!("- Observed scopes: {}", p.top_scopes.join(", ")));
    }
    lines.push(format!(
        "- Subject language & style: {lang_desc}; {casing_desc}; {}",
        if p.uses_trailing_period {
            "trailing period allowed"
        } else {
            "no trailing period"
        }
    ));
    lines.push(format!("- Body structure: {body_desc}"));
    if commit_template.is_some() {
        lines.push("- Follow the repository commit template provided below.".to_string());
    }
    if commitlint_config.is_some() {
        lines.push("- Strictly adhere to the repository commitlint config provided below.".to_string());
    }
    lines.join("\n")
}

fn infer_scope_from_changes(
    root: &Path,
    changed_files: &[GitChangedFile],
    top_scopes: &[String],
) -> Option<String> {
    let mut candidates: HashMap<String, usize> = HashMap::new();
    for ch in changed_files {
        let rel = ch.path.strip_prefix(root).unwrap_or(&ch.path);
        for comp in rel.components() {
            let part = comp.as_os_str().to_string_lossy();
            if matches!(part.as_ref(), "src" | "crates" | "packages" | "apps" | "lib" | "tests") {
                continue;
            }
            if part.contains('.') {
                continue;
            }
            *candidates.entry(part.to_string()).or_insert(0) += 1;
            break;
        }
    }
    for scope in top_scopes {
        if candidates.contains_key(scope) {
            return Some(scope.clone());
        }
    }
    top_keys_by_freq(candidates, 1)
        .into_iter()
        .next()
        .or_else(|| top_scopes.first().cloned())
}

fn infer_commit_type_from_changes(
    changed_files: &[GitChangedFile],
    top_types: &[String],
) -> String {
    if !changed_files.is_empty() {
        let all_docs = changed_files.iter().all(|f| {
            f.path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("txt"))
        });
        if all_docs {
            return "docs".to_string();
        }
        let all_tests = changed_files.iter().all(|f| {
            let s = f.path.to_string_lossy().to_ascii_lowercase();
            s.contains("test") || s.contains("spec")
        });
        if all_tests {
            return "test".to_string();
        }
        let has_added = changed_files
            .iter()
            .any(|f| matches!(f.status, GitFileStatusKind::Added | GitFileStatusKind::Untracked));
        if has_added {
            return "feat".to_string();
        }
    }
    top_types
        .iter()
        .find(|t| matches!(t.as_str(), "fix" | "feat" | "refactor" | "chore"))
        .cloned()
        .unwrap_or_else(|| "feat".to_string())
}

fn capitalize_first_ascii(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {
            let mut out = first.to_ascii_uppercase().to_string();
            out.push_str(chars.as_str());
            out
        }
        _ => s.to_string(),
    }
}

/// 统一规范化 Diff 文本行尾（兼容 `\r\r\n` / `\r\n` / 孤立 `\r` 与文件末尾换行符差异，避免整文件误统计为 `+N -N`）
pub fn normalize_diff_text(text: &str) -> String {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut out = String::with_capacity(text.len() + 1);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            while chars.peek() == Some(&'\r') {
                chars.next();
            }
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// 基于 `similar` (Myers/Patience Diff) 计算新旧文本的行级差异
pub fn compute_line_diff(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    let old_norm = normalize_diff_text(old_text);
    let new_norm = normalize_diff_text(new_text);
    let diff = TextDiff::from_lines(&old_norm, &new_norm);
    let mut lines = Vec::new();

    for change in diff.iter_all_changes() {
        let kind = match change.tag() {
            ChangeTag::Equal => DiffLineKind::Context,
            ChangeTag::Insert => DiffLineKind::Added,
            ChangeTag::Delete => DiffLineKind::Removed,
        };
        let mut content = change.to_string();
        while content.ends_with('\n') || content.ends_with('\r') {
            content.pop();
        }
        lines.push(DiffLine {
            kind,
            old_lineno: change.old_index(),
            new_lineno: change.new_index(),
            content,
        });
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    #[test]
    fn test_compute_line_diff() {
        let old = "fn a() {\n    let x = 1;\n}\n";
        let new = "fn a() {\n    let x = 42;\n    println!(\"{x}\");\n}\n";
        let diff = compute_line_diff(old, new);
        assert!(diff.iter().any(|l| l.kind == DiffLineKind::Removed));
        assert!(diff.iter().any(|l| l.kind == DiffLineKind::Added));
    }

    #[test]
    fn test_async_git_worker_and_optimistic_stage_toggle() {
        let tmp = std::env::temp_dir().join("rust_ai_ide_async_git_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let _ = git_command(&tmp).args(["init"]).output();
        fs::write(tmp.join("hello.rs"), "fn main() {}\n").unwrap();

        let woken = Arc::new(AtomicBool::new(false));
        let woken_cb = Arc::clone(&woken);
        let mut engine = GitEngine::open_with_wakeup(
            &tmp,
            Some(Arc::new(move || {
                woken_cb.store(true, Ordering::Release);
            })),
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let outcome = engine.poll_updates();
            if outcome.changed && !engine.changed_files.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }

        assert!(woken.load(Ordering::Acquire));
        assert_eq!(engine.changed_files.len(), 1);
        assert!(!engine.changed_files[0].staged);

        // 乐观翻转后立即反映到 UI 状态，随后后台 git add 同步一致
        let target = engine.changed_files[0].path.clone();
        engine.request_toggle_stage_file(&target);
        assert!(engine.changed_files[0].staged);

        let deadline2 = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline2 {
            let outcome = engine.poll_updates();
            if outcome.changed {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(engine.changed_files[0].staged);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_idea_checkbox_selection_does_not_stage_and_commits_only_checked() {
        let tmp = std::env::temp_dir().join("rust_ai_ide_idea_commit_selection_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let _ = git_command(&tmp).args(["init"]).output();
        ensure_git_identity(&tmp);
        let a_path = tmp.join("a.txt");
        let b_path = tmp.join("b.txt");
        let c_path = tmp.join("c.txt");
        fs::write(&a_path, "a1\n").unwrap();
        fs::write(&b_path, "b1\n").unwrap();
        let _ = git_command(&tmp).args(["add", "-A"]).output();
        let _ = git_command(&tmp).args(["commit", "-m", "init"]).output();

        // Modify a & b, add untracked c
        fs::write(&a_path, "a2\n").unwrap();
        fs::write(&b_path, "b2\n").unwrap();
        fs::write(&c_path, "c1\n").unwrap();

        let mut engine = GitEngine::open(&tmp);
        assert_eq!(engine.changed_files.len(), 3);
        // Tracked modified files are checked by default, untracked file is unchecked by default
        assert!(engine.is_file_checked_for_commit(&a_path));
        assert!(engine.is_file_checked_for_commit(&b_path));
        assert!(!engine.is_file_checked_for_commit(&c_path));

        // Toggle b off and c on — must NOT stage anything in git index!
        assert!(!engine.toggle_commit_file_checked(&b_path));
        assert!(engine.toggle_commit_file_checked(&c_path));
        engine.refresh();
        assert!(engine.changed_files.iter().all(|f| !f.staged));
        assert!(engine.is_file_checked_for_commit(&a_path));
        assert!(!engine.is_file_checked_for_commit(&b_path));
        assert!(engine.is_file_checked_for_commit(&c_path));

        // Commit only checked files (a.txt and c.txt)
        engine.request_commit_with_options("commit only checked", false, false);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let outcome = engine.poll_updates();
            if outcome.clear_commit_input {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }

        // Only b.txt should remain modified in working tree
        assert_eq!(engine.changed_files.len(), 1);
        assert_eq!(engine.changed_files[0].path, b_path);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_idea_git_stash_tag_remote_reset_and_blame_operations() {
        let tmp = std::env::temp_dir().join(format!(
            "rust_ai_ide_full_git_suite_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let _ = git_command(&tmp).args(["init"]).output();
        ensure_git_identity(&tmp);

        let f_path = tmp.join("lib.rs");
        fs::write(&f_path, "pub fn v() -> i32 { 1 }\n").unwrap();
        let _ = git_command(&tmp).args(["add", "-A"]).output();
        let _ = git_command(&tmp).args(["commit", "-m", "v1"]).output();

        let mut engine = GitEngine::open(&tmp);
        // 1. Tag creation
        engine.request_create_tag("v1.0.0", None);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !engine.tags.contains(&"v1.0.0".to_string()) {
            engine.poll_updates();
            thread::sleep(Duration::from_millis(20));
        }
        assert!(engine.tags.contains(&"v1.0.0".to_string()));

        // 2. Stash & Pop
        fs::write(&f_path, "pub fn v() -> i32 { 2 }\n").unwrap();
        engine.request_stash_save("wip feature", false);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && engine.stashes.is_empty() {
            engine.poll_updates();
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(engine.stashes.len(), 1);
        assert!(engine.stashes[0].message.contains("wip feature"));

        engine.request_stash_pop(&engine.stashes[0].index_ref.clone());
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !engine.stashes.is_empty() {
            engine.poll_updates();
            thread::sleep(Duration::from_millis(20));
        }
        assert!(engine.stashes.is_empty());
        assert!(fs::read_to_string(&f_path).unwrap().contains("2"));

        // 3. Remote add / remove
        engine.request_add_remote("upstream", "https://example.com/repo.git");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && engine.remotes.is_empty() {
            engine.poll_updates();
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(engine.remotes.len(), 1);
        assert_eq!(engine.remotes[0].name, "upstream");

        // 4. Blame & File history
        engine.request_load_file_blame(&f_path);
        engine.request_load_file_history(&f_path);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && (!engine.file_blames.contains_key(&f_path)
                || !engine.file_histories.contains_key(&f_path))
        {
            engine.poll_updates();
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!engine.file_blames[&f_path].is_empty());
        assert!(!engine.file_histories[&f_path].is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_derive_repo_name_from_url() {
        assert_eq!(
            derive_repo_name_from_url("https://github.com/rust-lang/rust.git").as_deref(),
            Some("rust")
        );
        assert_eq!(
            derive_repo_name_from_url("https://github.com/owner/my-repo/").as_deref(),
            Some("my-repo")
        );
        assert_eq!(
            derive_repo_name_from_url("git@github.com:org/awesome-ide.git").as_deref(),
            Some("awesome-ide")
        );
        assert_eq!(derive_repo_name_from_url("   "), None);
    }

    #[test]
    fn test_commit_history_style_analysis_and_ai_context() {
        // 1. Conventional commits with scopes + bullet bodies
        let conv_msgs = vec![
            "feat(ide-app): add markdown preview\n\n- Support tables\n- Add toolbar".to_string(),
            "fix(agent): preserve unicode env".to_string(),
            "perf(ide-app): cache wrapped rows\n\n- Avoid clone".to_string(),
            "chore(release): bump version".to_string(),
        ];
        let profile = analyze_commit_history(&conv_msgs);
        assert_eq!(profile.convention, CommitConventionKind::ConventionalCommits);
        assert!(profile.uses_scopes);
        assert!(profile.top_scopes.contains(&"ide-app".to_string()));
        assert_eq!(profile.language, CommitSubjectLanguage::English);
        assert!(profile.uses_bullet_body);

        // 2. Bracket tag + Chinese commits
        let zh_msgs = vec![
            "【功能】新增用户登录校验".to_string(),
            "【修复】解决并发提交空指针异常".to_string(),
            "【重构】拆分订单状态机".to_string(),
        ];
        let zh_profile = analyze_commit_history(&zh_msgs);
        assert_eq!(zh_profile.convention, CommitConventionKind::BracketTag);
        assert_eq!(zh_profile.language, CommitSubjectLanguage::Chinese);

        // 3. Gitmoji + Conventional Commits
        let gm_msgs = vec![
            "✨ feat(ui): add dark theme".to_string(),
            "🐛 fix(api): handle timeout".to_string(),
        ];
        let gm_profile = analyze_commit_history(&gm_msgs);
        assert_eq!(gm_profile.convention, CommitConventionKind::GitmojiConventional);
        assert!(matches!(gm_profile.top_gitmoji.as_deref(), Some("✨" | "🐛")));

        // 4. End-to-end AiCommitDiff collects history, style summary, and commit template
        let tmp = std::env::temp_dir().join(format!("rust_ai_ide_ai_ctx_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let _ = git_command(&tmp).args(["init"]).output();
        ensure_git_identity(&tmp);
        fs::write(tmp.join(".gitmessage"), "# <type>(<scope>): <subject>\n").unwrap();
        let f_path = tmp.join("src_mod.rs");
        fs::write(&f_path, "pub fn a() {}\n").unwrap();
        let _ = git_command(&tmp).args(["add", "-A"]).output();
        let _ = git_command(&tmp)
            .args(["commit", "-m", "feat(core): initial module\n\n- Add src_mod.rs"])
            .output();
        let _ = git_command(&tmp)
            .args(["commit", "--allow-empty", "-m", "fix(core): handle edge case"])
            .output();

        fs::write(&f_path, "pub fn a() -> i32 { 42 }\n").unwrap();
        let mut engine = GitEngine::open(&tmp);
        assert!(engine.request_ai_commit_diff());
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got_ctx: Option<AiCommitContext> = None;
        while Instant::now() < deadline {
            let outcome = engine.poll_updates();
            if let Some(ctx) = outcome.ai_diff_ready {
                got_ctx = Some(ctx);
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let ctx = got_ctx.expect("AiCommitContext should be produced");
        assert!(ctx.diff_text.contains("pub fn a() -> i32 { 42 }"));
        assert!(ctx
            .recent_commit_messages
            .iter()
            .any(|m| m.contains("feat(core): initial module")));
        assert!(ctx.style_summary.contains("Conventional Commits"));
        assert!(ctx.commit_template.as_deref().is_some_and(|t| t.contains("<type>(<scope>)")));

        let fallback = engine.generate_ai_commit_message();
        assert!(fallback.starts_with("fix(") || fallback.starts_with("feat("));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_non_git_workspace_detection_and_init_repo() {
        if detect_git_executable().is_none() {
            return;
        }
        let tmp = std::env::temp_dir().join(format!(
            "rust_ai_ide_git_init_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        fs::write(tmp.join("main.rs"), "fn main() {}\n").unwrap();

        let mut engine = GitEngine::open(&tmp);
        assert!(engine.has_loaded_snapshot);
        assert!(engine.git_installed);
        assert!(!engine.is_git_repo);

        engine.request_init_repo(true);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let outcome = engine.poll_updates();
            if outcome.changed && engine.is_git_repo {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }

        assert!(engine.is_git_repo);
        assert!(tmp.join(".git").exists());
        let gi = fs::read_to_string(tmp.join(".gitignore")).unwrap();
        assert!(gi.contains("/target/"));
        assert!(engine
            .changed_files
            .iter()
            .any(|f| f.path.file_name().and_then(|n| n.to_str()) == Some("main.rs")));

        let _ = fs::remove_dir_all(&tmp);
    }
}
