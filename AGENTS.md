# Rust AI IDE — Multi-Agent Workspace Instructions (`AGENTS.md`)

- **Architecture**: 100% Pure Rust IDE (`D:\work\rust-ai-ide`) replicating the **JetBrains IntelliJ IDEA (New UI / Islands Dark)** layout and keymap.
- **First-Tier Languages**:
  - **Rust**: `rust-analyzer` (LSP) + `codelldb` (DAP)
  - **TypeScript / JavaScript**: `vtsls` / `typescript-language-server` (LSP) + `js-debug` (DAP)
  - **Python**: `pyright-langserver` / `ruff` (LSP) + `debugpy` (DAP)
- **First-Tier Agents**:
  - **Google Antigravity**: ACP (Agent Client Protocol) JSON-RPC 2.0 over Stdio (`crates/ide-agent-hub`)
  - **Claude Code CLI**: `claude.cmd` headless Stream-JSON & embedded PTY
  - **OpenAI Codex CLI**: `codex.cmd` exec & embedded PTY
  - **Cursor Engine**: `Ctrl+K` Inline AI Edit with `similar` Myers/Patience Hunk Diffs and atomic Workspace Checkpoints
