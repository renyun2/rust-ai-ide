//! 矢量文件图标。图形来自 Material Icon Theme（MIT）与 vscode-icons 的文件夹图标（MIT）。
//! 运行时把 SVG path 光栅到当前 DPI，避免 14px 手绘笔画发虚。

use std::collections::HashMap;
use std::sync::Mutex;
use tiny_skia::{FillRule, Paint, Path as SkiaPath, PathBuilder, Pixmap, Transform};

struct SvgDoc {
    vb_x: f32,
    vb_y: f32,
    vb_w: f32,
    vb_h: f32,
    paths: Vec<(u32, SkiaPath)>,
}

pub fn rasterize_svg_to_rgba(svg: &str, max_side: u32) -> Option<(u32, u32, Vec<u8>)> {
    let doc = parse_svg_for_preview(svg)?;
    if doc.vb_w <= 0.0 || doc.vb_h <= 0.0 || max_side == 0 {
        return None;
    }
    let scale = (max_side as f32 / doc.vb_w.max(doc.vb_h)).clamp(0.1, 32.0);
    let out_w = ((doc.vb_w * scale).round() as u32).clamp(1, max_side);
    let out_h = ((doc.vb_h * scale).round() as u32).clamp(1, max_side);
    let mut pixmap = Pixmap::new(out_w, out_h)?;
    let xform = Transform::from_scale(out_w as f32 / doc.vb_w, out_h as f32 / doc.vb_h)
        .pre_translate(-doc.vb_x, -doc.vb_y);
    for (color, path) in &doc.paths {
        let mut paint = Paint::default();
        let r = ((*color >> 16) & 0xFF) as u8;
        let g = ((*color >> 8) & 0xFF) as u8;
        let b = (*color & 0xFF) as u8;
        paint.set_color_rgba8(r, g, b, 255);
        paint.anti_alias = true;
        pixmap.fill_path(path, &paint, FillRule::Winding, xform, None);
    }
    let mut rgba = Vec::with_capacity((out_w as usize) * (out_h as usize) * 4);
    for p in pixmap.pixels() {
        let a = p.alpha();
        if a == 0 {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else if a == 255 {
            rgba.extend_from_slice(&[p.red(), p.green(), p.blue(), 255]);
        } else {
            let af = a as f32 / 255.0;
            let ur = ((p.red() as f32 / af).round()).clamp(0.0, 255.0) as u8;
            let ug = ((p.green() as f32 / af).round()).clamp(0.0, 255.0) as u8;
            let ub = ((p.blue() as f32 / af).round()).clamp(0.0, 255.0) as u8;
            rgba.extend_from_slice(&[ur, ug, ub, a]);
        }
    }
    Some((out_w, out_h, rgba))
}

fn parse_svg_for_preview(svg: &str) -> Option<SvgDoc> {
    if let Some(doc) = parse_svg(svg) {
        return Some(doc);
    }
    let (vb_x, vb_y, vb_w, vb_h) = parse_view_box(svg);
    let mut paths = Vec::new();
    let bytes = svg.as_bytes();
    let mut i = 0;
    while i + 5 < bytes.len() {
        if bytes[i..].starts_with(b"<path") {
            let start = i;
            let end = svg[start..].find('>')? + start;
            let tag = &svg[start..=end];
            if let Some(d) = attr(tag, "d") {
                let fill_raw = attr(tag, "fill").unwrap_or("#CBD5E1");
                if !fill_raw.eq_ignore_ascii_case("none") {
                    let color = parse_hex_color(fill_raw).unwrap_or(0xCBD5E1);
                    if let Some(path) = parse_path_data(d) {
                        paths.push((color, path));
                    }
                }
            }
            i = end + 1;
        } else if bytes[i..].starts_with(b"<rect") {
            let start = i;
            let end = svg[start..].find('>')? + start;
            let tag = &svg[start..=end];
            let x = attr(tag, "x").and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
            let y = attr(tag, "y").and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
            let w = attr(tag, "width").and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
            let h = attr(tag, "height").and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
            let fill_raw = attr(tag, "fill").unwrap_or("#CBD5E1");
            if w > 0.0 && h > 0.0 && !fill_raw.eq_ignore_ascii_case("none") {
                if let Some(rect) = tiny_skia::Rect::from_xywh(x, y, w, h) {
                    paths.push((
                        parse_hex_color(fill_raw).unwrap_or(0xCBD5E1),
                        PathBuilder::from_rect(rect),
                    ));
                }
            }
            i = end + 1;
        } else if bytes[i..].starts_with(b"<circle") {
            let start = i;
            let end = svg[start..].find('>')? + start;
            let tag = &svg[start..=end];
            let cx = attr(tag, "cx").and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
            let cy = attr(tag, "cy").and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
            let r = attr(tag, "r").and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
            let fill_raw = attr(tag, "fill").unwrap_or("#CBD5E1");
            if r > 0.0 && !fill_raw.eq_ignore_ascii_case("none") {
                if let Some(path) = PathBuilder::from_circle(cx, cy, r) {
                    paths.push((parse_hex_color(fill_raw).unwrap_or(0xCBD5E1), path));
                }
            }
            i = end + 1;
        } else {
            i += 1;
        }
    }
    if paths.is_empty() {
        None
    } else {
        Some(SvgDoc {
            vb_x,
            vb_y,
            vb_w,
            vb_h,
            paths,
        })
    }
}

pub fn paint_icon(pixmap: &mut Pixmap, scale: f32, x: f32, y: f32, w: f32, h: f32, name: &str) {
    let Some(doc) = cached_icon(name).or_else(|| cached_icon("document")) else {
        return;
    };
    if doc.vb_w <= 0.0 || doc.vb_h <= 0.0 || w <= 0.0 || h <= 0.0 {
        return;
    }
    let xform = Transform::from_scale(scale, scale)
        .pre_translate(x, y)
        .pre_scale(w / doc.vb_w, h / doc.vb_h)
        .pre_translate(-doc.vb_x, -doc.vb_y);
    for (color, path) in &doc.paths {
        let mut paint = Paint::default();
        let r = ((*color >> 16) & 0xFF) as u8;
        let g = ((*color >> 8) & 0xFF) as u8;
        let b = (*color & 0xFF) as u8;
        paint.set_color_rgba8(r, g, b, 255);
        paint.anti_alias = true;
        pixmap.fill_path(path, &paint, FillRule::Winding, xform, None);
    }
}

fn cached_icon(name: &str) -> Option<SvgDocRef> {
    let mut cache = icon_cache().lock().ok()?;
    if !cache.contains_key(name) {
        let svg = icon_svg(name)?;
        cache.insert(name.to_string(), parse_svg(svg)?);
    }
    // 重新借用已缓存文档。调用方立即绘制，不把引用带出锁。
    // 用克隆路径避免锁贯穿绘制：图标路径很短。
    let doc = cache.get(name)?;
    Some(SvgDocRef {
        vb_x: doc.vb_x,
        vb_y: doc.vb_y,
        vb_w: doc.vb_w,
        vb_h: doc.vb_h,
        paths: doc.paths.clone(),
    })
}

struct SvgDocRef {
    vb_x: f32,
    vb_y: f32,
    vb_w: f32,
    vb_h: f32,
    paths: Vec<(u32, SkiaPath)>,
}

fn icon_cache() -> &'static Mutex<HashMap<String, SvgDoc>> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Mutex<HashMap<String, SvgDoc>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn icon_svg(name: &str) -> Option<&'static str> {
    Some(match name {
        "folder" => include_str!("../assets/file-icons/folder.svg"),
        "document" => include_str!("../assets/file-icons/document.svg"),
        "markdown" => include_str!("../assets/file-icons/markdown.svg"),
        "readme" => include_str!("../assets/file-icons/readme.svg"),
        "rust" => include_str!("../assets/file-icons/rust.svg"),
        "python" => include_str!("../assets/file-icons/python.svg"),
        "jupyter" => include_str!("../assets/file-icons/jupyter.svg"),
        "typescript" => include_str!("../assets/file-icons/typescript.svg"),
        "react-ts" => include_str!("../assets/file-icons/react-ts.svg"),
        "javascript" => include_str!("../assets/file-icons/javascript.svg"),
        "react" => include_str!("../assets/file-icons/react.svg"),
        "go" => include_str!("../assets/file-icons/go.svg"),
        "go-mod" => include_str!("../assets/file-icons/go-mod.svg"),
        "java" => include_str!("../assets/file-icons/java.svg"),
        "kotlin" => include_str!("../assets/file-icons/kotlin.svg"),
        "c" => include_str!("../assets/file-icons/c.svg"),
        "cpp" => include_str!("../assets/file-icons/cpp.svg"),
        "csharp" => include_str!("../assets/file-icons/csharp.svg"),
        "php" => include_str!("../assets/file-icons/php.svg"),
        "ruby" => include_str!("../assets/file-icons/ruby.svg"),
        "swift" => include_str!("../assets/file-icons/swift.svg"),
        "dart" => include_str!("../assets/file-icons/dart.svg"),
        "lua" => include_str!("../assets/file-icons/lua.svg"),
        "zig" => include_str!("../assets/file-icons/zig.svg"),
        "haskell" => include_str!("../assets/file-icons/haskell.svg"),
        "elixir" => include_str!("../assets/file-icons/elixir.svg"),
        "scala" => include_str!("../assets/file-icons/scala.svg"),
        "r" => include_str!("../assets/file-icons/r.svg"),
        "perl" => include_str!("../assets/file-icons/perl.svg"),
        "console" => include_str!("../assets/file-icons/console.svg"),
        "powershell" => include_str!("../assets/file-icons/powershell.svg"),
        "database" => include_str!("../assets/file-icons/database.svg"),
        "html" => include_str!("../assets/file-icons/html.svg"),
        "css" => include_str!("../assets/file-icons/css.svg"),
        "sass" => include_str!("../assets/file-icons/sass.svg"),
        "less" => include_str!("../assets/file-icons/less.svg"),
        "vue" => include_str!("../assets/file-icons/vue.svg"),
        "svelte" => include_str!("../assets/file-icons/svelte.svg"),
        "astro" => include_str!("../assets/file-icons/astro.svg"),
        "json" => include_str!("../assets/file-icons/json.svg"),
        "yaml" => include_str!("../assets/file-icons/yaml.svg"),
        "toml" => include_str!("../assets/file-icons/toml.svg"),
        "xml" => include_str!("../assets/file-icons/xml.svg"),
        "graphql" => include_str!("../assets/file-icons/graphql.svg"),
        "proto" => include_str!("../assets/file-icons/proto.svg"),
        "webassembly" => include_str!("../assets/file-icons/webassembly.svg"),
        "groovy" => include_str!("../assets/file-icons/groovy.svg"),
        "clojure" => include_str!("../assets/file-icons/clojure.svg"),
        "erlang" => include_str!("../assets/file-icons/erlang.svg"),
        "ocaml" => include_str!("../assets/file-icons/ocaml.svg"),
        "fsharp" => include_str!("../assets/file-icons/fsharp.svg"),
        "julia" => include_str!("../assets/file-icons/julia.svg"),
        "nim" => include_str!("../assets/file-icons/nim.svg"),
        "assembly" => include_str!("../assets/file-icons/assembly.svg"),
        "solidity" => include_str!("../assets/file-icons/solidity.svg"),
        "crystal" => include_str!("../assets/file-icons/crystal.svg"),
        "elm" => include_str!("../assets/file-icons/elm.svg"),
        "purescript" => include_str!("../assets/file-icons/purescript.svg"),
        "haxe" => include_str!("../assets/file-icons/haxe.svg"),
        "gleam" => include_str!("../assets/file-icons/gleam.svg"),
        "coffee" => include_str!("../assets/file-icons/coffee.svg"),
        "rescript" => include_str!("../assets/file-icons/rescript.svg"),
        "shader" => include_str!("../assets/file-icons/shader.svg"),
        "visualstudio" => include_str!("../assets/file-icons/visualstudio.svg"),
        "unity" => include_str!("../assets/file-icons/unity.svg"),
        "godot" => include_str!("../assets/file-icons/godot.svg"),
        "http" => include_str!("../assets/file-icons/http.svg"),
        "makefile" => include_str!("../assets/file-icons/makefile.svg"),
        "docker" => include_str!("../assets/file-icons/docker.svg"),
        "git" => include_str!("../assets/file-icons/git.svg"),
        "lock" => include_str!("../assets/file-icons/lock.svg"),
        "zip" => include_str!("../assets/file-icons/zip.svg"),
        "pdf" => include_str!("../assets/file-icons/pdf.svg"),
        "image" => include_str!("../assets/file-icons/image.svg"),
        "svg" => include_str!("../assets/file-icons/svg.svg"),
        "font" => include_str!("../assets/file-icons/font.svg"),
        "audio" => include_str!("../assets/file-icons/audio.svg"),
        "video" => include_str!("../assets/file-icons/video.svg"),
        "certificate" => include_str!("../assets/file-icons/certificate.svg"),
        "license" => include_str!("../assets/file-icons/license.svg"),
        "settings" => include_str!("../assets/file-icons/settings.svg"),
        "exe" => include_str!("../assets/file-icons/exe.svg"),
        "diff" => include_str!("../assets/file-icons/diff.svg"),
        "log" => include_str!("../assets/file-icons/log.svg"),
        "cmake" => include_str!("../assets/file-icons/cmake.svg"),
        "bazel" => include_str!("../assets/file-icons/bazel.svg"),
        "maven" => include_str!("../assets/file-icons/maven.svg"),
        "gradle" => include_str!("../assets/file-icons/gradle.svg"),
        "meson" => include_str!("../assets/file-icons/meson.svg"),
        "nuget" => include_str!("../assets/file-icons/nuget.svg"),
        "table" => include_str!("../assets/file-icons/table.svg"),
        "key" => include_str!("../assets/file-icons/key.svg"),
        "changelog" => include_str!("../assets/file-icons/changelog.svg"),
        "helm" => include_str!("../assets/file-icons/helm.svg"),
        "jenkins" => include_str!("../assets/file-icons/jenkins.svg"),
        "vite" => include_str!("../assets/file-icons/vite.svg"),
        "eslint" => include_str!("../assets/file-icons/eslint.svg"),
        "prettier" => include_str!("../assets/file-icons/prettier.svg"),
        "editorconfig" => include_str!("../assets/file-icons/editorconfig.svg"),
        "github-actions-workflow" => {
            include_str!("../assets/file-icons/github-actions-workflow.svg")
        }
        "nginx" => include_str!("../assets/file-icons/nginx.svg"),
        "taskfile" => include_str!("../assets/file-icons/taskfile.svg"),
        "just" => include_str!("../assets/file-icons/just.svg"),
        "npm" => include_str!("../assets/file-icons/npm.svg"),
        "pnpm" => include_str!("../assets/file-icons/pnpm.svg"),
        "yarn" => include_str!("../assets/file-icons/yarn.svg"),
        "terraform" => include_str!("../assets/file-icons/terraform.svg"),
        "authors" => include_str!("../assets/file-icons/authors.svg"),
        "codeowners" => include_str!("../assets/file-icons/codeowners.svg"),
        "contributing" => include_str!("../assets/file-icons/contributing.svg"),
        "webpack" => include_str!("../assets/file-icons/webpack.svg"),
        "gitlab" => include_str!("../assets/file-icons/gitlab.svg"),
        "kubernetes" => include_str!("../assets/file-icons/kubernetes.svg"),
        "azure" => include_str!("../assets/file-icons/azure.svg"),
        _ => return None,
    })
}

fn parse_svg(svg: &str) -> Option<SvgDoc> {
    let (vb_x, vb_y, vb_w, vb_h) = parse_view_box(svg);
    let mut paths = Vec::new();
    let bytes = svg.as_bytes();
    let mut i = 0;
    while i + 5 < bytes.len() {
        if bytes[i..].starts_with(b"<path") {
            let start = i;
            let end = svg[start..].find('>')? + start;
            let tag = &svg[start..=end];
            if let Some((color, data)) = path_tag(tag) {
                if let Some(path) = parse_path_data(&data) {
                    paths.push((color, path));
                }
            }
            i = end + 1;
        } else {
            i += 1;
        }
    }
    if paths.is_empty() {
        return None;
    }
    Some(SvgDoc {
        vb_x,
        vb_y,
        vb_w,
        vb_h,
        paths,
    })
}

fn parse_view_box(svg: &str) -> (f32, f32, f32, f32) {
    let Some(i) = svg.find("viewBox=") else {
        return (0.0, 0.0, 32.0, 32.0);
    };
    let rest = &svg[i + "viewBox=".len()..];
    let quote = rest.chars().next().unwrap_or('"');
    let body = rest[quote.len_utf8()..].split(quote).next().unwrap_or("");
    let nums: Vec<f32> = body
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() == 4 && nums[2] > 0.0 && nums[3] > 0.0 {
        (nums[0], nums[1], nums[2], nums[3])
    } else {
        (0.0, 0.0, 32.0, 32.0)
    }
}

fn path_tag(tag: &str) -> Option<(u32, &str)> {
    let fill = attr(tag, "fill")?;
    if fill.eq_ignore_ascii_case("none") || fill.eq_ignore_ascii_case("currentcolor") {
        return None;
    }
    let d = attr(tag, "d")?;
    Some((parse_hex_color(fill).unwrap_or(0x808080), d))
}

fn attr<'a>(tag: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=");
    let i = tag.find(&needle)?;
    let rest = &tag[i + needle.len()..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let body = &rest[quote.len_utf8()..];
    body.split(quote).next()
}

fn parse_hex_color(raw: &str) -> Option<u32> {
    let hex = raw.trim().trim_start_matches('#');
    match hex.len() {
        3 => {
            let r = u32::from_str_radix(&hex[0..1], 16).ok()?;
            let g = u32::from_str_radix(&hex[1..2], 16).ok()?;
            let b = u32::from_str_radix(&hex[2..3], 16).ok()?;
            Some((r << 20) | (r << 16) | (g << 12) | (g << 8) | (b << 4) | b)
        }
        6 | 8 => u32::from_str_radix(&hex[..6], 16).ok(),
        _ => None,
    }
}

fn parse_path_data(d: &str) -> Option<SkiaPath> {
    let b = d.as_bytes();
    let mut i = 0;
    let mut pb = PathBuilder::new();
    let mut cx = 0.0;
    let mut cy = 0.0;
    let mut sx = 0.0;
    let mut sy = 0.0;
    let mut last_cmd = b'M';
    let mut prev_cubic: Option<(f32, f32)> = None;
    let mut prev_quad: Option<(f32, f32)> = None;
    let mut started = false;
    while i < b.len() {
        skip_seps(b, &mut i);
        if i >= b.len() {
            break;
        }
        let explicit = b[i].is_ascii_alphabetic();
        let mut cmd = if explicit {
            let c = b[i];
            i += 1;
            c
        } else if last_cmd == b'M' {
            b'L'
        } else if last_cmd == b'm' {
            b'l'
        } else {
            last_cmd
        };
        if matches!(cmd, b'Z' | b'z') {
            pb.close();
            cx = sx;
            cy = sy;
            prev_cubic = None;
            prev_quad = None;
            last_cmd = cmd;
            continue;
        }
        let rel = cmd.is_ascii_lowercase();
        cmd = cmd.to_ascii_uppercase();
        match cmd {
            b'M' | b'L' => {
                let x = read_num(b, &mut i)?;
                let y = read_num(b, &mut i)?;
                let (x, y) = if rel { (cx + x, cy + y) } else { (x, y) };
                if cmd == b'M' {
                    pb.move_to(x, y);
                    sx = x;
                    sy = y;
                    started = true;
                    last_cmd = if rel { b'l' } else { b'L' };
                } else if started {
                    pb.line_to(x, y);
                    last_cmd = if rel { b'l' } else { b'L' };
                }
                cx = x;
                cy = y;
                prev_cubic = None;
                prev_quad = None;
            }
            b'H' => {
                let x = read_num(b, &mut i)?;
                let x = if rel { cx + x } else { x };
                if started {
                    pb.line_to(x, cy);
                }
                cx = x;
                prev_cubic = None;
                prev_quad = None;
                last_cmd = if rel { b'h' } else { b'H' };
            }
            b'V' => {
                let y = read_num(b, &mut i)?;
                let y = if rel { cy + y } else { y };
                if started {
                    pb.line_to(cx, y);
                }
                cy = y;
                prev_cubic = None;
                prev_quad = None;
                last_cmd = if rel { b'v' } else { b'V' };
            }
            b'C' => {
                let (x1, y1) = (read_num(b, &mut i)?, read_num(b, &mut i)?);
                let (x2, y2) = (read_num(b, &mut i)?, read_num(b, &mut i)?);
                let (x, y) = (read_num(b, &mut i)?, read_num(b, &mut i)?);
                let (x1, y1, x2, y2, x, y) = if rel {
                    (cx + x1, cy + y1, cx + x2, cy + y2, cx + x, cy + y)
                } else {
                    (x1, y1, x2, y2, x, y)
                };
                if started {
                    pb.cubic_to(x1, y1, x2, y2, x, y);
                }
                prev_cubic = Some((x2, y2));
                prev_quad = None;
                cx = x;
                cy = y;
                last_cmd = if rel { b'c' } else { b'C' };
            }
            b'S' => {
                let (x2, y2) = (read_num(b, &mut i)?, read_num(b, &mut i)?);
                let (x, y) = (read_num(b, &mut i)?, read_num(b, &mut i)?);
                let (x2, y2, x, y) = if rel {
                    (cx + x2, cy + y2, cx + x, cy + y)
                } else {
                    (x2, y2, x, y)
                };
                let (x1, y1) = match prev_cubic {
                    Some((px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                    None => (cx, cy),
                };
                if started {
                    pb.cubic_to(x1, y1, x2, y2, x, y);
                }
                prev_cubic = Some((x2, y2));
                prev_quad = None;
                cx = x;
                cy = y;
                last_cmd = if rel { b's' } else { b'S' };
            }
            b'Q' => {
                let (x1, y1) = (read_num(b, &mut i)?, read_num(b, &mut i)?);
                let (x, y) = (read_num(b, &mut i)?, read_num(b, &mut i)?);
                let (x1, y1, x, y) = if rel {
                    (cx + x1, cy + y1, cx + x, cy + y)
                } else {
                    (x1, y1, x, y)
                };
                if started {
                    pb.quad_to(x1, y1, x, y);
                }
                prev_quad = Some((x1, y1));
                prev_cubic = None;
                cx = x;
                cy = y;
                last_cmd = if rel { b'q' } else { b'Q' };
            }
            b'T' => {
                let (x, y) = (read_num(b, &mut i)?, read_num(b, &mut i)?);
                let (x, y) = if rel { (cx + x, cy + y) } else { (x, y) };
                let (x1, y1) = match prev_quad {
                    Some((px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                    None => (cx, cy),
                };
                if started {
                    pb.quad_to(x1, y1, x, y);
                }
                prev_quad = Some((x1, y1));
                prev_cubic = None;
                cx = x;
                cy = y;
                last_cmd = if rel { b't' } else { b'T' };
            }
            b'A' => {
                let rx = read_num(b, &mut i)?.abs();
                let ry = read_num(b, &mut i)?.abs();
                let rot = read_num(b, &mut i)?;
                let large = read_flag(b, &mut i)?;
                let sweep = read_flag(b, &mut i)?;
                let (x, y) = (read_num(b, &mut i)?, read_num(b, &mut i)?);
                let (x, y) = if rel { (cx + x, cy + y) } else { (x, y) };
                if started {
                    arc_to(&mut pb, cx, cy, x, y, rx, ry, rot, large, sweep);
                }
                prev_cubic = None;
                prev_quad = None;
                cx = x;
                cy = y;
                last_cmd = if rel { b'a' } else { b'A' };
            }
            _ => break,
        }
    }
    pb.finish()
}

fn arc_to(
    pb: &mut PathBuilder,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    mut rx: f32,
    mut ry: f32,
    deg: f32,
    large: bool,
    sweep: bool,
) {
    if (x1 - x2).abs() < f32::EPSILON && (y1 - y2).abs() < f32::EPSILON {
        return;
    }
    if rx < f32::EPSILON || ry < f32::EPSILON {
        pb.line_to(x2, y2);
        return;
    }
    let phi = deg.to_radians();
    let (sin_p, cos_p) = phi.sin_cos();
    let dx = (x1 - x2) / 2.0;
    let dy = (y1 - y2) / 2.0;
    let x1p = cos_p * dx + sin_p * dy;
    let y1p = -sin_p * dx + cos_p * dy;
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let sign = if large == sweep { -1.0 } else { 1.0 };
    let num = (rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p)
        / (rx * rx * y1p * y1p + ry * ry * x1p * x1p);
    let coef = sign * num.max(0.0).sqrt();
    let cxp = coef * rx * y1p / ry;
    let cyp = coef * -ry * x1p / rx;
    let cx = cos_p * cxp - sin_p * cyp + (x1 + x2) / 2.0;
    let cy = sin_p * cxp + cos_p * cyp + (y1 + y2) / 2.0;
    let theta1 = angle((1.0, 0.0), ((x1p - cxp) / rx, (y1p - cyp) / ry));
    let mut dtheta = angle(
        ((x1p - cxp) / rx, (y1p - cyp) / ry),
        ((-x1p - cxp) / rx, (-y1p - cyp) / ry),
    );
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f32::consts::TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += std::f32::consts::TAU;
    }
    let steps = ((dtheta.abs() / (10.0_f32.to_radians())).ceil() as i32).max(1);
    for step in 1..=steps {
        let t = theta1 + dtheta * (step as f32 / steps as f32);
        let (st, ct) = t.sin_cos();
        let px = cx + rx * cos_p * ct - ry * sin_p * st;
        let py = cy + rx * sin_p * ct + ry * cos_p * st;
        pb.line_to(px, py);
    }
}

fn angle(u: (f32, f32), v: (f32, f32)) -> f32 {
    let dot = u.0 * v.0 + u.1 * v.1;
    let cross = u.0 * v.1 - u.1 * v.0;
    cross.atan2(dot)
}

fn read_flag(b: &[u8], i: &mut usize) -> Option<bool> {
    skip_seps(b, i);
    if *i >= b.len() {
        return None;
    }
    let c = b[*i];
    if c == b'0' || c == b'1' {
        *i += 1;
        return Some(c == b'1');
    }
    let n = read_num(b, i)?;
    Some(n != 0.0)
}

fn read_num(b: &[u8], i: &mut usize) -> Option<f32> {
    skip_seps(b, i);
    if *i >= b.len() {
        return None;
    }
    let start = *i;
    if b[*i] == b'+' || b[*i] == b'-' {
        *i += 1;
    }
    let mut saw_digit = false;
    let mut saw_dot = false;
    while *i < b.len() {
        let c = b[*i];
        if c.is_ascii_digit() {
            saw_digit = true;
            *i += 1;
        } else if c == b'.' && !saw_dot {
            saw_dot = true;
            *i += 1;
        } else {
            break;
        }
    }
    if *i < b.len() && (b[*i] == b'e' || b[*i] == b'E') {
        let mark = *i;
        *i += 1;
        if *i < b.len() && (b[*i] == b'+' || b[*i] == b'-') {
            *i += 1;
        }
        let exp_start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i == exp_start {
            *i = mark;
        }
    }
    if !saw_digit {
        *i = start;
        return None;
    }
    std::str::from_utf8(&b[start..*i]).ok()?.parse().ok()
}

fn skip_seps(b: &[u8], i: &mut usize) {
    while *i < b.len() {
        let c = b[*i];
        if c == b',' || c.is_ascii_whitespace() {
            *i += 1;
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_svg;

    #[test]
    fn material_rust_icon_parses_to_a_filled_shape() {
        let doc = parse_svg(include_str!("../assets/file-icons/rust.svg")).expect("rust svg");
        assert!(doc.vb_w > 0.0 && doc.vb_h > 0.0);
        assert!(!doc.paths.is_empty());
    }

    #[test]
    fn python_icon_with_arcs_parses() {
        let doc = parse_svg(include_str!("../assets/file-icons/python.svg")).expect("python svg");
        assert!(doc.paths.len() >= 2);
    }

    #[test]
    fn markdown_and_readme_icons_parse() {
        let md = parse_svg(include_str!("../assets/file-icons/markdown.svg")).expect("markdown");
        let readme = parse_svg(include_str!("../assets/file-icons/readme.svg")).expect("readme");
        assert!(!md.paths.is_empty());
        assert!(!readme.paths.is_empty());
    }
}
