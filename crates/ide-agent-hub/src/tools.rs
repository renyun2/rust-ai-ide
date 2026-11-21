use crate::types::{AgentWorkbenchMode, IdeCodeContext, ProposedFilePatch};
use ide_core::LanguageId;
use ide_git::compute_line_diff;
use ide_search::find_in_files;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use uuid::Uuid;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn agent_tool_declarations() -> Vec<Value> {
    vec![
        json!({
            "name": "list_dir",
            "description": "List files and directories inside the workspace.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative directory path from workspace root (empty or '.' for root)." }
                }
            }
        }),
        json!({
            "name": "read_file",
            "description": "Read text file contents with line numbers.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative or workspace file path." },
                    "start_line": { "type": "integer", "description": "Optional 1-indexed start line." },
                    "end_line": { "type": "integer", "description": "Optional 1-indexed inclusive end line." }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "search_code",
            "description": "Search text across workspace files.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Text pattern to search." },
                    "max_results": { "type": "integer", "description": "Max matches (default 20)." }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "replace_in_file",
            "description": "Replace an exact substring in a workspace file (automatically creates checkpoint backup).",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative or workspace file path." },
                    "old_text": { "type": "string", "description": "Exact existing text to replace." },
                    "new_text": { "type": "string", "description": "Replacement text." }
                },
                "required": ["path", "old_text", "new_text"]
            }
        }),
        json!({
            "name": "write_file",
            "description": "Create or overwrite a workspace file (automatically creates checkpoint backup).",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative or workspace file path." },
                    "content": { "type": "string", "description": "Full file content." }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "run_command",
            "description": "Execute a shell command in workspace root (e.g., cargo check, cargo test, git status).",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "PowerShell command line to run." }
                },
                "required": ["command"]
            }
        }),
    ]
}

pub fn summarize_tool_args(args: &Value) -> String {
    if let Some(p) = args["path"].as_str() {
        return p.to_string();
    }
    if let Some(q) = args["query"].as_str() {
        return q.to_string();
    }
    if let Some(c) = args["command"].as_str() {
        return c.chars().take(48).collect();
    }
    String::new()
}

pub fn first_line_preview(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(72)
        .collect()
}

pub fn resolve_safe_workspace_path(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Ok(root.to_path_buf());
    }
    let rel = Path::new(trimmed);
    let candidate = if rel.is_absolute() {
        rel.to_path_buf()
    } else {
        let mut clean = PathBuf::new();
        for comp in rel.components() {
            match comp {
                Component::Normal(s) => clean.push(s),
                Component::CurDir => {}
                Component::ParentDir => {
                    clean.pop();
                }
                _ => {}
            }
        }
        root.join(clean)
    };
    let root_canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let check_target = if candidate.exists() {
        fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone())
    } else if let Some(parent) = candidate.parent() {
        let p_canon = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
        if let Some(name) = candidate.file_name() {
            p_canon.join(name)
        } else {
            p_canon
        }
    } else {
        candidate.clone()
    };
    if !check_target.starts_with(&root_canon) {
        return Err(format!(
            "安全限制：路径超出工作区根目录 ({})",
            candidate.display()
        ));
    }
    Ok(candidate)
}

