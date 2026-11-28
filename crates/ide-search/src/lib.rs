use anyhow::Result;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::SearcherBuilder;
use ide_core::{IdeAction, KeymapScheme, LanguageId};
use ide_syntax::{extract_structure_symbols, find_symbol_definition, SymbolKind};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use std::fs;
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
    Symbol {
        name: String,
        kind: SymbolKind,
        line: usize,
        relative_path: String,
        full_path: PathBuf,
        language: LanguageId,
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
pub struct IndexedWorkspaceSymbol {
    pub name: String,
    pub name_lower: String,
    pub kind: SymbolKind,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFileEntry {
    pub name: String,
    pub name_lower: String,
    pub relative_path: String,
    pub relative_path_lower: String,
    pub full_path: PathBuf,
    pub language: LanguageId,
    pub symbols: Vec<IndexedWorkspaceSymbol>,
}

const MAX_INDEXED_WORKSPACE_FILES: usize = 50_000;
const MAX_SYMBOL_INDEXED_FILES: usize = 4_000;
const MAX_SYMBOL_INDEX_FILE_BYTES: u64 = 512 * 1024;

fn build_workspace_walker(root: &Path) -> ignore::Walk {
    WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                ".git"
                    | "target"
                    | "node_modules"
                    | ".venv"
                    | "venv"
                    | "__pycache__"
                    | "dist"
                    | "build"
                    | "out"
                    | ".next"
                    | ".nuxt"
                    | ".gradle"
                    | ".dart_tool"
                    | ".cache"
                    | ".turbo"
                    | "coverage"
            )
        })
        .build()
}

fn extract_indexed_symbols_for_file(
    path: &Path,
    language: LanguageId,
) -> Vec<IndexedWorkspaceSymbol> {
    if !language.supports_symbol_indexing() {
        return Vec::new();
    }
    let Ok(meta) = fs::metadata(path) else {
        return Vec::new();
    };
    if meta.len() == 0 || meta.len() > MAX_SYMBOL_INDEX_FILE_BYTES {
        return Vec::new();
    }
    let Ok(source) = fs::read_to_string(path) else {
        return Vec::new();
    };
    extract_structure_symbols(&source, language)
        .into_iter()
        .map(|s| IndexedWorkspaceSymbol {
            name_lower: s.name.to_ascii_lowercase(),
            name: s.name,
            kind: s.kind,
            line: s.line,
        })
        .collect()
}

