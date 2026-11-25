use ide_core::RunConfiguration;
use parking_lot::Mutex;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyledChar {
    pub ch: char,
    pub fg: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StyledTerminalLine {
    pub spans: Vec<(String, u32)>,
}

impl StyledTerminalLine {
    pub fn plain(text: impl Into<String>, color: u32) -> Self {
        Self {
            spans: vec![(text.into(), color)],
        }
    }

    pub fn raw_text(&self) -> String {
        self.spans.iter().map(|(s, _)| s.as_str()).collect()
    }
}

pub type PtyWakeupCallback = Arc<dyn Fn() + Send + Sync>;

struct PtyTabBackend {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    shell_pid: Option<u32>,
}

impl Drop for PtyTabBackend {
    fn drop(&mut self) {
        if let Some(pid) = self.shell_pid {
            kill_descendant_processes(pid);
        }
        let _ = self.child.kill();
    }
}

#[derive(Debug, Clone)]
pub struct TerminalSnapshot {
    pub lines: Vec<StyledTerminalLine>,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub cursor_visible: bool,
    pub alt_screen_active: bool,
}

pub struct LivePtyTab {
    pub title: String,
    pub cwd: PathBuf,
    pub grid: Arc<Mutex<TerminalGrid>>,
    pub revision: Arc<AtomicU64>,
    pub alive: Arc<AtomicBool>,
    pub current_input: String,
    pub history: Vec<String>,
    pub history_cursor: Option<usize>,
    pub scroll_offset: usize,
    pub sel_anchor: Option<(usize, usize)>,
    pub sel_cursor: Option<(usize, usize)>,
    pub bound_run_key: Option<String>,
    pub run_active: Arc<AtomicBool>,
    pub run_seq: Arc<AtomicU64>,
    on_output: Option<PtyWakeupCallback>,
    spawn_attempted: bool,
    pending_rows: u16,
    pending_cols: u16,
    backend: Option<Arc<Mutex<PtyTabBackend>>>,
}

impl LivePtyTab {
    /// Creates a terminal tab lazily without spawning `powershell.exe` / ConPTY until the user
    /// interacts with the terminal or runs a command (saves ~40-60MB per inactive window).
    pub fn new_lazy(
        title: impl Into<String>,
        cwd: &Path,
        on_output: Option<PtyWakeupCallback>,
    ) -> Self {
        Self::new_lazy_bound(title, cwd, None, on_output)
    }

    pub fn new_lazy_bound(
        title: impl Into<String>,
        cwd: &Path,
        bound_run_key: Option<String>,
        on_output: Option<PtyWakeupCallback>,
    ) -> Self {
        let title = title.into();
        let cwd_buf = cwd.to_path_buf();
        let rows = 24usize;
        let cols = 120usize;
        let mut grid = TerminalGrid::new(rows, cols);
        grid.push_fallback_line(
            &format!("PS {}> (点击终端或运行命令时启动 PowerShell)", cwd_buf.display()),
            0x818594,
        );
        Self {
            title,
            cwd: cwd_buf,
            grid: Arc::new(Mutex::new(grid)),
            revision: Arc::new(AtomicU64::new(0)),
            alive: Arc::new(AtomicBool::new(false)),
            current_input: String::new(),
            history: Vec::new(),
            history_cursor: None,
            scroll_offset: 0,
            sel_anchor: None,
            sel_cursor: None,
            bound_run_key,
            run_active: Arc::new(AtomicBool::new(false)),
            run_seq: Arc::new(AtomicU64::new(0)),
            on_output,
            spawn_attempted: false,
            pending_rows: rows as u16,
            pending_cols: cols as u16,
            backend: None,
        }
    }

    pub fn spawn_powershell(
        title: impl Into<String>,
        cwd: &Path,
        on_output: Option<PtyWakeupCallback>,
    ) -> Self {
        let mut tab = Self::new_lazy(title, cwd, on_output);
        tab.ensure_spawned();
        tab
    }

    pub fn ensure_spawned(&mut self) {
        if self.spawn_attempted || self.backend.is_some() {
            return;
        }
        self.spawn_attempted = true;
        self.grid.lock().clear_all();

        let rows = self.pending_rows.max(6);
        let cols = self.pending_cols.max(30);
        let grid = self.grid.clone();
        let revision = self.revision.clone();
        let alive = self.alive.clone();
        let on_output = self.on_output.clone();

        let pty_system = native_pty_system();
        let pair_res = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });

        let mut backend_opt = None;

        if let Ok(pair) = pair_res {
            let mut cmd = CommandBuilder::new("powershell.exe");
            cmd.args([
                "-NoLogo",
                "-NoExit",
                "-Command",
                "[Console]::InputEncoding=[System.Text.UTF8Encoding]::new($false); [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false)",
            ]);
            cmd.cwd(&self.cwd);
            cmd.env("TERM", "xterm-256color");
            cmd.env("COLORTERM", "truecolor");

            if let Ok(child) = pair.slave.spawn_command(cmd) {
                let shell_pid = child.process_id();
                drop(pair.slave);
                if let (Ok(mut reader), Ok(writer)) =
                    (pair.master.try_clone_reader(), pair.master.take_writer())
                {
                    alive.store(true, Ordering::SeqCst);
                    let shared_writer = Arc::new(Mutex::new(writer));
                    let writer_for_reader = shared_writer.clone();
                    let grid_clone = grid.clone();
                    let rev_clone = revision.clone();
                    let alive_clone = alive.clone();
                    let run_active_clone = self.run_active.clone();
                    let run_seq_clone = self.run_seq.clone();
                    let wakeup = on_output.clone();

                    thread::spawn(move || {
                        let mut buf = [0u8; 8192];
                        let mut utf8_leftover: Vec<u8> = Vec::new();
                        let mut completion_tail = String::new();
                        let mut parser = VtStreamParser::new();

                        loop {
                            match reader.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    utf8_leftover.extend_from_slice(&buf[..n]);
                                    let mut pos = 0usize;
                                    let mut replies: Vec<String> = Vec::new();
                                    while pos < utf8_leftover.len() {
                                        match std::str::from_utf8(&utf8_leftover[pos..]) {
                                            Ok(s) => {
                                                let mut g = grid_clone.lock();
                                                parser.feed_with_replies(s, &mut g, &mut replies);
                                                append_completion_tail(&mut completion_tail, s);
                                                pos = utf8_leftover.len();
                                                break;
                                            }
                                            Err(e) => {
                                                let valid = e.valid_up_to();
                                                if valid > 0 {
                                                    if let Ok(s) = std::str::from_utf8(
                                                        &utf8_leftover[pos..pos + valid],
                                                    ) {
                                                        let mut g = grid_clone.lock();
                                                        parser.feed_with_replies(
                                                            s,
                                                            &mut g,
                                                            &mut replies,
                                                        );
                                                        append_completion_tail(&mut completion_tail, s);
                                                    }
                                                    pos += valid;
                                                }
                                                if let Some(err_len) = e.error_len() {
                                                    pos += err_len.max(1);
                                                } else {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    if pos > 0 {
                                        utf8_leftover.drain(..pos);
                                    }
                                    if run_active_clone.load(Ordering::SeqCst) {
                                        let seq = run_seq_clone.load(Ordering::SeqCst);
                                        let marker = format!("[Process finished #{seq} ");
                                        if completion_tail.contains(&marker) {
                                            run_active_clone.store(false, Ordering::SeqCst);
                                            completion_tail.clear();
                                        }
                                    }
                                    if !replies.is_empty() {
                                        let mut w = writer_for_reader.lock();
                                        for r in replies {
                                            let _ = w.write_all(r.as_bytes());
                                        }
                                        let _ = w.flush();
                                    }

                                    rev_clone.fetch_add(1, Ordering::Relaxed);
                                    if let Some(cb) = &wakeup {
                                        cb();
                                    }
                                }
                            }
                        }
                        alive_clone.store(false, Ordering::SeqCst);
                        run_active_clone.store(false, Ordering::SeqCst);
                        rev_clone.fetch_add(1, Ordering::Relaxed);
                        if let Some(cb) = &wakeup {
                            cb();
                        }
                    });

                    backend_opt = Some(Arc::new(Mutex::new(PtyTabBackend {
                        writer: shared_writer,
                        master: pair.master,
                        child,
                        shell_pid,
                    })));
                }
            }
        }

        if backend_opt.is_none() {
            grid.lock().push_fallback_line(
                "Warning: Failed to attach ConPTY powershell.exe backend.",
                0xF44747,
            );
        } else {
            grid.lock().resize(rows as usize, cols as usize);
        }

        self.backend = backend_opt;
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    pub fn send_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let normalized = normalize_terminal_text(text);
        self.ensure_spawned();
        self.clear_selection();
        self.scroll_offset = 0;
        if let Some(b) = &self.backend {
            let guard = b.lock();
            let mut w = guard.writer.lock();
            let _ = w.write_all(normalized.as_bytes());
            let _ = w.flush();
        }
    }

    /// Pastes clipboard text into the PTY, normalizing newlines to `\r` (avoiding ConPTY/PSReadLine
    /// treating `\n` as `Ctrl+Enter` / `InsertLineAbove` which reverses multi-line pastes) and
    /// wrapping with bracketed-paste markers (`\x1b[200~...\x1b[201~`) when mode 2004 is active.
    pub fn paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.ensure_spawned();
        self.clear_selection();
        self.scroll_offset = 0;
        let bracketed = self.grid.lock().bracketed_paste;
        let payload = format_terminal_paste_payload(text, bracketed);
        if let Some(b) = &self.backend {
            let guard = b.lock();
            let mut w = guard.writer.lock();
            let _ = w.write_all(payload.as_bytes());
            let _ = w.flush();
        }
    }

    pub fn send_command(&mut self, cmd_line: &str) {
        let trimmed = cmd_line.trim();
        if !trimmed.is_empty() {
            if self.history.last().map(|s| s.as_str()) != Some(trimmed) {
                self.history.push(trimmed.to_string());
            }
        }
        self.history_cursor = None;
        self.clear_selection();
        self.ensure_spawned();

        if trimmed.eq_ignore_ascii_case("cls") || trimmed.eq_ignore_ascii_case("clear") {
            self.clear();
            if let Some(b) = &self.backend {
                let guard = b.lock();
                let mut w = guard.writer.lock();
                let _ = w.write_all(b"cls\r");
                let _ = w.flush();
            }
            return;
        }

        if let Some(b) = &self.backend {
            let guard = b.lock();
            let mut w = guard.writer.lock();
            let _ = w.write_all(format!("{cmd_line}\r").as_bytes());
            let _ = w.flush();
        } else {
            self.grid.lock().push_fallback_line(
                &format!("PS {}> {trimmed}", self.cwd.display()),
                0xDFE1E5,
            );
        }
        self.scroll_offset = 0;
    }

    pub fn send_interrupt(&mut self) {
        self.scroll_offset = 0;
        self.ensure_spawned();
        if let Some(b) = &self.backend {
            let guard = b.lock();
            let mut w = guard.writer.lock();
            let _ = w.write_all(b"\x03");
            let _ = w.flush();
        }
    }

    pub fn send_raw_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.scroll_offset = 0;
        self.ensure_spawned();
        if let Some(b) = &self.backend {
            let guard = b.lock();
            let mut w = guard.writer.lock();
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    /// Reads the command line currently sitting at the shell prompt (e.g. `PS D:\proj> npm run dev`)
    /// right before submitting Enter, returning `(working_dir, command_line)`.
    pub fn record_prompt_command(&mut self) -> Option<(PathBuf, String)> {
        let (parsed_cwd, cmd) = self.grid.lock().extract_current_prompt_command()?;
        let trimmed = cmd.trim().to_string();
        if trimmed.is_empty() {
            return None;
        }
        if self.history.last().map(|s| s.as_str()) != Some(trimmed.as_str()) {
            self.history.push(trimmed.clone());
        }
        let cwd = parsed_cwd
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| self.cwd.clone());
        Some((cwd, trimmed))
    }

    pub fn resize_pty(&mut self, rows: u16, cols: u16) {
        let r = rows.max(6);
        let c = cols.max(30);
        self.pending_rows = r;
        self.pending_cols = c;
        self.grid.lock().resize(r as usize, c as usize);
        if let Some(b) = &self.backend {
            let guard = b.lock();
            let _ = guard.master.resize(PtySize {
                rows: r,
                cols: c,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    pub fn history_prev(&mut self, current_buf: &mut String) {
        if self.history.is_empty() {
            return;
        }
        let next_idx = match self.history_cursor {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_cursor = Some(next_idx);
        *current_buf = self.history[next_idx].clone();
    }

    pub fn history_next(&mut self, current_buf: &mut String) {
        let Some(idx) = self.history_cursor else {
            return;
        };
        if idx + 1 >= self.history.len() {
            self.history_cursor = None;
            current_buf.clear();
        } else {
            let next_idx = idx + 1;
            self.history_cursor = Some(next_idx);
            *current_buf = self.history[next_idx].clone();
        }
    }

    pub fn clear(&mut self) {
        self.grid.lock().clear_all();
        self.scroll_offset = 0;
        self.clear_selection();
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    pub fn clear_selection(&mut self) {
        self.sel_anchor = None;
        self.sel_cursor = None;
    }

    pub fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.sel_anchor?;
        let b = self.sel_cursor?;
        if a == b {
            None
        } else if a < b {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }

    pub fn select_all(&mut self) {
        let snap = self.snapshot();
        if snap.lines.is_empty() {
            return;
        }
        let last_line = snap.lines.len() - 1;
        let last_col = snap.lines[last_line].raw_text().chars().count();
        self.sel_anchor = Some((0, 0));
        self.sel_cursor = Some((last_line, last_col));
    }

    pub fn select_word_at(&mut self, line: usize, col: usize) {
        let snap = self.snapshot();
        if line >= snap.lines.len() {
            return;
        }
        let chars: Vec<char> = snap.lines[line].raw_text().chars().collect();
        if chars.is_empty() {
            return;
        }
        let idx = col.min(chars.len() - 1);
        let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\' | ':');
        let target = is_word(chars[idx]);
        let mut s = idx;
        while s > 0 && is_word(chars[s - 1]) == target && (target || chars[s - 1].is_whitespace() == chars[idx].is_whitespace()) {
            s -= 1;
        }
        let mut e = idx + 1;
        while e < chars.len() && is_word(chars[e]) == target && (target || chars[e].is_whitespace() == chars[idx].is_whitespace()) {
            e += 1;
        }
        self.sel_anchor = Some((line, s));
        self.sel_cursor = Some((line, e));
    }

    pub fn select_line(&mut self, line: usize) {
        let snap = self.snapshot();
        if line >= snap.lines.len() {
            return;
        }
        let len = snap.lines[line].raw_text().chars().count();
        self.sel_anchor = Some((line, 0));
        self.sel_cursor = Some((line, len));
    }

    pub fn selected_text(&self) -> Option<String> {
        let ((sl, sc), (el, ec)) = self.selection_range()?;
        let snap = self.snapshot();
        if snap.lines.is_empty() {
            return None;
        }
        let sl = sl.min(snap.lines.len() - 1);
        let el = el.min(snap.lines.len() - 1);
        if sl == el {
            let raw: Vec<char> = snap.lines[sl].raw_text().chars().collect();
            let s = sc.min(raw.len());
            let e = ec.min(raw.len());
            let txt: String = raw[s..e].iter().collect();
            return if txt.is_empty() { None } else { Some(txt) };
        }
        let mut out = String::new();
        for l in sl..=el {
            let raw = snap.lines[l].raw_text();
            let chars: Vec<char> = raw.chars().collect();
            if l == sl {
                let s = sc.min(chars.len());
                out.push_str(&chars[s..].iter().collect::<String>());
                out.push('\n');
            } else if l == el {
                let e = ec.min(chars.len());
                out.push_str(&chars[..e].iter().collect::<String>());
            } else {
                out.push_str(raw.trim_end_matches(' '));
                out.push('\n');
            }
        }
        Some(out)
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        self.grid.lock().snapshot()
    }

    pub fn snapshot_lines(&self) -> Vec<StyledTerminalLine> {
        self.snapshot().lines
    }

    pub fn current_revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    pub fn is_run_active(&self) -> bool {
        self.run_active.load(Ordering::SeqCst)
    }

    /// Starts a RunConfiguration command inside this bound tab, wrapping it with a split completion
    /// marker so only actual completion (not stdin echo) resets `run_active`.
    pub fn start_bound_run(&mut self, cfg: &RunConfiguration) {
        self.ensure_spawned();
        let seq = self.run_seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.run_active.store(true, Ordering::SeqCst);

        let mut parts = Vec::with_capacity(1 + cfg.args.len());
        parts.push(quote_ps_arg_if_needed(&cfg.command));
        for arg in &cfg.args {
            parts.push(quote_ps_arg_if_needed(arg));
        }
        let full_cmd = parts.join(" ");
        let body = if !cfg.cwd.as_os_str().is_empty() && cfg.cwd != self.cwd {
            let escaped_cwd = cfg.cwd.display().to_string().replace('\'', "''");
            format!("Set-Location -LiteralPath '{escaped_cwd}'; {full_cmd}")
        } else {
            full_cmd
        };

        let wrapped = format!(
            "try {{ {body} }} finally {{ Write-Host ('[' + 'Process ' + 'finished #{seq} (exit ' + $(if ($null -eq $LASTEXITCODE) {{ if ($?) {{ 0 }} else {{ 1 }} }} else {{ $LASTEXITCODE }}) + ')') }}"
        );
        self.send_command(&wrapped);
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    /// Stops only this tab's active script process tree while keeping all other run tabs alive.
    pub fn stop_active_run(&mut self) -> bool {
        let was_active = self.run_active.swap(false, Ordering::SeqCst);
        self.run_seq.fetch_add(1, Ordering::SeqCst);
        if let Some(b) = &self.backend {
            let pid = {
                let guard = b.lock();
                {
                    let mut w = guard.writer.lock();
                    let _ = w.write_all(b"\x03");
                    let _ = w.flush();
                }
                guard.shell_pid
            };
            if let Some(root_pid) = pid {
                kill_descendant_processes(root_pid);
            }
        }
        if was_active {
            self.grid
                .lock()
                .push_fallback_line("[Process stopped]", 0xE5A93B);
            self.revision.fetch_add(1, Ordering::Relaxed);
        }
        was_active
    }
}

pub struct TerminalSession {
    pub tabs: Vec<LivePtyTab>,
    pub active_tab: usize,
    pub cwd: PathBuf,
    pub input_buffer: String,
    on_output: Option<PtyWakeupCallback>,
}

impl TerminalSession {
    pub fn new(title: impl Into<String>, cwd: &Path) -> Self {
        Self::new_with_wakeup(title, cwd, None)
    }

    pub fn new_with_wakeup(
        title: impl Into<String>,
        cwd: &Path,
        on_output: Option<PtyWakeupCallback>,
    ) -> Self {
        let first_tab = LivePtyTab::new_lazy(title, cwd, on_output.clone());
        Self {
            tabs: vec![first_tab],
            active_tab: 0,
            cwd: cwd.to_path_buf(),
            input_buffer: String::new(),
            on_output,
        }
    }

    pub fn add_new_tab(&mut self) {
        let idx = self.tabs.len() + 1;
        let tab = LivePtyTab::spawn_powershell(
            format!("PowerShell ({idx})"),
            &self.cwd,
            self.on_output.clone(),
        );
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
    }

    pub fn close_tab(&mut self, idx: usize) {
        if self.tabs.len() <= 1 || idx >= self.tabs.len() {
            return;
        }
        self.tabs[idx].stop_active_run();
        self.tabs.remove(idx);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
    }

    pub fn active_pty(&self) -> &LivePtyTab {
        &self.tabs[self.active_tab.min(self.tabs.len().saturating_sub(1))]
    }

    pub fn active_pty_mut(&mut self) -> &mut LivePtyTab {
        let idx = self.active_tab.min(self.tabs.len().saturating_sub(1));
        &mut self.tabs[idx]
    }

    pub fn total_revision(&self) -> u64 {
        self.tabs
            .iter()
            .fold(0u64, |acc, t| acc.wrapping_add(t.current_revision()))
    }

    pub fn any_run_active(&self) -> bool {
        self.tabs.iter().any(|t| t.is_run_active())
    }

    pub fn is_run_config_running(&self, run_key: &str) -> bool {
        self.tabs
            .iter()
            .any(|t| t.bound_run_key.as_deref() == Some(run_key) && t.is_run_active())
    }

    /// 启动与特定 RunConfiguration 绑定的独立终端标签页，支持指定绑定键与展示标题（例如注入 `-d <device>` 时仍追踪原配置）
    pub fn start_bound_run_configuration_as(
        &mut self,
        run_key: &str,
        title: &str,
        cfg: &RunConfiguration,
    ) {
        let tab_idx = if let Some(existing) = self
            .tabs
            .iter()
            .position(|t| t.bound_run_key.as_deref() == Some(run_key))
        {
            existing
        } else {
            let tab_title = format!("▶ {title}");
            let tab = LivePtyTab::new_lazy_bound(
                tab_title,
                &self.cwd,
                Some(run_key.to_string()),
                self.on_output.clone(),
            );
            self.tabs.push(tab);
            self.tabs.len() - 1
        };
        self.active_tab = tab_idx;
        self.tabs[tab_idx].start_bound_run(cfg);
    }

    pub fn start_bound_run_configuration(&mut self, cfg: &RunConfiguration) {
        let run_key = cfg.identity_key();
        self.start_bound_run_configuration_as(&run_key, &cfg.name, cfg);
    }

    pub fn stop_bound_run_configuration(&mut self, run_key: &str) -> bool {
        let mut stopped = false;
        for tab in &mut self.tabs {
            if tab.bound_run_key.as_deref() == Some(run_key) && tab.stop_active_run() {
                stopped = true;
            }
        }
        stopped
    }

    pub fn execute_command_line(&mut self, cmd_line: &str) {
        self.active_pty_mut().send_command(cmd_line);
    }

    pub fn execute_run_configuration(&mut self, cfg: &RunConfiguration) {
        self.start_bound_run_configuration(cfg);
    }
}

fn append_completion_tail(tail: &mut String, chunk: &str) {
    if chunk.is_empty() {
        return;
    }
    tail.push_str(chunk);
    if tail.len() > 256 {
        let cut = tail.len() - 128;
        let boundary = tail
            .char_indices()
            .find(|&(idx, _)| idx >= cut)
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        tail.drain(..boundary);
    }
}

#[cfg(windows)]
fn kill_descendant_processes(root_pid: u32) {
    use std::collections::{HashSet, VecDeque};
    use std::ffi::c_void;

    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const PROCESS_TERMINATE: u32 = 0x0001;
    const INVALID_HANDLE: *mut c_void = (-1isize) as *mut c_void;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct PROCESSENTRY32W {
        dwSize: u32,
        cntUsage: u32,
        th32ProcessID: u32,
        th32DefaultHeapID: usize,
        th32ModuleID: u32,
        cntThreads: u32,
        th32ParentProcessID: u32,
        pcPriClassBase: i32,
        dwFlags: u32,
        szExeFile: [u16; 260],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(dwFlags: u32, th32ProcessID: u32) -> *mut c_void;
        fn Process32FirstW(hSnapshot: *mut c_void, lppe: *mut PROCESSENTRY32W) -> i32;
        fn Process32NextW(hSnapshot: *mut c_void, lppe: *mut PROCESSENTRY32W) -> i32;
        fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> *mut c_void;
        fn TerminateProcess(hProcess: *mut c_void, uExitCode: u32) -> i32;
        fn CloseHandle(hObject: *mut c_void) -> i32;
    }

    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() || snap == INVALID_HANDLE {
            return;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut procs: Vec<(u32, u32, String)> = Vec::new();
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let exe_name = String::from_utf16_lossy(&entry.szExeFile[..end]).to_ascii_lowercase();
                procs.push((entry.th32ProcessID, entry.th32ParentProcessID, exe_name));
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);

        let mut descendants = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert(root_pid);
        queue.push_back(root_pid);

        while let Some(parent) = queue.pop_front() {
            for (pid, ppid, exe_name) in &procs {
                if *ppid == parent
                    && !visited.contains(pid)
                    && exe_name != "conhost.exe"
                    && exe_name != "openconsole.exe"
                {
                    visited.insert(*pid);
                    queue.push_back(*pid);
                    descendants.push(*pid);
                }
            }
        }

        // Terminate deepest children first so parent wrappers don't respawn or complain cleanly.
        for pid in descendants.into_iter().rev() {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !h.is_null() {
                let _ = TerminateProcess(h, 1);
                let _ = CloseHandle(h);
            }
        }
    }
}

#[cfg(not(windows))]
fn kill_descendant_processes(_root_pid: u32) {}

const DEFAULT_FG: u32 = 0xDFE1E5;
const MAX_SCROLLBACK: usize = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TermCell {
    ch: char,
    fg: u32,
    width: u8, // 1=normal, 2=wide CJK, 0=wide continuation
}

impl Default for TermCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: DEFAULT_FG,
            width: 1,
        }
    }
}

pub struct TerminalGrid {
    rows: usize,
    cols: usize,
    screen: Vec<Vec<TermCell>>,
    alt_backup: Option<(Vec<Vec<TermCell>>, usize, usize)>,
    scrollback: Vec<StyledTerminalLine>,
    cursor_row: usize,
    cursor_col: usize,
    saved_row: usize,
    saved_col: usize,
    scroll_top: usize,
    scroll_bottom: usize,
    cursor_visible: bool,
    pending_wrap: bool,
    bracketed_paste: bool,
    current_fg: u32,
    bold: bool,
}

impl TerminalGrid {
    pub fn new(rows: usize, cols: usize) -> Self {
        let r = rows.max(2);
        let c = cols.max(10);
        Self {
            rows: r,
            cols: c,
            screen: vec![vec![TermCell::default(); c]; r],
            alt_backup: None,
            scrollback: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
            saved_row: 0,
            saved_col: 0,
            scroll_top: 0,
            scroll_bottom: r - 1,
            cursor_visible: true,
            pending_wrap: false,
            bracketed_paste: false,
            current_fg: DEFAULT_FG,
            bold: false,
        }
    }

    pub fn resize(&mut self, new_rows: usize, new_cols: usize) {
        let r = new_rows.max(2);
        let c = new_cols.max(10);
        if r == self.rows && c == self.cols {
            return;
        }
        for row in &mut self.screen {
            row.resize(c, TermCell::default());
        }
        if r > self.rows {
            let extra = r - self.rows;
            for _ in 0..extra {
                self.screen.push(vec![TermCell::default(); c]);
            }
        } else if r < self.rows {
            // Keep bottom rows containing cursor if possible
            let drop_top = (self.cursor_row + 1).saturating_sub(r);
            for _ in 0..drop_top {
                if !self.screen.is_empty() {
                    let top = self.screen.remove(0);
                    if self.alt_backup.is_none() {
                        self.push_scrollback_row(&top);
                    }
                    self.cursor_row = self.cursor_row.saturating_sub(1);
                }
            }
            self.screen.truncate(r);
        }
        self.rows = r;
        self.cols = c;
        self.cursor_row = self.cursor_row.min(r - 1);
        self.cursor_col = self.cursor_col.min(c - 1);
        self.scroll_top = 0;
        self.scroll_bottom = r - 1;
        self.pending_wrap = false;
    }

    pub fn clear_all(&mut self) {
        self.scrollback.clear();
        for row in &mut self.screen {
            for cell in row.iter_mut() {
                *cell = TermCell::default();
            }
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.pending_wrap = false;
    }

    pub fn push_fallback_line(&mut self, text: &str, color: u32) {
        self.scrollback.push(StyledTerminalLine::plain(text, color));
    }

    fn push_scrollback_row(&mut self, row: &[TermCell]) {
        self.scrollback.push(row_to_styled_line(row, false));
        if self.scrollback.len() > MAX_SCROLLBACK {
            let excess = self.scrollback.len() - MAX_SCROLLBACK;
            self.scrollback.drain(0..excess);
        }
    }

    fn put_char(&mut self, ch: char) {
        let w = char_display_width(ch);
        if self.pending_wrap {
            self.cursor_col = 0;
            self.line_feed();
            self.pending_wrap = false;
        }
        if w == 2 && self.cursor_col + 1 >= self.cols {
            if self.cursor_col < self.cols {
                self.screen[self.cursor_row][self.cursor_col] = TermCell::default();
            }
            self.cursor_col = 0;
            self.line_feed();
        }
        let fg = self.effective_fg();
        if self.cursor_col < self.cols {
            // If overwriting right half of wide char, clear left half
            if self.screen[self.cursor_row][self.cursor_col].width == 0 && self.cursor_col > 0 {
                self.screen[self.cursor_row][self.cursor_col - 1] = TermCell::default();
            }
            self.screen[self.cursor_row][self.cursor_col] = TermCell { ch, fg, width: w };
            if w == 2 && self.cursor_col + 1 < self.cols {
                self.screen[self.cursor_row][self.cursor_col + 1] = TermCell {
                    ch: ' ',
                    fg,
                    width: 0,
                };
            }
        }
        if self.cursor_col + (w as usize) >= self.cols {
            self.cursor_col = self.cols - 1;
            self.pending_wrap = true;
        } else {
            self.cursor_col += w as usize;
        }
    }

    fn effective_fg(&self) -> u32 {
        if self.bold && self.current_fg == DEFAULT_FG {
            0xFFFFFF
        } else {
            self.current_fg
        }
    }

    fn line_feed(&mut self) {
        self.pending_wrap = false;
        if self.cursor_row == self.scroll_bottom {
            self.scroll_up_region(1);
        } else if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.pending_wrap = false;
        if self.cursor_row == self.scroll_top {
            self.scroll_down_region(1);
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
        }
    }

    fn scroll_up_region(&mut self, n: usize) {
        let top = self.scroll_top.min(self.rows - 1);
        let bot = self.scroll_bottom.min(self.rows - 1);
        if top > bot {
            return;
        }
        for _ in 0..n {
            let removed = self.screen.remove(top);
            if top == 0 && self.alt_backup.is_none() {
                self.push_scrollback_row(&removed);
            }
            self.screen.insert(bot, vec![TermCell::default(); self.cols]);
        }
    }

    fn scroll_down_region(&mut self, n: usize) {
        let top = self.scroll_top.min(self.rows - 1);
        let bot = self.scroll_bottom.min(self.rows - 1);
        if top > bot {
            return;
        }
        for _ in 0..n {
            self.screen.remove(bot);
            self.screen.insert(top, vec![TermCell::default(); self.cols]);
        }
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        // Trim trailing empty screen rows below cursor so scrollback + prompt sits cleanly
        let mut last_nonempty = self.cursor_row;
        for r in (0..self.rows).rev() {
            let has_text = self.screen[r]
                .iter()
                .any(|c| c.width > 0 && c.ch != ' ');
            if has_text || r == self.cursor_row {
                last_nonempty = r.max(self.cursor_row);
                break;
            }
        }
        let mut out = Vec::with_capacity(self.scrollback.len() + last_nonempty + 1);
        if self.alt_backup.is_none() {
            out.extend(self.scrollback.iter().cloned());
        }
        for r in 0..=last_nonempty.min(self.rows - 1) {
            let keep_cols = if r == self.cursor_row {
                Some(self.cursor_col)
            } else {
                None
            };
            out.push(row_to_styled_line_with_min_col(&self.screen[r], keep_cols));
        }
        let cursor_line = if self.alt_backup.is_none() {
            self.scrollback.len() + self.cursor_row
        } else {
            self.cursor_row
        };
        TerminalSnapshot {
            lines: out,
            cursor_line,
            cursor_col: self.cursor_col,
            cursor_visible: self.cursor_visible,
            alt_screen_active: self.alt_backup.is_some(),
        }
    }

    /// Extracts `(optional_cwd, command)` from the active shell prompt line at `cursor_row`.
    pub fn extract_current_prompt_command(&self) -> Option<(Option<PathBuf>, String)> {
        if self.alt_backup.is_some() || self.screen.is_empty() {
            return None;
        }
        let end_row = self.cursor_row.min(self.screen.len().saturating_sub(1));
        let start_row = end_row.saturating_sub(2);
        for r in (start_row..=end_row).rev() {
            let line = row_to_styled_line(&self.screen[r], false).raw_text();
            let mut combined = line.trim_end().to_string();
            for next_r in (r + 1)..=end_row {
                let seg = row_to_styled_line(&self.screen[next_r], false).raw_text();
                let seg_trim = seg.trim_end();
                if !seg_trim.is_empty() {
                    combined.push_str(seg_trim);
                }
            }
            if let Some(parsed) = parse_shell_prompt_line(&combined) {
                return Some(parsed);
            }
        }
        None
    }
}

fn quote_ps_arg_if_needed(arg: &str) -> String {
    if arg.is_empty()
        || arg
            .chars()
            .any(|c| c.is_whitespace() || c == '\'' || c == '"')
    {
        format!("'{}'", arg.replace('\'', "''"))
    } else {
        arg.to_string()
    }
}

fn parse_shell_prompt_line(line: &str) -> Option<(Option<PathBuf>, String)> {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("PS ") {
        if let Some((path_part, cmd_part)) = rest.split_once("> ") {
            let cmd = cmd_part.trim();
            if !cmd.is_empty() {
                let cwd = {
                    let p = path_part.trim();
                    if p.is_empty() {
                        None
                    } else {
                        Some(PathBuf::from(p))
                    }
                };
                return Some((cwd, cmd.to_string()));
            }
        }
    }
    // Also recognize classic cmd.exe path prompt e.g. `D:\work\proj> npm run dev`
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 4 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
        if let Some((path_part, cmd_part)) = trimmed.split_once("> ") {
            let cmd = cmd_part.trim();
            if !cmd.is_empty() {
                return Some((Some(PathBuf::from(path_part.trim())), cmd.to_string()));
            }
        }
    }
    None
}

fn row_to_styled_line(row: &[TermCell], preserve_empty: bool) -> StyledTerminalLine {
    row_to_styled_line_with_min_col(row, if preserve_empty { Some(row.len()) } else { None })
}

fn row_to_styled_line_with_min_col(
    row: &[TermCell],
    min_col: Option<usize>,
) -> StyledTerminalLine {
    let mut end = row.len();
    while end > 0 {
        let cell = row[end - 1];
        if cell.ch != ' ' && cell.width > 0 {
            break;
        }
        if cell.width == 0 && end >= 2 && row[end - 2].ch != ' ' {
            break;
        }
        end -= 1;
    }
    if let Some(mc) = min_col {
        end = end.max(mc.min(row.len()));
    }
    let mut spans: Vec<(String, u32)> = Vec::new();
    for cell in &row[..end] {
        if cell.width == 0 {
            continue;
        }
        if let Some((last_s, last_fg)) = spans.last_mut() {
            if *last_fg == cell.fg {
                last_s.push(cell.ch);
                continue;
            }
        }
        spans.push((cell.ch.to_string(), cell.fg));
    }
    StyledTerminalLine { spans }
}

fn char_display_width(ch: char) -> u8 {
    let u = ch as u32;
    if (0x1100..=0x115F).contains(&u)
        || (0x2E80..=0xA4CF).contains(&u)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE10..=0xFE19).contains(&u)
        || (0xFE30..=0xFE6F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0xFFE0..=0xFFE6).contains(&u)
        || (0x20000..=0x2FFFD).contains(&u)
        || (0x30000..=0x3FFFD).contains(&u)
    {
        2
    } else {
        1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseState {
    Normal,
    Esc,
    CharsetSkip,
    Csi,
    Osc,
    OscEsc,
}

struct VtStreamParser {
    state: ParseState,
    buf: String,
}

impl VtStreamParser {
    fn new() -> Self {
        Self {
            state: ParseState::Normal,
            buf: String::new(),
        }
    }

    fn feed(&mut self, chunk: &str, grid: &mut TerminalGrid) {
        let mut dummy = Vec::new();
        self.feed_with_replies(chunk, grid, &mut dummy);
    }

    fn feed_with_replies(
        &mut self,
        chunk: &str,
        grid: &mut TerminalGrid,
        replies: &mut Vec<String>,
    ) {
        for ch in chunk.chars() {
            match self.state {
                ParseState::Normal => match ch {
                    '\x1b' => {
                        self.state = ParseState::Esc;
                    }
                    '\r' => {
                        grid.cursor_col = 0;
                        grid.pending_wrap = false;
                    }
                    '\n' | '\x0b' | '\x0c' => {
                        grid.line_feed();
                    }
                    '\x08' => {
                        grid.pending_wrap = false;
                        grid.cursor_col = grid.cursor_col.saturating_sub(1);
                    }
                    '\t' => {
                        grid.pending_wrap = false;
                        let next_tab = ((grid.cursor_col / 4) + 1) * 4;
                        grid.cursor_col = next_tab.min(grid.cols.saturating_sub(1));
                    }
                    '\x07' => {}
                    c if !c.is_control() => {
                        grid.put_char(c);
                    }
                    _ => {}
                },
                ParseState::Esc => match ch {
                    '[' => {
                        self.buf.clear();
                        self.state = ParseState::Csi;
                    }
                    ']' => {
                        self.buf.clear();
                        self.state = ParseState::Osc;
                    }
                    '7' => {
                        grid.saved_row = grid.cursor_row;
                        grid.saved_col = grid.cursor_col;
                        self.state = ParseState::Normal;
                    }
                    '8' => {
                        grid.cursor_row = grid.saved_row.min(grid.rows - 1);
                        grid.cursor_col = grid.saved_col.min(grid.cols - 1);
                        grid.pending_wrap = false;
                        self.state = ParseState::Normal;
                    }
                    'M' => {
                        grid.reverse_index();
                        self.state = ParseState::Normal;
                    }
                    'D' => {
                        grid.line_feed();
                        self.state = ParseState::Normal;
                    }
                    'E' => {
                        grid.cursor_col = 0;
                        grid.line_feed();
                        self.state = ParseState::Normal;
                    }
                    '(' | ')' | '*' | '+' => {
                        self.state = ParseState::CharsetSkip;
                    }
                    _ => {
                        self.state = ParseState::Normal;
                    }
                },
                ParseState::CharsetSkip => {
                    self.state = ParseState::Normal;
                }
                ParseState::Csi => {
                    if ('@'..='~').contains(&ch) {
                        self.handle_csi(ch, grid, replies);
                        self.state = ParseState::Normal;
                    } else {
                        self.buf.push(ch);
                    }
                }
                ParseState::Osc => {
                    if ch == '\x07' {
                        self.state = ParseState::Normal;
                    } else if ch == '\x1b' {
                        self.state = ParseState::OscEsc;
                    }
                }
                ParseState::OscEsc => {
                    self.state = ParseState::Normal;
                }
            }
        }
    }

    fn handle_csi(&self, final_ch: char, grid: &mut TerminalGrid, replies: &mut Vec<String>) {
        let raw = self.buf.trim();
        let private = raw.starts_with('?');
        let param_str = raw.trim_start_matches('?').trim_start_matches('>');
        let params: Vec<usize> = if param_str.is_empty() {
            Vec::new()
        } else {
            param_str
                .split(';')
                .map(|s| s.parse::<usize>().unwrap_or(0))
                .collect()
        };
        let p = |idx: usize, default: usize| -> usize {
            params
                .get(idx)
                .copied()
                .filter(|&v| v > 0)
                .unwrap_or(default)
        };
        let p0 = |idx: usize, default: usize| -> usize {
            params.get(idx).copied().unwrap_or(default)
        };

        if private {
            for &code in &params {
                match (code, final_ch) {
                    (25, 'h') => grid.cursor_visible = true,
                    (25, 'l') => grid.cursor_visible = false,
                    (2004, 'h') => grid.bracketed_paste = true,
                    (2004, 'l') => grid.bracketed_paste = false,
                    (1049, 'h') | (1047, 'h') | (47, 'h') => {
                        if grid.alt_backup.is_none() {
                            let backup = grid.screen.clone();
                            grid.alt_backup = Some((backup, grid.cursor_row, grid.cursor_col));
                            grid.screen = vec![vec![TermCell::default(); grid.cols]; grid.rows];
                            grid.cursor_row = 0;
                            grid.cursor_col = 0;
                            grid.pending_wrap = false;
                        }
                    }
                    (1049, 'l') | (1047, 'l') | (47, 'l') => {
                        if let Some((backup, r, c)) = grid.alt_backup.take() {
                            grid.screen = backup;
                            for row in &mut grid.screen {
                                row.resize(grid.cols, TermCell::default());
                            }
                            grid.screen.resize(grid.rows, vec![TermCell::default(); grid.cols]);
                            grid.cursor_row = r.min(grid.rows - 1);
                            grid.cursor_col = c.min(grid.cols - 1);
                            grid.pending_wrap = false;
                        }
                    }
                    _ => {}
                }
            }
            return;
        }

        match final_ch {
            'm' => {
                if params.is_empty() {
                    grid.current_fg = DEFAULT_FG;
                    grid.bold = false;
                    return;
                }
                let mut i = 0usize;
                while i < params.len() {
                    match params[i] {
                        0 => {
                            grid.current_fg = DEFAULT_FG;
                            grid.bold = false;
                        }
                        1 => grid.bold = true,
                        22 => grid.bold = false,
                        39 => grid.current_fg = DEFAULT_FG,
                        30 => grid.current_fg = 0x5A5D64,
                        31 | 91 => grid.current_fg = 0xF44747,
                        32 | 92 => grid.current_fg = 0x59A869,
                        33 | 93 => grid.current_fg = 0xF2C55C,
                        34 | 94 => grid.current_fg = 0x56A8F5,
                        35 | 95 => grid.current_fg = 0xC77DBB,
                        36 | 96 => grid.current_fg = 0x2AACB8,
                        37 | 97 => grid.current_fg = 0xFFFFFF,
                        90 => grid.current_fg = 0x7A7E85,
                        38 => {
                            if i + 1 < params.len() && params[i + 1] == 5 && i + 2 < params.len() {
                                grid.current_fg = xterm256_to_rgb(params[i + 2] as u8);
                                i += 2;
                            } else if i + 1 < params.len()
                                && params[i + 1] == 2
                                && i + 4 < params.len()
                            {
                                let r = params[i + 2].min(255) as u32;
                                let g = params[i + 3].min(255) as u32;
                                let b = params[i + 4].min(255) as u32;
                                grid.current_fg = (r << 16) | (g << 8) | b;
                                i += 4;
                            }
                        }
                        48 => {
                            if i + 1 < params.len() && params[i + 1] == 5 {
                                i += 2;
                            } else if i + 1 < params.len() && params[i + 1] == 2 {
                                i += 4;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
            'H' | 'f' => {
                let row1 = p(0, 1);
                let col1 = p(1, 1);
                grid.cursor_row = row1.saturating_sub(1).min(grid.rows - 1);
                grid.cursor_col = col1.saturating_sub(1).min(grid.cols - 1);
                grid.pending_wrap = false;
            }
            'A' => {
                let n = p(0, 1);
                grid.cursor_row = grid.cursor_row.saturating_sub(n);
                grid.pending_wrap = false;
            }
            'B' => {
                let n = p(0, 1);
                grid.cursor_row = (grid.cursor_row + n).min(grid.rows - 1);
                grid.pending_wrap = false;
            }
            'C' => {
                let n = p(0, 1);
                grid.cursor_col = (grid.cursor_col + n).min(grid.cols - 1);
                grid.pending_wrap = false;
            }
            'D' => {
                let n = p(0, 1);
                grid.cursor_col = grid.cursor_col.saturating_sub(n);
                grid.pending_wrap = false;
            }
            'E' => {
                let n = p(0, 1);
                grid.cursor_row = (grid.cursor_row + n).min(grid.rows - 1);
                grid.cursor_col = 0;
                grid.pending_wrap = false;
            }
            'F' => {
                let n = p(0, 1);
                grid.cursor_row = grid.cursor_row.saturating_sub(n);
                grid.cursor_col = 0;
                grid.pending_wrap = false;
            }
            'G' => {
                let col1 = p(0, 1);
                grid.cursor_col = col1.saturating_sub(1).min(grid.cols - 1);
                grid.pending_wrap = false;
            }
            'd' => {
                let row1 = p(0, 1);
                grid.cursor_row = row1.saturating_sub(1).min(grid.rows - 1);
                grid.pending_wrap = false;
            }
            'J' => {
                let mode = p0(0, 0);
                match mode {
                    0 => {
                        for c in grid.cursor_col..grid.cols {
                            grid.screen[grid.cursor_row][c] = TermCell::default();
                        }
                        for r in (grid.cursor_row + 1)..grid.rows {
                            for c in 0..grid.cols {
                                grid.screen[r][c] = TermCell::default();
                            }
                        }
                    }
                    1 => {
                        for r in 0..grid.cursor_row {
                            for c in 0..grid.cols {
                                grid.screen[r][c] = TermCell::default();
                            }
                        }
                        for c in 0..=grid.cursor_col.min(grid.cols - 1) {
                            grid.screen[grid.cursor_row][c] = TermCell::default();
                        }
                    }
                    2 | 3 => {
                        for r in 0..grid.rows {
                            for c in 0..grid.cols {
                                grid.screen[r][c] = TermCell::default();
                            }
                        }
                        if mode == 3 {
                            grid.scrollback.clear();
                        }
                    }
                    _ => {}
                }
            }
            'K' => {
                let mode = p0(0, 0);
                let r = grid.cursor_row;
                match mode {
                    0 => {
                        for c in grid.cursor_col..grid.cols {
                            grid.screen[r][c] = TermCell::default();
                        }
                    }
                    1 => {
                        for c in 0..=grid.cursor_col.min(grid.cols - 1) {
                            grid.screen[r][c] = TermCell::default();
                        }
                    }
                    2 => {
                        for c in 0..grid.cols {
                            grid.screen[r][c] = TermCell::default();
                        }
                    }
                    _ => {}
                }
            }
            'P' => {
                let n = p(0, 1).min(grid.cols - grid.cursor_col);
                let row = &mut grid.screen[grid.cursor_row];
                row.drain(grid.cursor_col..(grid.cursor_col + n));
                row.resize(grid.cols, TermCell::default());
            }
            '@' => {
                let n = p(0, 1).min(grid.cols - grid.cursor_col);
                let row = &mut grid.screen[grid.cursor_row];
                for _ in 0..n {
                    row.insert(grid.cursor_col, TermCell::default());
                }
                row.truncate(grid.cols);
            }
            'X' => {
                let n = p(0, 1);
                let end = (grid.cursor_col + n).min(grid.cols);
                for c in grid.cursor_col..end {
                    grid.screen[grid.cursor_row][c] = TermCell::default();
                }
            }
            'L' => {
                let n = p(0, 1).min(grid.rows);
                for _ in 0..n {
                    if grid.scroll_bottom < grid.screen.len() {
                        grid.screen.remove(grid.scroll_bottom);
                        grid.screen
                            .insert(grid.cursor_row, vec![TermCell::default(); grid.cols]);
                    }
                }
            }
            'M' => {
                let n = p(0, 1).min(grid.rows);
                for _ in 0..n {
                    if grid.cursor_row < grid.screen.len() {
                        grid.screen.remove(grid.cursor_row);
                        grid.screen
                            .insert(grid.scroll_bottom.min(grid.screen.len()), vec![TermCell::default(); grid.cols]);
                    }
                }
            }
            'S' => {
                let n = p(0, 1);
                grid.scroll_up_region(n);
            }
            'T' => {
                let n = p(0, 1);
                grid.scroll_down_region(n);
            }
            'r' => {
                let top = p(0, 1).saturating_sub(1).min(grid.rows - 1);
                let bot = p(1, grid.rows).saturating_sub(1).min(grid.rows - 1);
                if top < bot {
                    grid.scroll_top = top;
                    grid.scroll_bottom = bot;
                } else {
                    grid.scroll_top = 0;
                    grid.scroll_bottom = grid.rows - 1;
                }
                grid.cursor_row = 0;
                grid.cursor_col = 0;
            }
            's' => {
                grid.saved_row = grid.cursor_row;
                grid.saved_col = grid.cursor_col;
            }
            'u' => {
                grid.cursor_row = grid.saved_row.min(grid.rows - 1);
                grid.cursor_col = grid.saved_col.min(grid.cols - 1);
                grid.pending_wrap = false;
            }
            'n' => {
                let code = p0(0, 0);
                if code == 6 {
                    replies.push(format!(
                        "\x1b[{};{}R",
                        grid.cursor_row + 1,
                        grid.cursor_col + 1
                    ));
                } else if code == 5 {
                    replies.push("\x1b[0n".to_string());
                }
            }
            'c' => {
                if !raw.starts_with('>') {
                    replies.push("\x1b[?1;2c".to_string());
                }
            }
            _ => {}
        }
    }
}

fn xterm256_to_rgb(idx: u8) -> u32 {
    const ANSI16: [u32; 16] = [
        0x5A5D64, 0xF44747, 0x59A869, 0xF2C55C, 0x56A8F5, 0xC77DBB, 0x2AACB8, 0xDFE1E5,
        0x7A7E85, 0xF44747, 0x59A869, 0xF2C55C, 0x56A8F5, 0xC77DBB, 0x2AACB8, 0xFFFFFF,
    ];
    if idx < 16 {
        return ANSI16[idx as usize];
    }
    if idx < 232 {
        let n = idx - 16;
        let r = n / 36;
        let g = (n % 36) / 6;
        let b = n % 6;
        let to_c = |v: u8| if v == 0 { 0u32 } else { 55 + v as u32 * 40 };
        return (to_c(r) << 16) | (to_c(g) << 8) | to_c(b);
    }
    let gray = 8 + (idx as u32 - 232) * 10;
    (gray << 16) | (gray << 8) | gray
}

pub fn parse_ansi_colored_spans(line: &str, current_fg: &mut u32) -> Vec<(String, u32)> {
    let mut grid = TerminalGrid::new(2, 512);
    grid.current_fg = *current_fg;
    let mut parser = VtStreamParser::new();
    parser.feed(line, &mut grid);
    *current_fg = grid.current_fg;
    row_to_styled_line(&grid.screen[0], false).spans
}

/// Normalizes line endings for ConPTY input: `\r\n` and `\n` become `\r` (Enter),
/// because PowerShell/PSReadLine interprets bare `\n` (`0x0A`) as `InsertLineAbove`
/// (`Ctrl+Enter`), which reverses multi-line pastes line-by-line.
pub fn normalize_terminal_text(text: &str) -> String {
    text.replace("\r\n", "\r").replace('\n', "\r")
}

/// Formats clipboard text for terminal paste, normalizing line endings to `\r` and
/// wrapping with bracketed-paste escape markers when DEC private mode 2004 is enabled.
pub fn format_terminal_paste_payload(text: &str, bracketed_paste: bool) -> String {
    let normalized = normalize_terminal_text(text);
    if !bracketed_paste {
        return normalized;
    }
    let sanitized = normalized
        .replace("\x1b[200~", "")
        .replace("\x1b[201~", "");
    format!("\x1b[200~{sanitized}\x1b[201~")
}

pub fn strip_ansi_codes(input: &str) -> String {
    let mut fg = DEFAULT_FG;
    parse_ansi_colored_spans(input, &mut fg)
        .into_iter()
        .map(|(s, _)| s)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn test_strip_and_color_ansi_codes() {
        let raw = "\x1b[32m   Compiling\x1b[0m ide-app v0.1.0";
        let mut fg = DEFAULT_FG;
        let spans = parse_ansi_colored_spans(raw, &mut fg);
        assert_eq!(spans[0].1, 0x59A869);
        assert_eq!(strip_ansi_codes(raw), "   Compiling ide-app v0.1.0");
    }

    #[test]
    fn test_vt100_cup_and_clear_screen() {
        let mut grid = TerminalGrid::new(6, 40);
        let mut parser = VtStreamParser::new();
        parser.feed("\x1b[2J\x1b[2;1HPS D:\\> \x1b[32mecho ok\x1b[0m", &mut grid);
        let snap = grid.snapshot();
        assert_eq!(snap.cursor_line, 1);
        assert_eq!(snap.lines[1].raw_text(), "PS D:\\> echo ok");
    }

    #[test]
    fn test_terminal_paste_normalizes_newlines_and_bracketed_mode() {
        assert_eq!(
            normalize_terminal_text("echo 1\r\necho 2\necho 3"),
            "echo 1\recho 2\recho 3"
        );
        assert_eq!(
            format_terminal_paste_payload("line1\nline2\r\nline3", false),
            "line1\rline2\rline3"
        );
        assert_eq!(
            format_terminal_paste_payload("line1\n\x1b[201~line2", true),
            "\x1b[200~line1\rline2\x1b[201~"
        );

        let mut grid = TerminalGrid::new(6, 40);
        let mut parser = VtStreamParser::new();
        assert!(!grid.bracketed_paste);
        parser.feed("\x1b[?2004h", &mut grid);
        assert!(grid.bracketed_paste);
        parser.feed("\x1b[?2004l", &mut grid);
        assert!(!grid.bracketed_paste);
    }

    #[test]
    fn test_live_conpty_multiline_paste_preserves_line_order() {
        let mut tab = LivePtyTab::spawn_powershell("test-paste", Path::new("."), None);
        thread::sleep(Duration::from_millis(800));
        tab.paste_text("Write-Output '__PASTE_1__'\nWrite-Output '__PASTE_2__'\nWrite-Output '__PASTE_3__'\n");
        let start = Instant::now();
        let mut joined = String::new();
        let mut ready = false;
        while start.elapsed() < Duration::from_secs(6) {
            let snap = tab.snapshot();
            joined = snap
                .lines
                .iter()
                .map(|l| l.raw_text())
                .collect::<Vec<_>>()
                .join("\n");
            if joined.contains("__PASTE_3__") {
                ready = true;
                break;
            }
            thread::sleep(Duration::from_millis(80));
        }
        assert!(ready, "Timed out waiting for pasted commands:\n{joined}");
        let p1 = joined.find("__PASTE_1__").expect("first line present");
        let p2 = joined.find("__PASTE_2__").expect("second line present");
        let p3 = joined.find("__PASTE_3__").expect("third line present");
        assert!(
            p1 < p2 && p2 < p3,
            "Pasted lines appeared out of order (reversed?):\n{joined}"
        );
    }

    #[test]
    fn test_live_conpty_powershell_command_execution() {
        let mut tab = LivePtyTab::spawn_powershell("test", Path::new("."), None);
        thread::sleep(Duration::from_millis(800));
        tab.send_command("Write-Output '__PTY_LIVE_OK__'");
        let start = Instant::now();
        let mut got_output = false;
        let mut last_joined = String::new();
        while start.elapsed() < Duration::from_secs(6) {
            let snap = tab.snapshot();
            last_joined = snap
                .lines
                .iter()
                .map(|l| l.raw_text())
                .collect::<Vec<_>>()
                .join("\n");
            if last_joined.contains("__PTY_LIVE_OK__") {
                got_output = true;
                break;
            }
            thread::sleep(Duration::from_millis(80));
        }
        assert!(
            got_output,
            "ConPTY powershell failed (alive={}):\n{}",
            tab.alive.load(Ordering::Relaxed),
            last_joined
        );
    }

    #[test]
    fn test_extract_current_prompt_command() {
        let mut grid = TerminalGrid::new(6, 60);
        let mut parser = VtStreamParser::new();
        parser.feed(
            "\x1b[2J\x1b[2;1HPS D:\\work\\demo> docker compose up -d",
            &mut grid,
        );
        let parsed = grid.extract_current_prompt_command().expect("prompt command");
        assert_eq!(parsed.0, Some(PathBuf::from("D:\\work\\demo")));
        assert_eq!(parsed.1, "docker compose up -d");
    }

    #[test]
    fn test_lazy_terminal_session_defers_powershell_spawn() {
        let sess = TerminalSession::new("Local", Path::new("."));
        assert!(!sess.active_pty().alive.load(Ordering::Relaxed));
        assert!(sess.active_pty().backend.is_none());
    }

    #[test]
    fn test_bound_run_tabs_isolate_running_and_stopping_per_script() {
        let mut sess = TerminalSession::new("Local", Path::new("."));
        let cfg_a = RunConfiguration {
            name: "Script A".to_string(),
            language: ide_core::LanguageId::PlainText,
            command: "powershell".to_string(),
            args: vec!["-Command".to_string(), "Start-Sleep -Seconds 5".to_string()],
            cwd: PathBuf::from("."),
            source: ide_core::RunConfigSource::Custom,
            target_file: None,
        };
        let cfg_b = RunConfiguration {
            name: "Script B".to_string(),
            language: ide_core::LanguageId::PlainText,
            command: "powershell".to_string(),
            args: vec!["-Command".to_string(), "Start-Sleep -Seconds 6".to_string()],
            cwd: PathBuf::from("."),
            source: ide_core::RunConfigSource::Custom,
            target_file: None,
        };
        let key_a = cfg_a.identity_key();
        let key_b = cfg_b.identity_key();

        sess.start_bound_run_configuration(&cfg_a);
        assert!(sess.is_run_config_running(&key_a));
        assert!(!sess.is_run_config_running(&key_b));
        assert_eq!(sess.tabs.len(), 2);

        sess.start_bound_run_configuration(&cfg_b);
        assert!(sess.is_run_config_running(&key_a));
        assert!(sess.is_run_config_running(&key_b));
        assert_eq!(sess.tabs.len(), 3);

        // Stopping Script A leaves Script B running untouched
        assert!(sess.stop_bound_run_configuration(&key_a));
        assert!(!sess.is_run_config_running(&key_a));
        assert!(sess.is_run_config_running(&key_b));

        assert!(sess.stop_bound_run_configuration(&key_b));
        assert!(!sess.is_run_config_running(&key_b));
    }
}
