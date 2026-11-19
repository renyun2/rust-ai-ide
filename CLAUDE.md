# CLAUDE.md — Project Context for Claude Code

## Commands
- **Check Workspace**: `cargo check --workspace`
- **Run All Tests**: `cargo test --workspace`
- **Self-Check IDE**: `cargo run -p ide-app -- --self-check`
- **Launch Desktop IDE**: `cargo run -p ide-app`

## Workspace Crates
- `crates/ide-core`: JetBrains IDEA New UI theme (`IdeaTheme`), `LanguageId` (`Rust`, `TypeScript`, `JavaScript`, `Python`), `RunConfiguration` detector, `IdeAction` keymap.
- `crates/ide-vfs`: `ropey::Rope` backed `TextBuffer` with undo/redo and workspace file tree scanner.
- `crates/ide-syntax`: IDEA Darcula syntax highlighter, `StructureSymbol` extractor (`Alt+7`), and runnable gutter entry detector (`▶`).
- `crates/ide-search`: Ripgrep-powered `find_in_files` and IDEA `search_everywhere` (Double `Shift` / `Ctrl+P`).
- `crates/ide-lsp`: Multi-language LSP hub (`rust-analyzer`, `vtsls`, `pyright-langserver`) & JSON-RPC framing.
- `crates/ide-dap`: DAP debugger state machine (`codelldb`, `js-debug`, `debugpy`), breakpoints, stack frames, and AI crash context export.
- `crates/ide-git`: Git status/branch/log integration and `similar` line diff calculator.
- `crates/ide-terminal`: `portable-pty` terminal session runner & ANSI sequence parser.
- `crates/ide-agent-hub`: Multi-Agent orchestrator for Google Antigravity (ACP), Claude Code CLI, OpenAI Codex CLI, and Cursor `Ctrl+K` Inline Diff with Checkpoints.
- `crates/ide-app`: 100% Pure Rust desktop window & 2D surface renderer replicating JetBrains IDEA (New UI).
