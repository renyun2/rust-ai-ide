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

#[derive(Debug, Clone)]
struct GitSnapshot {
    current_branch: String,
    local_branches: Vec<String>,
    remote_branches: Vec<String>,
    tags: Vec<String>,
    ahead_behind: (usize, usize),
    changed_files: Vec<GitChangedFile>,
    working_diffs: HashMap<PathBuf, Vec<DiffLine>>,
    recent_commits: Vec<GitLogEntry>,
    head_detail: Option<GitCommitDetail>,
}

#[derive(Debug)]
enum GitWorkerCmd {
    Refresh,
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
    MergeBranch(String),
    CherryPick(String),
    RevertCommit(String),
    ResetSoft(String),
    Fetch,
    Pull,
    Push,
    LoadCommitDetail(String),
    AiCommitDiff,
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
    AiDiffReady {
        snapshot: GitSnapshot,
        diff_text: String,
    },
}

#[derive(Debug, Default)]
pub struct GitPollOutcome {
    pub changed: bool,
    pub notification: Option<String>,
    pub clear_commit_input: bool,
    pub ai_diff_ready: Option<(String, Vec<String>)>,
}

pub struct GitEngine {
    pub root: PathBuf,
    pub current_branch: String,
    pub local_branches: Vec<String>,
    pub remote_branches: Vec<String>,
    pub tags: Vec<String>,
    pub ahead_behind: (usize, usize),
    pub changed_files: Vec<GitChangedFile>,
    pub commit_checked_files: HashSet<PathBuf>,
    known_changed_files: HashSet<PathBuf>,
    pub working_diffs: HashMap<PathBuf, Vec<DiffLine>>,
    pub recent_commits: Vec<GitLogEntry>,
    pub commit_details: HashMap<String, GitCommitDetail>,
    pub commit_message_draft: String,
    pub last_operation_output: String,
    pub has_loaded_snapshot: bool,
    cmd_tx: mpsc::Sender<GitWorkerCmd>,
    event_rx: mpsc::Receiver<GitWorkerEvent>,
    pending_refresh: Arc<AtomicBool>,
    is_busy: Arc<AtomicBool>,
}

