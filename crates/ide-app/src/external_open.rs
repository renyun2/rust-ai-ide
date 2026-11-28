use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Target parsed from CLI flags, file:line:col specs, or custom IDE URL schemes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExternalOpenTarget {
    OpenFile {
        path: PathBuf,
        line: Option<usize>,
        column: Option<usize>,
        cwd: Option<PathBuf>,
        new_window: bool,
    },
    OpenWorkspace {
        path: PathBuf,
        new_window: bool,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedCliArgs {
    pub targets: Vec<ExternalOpenTarget>,
    pub explicit_workspace: Option<PathBuf>,
    pub new_window: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IpcEndpointInfo {
    port: u16,
    pid: u32,
    token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IpcOpenMessage {
    token: String,
    targets: Vec<ExternalOpenTarget>,
}

/// Removes the IPC endpoint file when the owning IDE process exits gracefully.
pub struct IpcServerGuard {
    info_path: PathBuf,
    pid: u32,
}

impl Drop for IpcServerGuard {
    fn drop(&mut self) {
        if let Ok(raw) = fs::read_to_string(&self.info_path) {
            if let Ok(info) = serde_json::from_str::<IpcEndpointInfo>(&raw) {
                if info.pid == self.pid {
                    let _ = fs::remove_file(&self.info_path);
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct EditorIntegrationPaths {
    pub exe_path: PathBuf,
    pub cli_bin_dir: PathBuf,
    pub launch_editor_value: String,
}

pub fn ipc_state_path(storage_dir: Option<&Path>) -> PathBuf {
    match storage_dir {
        Some(dir) => dir.join("ide-ipc.json"),
        None => std::env::temp_dir().join("rust-ai-ide-ipc.json"),
    }
}

fn generate_ipc_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let stack_hint = (&nanos as *const u128) as usize as u128;
    let mixed = nanos ^ (pid << 32) ^ stack_hint.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    format!("{mixed:032x}")
}

/// Starts the single-instance localhost IPC server and writes connection metadata to `ide-ipc.json`.
pub fn start_ipc_server<F>(storage_dir: Option<&Path>, on_targets: F) -> Option<IpcServerGuard>
where
    F: Fn(Vec<ExternalOpenTarget>) + Send + Sync + 'static,
{
    let info_path = ipc_state_path(storage_dir);
    if let Some(parent) = info_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    let pid = std::process::id();
    let token = generate_ipc_token();
    let info = IpcEndpointInfo {
        port,
        pid,
        token: token.clone(),
    };
    let json = serde_json::to_string_pretty(&info).ok()?;
    let tmp_path = info_path.with_extension("json.tmp");
    if fs::write(&tmp_path, &json).is_ok() {
        if fs::rename(&tmp_path, &info_path).is_err() {
            let _ = fs::write(&info_path, &json);
            let _ = fs::remove_file(&tmp_path);
        }
    } else {
        let _ = fs::write(&info_path, &json);
    }

    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut stream) = conn else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<IpcOpenMessage>(line.trim()) else {
                continue;
            };
            if msg.token != token || msg.targets.is_empty() {
                continue;
            }
            let ack = format!("OK {pid}\n");
            let _ = stream.write_all(ack.as_bytes());
            let _ = stream.flush();
            on_targets(msg.targets);
        }
    });

    Some(IpcServerGuard { info_path, pid })
}

#[cfg(target_os = "windows")]
fn allow_set_foreground_window(pid: u32) {
    #[link(name = "user32")]
    extern "system" {
        fn AllowSetForegroundWindow(dw_process_id: u32) -> i32;
    }
    unsafe {
        let _ = AllowSetForegroundWindow(pid);
    }
}

#[cfg(not(target_os = "windows"))]
fn allow_set_foreground_window(_pid: u32) {}

/// Forwards open targets to an already-running IDE instance if one is listening.
pub fn try_forward_via_ipc(storage_dir: Option<&Path>, targets: &[ExternalOpenTarget]) -> bool {
    if targets.is_empty() {
        return false;
    }
    let info_path = ipc_state_path(storage_dir);
    let Ok(raw) = fs::read_to_string(&info_path) else {
        return false;
    };
    let Ok(info) = serde_json::from_str::<IpcEndpointInfo>(&raw) else {
        return false;
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, info.port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    let payload = IpcOpenMessage {
        token: info.token,
        targets: targets.to_vec(),
    };
    let Ok(mut encoded) = serde_json::to_string(&payload) else {
        return false;
    };
    encoded.push('\n');
    if stream.write_all(encoded.as_bytes()).is_err() || stream.flush().is_err() {
        return false;
    }
    let mut ack = String::new();
    let mut reader = BufReader::new(&stream);
    if reader.read_line(&mut ack).is_err() {
        return false;
    }
    if let Some(rest) = ack.trim().strip_prefix("OK") {
        let server_pid = rest.trim().parse::<u32>().unwrap_or(info.pid);
        allow_set_foreground_window(server_pid);
        return true;
    }
    false
}

/// Checks whether a string looks like a Windows drive letter prefix such as `C:` or `D:`.
#[inline]
fn is_windows_drive_colon(prefix: &str) -> bool {
    let bytes = prefix.as_bytes();
    bytes.len() == 1 && bytes[0].is_ascii_alphabetic()
}

fn strip_trailing_colon_usize(s: &str) -> Option<(&str, usize)> {
    let idx = s.rfind(':')?;
    if idx == 0 {
        return None;
    }
    let head = &s[..idx];
    if is_windows_drive_colon(head) {
        return None;
    }
    let tail = &s[idx + 1..];
    if tail.is_empty() || !tail.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let val = tail.parse::<usize>().ok()?;
    Some((head, val))
}

fn parse_hash_l_fragment(frag: &str) -> Option<(Option<usize>, Option<usize>)> {
    let body = frag.strip_prefix('L').or_else(|| frag.strip_prefix('l'))?;
    let line_digits: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
    if line_digits.is_empty() {
        return None;
    }
    let line = line_digits.parse::<usize>().ok()?.max(1);
    let rest = &body[line_digits.len()..];
    let col = if let Some(after_c) = rest.strip_prefix('C').or_else(|| rest.strip_prefix('c')) {
        let col_digits: String = after_c.chars().take_while(|c| c.is_ascii_digit()).collect();
        col_digits
            .parse::<usize>()
            .ok()
            .filter(|&c| c > 0)
    } else {
        None
    };
    Some((Some(line), col))
}

fn parse_paren_line_col(s: &str) -> Option<(&str, usize, Option<usize>)> {
    let open_idx = s.rfind('(')?;
    if open_idx == 0 || !s.ends_with(')') {
        return None;
    }
    let inner = &s[open_idx + 1..s.len() - 1];
    let head = &s[..open_idx];
    if let Some((l_str, c_str)) = inner.split_once(',') {
        let l = l_str.trim().parse::<usize>().ok()?;
        let c = c_str.trim().parse::<usize>().ok()?;
        Some((head, l.max(1), (c > 0).then_some(c)))
    } else {
        let l = inner.trim().parse::<usize>().ok()?;
        Some((head, l.max(1), None))
    }
}

/// Splits `path:line:col`, `path:line`, `path(line,col)`, `path#LlineCcol`, etc.
/// Preserves Windows drive letters (`C:\...`) and existing literal paths.
pub fn split_path_and_position(raw: &str) -> (String, Option<usize>, Option<usize>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (String::new(), None, None);
    }
    if Path::new(trimmed).exists() {
        return (trimmed.to_string(), None, None);
    }
    if let Some((before_hash, frag)) = trimmed.rsplit_once('#') {
        if !before_hash.is_empty() {
            if let Some((line, col)) = parse_hash_l_fragment(frag) {
                return (before_hash.to_string(), line, col);
            }
        }
    }
    if let Some((head, line, col)) = parse_paren_line_col(trimmed) {
        return (head.to_string(), Some(line), col);
    }
    if let Some((head, first_num)) = strip_trailing_colon_usize(trimmed) {
        if let Some((base, line_num)) = strip_trailing_colon_usize(head) {
            let line = line_num.max(1);
            let col = (first_num > 0).then_some(first_num);
            return (base.to_string(), Some(line), col);
        }
        return (head.to_string(), Some(first_num.max(1)), None);
    }
    (trimmed.to_string(), None, None)
}

fn percent_decode(input: &str, plus_as_space: bool) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h1 = (bytes[i + 1] as char).to_digit(16);
            let h2 = (bytes[i + 2] as char).to_digit(16);
            if let (Some(a), Some(b)) = (h1, h2) {
                out.push(((a << 4) | b) as u8);
                i += 3;
                continue;
            }
        }
        if plus_as_space && bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn normalize_url_filesystem_path(raw: &str) -> String {
    let decoded = percent_decode(raw, false);
    let bytes = decoded.as_bytes();
    // On Windows / URI paths like `/D:/work/app.vue` or `//D:/work/app.vue`, strip leading slashes before `X:`.
    let trimmed_slashes = decoded.trim_start_matches('/');
    if trimmed_slashes.len() >= 2
        && trimmed_slashes.as_bytes()[0].is_ascii_alphabetic()
        && trimmed_slashes.as_bytes()[1] == b':'
    {
        return trimmed_slashes.to_string();
    }
    if bytes.starts_with(b"//") {
        return format!("/{}", decoded.trim_start_matches('/'));
    }
    decoded
}

fn parse_query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| match part.split_once('=') {
            Some((k, v)) => (
                percent_decode(k, true).to_ascii_lowercase(),
                percent_decode(v, true),
            ),
            None => (percent_decode(part, true).to_ascii_lowercase(), String::new()),
        })
        .collect()
}

/// Parses `rust-ai-ide://`, `vscode://`, `cursor://`, `idea://`, `jetbrains://`, and `file://` URIs.
pub fn parse_external_uri(raw: &str) -> Option<ExternalOpenTarget> {
    let trimmed = raw.trim();
    let scheme_end = trimmed.find("://")?;
    let scheme = trimmed[..scheme_end].to_ascii_lowercase();
    let supported = matches!(
        scheme.as_str(),
        "rust-ai-ide"
            | "vscode"
            | "vscode-insiders"
            | "cursor"
            | "windsurf"
            | "trae"
            | "idea"
            | "webstorm"
            | "pycharm"
            | "goland"
            | "clion"
            | "rustrover"
            | "jetbrains"
            | "file"
    );
    if !supported {
        return None;
    }

    let after_scheme = &trimmed[scheme_end + 3..];
    let (before_frag, fragment) = match after_scheme.split_once('#') {
        Some((a, f)) => (a, Some(f)),
        None => (after_scheme, None),
    };
    let (path_part, query_part) = match before_frag.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (before_frag, None),
    };
    let query_pairs = query_part.map(parse_query_pairs).unwrap_or_default();
    let find_q = |keys: &[&str]| -> Option<String> {
        query_pairs
            .iter()
            .find(|(k, v)| keys.contains(&k.as_str()) && !v.is_empty())
            .map(|(_, v)| v.clone())
    };

    let q_line = find_q(&["line", "l"]).and_then(|s| s.parse::<usize>().ok());
    let q_col = find_q(&["column", "col", "c"]).and_then(|s| s.parse::<usize>().ok());
    let (frag_line, frag_col) = fragment
        .and_then(parse_hash_l_fragment)
        .unwrap_or((None, None));
    let new_window = find_q(&["windowid", "newwindow"])
        .is_some_and(|v| v == "_blank" || v.eq_ignore_ascii_case("true") || v == "1");

    if scheme == "file" {
        let stripped = path_part
            .strip_prefix("localhost/")
            .or_else(|| path_part.strip_prefix("localhost\\"))
            .unwrap_or(path_part);
        let norm = normalize_url_filesystem_path(stripped);
        let (clean_path, emb_line, emb_col) = split_path_and_position(&norm);
        if clean_path.is_empty() {
            return None;
        }
        let path = PathBuf::from(&clean_path);
        let line = q_line.or(frag_line).or(emb_line).map(|n| n.max(1));
        let col = q_col.or(frag_col).or(emb_col).filter(|&n| n > 0);
        if line.is_none() && col.is_none() && path.is_dir() {
            return Some(ExternalOpenTarget::OpenWorkspace { path, new_window });
        }
        return Some(ExternalOpenTarget::OpenFile {
            path,
            line,
            column: col,
            cwd: None,
            new_window,
        });
    }

    // `rust-ai-ide://file/<path>[:line[:col]]` or `vscode://file/<path>[:line[:col]]`
    let lower_path = path_part.to_ascii_lowercase();
    if let Some(rest) = lower_path
        .strip_prefix("file/")
        .map(|_| &path_part[5..])
        .or_else(|| {
            lower_path
                .strip_prefix("open/")
                .map(|_| &path_part[5..])
        })
    {
        if !rest.is_empty() {
            let norm = normalize_url_filesystem_path(rest);
            let (clean_path, emb_line, emb_col) = split_path_and_position(&norm);
            if !clean_path.is_empty() {
                let path = PathBuf::from(&clean_path);
                let line = q_line.or(frag_line).or(emb_line).map(|n| n.max(1));
                let col = q_col.or(frag_col).or(emb_col).filter(|&n| n > 0);
                if line.is_none() && col.is_none() && path.is_dir() {
                    return Some(ExternalOpenTarget::OpenWorkspace { path, new_window });
                }
                return Some(ExternalOpenTarget::OpenFile {
                    path,
                    line,
                    column: col,
                    cwd: None,
                    new_window,
                });
            }
        }
    }

    // `rust-ai-ide://open?file=...&line=...&column=...` / `idea://open?file=...` / `jetbrains://...`
    if let Some(raw_file) = find_q(&["file", "path", "url", "target"]) {
        if raw_file.to_ascii_lowercase().starts_with("file://") {
            if let Some(ExternalOpenTarget::OpenFile {
                path,
                line: u_line,
                column: u_col,
                cwd,
                ..
            }) = parse_external_uri(&raw_file)
            {
                return Some(ExternalOpenTarget::OpenFile {
                    path,
                    line: q_line.or(frag_line).or(u_line).map(|n| n.max(1)),
                    column: q_col.or(frag_col).or(u_col).filter(|&n| n > 0),
                    cwd,
                    new_window,
                });
            }
        }
        let norm = normalize_url_filesystem_path(&raw_file);
        let (clean_path, emb_line, emb_col) = split_path_and_position(&norm);
        if !clean_path.is_empty() {
            let path = PathBuf::from(&clean_path);
            let line = q_line.or(frag_line).or(emb_line).map(|n| n.max(1));
            let col = q_col.or(frag_col).or(emb_col).filter(|&n| n > 0);
            if line.is_none() && col.is_none() && path.is_dir() {
                return Some(ExternalOpenTarget::OpenWorkspace { path, new_window });
            }
            return Some(ExternalOpenTarget::OpenFile {
                path,
                line,
                column: col,
                cwd: None,
                new_window,
            });
        }
    }

    None
}

