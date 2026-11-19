use anyhow::Result;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::SearcherBuilder;
use ide_core::IdeAction;
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentSearchHit {
    pub path: PathBuf,
    pub line_number: usize,
    pub line_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SearchEverywhereItem {
    File {
        name: String,
        relative_path: String,
        full_path: PathBuf,
    },
    Action {
        action: IdeAction,
        title: String,
        shortcut: String,
    },
    AgentCommand {
        agent_name: String,
        description: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFileEntry {
    pub name: String,
    pub name_lower: String,
    pub relative_path: String,
    pub relative_path_lower: String,
    pub full_path: PathBuf,
}

fn build_workspace_walker(root: &Path) -> ignore::Walk {
    WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(name.as_ref(), ".git" | "target" | "node_modules" | ".venv")
        })
        .build()
}

/// 一次性扫描工作区文件路径并建立内存索引，供 Search Everywhere 与 Find in Files 秒级复用
pub fn index_workspace_files(root: &Path) -> Vec<WorkspaceFileEntry> {
    let mut files = Vec::new();
    for entry in build_workspace_walker(root).flatten() {
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        let name_lower = name.to_ascii_lowercase();
        let relative_path_lower = rel.to_ascii_lowercase();
        files.push(WorkspaceFileEntry {
            name,
            name_lower,
            relative_path: rel,
            relative_path_lower,
            full_path: path.to_path_buf(),
        });
    }
    files
}

/// 基于 Ripgrep (`grep-searcher` + `ignore`) 的全局代码搜索 (Find in Path)
pub fn find_in_files(root: &Path, pattern: &str, max_hits: usize) -> Result<Vec<ContentSearchHit>> {
    if pattern.trim().is_empty() {
        return Ok(Vec::new());
    }

    let matcher = RegexMatcherBuilder::new()
        .case_smart(true)
        .fixed_strings(true)
        .build(pattern)?;

    let mut searcher = SearcherBuilder::new().line_number(true).build();
    let mut hits = Vec::new();

    for entry in build_workspace_walker(root).flatten() {
        if hits.len() >= max_hits {
            break;
        }
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path().to_path_buf();
        let _ = searcher.search_path(
            &matcher,
            &path,
            UTF8(|line_num, line| {
                if hits.len() < max_hits {
                    hits.push(ContentSearchHit {
                        path: path.clone(),
                        line_number: line_num as usize,
                        line_text: line.trim_end().to_string(),
                    });
                }
                Ok(hits.len() < max_hits)
            }),
        );
    }

    Ok(hits)
}

/// 基于内存文件索引的快速内容搜索，避免每次击键重复遍历目录树
pub fn find_in_files_indexed(
    files: &[WorkspaceFileEntry],
    pattern: &str,
    max_hits: usize,
) -> Result<Vec<ContentSearchHit>> {
    if pattern.trim().is_empty() {
        return Ok(Vec::new());
    }

    let matcher = RegexMatcherBuilder::new()
        .case_smart(true)
        .fixed_strings(true)
        .build(pattern)?;

    let mut searcher = SearcherBuilder::new().line_number(true).build();
    let mut hits = Vec::new();

    for file in files {
        if hits.len() >= max_hits {
            break;
        }
        let path = &file.full_path;
        let _ = searcher.search_path(
            &matcher,
            path,
            UTF8(|line_num, line| {
                if hits.len() < max_hits {
                    hits.push(ContentSearchHit {
                        path: path.clone(),
                        line_number: line_num as usize,
                        line_text: line.trim_end().to_string(),
                    });
                }
                Ok(hits.len() < max_hits)
            }),
        );
    }

    Ok(hits)
}

fn push_actions_and_agents(q: &str, max_results: usize, items: &mut Vec<SearchEverywhereItem>) {
    // 1. 匹配 IDE Actions
    let actions = [
        IdeAction::InlineAiEdit,
        IdeAction::SendSelectionToAgent,
        IdeAction::ShowIntentionActions,
        IdeAction::RunActiveConfig,
        IdeAction::DebugActiveConfig,
        IdeAction::ToggleBreakpoint,
        IdeAction::SaveActiveFile,
        IdeAction::ToggleProjectWindow,
        IdeAction::ToggleCommitWindow,
        IdeAction::ToggleStructureWindow,
        IdeAction::ToggleAgentHubWindow,
        IdeAction::ToggleTerminalWindow,
    ];

    for action in actions {
        if items.len() >= max_results {
            return;
        }
        let title = action.title();
        if q.is_empty() || title.to_ascii_lowercase().contains(q) {
            items.push(SearchEverywhereItem::Action {
                action,
                title: title.to_string(),
                shortcut: action.shortcut_label().to_string(),
            });
        }
    }

    // 2. 匹配 Multi-Agent 快捷入口
    let agents = [
        ("Google Antigravity (ACP)", "Switch to Antigravity ACP Agent Session"),
        ("Claude Code CLI", "Invoke local Claude Code Agent (claude.cmd)"),
        ("OpenAI Codex CLI", "Invoke local OpenAI Codex Agent (codex.cmd)"),
        ("Cursor Ctrl+K Inline Edit", "Edit current selection inline with Hunk Diff"),
    ];
    for (name, desc) in agents {
        if items.len() >= max_results {
            return;
        }
        if q.is_empty()
            || name.to_ascii_lowercase().contains(q)
            || desc.to_ascii_lowercase().contains(q)
        {
            items.push(SearchEverywhereItem::AgentCommand {
                agent_name: name.to_string(),
                description: desc.to_string(),
            });
        }
    }
}

/// 基于内存文件索引执行零磁盘 IO 的 Search Everywhere 检索 (<0.1ms)
pub fn search_everywhere_indexed(
    files: &[WorkspaceFileEntry],
    query: &str,
    max_results: usize,
) -> Vec<SearchEverywhereItem> {
    let q = query.trim().to_ascii_lowercase();
    let mut items = Vec::with_capacity(max_results);
    push_actions_and_agents(&q, max_results, &mut items);

    for file in files {
        if items.len() >= max_results {
            break;
        }
        if q.is_empty() || file.name_lower.contains(&q) || file.relative_path_lower.contains(&q) {
            items.push(SearchEverywhereItem::File {
                name: file.name.clone(),
                relative_path: file.relative_path.clone(),
                full_path: file.full_path.clone(),
            });
        }
    }

    items
}

/// IDEA 标志性的 Search Everywhere (双击 Shift) 综合检索：同时检索文件、IDE Action 与 Agent 快捷指令
pub fn search_everywhere(root: &Path, query: &str, max_results: usize) -> Vec<SearchEverywhereItem> {
    let q = query.trim().to_ascii_lowercase();
    let mut items = Vec::new();
    push_actions_and_agents(&q, max_results, &mut items);

    // 3. 匹配工作区文件
    for entry in build_workspace_walker(root).flatten() {
        if items.len() >= max_results {
            break;
        }
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        if q.is_empty()
            || name.to_ascii_lowercase().contains(&q)
            || rel.to_ascii_lowercase().contains(&q)
        {
            items.push(SearchEverywhereItem::File {
                name,
                relative_path: rel,
                full_path: path.to_path_buf(),
            });
        }
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_indexed_search_everywhere_and_find_in_files() {
        let tmp = std::env::temp_dir().join("rust_ai_ide_search_index_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("src")).unwrap();
        fs::write(
            tmp.join("src/demo.rs"),
            "pub fn fast_search_symbol() -> usize { 42 }\n",
        )
        .unwrap();

        let files = index_workspace_files(&tmp);
        assert_eq!(files.len(), 1);
        let se = search_everywhere_indexed(&files, "demo", 10);
        assert!(se
            .iter()
            .any(|item| matches!(item, SearchEverywhereItem::File { name, .. } if name == "demo.rs")));
        let hits = find_in_files_indexed(&files, "fast_search_symbol", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line_number, 1);
        let _ = fs::remove_dir_all(&tmp);
    }
}

