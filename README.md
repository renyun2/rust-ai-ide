<div align="center">
  <img src="assets/ri_icon.png" alt="RI logo" width="120" height="120" />

  <h1>RI — Pure Rust AI IDE</h1>

  <p>A native desktop IDE written entirely in Rust.<br/>IntelliJ IDEA New UI layout and keymap, with local coding agents in the same window.</p>

  <p>
    <a href="README.md">English</a> · <a href="README.zh-CN.md">简体中文</a>
  </p>

  <p>
    <img alt="Rust 2021" src="https://img.shields.io/badge/Rust-2021-orange?logo=rust&logoColor=white" />
    <img alt="Version 0.1.0" src="https://img.shields.io/badge/version-0.1.0-blue" />
    <img alt="Platform Windows" src="https://img.shields.io/badge/platform-Windows_10%2F11-0078D4?logo=windows&logoColor=white" />
    <img alt="License" src="https://img.shields.io/badge/license-MIT_OR_Apache--2.0-green" />
  </p>
</div>

RI (**R**ust **I**DE) is an early native workbench for Rust, TypeScript, JavaScript, and Python. It draws its own UI, edits text on a rope, and talks to language servers, debuggers, Git, a real terminal, and the coding-agent CLIs already installed on the machine.

The desktop target today is **Windows 10/11**. Language servers and agent CLIs are discovered locally. RI does not bundle a JVM, a browser, or those tools.

---

## Contents