fn git_command(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(root);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

fn parse_unified_diff_sections(
    diff_text: &str,
) -> (Vec<GitCommitFileChange>, HashMap<String, Vec<DiffLine>>) {
    let mut files: Vec<GitCommitFileChange> = Vec::new();
    let mut file_diffs: HashMap<String, Vec<DiffLine>> = HashMap::new();

    let mut current_path: Option<String> = None;
    let mut current_status = GitFileStatusKind::Modified;
    let mut current_lines: Vec<DiffLine> = Vec::new();
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
                         files_out: &mut Vec<GitCommitFileChange>,
                         diffs_out: &mut HashMap<String, Vec<DiffLine>>| {
        if let Some(path) = path_opt.take() {
            files_out.push(GitCommitFileChange {
                path: path.clone(),
                status,
                additions: add,
                deletions: del,
            });
            diffs_out.insert(path, std::mem::take(lines));
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
            current_lines.push(DiffLine {
                kind: DiffLineKind::Context,
                old_lineno: None,
                new_lineno: None,
                content: raw_line.to_string(),
            });
            continue;
        }

        if !in_hunk {
            continue;
        }

        if let Some(added) = raw_line.strip_prefix('+') {
            additions += 1;
            current_lines.push(DiffLine {
                kind: DiffLineKind::Added,
                old_lineno: None,
                new_lineno: Some(new_ln),
                content: added.to_string(),
            });
            new_ln += 1;
        } else if let Some(removed) = raw_line.strip_prefix('-') {
            deletions += 1;
            current_lines.push(DiffLine {
                kind: DiffLineKind::Removed,
                old_lineno: Some(old_ln),
                new_lineno: None,
                content: removed.to_string(),
            });
            old_ln += 1;
        } else if let Some(ctx) = raw_line.strip_prefix(' ') {
            current_lines.push(DiffLine {
                kind: DiffLineKind::Context,
                old_lineno: Some(old_ln),
                new_lineno: Some(new_ln),
                content: ctx.to_string(),
            });
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
            if let Ok(output) = git_command(root).args(["status", "--porcelain"]).output() {
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
            let mut recent_commits = Vec::new();
            if let Ok(output) = git_command(root)
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
                    let text = String::from_utf8_lossy(&output.stdout);
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
                            recent_commits.push(GitLogEntry {
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
                }
            }
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
            if let Ok(content) = std::fs::read_to_string(&ch.path) {
                let added_lines: Vec<DiffLine> = content
                    .lines()
                    .take(150)
                    .enumerate()
                    .map(|(idx, line)| DiffLine {
                        kind: DiffLineKind::Added,
                        old_lineno: None,
                        new_lineno: Some(idx + 1),
                        content: line.to_string(),
                    })
                    .collect();
                working_diffs.insert(ch.path.clone(), added_lines);
            }
        }
    }

    GitSnapshot {
        current_branch: branch_res.0,
        local_branches: branch_res.1,
        remote_branches: branch_res.2,
        tags: branch_res.3,
        ahead_behind: branch_res.4,
        changed_files: status_res,
        working_diffs,
        recent_commits: log_res.0,
        head_detail: log_res.1,
    }
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
                    GitWorkerCmd::MergeBranch(branch) => {
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
                    GitWorkerCmd::ResetSoft(hash) => {
                        let out = git_command(&root)
                            .args(["reset", "--soft", &hash])
                            .output();
                        let snapshot = collect_snapshot(&root);
                        let msg = match out {
                            Ok(o) if o.status.success() => {
                                format!("Soft reset HEAD to {hash}")
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
                    GitWorkerCmd::AiCommitDiff => {
                        let (snapshot, diff_text) = thread::scope(|s| {
                            let h_snap = s.spawn(|| collect_snapshot(&root));
                            let h_diff = s.spawn(|| {
                                git_command(&root)
                                    .args(["diff", "HEAD"])
                                    .output()
                                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                                    .unwrap_or_default()
                            });
                            (
                                h_snap.join().unwrap_or_else(|_| GitSnapshot {
                                    current_branch: "main".to_string(),
                                    local_branches: vec!["main".to_string()],
                                    remote_branches: Vec::new(),
                                    tags: Vec::new(),
                                    ahead_behind: (0, 0),
                                    changed_files: Vec::new(),
                                    working_diffs: HashMap::new(),
                                    recent_commits: Vec::new(),
                                    head_detail: None,
                                }),
                                h_diff.join().unwrap_or_default(),
                            )
                        });
                        notify(
                            &event_tx,
                            GitWorkerEvent::AiDiffReady {
                                snapshot,
                                diff_text,
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
        let engine = Self {
            root: root_buf,
            current_branch: "main".to_string(),
            local_branches: vec!["main".to_string()],
            remote_branches: Vec::new(),
            tags: Vec::new(),
            ahead_behind: (0, 0),
            changed_files: Vec::new(),
            commit_checked_files: HashSet::new(),
            known_changed_files: HashSet::new(),
            working_diffs: HashMap::new(),
            recent_commits: Vec::new(),
            commit_details: HashMap::new(),
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
        self.current_branch = snapshot.current_branch;
        self.local_branches = snapshot.local_branches;
        self.remote_branches = snapshot.remote_branches;
        self.tags = snapshot.tags;
        self.ahead_behind = snapshot.ahead_behind;

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
            self.commit_details.insert(detail.hash.clone(), detail);
        }
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

    pub fn request_merge_branch(&self, branch: &str) {
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::MergeBranch(branch.to_string()));
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
        let _ = self
            .cmd_tx
            .send(GitWorkerCmd::ResetSoft(commit_hash.to_string()));
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

    pub fn request_ai_commit_diff(&self) {
        let _ = self.cmd_tx.send(GitWorkerCmd::AiCommitDiff);
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
                    self.commit_details.insert(detail.hash.clone(), detail);
                }
                GitWorkerEvent::AiDiffReady {
                    snapshot,
                    diff_text,
                } => {
                    let changed_files: Vec<String> = snapshot
                        .changed_files
                        .iter()
                        .map(|c| format!("{:?}: {}", c.status, c.path.display()))
                        .collect();
                    self.apply_snapshot(snapshot);
                    outcome.ai_diff_ready = Some((diff_text, changed_files));
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

    /// 根据当前变更文件列表自动生成规范的 Conventional Commit 消息
    pub fn generate_ai_commit_message(&mut self) -> String {
        let count = self.changed_files.len();
        let sample_names: Vec<String> = self
            .changed_files
            .iter()
            .take(3)
            .filter_map(|f| f.path.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()))
            .collect();
        let msg = if sample_names.is_empty() {
            "feat(ide): update workspace modules and multi-agent integration".to_string()
        } else {
            format!(
                "feat(ide): update {} ({} files changed)",
                sample_names.join(", "),
                count
            )
        };
        self.commit_message_draft = msg.clone();
        msg
    }
}

/// 基于 `similar` (Myers/Patience Diff) 计算新旧文本的行级差异
pub fn compute_line_diff(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(old_text, new_text);
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
}
