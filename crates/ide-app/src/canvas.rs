use anyhow::Result;
use std::path::Path;
use std::rc::Rc;
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, Path as SkiaPath,
    PathBuilder, Pixmap, Point, PremultipliedColorU8, Rect as SkiaRect, SpreadMode, Stroke,
    Transform,
};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSX};

#[cfg(target_os = "windows")]
type PlatformTextEngine = DWriteTextEngine;
#[cfg(not(target_os = "windows"))]
type PlatformTextEngine = crate::text_cosmic::CosmicTextEngine;

#[inline]
fn shared_text_engine_for_scale(scale: f32) -> Rc<PlatformTextEngine> {
    #[cfg(target_os = "windows")]
    {
        shared_dwrite_for_scale(scale)
    }
    #[cfg(not(target_os = "windows"))]
    {
        crate::text_cosmic::shared_text_engine_for_scale(scale)
    }
}

/// 检测当前系统的原生显示器 DPI 缩放倍率（例如 96 DPI -> 1.0, 120 DPI -> 1.25, 144 DPI -> 1.5, 192 DPI -> 2.0）
#[cfg(target_os = "windows")]
pub fn detect_system_dpi_scale() -> f32 {
    unsafe {
        let hdc = GetDC(None);
        if hdc.is_invalid() {
            return 1.0;
        }
        let dpi_x = GetDeviceCaps(Some(hdc), LOGPIXELSX);
        let _ = ReleaseDC(None, hdc);
        if dpi_x >= 72 {
            (dpi_x as f32 / 96.0).clamp(1.0, 4.0)
        } else {
            1.0
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn detect_system_dpi_scale() -> f32 {
    for key in ["GDK_SCALE", "QT_SCALE_FACTOR"] {
        if let Ok(val) = std::env::var(key) {
            if let Ok(scale) = val.trim().parse::<f32>() {
                if scale.is_finite() && scale > 0.0 {
                    return scale.clamp(1.0, 4.0);
                }
            }
        }
    }
    1.0
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontStyle {
    UiRegular,
    UiBold,
    UiSmall,
    CodeMono,
    CodeMonoBold,
    ChatBody,
    ChatBodyBold,
    ChatMeta,
    ChatMetaBold,
    ChatCode,
    ChatCodeBold,
    BadgeMicro,
}

/// 借鉴 WindUI (`wind-ui-rust`) 的纯 Rust 轻量级高分屏 (HiDPI) 渲染栈：
/// - 逻辑坐标与物理帧缓冲解耦：`width` / `height` 为逻辑布局像素，`phys_width` / `phys_height` 为物理像素 (`scale_factor`)
/// - 2D 矢量光栅化：`tiny-skia` CPU SIMD 抗锯齿路径/圆角/描边/合成引擎（原生 `Transform::from_scale` 物理分辨率光栅化）
/// - 文字排版与渲染：Windows 原生 `DirectWrite` (`IDWriteTextLayout` 按物理 DPI 字号渲染 + ClearType 真背景合成 + 彩色字形拆层)
/// - 窗口呈现：同步输出至 `softbuffer` 兼容的 BGRA (`0x00RRGGBB`) 物理帧缓冲
pub struct SurfaceCanvas {
    pub width: usize,
    pub height: usize,
    pub phys_width: usize,
    pub phys_height: usize,
    pub scale_factor: f32,
    pixmap: Pixmap,
    #[cfg(test)]
    present_buf: Vec<u32>,
    text_engine: Rc<PlatformTextEngine>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FileIconKind {
    Rust,
    Python,
    Notebook,
    TypeScript,
    Tsx,
    JavaScript,
    Jsx,
    Go,
    Java,
    Kotlin,
    C,
    Cpp,
    CSharp,
    Php,
    Ruby,
    Swift,
    Dart,
    Lua,
    Zig,
    Haskell,
    Elixir,
    Scala,
    RLang,
    Perl,
    Shell,
    PowerShell,
    Batch,
    Sql,
    Html,
    Css,
    Scss,
    Less,
    Vue,
    Svelte,
    Astro,
    Json,
    Yaml,
    Toml,
    Xml,
    GraphQl,
    Proto,
    Wasm,
    Groovy,
    Clojure,
    Erlang,
    OCaml,
    FSharp,
    Julia,
    Nim,
    Assembly,
    Solidity,
    Crystal,
    Elm,
    PureScript,
    Haxe,
    Gleam,
    Coffee,
    ReScript,
    Shader,
    Project,
    Qt,
    Unity,
    Godot,
    Http,
    Bazel,
    Cmake,
    Maven,
    Gradle,
    Meson,
    Helm,
    Jenkins,
    Vite,
    Webpack,
    Eslint,
    Prettier,
    Editorconfig,
    GithubActions,
    Gitlab,
    Nginx,
    Task,
    Just,
    Npm,
    Terraform,
    GoMod,
    Changelog,
    Authors,
    Codeowners,
    Kubernetes,
    Csv,
    Spreadsheet,
    Ini,
    Env,
    Markdown,
    Readme,
    Text,
    Log,
    Diff,
    Image,
    Svg,
    Pdf,
    Font,
    Audio,
    Video,
    Archive,
    Binary,
    Lock,
    Git,
    Docker,
    Make,
    License,
    Config,
    Database,
    Certificate,
    Unknown,
}

impl FileIconKind {
    fn from_path(path: &Path) -> Self {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if let Some(kind) = Self::from_special_name(&name) {
            return kind;
        }
        let in_workflows = path.components().any(|c| {
            c.as_os_str()
                .to_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("workflows"))
        });
        if in_workflows && (name.ends_with(".yml") || name.ends_with(".yaml")) {
            return Self::GithubActions;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        Self::from_extension(&ext)
    }

    fn from_language(lang: ide_core::LanguageId) -> Self {
        match lang {
            ide_core::LanguageId::Rust => Self::Rust,
            ide_core::LanguageId::TypeScript => Self::TypeScript,
            ide_core::LanguageId::JavaScript => Self::JavaScript,
            ide_core::LanguageId::Python => Self::Python,
            ide_core::LanguageId::Json => Self::Json,
            ide_core::LanguageId::Toml => Self::Toml,
            ide_core::LanguageId::Markdown => Self::Markdown,
            ide_core::LanguageId::PlainText => Self::Unknown,
        }
    }

    fn from_special_name(name: &str) -> Option<Self> {
        if name.ends_with(".tar.gz")
            || name.ends_with(".tar.bz2")
            || name.ends_with(".tar.xz")
            || name.ends_with(".tar.zst")
        {
            return Some(Self::Archive);
        }
        if name.ends_with(".lock")
            || name.ends_with("-lock.json")
            || name.ends_with("-lock.yaml")
            || name == "go.sum"
            || name == "npm-shrinkwrap.json"
        {
            return Some(Self::Lock);
        }
        if name == "dockerfile" || name.starts_with("dockerfile.") || name == ".dockerignore" {
            return Some(Self::Docker);
        }
        if name.starts_with("docker-compose") || name == "compose.yml" || name == "compose.yaml" {
            return Some(Self::Docker);
        }
        if name.starts_with(".env") {
            return Some(Self::Env);
        }
        if name.starts_with(".git") && name != ".github" {
            return Some(Self::Git);
        }
        if name == "license"
            || name.starts_with("license.")
            || name == "licence"
            || name.starts_with("licence.")
            || name == "copying"
            || name.starts_with("copying.")
        {
            return Some(Self::License);
        }
        if name == "readme" || name.starts_with("readme.") {
            return Some(Self::Readme);
        }
        if name == "makefile" || name == "gnumakefile" {
            return Some(Self::Make);
        }
        if name == "justfile" {
            return Some(Self::Just);
        }
        if name == "cmakelists.txt" || name == "cmakecache.txt" {
            return Some(Self::Cmake);
        }
        if name == "cargo.toml"
            || name == "rust-toolchain"
            || name.starts_with("rust-toolchain.")
            || name == "rustfmt.toml"
            || name == "clippy.toml"
        {
            return Some(Self::Rust);
        }
        if name == "pyproject.toml"
            || name == "requirements.txt"
            || name.starts_with("requirements-")
            || name == "pipfile"
            || name == "setup.cfg"
            || name == ".python-version"
        {
            return Some(Self::Python);
        }
        if name == "go.mod" {
            return Some(Self::GoMod);
        }
        if name == "package.json" {
            return Some(Self::Npm);
        }
        if name == "pom.xml" || name == "mvnw" || name == "mvnw.cmd" {
            return Some(Self::Maven);
        }
        if name == "build.gradle"
            || name == "settings.gradle"
            || name == "gradlew"
            || name == "gradlew.bat"
        {
            return Some(Self::Gradle);
        }
        if name == "gemfile"
            || name == "rakefile"
            || name == "vagrantfile"
            || name == ".ruby-version"
        {
            return Some(Self::Ruby);
        }
        if name == "composer.json" {
            return Some(Self::Php);
        }
        if name == "tsconfig.json" || (name.starts_with("tsconfig.") && name.ends_with(".json")) {
            return Some(Self::TypeScript);
        }
        if name == "jsconfig.json" || name == ".node-version" {
            return Some(Self::JavaScript);
        }
        if name.starts_with(".prettier") {
            return Some(Self::Prettier);
        }
        if name.starts_with(".eslint") {
            return Some(Self::Eslint);
        }
        if name == ".editorconfig" {
            return Some(Self::Editorconfig);
        }
        if name.starts_with(".babel")
            || name == ".npmrc"
            || name == ".nvmrc"
            || name == ".yarnrc"
            || name == ".yarnrc.yml"
            || name == ".htaccess"
            || name == "procfile"
        {
            return Some(Self::Config);
        }
        if name == ".bashrc"
            || name == ".zshrc"
            || name == ".profile"
            || name == ".bash_profile"
            || name == ".zprofile"
            || name == ".bash_aliases"
        {
            return Some(Self::Shell);
        }
        if name == "jenkinsfile" {
            return Some(Self::Jenkins);
        }
        if name == "containerfile"
            || name.starts_with("containerfile.")
            || name.ends_with(".dockerfile")
            || name == ".containerignore"
            || name == "earthfile"
            || name == "skaffold.yaml"
            || name == "skaffold.yml"
        {
            return Some(Self::Docker);
        }
        if name == "chart.yaml" || name == ".helmignore" {
            return Some(Self::Helm);
        }
        if name == "kustomization.yaml" || name == "kustomization.yml" {
            return Some(Self::Kubernetes);
        }
        if name == "build"
            || name == "build.bazel"
            || name == "workspace"
            || name == "workspace.bazel"
            || name == "module.bazel"
        {
            return Some(Self::Bazel);
        }
        if name == "meson.build" || name == "meson_options.txt" {
            return Some(Self::Meson);
        }
        if name == "taskfile.yml" || name == "taskfile.yaml" {
            return Some(Self::Task);
        }
        if name == "nginx.conf" {
            return Some(Self::Nginx);
        }
        if name.starts_with("vite.config.") {
            return Some(Self::Vite);
        }
        if name.starts_with("webpack.config.") {
            return Some(Self::Webpack);
        }
        if name == "build.ninja"
            || name == "build.xml"
            || name == "configure.ac"
            || name == "makefile.am"
            || name == "makefile.in"
        {
            return Some(Self::Make);
        }
        if name == "mix.exs" {
            return Some(Self::Elixir);
        }
        if name == "build.sbt" {
            return Some(Self::Scala);
        }
        if name == "cabal.project" || name == "stack.yaml" || name == "stack.yml" {
            return Some(Self::Haskell);
        }
        if name == "deny.toml" || name == "cross.toml" {
            return Some(Self::Rust);
        }
        if name == ".gitlab-ci.yml" {
            return Some(Self::Gitlab);
        }
        if name == "azure-pipelines.yml" {
            return Some(Self::Config);
        }
        if name == "changelog" {
            return Some(Self::Changelog);
        }
        if name == "authors" {
            return Some(Self::Authors);
        }
        if name == "codeowners" {
            return Some(Self::Codeowners);
        }
        if name == ".travis.yml"
            || name == "appveyor.yml"
            || name == "bitrise.yml"
            || name == "turbo.json"
            || name == "nx.json"
            || name == "lerna.json"
            || name == ".bazelrc"
            || name == ".bazelignore"
            || name == ".bazelversion"
            || name == ".clang-format"
            || name == ".clang-tidy"
            || name == ".clangd"
        {
            return Some(Self::Config);
        }
        if name == "changelog"
            || name == "authors"
            || name == "notice"
            || name == "codeowners"
            || name == "contributing"
        {
            return Some(Self::Text);
        }
        None
    }

    fn from_extension(ext: &str) -> Self {
        match ext {
            "rs" => Self::Rust,
            "py" | "pyi" | "pyw" | "pyx" => Self::Python,
            "ipynb" => Self::Notebook,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "js" | "mjs" | "cjs" => Self::JavaScript,
            "jsx" => Self::Jsx,
            "go" | "mod" => Self::Go,
            "java" => Self::Java,
            "kt" | "kts" => Self::Kotlin,
            "c" | "h" => Self::C,
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "inl" => Self::Cpp,
            "cs" => Self::CSharp,
            "php" | "twig" => Self::Php,
            "rb" | "rake" | "erb" => Self::Ruby,
            "swift" => Self::Swift,
            "dart" => Self::Dart,
            "lua" => Self::Lua,
            "zig" => Self::Zig,
            "hs" | "lhs" => Self::Haskell,
            "ex" | "exs" => Self::Elixir,
            "scala" => Self::Scala,
            "r" => Self::RLang,
            "pl" | "pm" => Self::Perl,
            "sh" | "bash" | "zsh" | "fish" | "awk" => Self::Shell,
            "ps1" | "psm1" | "psd1" => Self::PowerShell,
            "bat" | "cmd" => Self::Batch,
            "sql" | "psql" | "cql" => Self::Sql,
            "html" | "htm" | "xhtml" | "haml" => Self::Html,
            "css" | "pcss" | "styl" => Self::Css,
            "scss" | "sass" => Self::Scss,
            "less" => Self::Less,
            "vue" => Self::Vue,
            "svelte" => Self::Svelte,
            "astro" => Self::Astro,
            "json" | "jsonc" | "json5" | "jsonl" | "hjson" | "avsc" => Self::Json,
            "yml" | "yaml" => Self::Yaml,
            "toml" => Self::Toml,
            "xml" | "xsd" | "xsl" | "plist" | "rss" | "atom" => Self::Xml,
            "graphql" | "gql" => Self::GraphQl,
            "proto" | "thrift" => Self::Proto,
            "wasm" | "wat" => Self::Wasm,
            "groovy" | "gradle" => Self::Groovy,
            "clj" | "cljs" | "cljc" | "edn" => Self::Clojure,
            "erl" | "hrl" => Self::Erlang,
            "ml" | "mli" => Self::OCaml,
            "fs" | "fsx" | "fsi" => Self::FSharp,
            "jl" => Self::Julia,
            "nim" => Self::Nim,
            "asm" | "s" => Self::Assembly,
            "sol" => Self::Solidity,
            "cr" => Self::Crystal,
            "elm" => Self::Elm,
            "purs" => Self::PureScript,
            "hx" => Self::Haxe,
            "gleam" => Self::Gleam,
            "coffee" | "litcoffee" => Self::Coffee,
            "res" | "resi" => Self::ReScript,
            "glsl" | "hlsl" | "wgsl" | "vert" | "frag" | "geom" | "comp" | "tesc" | "tese"
            | "metal" | "shader" => Self::Shader,
            "sln" | "slnx" | "csproj" | "fsproj" | "vbproj" | "vcxproj" | "props" | "targets" => {
                Self::Project
            }
            "qml" | "ui" | "qrc" => Self::Qt,
            "unity" | "prefab" | "unitypackage" => Self::Unity,
            "gd" | "gdscript" | "tscn" | "tres" => Self::Godot,
            "http" | "rest" => Self::Http,
            "dockerfile" => Self::Docker,
            "ninja" => Self::Make,
            "sbt" => Self::Scala,
            "cabal" => Self::Haskell,
            "gemspec" => Self::Ruby,
            "nuspec" => Self::Xml,
            "nupkg" | "snupkg" => Self::Archive,
            "pro" | "pri" => Self::Make,
            "rlib" | "rmeta" | "pdb" | "ilk" | "exp" | "idb" | "pch" | "gch" => Self::Binary,
            "csv" | "tsv" => Self::Csv,
            "xls" | "xlsx" | "ods" => Self::Spreadsheet,
            "ini" | "cfg" | "conf" | "config" | "properties" => Self::Ini,
            "env" => Self::Env,
            "md" | "mdx" | "markdown" | "rmd" => Self::Markdown,
            "txt" | "rst" | "adoc" | "asciidoc" | "tex" | "latex" | "bib" | "org" | "rtf" => {
                Self::Text
            }
            "log" => Self::Log,
            "diff" | "patch" | "rej" => Self::Diff,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" | "tif" | "tiff"
            | "heic" => Self::Image,
            "svg" => Self::Svg,
            "pdf" => Self::Pdf,
            "ttf" | "otf" | "woff" | "woff2" | "eot" => Self::Font,
            "mp3" | "wav" | "ogg" | "flac" | "aac" | "m4a" => Self::Audio,
            "mp4" | "webm" | "mkv" | "mov" | "avi" => Self::Video,
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "dmg" | "iso" | "jar"
            | "war" | "apk" | "deb" | "rpm" => Self::Archive,
            "exe" | "dll" | "so" | "dylib" | "bin" | "o" | "obj" | "a" | "lib" | "class"
            | "msi" => Self::Binary,
            "db" | "sqlite" | "sqlite3" | "mdb" | "prisma" => Self::Database,
            "doc" | "docx" | "ppt" | "pptx" | "odt" | "odp" => Self::Text,
            "pem" | "crt" | "cer" | "key" | "pub" | "p12" | "pfx" => Self::Certificate,
            "cmake" => Self::Cmake,
            "bzl" | "bazel" => Self::Bazel,
            "mk" => Self::Make,
            "m" | "mm" => Self::C,
            "v" | "sv" | "vhd" | "vhdl" => Self::Assembly,
            "tf" | "tfvars" => Self::Terraform,
            "hcl" | "nix" => Self::Config,
            "ll" => Self::Binary,
            _ => Self::Unknown,
        }
    }

    fn icon_name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::Notebook => "jupyter",
            Self::TypeScript => "typescript",
            Self::Tsx => "react-ts",
            Self::JavaScript => "javascript",
            Self::Jsx => "react",
            Self::Go => "go",
            Self::GoMod => "go-mod",
            Self::Java => "java",
            Self::Kotlin => "kotlin",
            Self::C => "c",
            Self::Cpp | Self::Qt => "cpp",
            Self::CSharp => "csharp",
            Self::Php => "php",
            Self::Ruby => "ruby",
            Self::Swift => "swift",
            Self::Dart => "dart",
            Self::Lua => "lua",
            Self::Zig => "zig",
            Self::Haskell => "haskell",
            Self::Elixir => "elixir",
            Self::Scala => "scala",
            Self::RLang => "r",
            Self::Perl => "perl",
            Self::Shell | Self::Batch => "console",
            Self::PowerShell => "powershell",
            Self::Sql | Self::Database => "database",
            Self::Html => "html",
            Self::Css => "css",
            Self::Scss => "sass",
            Self::Less => "less",
            Self::Vue => "vue",
            Self::Svelte => "svelte",
            Self::Astro => "astro",
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Xml => "xml",
            Self::GraphQl => "graphql",
            Self::Proto => "proto",
            Self::Wasm => "webassembly",
            Self::Groovy => "groovy",
            Self::Clojure => "clojure",
            Self::Erlang => "erlang",
            Self::OCaml => "ocaml",
            Self::FSharp => "fsharp",
            Self::Julia => "julia",
            Self::Nim => "nim",
            Self::Assembly => "assembly",
            Self::Solidity => "solidity",
            Self::Crystal => "crystal",
            Self::Elm => "elm",
            Self::PureScript => "purescript",
            Self::Haxe => "haxe",
            Self::Gleam => "gleam",
            Self::Coffee => "coffee",
            Self::ReScript => "rescript",
            Self::Shader => "shader",
            Self::Project => "visualstudio",
            Self::Unity => "unity",
            Self::Godot => "godot",
            Self::Http => "http",
            Self::Csv | Self::Spreadsheet => "table",
            Self::Ini | Self::Config => "settings",
            Self::Env => "key",
            Self::Markdown => "markdown",
            Self::Readme => "readme",
            Self::Text | Self::Unknown => "document",
            Self::Log => "log",
            Self::Diff => "diff",
            Self::Image => "image",
            Self::Svg => "svg",
            Self::Pdf => "pdf",
            Self::Font => "font",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Archive => "zip",
            Self::Binary => "exe",
            Self::Lock => "lock",
            Self::Git => "git",
            Self::Docker => "docker",
            Self::Make => "makefile",
            Self::License => "license",
            Self::Certificate => "certificate",
            Self::Bazel => "bazel",
            Self::Cmake => "cmake",
            Self::Maven => "maven",
            Self::Gradle => "gradle",
            Self::Meson => "meson",
            Self::Helm => "helm",
            Self::Jenkins => "jenkins",
            Self::Vite => "vite",
            Self::Webpack => "webpack",
            Self::Eslint => "eslint",
            Self::Prettier => "prettier",
            Self::Editorconfig => "editorconfig",
            Self::GithubActions => "github-actions-workflow",
            Self::Gitlab => "gitlab",
            Self::Nginx => "nginx",
            Self::Task => "taskfile",
            Self::Just => "just",
            Self::Npm => "npm",
            Self::Terraform => "terraform",
            Self::Changelog => "changelog",
            Self::Authors => "authors",
            Self::Codeowners => "codeowners",
            Self::Kubernetes => "kubernetes",
        }
    }
}

impl SurfaceCanvas {
    #[allow(dead_code)]
    pub fn new(width: usize, height: usize) -> Self {
        Self::new_with_scale(width, height, 1.0)
    }

    pub fn new_with_scale(phys_width: usize, phys_height: usize, scale_factor: f32) -> Self {
        let scale = scale_factor.clamp(0.75, 4.0);
        let pw = phys_width.max(1);
        let ph = phys_height.max(1);
        let lw = ((pw as f32) / scale).round().max(1.0) as usize;
        let lh = ((ph as f32) / scale).round().max(1.0) as usize;
        let pixmap =
            Pixmap::new(pw as u32, ph as u32).expect("Failed to allocate tiny-skia Pixmap");
        #[cfg(test)]
        let present_buf = vec![0u32; pw * ph];
        let text_engine = shared_text_engine_for_scale(scale);
        Self {
            width: lw,
            height: lh,
            phys_width: pw,
            phys_height: ph,
            scale_factor: scale,
            pixmap,
            #[cfg(test)]
            present_buf,
            text_engine,
        }
    }

    pub fn set_scale_factor(&mut self, scale_factor: f32) {
        let scale = scale_factor.clamp(0.75, 4.0);
        if (self.scale_factor - scale).abs() > 0.005 {
            self.text_engine = shared_text_engine_for_scale(scale);
        }
        self.scale_factor = scale;
        self.width = ((self.phys_width as f32) / scale).round().max(1.0) as usize;
        self.height = ((self.phys_height as f32) / scale).round().max(1.0) as usize;
    }

    #[allow(dead_code)]
    pub fn resize(&mut self, width: usize, height: usize) {
        let lw = width.max(1);
        let lh = height.max(1);
        let pw = ((lw as f32) * self.scale_factor).round().max(1.0) as usize;
        let ph = ((lh as f32) * self.scale_factor).round().max(1.0) as usize;
        self.resize_physical(pw, ph, self.scale_factor);
    }

    pub fn resize_physical(&mut self, phys_width: usize, phys_height: usize, scale_factor: f32) {
        let scale = scale_factor.clamp(0.75, 4.0);
        let pw = phys_width.max(1);
        let ph = phys_height.max(1);
        let lw = ((pw as f32) / scale).round().max(1.0) as usize;
        let lh = ((ph as f32) / scale).round().max(1.0) as usize;
        if (self.scale_factor - scale).abs() > 0.005 {
            self.text_engine = shared_text_engine_for_scale(scale);
        }
        self.scale_factor = scale;
        self.width = lw;
        self.height = lh;
        if pw == self.phys_width
            && ph == self.phys_height
            && self.pixmap.width() as usize == pw
            && self.pixmap.height() as usize == ph
        {
            return;
        }
        self.pixmap = Pixmap::new(pw as u32, ph as u32).expect("Failed to resize tiny-skia Pixmap");
        #[cfg(test)]
        self.present_buf.resize(pw * ph, 0);
        self.phys_width = pw;
        self.phys_height = ph;
    }

    /// Shrinks the window's RGBA framebuffer to 1x1 while minimized to release ~10-30MB immediately.
    pub fn shrink_for_minimized(&mut self) {
        if self.pixmap.width() <= 1 && self.pixmap.height() <= 1 && self.phys_width <= 1 {
            return;
        }
        if let Some(tiny) = Pixmap::new(1, 1) {
            self.pixmap = tiny;
            #[cfg(test)]
            self.present_buf.resize(1, 0);
            self.phys_width = 1;
            self.phys_height = 1;
            self.width = 1;
            self.height = 1;
        }
    }

    /// 释放非活跃后台窗口的 `tiny-skia` 离屏像素缓冲（保留逻辑尺寸以维持点击/悬停坐标计算），
    /// 下次 `render_frame` 时按需重新分配；单个 1.5x/2x 窗口可立即回收 13–33 MB。
    pub fn release_idle_framebuffer(&mut self) {
        if self.pixmap.width() <= 1 && self.pixmap.height() <= 1 {
            return;
        }
        if let Some(tiny) = Pixmap::new(1, 1) {
            self.pixmap = tiny;
        }
    }

    /// 清理字体排版与字形掩码缓存（供最小化或内存回收时调用）
    pub fn trim_text_caches(&self) {
        self.text_engine.trim_caches();
    }

    #[inline]
    pub fn to_logical_pos(&self, phys_x: f64, phys_y: f64) -> (i32, i32) {
        let s = self.scale_factor.max(0.5) as f64;
        ((phys_x / s).round() as i32, (phys_y / s).round() as i32)
    }

    #[inline]
    pub fn to_physical_pos(&self, log_x: i32, log_y: i32) -> (i32, i32) {
        let s = self.scale_factor;
        (
            (log_x as f32 * s).round() as i32,
            (log_y as f32 * s).round() as i32,
        )
    }

    #[inline]
    fn skia_transform(&self) -> Transform {
        Transform::from_scale(self.scale_factor, self.scale_factor)
    }

    /// 将 `tiny-skia` RGBA 帧缓冲转换为 `softbuffer` 所需的 `0x00RRGGBB` 像素切片（仅测试使用）
    #[cfg(test)]
    pub fn pixels(&mut self) -> &[u32] {
        let rgba = self.pixmap.data();
        for (dst, src) in self.present_buf.iter_mut().zip(rgba.chunks_exact(4)) {
            *dst = ((src[0] as u32) << 16) | ((src[1] as u32) << 8) | (src[2] as u32);
        }
        &self.present_buf
    }

    /// 单遍零中间缓冲直写 `softbuffer` 表面切片（省去中间 `present_buf` 拷贝，大幅降低高分屏帧耗时）
    pub fn copy_to_softbuffer(&self, dst_buf: &mut [u32]) {
        let rgba = self.pixmap.data();
        for (dst, src) in dst_buf.iter_mut().zip(rgba.chunks_exact(4)) {
            *dst = ((src[0] as u32) << 16) | ((src[1] as u32) << 8) | (src[2] as u32);
        }
    }

    pub fn clear(&mut self, color: u32) {
        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;
        if let Some(p) = PremultipliedColorU8::from_rgba(r, g, b, 255) {
            self.pixmap.pixels_mut().fill(p);
        }
    }

    #[inline]
    fn fill_phys_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: u32, alpha: u8) {
        let pw = self.phys_width as i32;
        let ph = self.phys_height as i32;
        let cx0 = x0.max(0);
        let cy0 = y0.max(0);
        let cx1 = x1.min(pw);
        let cy1 = y1.min(ph);
        if cx1 <= cx0 || cy1 <= cy0 || alpha == 0 {
            return;
        }
        let cr = ((color >> 16) & 0xFF) as u32;
        let cg = ((color >> 8) & 0xFF) as u32;
        let cb = (color & 0xFF) as u32;
        let pw_usize = self.phys_width;
        let ux0 = cx0 as usize;
        let ux1 = cx1 as usize;
        let px = self.pixmap.pixels_mut();

        if alpha == 255 {
            if let Some(solid) = PremultipliedColorU8::from_rgba(cr as u8, cg as u8, cb as u8, 255)
            {
                for row in (cy0 as usize)..(cy1 as usize) {
                    let base = row * pw_usize;
                    px[base + ux0..base + ux1].fill(solid);
                }
            }
        } else {
            let a = alpha as u32;
            let inv_a = 255 - a;
            for row in (cy0 as usize)..(cy1 as usize) {
                let base = row * pw_usize;
                for dst in &mut px[base + ux0..base + ux1] {
                    let nr = ((dst.red() as u32 * inv_a + cr * a + 127) / 255) as u8;
                    let ng = ((dst.green() as u32 * inv_a + cg * a + 127) / 255) as u8;
                    let nb = ((dst.blue() as u32 * inv_a + cb * a + 127) / 255) as u8;
                    if let Some(p) = PremultipliedColorU8::from_rgba(nr, ng, nb, 255) {
                        *dst = p;
                    }
                }
            }
        }
    }

    pub fn fill_rect(&mut self, rect: Rect, color: u32) {
        self.fill_rect_alpha(rect, color, 255);
    }

    pub fn fill_rect_alpha(&mut self, rect: Rect, color: u32, alpha: u8) {
        if rect.w <= 0 || rect.h <= 0 || alpha == 0 {
            return;
        }
        let s = self.scale_factor;
        let x0 = (rect.x as f32 * s).round() as i32;
        let y0 = (rect.y as f32 * s).round() as i32;
        let x1 = ((rect.x + rect.w) as f32 * s).round() as i32;
        let y1 = ((rect.y + rect.h) as f32 * s).round() as i32;
        self.fill_phys_rect(x0, y0, x1, y1, color, alpha);
    }

    /// 使用扫描线硬件 `slice.fill` + 四角 SDF 抗锯齿绘制极速圆角矩形
    pub fn fill_rounded_rect(&mut self, rect: Rect, radius: i32, color: u32) {
        self.fill_rounded_rect_alpha(rect, radius, color, 255);
    }

    /// 仅顶部两角带圆角的实心矩形（底部直角平齐，不会向上覆盖前一行文字）
    pub fn fill_top_rounded_rect(&mut self, rect: Rect, radius: i32, color: u32) {
        self.fill_rounded_rect_corners(rect, radius, color, 255, true, false);
    }

    /// 仅底部两角带圆角的实心矩形（顶部直角平齐，不会向上覆盖前一行文字）
    pub fn fill_bottom_rounded_rect(&mut self, rect: Rect, radius: i32, color: u32) {
        self.fill_rounded_rect_corners(rect, radius, color, 255, false, true);
    }

    pub fn fill_rounded_rect_alpha(&mut self, rect: Rect, radius: i32, color: u32, alpha: u8) {
        self.fill_rounded_rect_corners(rect, radius, color, alpha, true, true);
    }

    fn fill_rounded_rect_corners(
        &mut self,
        rect: Rect,
        radius: i32,
        color: u32,
        alpha: u8,
        round_top: bool,
        round_bottom: bool,
    ) {
        if rect.w <= 0 || rect.h <= 0 || alpha == 0 {
            return;
        }
        if radius <= 0 || (!round_top && !round_bottom) {
            self.fill_rect_alpha(rect, color, alpha);
            return;
        }
        let s = self.scale_factor;
        let x0 = (rect.x as f32 * s).round() as i32;
        let y0 = (rect.y as f32 * s).round() as i32;
        let x1 = ((rect.x + rect.w) as f32 * s).round() as i32;
        let y1 = ((rect.y + rect.h) as f32 * s).round() as i32;
        let w = x1 - x0;
        let h = y1 - y0;
        if w <= 0 || h <= 0 {
            return;
        }
        let max_r_v = if round_top && round_bottom { h / 2 } else { h };
        let r = ((radius as f32 * s).round() as i32).clamp(0, (w / 2).min(max_r_v));
        if r <= 0 {
            self.fill_phys_rect(x0, y0, x1, y1, color, alpha);
            return;
        }

        // 1. 中央主体全宽条带 + 上下水平条带直接走硬件 slice.fill
        let top_r = if round_top { r } else { 0 };
        let bot_r = if round_bottom { r } else { 0 };
        self.fill_phys_rect(x0, y0 + top_r, x1, y1 - bot_r, color, alpha);
        if round_top {
            self.fill_phys_rect(x0 + r, y0, x1 - r, y0 + r, color, alpha);
        }
        if round_bottom {
            self.fill_phys_rect(x0 + r, y1 - r, x1 - r, y1, color, alpha);
        }

        // 2. 仅对启用的圆角 r×r 像素执行精确 SDF 抗锯齿混合
        let pw = self.phys_width as i32;
        let ph = self.phys_height as i32;
        let cr = ((color >> 16) & 0xFF) as u32;
        let cg = ((color >> 8) & 0xFF) as u32;
        let cb = (color & 0xFF) as u32;
        let rf = r as f32;
        let base_a = alpha as f32;
        let pw_usize = self.phys_width;
        let px = self.pixmap.pixels_mut();

        let mut corners = [(0i32, 0i32, 0.0f32, 0.0f32); 4];
        let mut corner_len = 0usize;
        if round_top {
            corners[corner_len] = (x0, y0, (x0 + r) as f32, (y0 + r) as f32);
            corners[corner_len + 1] = (x1 - r, y0, (x1 - r) as f32, (y0 + r) as f32);
            corner_len += 2;
        }
        if round_bottom {
            corners[corner_len] = (x0, y1 - r, (x0 + r) as f32, (y1 - r) as f32);
            corners[corner_len + 1] = (x1 - r, y1 - r, (x1 - r) as f32, (y1 - r) as f32);
            corner_len += 2;
        }

        for &(cx_start, cy_start, ccx, ccy) in &corners[..corner_len] {
            let py_min = cy_start.max(0);
            let py_max = (cy_start + r).min(ph);
            let px_min = cx_start.max(0);
            let px_max = (cx_start + r).min(pw);
            for py in py_min..py_max {
                let dy = (py as f32 + 0.5) - ccy;
                let row_base = (py as usize) * pw_usize;
                for px_x in px_min..px_max {
                    let dx = (px_x as f32 + 0.5) - ccx;
                    let dist = dx.hypot(dy) - rf;
                    let cov = (0.5 - dist).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        let a = (cov * base_a).round() as u32;
                        let dst = &mut px[row_base + (px_x as usize)];
                        if a >= 255 {
                            if let Some(p) =
                                PremultipliedColorU8::from_rgba(cr as u8, cg as u8, cb as u8, 255)
                            {
                                *dst = p;
                            }
                        } else if a > 0 {
                            let inv = 255 - a;
                            let nr = ((dst.red() as u32 * inv + cr * a + 127) / 255) as u8;
                            let ng = ((dst.green() as u32 * inv + cg * a + 127) / 255) as u8;
                            let nb = ((dst.blue() as u32 * inv + cb * a + 127) / 255) as u8;
                            if let Some(p) = PremultipliedColorU8::from_rgba(nr, ng, nb, 255) {
                                *dst = p;
                            }
                        }
                    }
                }
            }
        }
    }

    /// 为浮层菜单与弹窗绘制柔和的多层投影（WindUI / IDEA 风格悬浮层质感）
    pub fn fill_rounded_shadow(&mut self, rect: Rect, radius: i32) {
        self.fill_rounded_rect_alpha(
            Rect::new(rect.x - 2, rect.y + 4, rect.w + 4, rect.h + 4),
            radius + 3,
            0x000000,
            24,
        );
        self.fill_rounded_rect_alpha(
            Rect::new(rect.x - 1, rect.y + 2, rect.w + 2, rect.h + 2),
            radius + 2,
            0x000000,
            38,
        );
    }

    /// 绘制 1px 矩形外框（用于无边框自定义窗口外边缘精修）
    pub fn stroke_rect_border(&mut self, rect: Rect, color: u32) {
        if rect.w <= 1 || rect.h <= 1 {
            return;
        }
        self.fill_rect(Rect::new(rect.x, rect.y, rect.w, 1), color);
        self.fill_rect(Rect::new(rect.x, rect.y + rect.h - 1, rect.w, 1), color);
        self.fill_rect(Rect::new(rect.x, rect.y, 1, rect.h), color);
        self.fill_rect(Rect::new(rect.x + rect.w - 1, rect.y, 1, rect.h), color);
    }

    /// 使用扫描线 + 四角圆环 SDF 绘制带 1px 清晰描边的抗锯齿圆角矩形
    pub fn stroke_rounded_rect(
        &mut self,
        rect: Rect,
        radius: i32,
        border_color: u32,
        fill_color: u32,
    ) {
        if rect.w <= 0 || rect.h <= 0 {
            return;
        }
        self.fill_rounded_rect(rect, radius, fill_color);
        if rect.w < 2 || rect.h < 2 {
            return;
        }
        let s = self.scale_factor;
        let x0 = (rect.x as f32 * s).round() as i32;
        let y0 = (rect.y as f32 * s).round() as i32;
        let x1 = ((rect.x + rect.w) as f32 * s).round() as i32;
        let y1 = ((rect.y + rect.h) as f32 * s).round() as i32;
        let w = x1 - x0;
        let h = y1 - y0;
        if w < 2 || h < 2 {
            return;
        }
        let bw = (s.round() as i32).max(1);
        let r = ((radius as f32 * s).round() as i32).clamp(0, (w / 2).min(h / 2));

        // 1. 四条直线边框直接走硬件 slice.fill
        self.fill_phys_rect(x0 + r, y0, x1 - r, y0 + bw, border_color, 255);
        self.fill_phys_rect(x0 + r, y1 - bw, x1 - r, y1, border_color, 255);
        self.fill_phys_rect(x0, y0 + r, x0 + bw, y1 - r, border_color, 255);
        self.fill_phys_rect(x1 - bw, y0 + r, x1, y1 - r, border_color, 255);

        if r <= 0 {
            return;
        }

        // 2. 四角 r×r 像素执行圆环 SDF 抗锯齿描边
        let pw = self.phys_width as i32;
        let ph = self.phys_height as i32;
        let br = ((border_color >> 16) & 0xFF) as u32;
        let bg = ((border_color >> 8) & 0xFF) as u32;
        let bb = (border_color & 0xFF) as u32;
        let rf = r as f32;
        let inner_r = (rf - bw as f32).max(0.0);
        let pw_usize = self.phys_width;
        let px = self.pixmap.pixels_mut();

        let corners = [
            (x0, y0, (x0 + r) as f32, (y0 + r) as f32),
            (x1 - r, y0, (x1 - r) as f32, (y0 + r) as f32),
            (x0, y1 - r, (x0 + r) as f32, (y1 - r) as f32),
            (x1 - r, y1 - r, (x1 - r) as f32, (y1 - r) as f32),
        ];

        for &(cx_start, cy_start, ccx, ccy) in &corners {
            let py_min = cy_start.max(0);
            let py_max = (cy_start + r).min(ph);
            let px_min = cx_start.max(0);
            let px_max = (cx_start + r).min(pw);
            for py in py_min..py_max {
                let dy = (py as f32 + 0.5) - ccy;
                let row_base = (py as usize) * pw_usize;
                for px_x in px_min..px_max {
                    let dx = (px_x as f32 + 0.5) - ccx;
                    let d = dx.hypot(dy);
                    let outer_cov = (rf + 0.5 - d).clamp(0.0, 1.0);
                    let inner_cov = (d - (inner_r - 0.5)).clamp(0.0, 1.0);
                    let cov = outer_cov * inner_cov;
                    if cov > 0.0 {
                        let a = (cov * 255.0).round() as u32;
                        let dst = &mut px[row_base + (px_x as usize)];
                        if a >= 255 {
                            if let Some(p) =
                                PremultipliedColorU8::from_rgba(br as u8, bg as u8, bb as u8, 255)
                            {
                                *dst = p;
                            }
                        } else if a > 0 {
                            let inv = 255 - a;
                            let nr = ((dst.red() as u32 * inv + br * a + 127) / 255) as u8;
                            let ng = ((dst.green() as u32 * inv + bg * a + 127) / 255) as u8;
                            let nb = ((dst.blue() as u32 * inv + bb * a + 127) / 255) as u8;
                            if let Some(p) = PremultipliedColorU8::from_rgba(nr, ng, nb, 255) {
                                *dst = p;
                            }
                        }
                    }
                }
            }
        }
    }

    /// `tiny-skia` 矢量抗锯齿实心圆（用于红点断点、状态灯）
    pub fn fill_circle(&mut self, cx: i32, cy: i32, r: i32, color: u32) {
        if r <= 0 {
            return;
        }
        let mut pb = PathBuilder::new();
        pb.push_circle(cx as f32 + 0.5, cy as f32 + 0.5, r as f32);
        if let Some(path) = pb.finish() {
            let paint = solid_paint(color, 255);
            let xform = self.skia_transform();
            self.pixmap
                .fill_path(&path, &paint, FillRule::Winding, xform, None);
        }
    }

    /// `tiny-skia` 矢量抗锯齿空心圆环
    pub fn stroke_circle(&mut self, cx: i32, cy: i32, r: f32, thickness: f32, color: u32) {
        if r <= 0.0 || thickness <= 0.0 {
            return;
        }
        let mut pb = PathBuilder::new();
        pb.push_circle(cx as f32 + 0.5, cy as f32 + 0.5, r);
        if let Some(path) = pb.finish() {
            let paint = solid_paint(color, 255);
            let stroke = Stroke {
                width: thickness,
                ..Stroke::default()
            };
            let xform = self.skia_transform();
            self.pixmap.stroke_path(&path, &paint, &stroke, xform, None);
        }
    }

    /// 极简克制的细圆弧加载动画指示器（自然平滑旋转，无夸张弹跳）
    pub fn draw_subtle_spinner(
        &mut self,
        cx: i32,
        cy: i32,
        radius: f32,
        phase_rad: f32,
        fg_color: u32,
        track_color: u32,
        alpha: f32,
    ) {
        let a = alpha.clamp(0.0, 1.0);
        if radius <= 1.0 || a <= 0.02 {
            return;
        }
        let fcx = cx as f32 + 0.5;
        let fcy = cy as f32 + 0.5;
        let xform = self.skia_transform();

        // 1. 柔和底环轨道
        let track_alpha = (a * 68.0).round() as u8;
        if track_alpha > 0 {
            let mut tp = PathBuilder::new();
            tp.push_circle(fcx, fcy, radius);
            if let Some(path) = tp.finish() {
                let paint = solid_paint(track_color, track_alpha);
                let stroke = Stroke {
                    width: 1.5,
                    ..Stroke::default()
                };
                self.pixmap.stroke_path(&path, &paint, &stroke, xform, None);
            }
        }

        // 2. 平滑旋转圆弧（~210° 圆弧，圆角端点）
        let mut ap = PathBuilder::new();
        let sweep = std::f32::consts::PI * 1.18;
        let steps = 14usize;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let ang = phase_rad + t * sweep;
            let px = fcx + ang.cos() * radius;
            let py = fcy + ang.sin() * radius;
            if i == 0 {
                ap.move_to(px, py);
            } else {
                ap.line_to(px, py);
            }
        }
        if let Some(arc_path) = ap.finish() {
            let fg_alpha = (a * 235.0).round() as u8;
            let paint = solid_paint(fg_color, fg_alpha);
            let stroke = Stroke {
                width: 1.7,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            self.pixmap
                .stroke_path(&arc_path, &paint, &stroke, xform, None);
        }
    }

    /// `tiny-skia` 圆角端点矢量抗锯齿线段绘制
    pub fn draw_line_aa(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32, color: u32) {
        let mut pb = PathBuilder::new();
        pb.move_to(x0 + 0.5, y0 + 0.5);
        pb.line_to(x1 + 0.5, y1 + 0.5);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, thickness, color);
        }
    }

    fn stroke_path_round(&mut self, path: &SkiaPath, thickness: f32, color: u32) {
        let paint = solid_paint(color, 255);
        let stroke = Stroke {
            width: thickness,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        let xform = self.skia_transform();
        self.pixmap.stroke_path(path, &paint, &stroke, xform, None);
    }

    fn fill_path_solid(&mut self, path: &SkiaPath, color: u32) {
        let paint = solid_paint(color, 255);
        let xform = self.skia_transform();
        self.pixmap
            .fill_path(path, &paint, FillRule::Winding, xform, None);
    }

    /// IDEA 标志性绿色运行三角 ▶（`tiny-skia` 矢量抗锯齿）
    pub fn fill_play_triangle(&mut self, x: i32, y: i32, size: i32, color: u32) {
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let s = size as f32;
        let mut pb = PathBuilder::new();
        pb.move_to(fx, fy);
        pb.line_to(fx + s * 0.85, fy + s * 0.5);
        pb.line_to(fx, fy + s);
        pb.close();
        if let Some(path) = pb.finish() {
            self.fill_path_solid(&path, color);
        }
    }

    /// 绘制平滑下拉箭头图标 ▾
    pub fn draw_chevron_down(&mut self, x: i32, y: i32, color: u32) {
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx, fy);
        pb.line_to(fx + 4.0, fy + 4.0);
        pb.line_to(fx + 8.0, fy);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.5, color);
        }
    }

    /// 绘制折叠状态向右箭头 ▸
    pub fn draw_chevron_right(&mut self, x: i32, y: i32, color: u32) {
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx, fy);
        pb.line_to(fx + 4.0, fy + 4.0);
        pb.line_to(fx, fy + 8.0);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.5, color);
        }
    }

    /// 绘制 IDEA 风格放大镜搜索图标
    pub fn draw_search_icon(&mut self, x: i32, y: i32, color: u32) {
        self.stroke_circle(x + 6, y + 6, 4.8, 1.5, color);
        self.draw_line_aa(
            (x + 10) as f32,
            (y + 10) as f32,
            (x + 14) as f32,
            (y + 14) as f32,
            1.8,
            color,
        );
    }

    /// 绘制 IDEA 顶部三横杠主菜单图标 (Hamburger Menu)
    pub fn draw_hamburger_icon(&mut self, x: i32, y: i32, color: u32) {
        self.fill_rounded_rect(Rect::new(x, y, 14, 2), 1, color);
        self.fill_rounded_rect(Rect::new(x, y + 5, 14, 2), 1, color);
        self.fill_rounded_rect(Rect::new(x, y + 10, 14, 2), 1, color);
    }

    /// 绘制 IDEA 风格 Git 分支矢量图标
    pub fn draw_git_branch_icon(&mut self, x: i32, y: i32, color: u32) {
        self.fill_circle(x + 2, y + 2, 2, color);
        self.fill_circle(x + 2, y + 11, 2, color);
        self.fill_circle(x + 9, y + 5, 2, color);
        self.draw_line_aa(
            (x + 2) as f32,
            (y + 2) as f32,
            (x + 2) as f32,
            (y + 11) as f32,
            1.6,
            color,
        );
        let mut pb = PathBuilder::new();
        pb.move_to((x + 2) as f32 + 0.5, (y + 9) as f32 + 0.5);
        pb.quad_to(
            (x + 3) as f32 + 0.5,
            (y + 5) as f32 + 0.5,
            (x + 9) as f32 + 0.5,
            (y + 5) as f32 + 0.5,
        );
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.5, color);
        }
    }

    /// 绘制文件夹矢量图标（vscode-icons）
    pub fn draw_folder_icon(&mut self, x: i32, y: i32, color: u32) {
        let _ = color;
        crate::svg_icon::paint_icon(
            &mut self.pixmap,
            self.scale_factor,
            x as f32,
            (y - 1) as f32,
            16.0,
            16.0,
            "folder",
        );
    }

    /// 绘制新版 IDEA 顶部空心绿色运行三角 ▶
    pub fn stroke_play_triangle(&mut self, x: i32, y: i32, size: i32, color: u32) {
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let s = size as f32;
        let mut pb = PathBuilder::new();
        pb.move_to(fx, fy);
        pb.line_to(fx + s * 0.85, fy + s * 0.5);
        pb.line_to(fx, fy + s);
        pb.close();
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.6, color);
        }
    }

    /// 绘制脚本运行中的暂停/停止双竖条图标 ❚❚
    pub fn draw_pause_icon(&mut self, x: i32, y: i32, size: i32, color: u32) {
        let bar_w = (size / 4).max(2);
        let bar_h = size.max(6);
        let gap = (size / 4).max(2);
        let start_x = x + ((size - bar_w * 2 - gap) / 2).max(0);
        self.fill_rounded_rect(Rect::new(start_x, y, bar_w, bar_h), 1, color);
        self.fill_rounded_rect(Rect::new(start_x + bar_w + gap, y, bar_w, bar_h), 1, color);
    }

    /// 绘制移动端手机设备矢量图标 📱
    pub fn draw_mobile_phone_icon(&mut self, x: i32, y: i32, color: u32) {
        self.stroke_rect_border(Rect::new(x + 3, y + 1, 9, 13), color);
        self.fill_rect(Rect::new(x + 6, y + 3, 3, 1), color);
        self.fill_rect(Rect::new(x + 6, y + 11, 3, 1), color);
    }

    /// 绘制新版 IDEA 顶部设置齿轮矢量图标 ⚙
    pub fn draw_gear_icon(&mut self, x: i32, y: i32, color: u32) {
        let cx = x + 7;
        let cy = y + 7;
        self.stroke_circle(cx, cy, 4.4, 1.5, color);
        self.fill_circle(cx, cy, 1, color);
        for &(dx, dy) in &[
            (0, -6),
            (0, 6),
            (-6, 0),
            (6, 0),
            (-4, -4),
            (4, 4),
            (-4, 4),
            (4, -4),
        ] {
            self.fill_circle(cx + dx, cy + dy, 1, color);
        }
    }

    /// 绘制新版 IDEA 空心绿色 Debug 虫子图标
    pub fn draw_debug_bug_outline_icon(&mut self, x: i32, y: i32, color: u32) {
        let cx = x + 7;
        let cy = y + 8;
        self.stroke_circle(cx, cy, 3.8, 1.4, color);
        // 头部触角
        self.draw_line_aa(
            (cx - 2) as f32,
            (cy - 4) as f32,
            (cx - 4) as f32,
            (cy - 6) as f32,
            1.3,
            color,
        );
        self.draw_line_aa(
            (cx + 2) as f32,
            (cy - 4) as f32,
            (cx + 4) as f32,
            (cy - 6) as f32,
            1.3,
            color,
        );
        // 六足
        self.draw_line_aa(
            (cx - 4) as f32,
            (cy - 1) as f32,
            (cx - 7) as f32,
            (cy - 2) as f32,
            1.3,
            color,
        );
        self.draw_line_aa(
            (cx + 4) as f32,
            (cy - 1) as f32,
            (cx + 7) as f32,
            (cy - 2) as f32,
            1.3,
            color,
        );
        self.draw_line_aa(
            (cx - 4) as f32,
            (cy + 1) as f32,
            (cx - 7) as f32,
            (cy + 1) as f32,
            1.3,
            color,
        );
        self.draw_line_aa(
            (cx + 4) as f32,
            (cy + 1) as f32,
            (cx + 7) as f32,
            (cy + 1) as f32,
            1.3,
            color,
        );
        self.draw_line_aa(
            (cx - 3) as f32,
            (cy + 3) as f32,
            (cx - 6) as f32,
            (cy + 5) as f32,
            1.3,
            color,
        );
        self.draw_line_aa(
            (cx + 3) as f32,
            (cy + 3) as f32,
            (cx + 6) as f32,
            (cy + 5) as f32,
            1.3,
            color,
        );
    }

    /// 绘制 IDEA 左侧边栏空心线框文件夹图标 (Project)
    pub fn draw_folder_outline_icon(&mut self, x: i32, y: i32, color: u32) {
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 1.0, fy + 2.0);
        pb.line_to(fx + 6.0, fy + 2.0);
        pb.line_to(fx + 8.0, fy + 4.0);
        pb.line_to(fx + 15.0, fy + 4.0);
        pb.line_to(fx + 15.0, fy + 13.0);
        pb.line_to(fx + 1.0, fy + 13.0);
        pb.close();
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.5, color);
        }
    }

    /// 绘制 IDEA 左侧边栏 Commit 图标（垂直线 + 圆点 + 对勾）
    pub fn draw_commit_icon(&mut self, x: i32, y: i32, color: u32) {
        self.draw_line_aa(
            (x + 4) as f32,
            (y + 1) as f32,
            (x + 4) as f32,
            (y + 5) as f32,
            1.5,
            color,
        );
        self.stroke_circle(x + 4, y + 8, 2.8, 1.5, color);
        self.draw_line_aa(
            (x + 4) as f32,
            (y + 11) as f32,
            (x + 4) as f32,
            (y + 15) as f32,
            1.5,
            color,
        );
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 9.0, fy + 8.0);
        pb.line_to(fx + 11.0, fy + 10.0);
        pb.line_to(fx + 15.0, fy + 5.0);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.5, color);
        }
    }

    /// 绘制 IDEA Structure 结构树图标
    pub fn draw_structure_icon(&mut self, x: i32, y: i32, color: u32) {
        self.fill_rounded_rect(Rect::new(x + 1, y + 2, 5, 4), 1, color);
        self.fill_rounded_rect(Rect::new(x + 9, y + 2, 6, 4), 1, color);
        self.fill_rounded_rect(Rect::new(x + 9, y + 9, 6, 4), 1, color);
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 3.0, fy + 6.0);
        pb.line_to(fx + 3.0, fy + 11.0);
        pb.line_to(fx + 9.0, fy + 11.0);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.4, color);
        }
    }

    /// 绘制 IDEA 终端图标 (`>_` 在圆角框中)
    pub fn draw_terminal_icon(&mut self, x: i32, y: i32, color: u32) {
        if let Some(frame) =
            build_rounded_rect_path((x + 1) as f32 + 0.5, (y + 2) as f32 + 0.5, 14.0, 11.0, 2.0)
        {
            self.stroke_path_round(&frame, 1.4, color);
        }
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 4.0, fy + 5.0);
        pb.line_to(fx + 7.0, fy + 7.5);
        pb.line_to(fx + 4.0, fy + 10.0);
        if let Some(chevron) = pb.finish() {
            self.stroke_path_round(&chevron, 1.5, color);
        }
        self.draw_line_aa(
            (x + 8) as f32,
            (y + 10) as f32,
            (x + 12) as f32,
            (y + 10) as f32,
            1.5,
            color,
        );
    }

    /// 绘制 IDEA Problems 感叹号圆环图标
    pub fn draw_problems_icon(&mut self, x: i32, y: i32, color: u32) {
        self.stroke_circle(x + 8, y + 8, 6.0, 1.4, color);
        self.draw_line_aa(
            (x + 8) as f32,
            (y + 4) as f32,
            (x + 8) as f32,
            (y + 9) as f32,
            1.6,
            color,
        );
        self.fill_circle(x + 8, y + 11, 1, color);
    }

    /// 绘制 IDEA / AI Assistant 星芒图标 (✦)
    pub fn draw_ai_sparkle_icon(&mut self, x: i32, y: i32, color: u32) {
        let cx = (x + 7) as f32 + 0.5;
        let cy = (y + 8) as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(cx, cy - 6.0);
        pb.quad_to(cx + 0.6, cy - 0.6, cx + 6.0, cy);
        pb.quad_to(cx + 0.6, cy + 0.6, cx, cy + 6.0);
        pb.quad_to(cx - 0.6, cy + 0.6, cx - 6.0, cy);
        pb.quad_to(cx - 0.6, cy - 0.6, cx, cy - 6.0);
        pb.close();
        if let Some(star) = pb.finish() {
            self.fill_path_solid(&star, color);
        }
        // 右上角小星
        self.draw_line_aa(cx + 5.0, cy - 6.8, cx + 5.0, cy - 2.8, 1.2, color);
        self.draw_line_aa(cx + 3.0, cy - 4.8, cx + 7.0, cy - 4.8, 1.2, color);
    }

    /// 绘制极简六边形立方体智能体徽标（参考图 Cursor 风格）
    pub fn draw_agent_cube_icon(&mut self, x: i32, y: i32, color: u32) {
        let cx = x as f32 + 6.5;
        let cy = y as f32 + 6.5;
        let r = 5.4f32;
        let dx = r * 0.866;
        let dy = r * 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(cx, cy - r);
        pb.line_to(cx + dx, cy - dy);
        pb.line_to(cx + dx, cy + dy);
        pb.line_to(cx, cy + r);
        pb.line_to(cx - dx, cy + dy);
        pb.line_to(cx - dx, cy - dy);
        pb.close();
        if let Some(hex) = pb.finish() {
            self.stroke_path_round(&hex, 1.25, color);
        }
        self.draw_line_aa(cx - 0.5, cy - 0.5, cx + dx - 0.8, cy - dy, 1.1, color);
        self.draw_line_aa(cx - 0.5, cy - 0.5, cx - dx - 0.2, cy - dy, 1.1, color);
        self.draw_line_aa(cx - 0.5, cy - 0.5, cx - 0.5, cy + r - 0.5, 1.1, color);
    }

    /// 绘制工作模式极简矢量小图标（0 = Agent, 1 = Edit, 2 = Ask）
    pub fn draw_mode_icon(&mut self, x: i32, y: i32, mode_kind: u8, color: u32) {
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        match mode_kind {
            0 => {
                // Agent: 简洁无限/智能菱形节点
                let cx = fx + 5.5;
                let cy = fy + 5.5;
                let mut pb = PathBuilder::new();
                pb.move_to(cx, cy - 4.5);
                pb.line_to(cx + 4.5, cy);
                pb.line_to(cx, cy + 4.5);
                pb.line_to(cx - 4.5, cy);
                pb.close();
                if let Some(p) = pb.finish() {
                    self.stroke_path_round(&p, 1.25, color);
                }
                self.fill_circle(x + 6, y + 6, 1, color);
            }
            1 => {
                // Edit: 简洁斜线画笔/编辑线
                self.draw_line_aa(fx + 2.0, fy + 9.0, fx + 8.5, fy + 2.5, 1.35, color);
                self.draw_line_aa(fx + 6.5, fy + 2.0, fx + 9.0, fy + 4.5, 1.35, color);
                self.draw_line_aa(fx + 1.5, fy + 9.5, fx + 4.0, fy + 9.0, 1.25, color);
            }
            _ => {
                // Ask: 简洁对话气泡
                if let Some(p) = build_rounded_rect_path(fx + 1.0, fy + 1.5, 9.0, 7.0, 2.0) {
                    self.stroke_path_round(&p, 1.25, color);
                }
                self.draw_line_aa(fx + 3.0, fy + 8.5, fx + 2.0, fy + 10.5, 1.25, color);
            }
        }
    }

    /// 绘制 Changes / Diff 双栏对比图标
    pub fn draw_changes_diff_icon(&mut self, x: i32, y: i32, color: u32) {
        if let Some(frame) =
            build_rounded_rect_path((x + 2) as f32 + 0.5, (y + 2) as f32 + 0.5, 12.0, 11.0, 1.5)
        {
            self.stroke_path_round(&frame, 1.4, color);
        }
        self.draw_line_aa(
            (x + 8) as f32,
            (y + 2) as f32,
            (x + 8) as f32,
            (y + 13) as f32,
            1.2,
            color,
        );
        self.draw_line_aa(
            (x + 4) as f32,
            (y + 6) as f32,
            (x + 6) as f32,
            (y + 6) as f32,
            1.3,
            color,
        );
        self.draw_line_aa(
            (x + 10) as f32,
            (y + 9) as f32,
            (x + 12) as f32,
            (y + 9) as f32,
            1.3,
            color,
        );
    }

    /// 绘制主题切换半月/太阳图标
    pub fn draw_theme_icon(&mut self, x: i32, y: i32, color: u32) {
        self.stroke_circle(x + 8, y + 8, 5.2, 1.5, color);
        self.fill_circle(x + 9, y + 7, 3, color);
    }

    /// 绘制 JetBrains IntelliJ IDEA 风格的 **RI** 品牌徽标（用于主工具栏与系统窗口图标）
    pub fn draw_ri_logo_icon(&mut self, rect: Rect) {
        if rect.w <= 0 || rect.h <= 0 {
            return;
        }
        let s = self.scale_factor;
        let side = rect.w.min(rect.h) as f32 * s;
        let ox = rect.x as f32 * s + ((rect.w as f32 * s - side) * 0.5);
        let oy = rect.y as f32 * s + ((rect.h as f32 * s - side) * 0.5);
        draw_ri_logo_to_pixmap(&mut self.pixmap, ox, oy, side);
    }

    /// 绘制新建文件图标
    pub fn draw_new_file_icon(&mut self, x: i32, y: i32, color: u32) {
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 2.0, fy + 1.0);
        pb.line_to(fx + 9.0, fy + 1.0);
        pb.line_to(fx + 12.0, fy + 4.0);
        pb.line_to(fx + 12.0, fy + 13.0);
        pb.line_to(fx + 2.0, fy + 13.0);
        pb.close();
        if let Some(doc) = pb.finish() {
            self.stroke_path_round(&doc, 1.3, color);
        }
        self.draw_line_aa(
            (x + 7) as f32,
            (y + 6) as f32,
            (x + 7) as f32,
            (y + 10) as f32,
            1.3,
            color,
        );
        self.draw_line_aa(
            (x + 5) as f32,
            (y + 8) as f32,
            (x + 9) as f32,
            (y + 8) as f32,
            1.3,
            color,
        );
    }

    /// 绘制新建文件夹图标
    pub fn draw_new_folder_icon(&mut self, x: i32, y: i32, color: u32) {
        self.draw_folder_outline_icon(x, y, color);
        self.draw_line_aa(
            (x + 8) as f32,
            (y + 6) as f32,
            (x + 8) as f32,
            (y + 11) as f32,
            1.4,
            color,
        );
        self.draw_line_aa(
            x as f32 + 5.5,
            y as f32 + 8.5,
            x as f32 + 10.5,
            y as f32 + 8.5,
            1.4,
            color,
        );
    }

    /// 绘制全部展开图标
    pub fn draw_expand_all_icon(&mut self, x: i32, y: i32, color: u32) {
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 4.0, fy + 5.0);
        pb.line_to(fx + 7.0, fy + 2.0);
        pb.line_to(fx + 10.0, fy + 5.0);
        pb.move_to(fx + 4.0, fy + 9.0);
        pb.line_to(fx + 7.0, fy + 12.0);
        pb.line_to(fx + 10.0, fy + 9.0);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.4, color);
        }
    }

    /// 绘制全部折叠图标
    pub fn draw_collapse_all_icon(&mut self, x: i32, y: i32, color: u32) {
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 4.0, fy + 2.0);
        pb.line_to(fx + 7.0, fy + 5.0);
        pb.line_to(fx + 10.0, fy + 2.0);
        pb.move_to(fx + 4.0, fy + 12.0);
        pb.line_to(fx + 7.0, fy + 9.0);
        pb.line_to(fx + 10.0, fy + 12.0);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.4, color);
        }
    }

    /// 绘制刷新图标 ↻
    pub fn draw_refresh_icon(&mut self, x: i32, y: i32, color: u32) {
        self.stroke_circle(x + 7, y + 7, 4.5, 1.4, color);
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 10.0, fy + 2.0);
        pb.line_to(fx + 12.0, fy + 6.0);
        pb.line_to(fx + 8.0, fy + 6.0);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.5, color);
        }
    }

    /// 绘制撤销/回滚箭头 ↶
    pub fn draw_rollback_icon(&mut self, x: i32, y: i32, color: u32) {
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 3.0, fy + 5.0);
        pb.line_to(fx + 9.5, fy + 5.0);
        pb.quad_to(fx + 12.5, fy + 8.0, fx + 9.5, fy + 11.0);
        pb.line_to(fx + 4.0, fy + 11.0);
        pb.move_to(fx + 6.0, fy + 2.0);
        pb.line_to(fx + 3.0, fy + 5.0);
        pb.line_to(fx + 6.0, fy + 8.0);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.5, color);
        }
    }

    /// 绘制加号 `+`
    pub fn draw_plus_icon(&mut self, x: i32, y: i32, color: u32) {
        self.draw_line_aa(
            (x + 6) as f32,
            (y + 2) as f32,
            (x + 6) as f32,
            (y + 11) as f32,
            1.6,
            color,
        );
        self.draw_line_aa(
            x as f32 + 1.5,
            y as f32 + 6.5,
            x as f32 + 10.5,
            y as f32 + 6.5,
            1.6,
            color,
        );
    }

    /// 绘制向上发送箭头 `↑`
    pub fn draw_send_up_icon(&mut self, x: i32, y: i32, color: u32) {
        self.draw_line_aa(
            (x + 6) as f32,
            (y + 2) as f32,
            (x + 6) as f32,
            (y + 11) as f32,
            1.7,
            color,
        );
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 2.0, fy + 6.0);
        pb.line_to(fx + 6.0, fy + 2.0);
        pb.line_to(fx + 10.0, fy + 6.0);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.7, color);
        }
    }

    /// 绘制铅笔图标（编辑）
    pub fn draw_pencil_icon(&mut self, x: i32, y: i32, color: u32) {
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 8.8, fy + 1.6);
        pb.line_to(fx + 12.2, fy + 5.0);
        pb.line_to(fx + 5.2, fy + 12.0);
        pb.line_to(fx + 2.6, fy + 12.0);
        pb.line_to(fx + 1.8, fy + 11.2);
        pb.line_to(fx + 1.8, fy + 8.6);
        pb.close();
        pb.move_to(fx + 7.6, fy + 2.8);
        pb.line_to(fx + 11.0, fy + 6.2);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.3, color);
        }
    }

    /// 绘制垂直三点 `⋮`
    pub fn draw_more_dots_icon(&mut self, x: i32, y: i32, color: u32) {
        self.fill_circle(x + 4, y + 3, 1, color);
        self.fill_circle(x + 4, y + 7, 1, color);
        self.fill_circle(x + 4, y + 11, 1, color);
    }

    /// 绘制清空/垃圾桶图标
    pub fn draw_trash_icon(&mut self, x: i32, y: i32, color: u32) {
        self.draw_line_aa(
            (x + 2) as f32,
            (y + 3) as f32,
            (x + 12) as f32,
            (y + 3) as f32,
            1.4,
            color,
        );
        self.draw_line_aa(
            (x + 5) as f32,
            (y + 1) as f32,
            (x + 9) as f32,
            (y + 1) as f32,
            1.4,
            color,
        );
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 3.0, fy + 3.0);
        pb.line_to(fx + 4.0, fy + 12.0);
        pb.line_to(fx + 10.0, fy + 12.0);
        pb.line_to(fx + 11.0, fy + 3.0);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.4, color);
        }
    }

    /// 绘制菜单勾选对勾 `✓`
    pub fn draw_check_icon(&mut self, x: i32, y: i32, color: u32) {
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 1.0, fy + 5.0);
        pb.line_to(fx + 4.5, fy + 8.5);
        pb.line_to(fx + 10.5, fy + 1.5);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.6, color);
        }
    }

    /// 绘制半选/部分勾选横杠 `—` (IDEA 分组复选框部分选中态)
    pub fn draw_dash_icon(&mut self, x: i32, y: i32, color: u32) {
        self.draw_line_aa(
            (x + 2) as f32,
            (y + 5) as f32,
            (x + 9) as f32,
            (y + 5) as f32,
            1.8,
            color,
        );
    }

    /// 绘制跳转到源文件图标 `↗` (Jump to Source)
    pub fn draw_jump_to_source_icon(&mut self, x: i32, y: i32, color: u32) {
        let fx = x as f32 + 0.5;
        let fy = y as f32 + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(fx + 5.0, fy + 2.0);
        pb.line_to(fx + 1.5, fy + 2.0);
        pb.line_to(fx + 1.5, fy + 10.5);
        pb.line_to(fx + 10.0, fy + 10.5);
        pb.line_to(fx + 10.0, fy + 7.0);
        if let Some(path) = pb.finish() {
            self.stroke_path_round(&path, 1.25, color);
        }
        let mut arrow = PathBuilder::new();
        arrow.move_to(fx + 5.5, fy + 6.5);
        arrow.line_to(fx + 11.0, fy + 1.0);
        arrow.move_to(fx + 7.5, fy + 1.0);
        arrow.line_to(fx + 11.0, fy + 1.0);
        arrow.line_to(fx + 11.0, fy + 4.5);
        if let Some(path) = arrow.finish() {
            self.stroke_path_round(&path, 1.35, color);
        }
    }

    /// 在工具栏图标右上角绘制数字徽章（如 Git 更改数量）
    pub fn draw_count_badge(
        &mut self,
        anchor_btn: Rect,
        count: usize,
        bg: u32,
        fg: u32,
        cutout_bg: u32,
    ) {
        if count == 0 {
            return;
        }
        let text = if count > 99 {
            "99+".to_string()
        } else {
            count.to_string()
        };
        let (tw, th) = self.measure_text(&text, FontStyle::BadgeMicro);
        let bh = 13;
        let bw = (tw + 7).clamp(14, 26);
        let bx = (anchor_btn.x + anchor_btn.w - bw + 3).min(anchor_btn.x + anchor_btn.w + 2);
        let by = anchor_btn.y - 2;
        let outer = Rect::new(bx - 1, by - 1, bw + 2, bh + 2);
        self.fill_rounded_rect(outer, 7, cutout_bg);
        let badge = Rect::new(bx, by, bw, bh);
        self.fill_rounded_rect(badge, 6, bg);
        let tx = badge.x + (badge.w - tw) / 2;
        let ty = badge.y + (badge.h - th) / 2;
        self.draw_text_styled(tx, ty, &text, fg, badge.w, FontStyle::BadgeMicro);
    }

    /// 绘制自定义顶部栏最小化图标 `—`
    pub fn draw_window_minimize_icon(&mut self, cx: i32, cy: i32, color: u32) {
        self.draw_line_aa(
            (cx - 5) as f32,
            cy as f32,
            (cx + 5) as f32,
            cy as f32,
            1.3,
            color,
        );
    }

    /// 绘制自定义顶部栏最大化图标 `□`
    pub fn draw_window_maximize_icon(&mut self, cx: i32, cy: i32, color: u32) {
        if let Some(path) =
            build_rounded_rect_path((cx - 5) as f32 + 0.5, (cy - 5) as f32 + 0.5, 9.0, 9.0, 1.0)
        {
            let paint = solid_paint(color, 255);
            let stroke = Stroke {
                width: 1.2,
                ..Stroke::default()
            };
            let xform = self.skia_transform();
            self.pixmap.stroke_path(&path, &paint, &stroke, xform, None);
        }
    }

    /// 绘制自定义顶部栏向下还原图标 `❐`
    pub fn draw_window_restore_icon(&mut self, cx: i32, cy: i32, color: u32) {
        let mut pb = PathBuilder::new();
        let x0 = (cx - 4) as f32 + 0.5;
        let y0 = (cy - 2) as f32 + 0.5;
        pb.move_to(x0, y0);
        pb.line_to(x0 + 7.0, y0);
        pb.line_to(x0 + 7.0, y0 + 7.0);
        pb.line_to(x0, y0 + 7.0);
        pb.close();
        // 后层右上角偏移框
        pb.move_to(x0 + 2.0, y0);
        pb.line_to(x0 + 2.0, y0 - 2.5);
        pb.line_to(x0 + 9.5, y0 - 2.5);
        pb.line_to(x0 + 9.5, y0 + 5.0);
        pb.line_to(x0 + 7.0, y0 + 5.0);
        if let Some(path) = pb.finish() {
            let paint = solid_paint(color, 255);
            let stroke = Stroke {
                width: 1.15,
                ..Stroke::default()
            };
            let xform = self.skia_transform();
            self.pixmap.stroke_path(&path, &paint, &stroke, xform, None);
        }
    }

    /// 绘制自定义顶部栏关闭图标 `×`
    pub fn draw_window_close_icon(&mut self, cx: i32, cy: i32, color: u32) {
        self.draw_line_aa(
            (cx - 5) as f32,
            (cy - 5) as f32,
            (cx + 4) as f32,
            (cy + 4) as f32,
            1.35,
            color,
        );
        self.draw_line_aa(
            (cx + 4) as f32,
            (cy - 5) as f32,
            (cx - 5) as f32,
            (cy + 4) as f32,
            1.35,
            color,
        );
    }

    /// 按路径绘制矢量文件图标。编程语言和常见文件各有图形，未知类型使用文档页。
    pub fn draw_path_file_icon(&mut self, x: i32, y: i32, path: &Path) {
        self.draw_file_icon_kind(x, y, FileIconKind::from_path(path));
    }

    /// 按运行配置（npm/docker/gradle/maven/cargo/python/go/文件）绘制匹配的矢量图标。
    pub fn draw_run_config_icon(&mut self, x: i32, y: i32, cfg: &ide_core::RunConfiguration) {
        let trimmed = cfg
            .command
            .trim()
            .trim_start_matches(".\\")
            .trim_start_matches("./");
        let cmd_file = Path::new(trimmed)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(trimmed)
            .to_ascii_lowercase();
        let cmd_base = cmd_file
            .strip_suffix(".exe")
            .or_else(|| cmd_file.strip_suffix(".cmd"))
            .or_else(|| cmd_file.strip_suffix(".bat"))
            .unwrap_or(&cmd_file);

        let kind = match cmd_base {
            "npm" | "pnpm" | "yarn" | "bun" | "npx" => FileIconKind::Npm,
            "node" | "nodemon" => FileIconKind::JavaScript,
            "tsx" | "ts-node" | "tsc" | "deno" => FileIconKind::TypeScript,
            "vite" => FileIconKind::Vite,
            "docker" | "docker-compose" | "podman" => FileIconKind::Docker,
            "gradle" | "gradlew" => FileIconKind::Gradle,
            "mvn" | "mvnw" => FileIconKind::Maven,
            "flutter" | "dart" => FileIconKind::Dart,
            "adb" | "emulator" => FileIconKind::Kotlin,
            "cargo" | "rustc" | "rustup" => FileIconKind::Rust,
            "python" | "python3" | "py" | "pytest" | "uv" | "poetry" | "uvicorn" | "flask" => {
                FileIconKind::Python
            }
            "go" => FileIconKind::Go,
            "java" => FileIconKind::Java,
            "make" | "gmake" => FileIconKind::Make,
            "just" => FileIconKind::Just,
            "cmake" => FileIconKind::Cmake,
            "dotnet" => FileIconKind::CSharp,
            "ruby" | "bundle" | "rake" => FileIconKind::Ruby,
            "php" | "composer" => FileIconKind::Php,
            "lua" => FileIconKind::Lua,
            "powershell" | "pwsh" => FileIconKind::PowerShell,
            "bash" | "sh" | "zsh" => FileIconKind::Shell,
            "cmd" => FileIconKind::Batch,
            _ => {
                if let Some(target) = &cfg.target_file {
                    FileIconKind::from_path(target)
                } else {
                    FileIconKind::from_language(cfg.language)
                }
            }
        };
        self.draw_file_icon_kind(x, y, kind);
    }

    fn draw_file_icon_kind(&mut self, x: i32, y: i32, kind: FileIconKind) {
        crate::svg_icon::paint_icon(
            &mut self.pixmap,
            self.scale_factor,
            x as f32,
            y as f32,
            16.0,
            16.0,
            kind.icon_name(),
        );
    }

    /// 计算垂直/水平滚动条滑块偏移与长度（自动兼容窄轨道尺寸）
    fn calc_scrollbar_thumb_span(
        track_len: i32,
        scroll_offset: usize,
        visible_items: usize,
        total_items: usize,
    ) -> Option<(i32, i32)> {
        if total_items <= visible_items || track_len <= 16 {
            return None;
        }
        let max_scroll = total_items.saturating_sub(visible_items).max(1);
        let clamped_offset = scroll_offset.min(max_scroll);
        let max_thumb = (track_len as f32 - 4.0).max(8.0);
        let min_thumb = 24.0_f32.min(max_thumb);
        let raw_thumb = (visible_items as f32 / total_items as f32) * (track_len as f32);
        let thumb_len = if raw_thumb.is_finite() {
            raw_thumb.clamp(min_thumb, max_thumb) as i32
        } else {
            min_thumb as i32
        };
        let avail = (track_len - thumb_len).max(1);
        let thumb_pos = ((clamped_offset as f32 / max_scroll as f32) * (avail as f32)) as i32;
        Some((thumb_pos, thumb_len))
    }

    pub fn draw_vertical_scrollbar(
        &mut self,
        track_rect: Rect,
        scroll_offset: usize,
        visible_items: usize,
        total_items: usize,
        thumb_color: u32,
    ) -> Rect {
        let Some((thumb_offset, thumb_h)) = Self::calc_scrollbar_thumb_span(
            track_rect.h,
            scroll_offset,
            visible_items,
            total_items,
        ) else {
            return Rect::new(track_rect.x, track_rect.y, track_rect.w, 0);
        };
        let thumb_rect = Rect::new(
            track_rect.x + 1,
            track_rect.y + thumb_offset,
            (track_rect.w - 2).max(4),
            thumb_h,
        );
        self.fill_rounded_rect(thumb_rect, 3, thumb_color);
        thumb_rect
    }

    pub fn draw_horizontal_scrollbar(
        &mut self,
        track_rect: Rect,
        scroll_offset: usize,
        visible_items: usize,
        total_items: usize,
        thumb_color: u32,
    ) -> Rect {
        let Some((thumb_offset, thumb_w)) = Self::calc_scrollbar_thumb_span(
            track_rect.w,
            scroll_offset,
            visible_items,
            total_items,
        ) else {
            return Rect::new(track_rect.x, track_rect.y, 0, track_rect.h);
        };
        let thumb_rect = Rect::new(
            track_rect.x + thumb_offset,
            track_rect.y + 1,
            thumb_w,
            (track_rect.h - 2).max(4),
        );
        self.fill_rounded_rect(thumb_rect, 3, thumb_color);
        thumb_rect
    }

    /// 精确测量文本在指定字体下的像素宽高 `(width, height)`
    pub fn measure_text(&self, text: &str, style: FontStyle) -> (i32, i32) {
        self.text_engine.measure_text(text, style)
    }

    /// 根据鼠标点击相对于行首的 X 偏移量计算最接近的字符列号（基于原生命中测试 O(1) 计算）
    pub fn measure_caret_col_in_line(&self, line: &str, rel_x: i32, style: FontStyle) -> usize {
        self.text_engine.hit_test_col(line, rel_x, style)
    }

    /// 按最大像素宽度对文本执行末尾省略号截断（超出时显示 `…` 而非硬裁剪）
    pub fn ellipsize_text(&self, text: &str, max_width: i32, style: FontStyle) -> String {
        if max_width <= 0 || text.is_empty() {
            return String::new();
        }
        if self.measure_text(text, style).0 <= max_width {
            return text.to_string();
        }
        let (ew, _) = self.measure_text("…", style);
        if max_width <= ew {
            return "…".to_string();
        }
        let avail = max_width - ew;
        let chars: Vec<char> = text.chars().collect();
        let mut cut = self
            .measure_caret_col_in_line(text, avail, style)
            .min(chars.len());
        while cut > 0 {
            let prefix: String = chars[..cut].iter().collect();
            let cand = format!("{prefix}…");
            if self.measure_text(&cand, style).0 <= max_width {
                return cand;
            }
            cut -= 1;
        }
        "…".to_string()
    }

    /// 对文件名或标签执行智能省略（超长时保留前缀与扩展名，如 `very_long_file….tsx`）
    pub fn ellipsize_filename(&self, label: &str, max_width: i32, style: FontStyle) -> String {
        if max_width <= 0 || label.is_empty() {
            return String::new();
        }
        if self.measure_text(label, style).0 <= max_width {
            return label.to_string();
        }
        if let Some(dot_idx) = label.rfind('.') {
            let ext = &label[dot_idx..];
            let stem = &label[..dot_idx];
            if !stem.is_empty() && ext.chars().count() <= 8 {
                let suffix = format!("…{ext}");
                let (sw, _) = self.measure_text(&suffix, style);
                if sw < max_width - 14 {
                    let stem_chars: Vec<char> = stem.chars().collect();
                    let avail = max_width - sw;
                    let mut cut = self
                        .measure_caret_col_in_line(stem, avail, style)
                        .min(stem_chars.len());
                    while cut > 1 {
                        let prefix: String = stem_chars[..cut].iter().collect();
                        let cand = format!("{prefix}{suffix}");
                        if self.measure_text(&cand, style).0 <= max_width {
                            return cand;
                        }
                        cut -= 1;
                    }
                }
            }
        }
        self.ellipsize_text(label, max_width, style)
    }

    /// 自动附加省略号绘制超长文本，避免边缘生硬裁剪
    pub fn draw_text_ellipsis(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        color: u32,
        max_width: i32,
        style: FontStyle,
    ) -> i32 {
        let fitted = self.ellipsize_text(text, max_width, style);
        self.draw_text_styled(x, y, &fitted, color, max_width, style)
    }

    /// 使用 DirectWrite ClearType 真背景合成渲染平滑中英文/符号文本，并返回绘制后的右边界 X 坐标
    pub fn draw_text_styled(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        color: u32,
        max_width: i32,
        style: FontStyle,
    ) -> i32 {
        self.text_engine
            .draw_text(&mut self.pixmap, x, y, text, color, max_width, style)
    }

    /// 在指定裁剪矩形 `clip_rect` 内渲染文本（支持水平/垂直滚动左边缘平滑裁剪），返回文本自然右边界 X 坐标
    pub fn draw_text_clipped(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        color: u32,
        clip_rect: Rect,
        style: FontStyle,
    ) -> i32 {
        self.text_engine
            .draw_text_clipped(&mut self.pixmap, x, y, text, color, clip_rect, style)
    }

    /// 在矩形区域内水平+垂直居中绘制文本
    pub fn draw_text_centered(&mut self, rect: Rect, text: &str, color: u32, style: FontStyle) {
        let (tw, th) = self.measure_text(text, style);
        let tx = rect.x + ((rect.w - tw).max(4)) / 2;
        let ty = rect.y + ((rect.h - th).max(2)) / 2;
        self.draw_text_styled(tx, ty, text, color, rect.w - 4, style);
    }

    /// 按像素最大宽度高速自动折行（优先单次整行测量 + DirectWrite `HitTestPoint` O(1) 切分，消除逐字符测量开销）
    #[allow(dead_code)]
    pub fn wrap_text_by_pixel_width(
        &self,
        text: &str,
        max_width: i32,
        style: FontStyle,
    ) -> Vec<String> {
        let max_w = max_width.max(32);
        let mut wrapped = Vec::new();
        for raw_line in text.lines() {
            if raw_line.is_empty() {
                wrapped.push(String::new());
                continue;
            }
            let (full_w, _) = self.measure_text(raw_line, style);
            if full_w <= max_w {
                wrapped.push(raw_line.to_string());
                continue;
            }
            let mut rem = raw_line;
            while !rem.is_empty() {
                let (rw, _) = self.measure_text(rem, style);
                if rw <= max_w {
                    wrapped.push(rem.to_string());
                    break;
                }
                let mut split_chars = self.text_engine.hit_test_col(rem, max_w, style).max(1);
                let total_chars = rem.chars().count();
                if split_chars >= total_chars {
                    split_chars = total_chars.saturating_sub(1).max(1);
                }
                let prefix_byte = rem
                    .char_indices()
                    .nth(split_chars)
                    .map(|(b, _)| b)
                    .unwrap_or(rem.len());
                let (head, tail) = rem.split_at(prefix_byte);
                // 若恰好超出 max_w，回退一个字符确保绝不越界
                let (hw, _) = self.measure_text(head, style);
                if hw > max_w && split_chars > 1 {
                    let prev_byte = rem
                        .char_indices()
                        .nth(split_chars - 1)
                        .map(|(b, _)| b)
                        .unwrap_or(prefix_byte);
                    let (h2, t2) = rem.split_at(prev_byte);
                    wrapped.push(h2.to_string());
                    rem = t2;
                } else {
                    wrapped.push(head.to_string());
                    rem = tail;
                }
            }
        }
        wrapped
    }

    /// 使用 `tiny-skia` 原生导出当前帧缓冲为 PNG 截图文件
    pub fn save_png(&self, path: &Path) -> Result<()> {
        self.pixmap.save_png(path)?;
        Ok(())
    }

    /// 在指定矩形内等比居中绘制 RGBA 图像（支持 HiDPI 双线性采样、透明棋盘格底纹与圆角抗锯齿裁切）
    #[allow(clippy::too_many_arguments)]
    pub fn draw_rgba_image_fit(
        &mut self,
        rect: Rect,
        radius: i32,
        img_w: u32,
        img_h: u32,
        rgba: &[u8],
        bg_color: u32,
        border_color: u32,
    ) -> Rect {
        if rect.w <= 4 || rect.h <= 4 {
            return rect;
        }
        self.stroke_rounded_rect(rect, radius, border_color, bg_color);
        if img_w == 0 || img_h == 0 || rgba.len() < (img_w as usize * img_h as usize * 4) {
            return rect;
        }
        let inner_pad = 2i32;
        let max_w = (rect.w - inner_pad * 2).max(1) as f32;
        let max_h = (rect.h - inner_pad * 2).max(1) as f32;
        let scale_fit = (max_w / img_w as f32).min(max_h / img_h as f32);
        let draw_w = ((img_w as f32 * scale_fit).round() as i32).clamp(1, rect.w - inner_pad * 2);
        let draw_h = ((img_h as f32 * scale_fit).round() as i32).clamp(1, rect.h - inner_pad * 2);
        let draw_x = rect.x + (rect.w - draw_w) / 2;
        let draw_y = rect.y + (rect.h - draw_h) / 2;
        let fitted = Rect::new(draw_x, draw_y, draw_w, draw_h);

        let s = self.scale_factor;
        let px0 = (draw_x as f32 * s).round() as i32;
        let py0 = (draw_y as f32 * s).round() as i32;
        let px1 = ((draw_x + draw_w) as f32 * s).round() as i32;
        let py1 = ((draw_y + draw_h) as f32 * s).round() as i32;
        let cx0 = px0.max(0);
        let cy0 = py0.max(0);
        let cx1 = px1.min(self.phys_width as i32);
        let cy1 = py1.min(self.phys_height as i32);
        if cx1 <= cx0 || cy1 <= cy0 {
            return fitted;
        }

        let pw_phys = (px1 - px0).max(1) as f32;
        let ph_phys = (py1 - py0).max(1) as f32;
        let r_phys =
            ((radius.saturating_sub(1) as f32) * s).clamp(0.0, (pw_phys * 0.5).min(ph_phys * 0.5));
        let pw_usize = self.phys_width;
        let out_px = self.pixmap.pixels_mut();
        let iw = img_w as usize;
        let ih = img_h as usize;

        for py in cy0..cy1 {
            let v = ((py - py0) as f32 + 0.5) / ph_phys;
            let sy_f = (v * ih as f32 - 0.5).clamp(0.0, (ih - 1) as f32);
            let sy0 = sy_f.floor() as usize;
            let sy1 = (sy0 + 1).min(ih - 1);
            let fy = sy_f - sy0 as f32;
            let row_base = (py as usize) * pw_usize;

            for px_x in cx0..cx1 {
                // 圆角裁切掩码
                let mut corner_cov = 1.0f32;
                if r_phys > 0.5 {
                    let dx_edge = ((px_x - px0) as f32 + 0.5 - r_phys)
                        .min((px1 - px_x) as f32 - 0.5 - r_phys);
                    let dy_edge =
                        ((py - py0) as f32 + 0.5 - r_phys).min((py1 - py) as f32 - 0.5 - r_phys);
                    if dx_edge < 0.0 && dy_edge < 0.0 {
                        let dist = dx_edge.hypot(dy_edge) - r_phys;
                        corner_cov = (0.5 - dist).clamp(0.0, 1.0);
                        if corner_cov <= 0.0 {
                            continue;
                        }
                    }
                }

                let u = ((px_x - px0) as f32 + 0.5) / pw_phys;
                let sx_f = (u * iw as f32 - 0.5).clamp(0.0, (iw - 1) as f32);
                let sx0 = sx_f.floor() as usize;
                let sx1 = (sx0 + 1).min(iw - 1);
                let fx = sx_f - sx0 as f32;

                let i00 = (sy0 * iw + sx0) * 4;
                let i10 = (sy0 * iw + sx1) * 4;
                let i01 = (sy1 * iw + sx0) * 4;
                let i11 = (sy1 * iw + sx1) * 4;
                let w00 = (1.0 - fx) * (1.0 - fy);
                let w10 = fx * (1.0 - fy);
                let w01 = (1.0 - fx) * fy;
                let w11 = fx * fy;

                let sr = rgba[i00] as f32 * w00
                    + rgba[i10] as f32 * w10
                    + rgba[i01] as f32 * w01
                    + rgba[i11] as f32 * w11;
                let sg = rgba[i00 + 1] as f32 * w00
                    + rgba[i10 + 1] as f32 * w10
                    + rgba[i01 + 1] as f32 * w01
                    + rgba[i11 + 1] as f32 * w11;
                let sb = rgba[i00 + 2] as f32 * w00
                    + rgba[i10 + 2] as f32 * w10
                    + rgba[i01 + 2] as f32 * w01
                    + rgba[i11 + 2] as f32 * w11;
                let sa = (rgba[i00 + 3] as f32 * w00
                    + rgba[i10 + 3] as f32 * w10
                    + rgba[i01 + 3] as f32 * w01
                    + rgba[i11 + 3] as f32 * w11)
                    * corner_cov
                    / 255.0;

                let dst = &mut out_px[row_base + (px_x as usize)];
                let a = sa.clamp(0.0, 1.0);
                let inv = 1.0 - a;
                let nr = (dst.red() as f32 * inv + sr * a).round().clamp(0.0, 255.0) as u8;
                let ng = (dst.green() as f32 * inv + sg * a)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                let nb = (dst.blue() as f32 * inv + sb * a).round().clamp(0.0, 255.0) as u8;
                if let Some(p) = PremultipliedColorU8::from_rgba(nr, ng, nb, 255) {
                    *dst = p;
                }
            }
        }
        fitted
    }

    /// 绘制虚线圆角矩形边框（用于文件拖拽悬停区高亮）
    pub fn draw_dashed_rounded_border(&mut self, rect: Rect, radius: i32, color: u32) {
        if rect.w <= 4 || rect.h <= 4 {
            return;
        }
        let path = build_rounded_rect_path(
            rect.x as f32 + 0.5,
            rect.y as f32 + 0.5,
            rect.w as f32 - 1.0,
            rect.h as f32 - 1.0,
            radius as f32,
        );
        if let Some(p) = path {
            let paint = solid_paint(color, 255);
            let stroke = Stroke {
                width: 1.6,
                dash: tiny_skia::StrokeDash::new(vec![6.0, 4.0], 0.0),
                ..Stroke::default()
            };
            let xform = self.skia_transform();
            self.pixmap.stroke_path(&p, &paint, &stroke, xform, None);
        }
    }
}

