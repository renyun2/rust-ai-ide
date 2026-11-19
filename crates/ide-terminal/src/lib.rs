use anyhow::Result;
use ide_core::RunConfiguration;
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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

struct PtyTabBackend {
    writer: Box<dyn Write + Send>,
    _master: Box<dyn MasterPty + Send>,
}

pub struct LivePtyTab {
    pub title: String,
    pub cwd: PathBuf,
    pub lines: Arc<Mutex<Vec<StyledTerminalLine>>>,
    pub current_input: String,
    pub scroll_offset: usize,
    backend: Option<Arc<Mutex<PtyTabBackend>>>,
}

impl LivePtyTab {
    pub fn spawn_powershell(title: impl Into<String>, cwd: &Path) -> Self {
        let title = title.into();
        let cwd_buf = cwd.to_path_buf();
        let lines = Arc::new(Mutex::new(Vec::new()));

        let pty_system = native_pty_system();
        let pair_res = pty_system.openpty(PtySize {
            rows: 36,
            cols: 140,
            pixel_width: 0,
            pixel_height: 0,
        });

        let mut backend_opt = None;

        if let Ok(pair) = pair_res {
            let mut cmd = CommandBuilder::new("powershell.exe");
            cmd.args(["-NoLogo"]);
            cmd.cwd(&cwd_buf);

            if let Ok(_child) = pair.slave.spawn_command(cmd) {
                drop(pair.slave);
                if let (Ok(mut reader), Ok(writer)) =
                    (pair.master.try_clone_reader(), pair.master.take_writer())
                {
                    let lines_clone = lines.clone();
                    thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        let mut pending = String::new();
                        let mut current_fg: u32 = 0xDFE1E5;
                        while let Ok(n) = reader.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            let chunk = String::from_utf8_lossy(&buf[..n]);
                            pending.push_str(&chunk);
                            let parsed_lines =
                                parse_ansi_stream_lines(&mut pending, &mut current_fg);
                            if !parsed_lines.is_empty() {
                                let mut guard = lines_clone.lock();
                                for pl in parsed_lines {
                                    if !pl.raw_text().trim().is_empty() || guard.is_empty() {
                                        guard.push(pl);
                                    }
                                }
                                if guard.len() > 600 {
                                    let drain_count = guard.len() - 500;
                                    guard.drain(0..drain_count);
                                }
                            }
                        }
                    });

                    backend_opt = Some(Arc::new(Mutex::new(PtyTabBackend {
                        writer,
                        _master: pair.master,
                    })));
                }
            }
        }

        {
            let mut g = lines.lock();
            g.push(StyledTerminalLine {
                spans: vec![
                    (
                        format!("PS {}> ", cwd_buf.display()),
                        0xDFE1E5,
                    ),
                    ("cargo ".to_string(), 0xF2C55C),
                    ("check --workspace".to_string(), 0xDFE1E5),
                ],
            });
            g.push(StyledTerminalLine {
                spans: vec![
                    ("    Finished ".to_string(), 0x59A869),
                    (
                        "`dev` profile [unoptimized + debuginfo] target(s) in 0.25s".to_string(),
                        0xDFE1E5,
                    ),
                ],
            });
        }

        Self {
            title,
            cwd: cwd_buf,
            lines,
            current_input: String::new(),
            scroll_offset: 0,
            backend: backend_opt,
        }
    }

    pub fn send_command(&mut self, cmd_line: &str) {
        let trimmed = cmd_line.trim();
        {
            let mut g = self.lines.lock();
            let mut parts = trimmed.splitn(2, ' ');
            let first = parts.next().unwrap_or("");
            let rest = parts.next().unwrap_or("");
            let mut spans = vec![(format!("PS {}> ", self.cwd.display()), 0xDFE1E5)];
            if !first.is_empty() {
                spans.push((first.to_string(), 0xF2C55C));
            }
            if !rest.is_empty() {
                spans.push((format!(" {rest}"), 0xDFE1E5));
            }
            g.push(StyledTerminalLine { spans });
        }

        if let Some(b) = &self.backend {
            let mut guard = b.lock();
            let _ = guard.writer.write_all(format!("{trimmed}\r\n").as_bytes());
            let _ = guard.writer.flush();
        }
        self.scroll_offset = 0;
    }

    pub fn snapshot_lines(&self) -> Vec<StyledTerminalLine> {
        self.lines.lock().clone()
    }
}