fn parse_plus_position(arg: &str) -> Option<(usize, Option<usize>)> {
    let rest = arg.strip_prefix('+')?;
    if rest.is_empty() {
        return None;
    }
    if let Some((l_str, c_str)) = rest.split_once(':') {
        let l = l_str.parse::<usize>().ok()?;
        let c = c_str.parse::<usize>().ok()?;
        Some((l.max(1), (c > 0).then_some(c)))
    } else if rest.chars().all(|c| c.is_ascii_digit()) {
        let l = rest.parse::<usize>().ok()?;
        Some((l.max(1), None))
    } else {
        None
    }
}

fn resolve_path_against_cwd(path: PathBuf, cwd: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    if let Some(base) = cwd {
        let candidate = base.join(&path);
        if candidate.exists() {
            return candidate;
        }
    }
    path
}

fn looks_like_file_arg(raw: &str) -> bool {
    raw.contains('.') || raw.contains('/') || raw.contains('\\')
}

/// Parses CLI arguments across VS Code (`-g`/`--goto`), JetBrains (`--line`/`--column`),
/// Vim (`+line[:col]`), `launch-editor`/`launch-ide`, TypeScript/MSBuild (`file(line,col)`),
/// GitHub (`file#LlineCcol`), and URL schemes (`rust-ai-ide://`, `vscode://`, `idea://`).
pub fn parse_cli_args(args: &[String], cwd: Option<&Path>) -> ParsedCliArgs {
    let mut parsed = ParsedCliArgs::default();
    let mut pending_line: Option<usize> = None;
    let mut pending_col: Option<usize> = None;
    let mut stop_flags = false;
    let mut i = 1usize;

    while i < args.len() {
        let arg = &args[i];
        if !stop_flags {
            if arg == "--" {
                stop_flags = true;
                i += 1;
                continue;
            }
            if arg == "--self-check" || arg == "--ui-test" || arg == "-w" || arg == "--wait"
                || arg == "-r" || arg == "--reuse-window"
            {
                i += 1;
                continue;
            }
            if arg == "-n" || arg == "--new-window" {
                parsed.new_window = true;
                i += 1;
                continue;
            }
            if arg == "--export-icon" {
                if args.get(i + 1).is_some_and(|next| !next.starts_with('-')) {
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if arg == "--workspace" {
                if let Some(next) = args.get(i + 1) {
                    let p = resolve_path_against_cwd(PathBuf::from(next), cwd);
                    if p.is_dir() {
                        parsed.explicit_workspace = Some(p.clone());
                        parsed.targets.push(ExternalOpenTarget::OpenWorkspace {
                            path: p,
                            new_window: parsed.new_window,
                        });
                    }
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if let Some(val) = arg.strip_prefix("--workspace=") {
                let p = resolve_path_against_cwd(PathBuf::from(val), cwd);
                if p.is_dir() {
                    parsed.explicit_workspace = Some(p.clone());
                    parsed.targets.push(ExternalOpenTarget::OpenWorkspace {
                        path: p,
                        new_window: parsed.new_window,
                    });
                }
                i += 1;
                continue;
            }
            if arg == "-l" || arg == "--line" {
                if let Some(val) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                    pending_line = Some(val.max(1));
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if let Some(val) = arg
                .strip_prefix("--line=")
                .and_then(|s| s.parse::<usize>().ok())
            {
                pending_line = Some(val.max(1));
                i += 1;
                continue;
            }
            if arg == "-c" || arg == "--column" || arg == "--col" {
                if let Some(val) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                    pending_col = (val > 0).then_some(val);
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if let Some(val) = arg
                .strip_prefix("--column=")
                .or_else(|| arg.strip_prefix("--col="))
                .and_then(|s| s.parse::<usize>().ok())
            {
                pending_col = (val > 0).then_some(val);
                i += 1;
                continue;
            }
            if let Some((pl, pc)) = parse_plus_position(arg) {
                pending_line = Some(pl);
                if pc.is_some() {
                    pending_col = pc;
                }
                i += 1;
                continue;
            }
            if arg == "-g" || arg == "--goto" {
                if let Some(target_arg) = args.get(i + 1) {
                    push_target_token(
                        target_arg,
                        args.get(i + 2..).unwrap_or(&[]),
                        cwd,
                        &mut pending_line,
                        &mut pending_col,
                        parsed.new_window,
                        true,
                        &mut parsed,
                    );
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if let Some(target_arg) = arg.strip_prefix("--goto=") {
                push_target_token(
                    target_arg,
                    args.get(i + 1..).unwrap_or(&[]),
                    cwd,
                    &mut pending_line,
                    &mut pending_col,
                    parsed.new_window,
                    true,
                    &mut parsed,
                );
                i += 1;
                continue;
            }
            if arg.starts_with('-') {
                i += 1;
                continue;
            }
        }

        let consumed_extra = push_target_token(
            arg,
            args.get(i + 1..).unwrap_or(&[]),
            cwd,
            &mut pending_line,
            &mut pending_col,
            parsed.new_window,
            false,
            &mut parsed,
        );
        i += 1 + consumed_extra;
    }

    // Apply trailing `--line` / `--column` if specified after the filename (e.g. `file.vue --line 12`).
    if (pending_line.is_some() || pending_col.is_some()) && !parsed.targets.is_empty() {
        if let Some(ExternalOpenTarget::OpenFile { line, column, .. }) = parsed.targets.last_mut()
        {
            if line.is_none() {
                *line = pending_line;
            }
            if column.is_none() {
                *column = pending_col;
            }
        }
    }

    parsed
}

#[allow(clippy::too_many_arguments)]
fn push_target_token(
    token: &str,
    rest_args: &[String],
    cwd: Option<&Path>,
    pending_line: &mut Option<usize>,
    pending_col: &mut Option<usize>,
    new_window: bool,
    force_file: bool,
    parsed: &mut ParsedCliArgs,
) -> usize {
    if let Some(mut uri_target) = parse_external_uri(token) {
        if let ExternalOpenTarget::OpenFile {
            ref mut path,
            ref mut line,
            ref mut column,
            cwd: ref mut target_cwd,
            new_window: ref mut nw,
        } = uri_target
        {
            *path = resolve_path_against_cwd(path.clone(), cwd);
            if line.is_none() {
                *line = pending_line.take();
            } else {
                *pending_line = None;
            }
            if column.is_none() {
                *column = pending_col.take();
            } else {
                *pending_col = None;
            }
            *target_cwd = cwd.map(Path::to_path_buf);
            *nw |= new_window;
        }
        parsed.targets.push(uri_target);
        return 0;
    }

    let (file_part, emb_line, emb_col) = split_path_and_position(token);
    if file_part.is_empty() {
        return 0;
    }
    let resolved = resolve_path_against_cwd(PathBuf::from(&file_part), cwd);
    let has_pos = emb_line.is_some() || pending_line.is_some() || pending_col.is_some();

    if !has_pos && !force_file && resolved.is_dir() {
        if parsed.explicit_workspace.is_none() {
            parsed.explicit_workspace = Some(resolved.clone());
        }
        parsed.targets.push(ExternalOpenTarget::OpenWorkspace {
            path: resolved,
            new_window,
        });
        return 0;
    }

    let is_existing_file = resolved.is_file();
    if !is_existing_file && !force_file && !has_pos && !looks_like_file_arg(&file_part) {
        return 0;
    }

    let mut consumed = 0usize;
    let mut line = emb_line.or_else(|| pending_line.take());
    let mut column = emb_col.or_else(|| pending_col.take());

    if emb_line.is_none() && line.is_none() && is_existing_file {
        if let Some(next_line) = rest_args
            .first()
            .filter(|s| !Path::new(s.as_str()).exists() && s.chars().all(|c| c.is_ascii_digit()))
            .and_then(|s| s.parse::<usize>().ok())
        {
            line = Some(next_line.max(1));
            consumed = 1;
            if let Some(next_col) = rest_args
                .get(1)
                .filter(|s| {
                    !Path::new(s.as_str()).exists() && s.chars().all(|c| c.is_ascii_digit())
                })
                .and_then(|s| s.parse::<usize>().ok())
            {
                column = (next_col > 0).then_some(next_col);
                consumed = 2;
            }
        }
    }

    if is_existing_file || force_file || has_pos {
        parsed.targets.push(ExternalOpenTarget::OpenFile {
            path: resolved,
            line,
            column,
            cwd: cwd.map(Path::to_path_buf),
            new_window,
        });
    }
    consumed
}

pub fn path_is_inside_workspace(file: &Path, workspace: &Path) -> bool {
    let f = file
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_string();
    let w = workspace
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_string();
    if w.is_empty() {
        return false;
    }
    #[cfg(windows)]
    let (f_cmp, w_cmp) = (f.to_ascii_lowercase(), w.to_ascii_lowercase());
    #[cfg(not(windows))]
    let (f_cmp, w_cmp) = (f, w);

    f_cmp == w_cmp || (f_cmp.starts_with(&w_cmp) && f_cmp.as_bytes().get(w_cmp.len()) == Some(&b'\\'))
}

/// Finds the workspace root for a file during cold start, checking known workspaces first
/// and then walking up for project markers (`package.json`, `Cargo.toml`, `.git`, etc.).
pub fn infer_workspace_for_file(file: &Path, known_workspaces: &[PathBuf]) -> Option<PathBuf> {
    let abs = if file.is_absolute() {
        file.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(file)
    };
    let mut best_known: Option<PathBuf> = None;
    for ws in known_workspaces {
        if ws.is_dir() && path_is_inside_workspace(&abs, ws) {
            let better = best_known
                .as_ref()
                .is_none_or(|prev| ws.as_os_str().len() > prev.as_os_str().len());
            if better {
                best_known = Some(ws.clone());
            }
        }
    }
    if best_known.is_some() {
        return best_known;
    }

    let start_dir = if abs.is_dir() {
        abs.as_path()
    } else {
        abs.parent()?
    };
    const MARKERS: &[&str] = &[
        "package.json",
        "Cargo.toml",
        "pyproject.toml",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "go.mod",
        ".git",
        "vite.config.ts",
        "vite.config.js",
        "tsconfig.json",
    ];
    for ancestor in start_dir.ancestors() {
        if MARKERS.iter().any(|m| ancestor.join(m).exists()) {
            return Some(ancestor.to_path_buf());
        }
    }
    Some(start_dir.to_path_buf())
}

#[cfg(target_os = "windows")]
fn to_windows_short_path(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetShortPathNameW(
            lpsz_short_path_src: *const u16,
            lpsz_short_path_dst: *mut u16,
            cch_buffer: u32,
        ) -> u32;
    }
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    unsafe {
        let needed = GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0);
        if needed == 0 {
            return None;
        }
        let mut buf = vec![0u16; needed as usize];
        let written = GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), needed);
        if written == 0 || written >= needed {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..written as usize]))
    }
}

/// Ensures `editor-shim/code.cmd` (or `code` on Unix) and `bin/rust-ai-ide.cmd` exist and
/// point to the currently running IDE executable.
pub fn ensure_editor_shims(storage_dir: &Path) -> Option<EditorIntegrationPaths> {
    let exe_path = std::env::current_exe().ok()?;
    let shim_dir = storage_dir.join("editor-shim");
    let bin_dir = storage_dir.join("bin");
    fs::create_dir_all(&shim_dir).ok()?;
    fs::create_dir_all(&bin_dir).ok()?;

    #[cfg(target_os = "windows")]
    let launch_editor_value = {
        let code_shim = shim_dir.join("code.cmd");
        let cli_cmd = bin_dir.join("rust-ai-ide.cmd");
        let exe_str = exe_path.to_string_lossy();
        let script = format!("@echo off\r\n\"{exe_str}\" %*\r\n");
        let _ = write_if_changed(&code_shim, &script);
        let _ = write_if_changed(&cli_cmd, &script);
        if code_shim.to_string_lossy().contains(' ') {
            to_windows_short_path(&code_shim)
                .unwrap_or_else(|| code_shim.to_string_lossy().into_owned())
        } else {
            code_shim.to_string_lossy().into_owned()
        }
    };

    #[cfg(not(target_os = "windows"))]
    let launch_editor_value = {
        use std::os::unix::fs::PermissionsExt;
        let code_shim = shim_dir.join("code");
        let cli_cmd = bin_dir.join("rust-ai-ide");
        let exe_str = exe_path.to_string_lossy().replace('\'', "'\"'\"'");
        let script = format!("#!/bin/sh\nexec '{exe_str}' \"$@\"\n");
        let _ = write_if_changed(&code_shim, &script);
        let _ = write_if_changed(&cli_cmd, &script);
        if let Ok(meta) = fs::metadata(&code_shim) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(&code_shim, perms);
        }
        if let Ok(meta) = fs::metadata(&cli_cmd) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(&cli_cmd, perms);
        }
        code_shim.to_string_lossy().into_owned()
    };

    Some(EditorIntegrationPaths {
        exe_path,
        cli_bin_dir: bin_dir,
        launch_editor_value,
    })
}