/// 一次性扫描工作区文件路径与多语言符号大纲并建立内存索引，供 Search Everywhere、Go to Declaration 与 Find in Files 秒级复用
pub fn index_workspace_files(root: &Path) -> Vec<WorkspaceFileEntry> {
    let mut files = Vec::new();
    let mut symbol_indexed_count = 0usize;

    for entry in build_workspace_walker(root).flatten() {
        if files.len() >= MAX_INDEXED_WORKSPACE_FILES {
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
        let name_lower = name.to_ascii_lowercase();
        let relative_path_lower = rel.to_ascii_lowercase();
        let language = LanguageId::from_path(path);
        let symbols = if symbol_indexed_count < MAX_SYMBOL_INDEXED_FILES
            && language.supports_symbol_indexing()
        {
            let syms = extract_indexed_symbols_for_file(path, language);
            if !syms.is_empty() {
                symbol_indexed_count += 1;
            }
            syms
        } else {
            Vec::new()
        };

        files.push(WorkspaceFileEntry {
            name,
            name_lower,
            relative_path: rel,
            relative_path_lower,
            full_path: path.to_path_buf(),
            language,
            symbols,
        });
    }
    files.shrink_to_fit();
    files
}

/// 基于内存工作区符号索引在跨文件中秒级定位符号声明位置，返回 `(file_path, 0-based line, 0-based col)`
pub fn find_workspace_symbol_definition(
    files: &[WorkspaceFileEntry],
    symbol: &str,
    exclude_path: Option<&Path>,
    preferred_lang: Option<LanguageId>,
) -> Option<(PathBuf, usize, usize)> {
    let target = symbol.trim();
    if target.is_empty() {
        return None;
    }

    let mut best_hit: Option<(i32, &PathBuf, LanguageId, usize)> = None;
    for file in files {
        if exclude_path.is_some_and(|ex| ex == file.full_path) {
            continue;
        }
        for sym in &file.symbols {
            if sym.name == target || sym.name.trim_start_matches('#') == target {
                let mut score = 100;
                if preferred_lang.is_some_and(|l| l == file.language) {
                    score += 50;
                }
                if matches!(
                    sym.kind,
                    SymbolKind::Struct
                        | SymbolKind::Class
                        | SymbolKind::Enum
                        | SymbolKind::Trait
                        | SymbolKind::Interface
                        | SymbolKind::Function
                ) {
                    score += 20;
                }
                if best_hit.as_ref().is_none_or(|(best_s, ..)| score > *best_s) {
                    best_hit = Some((score, &file.full_path, file.language, sym.line));
                }
            }
        }
    }

    if let Some((_, path, lang, line_idx)) = best_hit {
        let col = fs::read_to_string(path)
            .ok()
            .and_then(|src| {
                src.lines()
                    .nth(line_idx)
                    .and_then(|l| l.find(target).map(|b| l[..b].chars().count()))
                    .or_else(|| find_symbol_definition(&src, lang, target).map(|(_, c)| c))
            })
            .unwrap_or(0);
        return Some((path.clone(), line_idx, col));
    }

    None
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

fn push_actions_and_agents(
    q: &str,
    max_results: usize,
    keymap: KeymapScheme,
    items: &mut Vec<SearchEverywhereItem>,
) {
    // 1. 匹配 IDE Actions
    let actions = [
        IdeAction::RunActiveConfig,
        IdeAction::RunCurrentFile,
        IdeAction::DebugActiveConfig,
        IdeAction::CheckMobileDevicesAndSdk,
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
                shortcut: action.shortcut_label_for(keymap).to_string(),
            });
        }
    }

    // 2. 匹配 Multi-Agent 快捷入口
    let agents = [
        ("Google Antigravity (ACP)", "Switch to Antigravity ACP Agent Session"),
        ("Claude Code CLI", "Invoke local Claude Code Agent (claude.cmd)"),
        ("OpenAI Codex CLI", "Invoke local OpenAI Codex Agent (codex.cmd)"),
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

/// 基于内存文件与符号索引执行零磁盘 IO 的 Search Everywhere 检索 (<0.1ms)
pub fn search_everywhere_indexed(
    files: &[WorkspaceFileEntry],
    query: &str,
    max_results: usize,
) -> Vec<SearchEverywhereItem> {
    search_everywhere_indexed_with_keymap(files, query, max_results, KeymapScheme::IntelliJIdea)
}

/// 支持按当前快捷键方案渲染动作快捷键标签的 Search Everywhere 检索
pub fn search_everywhere_indexed_with_keymap(
    files: &[WorkspaceFileEntry],
    query: &str,
    max_results: usize,
    keymap: KeymapScheme,
) -> Vec<SearchEverywhereItem> {
    let q = query.trim().to_ascii_lowercase();
    let mut items = Vec::with_capacity(max_results);
    push_actions_and_agents(&q, max_results, keymap, &mut items);

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

    // 当用户输入查询词时，同步检索全工作区已索引的代码符号（类、结构体、函数、接口、枚举、常量）
    if !q.is_empty() && items.len() < max_results {
        let mut symbol_matches: Vec<(i32, SearchEverywhereItem)> = Vec::new();
        for file in files {
            for sym in &file.symbols {
                if sym.name_lower.contains(&q) {
                    let score = if sym.name_lower == q {
                        300
                    } else if sym.name_lower.starts_with(&q) {
                        200
                    } else {
                        100
                    };
                    symbol_matches.push((
                        score,
                        SearchEverywhereItem::Symbol {
                            name: sym.name.clone(),
                            kind: sym.kind,
                            line: sym.line + 1,
                            relative_path: file.relative_path.clone(),
                            full_path: file.full_path.clone(),
                            language: file.language,
                        },
                    ));
                }
            }
        }
        symbol_matches.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, item) in symbol_matches {
            if items.len() >= max_results {
                break;
            }
            items.push(item);
        }
    }

    items
}

/// IDEA 标志性的 Search Everywhere (双击 Shift) 综合检索：同时检索文件、符号、IDE Action 与 Agent 快捷指令
pub fn search_everywhere(root: &Path, query: &str, max_results: usize) -> Vec<SearchEverywhereItem> {
    let indexed = index_workspace_files(root);
    search_everywhere_indexed(&indexed, query, max_results)
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
        assert_eq!(files[0].symbols.len(), 1);
        assert_eq!(files[0].symbols[0].name, "fast_search_symbol");

        let se = search_everywhere_indexed(&files, "demo", 10);
        assert!(se
            .iter()
            .any(|item| matches!(item, SearchEverywhereItem::File { name, .. } if name == "demo.rs")));

        let sym_se = search_everywhere_indexed(&files, "fast_search", 10);
        assert!(sym_se.iter().any(|item| matches!(
            item,
            SearchEverywhereItem::Symbol { name, line: 1, .. } if name == "fast_search_symbol"
        )));

        let def = find_workspace_symbol_definition(&files, "fast_search_symbol", None, Some(LanguageId::Rust));
        assert!(def.is_some());

        let hits = find_in_files_indexed(&files, "fast_search_symbol", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line_number, 1);
        let _ = fs::remove_dir_all(&tmp);
    }
}
