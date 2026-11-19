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

/// 基于 Ripgrep (`grep-searcher` + `ignore`) 的毫秒级全局代码搜索 (Find in Path)
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

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(name.as_ref(), ".git" | "target" | "node_modules" | ".venv")
        })
        .build();

    for entry in walker.flatten() {
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

/// IDEA 标志性的 Search Everywhere (双击 Shift) 综合检索：同时检索文件、IDE Action 与 Agent 快捷指令
pub fn search_everywhere(root: &Path, query: &str, max_results: usize) -> Vec<SearchEverywhereItem> {
    let q = query.trim().to_ascii_lowercase();
    let mut items = Vec::new();

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
        let title = action.title();
        if q.is_empty() || title.to_ascii_lowercase().contains(&q) {
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
        if q.is_empty()
            || name.to_ascii_lowercase().contains(&q)
            || desc.to_ascii_lowercase().contains(&q)
        {
            items.push(SearchEverywhereItem::AgentCommand {
                agent_name: name.to_string(),
                description: desc.to_string(),
            });
        }
    }

    // 3. 匹配工作区文件
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(name.as_ref(), ".git" | "target" | "node_modules" | ".venv")
        })
        .build();

    for entry in walker.flatten() {
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