fn write_if_changed(path: &Path, content: &str) -> std::io::Result<()> {
    if let Ok(existing) = fs::read_to_string(path) {
        if existing == content {
            return Ok(());
        }
    }
    fs::write(path, content)
}

/// Environment variables injected into IDE terminals & RunConfigurations so
/// `vite-plugin-vue-inspector`, `launch-editor`, `launch-ide`, React/Next/Nuxt/Webpack/Vite
/// automatically jump back into this IDE at the exact file, line, and column.
pub fn terminal_launch_env(paths: &EditorIntegrationPaths) -> Vec<(String, String)> {
    vec![
        (
            "LAUNCH_EDITOR".to_string(),
            paths.launch_editor_value.clone(),
        ),
        (
            "REACT_EDITOR".to_string(),
            paths.launch_editor_value.clone(),
        ),
        (
            "VUE_EDITOR".to_string(),
            paths.launch_editor_value.clone(),
        ),
        (
            "RUST_AI_IDE_BIN".to_string(),
            paths.exe_path.to_string_lossy().into_owned(),
        ),
    ]
}

#[cfg(target_os = "windows")]
mod win_reg {
    use anyhow::{anyhow, Result};
    use core::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    const HKEY_CURRENT_USER: isize = -0x7FFF_FFFF; // 0x80000001
    const KEY_READ: u32 = 0x20019;
    const KEY_WRITE: u32 = 0x20006;
    const REG_SZ: u32 = 1;
    const REG_EXPAND_SZ: u32 = 2;
    const HWND_BROADCAST: isize = 0xFFFF;
    const WM_SETTINGCHANGE: u32 = 0x001A;
    const SMTO_ABORTIFHUNG: u32 = 0x0002;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegCreateKeyExW(
            h_key: isize,
            lp_sub_key: *const u16,
            reserved: u32,
            lp_class: *const u16,
            dw_options: u32,
            sam_desired: u32,
            lp_security_attributes: *const c_void,
            phk_result: *mut isize,
            lpdw_disposition: *mut u32,
        ) -> i32;
        fn RegOpenKeyExW(
            h_key: isize,
            lp_sub_key: *const u16,
            ul_options: u32,
            sam_desired: u32,
            phk_result: *mut isize,
        ) -> i32;
        fn RegSetValueExW(
            h_key: isize,
            lp_value_name: *const u16,
            reserved: u32,
            dw_type: u32,
            lp_data: *const u8,
            cb_data: u32,
        ) -> i32;
        fn RegQueryValueExW(
            h_key: isize,
            lp_value_name: *const u16,
            lp_reserved: *mut u32,
            lp_type: *mut u32,
            lp_data: *mut u8,
            lpcb_data: *mut u32,
        ) -> i32;
        fn RegDeleteValueW(h_key: isize, lp_value_name: *const u16) -> i32;
        fn RegCloseKey(h_key: isize) -> i32;
    }

    #[link(name = "user32")]
    extern "system" {
        fn SendMessageTimeoutW(
            hwnd: isize,
            msg: u32,
            wparam: usize,
            lparam: isize,
            fu_flags: u32,
            u_timeout: u32,
            lpdw_result: *mut usize,
        ) -> isize;
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn reg_set_sz(subkey: &str, name: Option<&str>, value: &str, reg_type: u32) -> Result<()> {
        let sk = to_wide(subkey);
        let mut hkey: isize = 0;
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                sk.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_WRITE,
                std::ptr::null(),
                &mut hkey,
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            return Err(anyhow!("RegCreateKeyExW({subkey}) failed: {rc}"));
        }
        let vn = name.map(to_wide);
        let val_w = to_wide(value);
        let rc2 = unsafe {
            RegSetValueExW(
                hkey,
                vn.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
                0,
                reg_type,
                val_w.as_ptr() as *const u8,
                (val_w.len() * 2) as u32,
            )
        };
        unsafe {
            RegCloseKey(hkey);
        }
        if rc2 != 0 {
            return Err(anyhow!("RegSetValueExW({subkey}) failed: {rc2}"));
        }
        Ok(())
    }

    fn reg_read_sz(subkey: &str, name: Option<&str>) -> Option<(String, u32)> {
        let sk = to_wide(subkey);
        let mut hkey: isize = 0;
        let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sk.as_ptr(), 0, KEY_READ, &mut hkey) };
        if rc != 0 {
            return None;
        }
        let vn = name.map(to_wide);
        let mut val_type: u32 = 0;
        let mut bytes_len: u32 = 0;
        let rc_len = unsafe {
            RegQueryValueExW(
                hkey,
                vn.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
                std::ptr::null_mut(),
                &mut val_type,
                std::ptr::null_mut(),
                &mut bytes_len,
            )
        };
        if rc_len != 0 || bytes_len < 2 {
            unsafe { RegCloseKey(hkey) };
            return None;
        }
        let mut buf = vec![0u16; (bytes_len as usize + 1) / 2];
        let mut actual = (buf.len() * 2) as u32;
        let rc_val = unsafe {
            RegQueryValueExW(
                hkey,
                vn.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
                std::ptr::null_mut(),
                &mut val_type,
                buf.as_mut_ptr() as *mut u8,
                &mut actual,
            )
        };
        unsafe { RegCloseKey(hkey) };
        if rc_val != 0 {
            return None;
        }
        let used = (actual as usize / 2).min(buf.len());
        let trimmed = &buf[..used];
        let nul_pos = trimmed.iter().position(|&c| c == 0).unwrap_or(trimmed.len());
        Some((String::from_utf16_lossy(&trimmed[..nul_pos]), val_type))
    }

    fn reg_delete_value(subkey: &str, name: &str) {
        let sk = to_wide(subkey);
        let mut hkey: isize = 0;
        let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sk.as_ptr(), 0, KEY_WRITE, &mut hkey) };
        if rc == 0 {
            let vn = to_wide(name);
            unsafe {
                let _ = RegDeleteValueW(hkey, vn.as_ptr());
                RegCloseKey(hkey);
            }
        }
    }

    fn broadcast_env_change() {
        let env_w = to_wide("Environment");
        let mut res: usize = 0;
        unsafe {
            let _ = SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                env_w.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                250,
                &mut res,
            );
        }
    }

    pub fn ensure_url_protocol(exe_path: &Path) -> Result<()> {
        let cmd = format!("\"{}\" \"%1\"", exe_path.display());
        let icon = format!("\"{}\",0", exe_path.display());
        reg_set_sz(
            r"Software\Classes\rust-ai-ide",
            None,
            "URL:Rust AI IDE Protocol",
            REG_SZ,
        )?;
        reg_set_sz(r"Software\Classes\rust-ai-ide", Some("URL Protocol"), "", REG_SZ)?;
        reg_set_sz(r"Software\Classes\rust-ai-ide\DefaultIcon", None, &icon, REG_SZ)?;
        reg_set_sz(
            r"Software\Classes\rust-ai-ide\shell\open\command",
            None,
            &cmd,
            REG_SZ,
        )?;
        Ok(())
    }

    pub fn is_url_protocol_registered(exe_path: &Path) -> bool {
        let expected = format!("\"{}\" \"%1\"", exe_path.display());
        reg_read_sz(r"Software\Classes\rust-ai-ide\shell\open\command", None)
            .is_some_and(|(v, _)| v.eq_ignore_ascii_case(&expected))
    }

    pub fn is_user_launch_editor_enabled(expected_val: &str) -> bool {
        reg_read_sz("Environment", Some("LAUNCH_EDITOR"))
            .is_some_and(|(v, _)| v.eq_ignore_ascii_case(expected_val))
    }

    pub fn set_user_launch_editor_enabled(shim_val: &str, enable: bool) -> Result<()> {
        if enable {
            reg_set_sz("Environment", Some("LAUNCH_EDITOR"), shim_val, REG_SZ)?;
            reg_set_sz("Environment", Some("REACT_EDITOR"), shim_val, REG_SZ)?;
        } else {
            reg_delete_value("Environment", "LAUNCH_EDITOR");
            reg_delete_value("Environment", "REACT_EDITOR");
        }
        broadcast_env_change();
        Ok(())
    }

    pub fn is_cli_dir_in_user_path(bin_dir: &Path) -> bool {
        let target = bin_dir
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_string();
        let Some((current, _)) = reg_read_sz("Environment", Some("Path")) else {
            return false;
        };
        current.split(';').any(|entry| {
            entry
                .trim()
                .trim_end_matches(['\\', '/'])
                .eq_ignore_ascii_case(&target)
        })
    }

    pub fn add_cli_dir_to_user_path(bin_dir: &Path) -> Result<()> {
        if is_cli_dir_in_user_path(bin_dir) {
            return Ok(());
        }
        let target = bin_dir.to_string_lossy().into_owned();
        let (existing, val_type) =
            reg_read_sz("Environment", Some("Path")).unwrap_or((String::new(), REG_EXPAND_SZ));
        let mut parts: Vec<String> = existing
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        parts.push(target);
        let joined = parts.join(";");
        let ty = if val_type == REG_SZ {
            REG_SZ
        } else {
            REG_EXPAND_SZ
        };
        reg_set_sz("Environment", Some("Path"), &joined, ty)?;
        broadcast_env_change();
        let _ = Path::new("").as_os_str().encode_wide();
        Ok(())
    }
}

