<div align="center">
  <img src="assets/ri_icon.png" alt="RI logo" width="120" height="120" />

  <h1>RI — Pure Rust AI IDE</h1>

  <p>A native desktop IDE for Rust, TypeScript, JavaScript, Python, and Flutter.<br/>IntelliJ IDEA New UI layout and keymap, with local coding agents in one lightweight native process.</p>

  <p>
    <a href="README.md">English</a> · <a href="README.zh-CN.md">简体中文</a>
  </p>

  <p>
    <img alt="Rust 2021" src="https://img.shields.io/badge/Rust-2021-orange?logo=rust&logoColor=white" />
    <img alt="Version 0.2.0" src="https://img.shields.io/badge/version-0.2.0-blue" />
    <img alt="Platforms Windows | Linux | macOS" src="https://img.shields.io/badge/platform-Windows_%7C_Linux_%7C_macOS-0078D4" />
    <img alt="License" src="https://img.shields.io/badge/license-MIT_OR_Apache--2.0-green" />
  </p>
</div>

RI draws its own UI, edits text on a `ropey` buffer, and connects to the language servers, debuggers, Git, terminal, and coding-agent CLIs already installed on your machine. There is no bundled JVM, Chromium, Electron, or Node.js runtime.

---

## Contents

- [Highlights](#highlights)
- [Comparison with Other IDEs](#comparison-with-other-ides)
- [Memory & Performance](#memory--performance)
- [Packaging & Installation](#packaging--installation)
- [Getting Started](#getting-started)
- [Features](#features)
- [Keymap](#keymap)
- [Requirements](#requirements)
- [Architecture](#architecture)
- [Development](#development)
- [Releases](#releases)
- [Project Layout](#project-layout)
- [Status](#status)
- [License](#license)

---

## Highlights

- **Single native process, ~59 MiB RAM.** Window, editor, tool windows, and the agent panel run in one Rust process. Opening this workspace on Windows settles at **59.18 MiB Working Set / 43.97 MiB Private Bytes**.
- **3.28 MiB portable package.** On Windows x86_64, the stripped release `ide-app.exe` (with embedded multi-resolution icon) is **8.24 MiB** and compresses to **3.28 MiB** (`RI-v0.2.0-windows-x86_64.zip`) — two orders of magnitude smaller than JVM or Electron IDEs.
- **Cross-platform (`v0.2.0`).** Windows (DirectWrite + ConPTY + DWM), Linux (X11/Wayland + `cosmic-text`), and macOS (`arm64` / `x86_64` `.app` bundle + `Cmd` shortcuts).
- **IDEA New UI & keymap.** Islands Dark, Darcula Classic, and Islands Light themes, New UI tool stripes, `Ctrl+Alt+U` live English / 中文 switching, and startup reveal animation.
- **Local agents in the workbench.** Cursor (with persistent Chrome DevTools MCP bridge and mode-isolated sessions), Google Antigravity (ACP), Claude Code, and OpenAI Codex share tabs, wait queues, inline Myers/Patience diffs, and workspace checkpoints.
- **First-tier languages & mobile.** Rust, TypeScript/JavaScript, and Python via `PATH` LSP/DAP binaries; Flutter & Android device selector and SDK settings.

---

## Comparison with Other IDEs

> **Measurement context**: Disk sizes of RI and installed IDEs below were measured directly on the same Windows workstation (`AMD Ryzen 9 5950X 16-Core`, `64 GiB RAM`). Process architecture and runtime stack reflect each IDE's shipping runtime.

### Installer / Disk Footprint & Runtime Stack

| Dimension | RI `v0.2.0` | IntelliJ IDEA | Cursor | VS Code | Antigravity IDE |
| --- | --- | --- | --- | --- | --- |
| **Shipping artifact (Windows)** | Portable `.zip` (**3.28 MiB**) → single `.exe` (**8.24 MiB**) | JetBrains installer + JBR + JVM bundles | Electron installer + ASAR + Node/Chromium | Electron installer + ASAR + Node/Chromium | Electron installer + ASAR + Node/Chromium |
| **Installed disk size (measured)** | **8.24 MiB** (`ide-app.exe`) | **4,229.58 MiB** (~4.13 GiB) | **1,104.72 MiB** (~1.08 GiB) | **1,081.49 MiB** (~1.06 GiB) | **921.28 MiB** |
| **Size ratio vs `ide-app.exe`** | **1×** | **~513×** | **~134×** | **~131×** | **~112×** |
| **Process model** | **1 native OS process** (+ optional LSP/agent CLIs) | JVM process + JBR + indexers/daemons | Electron main + renderer + GPU + extension host + workers | Electron main + renderer + GPU + extension host + workers | Electron main + renderer + GPU + extension host + workers |
| **UI & text rendering** | `tao` + `tiny-skia` + `softbuffer` + DirectWrite / `cosmic-text` | Swing / Skiko on JetBrains Runtime (JVM) | Chromium Blink + CSS + Skia | Chromium Blink + CSS + Skia | Chromium Blink + CSS + Skia |
| **Text buffer** | `ropey` (incremental rope) | JVM char/byte arrays & PSI | Piece table in JS/V8 | Piece table in JS/V8 | Piece table in JS/V8 |
| **Terminal** | `portable-pty` (ConPTY / PTY) | `pty4j` (JNI) | `node-pty` (native Node addon) | `node-pty` (native Node addon) | `node-pty` (native Node addon) |
| **Built-in AI agents** | Native Rust hub (Cursor, Antigravity ACP, Claude Code, Codex) | AI Assistant plugin | Built-in Cursor runtime | Copilot extension | Built-in Antigravity runtime |

### Measured Runtime Footprint (RI `v0.2.0` on Windows)

Opening this repository (`ide-app.exe --workspace D:\work\rust-ai-ide`) in Release mode:

| Metric | Measured Value | Notes |
| --- | ---: | --- |
| **Cold self-check (`--self-check`)** | **190 ms** | Initializes workspace, Git, syntax, and renders 5 off-screen `1560×940` states |
| **Startup RAM (`t = 1.5s`)** | **58.93 MiB** Working Set / **43.97 MiB** Private Bytes | Peak Working Set: **58.96 MiB** |
| **Settled RAM (`t = 5.0s`)** | **59.18 MiB** Working Set / **43.97 MiB** Private Bytes | Peak Working Set: **59.18 MiB**, single process |

By contrast, JVM-based IDEs (IntelliJ IDEA) typically start with a multi-hundred-MB to multi-GB JVM heap plus IDE caches, and Electron/Chromium IDEs (VS Code, Cursor, Antigravity IDE) spawn a multi-process Chromium + Node.js tree (main, renderer, GPU, extension host, language/agent helpers) even before large plugins load. RI keeps the IDE shell itself around **~44 MiB private memory / ~59 MiB working set** and only spawns external LSP, DAP, or agent CLI processes when you use them.

---

## Memory & Performance

### Frame Render & Scrolling Benchmark

Measured via `fast_scrolling_and_glyph_mask_cache_achieve_high_fps` (rendering 20 full `1560×940` IDE frames while scrolling code + horizontal pan):

| Build / Configuration | 20 Frames | Per Frame | Equivalent Rate | Speedup vs Uncached |
| --- | ---: | ---: | ---: | ---: |
| Uncached DirectWrite baseline (no glyph-mask cache) | ~1,191 ms | ~59.6 ms | ~17 FPS | 1.0× |
| `v0.2.0` Debug (`opt-level = 2`) | **59.72 ms** | **2.99 ms** | **~335 FPS** | **~19.9×** |
| `v0.2.0` Release (`opt-level = 3`, thin LTO) | **38.35 ms** | **1.92 ms** | **~522 FPS** | **~31.0×** |

### Bounded Memory Architecture (`v0.2.0`)

Every cache and persistence layer in RI is explicitly bounded to prevent unbounded growth during long coding or AI-agent sessions:

- **Glyph & text layout caches** (`ide-app`): DirectWrite subpixel mask cache capped at **1,024 entries / 16 MiB**, layout cache at **192 entries**, measure cache at **3,072 entries**; `cosmic-text` shaped cache capped at **1,024 entries**; strings over **300 bytes** bypass caches. Persistent DIB targets are capped at `2400×256`.
- **Agent sessions & checkpoints** (`ide-agent-hub`): Up to **12 checkpoints** per session, **2 MiB** snapshot cap per session, **8 MiB** total persisted history cap, and **512 KiB** per-file snapshot cap (binary/oversized/minified artifacts are skipped). Code blocks in chat rows use `Arc<str>` to eliminate deep string clones on scroll/render.
- **Git & workspace search** (`ide-git`, `ide-search`): Diff output bounded at **1,500 lines/file**, **8,000 total lines**, **1,024 bytes/line**, and **256 KiB** for untracked file diffs; commit detail (**16**), file history (**8**), and blame (**8**) caches are bounded. Workspace indexing caps at **50,000 files** while pruning `target/`, `node_modules/`, `.git/`, `dist/`, `.cache/`, and `.turbo/`.
- **Media previews & VFS** (`ide-app`, `ide-vfs`): Video duration probes read bounded MP4 `mvhd` boxes instead of loading multi-gigabyte media into memory; VFS shrinks excess redo stack capacity on clear.

---

## Packaging & Installation

RI distributes self-contained packages for all three desktop operating systems via [GitHub Releases](https://github.com/renyun2/rust-ai-ide/releases):

| Platform | Release Asset | Contents | Windows / OS Size Note |
| --- | --- | --- | --- |
| **Windows x86_64** | `RI-<tag>-windows-x86_64.zip` | Portable `ide-app.exe` (embedded ICO, no installer wizard required) | **3.28 MiB** zip → **8.24 MiB** exe (`v0.2.0`) |
| **Linux x86_64** | `RI-<tag>-linux-x86_64.tar.gz` | `ide-app`, `rust-ai-ide.png`, `rust-ai-ide.desktop` | Stripped native binary + desktop entry |
| **macOS Apple Silicon (`arm64`)** | `RI-<tag>-macos-arm64.zip` | `Rust AI IDE.app` bundle (`Info.plist` + `AppIcon.icns`) | Universal-free native `aarch64` bundle |
| **macOS Intel (`x86_64`)** | `RI-<tag>-macos-x86_64.zip` | `Rust AI IDE.app` bundle (`Info.plist` + `AppIcon.icns`) | Native `x86_64` bundle |

Each archive is accompanied by a `.sha256` checksum file. Simply extract and run — no installer admin prompt, registry bloat, or background updater service.

---

## Getting Started

### Run from a Release Archive

- **Windows**: Extract `RI-v0.2.0-windows-x86_64.zip` and double-click `ide-app.exe` (or run `.\ide-app.exe --workspace D:\path\to\project`).
- **Linux**: Extract `RI-v0.2.0-linux-x86_64.tar.gz` and run `./ide-app --workspace /path/to/project` (ensure GTK3 and a sans/mono system font are installed).
- **macOS**: Extract `RI-v0.2.0-macos-arm64.zip` (or `x86_64`) and move `Rust AI IDE.app` to `/Applications`.

### Build & Run from Source

```powershell
# Open the current directory
cargo run -p ide-app --release

# Open a specific workspace
cargo run -p ide-app --release -- --workspace D:\path\to\your\project

# Render off-screen self-check frames and exit
cargo run -p ide-app --release -- --self-check

# Rasterize the in-app logo to PNG or multi-res Windows ICO
cargo run -p ide-app --release -- --export-icon assets/ri_icon.png
cargo run -p ide-app --release -- --export-icon assets/ri_icon.ico
```

| Flag | Effect |
| --- | --- |
| `--workspace <path>` | Open this directory instead of the current working directory |
| `--self-check` | Render off-screen UI snapshots and exit (used by CI and smoke tests) |
| `--export-icon <path>` | Export the RI mark as a 256×256 `.png` or multi-res `.ico` and exit |

---

## Features

### Workbench

- **Themes**: **JetBrains Dark (Islands)**, **Darcula Classic**, **IntelliJ Light (Islands)** — cycle with `` Ctrl+` `` or configure in `Ctrl+Alt+S` (macOS: `Cmd+,`).
- **Settings**: Appearance, Menus and Toolbars, System, Editor, Font, Color Scheme, Code Style, Keymap, Plugins, Git, Debugger, Terminal, Languages, **Mobile SDK**, Advanced.
- **DPI & Live Zoom**: Detects system DPI (`100%`–`400%`) at startup; zoom live with `Ctrl+=`, `Ctrl+-`, `Ctrl+0`.
- **Native Chrome & Startup**: Borderless window with custom titlebar, Windows 11 rounded corners + embedded taskbar icon + reveal animation, multi-window workspace persistence, and Git branch popup.
- **IME & Text Input Undo/Redo**: IME candidate window repositions only on true caret movement (no frame-by-frame flicker during CJK input), and all input boxes support `Ctrl+Z` / `Ctrl+Shift+Z` / `Ctrl+Y`.

### Editor & Search

- `ropey` buffer with undo/redo, duplicate/delete line, line comment toggle, reformat, rename (`Shift+F6`), find/replace, go to line, and intentions (`Alt+Enter`).
- **Search Everywhere** (Double `Shift`), **Go to File** (`Ctrl+Shift+N`), **Find in Files** (`Ctrl+Shift+F`), and **Find Action** (`Ctrl+Shift+A`) powered by `ripgrep` crates.
- Tree-sitter syntax highlighting and **Structure** (`Alt+7`) for Rust, TypeScript, JavaScript, Python, JSON, TOML, and Markdown.

### Languages, Run, Debug & Mobile

| Language | Language Server | Debugger | Run Configurations |
| --- | --- | --- | --- |
| Rust | `rust-analyzer` | `codelldb` | Cargo check, test, run |
| TypeScript / JavaScript | `vtsls` or `typescript-language-server` | `js-debug` | Auto-detected from project |
| Python | `pyright-langserver` or `ruff` | `debugpy` | Python script / entry |
| Flutter / Android | — | Device via ADB | `flutter run`, `test`, `build apk`, `pub get`, `doctor` |

Opening a Flutter or Android project adds the toolbar **device** selector. Configure custom **Flutter SDK**, **Android SDK**, and **JDK** paths in `Ctrl+Alt+S` → **Mobile SDK**, or leave blank for auto-detection.

### Git & Terminal

- **Commit** (`Alt+0`): change tree, checkbox-only IDEA commit selection, inline `similar` diffs, and AI commit message generation.
- **Git Log** (`Alt+9`): branch filter, graph, file diffs, fetch, merge, rebase, stash, blame, and file history.
- **Terminal** (`Alt+F12`): `portable-pty` (ConPTY on Windows, PTY on Unix), multi-tab, VT100/ANSI color.

### AI Assistant (`ide-agent-hub`)

Open with `Alt+8`. Connects directly to your local coding agents:

| Provider | Integration |
| --- | --- |
| **Cursor** | Local CLI with mode-isolated sessions (Ask/Edit/Agent), `Ctrl+K` inline edit, and persistent `chrome-devtools-mcp` bridge (`browser_bridge`) |
| **Google Antigravity** | ACP (JSON-RPC 2.0 over stdio via `agy`) |
| **Claude Code** | `claude` headless stream-JSON + embedded PTY |
| **OpenAI Codex** | `codex` exec + embedded PTY |
| **Custom Endpoint** | OpenAI-compatible HTTP or Gemini REST fallback |

Includes **Ask / Edit / Agent** modes, reasoning effort selector, image drag-and-drop/paste, collapsible fenced code chips, session tabs ordered by activity, queued follow-up prompts (undo / return to edit / discard / send now), and `Ctrl+Alt+Z` workspace checkpoint revert.

---

## Keymap

IntelliJ IDEA bindings (`Ctrl` on Windows/Linux; `Cmd` aliases for standard editing/navigation on macOS).

| Keys | Action | Keys | Action |
| --- | --- | --- | --- |
| Double `Shift` | Search Everywhere | `Ctrl+Alt+S` | Settings |
| `Ctrl+Shift+F` | Find in Files | `` Ctrl+` `` | Cycle theme |
| `Ctrl+Shift+A` | Find Action | `Ctrl+Alt+U` | English / 中文 |
| `Ctrl+Shift+N` | Go to File | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Zoom in, out, reset |
| `Ctrl+B` | Go to declaration | `Ctrl+K` | Inline AI edit |
| `Ctrl+D` / `Ctrl+Y` | Duplicate / delete line | `Alt+L` | Send selection to agent |
| `Ctrl+/` | Toggle line comment | `Tab` / `Esc` | Accept / reject inline diff |
| `Ctrl+Alt+L` | Reformat | `Ctrl+Alt+Z` | Revert latest AI checkpoint |
| `Shift+F6` / `Alt+Enter` | Rename / intentions | `Alt+G` / `Alt+C` / `Alt+X` | Antigravity / Claude / Codex |
| `Shift+F10` / `Shift+F9` | Run / debug | `Alt+1` / `Alt+0` / `Alt+9` | Project / Commit / Git Log |
| `Ctrl+F8` | Toggle breakpoint | `Alt+7` / `Alt+8` / `Alt+F12` | Structure / Agents / Terminal |

---

## Requirements

- **Runtime**: Windows 10/11, Linux (GTK3, D-Bus, X11/Wayland, system fonts), or macOS 11+.
- **Building from source**: Rust stable (2021 edition). On Linux: `libgtk-3-dev`, `libdbus-1-dev`, `pkg-config`, `fonts-dejavu-core`.
- **Optional external tools** (discovered on `PATH` or Settings):
  - LSP: `rust-analyzer`, `vtsls` / `typescript-language-server`, `pyright-langserver` / `ruff`
  - DAP: `codelldb`, `js-debug`, `debugpy`
  - Agents: `agy`, `claude`, `codex`, Cursor CLI
  - Mobile: Flutter SDK, Android SDK (`platform-tools`), JDK 17

---

## Architecture

Ten workspace crates isolate protocol/engine logic from `ide-app` UI rendering:

| Crate | Role |
| --- | --- |
| [`ide-core`](crates/ide-core) | Themes, keymap, `LanguageId`, run configurations, Flutter/Android SDK probe |
| [`ide-vfs`](crates/ide-vfs) | `ropey` buffer, undo snapshots, workspace file tree |
| [`ide-syntax`](crates/ide-syntax) | Tree-sitter highlight spans, structure symbols, runnable gutter marks |
| [`ide-search`](crates/ide-search) | Ripgrep-backed Search Everywhere and Find in Files |
| [`ide-lsp`](crates/ide-lsp) | LSP JSON-RPC client and local server discovery |
| [`ide-dap`](crates/ide-dap) | DAP session, breakpoints, stack frames, crash context for agents |
| [`ide-git`](crates/ide-git) | Status, stage, commit, branches, log, blame, bounded line diffs |
| [`ide-terminal`](crates/ide-terminal) | `portable-pty` session (ConPTY / PTY) and VT100 parser |
| [`ide-agent-hub`](crates/ide-agent-hub) | Agent registry, sessions, wait queue, ACP, CLI runners, `browser_bridge`, inline diff, checkpoints |
| [`ide-app`](crates/ide-app) | `tao` event loop and DirectWrite / `cosmic-text` + `tiny-skia` + `softbuffer` renderer |

---

## Development

```powershell
cargo check --workspace
cargo test --workspace
cargo run -p ide-app -- --self-check
```

Release profile (`Cargo.toml`): `opt-level = 3`, `lto = "thin"`, `codegen-units = 1`, `strip = "symbols"`. Dev profile optimizes dependencies at `opt-level = 3` so debug builds remain responsive.

---

## Releases

Publishing a GitHub Release triggers [`.github/workflows/release.yml`](.github/workflows/release.yml) across `windows-latest`, `ubuntu-latest`, and `macos-latest` (`arm64` & `x86_64`):

1. Run `cargo test --workspace` and `--self-check`.
2. Build `cargo build -p ide-app --release` for each target.
3. Package Windows `.zip`, Linux `.tar.gz`, and macOS `.app` `.zip` archives with `.sha256` checksums and attach them to the Release.

Pushes to `master`/`main` and pull requests run multi-OS CI tests via [`.github/workflows/ci.yml`](.github/workflows/ci.yml).

---

## Project Layout

```text
rust-ai-ide/
├── crates/
│   ├── ide-core/           # theme, keymap, run configs, mobile SDK
│   ├── ide-vfs/            # ropey buffer, file tree
│   ├── ide-syntax/         # tree-sitter highlighting & structure
│   ├── ide-search/         # ripgrep search
│   ├── ide-lsp/            # LSP client
│   ├── ide-dap/            # DAP client
│   ├── ide-git/            # git status, log, diff, blame
│   ├── ide-terminal/       # ConPTY / PTY terminal
│   ├── ide-agent-hub/      # ACP, CLI runners, browser_bridge, checkpoints
│   └── ide-app/            # desktop window, renderer, persistence
├── .github/workflows/      # ci.yml, release.yml
├── assets/                 # ri_icon.svg, ri_icon.png, ri_icon.ico
├── Cargo.toml
├── README.md
└── README.zh-CN.md
```

---

## Status

Version **0.2.0**. Runs natively on Windows, Linux, and macOS. Visual layout, themes, and keymap follow IntelliJ IDEA New UI; the implementation is independent.

---

## License

Licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

at your option (`MIT OR Apache-2.0`). File-type icons retain their separate MIT terms in [`crates/ide-app/assets/file-icons/NOTICE.md`](crates/ide-app/assets/file-icons/NOTICE.md).
