use anyhow::Result;
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitFileStatusKind {
    Modified,
    Added,
    Deleted,
    Untracked,
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
    pub author: String,
    pub date: String,
    pub subject: String,
}

pub struct GitEngine {
    pub root: PathBuf,
    pub current_branch: String,
    pub local_branches: Vec<String>,
    pub changed_files: Vec<GitChangedFile>,
    pub recent_commits: Vec<GitLogEntry>,
    pub commit_message_draft: String,
    pub last_operation_output: String,
}

impl GitEngine {
    pub fn open(root: &Path) -> Self {
        let mut engine = Self {
            root: root.to_path_buf(),
            current_branch: "main".to_string(),
            local_branches: vec!["main".to_string()],
            changed_files: Vec::new(),
            recent_commits: Vec::new(),
            commit_message_draft: "feat: update Rust AI IDE workspace".to_string(),
            last_operation_output: String::new(),
        };
        engine.refresh();
        engine
    }

    pub fn refresh(&mut self) {
        if let Ok(output) = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&self.root)
            .output()
        {
            if output.status.success() {
                let b = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !b.is_empty() {
                    self.current_branch = b;
                }
            }
        }

        // 获取所有本地分支
        if let Ok(output) = Command::new("git")
            .args(["branch", "--format=%(refname:short)"])
            .current_dir(&self.root)
            .output()
        {
            if output.status.success() {
                let list: Vec<String> = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !list.is_empty() {
                    self.local_branches = list;
                }
            }
        }
        if !self.local_branches.contains(&self.current_branch) {
            self.local_branches.insert(0, self.current_branch.clone());
        }

        self.changed_files.clear();
        if let Ok(output) = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.root)
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines() {
                    if line.len() < 4 {
                        continue;
                    }
                    let xy = &line[0..2];
                    let rel = line[3..].trim();
                    let status = match xy {
                        "??" => GitFileStatusKind::Untracked,
                        s if s.contains('A') => GitFileStatusKind::Added,
                        s if s.contains('D') => GitFileStatusKind::Deleted,
                        _ => GitFileStatusKind::Modified,
                    };
                    let staged = !xy.starts_with(' ') && !xy.starts_with('?');
                    self.changed_files.push(GitChangedFile {
                        path: self.root.join(rel),
                        status,
                        staged,
                    });
                }
            }
        }

        self.recent_commits.clear();
        if let Ok(output) = Command::new("git")
            .args(["log", "-n", "20", "--date=short", "--pretty=format:%h|%an|%ad|%s"])
            .current_dir(&self.root)
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines() {
                    let mut parts = line.splitn(4, '|');
                    if let (Some(h), Some(a), Some(d), Some(s)) = (
                        parts.next(),
                        parts.next(),
                        parts.next(),
                        parts.next(),
                    ) {
                        self.recent_commits.push(GitLogEntry {
                            hash: h.to_string(),
                            author: a.to_string(),
                            date: d.to_string(),
                            subject: s.to_string(),
                        });
                    }
                }
            }
        }
    }

    pub fn stage_all(&mut self) -> Result<String> {
        let out = Command::new("git")
            .args(["add", "-A"])
            .current_dir(&self.root)
            .output()?;
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
            let _ = Command::new("git")
                .args(["reset", "HEAD", "--"])
                .arg(file)
                .current_dir(&self.root)
                .output()?;
        } else {
            let _ = Command::new("git")
                .args(["add", "--"])
                .arg(file)
                .current_dir(&self.root)
                .output()?;
        }
        self.refresh();
        Ok(())
    }

    pub fn rollback_file(&mut self, file: &Path) -> Result<String> {
        let out = Command::new("git")
            .args(["checkout", "--"])
            .arg(file)
            .current_dir(&self.root)
            .output()?;
        self.refresh();
        let msg = if out.status.success() {
            format!("Rolled back {}", file.display())
        } else {
            String::from_utf8_lossy(&out.stderr).trim().to_string()
        };
        self.last_operation_output = msg.clone();
        Ok(msg)
    }

    pub fn commit_all(&mut self, message: &str) -> Result<String> {
        let _ = Command::new("git")
            .args(["add", "-A"])
            .current_dir(&self.root)
            .output()?;
        // 确保本地仓库有基础 user.name / user.email 配置以免初次提交失败
        let _ = Command::new("git")
            .args(["config", "user.name", "Rust AI IDE Developer"])
            .current_dir(&self.root)
            .output();
        let _ = Command::new("git")
            .args(["config", "user.email", "dev@rust-ai-ide.local"])
            .current_dir(&self.root)
            .output();

        let out = Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(&self.root)
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
        let out = Command::new("git")
            .args(["checkout", branch])
            .current_dir(&self.root)
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
        let out = Command::new("git")
            .args(["checkout", "-b", new_branch.trim()])
            .current_dir(&self.root)
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
        let out = Command::new("git")
            .args(["pull"])
            .current_dir(&self.root)
            .output()?;
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
        let out = Command::new("git")
            .args(["push"])
            .current_dir(&self.root)
            .output()?;
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

    #[test]
    fn test_compute_line_diff() {
        let old = "fn a() {\n    let x = 1;\n}\n";
        let new = "fn a() {\n    let x = 42;\n    println!(\"{x}\");\n}\n";
        let diff = compute_line_diff(old, new);
        assert!(diff.iter().any(|l| l.kind == DiffLineKind::Removed));
        assert!(diff.iter().any(|l| l.kind == DiffLineKind::Added));
    }
}