pub fn ensure_windows_url_protocol(exe_path: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        win_reg::ensure_url_protocol(exe_path)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = exe_path;
        Ok(())
    }
}

pub fn is_windows_url_protocol_registered(exe_path: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        win_reg::is_url_protocol_registered(exe_path)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = exe_path;
        false
    }
}

pub fn is_user_launch_editor_enabled(shim_val: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        win_reg::is_user_launch_editor_enabled(shim_val)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = shim_val;
        false
    }
}

pub fn set_user_launch_editor_enabled(shim_val: &str, enable: bool) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        win_reg::set_user_launch_editor_enabled(shim_val, enable)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (shim_val, enable);
        Ok(())
    }
}

pub fn is_cli_dir_in_user_path(bin_dir: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        win_reg::is_cli_dir_in_user_path(bin_dir)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = bin_dir;
        false
    }
}

pub fn add_cli_dir_to_user_path(bin_dir: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        win_reg::add_cli_dir_to_user_path(bin_dir)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = bin_dir;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_windows_and_unix_positions() {
        assert_eq!(
            split_path_and_position(r"D:\work\vue-app\src\App.vue:42:15"),
            (r"D:\work\vue-app\src\App.vue".to_string(), Some(42), Some(15))
        );
        assert_eq!(
            split_path_and_position(r"D:\work\vue-app\src\App.vue:42"),
            (r"D:\work\vue-app\src\App.vue".to_string(), Some(42), None)
        );
        assert_eq!(
            split_path_and_position(r"D:\work\vue-app\src\App.vue:42:0"),
            (r"D:\work\vue-app\src\App.vue".to_string(), Some(42), None)
        );
        assert_eq!(
            split_path_and_position("src/App.vue(19,8)"),
            ("src/App.vue".to_string(), Some(19), Some(8))
        );
        assert_eq!(
            split_path_and_position("src/App.vue#L33C7"),
            ("src/App.vue".to_string(), Some(33), Some(7))
        );
        assert_eq!(
            split_path_and_position(r"C:\123"),
            (r"C:\123".to_string(), None, None)
        );
    }

    #[test]
    fn parses_custom_ide_uris() {
        assert_eq!(
            parse_external_uri("rust-ai-ide://file/D:/work/app/src/App.vue:28:6"),
            Some(ExternalOpenTarget::OpenFile {
                path: PathBuf::from("D:/work/app/src/App.vue"),
                line: Some(28),
                column: Some(6),
                cwd: None,
                new_window: false,
            })
        );
        assert_eq!(
            parse_external_uri(
                "rust-ai-ide://open?file=D%3A%2Fwork%2Fapp%2Fsrc%20dir%2FApp.vue&line=14&column=3"
            ),
            Some(ExternalOpenTarget::OpenFile {
                path: PathBuf::from("D:/work/app/src dir/App.vue"),
                line: Some(14),
                column: Some(3),
                cwd: None,
                new_window: false,
            })
        );
        assert_eq!(
            parse_external_uri("vscode://file/D:/work/app/main.ts:9:2?windowId=_blank"),
            Some(ExternalOpenTarget::OpenFile {
                path: PathBuf::from("D:/work/app/main.ts"),
                line: Some(9),
                column: Some(2),
                cwd: None,
                new_window: true,
            })
        );
        assert_eq!(
            parse_external_uri("idea://open?file=D:/work/app/App.java&line=77"),
            Some(ExternalOpenTarget::OpenFile {
                path: PathBuf::from("D:/work/app/App.java"),
                line: Some(77),
                column: None,
                cwd: None,
                new_window: false,
            })
        );
        assert_eq!(
            parse_external_uri("file:///D:/work/app/src/App.vue#L18C4"),
            Some(ExternalOpenTarget::OpenFile {
                path: PathBuf::from("D:/work/app/src/App.vue"),
                line: Some(18),
                column: Some(4),
                cwd: None,
                new_window: false,
            })
        );
    }

    #[test]
    fn parses_all_cli_flag_styles() {
        let dir = std::env::temp_dir().join("rust_ai_ide_ext_open_test");
        let _ = fs::create_dir_all(&dir);
        let file = dir.join("Comp.vue");
        fs::write(&file, "<template><div/></template>\n").unwrap();

        // 1. VS Code `-g file:line:col` (used by launch-editor / vite-plugin-vue-inspector)
        let args_g = vec![
            "ide-app.exe".into(),
            "-g".into(),
            format!("{}:15:7", file.display()),
        ];
        let p1 = parse_cli_args(&args_g, None);
        assert_eq!(
            p1.targets,
            vec![ExternalOpenTarget::OpenFile {
                path: file.clone(),
                line: Some(15),
                column: Some(7),
                cwd: None,
                new_window: false,
            }]
        );

        // 2. JetBrains IDEA `--line N --column M file`
        let args_idea = vec![
            "ide-app.exe".into(),
            "--line".into(),
            "22".into(),
            "--column".into(),
            "9".into(),
            file.display().to_string(),
        ];
        let p2 = parse_cli_args(&args_idea, None);
        assert_eq!(
            p2.targets,
            vec![ExternalOpenTarget::OpenFile {
                path: file.clone(),
                line: Some(22),
                column: Some(9),
                cwd: None,
                new_window: false,
            }]
        );

        // 3. Vim `+line:col file` with relative cwd
        let args_vim = vec!["ide-app.exe".into(), "+31:4".into(), "Comp.vue".into()];
        let p3 = parse_cli_args(&args_vim, Some(&dir));
        assert_eq!(
            p3.targets,
            vec![ExternalOpenTarget::OpenFile {
                path: file.clone(),
                line: Some(31),
                column: Some(4),
                cwd: Some(dir.clone()),
                new_window: false,
            }]
        );

        // 4. Legacy space-separated `file line col`
        let args_legacy = vec![
            "ide-app.exe".into(),
            file.display().to_string(),
            "12".into(),
            "4".into(),
        ];
        let p4 = parse_cli_args(&args_legacy, None);
        assert_eq!(
            p4.targets,
            vec![ExternalOpenTarget::OpenFile {
                path: file.clone(),
                line: Some(12),
                column: Some(4),
                cwd: None,
                new_window: false,
            }]
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ipc_roundtrip_delivers_targets() {
        let dir = std::env::temp_dir().join(format!(
            "rust_ai_ide_ipc_test_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let _guard = start_ipc_server(Some(&dir), move |targets| {
            let _ = tx.send(targets);
        })
        .expect("ipc server");

        let sent = vec![ExternalOpenTarget::OpenFile {
            path: PathBuf::from(r"D:\demo\src\App.vue"),
            line: Some(42),
            column: Some(9),
            cwd: None,
            new_window: false,
        }];
        assert!(try_forward_via_ipc(Some(&dir), &sent));
        let received = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ipc payload");
        assert_eq!(received, sent);

        drop(_guard);
        let _ = fs::remove_dir_all(&dir);
    }
}