pub fn execute_workspace_tool(
    root: &Path,
    name: &str,
    args: &Value,
    ckpt_backups: &mut HashMap<PathBuf, Option<String>>,
    modified_inbox: &Arc<Mutex<Vec<PathBuf>>>,
) -> String {
    match name {
        "list_dir" => {
            let p_str = args["path"].as_str().unwrap_or(".");
            let dir = match resolve_safe_workspace_path(root, p_str) {
                Ok(d) => d,
                Err(e) => return e,
            };
            let Ok(rd) = fs::read_dir(&dir) else {
                return format!("Error: cannot read directory {}", dir.display());
            };
            let mut entries = Vec::new();
            for e in rd.flatten().take(120) {
                let name = e.file_name().to_string_lossy().to_string();
                if matches!(name.as_str(), ".git" | "target" | "node_modules") {
                    continue;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                entries.push(if is_dir { format!("{name}/") } else { name });
            }
            entries.sort();
            if entries.is_empty() {
                "(empty directory)".to_string()
            } else {
                entries.join("\n")
            }
        }
        "read_file" => {
            let p_str = args["path"].as_str().unwrap_or("");
            let path = match resolve_safe_workspace_path(root, p_str) {
                Ok(p) => p,
                Err(e) => return e,
            };
            let Ok(content) = fs::read_to_string(&path) else {
                return format!("Error: failed to read {}", path.display());
            };
            let lines: Vec<&str> = content.lines().collect();
            let start = args["start_line"]
                .as_u64()
                .map(|n| n.saturating_sub(1) as usize)
                .unwrap_or(0)
                .min(lines.len());
            let end = args["end_line"]
                .as_u64()
                .map(|n| n as usize)
                .unwrap_or((start + 200).min(lines.len()))
                .min(lines.len());
            let mut out = String::new();
            for (i, line) in lines[start..end].iter().enumerate() {
                out.push_str(&format!("{}: {}\n", start + i + 1, line));
            }
            if out.len() > 14000 {
                out.truncate(14000);
                out.push_str("\n...(truncated)");
            }
            out
        }
        "search_code" => {
            let query = args["query"].as_str().unwrap_or("").trim();
            if query.is_empty() {
                return "Error: empty search query".to_string();
            }
            let max_res = args["max_results"].as_u64().unwrap_or(20) as usize;
            let hits = find_in_files(root, query, max_res.min(40)).unwrap_or_default();
            if hits.is_empty() {
                return format!("No matches for `{query}`.");
            }
            hits.iter()
                .map(|h| {
                    let rel = h.path.strip_prefix(root).unwrap_or(&h.path).display();
                    format!("{rel}:{}: {}", h.line_number, h.line_text.trim())
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        "replace_in_file" => {
            let p_str = args["path"].as_str().unwrap_or("");
            let old_text = args["old_text"].as_str().unwrap_or("");
            let new_text = args["new_text"].as_str().unwrap_or("");
            let path = match resolve_safe_workspace_path(root, p_str) {
                Ok(p) => p,
                Err(e) => return e,
            };
            let Ok(content) = fs::read_to_string(&path) else {
                return format!("Error: failed to read {}", path.display());
            };
            let matches_cnt = content.matches(old_text).count();
            if matches_cnt == 0 {
                return "Error: old_text not found in file.".to_string();
            }
            if matches_cnt > 1 {
                return format!(
                    "Error: old_text matched {matches_cnt} times; provide more surrounding context."
                );
            }
            if crate::cli_runner::is_snapshot_eligible_path(&path)
                && crate::cli_runner::is_valid_text_snapshot(&content)
            {
                ckpt_backups
                    .entry(path.clone())
                    .or_insert_with(|| Some(content.clone()));
            }
            let updated = content.replacen(old_text, new_text, 1);
            match fs::write(&path, updated) {
                Ok(()) => {
                    modified_inbox.lock().push(path.clone());
                    format!("OK: replaced block in {}", path.display())
                }
                Err(e) => format!("Error writing {}: {e}", path.display()),
            }
        }
        "write_file" => {
            let p_str = args["path"].as_str().unwrap_or("");
            let content = args["content"].as_str().unwrap_or("");
            let path = match resolve_safe_workspace_path(root, p_str) {
                Ok(p) => p,
                Err(e) => return e,
            };
            if crate::cli_runner::is_snapshot_eligible_path(&path) {
                ckpt_backups
                    .entry(path.clone())
                    .or_insert_with(|| crate::cli_runner::read_text_snapshot_bounded(&path));
            }
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::write(&path, content) {
                Ok(()) => {
                    modified_inbox.lock().push(path.clone());
                    format!("OK: wrote {} ({} bytes)", path.display(), content.len())
                }
                Err(e) => format!("Error writing {}: {e}", path.display()),
            }
        }
        "run_command" => {
            let cmd_line = args["command"].as_str().unwrap_or("").trim();
            if cmd_line.is_empty() {
                return "Error: empty command".to_string();
            }
            #[cfg(target_os = "windows")]
            let mut cmd = {
                let mut c = Command::new("powershell.exe");
                c.args(["-NoProfile", "-Command", cmd_line]);
                c.creation_flags(CREATE_NO_WINDOW);
                c
            };
            #[cfg(not(target_os = "windows"))]
            let mut cmd = {
                let mut c = Command::new("sh");
                c.args(["-c", cmd_line]);
                c
            };
            cmd.current_dir(root);
            match cmd.output() {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let mut combined =
                        format!("Exit: {}\n{}\n{}", out.status, stdout.trim(), stderr.trim());
                    if combined.len() > 10000 {
                        combined.truncate(10000);
                        combined.push_str("\n...(truncated)");
                    }
                    combined
                }
                Err(e) => format!("Error executing command: {e}"),
            }
        }
        _ => format!("Error: unknown tool `{name}`"),
    }
}

pub fn build_patch_from_block(
    file_path: &Path,
    language: LanguageId,
    start_line: usize,
    end_line: usize,
    original_block: &str,
    proposed_code: String,
) -> ProposedFilePatch {
    let diff_lines = compute_line_diff(original_block, &proposed_code);
    let file_label = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    ProposedFilePatch {
        id: format!("patch-{}", &Uuid::new_v4().to_string()[..8]),
        file_path: file_path.to_path_buf(),
        language,
        start_line,
        end_line,
        summary: format!("{file_label} (L{}-L{})", start_line + 1, end_line + 1),
        original_code: original_block.to_string(),
        proposed_code,
        diff_lines,
        applied: false,
        dismissed: false,
    }
}

pub fn maybe_extract_patch_from_reply(
    reply: &str,
    mode: AgentWorkbenchMode,
    ctx: Option<&IdeCodeContext>,
) -> Option<ProposedFilePatch> {
    let c = ctx?;
    if mode == AgentWorkbenchMode::Ask && !reply.contains("```") {
        return None;
    }
    let code_block = extract_first_fenced_code_block(reply)?;
    let (start_line, end_line, original_block) =
        extract_enclosing_block(&c.file_text, c.cursor_line);
    if code_block.trim().is_empty() || code_block.trim() == original_block.trim() {
        return None;
    }
    Some(build_patch_from_block(
        &c.file_path,
        c.language,
        start_line,
        end_line,
        &original_block,
        code_block,
    ))
}

pub fn extract_first_fenced_code_block(text: &str) -> Option<String> {
    let mut in_fence = false;
    let mut buf = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !in_fence {
            if trimmed.starts_with("```") {
                in_fence = true;
            }
        } else if trimmed.starts_with("```") {
            if !buf.trim().is_empty() {
                return Some(buf);
            }
            in_fence = false;
            buf.clear();
        } else {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    None
}

/// 从编辑器全文中提取光标所在的函数 / struct / enum / impl 完整代码块（行范围 0-indexed inclusive）
pub fn extract_enclosing_block(text: &str, cursor_line: usize) -> (usize, usize, String) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return (0, 0, String::new());
    }
    let cur = cursor_line.min(lines.len() - 1);

    let mut start = cur;
    for i in (0..=cur).rev() {
        let trimmed = lines[i].trim_start();
        if trimmed.starts_with("pub fn ")
            || trimmed.starts_with("fn ")
            || trimmed.starts_with("pub async fn ")
            || trimmed.starts_with("async fn ")
            || trimmed.starts_with("pub struct ")
            || trimmed.starts_with("struct ")
            || trimmed.starts_with("pub enum ")
            || trimmed.starts_with("enum ")
            || trimmed.starts_with("impl ")
            || trimmed.starts_with("def ")
            || trimmed.starts_with("function ")
            || trimmed.starts_with("export function ")
        {
            start = i;
            break;
        }
        if cur - i >= 18 {
            start = cur.saturating_sub(3);
            break;
        }
    }

    let mut brace_depth: i32 = 0;
    let mut saw_brace = false;
    let mut end = (start + 8).min(lines.len() - 1);
    for (idx, line) in lines.iter().enumerate().skip(start).take(32) {
        for ch in line.chars() {
            if ch == '{' {
                brace_depth += 1;
                saw_brace = true;
            } else if ch == '}' {
                brace_depth -= 1;
            }
        }
        end = idx;
        if saw_brace && brace_depth <= 0 && idx >= cur {
            break;
        }
    }

    let slice = lines[start..=end].join("\n") + "\n";
    (start, end, slice)
}

/// 自动扫描并兼容项目根目录下的 `.cursorrules`、`CLAUDE.md`、`AGENTS.md`
pub fn load_workspace_agent_rules(root: &Path) -> String {
    let candidates = ["CLAUDE.md", "AGENTS.md", ".cursorrules"];
    let mut found = Vec::new();
    for file in candidates {
        let p = root.join(file);
        if p.exists() {
            if let Ok(content) = fs::read_to_string(&p) {
                found.push(format!("{} ({} bytes)", file, content.len()));
            }
        }
    }
    if found.is_empty() {
        "• Rules: Auto-watching `CLAUDE.md`, `AGENTS.md`, `.cursorrules`".to_string()
    } else {
        format!("• Rules: {}", found.join(", "))
    }
}
