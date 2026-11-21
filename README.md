<div align="center">
  <img src="assets/ri_icon.png" alt="RI logo" width="120" height="120" />

  <h1>RI — Pure Rust AI IDE</h1>

  <p>A native Windows IDE for Rust, TypeScript, JavaScript, Python, and Flutter.<br/>IntelliJ IDEA New UI layout and keymap, with local coding agents in the same window.</p>

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

RI draws its own UI, edits text on a rope, and talks to the language servers, debuggers, Git, terminal, and coding-agent CLIs already installed on the machine. Flutter and Android projects get a device selector and SDK paths in Settings. There is no bundled JVM, Chromium, or copy of those tools.

---

## Contents

- [Highlights](#highlights)
- [Getting started](#getting-started)
- [Features](#features)
- [Keymap](#keymap)
- [Requirements](#requirements)
- [Architecture](#architecture)
- [Performance](#performance)
- [Development](#development)
- [Releases](#releases)
- [Project layout](#project-layout)
- [Status](#status)
- [License](#license)

---

## Highlights

- **One native process.** Window, editor, tool windows, and the agent panel are Rust. A stripped release `ide-app` has been measured at **6.04 MB**.
- **IDEA layout.** Islands Dark, Darcula Classic, and Islands Light, the New UI tool stripes, and the IntelliJ keymap. UI language switches with `Ctrl+Alt+U`.
- **Local agents in the workbench.** Cursor, Antigravity (ACP), Claude Code, and Codex share sessions, inline diffs, and checkpoints.
- **First-tier languages.** Rust, TypeScript/JavaScript, and Python use the LSP and DAP binaries on your `PATH`. JSON, TOML, and Markdown are highlighted.
- **Flutter and Android.** The toolbar lists phones and emulators. Settings stores Flutter SDK, Android SDK, and JDK directories.

---

## Getting started

```powershell
# Open the current directory
cargo run -p ide-app --release

# Open a project
cargo run -p ide-app --release -- --workspace D:\path\to\your\project

# Render one frame off-screen and exit
cargo run -p ide-app --release -- --self-check

# Rasterize the in-app logo to a 256×256 PNG
cargo run -p ide-app --release -- --export-icon assets/ri_icon.png
```

| Flag | Effect |
| --- | --- |
| `--workspace <path>` | Open this directory instead of the current one |
| `--self-check` | Render off-screen and exit. Used by CI and local smoke tests |
| `--export-icon <path>` | Write the RI mark as a PNG and exit |

---

## Features

### Workbench

- Themes: **JetBrains Dark (Islands)**, **Darcula Classic**, **IntelliJ Light (Islands)**. Cycle with `` Ctrl+` `` or open Settings with `Ctrl+Alt+S`.
- Settings follow the IDEA page list: Appearance, Menus and Toolbars, System, Editor, Font, Color Scheme, Code Style, Keymap, Plugins, Git, Debugger, Terminal, Languages, **Mobile SDK**, Advanced.
- Startup reads the system DPI (`100%`–`400%`). `Ctrl+=`, `Ctrl+-`, and `Ctrl+0` zoom the UI live.
- Borderless window, Windows 11 rounded corners, workspace switcher (this window or a new one), and a Git branch popup.
- Gutter width follows the line count. Breakpoints and runnable gutter marks share that gutter.
- Project, Git, and agent work leave the UI thread.

### Editor and search

- `ropey` buffer with undo/redo, line duplicate and delete, comment toggle, reformat, rename (`Shift+F6`), and intention actions (`Alt+Enter`).
- **Search Everywhere** (Double `Shift`), **Go to File** (`Ctrl+Shift+N`), **Find in Files** (`Ctrl+Shift+F`), and **Find Action** (`Ctrl+Shift+A`), on the ripgrep stack.
- Syntax color and structure for Rust, TypeScript, JavaScript, Python, JSON, TOML, and Markdown. **Structure** is `Alt+7`.

### Languages, run, and debug

| Language | Language server | Debugger | Run configurations |
| --- | --- | --- | --- |
| Rust | `rust-analyzer` | `codelldb` | Cargo check, test, run |
| TypeScript / JavaScript | `vtsls` or `typescript-language-server` | `js-debug` | Detected from the project |
| Python | `pyright-langserver` or `ruff` | `debugpy` | Python script / entry |
| Flutter | — | device via ADB | `flutter run`, test, build apk, pub get, doctor |

`Shift+F10` runs the active configuration. `Ctrl+Shift+F10` runs the current file. `Shift+F9` starts a debug session. `Ctrl+F8` toggles a breakpoint. Gutter `▶` marks come from the syntax crate.

### Mobile

Opening a Flutter or Android workspace adds a **device** control to the toolbar. RI probes Flutter, the Android SDK, ADB, JDK, phones, and emulators.

`Ctrl+Alt+S` → **Mobile SDK** (or **Path** on the device popup) sets three directories:

| Field | Directory contains |
| --- | --- |
| Flutter SDK | `bin\flutter.bat` (or `flutter` / `flutter.cmd` / `flutter.exe`) |
| Android SDK | `platform-tools` |
| JDK | `bin\java.exe` |

Leave a field empty to detect it from `PATH`, `ANDROID_HOME` / `ANDROID_SDK_ROOT`, `JAVA_HOME`, `local.properties`, and common install folders. A filled-in path that is missing or incomplete stays on **SDK not installed** and is not replaced by another install on the machine. Run and debug then use the resolved `flutter`, `dart`, and `adb` binaries.

### Git and terminal

- **Commit** (`Alt+0`): change list, stage and unstage, inline diffs (`similar`), and the commit message. Diff and status work runs on a background worker.
- **Log** (`Alt+9`): branch filter, graph, and file diff. Branches (`Ctrl+Shift+G`), new branch (`Ctrl+Alt+B`), pull (`Ctrl+T`), push (`Ctrl+Shift+K`), fetch, merge, rebase, stash, blame, and file history are on the Git menu.
- **Terminal** (`Alt+F12`): `portable-pty` on Windows ConPTY, multiple tabs, VT100, and ANSI color.

### AI assistant

`ide-agent-hub` is where the local CLIs show up. Open it with `Alt+8`.

| Provider | How RI talks to it |
| --- | --- |
| Cursor | Local CLI, plus `Ctrl+K` inline edit |
| Google Antigravity | ACP, JSON-RPC 2.0 over stdio (`agy`) |
| Claude Code | `claude` headless stream-JSON and an embedded PTY |
| OpenAI Codex | `codex` exec and an embedded PTY |
| Custom endpoint | OpenAI-compatible or Gemini REST fallback |

`Alt+G`, `Alt+C`, and `Alt+X` switch Antigravity, Claude Code, and Codex. Each session has **Ask**, **Edit**, and **Agent** modes, a reasoning effort, and an optional fast mode. Drop or paste images, fold fenced code into chips, and jump from a reply to a file and line. `Alt+L` sends the selection into the panel.

**Sessions.** Tabs sit on the top of the panel. The newest conversation is last, ordered by the time you sent a message, so sending in an older session moves that tab to the end. **+ New** is pinned to the right; the session count sits just left of it. Hover the tabs and scroll with the vertical wheel, the horizontal wheel, or Shift+wheel. Clicking a tab that is already visible keeps the scroll position.

**Waiting area.** If the current session is still responding, Enter queues the next prompt above the composer. When the turn finishes, the oldest queued prompt is sent. Each queued item can be:

- **Undo** — put the text back into the composer, in front of any draft
- **Return to edit** — replace the composer text and restore that item's mode
- **Close** — discard it
- **Send now** — cancel the in-flight turn and send this prompt immediately

Another session can run in parallel. Inline edits use Myers and Patience hunks: `Tab` accepts, `Esc` rejects. `Ctrl+Alt+Z` reverts the latest workspace checkpoint.

---

## Keymap

IntelliJ IDEA bindings. On Windows, `Ctrl` is the primary modifier.

| Keys | Action | Keys | Action |
| --- | --- | --- | --- |
| Double `Shift` | Search Everywhere | `Ctrl+Alt+S` | Settings |
| `Ctrl+Shift+F` | Find in Files | `` Ctrl+` `` | Cycle theme |
| `Ctrl+Shift+A` | Find Action | `Ctrl+Alt+U` | English / 中文 |
| `Ctrl+Shift+N` | Go to File | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Zoom in, out, reset |
| `Ctrl+B` | Go to declaration | `Ctrl+K` | Inline AI edit |
| `Ctrl+D` / `Ctrl+Y` | Duplicate line / delete line | `Alt+L` | Send selection to the agent |
| `Ctrl+/` | Toggle line comment | `Tab` / `Esc` | Accept / reject inline diff |
| `Ctrl+Alt+L` | Reformat | `Ctrl+Alt+Z` | Revert the latest AI checkpoint |
| `Shift+F6` / `Alt+Enter` | Rename / intentions | `Alt+G` / `Alt+C` / `Alt+X` | Antigravity / Claude / Codex |
| `Shift+F10` / `Shift+F9` | Run / debug | `Alt+1` | Project |
| `Ctrl+Shift+F10` | Run current file | `Alt+0` | Commit |
| `Ctrl+F8` | Toggle breakpoint | `Alt+9` | Git log |
| `Ctrl+T` / `Ctrl+Shift+K` | Git pull / push | `Alt+7` / `Alt+8` / `Alt+F12` | Structure / Agents / Terminal |

---

## Requirements

- Rust stable, 2021 edition (`rustup` default is enough)
- Windows 10 or Windows 11 (DirectWrite, ConPTY, and DWM rounded corners)
- Optional tools, installed separately and discovered on `PATH` or from Settings:

| Kind | Tools |
| --- | --- |
| Language servers | `rust-analyzer`, `vtsls` or `typescript-language-server`, `pyright-langserver` or `ruff` |
| Debuggers | `codelldb`, `js-debug`, `debugpy` |
| Agents | `agy`, `claude`, `codex`, Cursor CLI |
| Mobile | Flutter SDK, Android SDK (`platform-tools`), JDK 17 for Gradle |

---

## Architecture

Ten crates. UI state stays in `ide-app`. Protocols and buffers stay in the libraries.

| Crate | Role |
| --- | --- |
| [`ide-core`](crates/ide-core) | Themes, keymap, `LanguageId`, run configurations, Flutter/Android SDK probe |
| [`ide-vfs`](crates/ide-vfs) | `ropey` buffer, undo snapshots, workspace file tree |
| [`ide-syntax`](crates/ide-syntax) | Highlight spans, structure symbols, runnable gutter marks |
| [`ide-search`](crates/ide-search) | Search Everywhere and Find in Files |
| [`ide-lsp`](crates/ide-lsp) | LSP JSON-RPC client and local server discovery |
| [`ide-dap`](crates/ide-dap) | DAP session, breakpoints, stack frames, crash context for agents |
| [`ide-git`](crates/ide-git) | Status, stage, commit, branches, log, line diffs |
| [`ide-terminal`](crates/ide-terminal) | ConPTY session and VT100 parser |
| [`ide-agent-hub`](crates/ide-agent-hub) | Agent registry, sessions, wait queue, ACP, CLI runners, inline edit, checkpoints |
| [`ide-app`](crates/ide-app) | `tao` event loop and the DirectWrite + tiny-skia + softbuffer renderer |

```text
tao window
    └── ide-app
            ├── ide-core         theme, keymap, run configs, mobile SDK
            ├── ide-vfs          rope buffer, file tree
            ├── ide-syntax       color, structure
            ├── ide-search       ripgrep index
            ├── ide-lsp / ide-dap
            ├── ide-git
            ├── ide-terminal     ConPTY
            └── ide-agent-hub    local CLIs, sessions, inline diff
```

---

## Performance

The renderer keeps glyph masks, highlight spans, and text measurements against the buffer revision, fills common rounded rectangles per scanline, and presents once through `softbuffer`. Wheel input is accumulated inside the frame budget.

Local scroll check `fast_scrolling_and_glyph_mask_cache_achieve_high_fps`: 20 full frames of a 1560×940 IDE surface while the code view scrolls, on one machine. These numbers describe this renderer.

| Build | 20 frames | Per frame | Equivalent rate | Versus the uncached baseline |
| --- | ---: | ---: | ---: | ---: |
| Baseline, DirectWrite every frame, no glyph-mask cache | ~1191 ms | ~59.6 ms | ~17 FPS | 1.0× |
| After the cache, debug profile (`opt-level = 2`) | 57.49 ms | 2.87 ms | ~348 FPS | ~20.7× |
| After the cache, release (`opt-level = 3`, thin LTO) | 33.43 ms | 1.67 ms | ~598 FPS | ~35.6× |

| | RI | IntelliJ IDEA | VS Code / Cursor |
| --- | --- | --- | --- |
| Shipping shape | One stripped native binary, measured around 6 MB | JBR plus the IDE distribution | Chromium and Node |
| UI | `tao`, `tiny-skia`, `softbuffer`, DirectWrite | Swing on a JetBrains runtime | Blink, CSS, Skia |
| Text | `ropey` | JVM heap | Editor core plus a renderer process |
| Terminal | `portable-pty` / ConPTY | Pty4j | `node-pty` |

---

## Development

```powershell
cargo check --workspace
cargo test --workspace
cargo run -p ide-app -- --self-check
```

Release profile (`Cargo.toml`): `opt-level = 3`, thin LTO, one codegen unit, stripped symbols. The dev profile optimizes dependencies at level 3 so the UI is usable without a full release build.

Issues and pull requests are welcome. Run `cargo test --workspace` and `cargo run -p ide-app -- --self-check` before sending a change. Keep protocol code in its crate and window code in `ide-app`.

---

## Releases

Publishing a GitHub Release starts the Windows build. Actions uses the workflow on `master`, then checks out the tag you selected.

1. Open [Releases](https://github.com/renyun2/rust-ai-ide/releases) and draft a new release.
2. Choose or create a tag such as `v0.1.0`.
3. Mark it as a pre-release if needed, then publish.

[`.github/workflows/release.yml`](.github/workflows/release.yml) on `windows-latest`:

1. `cargo test --workspace`
2. `cargo build -p ide-app --release`
3. `ide-app.exe --self-check`
4. Attach `RI-<tag>-windows-x86_64.zip` and a SHA-256 file to that release

Pushes to `master` or `main`, and pull requests, run tests only ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)). Pushing a tag without publishing a release does not build a binary.

On a private repository, allow Actions to upload release assets: **Settings → Actions → General → Workflow permissions → Read and write permissions**.

---

## Project layout

```text
rust-ai-ide/
├── crates/
│   ├── ide-core/           # theme, keymap, run configs, mobile SDK
│   ├── ide-vfs/
│   ├── ide-syntax/
│   ├── ide-search/
│   ├── ide-lsp/
│   ├── ide-dap/
│   ├── ide-git/
│   ├── ide-terminal/
│   ├── ide-agent-hub/      # sessions, wait queue, ACP, inline edit
│   └── ide-app/
├── .github/workflows/      # ci.yml, release.yml
├── assets/                 # ri_icon.svg, ri_icon.png
├── Cargo.toml
├── README.md
└── README.zh-CN.md
```

The mark is drawn at runtime by `draw_ri_logo_to_pixmap` in [`crates/ide-app/src/canvas.rs`](crates/ide-app/src/canvas.rs). The repo also stores [`assets/ri_icon.svg`](assets/ri_icon.svg) and [`assets/ri_icon.png`](assets/ri_icon.png).

---

## Status

Version **0.1.0**. The workbench, editor, search, Git, terminal, mobile device probe, and agent hub run on Windows. LSP and DAP clients expect the matching tools on `PATH` or, for Flutter, Android, and the JDK, in **Settings → Mobile SDK**. macOS and Linux are not supported targets: text uses DirectWrite and the terminal uses ConPTY.

The layout, color, and keymap follow the IntelliJ New UI. The implementation is independent.

---

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

at your option. The workspace package in [`Cargo.toml`](Cargo.toml) records this as `MIT OR Apache-2.0`.

File-type icons keep the separate MIT terms in [`crates/ide-app/assets/file-icons/NOTICE.md`](crates/ide-app/assets/file-icons/NOTICE.md).