#[allow(dead_code)]
#[inline]
fn rgb_to_skia_color(rgb: u32) -> Color {
    let r = ((rgb >> 16) & 0xFF) as u8;
    let g = ((rgb >> 8) & 0xFF) as u8;
    let b = (rgb & 0xFF) as u8;
    Color::from_rgba8(r, g, b, 255)
}

#[inline]
fn solid_paint(rgb: u32, alpha: u8) -> Paint<'static> {
    let r = ((rgb >> 16) & 0xFF) as u8;
    let g = ((rgb >> 8) & 0xFF) as u8;
    let b = (rgb & 0xFF) as u8;
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, alpha);
    paint.anti_alias = true;
    paint
}

fn build_rounded_rect_path(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<SkiaPath> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = radius.clamp(0.0, (w * 0.5).min(h * 0.5));
    let mut pb = PathBuilder::new();
    if r <= 0.01 {
        if let Some(sk_rect) = SkiaRect::from_xywh(x, y, w, h) {
            pb.push_rect(sk_rect);
            return pb.finish();
        }
        return None;
    }

    const KAPPA: f32 = 0.552_284_8;
    let c = r * KAPPA;
    let right = x + w;
    let bottom = y + h;

    pb.move_to(x + r, y);
    pb.line_to(right - r, y);
    pb.cubic_to(right - r + c, y, right, y + r - c, right, y + r);
    pb.line_to(right, bottom - r);
    pb.cubic_to(
        right,
        bottom - r + c,
        right - r + c,
        bottom,
        right - r,
        bottom,
    );
    pb.line_to(x + r, bottom);
    pb.cubic_to(x + r - c, bottom, x, bottom - r + c, x, bottom - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - c, x + r - c, y, x + r, y);
    pb.close();
    pb.finish()
}