- [The problem](#the-problem)
- [Why RI](#why-ri)
- [Features](#features)
- [Architecture](#architecture)
- [Performance](#performance)
- [Keymap](#keymap)
- [Requirements](#requirements)
- [Getting started](#getting-started)
- [Development](#development)
- [Releases](#releases)
- [Project layout](#project-layout)
- [Status](#status)
- [Brand assets](#brand-assets)
- [License](#license)

---

## The problem

Daily coding still splits across a heavy editor and a pile of separate agent terminals.

| Pain | What it costs |
| --- | --- |
| **Browser-shell IDEs** | VS Code and Cursor ship Chromium and Node. The install is large, memory is split across renderer and extension processes, and the UI is a web page hosted in a native frame. |
| **JVM IDEs** | IntelliJ IDEA ships a full JetBrains Runtime plus a large JAR and plugin graph. Cold start and heap behavior are part of the edit loop. |
| **Agents outside the editor** | Antigravity, Claude Code, Codex, Cursor, and Gemini each have their own CLI or product. Diffs, file context, and rollback do not share one workbench. |
| **High-DPI redraw** | A software-rendered IDE stutters on 2K and 4K displays when every scrolled frame calls the text engine again. |
| **Keymap tax** | People who work in the IntelliJ keymap pay a switching cost in every other editor, including the AI tools they open beside it. |

RI is the attempt to put that loop back in one small native process: IDEA-shaped UI, the languages you already use, and the local agents you already installed.

---

## Why RI

- **One native process.** The window, editor, tool windows, and agent panel are Rust. A stripped release binary of `ide-app` has been measured at **6.04 MB** (`6,335,488` bytes) with thin LTO. There is no bundled JBR and no bundled Chromium.
- **IDEA layout without the JVM.** Islands Dark, Darcula Classic, and Islands Light, the New UI tool stripes, and the IntelliJ keymap live in `ide-core`. The UI language switches between English and Chinese with `Ctrl+Alt+U`.
- **Agents share the workbench.** `ide-agent-hub` drives local CLIs (Cursor, Antigravity ACP, Claude Code, Codex, Gemini) and a `Ctrl+K` inline edit path. Proposed edits are hunk diffs. A workspace checkpoint can be rolled back with `Ctrl+Alt+Z`.
- **The language loop is explicit.** Rust, TypeScript/JavaScript, and Python are first-tier: LSP for intelligence, DAP for debug, Git for review, and ConPTY for the shell. Each server is the one on your `PATH`, not a copy vendored into RI.
- **Scrolling is a rendering problem, treated as one.** Glyph masks, highlight spans, and text measurements are cached against the buffer revision. Frames are written straight into a `softbuffer` surface.

---

## Features

### Workbench

- Three themes, switched with `` Ctrl+` `` or from Settings (`Ctrl+Alt+S`): **JetBrains Dark (Islands)**, **Darcula Classic**, **IntelliJ Light (Islands)**.
- System DPI detection on startup (`100%`–`400%`) and live UI zoom with `Ctrl+=`, `Ctrl+-`, and `Ctrl+0`.
- Borderless window, Windows 11 rounded corners, workspace switcher (this window or a new window), and a Git branch popup.
- Gutter width follows the line count, so five- and six-digit files keep their line numbers. Breakpoints and runnable gutter marks sit in that same gutter.
- Project, Git, and agent work run off the UI thread. Loading states use a small arc spinner.

### Editor and search

- `ropey` text buffer with undo/redo, multi-caret style edits, word selection, indent, and dirty-state tracking.
- **Search Everywhere** (`Double Shift`), **Go to File** (`Ctrl+Shift+N`), and **Find in Files** (`Ctrl+Shift+F`) on the ripgrep stack (`ignore`, `grep-searcher`, `grep-regex`).
- Syntax color and structure for Rust, TypeScript, JavaScript, and Python. **Structure** is `Alt+7`.

### Languages, debug, Git, terminal

| Language | Language server | Debugger |
| --- | --- | --- |
| Rust | `rust-analyzer` | `codelldb` |
| TypeScript / JavaScript | `vtsls` or `typescript-language-server` | `js-debug` |
| Python | `pyright-langserver` or `ruff` | `debugpy` |

- **Git** (`Alt+0`): change list with stage and unstage, inline line diffs (`similar`), commit message, and a three-pane log (branch filter, graph, file diff). Repository work is done on a background worker.
- **Terminal** (`Alt+F12`): `portable-pty` on Windows ConPTY, multiple tabs, VT100, and ANSI color. PowerShell, Cargo, and Git run as normal console programs.

### Agents

`ide-agent-hub` is the single place those tools show up:

| Provider | How RI talks to it |
| --- | --- |
| Google Antigravity | ACP, JSON-RPC 2.0 over stdio (`agy`) |
| Claude Code | `claude` headless stream-JSON and an embedded PTY |
| OpenAI Codex | `codex` exec and an embedded PTY |
| Cursor | Local CLI, plus `Ctrl+K` inline edit |
| Gemini | Local `gemini` CLI |
| Custom endpoint | OpenAI-compatible HTTP fallback |

The agent panel keeps multiple sessions, accepts dropped or pasted images, folds fenced code into chips, and can jump to a file and line from the reply. Thinking effort and a fast mode are session settings. Inline edits use Myers and Patience hunks. Accept with `Tab`, reject with `Esc`.

---

## Architecture

Ten crates. UI state stays in `ide-app`. Protocols and buffers stay in the libraries.

| Crate | Role |
| --- | --- |
| [`ide-core`](crates/ide-core) | Themes, `LanguageId`, run-configuration detection, actions, keymap, English/Chinese menu strings |
| [`ide-vfs`](crates/ide-vfs) | `ropey` buffer, undo snapshots, selections, workspace file tree |
| [`ide-syntax`](crates/ide-syntax) | Highlight spans, structure symbols, runnable gutter detection |
| [`ide-search`](crates/ide-search) | Workspace index, Search Everywhere, Find in Files |
| [`ide-lsp`](crates/ide-lsp) | LSP JSON-RPC client and local server discovery |
| [`ide-dap`](crates/ide-dap) | DAP session, breakpoints, stack frames, crash context for agents |
| [`ide-git`](crates/ide-git) | Status, stage, commit, branches, log, line diffs |
| [`ide-terminal`](crates/ide-terminal) | ConPTY session and VT100 parser |
| [`ide-agent-hub`](crates/ide-agent-hub) | Agent registry, sessions, ACP, CLI runners, inline edit, checkpoints |
| [`ide-app`](crates/ide-app) | `tao` event loop and the DirectWrite + tiny-skia + softbuffer renderer |

```text
tao window
    └── ide-app          UI, input, frame
            ├── ide-core         theme, keymap, language id
            ├── ide-vfs          rope buffer, file tree
            ├── ide-syntax       color, structure
            ├── ide-search       ripgrep index
            ├── ide-lsp / ide-dap
            ├── ide-git
            ├── ide-terminal     ConPTY
            └── ide-agent-hub    local CLIs + inline diff
```

---

## Performance

The renderer keeps five caches and shortcuts on the hot path:

1. **Glyph masks.** DirectWrite ClearType coverage is rasterized once into a compact mask and reused with integer blends.
2. **Revision-tied spans.** Highlight spans and measured widths are cached against the buffer revision and reused while scrolling.
3. **Scanline fills.** Common rounded rectangles are filled per scanline with a small SDF corner, instead of rebuilding a path every frame.
4. **Direct present.** The frame is copied once into `softbuffer`. There is no second full-screen intermediate buffer.
5. **Coalesced wheel input.** Fast scrolling is accumulated and stepped inside the frame budget.

Local scroll check `fast_scrolling_and_glyph_mask_cache_achieve_high_fps`: 20 full frames of a 1560×940 IDE surface while the code view scrolls. Same machine throughout. These numbers describe this renderer. They are not a product-wide benchmark against IDEA or VS Code.

| Build | 20 frames | Per frame | Equivalent rate | Versus the uncached baseline |
| --- | ---: | ---: | ---: | ---: |
| Baseline, DirectWrite called every frame, no glyph-mask cache | ~1191 ms | ~59.6 ms | ~17 FPS | 1.0× |
| After the cache, debug profile (`opt-level = 2`) | 57.49 ms | 2.87 ms | ~348 FPS | ~20.7× |
| After the cache, release (`opt-level = 3`, thin LTO) | 33.43 ms | 1.67 ms | ~598 FPS | ~35.6× |

| | RI | IntelliJ IDEA | VS Code / Cursor |
| --- | --- | --- | --- |
| Shipping shape | One stripped native binary, measured around 6 MB | JBR plus the IDE distribution | Chromium and Node |
| UI | `tao`, `tiny-skia`, `softbuffer`, DirectWrite | Swing on a JetBrains runtime | Blink, CSS, Skia |
| Text | `ropey` on a Rust thread | JVM heap | Editor core in the extension host plus the renderer process |
| Terminal | `portable-pty` / ConPTY | Pty4j | `node-pty` |

---

## Keymap

IntelliJ IDEA bindings. On Windows, `Ctrl` is the primary modifier.

| Keys | Action | Keys | Action |
| --- | --- | --- | --- |
| `Double Shift` | Search Everywhere | `Ctrl+Alt+S` | Settings |
| `Ctrl+Shift+F` | Find in Files | `` Ctrl+` `` | Cycle theme |
| `Ctrl+Shift+A` | Find Action | `Ctrl+Alt+U` | English / 中文 |
| `Ctrl+Shift+N` | Go to File | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Zoom in, out, reset |
| `Ctrl+B` | Go to declaration | `Ctrl+K` | Inline AI edit |
| `Ctrl+D` / `Ctrl+Y` | Duplicate line / delete line | `Alt+L` | Send selection to the agent panel |
| `Ctrl+/` | Toggle line comment | `Tab` / `Esc` | Accept / reject inline diff |
| `Ctrl+Alt+L` | Reformat | `Ctrl+Alt+Z` | Revert the latest AI checkpoint |
| `Shift+F10` / `Shift+F9` | Run / debug | `Alt+1` | Project |
| `Ctrl+F8` | Toggle breakpoint | `Alt+0` | Commit |
| `Ctrl+T` / `Ctrl+Shift+K` | Git pull / push | `Alt+7` / `Alt+8` / `Alt+F12` | Structure / Agents / Terminal |

---

## Requirements

- Rust stable, 2021 edition (`rustup` default is enough)
- Windows 10 or Windows 11 (DirectWrite, ConPTY, and DWM rounded corners)
- Optional, installed separately and picked up from `PATH`:
  - Language servers: `rust-analyzer`, `vtsls` or `typescript-language-server`, `pyright-langserver` or `ruff`
  - Debuggers: `codelldb`, `js-debug`, `debugpy`
  - Agents: `agy`, `claude`, `codex`, Cursor CLI, `gemini`

---

## Getting started

```powershell
# Open the current directory as the workspace
cargo run -p ide-app --release

# Open a specific project
cargo run -p ide-app --release -- --workspace D:\path\to\your\project

# Headless UI self-check: render a frame and print hit-target stats
cargo run -p ide-app --release -- --self-check

# Write the 256×256 brand icon from the in-app vector logo
cargo run -p ide-app --release -- --export-icon assets/ri_icon.png
```

| Flag | Effect |
| --- | --- |
| `--workspace <path>` | Open this directory instead of the current one |
| `--self-check` | Render off-screen and exit. Useful in CI and local smoke tests |
| `--export-icon <path>` | Rasterize the RI mark to a PNG and exit |

---

## Development

```powershell
cargo check --workspace
cargo test --workspace
cargo run -p ide-app -- --self-check
```

Release profile (`Cargo.toml`): `opt-level = 3`, thin LTO, a single codegen unit, and stripped symbols. The dev profile opts dependencies at level 3 so the UI is usable without a full release build.

Issues and pull requests are welcome. Please run `cargo test --workspace` and `cargo run -p ide-app -- --self-check` before sending a change. Match the surrounding crate style. Keep protocol code in its crate (`ide-lsp`, `ide-dap`, `ide-agent-hub`) and window code in `ide-app`.

---

## Releases

GitHub Actions builds the Windows release when a version tag is pushed. The workflow file has to be in that commit. Tagging an older commit that does not contain `.github/workflows/release.yml` does not start a build.

```powershell
git tag v0.1.0
git push origin v0.1.0
```

[`.github/workflows/release.yml`](.github/workflows/release.yml) then runs on `windows-latest`:

1. `cargo test --workspace`
2. `cargo build -p ide-app --release`
3. `ide-app.exe --self-check`
4. Publish a GitHub Release with `RI-v0.1.0-windows-x86_64.zip` and a SHA-256 file

A tag that contains a hyphen, such as `v0.2.0-rc.1`, is published as a pre-release. Pushing to `master` or `main`, and opening a pull request, runs tests only ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)).

On a private repository, allow Actions to create releases: **Settings → Actions → General → Workflow permissions → Read and write permissions**.

---

## Project layout

```text
rust-ai-ide/
├── crates/
│   ├── ide-core/
│   ├── ide-vfs/
│   ├── ide-syntax/
│   ├── ide-search/
│   ├── ide-lsp/
│   ├── ide-dap/
│   ├── ide-git/
│   ├── ide-terminal/
│   ├── ide-agent-hub/
│   └── ide-app/
├── .github/workflows/      # ci.yml, release.yml
├── assets/                 # ri_icon.svg, ri_icon.png
├── Cargo.toml              # workspace, version 0.1.0
├── README.md               # this file
└── README.zh-CN.md
```

---

## Status

Version **0.1.0**. The workbench, editor, search, Git tool window, terminal, and agent hub run on Windows. LSP and DAP clients expect the corresponding tools on `PATH`. Other desktop platforms are not a supported target yet: the text stack is DirectWrite and the terminal stack is ConPTY.

This repository follows the IntelliJ New UI closely in layout, color, and keymap. It is an independent implementation.

---

## Brand assets

The mark is drawn at runtime by `draw_ri_logo_to_pixmap` in [`crates/ide-app/src/canvas.rs`](crates/ide-app/src/canvas.rs) and also stored in the repo:

- Vector: [`assets/ri_icon.svg`](assets/ri_icon.svg)
- 256×256 PNG: [`assets/ri_icon.png`](assets/ri_icon.png)

---

## License

Licensed under either of

- Apache License, Version 2.0
- MIT license

at your option. The choice is recorded on the workspace package in [`Cargo.toml`](Cargo.toml) as `MIT OR Apache-2.0`.