pub struct TerminalSession {
    pub tabs: Vec<LivePtyTab>,
    pub active_tab: usize,
    pub cwd: PathBuf,
    pub input_buffer: String,
}

impl TerminalSession {
    pub fn new(title: impl Into<String>, cwd: &Path) -> Self {
        let first_tab = LivePtyTab::spawn_powershell(title, cwd);
        Self {
            tabs: vec![first_tab],
            active_tab: 0,
            cwd: cwd.to_path_buf(),
            input_buffer: String::new(),
        }
    }

    pub fn add_new_tab(&mut self) {
        let idx = self.tabs.len() + 1;
        let tab = LivePtyTab::spawn_powershell(format!("本地 ({idx})"), &self.cwd);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
    }

    pub fn close_tab(&mut self, idx: usize) {
        if self.tabs.len() <= 1 || idx >= self.tabs.len() {
            return;
        }
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

    pub fn execute_command_line(&mut self, cmd_line: &str) {
        self.active_pty_mut().send_command(cmd_line);
    }

    pub fn execute_run_configuration(&mut self, cfg: &RunConfiguration) {
        let full_cmd = if cfg.args.is_empty() {
            cfg.command.clone()
        } else {
            format!("{} {}", cfg.command, cfg.args.join(" "))
        };
        self.execute_command_line(&full_cmd);
    }
}

/// 解析 ANSI 颜色序列并转化为带 RGB 颜色的 `StyledTerminalLine`（支持绿、黄、红、蓝、青等原生终端色彩）
fn parse_ansi_stream_lines(pending: &mut String, current_fg: &mut u32) -> Vec<StyledTerminalLine> {
    let mut out = Vec::new();
    while let Some(nl_pos) = pending.find('\n') {
        let raw_line: String = pending.drain(..=nl_pos).collect();
        let cleaned = strip_osc_and_private_sequences(&raw_line);
        let spans = parse_ansi_colored_spans(cleaned.trim_end_matches(['\r', '\n']), current_fg);
        out.push(StyledTerminalLine { spans });
    }
    out
}

fn strip_osc_and_private_sequences(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek().copied() {
                Some(']') => {
                    // OSC sequence until BEL (\x07) or ST (\x1b\\)
                    chars.next();
                    while let Some(nc) = chars.next() {
                        if nc == '\x07' {
                            break;
                        }
                    }
                }
                Some('[') => {
                    out.push('\x1b');
                }
                _ => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn parse_ansi_colored_spans(line: &str, current_fg: &mut u32) -> Vec<(String, u32)> {
    let mut spans = Vec::new();
    let mut current_text = String::new();
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            let mut seq = String::new();
            let mut final_char = 'm';
            for nc in chars.by_ref() {
                if ('@'..='~').contains(&nc) {
                    final_char = nc;
                    break;
                }
                seq.push(nc);
            }
            if final_char == 'm' {
                if !current_text.is_empty() {
                    spans.push((std::mem::take(&mut current_text), *current_fg));
                }
                for code_str in seq.split(';') {
                    match code_str.parse::<u32>().unwrap_or(0) {
                        0 | 39 => *current_fg = 0xDFE1E5,
                        31 | 91 => *current_fg = 0xF44747, // 终端红 (error)
                        32 | 92 => *current_fg = 0x59A869, // 终端绿 (Compiling / Finished / Running)
                        33 | 93 => *current_fg = 0xF2C55C, // 终端黄 (Command / Warning)
                        34 | 94 => *current_fg = 0x56A8F5, // 终端蓝
                        35 | 95 => *current_fg = 0xC77DBB, // 终端紫
                        36 | 96 => *current_fg = 0x2AACB8, // 终端青
                        _ => {}
                    }
                }
            }
        } else if c != '\r' && !c.is_control() {
            current_text.push(c);
        }
    }

    if !current_text.is_empty() {
        spans.push((current_text, *current_fg));
    }
    spans
}

pub fn strip_ansi_codes(input: &str) -> String {
    let mut fg = 0xDFE1E5;
    parse_ansi_colored_spans(input, &mut fg)
        .into_iter()
        .map(|(s, _)| s)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_and_color_ansi_codes() {
        let raw = "\x1b[32m   Compiling\x1b[0m ide-app v0.1.0";
        let mut fg = 0xDFE1E5;
        let spans = parse_ansi_colored_spans(raw, &mut fg);
        assert_eq!(spans[0].1, 0x59A869);
        assert_eq!(strip_ansi_codes(raw), "   Compiling ide-app v0.1.0");
    }
}
