<div align="center">
  <img src="assets/ri_icon.png" alt="RI 标志" width="120" height="120" />

  <h1>RI — 纯 Rust AI IDE</h1>

  <p>完全用 Rust 编写的原生桌面 IDE。<br/>采用 IntelliJ IDEA New UI 的布局与快捷键，并在同一窗口接入本机编码 Agent。</p>

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

RI（**R**ust **I**DE）是面向 Rust、TypeScript、JavaScript 与 Python 的早期原生工作台。它自行绘制界面，用 rope 编辑文本，并连接语言服务、调试器、Git、真实终端，以及本机已经安装的编码 Agent CLI。

当前桌面目标是 **Windows 10/11**。语言服务和 Agent CLI 在本机发现。RI 不捆绑 JVM、浏览器或这些外部工具。

---

## 目录

- [要解决的问题](#要解决的问题)
- [优势](#优势)
- [功能](#功能)
- [架构](#架构)
- [性能](#性能)
- [快捷键](#快捷键)
- [环境要求](#环境要求)
- [快速开始](#快速开始)
- [开发](#开发)
- [发行版](#发行版)
- [仓库结构](#仓库结构)
- [现状](#现状)
- [品牌资源](#品牌资源)
- [许可证](#许可证)

---

## 要解决的问题

日常编码仍然拆在一个沉重的编辑器和一堆彼此独立的 Agent 终端之间。

| 痛点 | 代价 |
| --- | --- |
| **浏览器壳 IDE** | VS Code 与 Cursor 携带 Chromium 和 Node。安装体积大，内存拆在渲染进程与扩展进程之间，界面是嵌在原生窗口里的网页。 |
| **JVM IDE** | IntelliJ IDEA 携带完整的 JetBrains Runtime，以及庞大的 JAR 与插件图。冷启动和堆行为会进入每一次编辑。 |
| **Agent 在编辑器之外** | Antigravity、Claude Code、Codex、Cursor、Gemini 各自有 CLI 或独立产品。Diff、文件上下文和回滚不在同一个工作台里。 |
| **高分屏重绘** | 软件渲染的 IDE 在 2K、4K 上滚动时，如果每一帧都重新调用文本引擎，就会卡顿。 |
| **快捷键切换成本** | 习惯 IntelliJ 键位的人，在旁边打开的每一个编辑器和 AI 工具上，都要再付一次切换成本。 |

RI 要做的是把这条链路放回一个小的原生进程：IDEA 形态的界面、你已经在用的语言，以及你已经装好的本地 Agent。

---

## 优势

- **一个原生进程。** 窗口、编辑器、工具窗和 Agent 面板都是 Rust。开启 thin LTO 并剥离符号后，`ide-app` 的发布二进制实测为 **6.04 MB**（`6,335,488` 字节）。不捆绑 JBR，也不捆绑 Chromium。
- **不用 JVM 的 IDEA 布局。** Islands Dark、Darcula Classic、Islands Light、New UI 工具条，以及 IntelliJ 键位都在 `ide-core`。界面语言用 `Ctrl+Alt+U` 在英文和中文之间切换。
- **Agent 共用一个工作台。** `ide-agent-hub` 驱动本机 CLI（Cursor、Antigravity ACP、Claude Code、Codex、Gemini），并提供 `Ctrl+K` 行内修改。提议的编辑是 hunk diff。工作区检查点可用 `Ctrl+Alt+Z` 回滚。
- **语言链路是显式的。** Rust、TypeScript/JavaScript、Python 是一等支持：LSP 提供智能感知，DAP 提供调试，Git 提供审阅，ConPTY 提供终端。用的是你 `PATH` 上的服务，不是打进 RI 的副本。
- **滚动被当成渲染问题来处理。** 字形掩码、高亮区间和文本测量按缓冲区版本缓存。帧直接写入 `softbuffer`。

---

## 功能

### 工作台

- 三套主题，用 `` Ctrl+` `` 或设置（`Ctrl+Alt+S`）切换：**JetBrains Dark (Islands)**、**Darcula Classic**、**IntelliJ Light (Islands)**。
- 启动时读取系统 DPI（`100%`–`400%`），并用 `Ctrl+=`、`Ctrl+-`、`Ctrl+0` 实时缩放界面。
- 无边框窗口、Windows 11 圆角、工作区切换（当前窗口或新窗口），以及 Git 分支弹层。
- 行号栏宽度随总行数变化，五位、六位行号不会被截断。断点与可运行标记画在同一条 gutter 上。
- 项目、Git 和 Agent 工作离开 UI 线程。加载状态使用细圆弧指示。

### 编辑器与搜索

- 基于 `ropey` 的文本缓冲，带撤销/重做、选区编辑、双击选词、缩进和脏状态。
- **Search Everywhere**（双击 `Shift`）、**转到文件**（`Ctrl+Shift+N`）和 **在文件中查找**（`Ctrl+Shift+F`），底层是 ripgrep 栈（`ignore`、`grep-searcher`、`grep-regex`）。
- Rust、TypeScript、JavaScript、Python 的着色与结构。**结构**面板是 `Alt+7`。

### 语言、调试、Git、终端

| 语言 | 语言服务 | 调试器 |
| --- | --- | --- |
| Rust | `rust-analyzer` | `codelldb` |
| TypeScript / JavaScript | `vtsls` 或 `typescript-language-server` | `js-debug` |
| Python | `pyright-langserver` 或 `ruff` | `debugpy` |

- **Git**（`Alt+0`）：变更列表、暂存/取消暂存、行级 diff（`similar`）、提交说明，以及三栏历史（分支过滤、图、文件 diff）。仓库操作在后台 worker 上执行。
- **终端**（`Alt+F12`）：Windows ConPTY 上的 `portable-pty`，多标签、VT100 与 ANSI 颜色。PowerShell、Cargo 和 Git 按普通控制台程序运行。

### Agent

这些工具都出现在 `ide-agent-hub`：

| 提供方 | RI 如何连接 |
| --- | --- |
| Google Antigravity | ACP，stdio 上的 JSON-RPC 2.0（`agy`） |
| Claude Code | `claude` 无头 Stream-JSON，以及内嵌 PTY |
| OpenAI Codex | `codex` exec，以及内嵌 PTY |
| Cursor | 本地 CLI，加上 `Ctrl+K` 行内修改 |
| Gemini | 本地 `gemini` CLI |
| 自定义端点 | 兼容 OpenAI 的 HTTP 回退 |

Agent 面板支持多会话、拖入或粘贴图片、把围栏代码折成芯片，并能从回复跳到文件和行号。思考深度和快速模式是会话设置。行内修改使用 Myers 与 Patience hunk。`Tab` 接受，`Esc` 拒绝。

---

## 架构

十个 crate。界面状态留在 `ide-app`。协议和缓冲区留在各自的库里。

| Crate | 职责 |
| --- | --- |
| [`ide-core`](crates/ide-core) | 主题、`LanguageId`、运行配置探测、动作、键位、中英文菜单文案 |
| [`ide-vfs`](crates/ide-vfs) | `ropey` 缓冲、撤销快照、选区、工作区文件树 |
| [`ide-syntax`](crates/ide-syntax) | 高亮区间、结构符号、可运行 gutter 检测 |
| [`ide-search`](crates/ide-search) | 工作区索引、Search Everywhere、Find in Files |
| [`ide-lsp`](crates/ide-lsp) | LSP JSON-RPC 客户端与本机服务发现 |
| [`ide-dap`](crates/ide-dap) | DAP 会话、断点、调用栈、供 Agent 使用的崩溃上下文 |
| [`ide-git`](crates/ide-git) | 状态、暂存、提交、分支、日志、行级 diff |
| [`ide-terminal`](crates/ide-terminal) | ConPTY 会话与 VT100 解析 |
| [`ide-agent-hub`](crates/ide-agent-hub) | Agent 注册、会话、ACP、CLI 运行器、行内修改、检查点 |
| [`ide-app`](crates/ide-app) | `tao` 事件循环，以及 DirectWrite + tiny-skia + softbuffer 渲染器 |

```text
tao 窗口
    └── ide-app          界面、输入、帧
            ├── ide-core         主题、键位、语言 id
            ├── ide-vfs          rope 缓冲、文件树
            ├── ide-syntax       着色、结构
            ├── ide-search       ripgrep 索引
            ├── ide-lsp / ide-dap
            ├── ide-git
            ├── ide-terminal     ConPTY
            └── ide-agent-hub    本机 CLI + 行内 diff
```

---

## 性能

渲染器在热路径上保留五项缓存和捷径：

1. **字形掩码。** DirectWrite ClearType 覆盖率光栅化一次，存成紧凑掩码，之后用整数混合复用。
2. **按版本绑定的区间。** 高亮区间和测量宽度相对缓冲区版本缓存，滚动时直接复用。
3. **扫描线填充。** 常见圆角矩形按扫描线加小块 SDF 圆角填充，而不是每帧重建路径。
4. **直接提交。** 帧只拷贝一次到 `softbuffer`，没有第二块全屏中间缓冲。
5. **合并滚轮输入。** 快速滚动在帧预算内累加并步进。

本地滚动检查 `fast_scrolling_and_glyph_mask_cache_achieve_high_fps`：连续渲染 20 帧 1560×940 的完整 IDE 表面，同时滚动代码视图。同一台机器。这些数字描述的是这条渲染路径，不是对 IDEA 或 VS Code 的产品级对比。

| 构建 | 20 帧 | 单帧 | 等效帧率 | 相对无缓存基线 |
| --- | ---: | ---: | ---: | ---: |
| 基线：每帧调用 DirectWrite，无字形掩码缓存 | ~1191 ms | ~59.6 ms | ~17 FPS | 1.0× |
| 缓存之后，debug（`opt-level = 2`） | 57.49 ms | 2.87 ms | ~348 FPS | ~20.7× |
| 缓存之后，release（`opt-level = 3`，thin LTO） | 33.43 ms | 1.67 ms | ~598 FPS | ~35.6× |

| | RI | IntelliJ IDEA | VS Code / Cursor |
| --- | --- | --- | --- |
| 交付形态 | 一个剥离符号的原生二进制，实测约 6 MB | JBR 加 IDE 发行包 | Chromium 与 Node |
| 界面 | `tao`、`tiny-skia`、`softbuffer`、DirectWrite | Swing，跑在 JetBrains Runtime 上 | Blink、CSS、Skia |
| 文本 | Rust 线程上的 `ropey` | JVM 堆 | 扩展宿主里的编辑器核心，加上渲染进程 |
| 终端 | `portable-pty` / ConPTY | Pty4j | `node-pty` |

---

## 快捷键

IntelliJ IDEA 键位。在 Windows 上主键是 `Ctrl`。

| 按键 | 动作 | 按键 | 动作 |
| --- | --- | --- | --- |
| 双击 `Shift` | Search Everywhere | `Ctrl+Alt+S` | 设置 |
| `Ctrl+Shift+F` | 在文件中查找 | `` Ctrl+` `` | 切换主题 |
| `Ctrl+Shift+A` | 查找命令 | `Ctrl+Alt+U` | 英文 / 中文 |
| `Ctrl+Shift+N` | 转到文件 | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | 放大、缩小、重置 |
| `Ctrl+B` | 转到声明 | `Ctrl+K` | 行内 AI 修改 |
| `Ctrl+D` / `Ctrl+Y` | 复制行 / 删除行 | `Alt+L` | 把选区发给 Agent 面板 |
| `Ctrl+/` | 切换行注释 | `Tab` / `Esc` | 接受 / 拒绝行内 diff |
| `Ctrl+Alt+L` | 格式化 | `Ctrl+Alt+Z` | 回滚最近一次 AI 检查点 |
| `Shift+F10` / `Shift+F9` | 运行 / 调试 | `Alt+1` | 项目 |
| `Ctrl+F8` | 切换断点 | `Alt+0` | 提交 |
| `Ctrl+T` / `Ctrl+Shift+K` | Git 拉取 / 推送 | `Alt+7` / `Alt+8` / `Alt+F12` | 结构 / Agent / 终端 |

---

## 环境要求

- Rust stable，2021 edition（`rustup` 默认工具链即可）
- Windows 10 或 Windows 11（DirectWrite、ConPTY、DWM 圆角）
- 可选，单独安装，并从 `PATH` 发现：
  - 语言服务：`rust-analyzer`、`vtsls` 或 `typescript-language-server`、`pyright-langserver` 或 `ruff`
  - 调试器：`codelldb`、`js-debug`、`debugpy`
  - Agent：`agy`、`claude`、`codex`、Cursor CLI、`gemini`

---

## 快速开始

```powershell
# 以当前目录为工作区启动
cargo run -p ide-app --release

# 打开指定项目
cargo run -p ide-app --release -- --workspace D:\path\to\your\project

# 无头界面自检：渲染一帧并打印热区统计
cargo run -p ide-app --release -- --self-check

# 用内置矢量标志导出 256×256 PNG
cargo run -p ide-app --release -- --export-icon assets/ri_icon.png
```

| 参数 | 作用 |
| --- | --- |
| `--workspace <path>` | 打开该目录，而不是当前目录 |
| `--self-check` | 离屏渲染后退出，用于 CI 和本地冒烟 |
| `--export-icon <path>` | 把 RI 标志光栅化为 PNG 后退出 |

---

## 开发

```powershell
cargo check --workspace
cargo test --workspace
cargo run -p ide-app -- --self-check
```

发布配置（`Cargo.toml`）：`opt-level = 3`、thin LTO、单个 codegen unit、剥离符号。开发配置把依赖优化到 level 3，这样不用完整 release 构建也能操作界面。

欢迎 Issue 和 Pull Request。提交前请运行 `cargo test --workspace` 和 `cargo run -p ide-app -- --self-check`。风格与所在 crate 保持一致。协议代码放在对应 crate（`ide-lsp`、`ide-dap`、`ide-agent-hub`），窗口代码放在 `ide-app`。

---

## 发行版

推送版本标签后，GitHub Actions 会构建 Windows 发行包。工作流文件必须包含在那个提交里。给一个还没有 `.github/workflows/release.yml` 的旧提交打标签，不会开始构建。

```powershell
git tag v0.1.0
git push origin v0.1.0
```

[`.github/workflows/release.yml`](.github/workflows/release.yml) 在 `windows-latest` 上依次执行：

1. `cargo test --workspace`
2. `cargo build -p ide-app --release`
3. `ide-app.exe --self-check`
4. 发布 GitHub Release，附带 `RI-v0.1.0-windows-x86_64.zip` 和 SHA-256 校验文件

标签名里带连字符时（例如 `v0.2.0-rc.1`）会标成预发布。推送到 `master` 或 `main`，以及打开 Pull Request，只跑测试（[`.github/workflows/ci.yml`](.github/workflows/ci.yml)）。

私有仓库需要允许 Actions 创建 Release：**Settings → Actions → General → Workflow permissions → Read and write permissions**。

---

## 仓库结构

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
├── .github/workflows/      # ci.yml、release.yml
├── assets/                 # ri_icon.svg、ri_icon.png
├── Cargo.toml              # workspace，版本 0.1.0
├── README.md               # 英文主文档
└── README.zh-CN.md         # 本文件
```

---

## 现状

版本 **0.1.0**。工作台、编辑器、搜索、Git 工具窗、终端和 Agent 中心在 Windows 上运行。LSP 与 DAP 客户端要求对应工具位于 `PATH`。其他桌面平台还不是受支持目标：文本栈是 DirectWrite，终端栈是 ConPTY。

本仓库在布局、配色和键位上贴近 IntelliJ New UI，实现是独立的。

---

## 品牌资源

标志在运行时由 [`crates/ide-app/src/canvas.rs`](crates/ide-app/src/canvas.rs) 里的 `draw_ri_logo_to_pixmap` 绘制，仓库里也保存了一份：

- 矢量：[`assets/ri_icon.svg`](assets/ri_icon.svg)
- 256×256 PNG：[`assets/ri_icon.png`](assets/ri_icon.png)

---

## 许可证

可任选其一：

- Apache License 2.0
- MIT license

该选择写在 [`Cargo.toml`](Cargo.toml) 的 workspace package 上：`MIT OR Apache-2.0`。
