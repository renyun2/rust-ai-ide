<div align="center">
  <img src="assets/ri_icon.png" alt="RI — 纯 Rust AI IDE 标志" width="120" height="120" />

  <h1>RI — 纯 Rust AI IDE</h1>

  <p><strong>100% 纯 Rust 打造的轻量级跨平台原生桌面 IDE 与多 Agent 协同编程工作台。</strong><br/>复刻 IntelliJ IDEA New UI（Islands）布局与键位，常驻内存约 59 MiB，约 522 FPS 渲染，原生接入本地编码 Agent —— 零 JVM、零 Electron、零 Chromium。</p>

  <p>
    <a href="https://github.com/renyun2/rust-ai-ide/releases/latest">下载发行版</a> ·
    <a href="README.md">English</a> ·
    <a href="README.zh-CN.md">简体中文</a>
  </p>

  <p>
    <a href="https://github.com/renyun2/rust-ai-ide/releases/tag/v0.5.0"><img alt="Version 0.5.0" src="https://img.shields.io/badge/version-0.5.0-blue" /></a>
    <img alt="Rust 2021" src="https://img.shields.io/badge/Rust-2021-orange?logo=rust&logoColor=white" />
    <img alt="Platforms Windows | Linux | macOS" src="https://img.shields.io/badge/platform-Windows_%7C_Linux_%7C_macOS-0078D4" />
    <img alt="License MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT_OR_Apache--2.0-green" />
  </p>
</div>

**RI（Rust AI IDE）** 是一款面向 **Rust**、**Java（Maven / Gradle）**、**TypeScript / JavaScript**、**Python** 与 **Flutter / Android** 的开源纯 Rust 原生桌面 IDE。它自行绘制 2D 界面（`tao` + `tiny-skia` + `softbuffer`），基于增量式 `ropey` 缓冲编辑代码，并在**单个原生操作系统进程**中直连本机 LSP 语言服务、DAP 调试器、Git、ConPTY/PTY 终端以及本地编码 Agent（**Cursor CLI**、**Claude Code**、**OpenAI Codex**、**Google Antigravity ACP**）。

---

## 目录