/// 在给定 `tiny-skia` Pixmap 上绘制 JetBrains IntelliJ IDEA 风格的 **RI** 品牌徽标
pub fn draw_ri_logo_to_pixmap(pixmap: &mut Pixmap, ox: f32, oy: f32, size: f32) {
    if size <= 1.0 {
        return;
    }
    let scale = size / 100.0;
    let xform = Transform::from_scale(scale, scale).post_translate(ox, oy);

    // 1. 外层 IntelliJ 渐变圆角方框
    if let Some(outer) = build_rounded_rect_path(2.0, 2.0, 96.0, 96.0, 22.0) {
        if let Some(grad) = LinearGradient::new(
            Point::from_xy(6.0, 6.0),
            Point::from_xy(94.0, 94.0),
            vec![
                GradientStop::new(0.00, Color::from_rgba8(0xFE, 0x31, 0x5D, 255)),
                GradientStop::new(0.38, Color::from_rgba8(0xC8, 0x37, 0xE8, 255)),
                GradientStop::new(0.72, Color::from_rgba8(0x3D, 0x62, 0xFA, 255)),
                GradientStop::new(1.00, Color::from_rgba8(0x08, 0x7C, 0xFA, 255)),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        ) {
            let mut paint = Paint::default();
            paint.shader = grad;
            paint.anti_alias = true;
            pixmap.fill_path(&outer, &paint, FillRule::Winding, xform, None);
        }

        if let Some(warm) = LinearGradient::new(
            Point::from_xy(88.0, 8.0),
            Point::from_xy(28.0, 68.0),
            vec![
                GradientStop::new(0.0, Color::from_rgba8(0xFF, 0x78, 0x30, 148)),
                GradientStop::new(1.0, Color::from_rgba8(0xFE, 0x31, 0x5D, 0)),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        ) {
            let mut paint = Paint::default();
            paint.shader = warm;
            paint.anti_alias = true;
            pixmap.fill_path(&outer, &paint, FillRule::Winding, xform, None);
        }
    }

    // 2. 中心曜石黑方块 + 微弱高光描边
    if let Some(inner) = build_rounded_rect_path(16.0, 16.0, 68.0, 68.0, 5.0) {
        let dark = solid_paint(0x101218, 255);
        pixmap.fill_path(&inner, &dark, FillRule::Winding, xform, None);

        let mut border = Paint::default();
        border.set_color_rgba8(255, 255, 255, 26);
        border.anti_alias = true;
        let stroke = Stroke {
            width: 1.0,
            ..Stroke::default()
        };
        pixmap.stroke_path(&inner, &border, &stroke, xform, None);
    }

    let white = solid_paint(0xFFFFFF, 255);

    // 3. 几何粗体 'R'（EvenOdd 挖空上半环）
    let mut r_pb = PathBuilder::new();
    r_pb.move_to(24.0, 25.0);
    r_pb.line_to(37.0, 25.0);
    r_pb.cubic_to(42.8, 25.0, 46.0, 28.6, 46.0, 34.4);
    r_pb.cubic_to(46.0, 38.8, 43.8, 41.8, 40.0, 43.0);
    r_pb.line_to(46.6, 56.0);
    r_pb.line_to(40.0, 56.0);
    r_pb.line_to(34.2, 43.8);
    r_pb.line_to(30.0, 43.8);
    r_pb.line_to(30.0, 56.0);
    r_pb.line_to(24.0, 56.0);
    r_pb.close();

    r_pb.move_to(30.0, 30.2);
    r_pb.line_to(36.4, 30.2);
    r_pb.cubic_to(38.8, 30.2, 40.2, 31.8, 40.2, 34.5);
    r_pb.cubic_to(40.2, 37.2, 38.8, 38.8, 36.4, 38.8);
    r_pb.line_to(30.0, 38.8);
    r_pb.close();

    if let Some(r_path) = r_pb.finish() {
        pixmap.fill_path(&r_path, &white, FillRule::EvenOdd, xform, None);
    }

    // 4. 经典 JetBrains 上下衬线几何 'I'
    let mut i_pb = PathBuilder::new();
    i_pb.move_to(51.0, 25.0);
    i_pb.line_to(66.0, 25.0);
    i_pb.line_to(66.0, 30.4);
    i_pb.line_to(61.5, 30.4);
    i_pb.line_to(61.5, 50.6);
    i_pb.line_to(66.0, 50.6);
    i_pb.line_to(66.0, 56.0);
    i_pb.line_to(51.0, 56.0);
    i_pb.line_to(51.0, 50.6);
    i_pb.line_to(55.5, 50.6);
    i_pb.line_to(55.5, 30.4);
    i_pb.line_to(51.0, 30.4);
    i_pb.close();
    if let Some(i_path) = i_pb.finish() {
        pixmap.fill_path(&i_path, &white, FillRule::Winding, xform, None);
    }

    // 5. JetBrains 标志性左下角白色横杠
    if let Some(bar) = build_rounded_rect_path(24.0, 69.0, 27.0, 5.6, 1.2) {
        pixmap.fill_path(&bar, &white, FillRule::Winding, xform, None);
    }
}

/// 渲染指定尺寸的非预乘 RGBA8 **RI** 品牌图标字节数组（供操作系统窗口/任务栏图标使用）
pub fn render_ri_icon_rgba(size: u32) -> Vec<u8> {
    let px_size = size.max(16);
    let Some(mut pixmap) = Pixmap::new(px_size, px_size) else {
        return Vec::new();
    };
    draw_ri_logo_to_pixmap(&mut pixmap, 0.0, 0.0, px_size as f32);
    let mut out = Vec::with_capacity((px_size * px_size * 4) as usize);
    for p in pixmap.pixels() {
        let a = p.alpha();
        if a == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
        } else if a == 255 {
            out.extend_from_slice(&[p.red(), p.green(), p.blue(), 255]);
        } else {
            let af = a as u16;
            let r = (((p.red() as u16) * 255) / af).min(255) as u8;
            let g = (((p.green() as u16) * 255) / af).min(255) as u8;
            let b = (((p.blue() as u16) * 255) / af).min(255) as u8;
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    out
}

/// 将矢量 **RI** 品牌图标光栅化并保存为 PNG 文件
pub fn save_ri_icon_png(path: &Path, size: u32) -> Result<()> {
    let px_size = size.max(16);
    let mut pixmap = Pixmap::new(px_size, px_size)
        .ok_or_else(|| anyhow::anyhow!("Failed to allocate {}x{} Pixmap", px_size, px_size))?;
    draw_ri_logo_to_pixmap(&mut pixmap, 0.0, 0.0, px_size as f32);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    pixmap.save_png(path)?;
    Ok(())
}

// =============================================================================
// DirectWrite 文字引擎（借鉴 WindUI `text/dwrite.rs` 的真背景 ClearType 合成）
// =============================================================================

#[cfg(target_os = "windows")]
mod dwrite_backend {
use super::{FontStyle, Rect};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::rc::{Rc, Weak};
use tiny_skia::{Pixmap, PremultipliedColorU8};
use windows::core::{implement, IUnknown, Interface, Ref, Result as WinResult, BOOL, PCWSTR};
use windows::Win32::Foundation::{COLORREF, DWRITE_E_NOCOLOR, FALSE};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteBitmapRenderTarget, IDWriteFactory, IDWriteFactory2,
    IDWriteGdiInterop, IDWriteInlineObject, IDWritePixelSnapping_Impl, IDWriteRenderingParams,
    IDWriteTextFormat, IDWriteTextLayout, IDWriteTextRenderer, IDWriteTextRenderer_Impl,
    DWRITE_COLOR_F, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_GLYPH_RUN, DWRITE_GLYPH_RUN_DESCRIPTION,
    DWRITE_HIT_TEST_METRICS, DWRITE_MATRIX, DWRITE_MEASURING_MODE, DWRITE_STRIKETHROUGH,
    DWRITE_TEXT_METRICS, DWRITE_UNDERLINE, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Gdi::{
    GetCurrentObject, GetObjectW, DIBSECTION, OBJ_BITMAP,
};

const MEASURE_CACHE_CAP: usize = 3072;
const LAYOUT_CACHE_CAP: usize = 192;
const MASK_CACHE_CAP: usize = 1024;
const MASK_CACHE_MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_CACHED_TEXT_BYTES: usize = 300;
const MAX_PERSISTENT_BITMAP_W: i32 = 2400;
const MAX_PERSISTENT_BITMAP_H: i32 = 256;

pub fn shared_dwrite_for_scale(scale: f32) -> Rc<DWriteTextEngine> {
    thread_local! {
        static SHARED_DWRITE_ENGINES: RefCell<HashMap<u32, Weak<DWriteTextEngine>>> =
            RefCell::new(HashMap::new());
    }
    let key = (scale.clamp(0.75, 4.0) * 100.0).round() as u32;
    SHARED_DWRITE_ENGINES.with(|map_cell| {
        let mut map = map_cell.borrow_mut();
        map.retain(|_, w| w.strong_count() > 0);
        if let Some(existing) = map.get(&key).and_then(|w| w.upgrade()) {
            return existing;
        }
        let engine = Rc::new(DWriteTextEngine::new((key as f32) / 100.0));
        map.insert(key, Rc::downgrade(&engine));
        engine
    })
}

/// 预光栅化的 DirectWrite ClearType 亚像素覆盖率掩码缓存项：
/// 使用紧凑的 RGB 3 字节/像素存储（相比 `u32` 节省 25% 内存），
/// 并受条目数与总字节预算双重约束，防止超长代码行撑爆内存。
struct CachedGlyphMask {
    log_mw: i32,
    min_x: usize,
    max_x: usize,
    min_y: usize,
    max_y: usize,
    row_w: usize,
    /// Packed RGB subpixel coverage bytes (`row_w * (max_y - min_y) * 3`)
    subpixel_mask: Vec<u8>,
    has_color_glyphs: bool,
}

struct DWriteState {
    scale_factor: f32,
    formats: HashMap<FontStyle, IDWriteTextFormat>,
    measure_cache: HashMap<FontStyle, HashMap<String, (i32, i32)>>,
    layout_cache: HashMap<FontStyle, HashMap<String, (IDWriteTextLayout, i32, i32, i32, i32)>>,
    mask_cache: HashMap<FontStyle, HashMap<String, Rc<CachedGlyphMask>>>,
    mask_cache_bytes: usize,
    bitmap_target: Option<IDWriteBitmapRenderTarget>,
    bitmap_w: i32,
    bitmap_h: i32,
    bitmap_bits: *mut u32,
    bitmap_stride_px: i32,
}

pub struct DWriteTextEngine {
    factory: IDWriteFactory,
    gdi_interop: IDWriteGdiInterop,
    renderer: IDWriteTextRenderer,
    state: RefCell<DWriteState>,
}

impl DWriteTextEngine {
    fn new(scale_factor: f32) -> Self {
        unsafe {
            let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
                .expect("DWriteCreateFactory failed");
            let gdi_interop = factory.GetGdiInterop().expect("GetGdiInterop failed");
            let params = factory
                .CreateRenderingParams()
                .expect("CreateRenderingParams failed");
            let factory2: Option<IDWriteFactory2> = factory.cast().ok();
            let renderer: IDWriteTextRenderer = GlyphRenderer { params, factory2 }.into();
            Self {
                factory,
                gdi_interop,
                renderer,
                state: RefCell::new(DWriteState {
                    scale_factor: scale_factor.clamp(0.75, 4.0),
                    formats: HashMap::new(),
                    measure_cache: HashMap::new(),
                    layout_cache: HashMap::new(),
                    mask_cache: HashMap::new(),
                    mask_cache_bytes: 0,
                    bitmap_target: None,
                    bitmap_w: 0,
                    bitmap_h: 0,
                    bitmap_bits: std::ptr::null_mut(),
                    bitmap_stride_px: 0,
                }),
            }
        }
    }

    pub fn hit_test_col(&self, line: &str, log_rel_x: i32, style: FontStyle) -> usize {
        if log_rel_x <= 0 || line.is_empty() {
            return 0;
        }
        let Some((layout, _, _, _, _)) = self.get_or_create_layout(line, style) else {
            return 0;
        };
        let scale = self.state.borrow().scale_factor;
        let phys_x = (log_rel_x as f32) * scale;
        let mut is_trailing = FALSE;
        let mut is_inside = FALSE;
        let mut ht = DWRITE_HIT_TEST_METRICS::default();
        if unsafe { layout.HitTestPoint(phys_x, 0.0, &mut is_trailing, &mut is_inside, &mut ht) }
            .is_err()
        {
            return 0;
        }
        let utf16_pos = if is_trailing.as_bool() {
            (ht.textPosition + ht.length) as usize
        } else {
            ht.textPosition as usize
        };
        utf16_offset_to_char_idx(line, utf16_pos)
    }

    fn get_or_create_format(&self, style: FontStyle) -> Option<IDWriteTextFormat> {
        let scale = {
            let st = self.state.borrow();
            if let Some(fmt) = st.formats.get(&style) {
                return Some(fmt.clone());
            }
            st.scale_factor
        };

        let (family, base_size, weight) = match style {
            FontStyle::UiRegular => ("Microsoft YaHei UI", 14.5f32, DWRITE_FONT_WEIGHT_NORMAL),
            FontStyle::UiBold => ("Microsoft YaHei UI", 14.5f32, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            FontStyle::UiSmall => ("Microsoft YaHei UI", 13.5f32, DWRITE_FONT_WEIGHT_NORMAL),
            FontStyle::CodeMono => ("Cascadia Mono", 15.0f32, DWRITE_FONT_WEIGHT_NORMAL),
            FontStyle::CodeMonoBold => ("Cascadia Mono", 15.0f32, DWRITE_FONT_WEIGHT_BOLD),
            FontStyle::ChatBody => ("Segoe UI", 14.0f32, DWRITE_FONT_WEIGHT_NORMAL),
            FontStyle::ChatBodyBold => ("Segoe UI", 14.0f32, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            FontStyle::ChatMeta => ("Segoe UI", 12.2f32, DWRITE_FONT_WEIGHT_NORMAL),
            FontStyle::ChatMetaBold => ("Segoe UI", 12.2f32, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            FontStyle::ChatCode => ("Cascadia Mono", 12.6f32, DWRITE_FONT_WEIGHT_NORMAL),
            FontStyle::ChatCodeBold => ("Cascadia Mono", 12.6f32, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            FontStyle::BadgeMicro => ("Segoe UI", 10.0f32, DWRITE_FONT_WEIGHT_BOLD),
        };
        let size = (base_size * scale).max(9.0);

        let fam_w = wide_nul(family);
        let locale_w = wide_nul("zh-cn");
        let format = unsafe {
            self.factory
                .CreateTextFormat(
                    PCWSTR(fam_w.as_ptr()),
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    PCWSTR(locale_w.as_ptr()),
                )
                .ok()?
        };
        unsafe {
            let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        }
        self.state
            .borrow_mut()
            .formats
            .insert(style, format.clone());
        Some(format)
    }

    fn create_layout(&self, text: &str, style: FontStyle) -> Option<IDWriteTextLayout> {
        let format = self.get_or_create_format(style)?;
        let text_w: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.factory
                .CreateTextLayout(&text_w, &format, f32::MAX, f32::MAX)
                .ok()
        }
    }

    pub fn trim_caches(&self) {
        let mut st = self.state.borrow_mut();
        st.layout_cache.clear();
        st.mask_cache.clear();
        st.mask_cache_bytes = 0;
        if st.measure_cache.values().map(|m| m.len()).sum::<usize>() > 1024 {
            st.measure_cache.clear();
        }
        st.bitmap_target = None;
        st.bitmap_w = 0;
        st.bitmap_h = 0;
        st.bitmap_bits = std::ptr::null_mut();
        st.bitmap_stride_px = 0;
    }

    fn get_or_create_layout(
        &self,
        text: &str,
        style: FontStyle,
    ) -> Option<(IDWriteTextLayout, i32, i32, i32, i32)> {
        let scale = {
            let st = self.state.borrow();
            if text.len() <= MAX_CACHED_TEXT_BYTES {
                if let Some(by_text) = st.layout_cache.get(&style) {
                    if let Some((layout, log_mw, log_th, phys_mw, phys_th)) = by_text.get(text) {
                        return Some((layout.clone(), *log_mw, *log_th, *phys_mw, *phys_th));
                    }
                }
            }
            st.scale_factor
        };
        let layout = self.create_layout(text, style)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        if unsafe { layout.GetMetrics(&mut metrics) }.is_err() {
            return None;
        }
        let phys_mw = (metrics.widthIncludingTrailingWhitespace.ceil() as i32).max(1);
        let phys_th = (metrics.height.ceil() as i32).max(1);
        let log_mw = ((metrics.widthIncludingTrailingWhitespace / scale).round() as i32).max(1);
        let log_th = ((metrics.height / scale).round() as i32).max(1);

        if text.len() <= MAX_CACHED_TEXT_BYTES {
            let mut st = self.state.borrow_mut();
            let total_cached: usize = st.layout_cache.values().map(|m| m.len()).sum();
            if total_cached >= LAYOUT_CACHE_CAP {
                st.layout_cache.clear();
            }
            st.layout_cache.entry(style).or_default().insert(
                text.to_string(),
                (layout.clone(), log_mw, log_th, phys_mw, phys_th),
            );
        }
        Some((layout, log_mw, log_th, phys_mw, phys_th))
    }

    fn create_dib_target(&self, nw: i32, nh: i32) -> Option<(IDWriteBitmapRenderTarget, *mut u32, i32)> {
        let brt = unsafe {
            self.gdi_interop
                .CreateBitmapRenderTarget(None, nw as u32, nh as u32)
                .ok()?
        };
        unsafe {
            let _ = brt.SetPixelsPerDip(1.0);
        }
        let dc = unsafe { brt.GetMemoryDC() };
        let hbm = unsafe { GetCurrentObject(dc, OBJ_BITMAP) };
        let mut ds = DIBSECTION::default();
        let got = unsafe {
            GetObjectW(
                hbm,
                std::mem::size_of::<DIBSECTION>() as i32,
                Some(&mut ds as *mut _ as *mut c_void),
            )
        };
        if got == 0 || ds.dsBm.bmBits.is_null() {
            return None;
        }
        Some((brt, ds.dsBm.bmBits as *mut u32, ds.dsBm.bmWidthBytes / 4))
    }

    fn ensure_bitmap(&self, w: i32, h: i32) -> Option<(IDWriteBitmapRenderTarget, *mut u32, i32)> {
        // 超宽/超大单次文本使用临时位图，避免常驻 DIB 永久膨胀
        if w > MAX_PERSISTENT_BITMAP_W || h > MAX_PERSISTENT_BITMAP_H {
            return self.create_dib_target(w.max(64), h.max(32));
        }
        let mut st = self.state.borrow_mut();
        if st.bitmap_target.is_none()
            || st.bitmap_bits.is_null()
            || w > st.bitmap_w
            || h > st.bitmap_h
        {
            let nw = w.max(st.bitmap_w).max(64).min(MAX_PERSISTENT_BITMAP_W);
            let nh = h.max(st.bitmap_h).max(32).min(MAX_PERSISTENT_BITMAP_H);
            let (brt, bits, stride_px) = self.create_dib_target(nw, nh)?;
            st.bitmap_target = Some(brt);
            st.bitmap_w = nw;
            st.bitmap_h = nh;
            st.bitmap_bits = bits;
            st.bitmap_stride_px = stride_px;
        }
        Some((
            st.bitmap_target.clone()?,
            st.bitmap_bits,
            st.bitmap_stride_px,
        ))
    }

    pub fn measure_text(&self, text: &str, style: FontStyle) -> (i32, i32) {
        if text.is_empty() {
            return (0, 18);
        }
        {
            let st = self.state.borrow();
            if let Some(by_text) = st.measure_cache.get(&style) {
                if let Some(&cached) = by_text.get(text) {
                    return cached;
                }
            }
        }
        let Some((_, log_mw, log_th, _, _)) = self.get_or_create_layout(text, style) else {
            return (0, 18);
        };
        let res = (log_mw, log_th);
        let mut st = self.state.borrow_mut();
        let total_cached: usize = st.measure_cache.values().map(|m| m.len()).sum();
        if total_cached >= MEASURE_CACHE_CAP {
            st.measure_cache.clear();
        }
        st.measure_cache
            .entry(style)
            .or_default()
            .insert(text.to_string(), res);
        res
    }

    pub fn draw_text(
        &self,
        pixmap: &mut Pixmap,
        x: i32,
        y: i32,
        text: &str,
        color: u32,
        max_width: i32,
        style: FontStyle,
    ) -> i32 {
        if text.is_empty() || max_width <= 0 {
            return x;
        }
        let scale = self.state.borrow().scale_factor;
        let log_ph = ((pixmap.height() as f32) / scale).ceil() as i32;
        let clip = Rect::new(0, 0, (x + max_width).max(0), log_ph);
        let (_, mw) = self.draw_text_in_clip(pixmap, x, y, text, color, clip, style);
        x + mw.min(max_width)
    }

    pub fn draw_text_clipped(
        &self,
        pixmap: &mut Pixmap,
        x: i32,
        y: i32,
        text: &str,
        color: u32,
        clip_rect: Rect,
        style: FontStyle,
    ) -> i32 {
        if text.is_empty() || clip_rect.w <= 0 || clip_rect.h <= 0 {
            return x;
        }
        let (_, mw) = self.draw_text_in_clip(pixmap, x, y, text, color, clip_rect, style);
        x + mw
    }

    fn get_or_create_glyph_mask(
        &self,
        text: &str,
        style: FontStyle,
    ) -> Option<Rc<CachedGlyphMask>> {
        {
            let st = self.state.borrow();
            if let Some(by_text) = st.mask_cache.get(&style) {
                if let Some(cached) = by_text.get(text) {
                    return Some(Rc::clone(cached));
                }
            }
        }

        let (layout, log_mw, _log_th, phys_mw, phys_th) = self.get_or_create_layout(text, style)?;

        // 纯空白文本（如代码缩进空格）无需光栅化，直接缓存空掩码
        if text.chars().all(|c| c == ' ' || c == '\t') {
            let empty_mask = Rc::new(CachedGlyphMask {
                log_mw,
                min_x: 0,
                max_x: 0,
                min_y: 0,
                max_y: 0,
                row_w: 0,
                subpixel_mask: Vec::new(),
                has_color_glyphs: false,
            });
            let mut st = self.state.borrow_mut();
            st.mask_cache
                .entry(style)
                .or_default()
                .insert(text.to_string(), Rc::clone(&empty_mask));
            return Some(empty_mask);
        }

        let tw = (phys_mw + 4).max(4);
        let th = (phys_th + 4).max(4);
        let (brt, bits, stride_px) = self.ensure_bitmap(tw, th)?;
        let cw = tw as usize;
        let ch = th as usize;
        let stride_usize = stride_px as usize;

        // 1. 清空目标 DIB 区域为纯黑 (0x00000000)
        for row in 0..ch {
            let dst_row =
                unsafe { std::slice::from_raw_parts_mut(bits.add(row * stride_usize), cw) };
            dst_row.fill(0);
        }

        // 2. 使用纯白前景色 (0xFFFFFF) 光栅化 ClearType 亚像素覆盖率
        let ctx = BitmapCtx {
            target: brt,
            color: COLORREF(0x00FFFFFF),
            used_color_glyphs: Cell::new(false),
        };
        unsafe {
            let _ = layout.Draw(
                Some(&ctx as *const _ as *const c_void),
                &self.renderer,
                0.0,
                0.0,
            );
        }

        let has_color_glyphs = ctx.used_color_glyphs.get();
        let mut min_x = cw;
        let mut max_x = 0usize;
        let mut min_y = ch;
        let mut max_y = 0usize;

        if !has_color_glyphs {
            for row in 0..ch {
                let src_row =
                    unsafe { std::slice::from_raw_parts(bits.add(row * stride_usize), cw) };
                for (col, &px) in src_row.iter().enumerate() {
                    if (px & 0x00FFFFFF) != 0 {
                        if row < min_y {
                            min_y = row;
                        }
                        if row + 1 > max_y {
                            max_y = row + 1;
                        }
                        if col < min_x {
                            min_x = col;
                        }
                        if col + 1 > max_x {
                            max_x = col + 1;
                        }
                    }
                }
            }
        }

        let (row_w, subpixel_mask) = if !has_color_glyphs && max_x > min_x && max_y > min_y {
            let rw = max_x - min_x;
            let rh = max_y - min_y;
            let mut buf = Vec::with_capacity(rw * rh * 3);
            for row in min_y..max_y {
                let src_row =
                    unsafe { std::slice::from_raw_parts(bits.add(row * stride_usize + min_x), rw) };
                for &px in src_row {
                    buf.push(((px >> 16) & 0xFF) as u8);
                    buf.push(((px >> 8) & 0xFF) as u8);
                    buf.push((px & 0xFF) as u8);
                }
            }
            (rw, buf)
        } else {
            min_x = 0;
            max_x = 0;
            min_y = 0;
            max_y = 0;
            (0, Vec::new())
        };

        let mask_bytes = subpixel_mask.len() + text.len();
        let mask_rc = Rc::new(CachedGlyphMask {
            log_mw,
            min_x,
            max_x,
            min_y,
            max_y,
            row_w,
            subpixel_mask,
            has_color_glyphs,
        });

        if text.len() <= MAX_CACHED_TEXT_BYTES
            && phys_mw <= MAX_PERSISTENT_BITMAP_W
            && mask_bytes <= 256 * 1024
        {
            let mut st = self.state.borrow_mut();
            let total_cached: usize = st.mask_cache.values().map(|m| m.len()).sum();
            if total_cached >= MASK_CACHE_CAP
                || st.mask_cache_bytes.saturating_add(mask_bytes) > MASK_CACHE_MAX_BYTES
            {
                st.mask_cache.clear();
                st.mask_cache_bytes = 0;
            }
            st.mask_cache
                .entry(style)
                .or_default()
                .insert(text.to_string(), Rc::clone(&mask_rc));
            st.mask_cache_bytes = st.mask_cache_bytes.saturating_add(mask_bytes);
        }
        Some(mask_rc)
    }

    fn draw_text_in_clip(
        &self,
        pixmap: &mut Pixmap,
        x: i32,
        y: i32,
        text: &str,
        color: u32,
        clip_rect: Rect,
        style: FontStyle,
    ) -> (bool, i32) {
        let Some(mask) = self.get_or_create_glyph_mask(text, style) else {
            return (false, 0);
        };
        let log_mw = mask.log_mw;
        let scale = self.state.borrow().scale_factor;

        let pw = pixmap.width() as i32;
        let ph = pixmap.height() as i32;

        let phys_x = (x as f32 * scale).round() as i32;
        let phys_y = (y as f32 * scale).round() as i32;

        let clip_x0 = ((clip_rect.x as f32 * scale).floor() as i32).max(0);
        let clip_x1 = (((clip_rect.x + clip_rect.w) as f32 * scale).ceil() as i32).min(pw);
        let clip_y0 = ((clip_rect.y as f32 * scale).floor() as i32).max(0);
        let clip_y1 = (((clip_rect.y + clip_rect.h) as f32 * scale).ceil() as i32).min(ph);

        // 快速路径（覆盖 99.9% 的代码、菜单与中英文文本）：直接使用预光栅化的 ClearType 亚像素掩码合成
        if !mask.has_color_glyphs {
            if mask.row_w == 0 || mask.max_y <= mask.min_y {
                return (true, log_mw);
            }
            let dst_x0 = phys_x + mask.min_x as i32;
            let dst_x1 = phys_x + mask.max_x as i32;
            let dst_y0 = phys_y + mask.min_y as i32;
            let dst_y1 = phys_y + mask.max_y as i32;

            let vx0 = dst_x0.max(clip_x0);
            let vx1 = dst_x1.min(clip_x1);
            let vy0 = dst_y0.max(clip_y0);
            let vy1 = dst_y1.min(clip_y1);
            if vx1 <= vx0 || vy1 <= vy0 {
                return (false, log_mw);
            }

            let fg_r = (color >> 16) & 0xFF;
            let fg_g = (color >> 8) & 0xFF;
            let fg_b = color & 0xFF;
            let pw_usize = pw as usize;
            let cw = (vx1 - vx0) as usize;
            let mask_c0 = (vx0 - dst_x0) as usize;
            let px = pixmap.pixels_mut();

            for vy in vy0..vy1 {
                let mask_r = (vy - dst_y0) as usize;
                let m_start = (mask_r * mask.row_w + mask_c0) * 3;
                let mask_row = &mask.subpixel_mask[m_start..m_start + cw * 3];
                let d_start = (vy as usize) * pw_usize + (vx0 as usize);
                let dst_row = &mut px[d_start..d_start + cw];

                for (dst, rgb) in dst_row.iter_mut().zip(mask_row.chunks_exact(3)) {
                    let ar = rgb[0] as u32;
                    let ag = rgb[1] as u32;
                    let ab = rgb[2] as u32;
                    if (ar | ag | ab) != 0 {
                        let br = dst.red() as u32;
                        let bg = dst.green() as u32;
                        let bb = dst.blue() as u32;
                        let nr = ((br * (255 - ar) + fg_r * ar + 127) / 255) as u8;
                        let ng = ((bg * (255 - ag) + fg_g * ag + 127) / 255) as u8;
                        let nb = ((bb * (255 - ab) + fg_b * ab + 127) / 255) as u8;
                        if let Some(p) = PremultipliedColorU8::from_rgba(nr, ng, nb, 255) {
                            *dst = p;
                        }
                    }
                }
            }
            return (true, log_mw);
        }

        // 彩色 Emoji 字形回退路径：直接通过 DirectWrite 彩色字形拆层合成
        let Some((layout, _, _, phys_mw, phys_th)) = self.get_or_create_layout(text, style) else {
            return (false, log_mw);
        };
        let vis_x0 = phys_x.max(clip_x0);
        let vis_x1 = (phys_x + phys_mw + 4).min(clip_x1);
        if vis_x1 <= vis_x0 {
            return (false, log_mw);
        }
        let vis_y0 = phys_y.max(clip_y0);
        let vis_y1 = (phys_y + phys_th + 4).min(clip_y1);
        if vis_y1 <= vis_y0 {
            return (false, log_mw);
        }

        let tw = vis_x1 - vis_x0;
        let th = vis_y1 - vis_y0;
        let glyph_dx = (phys_x - vis_x0) as f32;
        let glyph_dy = (phys_y - vis_y0) as f32;

        let Some((brt, bits, stride_px)) = self.ensure_bitmap(tw, th) else {
            return (false, log_mw);
        };

        let cw = tw as usize;
        let ch = th as usize;
        let pw_usize = pw as usize;
        let vx0 = vis_x0 as usize;
        let vy0 = vis_y0 as usize;
        let stride_usize = stride_px as usize;

        {
            let px = pixmap.pixels();
            for row in 0..ch {
                let src_start = (vy0 + row) * pw_usize + vx0;
                let src_row = &px[src_start..src_start + cw];
                let dst_row =
                    unsafe { std::slice::from_raw_parts_mut(bits.add(row * stride_usize), cw) };
                for (dst, &p) in dst_row.iter_mut().zip(src_row.iter()) {
                    *dst = ((p.alpha() as u32) << 24)
                        | ((p.red() as u32) << 16)
                        | ((p.green() as u32) << 8)
                        | (p.blue() as u32);
                }
            }
        }

        let fg_r = (color >> 16) & 0xFF;
        let fg_g = (color >> 8) & 0xFF;
        let fg_b = color & 0xFF;
        let colorref = COLORREF((fg_b << 16) | (fg_g << 8) | fg_r);
        let ctx = BitmapCtx {
            target: brt.clone(),
            color: colorref,
            used_color_glyphs: Cell::new(false),
        };
        unsafe {
            let _ = layout.Draw(
                Some(&ctx as *const _ as *const c_void),
                &self.renderer,
                glyph_dx,
                glyph_dy,
            );
        }

        {
            let px = pixmap.pixels_mut();
            for row in 0..ch {
                let dst_start = (vy0 + row) * pw_usize + vx0;
                let dst_row = &mut px[dst_start..dst_start + cw];
                let src_row =
                    unsafe { std::slice::from_raw_parts(bits.add(row * stride_usize), cw) };
                for (dst, &new_bgra) in dst_row.iter_mut().zip(src_row.iter()) {
                    let nb = (new_bgra & 0xFF) as u8;
                    let ng = ((new_bgra >> 8) & 0xFF) as u8;
                    let nr = ((new_bgra >> 16) & 0xFF) as u8;
                    if (nr, ng, nb) != (dst.red(), dst.green(), dst.blue()) {
                        if let Some(p) = PremultipliedColorU8::from_rgba(nr, ng, nb, 255) {
                            *dst = p;
                        }
                    }
                }
            }
        }

        (true, log_mw)
    }
}

fn utf16_offset_to_char_idx(s: &str, target_utf16: usize) -> usize {
    let mut u16_acc = 0usize;
    for (char_idx, ch) in s.chars().enumerate() {
        if u16_acc >= target_utf16 {
            return char_idx;
        }
        u16_acc += ch.len_utf16();
    }
    s.chars().count()
}

fn wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct BitmapCtx {
    target: IDWriteBitmapRenderTarget,
    color: COLORREF,
    used_color_glyphs: Cell<bool>,
}

fn color_f_to_colorref(c: DWRITE_COLOR_F) -> COLORREF {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    COLORREF((q(c.b) << 16) | (q(c.g) << 8) | q(c.r))
}

#[implement(IDWriteTextRenderer)]
struct GlyphRenderer {
    params: IDWriteRenderingParams,
    factory2: Option<IDWriteFactory2>,
}

impl IDWriteTextRenderer_Impl for GlyphRenderer_Impl {
    fn DrawGlyphRun(
        &self,
        clientdrawingcontext: *const c_void,
        baselineoriginx: f32,
        baselineoriginy: f32,
        measuringmode: DWRITE_MEASURING_MODE,
        glyphrun: *const DWRITE_GLYPH_RUN,
        glyphrundescription: *const DWRITE_GLYPH_RUN_DESCRIPTION,
        _clientdrawingeffect: Ref<'_, IUnknown>,
    ) -> WinResult<()> {
        if clientdrawingcontext.is_null() {
            return Ok(());
        }
        let ctx = unsafe { &*(clientdrawingcontext as *const BitmapCtx) };

        if let Some(f2) = &self.factory2 {
            let desc = if glyphrundescription.is_null() {
                None
            } else {
                Some(glyphrundescription)
            };
            let enumr = unsafe {
                f2.TranslateColorGlyphRun(
                    baselineoriginx,
                    baselineoriginy,
                    glyphrun,
                    desc,
                    measuringmode,
                    None,
                    0,
                )
            };
            match enumr {
                Ok(en) => {
                    ctx.used_color_glyphs.set(true);
                    unsafe {
                        while let Ok(more) = en.MoveNext() {
                            if !more.as_bool() {
                                break;
                            }
                            let Ok(run_ptr) = en.GetCurrentRun() else {
                                break;
                            };
                            if run_ptr.is_null() {
                                break;
                            }
                            let run = &*run_ptr;
                            let color = if run.paletteIndex == 0xFFFF {
                                ctx.color
                            } else {
                                color_f_to_colorref(run.runColor)
                            };
                            let _ = ctx.target.DrawGlyphRun(
                                run.baselineOriginX,
                                run.baselineOriginY,
                                measuringmode,
                                &run.glyphRun,
                                &self.params,
                                color,
                                None,
                            );
                        }
                    }
                    return Ok(());
                }
                Err(e) if e.code() == DWRITE_E_NOCOLOR => {}
                Err(_) => {}
            }
        }

        unsafe {
            let _ = ctx.target.DrawGlyphRun(
                baselineoriginx,
                baselineoriginy,
                measuringmode,
                glyphrun,
                &self.params,
                ctx.color,
                None,
            );
        }
        Ok(())
    }

    fn DrawUnderline(
        &self,
        _ctx: *const c_void,
        _x: f32,
        _y: f32,
        _underline: *const DWRITE_UNDERLINE,
        _effect: Ref<'_, IUnknown>,
    ) -> WinResult<()> {
        Ok(())
    }

    fn DrawStrikethrough(
        &self,
        _ctx: *const c_void,
        _x: f32,
        _y: f32,
        _strikethrough: *const DWRITE_STRIKETHROUGH,
        _effect: Ref<'_, IUnknown>,
    ) -> WinResult<()> {
        Ok(())
    }

    fn DrawInlineObject(
        &self,
        _ctx: *const c_void,
        _x: f32,
        _y: f32,
        _inlineobject: Ref<'_, IDWriteInlineObject>,
        _issideways: BOOL,
        _isrtl: BOOL,
        _effect: Ref<'_, IUnknown>,
    ) -> WinResult<()> {
        Ok(())
    }
}

impl IDWritePixelSnapping_Impl for GlyphRenderer_Impl {
    fn IsPixelSnappingDisabled(&self, _ctx: *const c_void) -> WinResult<BOOL> {
        Ok(FALSE)
    }

    fn GetCurrentTransform(
        &self,
        _ctx: *const c_void,
        transform: *mut DWRITE_MATRIX,
    ) -> WinResult<()> {
        if transform.is_null() {
            return Ok(());
        }
        unsafe {
            *transform = DWRITE_MATRIX {
                m11: 1.0,
                m12: 0.0,
                m21: 0.0,
                m22: 1.0,
                dx: 0.0,
                dy: 0.0,
            };
        }
        Ok(())
    }

    fn GetPixelsPerDip(&self, _ctx: *const c_void) -> WinResult<f32> {
        Ok(1.0)
    }
}
} // mod dwrite_backend

#[cfg(target_os = "windows")]
use dwrite_backend::{shared_dwrite_for_scale, DWriteTextEngine};

#[cfg(test)]
mod file_icon_tests {
    use super::{FileIconKind, SurfaceCanvas};
    use std::path::Path;

    #[test]
    fn programming_and_common_files_get_distinct_vector_icons() {
        assert_eq!(
            FileIconKind::from_path(Path::new("src/main.rs")),
            FileIconKind::Rust
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("app.tsx")),
            FileIconKind::Tsx
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("lib.go")),
            FileIconKind::Go
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("Main.java")),
            FileIconKind::Java
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("widget.vue")),
            FileIconKind::Vue
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("style.css")),
            FileIconKind::Css
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("Cargo.toml")),
            FileIconKind::Rust
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("Cargo.lock")),
            FileIconKind::Lock
        );
        assert_eq!(
            FileIconKind::from_path(Path::new(".gitignore")),
            FileIconKind::Git
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("Dockerfile")),
            FileIconKind::Docker
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("photo.PNG")),
            FileIconKind::Image
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("clip.mp4")),
            FileIconKind::Video
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("notes.txt")),
            FileIconKind::Text
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("archive.tar.gz")),
            FileIconKind::Archive
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("Makefile")),
            FileIconKind::Make
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("schema.prisma")),
            FileIconKind::Database
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("app.cr")),
            FileIconKind::Crystal
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("shader.glsl")),
            FileIconKind::Shader
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("App.sln")),
            FileIconKind::Project
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("Containerfile")),
            FileIconKind::Docker
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("BUILD.bazel")),
            FileIconKind::Bazel
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("pom.xml")),
            FileIconKind::Maven
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("Chart.yaml")),
            FileIconKind::Helm
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("lib.rlib")),
            FileIconKind::Binary
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("CHANGELOG")),
            FileIconKind::Changelog
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("api.http")),
            FileIconKind::Http
        );
        assert_eq!(
            FileIconKind::from_path(Path::new("unknown.qqq")),
            FileIconKind::Unknown
        );
        assert_ne!(
            FileIconKind::from_path(Path::new("a.rs")),
            FileIconKind::from_path(Path::new("a.py"))
        );
    }

    #[test]
    fn svg_file_icons_paint_visible_pixels() {
        let mut canvas = SurfaceCanvas::new(48, 48);
        canvas.draw_path_file_icon(4, 4, std::path::Path::new("src/lib.rs"));
        canvas.draw_path_file_icon(24, 4, std::path::Path::new("README.md"));
        canvas.draw_path_file_icon(4, 24, std::path::Path::new("docs/guide.md"));
        canvas.draw_folder_icon(4, 24, 0xD9A343);
        let painted = canvas.pixels().iter().filter(|px| **px != 0).count();
        assert!(painted > 40, "icons should cover more than a few pixels, got {painted}");
    }

    #[test]
    fn json_icon_stays_inside_its_slot() {
        let mut canvas = SurfaceCanvas::new(80, 80);
        canvas.draw_path_file_icon(24, 40, Path::new("data.json"));
        let px = canvas.pixels();
        let mut above = 0;
        let mut inside = 0;
        for y in 0..80 {
            for x in 0..80 {
                if px[y * 80 + x] == 0 {
                    continue;
                }
                if y < 38 {
                    above += 1;
                }
                if (24..42).contains(&x) && (40..58).contains(&y) {
                    inside += 1;
                }
            }
        }
        assert_eq!(above, 0, "json icon painted onto the row above");
        assert!(inside > 8, "json icon missing from its own slot, got {inside}");
    }

    #[test]
    fn shared_text_engine_and_minimized_shrink() {
        let mut c1 = SurfaceCanvas::new_with_scale(800, 600, 1.25);
        let c2 = SurfaceCanvas::new_with_scale(1024, 768, 1.25);
        assert!(std::rc::Rc::ptr_eq(&c1.text_engine, &c2.text_engine));

        c1.shrink_for_minimized();
        assert_eq!(c1.phys_width, 1);
        assert_eq!(c1.phys_height, 1);

        c1.resize_physical(800, 600, 1.25);
        assert_eq!(c1.phys_width, 800);
        assert_eq!(c1.phys_height, 600);
        assert!(std::rc::Rc::ptr_eq(&c1.text_engine, &c2.text_engine));

        c1.release_idle_framebuffer();
        assert_eq!(c1.width, 640);
        assert_eq!(c1.height, 480);
        assert_eq!((c1.pixmap.width(), c1.pixmap.height()), (1, 1));

        c1.resize_physical(800, 600, 1.25);
        assert_eq!((c1.pixmap.width(), c1.pixmap.height()), (800, 600));

        c1.trim_text_caches();
        c1.draw_text_styled(4, 4, "hello", 0xFFFFFF, 100, super::FontStyle::UiRegular);
    }
}
