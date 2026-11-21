<div align="center">
  <img src="assets/ri_icon.png" alt="RI 标志" width="120" height="120" />

  <h1>RI — 纯 Rust AI IDE</h1>

  <p>面向 Rust、TypeScript、JavaScript、Python 与 Flutter 的原生 Windows IDE。<br/>采用 IntelliJ IDEA New UI 的布局与快捷键，并在同一窗口接入本机编码 Agent。</p>

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

RI 自行绘制界面，用 rope 编辑文本，并连接本机已经安装的语言服务、调试器、Git、终端和编码 Agent CLI。Flutter 与 Android 工程带有设备选择，SDK 目录在设置里填写。不捆绑 JVM、Chromium，也不捆绑这些外部工具。

---

## 目录

- [特性](#特性)
- [快速开始](#快速开始)
- [功能](#功能)
- [快捷键](#快捷键)
- [环境要求](#环境要求)
- [架构](#架构)
- [性能](#性能)
- [开发](#开发)
- [发行版](#发行版)
- [仓库结构](#仓库结构)
- [现状](#现状)
- [许可证](#许可证)

---

## 特性

- **一个原生进程。** 窗口、编辑器、工具窗和 Agent 面板都是 Rust。剥离符号后的 `ide-app` 发布二进制实测 **6.04 MB**。
- **IDEA 布局。** Islands Dark、Darcula Classic、Islands Light、New UI 工具条和 IntelliJ 键位。`Ctrl+Alt+U` 在英文和中文之间切换。
- **本机 Agent 在同一个工作台里。** Cursor、Antigravity（ACP）、Claude Code、Codex 共用会话、行内 diff 和检查点。
- **一等语言。** Rust、TypeScript/JavaScript、Python 使用 `PATH` 上的 LSP 和 DAP。JSON、TOML、Markdown 提供着色。
- **Flutter 与 Android。** 工具栏列出手机和模拟器。设置里保存 Flutter SDK、Android SDK 和 JDK 目录。

---

## 快速开始

```powershell
# 以当前目录为工作区启动
cargo run -p ide-app --release

# 打开指定项目
cargo run -p ide-app --release -- --workspace D:\path\to\your\project

# 离屏渲染一帧后退出
cargo run -p ide-app --release -- --self-check

# 把内置标志导出为 256×256 PNG
cargo run -p ide-app --release -- --export-icon assets/ri_icon.png
```

| 参数 | 作用 |
| --- | --- |
| `--workspace <path>` | 打开该目录，而不是当前目录 |
| `--self-check` | 离屏渲染后退出，用于 CI 和本地冒烟 |
| `--export-icon <path>` | 把 RI 标志写成 PNG 后退出 |

---

## 功能

### 工作台

- 三套主题：**JetBrains Dark (Islands)**、**Darcula Classic**、**IntelliJ Light (Islands)**。`` Ctrl+` `` 循环切换，`Ctrl+Alt+S` 打开设置。
- 设置页对齐 IDEA：外观、菜单和工具栏、系统设置、编辑器、字体、配色、代码风格、键位、插件、Git、调试、终端、语言、**移动端 SDK**、高级。
- 启动时读取系统 DPI（`100%`–`400%`）。`Ctrl+=`、`Ctrl+-`、`Ctrl+0` 实时缩放界面。
- 无边框窗口、Windows 11 圆角、工作区切换（当前窗口或新窗口），以及 Git 分支弹层。
- 行号栏宽度随总行数变化。断点与可运行标记画在同一条 gutter 上。
- 项目、Git 和 Agent 工作离开 UI 线程。

### 编辑器与搜索

- 基于 `ropey` 的文本缓冲，带撤销/重做、复制行、删除行、行注释、格式化、重命名（`Shift+F6`）和意图操作（`Alt+Enter`）。
- **Search Everywhere**（双击 `Shift`）、**转到文件**（`Ctrl+Shift+N`）、**在文件中查找**（`Ctrl+Shift+F`）、**查找命令**（`Ctrl+Shift+A`），底层是 ripgrep 栈。
- Rust、TypeScript、JavaScript、Python、JSON、TOML、Markdown 的着色与结构。**结构**面板是 `Alt+7`。

### 语言、运行与调试

| 语言 | 语言服务 | 调试器 | 运行配置 |
| --- | --- | --- | --- |
| Rust | `rust-analyzer` | `codelldb` | Cargo check、test、run |
| TypeScript / JavaScript | `vtsls` 或 `typescript-language-server` | `js-debug` | 按工程探测 |
| Python | `pyright-langserver` 或 `ruff` | `debugpy` | Python 脚本 / 入口 |
| Flutter | — | 通过 ADB 选择设备 | `flutter run`、test、build apk、pub get、doctor |

`Shift+F10` 运行当前配置。`Ctrl+Shift+F10` 运行当前文件。`Shift+F9` 开始调试。`Ctrl+F8` 切换断点。gutter 上的 `▶` 来自语法分析。

### 移动端

打开 Flutter 或 Android 工作区后，工具栏会出现**设备**入口。RI 会探测 Flutter、Android SDK、ADB、JDK、已连接手机和模拟器。

`Ctrl+Alt+S` → **移动端 SDK**（或在设备弹层点**目录**）填写三个安装目录：

| 字段 | 目录里需要有 |
| --- | --- |
| Flutter SDK | `bin\flutter.bat`（或 `flutter` / `flutter.cmd` / `flutter.exe`） |
| Android SDK | `platform-tools` |
| JDK | `bin\java.exe` |

留空时按 `PATH`、`ANDROID_HOME` / `ANDROID_SDK_ROOT`、`JAVA_HOME`、`local.properties` 和常见安装位置自动检测。填了路径但目录不存在或不完整时，状态为 **SDK 未安装**，不会改用机器上的另一份安装。运行和调试会使用解析后的 `flutter`、`dart`、`adb`。

### Git 与终端

- **提交**（`Alt+0`）：变更列表、暂存/取消暂存、行级 diff（`similar`）和提交说明。状态与 diff 在后台 worker 上计算。
- **历史**（`Alt+9`）：分支过滤、图和文件 diff。Git 菜单还包括分支（`Ctrl+Shift+G`）、新建分支（`Ctrl+Alt+B`）、拉取（`Ctrl+T`）、推送（`Ctrl+Shift+K`）、fetch、merge、rebase、stash、blame 和文件历史。
- **终端**（`Alt+F12`）：Windows ConPTY 上的 `portable-pty`，多标签、VT100 与 ANSI 颜色。

### AI 助手

本地 CLI 都出现在 `ide-agent-hub`。`Alt+8` 打开面板。

| 提供方 | RI 如何连接 |
| --- | --- |
| Cursor | 本地 CLI，加上 `Ctrl+K` 行内修改 |
| Google Antigravity | ACP，stdio 上的 JSON-RPC 2.0（`agy`） |
| Claude Code | `claude` 无头 Stream-JSON，以及内嵌 PTY |
| OpenAI Codex | `codex` exec，以及内嵌 PTY |
| 自定义端点 | 兼容 OpenAI 的 HTTP，或 Gemini REST |

`Alt+G`、`Alt+C`、`Alt+X` 分别切换 Antigravity、Claude Code 和 Codex。每个会话有 **Ask**、**Edit**、**Agent** 三种模式，以及思考深度和可选的快速模式。可以拖入或粘贴图片，把围栏代码折成芯片，并从回复跳到文件和行号。`Alt+L` 把选区发到面板。

**会话。** 标签在面板顶部。最新对话在最右边，顺序按你发出消息的时间计算；在较早的会话里发送后，该标签会移到末尾。**+ 新会话**固定在最右侧，会话数量按钮紧挨在它左边。鼠标停在标签上时，可用上下滚轮、左右滚轮或 Shift+滚轮翻页。点击已经看得见的标签，滚动位置保持不变。

**等待区。** 当前会话还在回复时，按 Enter 会把下一条放进输入框上方的等待区。这一轮结束后，最早排队的一条会自动发出。每条可以：

- **撤销**：放回输入框，已有草稿保留在后面
- **退回编辑**：用这条内容替换输入框，并恢复当时的模式
- **关闭**：丢掉这条消息
- **立即发送**：中断当前回复，马上发送这条

另开一个会话可以并行提问。行内修改使用 Myers 与 Patience hunk：`Tab` 接受，`Esc` 拒绝。`Ctrl+Alt+Z` 回滚最近一次工作区检查点。

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
| `Ctrl+D` / `Ctrl+Y` | 复制行 / 删除行 | `Alt+L` | 把选区发给 Agent |
| `Ctrl+/` | 切换行注释 | `Tab` / `Esc` | 接受 / 拒绝行内 diff |
| `Ctrl+Alt+L` | 格式化 | `Ctrl+Alt+Z` | 回滚最近一次 AI 检查点 |
| `Shift+F6` / `Alt+Enter` | 重命名 / 意图操作 | `Alt+G` / `Alt+C` / `Alt+X` | Antigravity / Claude / Codex |
| `Shift+F10` / `Shift+F9` | 运行 / 调试 | `Alt+1` | 项目 |
| `Ctrl+Shift+F10` | 运行当前文件 | `Alt+0` | 提交 |
| `Ctrl+F8` | 切换断点 | `Alt+9` | Git 历史 |
| `Ctrl+T` / `Ctrl+Shift+K` | Git 拉取 / 推送 | `Alt+7` / `Alt+8` / `Alt+F12` | 结构 / Agent / 终端 |

---

## 环境要求

- Rust stable，2021 edition（`rustup` 默认工具链即可）
- Windows 10 或 Windows 11（DirectWrite、ConPTY、DWM 圆角）
- 可选工具，单独安装，并从 `PATH` 或设置中发现：

| 类别 | 工具 |
| --- | --- |
| 语言服务 | `rust-analyzer`、`vtsls` 或 `typescript-language-server`、`pyright-langserver` 或 `ruff` |
| 调试器 | `codelldb`、`js-debug`、`debugpy` |
| Agent | `agy`、`claude`、`codex`、Cursor CLI |
| 移动端 | Flutter SDK、Android SDK（`platform-tools`）、供 Gradle 使用的 JDK 17 |

---

## 架构

十个 crate。界面状态留在 `ide-app`。协议和缓冲区留在各自的库里。

| Crate | 职责 |
| --- | --- |
| [`ide-core`](crates/ide-core) | 主题、键位、`LanguageId`、运行配置、Flutter/Android SDK 探测 |
| [`ide-vfs`](crates/ide-vfs) | `ropey` 缓冲、撤销快照、工作区文件树 |
| [`ide-syntax`](crates/ide-syntax) | 高亮区间、结构符号、可运行 gutter |
| [`ide-search`](crates/ide-search) | Search Everywhere、在文件中查找 |
| [`ide-lsp`](crates/ide-lsp) | LSP JSON-RPC 客户端与本机服务发现 |
| [`ide-dap`](crates/ide-dap) | DAP 会话、断点、调用栈、供 Agent 使用的崩溃上下文 |
| [`ide-git`](crates/ide-git) | 状态、暂存、提交、分支、日志、行级 diff |
| [`ide-terminal`](crates/ide-terminal) | ConPTY 会话与 VT100 解析 |
| [`ide-agent-hub`](crates/ide-agent-hub) | Agent 注册、会话、等待区、ACP、CLI 运行器、行内修改、检查点 |
| [`ide-app`](crates/ide-app) | `tao` 事件循环，以及 DirectWrite + tiny-skia + softbuffer 渲染器 |

```text
tao 窗口
    └── ide-app
            ├── ide-core         主题、键位、运行配置、移动端 SDK
            ├── ide-vfs          rope 缓冲、文件树
            ├── ide-syntax       着色、结构
            ├── ide-search       ripgrep 索引
            ├── ide-lsp / ide-dap
            ├── ide-git
            ├── ide-terminal     ConPTY
            └── ide-agent-hub    本机 CLI、会话、行内 diff
```

---

## 性能

渲染器按缓冲区版本缓存字形掩码、高亮区间和文本测量，常见圆角矩形按扫描线填充，帧经 `softbuffer` 提交一次。滚轮输入在帧预算内累加。

本地滚动检查 `fast_scrolling_and_glyph_mask_cache_achieve_high_fps`：同一台机器上连续渲染 20 帧 1560×940 的完整 IDE 表面，同时滚动代码视图。这些数字描述的是这条渲染路径。

| 构建 | 20 帧 | 单帧 | 等效帧率 | 相对无缓存基线 |
| --- | ---: | ---: | ---: | ---: |
| 基线：每帧调用 DirectWrite，无字形掩码缓存 | ~1191 ms | ~59.6 ms | ~17 FPS | 1.0× |
| 缓存之后，debug（`opt-level = 2`） | 57.49 ms | 2.87 ms | ~348 FPS | ~20.7× |
| 缓存之后，release（`opt-level = 3`，thin LTO） | 33.43 ms | 1.67 ms | ~598 FPS | ~35.6× |

| | RI | IntelliJ IDEA | VS Code / Cursor |
| --- | --- | --- | --- |
| 交付形态 | 一个剥离符号的原生二进制，实测约 6 MB | JBR 加 IDE 发行包 | Chromium 与 Node |
| 界面 | `tao`、`tiny-skia`、`softbuffer`、DirectWrite | Swing，跑在 JetBrains Runtime 上 | Blink、CSS、Skia |
| 文本 | `ropey` | JVM 堆 | 编辑器核心加上渲染进程 |
| 终端 | `portable-pty` / ConPTY | Pty4j | `node-pty` |

---

## 开发

```powershell
cargo check --workspace
cargo test --workspace
cargo run -p ide-app -- --self-check
```

发布配置（`Cargo.toml`）：`opt-level = 3`、thin LTO、单个 codegen unit、剥离符号。开发配置把依赖优化到 level 3，这样不用完整 release 构建也能操作界面。

欢迎 Issue 和 Pull Request。提交前请运行 `cargo test --workspace` 和 `cargo run -p ide-app -- --self-check`。协议代码放在对应 crate，窗口代码放在 `ide-app`。

---

## 发行版

推送版本标签后，GitHub Actions 会构建 Windows 发行包。工作流文件必须包含在那个提交里。

```powershell
git tag v0.1.0
git push origin v0.1.0
```

[`.github/workflows/release.yml`](.github/workflows/release.yml) 在 `windows-latest` 上依次执行：

1. `cargo test --workspace`
2. `cargo build -p ide-app --release`
3. `ide-app.exe --self-check`
4. 发布 `RI-<tag>-windows-x86_64.zip` 和 SHA-256 校验文件

标签名里带连字符时（例如 `v0.2.0-rc.1`）会标成预发布。推送到 `master` 或 `main`，以及打开 Pull Request，只跑测试（[`.github/workflows/ci.yml`](.github/workflows/ci.yml)）。

私有仓库需要允许 Actions 创建 Release：**Settings → Actions → General → Workflow permissions → Read and write permissions**。

---

## 仓库结构

```text
rust-ai-ide/
├── crates/
│   ├── ide-core/           # 主题、键位、运行配置、移动端 SDK
│   ├── ide-vfs/
│   ├── ide-syntax/
│   ├── ide-search/
│   ├── ide-lsp/
│   ├── ide-dap/
│   ├── ide-git/
│   ├── ide-terminal/
│   ├── ide-agent-hub/      # 会话、等待区、ACP、行内修改
│   └── ide-app/
├── .github/workflows/      # ci.yml、release.yml
├── assets/                 # ri_icon.svg、ri_icon.png
├── Cargo.toml
├── README.md               # 英文说明
└── README.zh-CN.md         # 本文件
```

标志在运行时由 [`crates/ide-app/src/canvas.rs`](crates/ide-app/src/canvas.rs) 里的 `draw_ri_logo_to_pixmap` 绘制。仓库里同时保存 [`assets/ri_icon.svg`](assets/ri_icon.svg) 和 [`assets/ri_icon.png`](assets/ri_icon.png)。

---

## 现状

版本 **0.1.0**。工作台、编辑器、搜索、Git、终端、移动设备探测和 Agent 中心在 Windows 上运行。LSP 与 DAP 要求对应工具位于 `PATH`；Flutter、Android 和 JDK 也可以在 **设置 → 移动端 SDK** 里指定目录。macOS 与 Linux 还不是受支持目标：文本栈是 DirectWrite，终端栈是 ConPTY。

布局、配色和键位贴近 IntelliJ New UI，实现是独立的。

---

## 许可证

可任选其一：

- [Apache License 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

该选择写在 [`Cargo.toml`](Cargo.toml) 的 workspace package 上：`MIT OR Apache-2.0`。

文件类型图标沿用 [`crates/ide-app/assets/file-icons/NOTICE.md`](crates/ide-app/assets/file-icons/NOTICE.md) 中单独的 MIT 条款。