- [v0.5.0 新特性](#v050-新特性)
- [为什么选择 RI？（核心亮点）](#为什么选择-ri核心亮点)
- [主流 IDE 横向对比](#主流-ide-横向对比)
- [内存占用与性能实测](#内存占用与性能实测)
- [安装包与交付形态](#安装包与交付形态)
- [快速开始](#快速开始)
- [功能一览](#功能一览)
- [快捷键](#快捷键)
- [环境要求](#环境要求)
- [架构](#架构)
- [常见问题（FAQ）](#常见问题faq)
- [开发与构建](#开发与构建)
- [发行版](#发行版)
- [仓库结构](#仓库结构)
- [现状](#现状)
- [许可证](#许可证)

---

## v0.5.0 新特性

- **全语言支持扩展（Go、Kotlin、Dart、C/C++、C#、前端、配置与脚本）：**
  - 全面扩展 `LanguageId`、Darcula/Islands 语法高亮、**Structure（`Alt+7`）** 结构提取、文件类型图标、Markdown 代码块映射及本地 LSP 服务探测，覆盖 **Go**（`gopls`）、**Kotlin**（`kotlin-language-server`）、**Dart**、**C / C++**（`clangd`）、**C#**、**HTML / Vue / Svelte / Astro**、**CSS / SCSS / Sass / Less**、**YAML**、**XML / SVG / XAML**、**SQL** 与 **Shell / PowerShell / Dockerfile / Makefile / CMake**。
- **工作区代码符号索引 & `Ctrl+悬浮` / `Ctrl+左键` 跳转：**
  - 在 `ide-search` 中索引全工作区代码符号（类、结构体、函数、接口、枚举、常量），支持 **Search Everywhere（双击 Shift）** 零磁盘 I/O 秒级符号检索。
  - 新增编辑器 **`Ctrl+鼠标悬浮`** 符号下划线高亮与预览提示，以及 **`Ctrl+鼠标左键` / `Ctrl+B`** 跨文件定义跳转（`find_workspace_symbol_definition`）。
- **Windows 输入体验、终端多行粘贴与控制台窗口闪烁修复：**
  - **远程/手机 Unicode 输入与防重复上屏**：兼容 Windows `WM_CHAR` / `VK_PACKET`（`SendInput(KEYEVENTF_UNICODE)`）远程/手机端输入，并消除编辑器与 AI 输入框中由 IME / 按键事件回显导致的字符打字重复两次问题。
  - **终端多行粘贴顺序与 Bracketed Paste**：在 ConPTY/PSReadLine 粘贴链路中将 `\r\n` / `\n` 统一归一化为 `\r`，并实现 DEC 私有模式 `2004` 括号粘贴包裹，彻底解决多行粘贴首尾行反转问题。
  - **彻底消除子进程与自更新弹窗闪烁**：直接解析原生 `agy.exe` 替代 `.cmd` 包装脚本，使用纯 Rust `PATH` 查找替代外部 `where` 调用，刷新 `last_check.timestamp` 抑制 `agy.exe --bg-updater` 后台弹窗，并在 Agent、LSP 与 Git 子进程中统一启用 `CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT`。

---

## 为什么选择 RI？（核心亮点）

- **单进程原生运行，常驻内存约 59 MiB。** 窗口、编辑器、工具窗、Git、终端与 Agent 面板全部跑在同一个 Rust 进程中；Windows 打开本工作区稳定后仅占 **59.18 MiB Working Set / 43.97 MiB Private Bytes**（`v0.2.0` 实测基线）。
- **约 3.3 MiB 绿色包，零运行时膨胀。** 不捆绑 JVM、Electron、Chromium 或 Node.js 运行时 —— 磁盘体积比 IntelliJ IDEA 小 **约 500 倍**，比 Electron AI 编辑器小 **约 110–134 倍**。
- **约 522 FPS 缓存帧渲染（单帧 1.92 ms）。** 自研 2D 画布配合有界 DirectWrite / `cosmic-text` 字形掩码缓存，滚动帧率较无缓存基线提升 **约 31 倍**。
- **原汁原味 IntelliJ IDEA New UI 与键位。** 内置 Islands Dark、Darcula Classic、Islands Light 三套主题，`Ctrl+Alt+U` 中英文实时切换，输入法防闪烁，支持多窗口工作区恢复与启动揭幕动画。
- **多 Agent 协同工作台（`Alt+8`）。** 统一驱动 **Cursor CLI**（Ask/Edit/Agent 模式会话隔离 + 持久化 Chrome DevTools MCP 桥接）、**Google Antigravity（ACP）**、**Claude Code**、**OpenAI Codex** 及 **OpenAI/Gemini 自定义端点**，共用 `Ctrl+K` Myers/Patience 行内 diff、排队等待区与 `Ctrl+Alt+Z` 工作区检查点回滚。
- **全平台原生分发。** 提供 **Windows x86_64**（DirectWrite + ConPTY + DWM）、**Linux x86_64**（X11/Wayland + `cosmic-text`）与 **macOS**（Apple Silicon `arm64` / Intel `x86_64` `.app` 应用包 + `Cmd` 快捷键）。

---

## 主流 IDE 横向对比

> **实测环境说明**：下表中 RI（`v0.2.0` Release 基线）与本机已安装 IDE 的磁盘占用均在同一台 Windows 开发机上实测获得（`AMD Ryzen 9 5950X 16 核 32 线程`、`64 GiB 内存`）；架构与运行时栈对照各 IDE 实际发行形态。

### 安装体积、进程模型与技术栈对比

| 对比维度 | RI（`v0.2.0` 基线） | IntelliJ IDEA | Cursor | VS Code | Antigravity IDE |
| --- | --- | --- | --- | --- | --- |
| **Windows 交付形态** | 免安装便携 `.zip`（**3.28 MiB**）→ 单文件 `.exe`（**8.24 MiB**） | 安装程序 + JBR + JVM 插件包 | Electron 安装包 + ASAR + Node/Chromium | Electron 安装包 + ASAR + Node/Chromium | Electron 安装包 + ASAR + Node/Chromium |
| **本机实测安装体积** | **8.24 MiB**（`ide-app.exe`） | **4,229.58 MiB**（约 4.13 GiB） | **1,104.72 MiB**（约 1.08 GiB） | **1,081.49 MiB**（约 1.06 GiB） | **921.28 MiB** |
| **相对 `ide-app.exe` 体积倍数** | **1×** | **约 513×** | **约 134×** | **约 131×** | **约 112×** |
| **进程架构** | **1 个原生进程**（按需拉起外部 LSP/Agent CLI） | JVM 主进程 + JBR + 索引/守护进程 | Electron 主进程 + 渲染 + GPU + 扩展宿主 + 多 Worker | Electron 主进程 + 渲染 + GPU + 扩展宿主 + 多 Worker | Electron 主进程 + 渲染 + GPU + 扩展宿主 + 多 Worker |
| **UI 与文本渲染** | `tao` + `tiny-skia` + `softbuffer` + DirectWrite / `cosmic-text` | Swing / Skiko（运行于 JetBrains Runtime） | Chromium Blink + CSS + Skia | Chromium Blink + CSS + Skia | Chromium Blink + CSS + Skia |
| **文本缓冲区** | `ropey`（增量 Rope） | JVM heap 字符数组与 PSI | JS/V8 Piece Table | JS/V8 Piece Table | JS/V8 Piece Table |
| **终端引擎** | `portable-pty`（ConPTY / PTY） | `pty4j`（JNI） | `node-pty`（原生 Node 扩展） | `node-pty`（原生 Node 扩展） | `node-pty`（原生 Node 扩展） |
| **AI Agent 集成** | 原生 Rust Hub（Cursor / Antigravity ACP / Claude Code / Codex） | AI Assistant 插件 | 内置 Cursor 运行时 | Copilot 扩展 | 内置 Antigravity 运行时 |

### Windows 实测冷启动与内存占用（Release 基线）

以 Release 构建打开当前 Rust 仓库（`ide-app.exe --workspace D:\work\rust-ai-ide`，`v0.2.0` 实测）：

| 实测指标 | 实测数据 | 说明 |
| --- | ---: | --- |
| **离屏自检启动（`--self-check`）** | **190 ms** | 完成工作区加载、Git/语法初始化并连续渲染 5 个 `1560×940` 界面状态 |
| **启动后内存（`t = 1.5s`）** | **58.93 MiB** Working Set / **43.97 MiB** Private Bytes | 峰值 Working Set：**58.96 MiB** |
| **工作区稳定常驻（`t = 5.0s`）** | **59.18 MiB** Working Set / **43.97 MiB** Private Bytes | 峰值 Working Set：**59.18 MiB**，单进程无额外常驻守护进程 |

相比之下，基于 JVM 的 IntelliJ IDEA 通常需要数百 MiB 至数 GiB 的 JVM 堆与索引内存，而基于 Electron/Chromium 的 VS Code、Cursor、Antigravity IDE 在打开工程后通常包含多个 Chromium 渲染进程、GPU 进程及 Node 扩展宿主进程。RI 将 IDE 本体常驻私有内存控制在 **~44 MiB（Working Set ~59 MiB）**，外部 LSP、DAP 与 Agent CLI 仅在启用时按需运行。

---

## 内存占用与性能实测

### 编辑器滚动与帧渲染基准

基于工作区单元测试 `fast_scrolling_and_glyph_mask_cache_achieve_high_fps`（在 `1560×940` 完整 IDE 表面上连续滚动代码并混合横向滚动，连续渲染 20 帧，`v0.2.0` 实测）：

| 构建配置 | 20 帧总耗时 | 单帧平均耗时 | 等效帧率 | 相对无缓存基线 |
| --- | ---: | ---: | ---: | ---: |
| 无字形掩码缓存基线（每帧 DirectWrite） | ~1,191 ms | ~59.6 ms | ~17 FPS | 1.0× |
| Debug（`opt-level = 2`） | **59.72 ms** | **2.99 ms** | **~335 FPS** | **~19.9×** |
| Release（`opt-level = 3`, thin LTO） | **38.35 ms** | **1.92 ms** | **~522 FPS** | **~31.0×** |

### 有界内存与低拷贝设计

为避免长时间编程与多轮 AI 对话导致内存缓慢膨胀，RI 对所有核心缓存和持久化链路均设置了明确上限：

- **字形与排版缓存**（`ide-app`）：DirectWrite 亚像素字形掩码缓存上限 **1,024 条 / 16 MiB**、布局缓存 **192 条**、文本测量缓存 **3,072 条**；`cosmic-text` 整形缓存上限 **1,024 条**；超过 **300 字节** 的长文本跳过缓存；持久 DIB 渲染目标上限为 `2400×256`。
- **Agent 会话与工作区检查点**（`ide-agent-hub`）：单会话最多保留 **12 个** 检查点、单会话快照上限 **2 MiB**、历史持久化总快照上限 **8 MiB**、单文件快照上限 **512 KiB**（自动跳过二进制或超大/压缩文件）；聊天代码块改用 `Arc<str>` 共享，避免滚动与渲染时深拷贝。
- **Git 与工作区索引**（`ide-git`、`ide-search`）：Diff 上限单文件 **1,500 行**、单次总计 **8,000 行**、单行 **1,024 字节**，未跟踪文件 diff 上限 **256 KiB**；提交详情（**16**）、文件历史（**8**）、Blame（**8**）缓存均有界。工作区索引上限 **50,000 文件**，自动剪枝 `target/`、`node_modules/`、`.git/`、`dist/`、`.cache/`、`.turbo/`。
- **媒体预览与编辑器缓冲**（`ide-app`、`ide-vfs`）：MP4 时长读取采用有界 `mvhd` 头部解析，不把整段大视频读入内存；VFS 清空重做栈时主动 `shrink_to_fit()` 归还多余容量。

---

## 安装包与交付形态

通过 [GitHub Releases](https://github.com/renyun2/rust-ai-ide/releases) 自动构建并发布三大桌面平台包，全部采用免安装绿色解压即用形态（附带 `.sha256` 校验文件）：

| 目标平台 | 发行包文件名 | 包内结构 | 说明 |
| --- | --- | --- | --- |
| **Windows x86_64** | `RI-v0.5.0-windows-x86_64.zip` | 单文件 `ide-app.exe`（内置多分辨率图标，无需安装向导） | 原生单可执行文件 |
| **Linux x86_64** | `RI-v0.5.0-linux-x86_64.tar.gz` | `ide-app`、`rust-ai-ide.png`、`rust-ai-ide.desktop` | 剥离符号原生二进制 + 桌面入口 |
| **macOS Apple Silicon（`arm64`）** | `RI-v0.5.0-macos-arm64.zip` | `Rust AI IDE.app`（含 `Info.plist` 与 `AppIcon.icns`） | 原生 `aarch64` 应用包 |
| **macOS Intel（`x86_64`）** | `RI-v0.5.0-macos-x86_64.zip` | `Rust AI IDE.app`（含 `Info.plist` 与 `AppIcon.icns`） | 原生 `x86_64` 应用包 |

无需管理员权限、无注册表残留、无后台自动更新守护进程。

---

## 快速开始

### 直接使用发行包

- **Windows**：解压 `RI-v0.5.0-windows-x86_64.zip`，双击 `ide-app.exe` 启动，或执行 `.\ide-app.exe --workspace D:\path\to\project`。
- **Linux**：解压 `RI-v0.5.0-linux-x86_64.tar.gz` 并运行 `./ide-app --workspace /path/to/project`（需系统具备 GTK3 及无衬线/等宽字体）。
- **macOS**：解压 `RI-v0.5.0-macos-arm64.zip`（或 `x86_64`），将 `Rust AI IDE.app` 拖入 `/Applications`。若首次打开提示 **“Rust AI IDE”已损坏，无法打开** 或无法验证开发者（Gatekeeper 对未公证开源应用拦截），在终端执行以下两条命令清除隔离属性后即可正常打开：
  ```bash
  xattr -cr "/Applications/Rust AI IDE.app"
  codesign --force --deep --sign - "/Applications/Rust AI IDE.app"
  ```

### 从源码编译运行

```powershell
# 打开当前目录
cargo run -p ide-app --release

# 打开指定工作区
cargo run -p ide-app --release -- --workspace D:\path\to\your\project

# 离屏渲染自检帧并退出
cargo run -p ide-app --release -- --self-check

# 导出 PNG 或多分辨率 Windows ICO 图标
cargo run -p ide-app --release -- --export-icon assets/ri_icon.png
cargo run -p ide-app --release -- --export-icon assets/ri_icon.ico
```

| 参数 | 作用 |
| --- | --- |
| `--workspace <path>` | 打开指定项目目录，而非当前目录 |
| `--self-check` | 离屏渲染并验证 UI 状态后退出，用于 CI 与本地冒烟 |
| `--export-icon <path>` | 导出 256×256 `.png` 或多分辨率 `.ico` 标志并退出 |

---

## 功能一览

### 工作台

- **三套主题**：**JetBrains Dark (Islands)**、**Darcula Classic**、**IntelliJ Light (Islands)**；`` Ctrl+` `` 循环切换，`Ctrl+Alt+S`（macOS 为 `Cmd+,`）打开设置。
- **IDEA 风格设置页**：外观、菜单与工具栏、系统设置、编辑器、字体、配色、代码风格、键位、插件、Git、调试、终端、语言和框架、**移动端 SDK**、高级。
- **DPI 与实时缩放**：启动自动检测系统缩放（`100%`–`400%`），支持 `Ctrl+=`、`Ctrl+-`、`Ctrl+0` 实时缩放界面。
- **原生窗口与启动动画**：无边框自绘标题栏、Windows 11 圆角、嵌入式任务栏图标、启动揭幕动画、多窗口工作区恢复与 Git 分支弹窗。
- **输入法防闪烁与全输入框撤销重做**：仅在光标位置真实改变时更新 IME 候选框位置（彻底消除中文输入法闪烁），全部文本输入框均支持 `Ctrl+Z` / `Ctrl+Shift+Z` / `Ctrl+Y`。

### 编辑器与全局搜索

- 基于 `ropey` 的文本缓冲：撤销/重做、复制/删除行、行注释、格式化、重命名（`Shift+F6`）、查找/替换、跳转行与意图操作（`Alt+Enter`）。
- **Search Everywhere**（双击 `Shift`）、**转到文件**（`Ctrl+Shift+N`）、**在文件中查找**（`Ctrl+Shift+F`）、**查找命令**（`Ctrl+Shift+A`），基于 `ripgrep` 栈实现。
- 语法高亮、**结构**大纲（`Alt+7`）与可运行 gutter 标记（`▶`），覆盖 **Rust、Java、TypeScript、JavaScript、Python、JSON、TOML、Markdown**。

### 语言、运行、调试与移动端

| 语言 | 语言服务 / 诊断 | 调试器 | 运行配置 |
| --- | --- | --- | --- |
| **Rust** | `rust-analyzer` | `codelldb` | Cargo `check` / `test` / `run` |
| **Java** | `jdtls` / `java-language-server` + Maven/`javac` 解析 | `java-debug` | Maven（`spring-boot:run`、`quarkus:dev`、`javafx:run`、`exec:java`、`compile`、`test`、`package`）、Gradle 任务、JUnit `@Test`、单文件 `.java` |
| **TypeScript / JavaScript** | `vtsls` 或 `typescript-language-server` | `js-debug` | 按 `package.json` / 工程自动探测 |
| **Python** | `pyright-langserver` 或 `ruff` | `debugpy` | Python 脚本 / 入口 |
| **Flutter / Android** | — | 通过 ADB 选择设备 | `flutter run` / `test` / `build apk` / `pub get` / `doctor` |

打开 Flutter 或 Android 工程后工具栏自动出现**设备**选择器，可在 `Ctrl+Alt+S` → **移动端 SDK** 中指定 Flutter SDK、Android SDK 与 JDK 路径（留空则自动探测）。

### Git 与终端

- **提交**（`Alt+0`）：变更文件树、IDEA 勾选式按需提交、行级 `similar` diff 与 AI 生成提交信息。
- **Git 历史**（`Alt+9`）：分支过滤、提交图、文件 diff、fetch、merge、rebase、stash、blame 与单文件历史。
- **终端**（`Alt+F12`）：基于 `portable-pty`（Windows ConPTY / Unix PTY），支持多标签与 VT100/ANSI 色彩。

### AI 助手（`ide-agent-hub`）

`Alt+8` 打开面板，统一驱动本机编码 Agent：

| 提供方 | 接入方式 |
| --- | --- |
| **Cursor** | 本地 CLI（Ask / Edit / Agent 模式会话隔离），`Ctrl+K` 行内编辑，以及持久化受控模式 `chrome-devtools-mcp` 桥接（`browser_bridge`） |
| **Google Antigravity** | ACP（基于 stdio 的 JSON-RPC 2.0，`agy`） |
| **Claude Code** | `claude` 无头 Stream-JSON + 内嵌 PTY |
| **OpenAI Codex** | `codex` exec + 内嵌 PTY |
| **自定义端点** | 兼容 OpenAI HTTP 或 Gemini REST |

支持 **Ask / Edit / Agent** 模式切换、推理深度选择、图片拖拽/粘贴、围栏代码块折叠、按发送时间排序的会话标签、排队等待区（编辑 / 撤销 / 关闭 / 立即发送）以及 `Ctrl+Alt+Z` 一键回滚工作区检查点。

---

## 快捷键

采用 IntelliJ IDEA 键位（Windows/Linux 主键为 `Ctrl`；macOS 上常用编辑与导航键同时支持 `Cmd` 映射）。

| 按键 | 动作 | 按键 | 动作 |
| --- | --- | --- | --- |
| 双击 `Shift` | Search Everywhere | `Ctrl+Alt+S` | 设置 |
| `Ctrl+Shift+F` | 在文件中查找 | `` Ctrl+` `` | 切换主题 |
| `Ctrl+Shift+A` | 查找命令 | `Ctrl+Alt+U` | 英文 / 中文 |
| `Ctrl+Shift+N` | 转到文件 | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | 放大 / 缩小 / 重置 |
| `Ctrl+B` | 转到声明 | `Ctrl+K` | 行内 AI 修改 |
| `Ctrl+D` / `Ctrl+Y` | 复制行 / 删除行 | `Alt+L` | 把选区发给 Agent |
| `Ctrl+/` | 切换行注释 | `Tab` / `Esc` | 接受 / 拒绝行内 diff |
| `Ctrl+Alt+L` | 格式化 | `Ctrl+Alt+Z` | 回滚最近一次 AI 检查点 |
| `Shift+F6` / `Alt+Enter` | 重命名 / 意图操作 | `Alt+G` / `Alt+C` / `Alt+X` | Antigravity / Claude / Codex |
| `Shift+F10` / `Shift+F9` | 运行 / 调试 | `Alt+1` / `Alt+0` / `Alt+9` | 项目 / 提交 / Git 历史 |
| `Ctrl+F8` | 切换断点 | `Alt+7` / `Alt+8` / `Alt+F12` | 结构 / Agent / 终端 |

---

## 环境要求

- **运行环境**：Windows 10/11、Linux（GTK3、D-Bus、X11/Wayland 与系统字体）、macOS 11+。
- **源码编译**：Rust stable（2021 edition）。Linux 下需安装 `libgtk-3-dev`、`libdbus-1-dev`、`pkg-config`、`fonts-dejavu-core`。
- **可选外部工具**（从 `PATH` 或设置中发现）：
  - 语言服务：`rust-analyzer`、`jdtls` / `java-language-server`、`vtsls` / `typescript-language-server`、`pyright-langserver` / `ruff`
  - 调试器：`codelldb`、`java-debug`、`js-debug`、`debugpy`
  - 构建与工具链：Cargo/Rustup、JDK 17+ 与 Maven/Gradle、Node.js、Python、Flutter SDK、Android SDK（`platform-tools`）
  - Agent：`agy`、`claude`、`codex`、Cursor CLI（浏览器桥接需本机 Node/`npx` 以拉起 `chrome-devtools-mcp`）

---

## 架构

工作区划分为 10 个 crate，协议与缓冲逻辑与 `ide-app` 桌面渲染层彻底解耦：

| Crate | 职责 |
| --- | --- |
| [`ide-core`](crates/ide-core) | 主题、键位、`LanguageId`、Cargo/Maven/Gradle/Node/Flutter 运行配置、移动端 SDK 探测 |
| [`ide-vfs`](crates/ide-vfs) | `ropey` 文本缓冲、撤销快照、工作区文件树与语言模板 |
| [`ide-syntax`](crates/ide-syntax) | 语法高亮、结构符号、可运行 gutter 标记（Rust、Java、TS/JS、Python 等） |
| [`ide-search`](crates/ide-search) | 基于 ripgrep 栈的 Search Everywhere 与在文件中查找 |
| [`ide-lsp`](crates/ide-lsp) | LSP JSON-RPC 客户端、本机服务发现、Maven/`javac` 编译器诊断解析 |
| [`ide-dap`](crates/ide-dap) | DAP 调试会话、断点、调用栈、局部变量与供 Agent 使用的崩溃上下文 |
| [`ide-git`](crates/ide-git) | 状态、暂存、提交、分支、历史图、blame 与有界行级 diff |
| [`ide-terminal`](crates/ide-terminal) | `portable-pty`（ConPTY / PTY）终端与 VT100 解析 |
| [`ide-agent-hub`](crates/ide-agent-hub) | Agent 注册、会话、等待区、ACP、CLI 运行器、`browser_bridge`、行内 diff 与检查点 |
| [`ide-app`](crates/ide-app) | `tao` 事件循环与 DirectWrite / `cosmic-text` + `tiny-skia` + `softbuffer` 渲染器 |

---

## 常见问题（FAQ）

- **RI 会捆绑 JVM、Chromium 或 Electron 吗？**
  不会。RI 使用纯 Rust 编译为单一原生二进制，界面与文本基于 `tao`、`tiny-skia`、`softbuffer`、`ropey` 及系统原生 API（Windows 下 DirectWrite/ConPTY，Linux/macOS 下 `cosmic-text`/PTY）。
- **RI 可以作为轻量级 IntelliJ IDEA 替代品用于 Rust 或 Java 开发吗？**
  可以。RI 复刻了 IntelliJ IDEA New UI（Islands Dark / Darcula / Islands Light）与完整 IDEA 键位，开箱支持 Cargo 工程与 Java Maven/Gradle/Spring Boot/JUnit 项目（`jdtls`、`java-debug`、gutter `▶` 运行），常驻内存仅约 59 MiB。
- **支持哪些本地 AI 编码 Agent？**
  支持 Cursor CLI（含 `Ctrl+K` 行内编辑与受控模式 `chrome-devtools-mcp` 浏览器自动化）、Google Antigravity（ACP）、Claude Code CLI、OpenAI Codex CLI 以及兼容 OpenAI/Gemini 的自定义端点。
- **支持 macOS 和 Linux 吗？**
  支持。每次 [GitHub Release](https://github.com/renyun2/rust-ai-ide/releases) 都会自动构建并附上 Windows（`x86_64`）、Linux（`x86_64`）与 macOS（Apple Silicon `arm64` 与 Intel `x86_64`）原生发行包。

---

## 开发与构建

```powershell
cargo check --workspace
cargo test --workspace
cargo run -p ide-app -- --self-check
```

发布配置（`Cargo.toml`）：`opt-level = 3`、`lto = "thin"`、`codegen-units = 1`、`strip = "symbols"`。开发配置将依赖项优化到 `opt-level = 3`，保证日常开发调试同样流畅。

---

## 发行版

在 GitHub 发布 Release 后，[`.github/workflows/release.yml`](.github/workflows/release.yml) 会在 `windows-latest`、`ubuntu-latest` 与 `macos-latest`（`arm64` / `x86_64`）上自动执行：

1. `cargo test --workspace` 与 `--self-check`
2. `cargo build -p ide-app --release`
3. 打包上传 Windows `.zip`、Linux `.tar.gz`、macOS `.app` `.zip` 及 `.sha256` 校验文件

推送到 `master`/`main` 或提交 PR 时，[`.github/workflows/ci.yml`](.github/workflows/ci.yml) 会自动运行全平台测试与自检。

---

## 仓库结构

```text
rust-ai-ide/
├── crates/
│   ├── ide-core/           # 主题、键位、运行配置（Cargo/Maven/Gradle/Flutter）、移动端 SDK
│   ├── ide-vfs/            # ropey 缓冲、文件树、Java/TS/Rust/Python 模板
│   ├── ide-syntax/         # 语法高亮、结构与可运行 gutter 标记
│   ├── ide-search/         # ripgrep 搜索
│   ├── ide-lsp/            # LSP 客户端与 Maven/javac 诊断
│   ├── ide-dap/            # DAP 客户端（codelldb、java-debug、js-debug、debugpy）
│   ├── ide-git/            # Git 状态、日志、diff、blame
│   ├── ide-terminal/       # ConPTY / PTY 终端
│   ├── ide-agent-hub/      # ACP、CLI 运行器、browser_bridge、检查点
│   └── ide-app/            # 桌面窗口、渲染器与状态持久化
├── .github/workflows/      # ci.yml、release.yml
├── assets/                 # ri_icon.svg、ri_icon.png、ri_icon.ico
├── Cargo.toml
├── README.md               # 英文说明
└── README.zh-CN.md         # 本文件
```

---

## 现状

版本 **0.5.0**。已支持 Windows、Linux 与 macOS 原生运行。界面布局、配色与快捷键对齐 IntelliJ IDEA New UI，所有实现均为独立纯 Rust 构建。

---

## 许可证

可任选其一：

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

详见 [`Cargo.toml`](Cargo.toml)（`MIT OR Apache-2.0`）。文件类型图标沿用 [`crates/ide-app/assets/file-icons/NOTICE.md`](crates/ide-app/assets/file-icons/NOTICE.md) 中的单独 MIT 条款。
