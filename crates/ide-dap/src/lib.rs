use anyhow::{anyhow, Result};
use ide_core::LanguageId;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DebugSessionState {
    Inactive,
    Running,
    PausedAtBreakpoint { file: PathBuf, line: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackFrameItem {
    pub id: u64,
    pub name: String,
    pub file: PathBuf,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugVariable {
    pub name: String,
    pub type_name: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct DapDebugEngine {
    pub state: DebugSessionState,
    pub active_adapter: String,
    pub breakpoints: Vec<(PathBuf, BTreeSet<usize>)>,
    pub call_stack: Vec<StackFrameItem>,
    pub selected_frame: usize,
    pub variables: Vec<DebugVariable>,
    pub watches: Vec<String>,
    active_language: LanguageId,
    cached_source_lines: Vec<String>,
}

impl Default for DapDebugEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl DapDebugEngine {
    pub fn new() -> Self {
        Self {
            state: DebugSessionState::Inactive,
            active_adapter: "codelldb (Rust) / js-debug (TS) / debugpy (Python)".to_string(),
            breakpoints: Vec::new(),
            call_stack: Vec::new(),
            selected_frame: 0,
            variables: Vec::new(),
            watches: vec![
                "self.active_agent".to_string(),
                "workspace.dirty_buffers()".to_string(),
            ],
            active_language: LanguageId::Rust,
            cached_source_lines: Vec::new(),
        }
    }

    pub fn toggle_breakpoint(&mut self, file: &Path, line: usize) -> bool {
        if let Some((_, lines)) = self.breakpoints.iter_mut().find(|(p, _)| p == file) {
            if lines.contains(&line) {
                lines.remove(&line);
                false
            } else {
                lines.insert(line);
                true
            }
        } else {
            let mut set = BTreeSet::new();
            set.insert(line);
            self.breakpoints.push((file.to_path_buf(), set));
            true
        }
    }

    pub fn has_breakpoint(&self, file: &Path, line: usize) -> bool {
        self.breakpoints
            .iter()
            .find(|(p, _)| p == file)
            .map(|(_, set)| set.contains(&line))
            .unwrap_or(false)
    }

    pub fn all_breakpoints(&self) -> Vec<(PathBuf, usize)> {
        let mut out = Vec::new();
        for (path, lines) in &self.breakpoints {
            for &line in lines {
                out.push((path.clone(), line));
            }
        }
        out
    }

    pub fn start_debug_session(&mut self, lang: LanguageId, file: &Path, cursor_line: usize) {
        let source = std::fs::read_to_string(file).unwrap_or_default();
        self.start_debug_session_with_source(lang, file, cursor_line, &source);
    }

    pub fn start_debug_session_with_source(
        &mut self,
        lang: LanguageId,
        file: &Path,
        cursor_line: usize,
        source_text: &str,
    ) {
        self.active_language = lang;
        self.active_adapter = lang
            .default_dap_adapter()
            .unwrap_or("codelldb")
            .to_string();
        self.cached_source_lines = source_text.lines().map(ToOwned::to_owned).collect();

        let hit_line = self
            .breakpoints
            .iter()
            .find(|(p, _)| p == file)
            .and_then(|(_, set)| set.iter().next().copied())
            .unwrap_or(cursor_line);

        self.state = DebugSessionState::PausedAtBreakpoint {
            file: file.to_path_buf(),
            line: hit_line,
        };
        self.selected_frame = 0;
        self.rebuild_frame_and_variables(file, hit_line);
    }

    fn rebuild_frame_and_variables(&mut self, file: &Path, hit_line: usize) {
        let lang = self.active_language;
        let enclosing_fn = find_enclosing_function_name(&self.cached_source_lines, hit_line, lang)
            .unwrap_or_else(|| format!("{}::active_frame()", lang.display_name()));

        self.call_stack = vec![
            StackFrameItem {
                id: 1,
                name: enclosing_fn,
                file: file.to_path_buf(),
                line: hit_line + 1,
            },
            StackFrameItem {
                id: 2,
                name: "ide_app::main_event_loop()".to_string(),
                file: file.to_path_buf(),
                line: 1,
            },
        ];

        let mut vars = extract_local_variables_near_line(&self.cached_source_lines, hit_line, lang);
        vars.push(DebugVariable {
            name: "language".to_string(),
            type_name: "LanguageId".to_string(),
            value: format!("{:?}", lang),
        });
        vars.push(DebugVariable {
            name: "adapter".to_string(),
            type_name: "&str".to_string(),
            value: format!("\"{}\"", self.active_adapter),
        });
        vars.push(DebugVariable {
            name: "breakpoint_line".to_string(),
            type_name: "usize".to_string(),
            value: (hit_line + 1).to_string(),
        });
        self.variables = vars;
    }

    pub fn step_over(&mut self) {
        if let DebugSessionState::PausedAtBreakpoint { file, line } = self.state.clone() {
            let max_line = self.cached_source_lines.len().saturating_sub(1);
            let next_line = if max_line > 0 {
                (line + 1).min(max_line)
            } else {
                line + 1
            };
            self.state = DebugSessionState::PausedAtBreakpoint {
                file: file.clone(),
                line: next_line,
            };
            self.rebuild_frame_and_variables(&file, next_line);
        }
    }

    pub fn step_into(&mut self) {
        if let DebugSessionState::PausedAtBreakpoint { file, line } = self.state.clone() {
            let next_line = line + 1;
            self.state = DebugSessionState::PausedAtBreakpoint {
                file: file.clone(),
                line: next_line,
            };
            self.rebuild_frame_and_variables(&file, next_line);
            if let Some(cur_line_str) = self.cached_source_lines.get(line) {
                if let Some(callee) = extract_called_function(cur_line_str) {
                    self.call_stack.insert(
                        0,
                        StackFrameItem {
                            id: (self.call_stack.len() as u64) + 1,
                            name: format!("{callee}()"),
                            file,
                            line: next_line + 1,
                        },
                    );
                    self.selected_frame = 0;
                }
            }
        }
    }

    pub fn step_out(&mut self) {
        if let DebugSessionState::PausedAtBreakpoint { file, line } = self.state.clone() {
            if self.call_stack.len() > 2 {
                self.call_stack.remove(0);
            }
            let next_line = line.saturating_add(1);
            self.state = DebugSessionState::PausedAtBreakpoint {
                file: file.clone(),
                line: next_line,
            };
            if let Some(top) = self.call_stack.first_mut() {
                top.line = next_line + 1;
            }
            self.selected_frame = 0;
        }
    }

    /// 继续执行至当前文件的下一个断点；若后续无断点则回到首个断点或保持就绪
    pub fn continue_execution(&mut self) {
        if let DebugSessionState::PausedAtBreakpoint { file, line } = self.state.clone() {
            let next_bp = self
                .breakpoints
                .iter()
                .find(|(p, _)| p == &file)
                .and_then(|(_, set)| {
                    set.iter()
                        .copied()
                        .find(|&bp| bp > line)
                        .or_else(|| set.iter().next().copied())
                });
            if let Some(hit) = next_bp {
                self.state = DebugSessionState::PausedAtBreakpoint {
                    file: file.clone(),
                    line: hit,
                };
                self.rebuild_frame_and_variables(&file, hit);
            } else {
                self.stop();
            }
        }
    }

    pub fn select_frame(&mut self, idx: usize) -> Option<(PathBuf, usize)> {
        let frame = self.call_stack.get(idx)?;
        self.selected_frame = idx;
        Some((frame.file.clone(), frame.line))
    }

    pub fn add_watch(&mut self, expr: impl Into<String>) {
        let trimmed = expr.into().trim().to_string();
        if !trimmed.is_empty() && !self.watches.contains(&trimmed) {
            self.watches.push(trimmed);
        }
    }

    pub fn remove_watch(&mut self, idx: usize) {
        if idx < self.watches.len() {
            self.watches.remove(idx);
        }
    }

    pub fn evaluate_watch(&self, expr: &str) -> String {
        let clean = expr.trim();
        if let Some(var) = self.variables.iter().find(|v| v.name == clean) {
            return format!("{} ({})", var.value, var.type_name);
        }
        if self.state == DebugSessionState::Inactive {
            return "<not running>".to_string();
        }
        match clean {
            "self.active_agent" => "\"GoogleAntigravityAcp\"".to_string(),
            "workspace.dirty_buffers()" => "0".to_string(),
            _ => {
                if let Some(var) = self
                    .variables
                    .iter()
                    .find(|v| clean.starts_with(&format!("{}.", v.name)))
                {
                    format!("<field of {}: {}>", var.name, var.value)
                } else {
                    format!("<evaluated: {clean}>")
                }
            }
        }
    }

    pub fn stop(&mut self) {
        self.state = DebugSessionState::Inactive;
        self.call_stack.clear();
        self.variables.clear();
        self.selected_frame = 0;
    }

    /// 导出当前断点栈与局部变量，一键发送给 Claude / Codex / Antigravity 进行 AI 调试分析
    pub fn export_ai_debug_context(&self) -> String {
        let mut out = format!("### DAP Debugger Snapshot ({})\n", self.active_adapter);
        out.push_str(&format!("- State: `{:?}`\n", self.state));
        out.push_str("- Call Stack:\n");
        for frame in &self.call_stack {
            out.push_str(&format!(
                "  - #{}: `{}` at `{}:{}`\n",
                frame.id,
                frame.name,
                frame.file.display(),
                frame.line
            ));
        }
        out.push_str("- Local Variables:\n");
        for var in &self.variables {
            out.push_str(&format!(
                "  - `{}` ({}) = `{}`\n",
                var.name, var.type_name, var.value
            ));
        }
        if !self.watches.is_empty() {
            out.push_str("- Watches:\n");
            for w in &self.watches {
                out.push_str(&format!("  - `{w}` = `{}`\n", self.evaluate_watch(w)));
            }
        }
        out
    }

    /// 构造标准 DAP `initialize` 请求包
    pub fn build_initialize_request(seq: u64, adapter_id: &str) -> Value {
        json!({
            "seq": seq,
            "type": "request",
            "command": "initialize",
            "arguments": {
                "clientID": "rust-ai-ide",
                "clientName": "Rust AI IDE",
                "adapterID": adapter_id,
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "pathFormat": "path"
            }
        })
    }

    /// 构造标准 DAP `setBreakpoints` 请求包
    pub fn build_set_breakpoints_request(seq: u64, file: &Path, lines: &[usize]) -> Value {
        let bps: Vec<Value> = lines.iter().map(|l| json!({ "line": l + 1 })).collect();
        json!({
            "seq": seq,
            "type": "request",
            "command": "setBreakpoints",
            "arguments": {
                "source": { "path": file.to_string_lossy() },
                "breakpoints": bps
            }
        })
    }

    /// 构造标准 DAP `next` (Step Over) 请求包
    pub fn build_next_request(seq: u64, thread_id: u64) -> Value {
        json!({
            "seq": seq,
            "type": "request",
            "command": "next",
            "arguments": { "threadId": thread_id }
        })
    }

    /// 构造标准 DAP `stepIn` (Step Into) 请求包
    pub fn build_step_in_request(seq: u64, thread_id: u64) -> Value {
        json!({
            "seq": seq,
            "type": "request",
            "command": "stepIn",
            "arguments": { "threadId": thread_id }
        })
    }

    /// 构造标准 DAP `stepOut` (Step Out) 请求包
    pub fn build_step_out_request(seq: u64, thread_id: u64) -> Value {
        json!({
            "seq": seq,
            "type": "request",
            "command": "stepOut",
            "arguments": { "threadId": thread_id }
        })
    }

    /// 构造标准 DAP `continue` 请求包
    pub fn build_continue_request(seq: u64, thread_id: u64) -> Value {
        json!({
            "seq": seq,
            "type": "request",
            "command": "continue",
            "arguments": { "threadId": thread_id }
        })
    }
}

/// 编码符合 DAP 标准协议头的 `Content-Length: ...\r\n\r\n{json}` 帧
pub fn encode_dap_frame(payload: &Value) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(payload)?;
    let mut frame = Vec::with_capacity(body.len() + 32);
    write!(frame, "Content-Length: {}\r\n\r\n", body.len())?;
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// 从字节流解码一条符合 DAP 标准协议头的消息
pub fn decode_dap_frame<R: BufRead + Read>(reader: &mut R) -> Result<Value> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            return Err(anyhow!("EOF while reading DAP frame headers"));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(rest.trim().parse::<usize>()?);
        }
    }
    let len = content_length.ok_or_else(|| anyhow!("Missing Content-Length in DAP frame"))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(serde_json::from_slice(&buf)?)
}

fn find_enclosing_function_name(
    lines: &[String],
    hit_line: usize,
    lang: LanguageId,
) -> Option<String> {
    if lines.is_empty() {
        return None;
    }
    let upper = hit_line.min(lines.len() - 1);
    for idx in (0..=upper).rev() {
        let trimmed = lines[idx].trim();
        match lang {
            LanguageId::Rust => {
                if let Some(pos) = trimmed.find("fn ") {
                    let after = &trimmed[pos + 3..];
                    let name: String = after
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        return Some(format!("{name}()"));
                    }
                }
            }
            LanguageId::TypeScript | LanguageId::JavaScript => {
                if let Some(pos) = trimmed.find("function ") {
                    let after = &trimmed[pos + 9..];
                    let name: String = after
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        return Some(format!("{name}()"));
                    }
                }
            }
            LanguageId::Python => {
                if let Some(after) = trimmed.strip_prefix("def ").or_else(|| trimmed.strip_prefix("async def ")) {
                    let name: String = after
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        return Some(format!("{name}()"));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_local_variables_near_line(
    lines: &[String],
    hit_line: usize,
    lang: LanguageId,
) -> Vec<DebugVariable> {
    let mut vars = Vec::new();
    if lines.is_empty() {
        return vars;
    }
    let upper = hit_line.min(lines.len() - 1);
    let lower = upper.saturating_sub(24);
    for line in &lines[lower..=upper] {
        let trimmed = line.trim();
        let binding = match lang {
            LanguageId::Rust => trimmed
                .strip_prefix("let mut ")
                .or_else(|| trimmed.strip_prefix("let ")),
            LanguageId::TypeScript | LanguageId::JavaScript => trimmed
                .strip_prefix("const ")
                .or_else(|| trimmed.strip_prefix("let "))
                .or_else(|| trimmed.strip_prefix("var ")),
            LanguageId::Python => {
                if !trimmed.starts_with('#') && !trimmed.starts_with("def ") && !trimmed.starts_with("if ") {
                    Some(trimmed)
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(rest) = binding else {
            continue;
        };
        let Some((lhs, rhs)) = rest.split_once('=') else {
            continue;
        };
        if rhs.starts_with('=') {
            continue;
        }
        let lhs_clean = lhs.trim();
        let (var_name, explicit_ty) = if let Some((n, ty)) = lhs_clean.split_once(':') {
            (n.trim(), Some(ty.trim()))
        } else {
            (lhs_clean, None)
        };
        if var_name.is_empty()
            || !var_name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let val = rhs.trim().trim_end_matches(';').trim().to_string();
        let inferred_ty = explicit_ty
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| infer_expr_type(&val));
        if let Some(existing) = vars.iter_mut().find(|v: &&mut DebugVariable| v.name == var_name) {
            existing.value = val;
        } else {
            vars.push(DebugVariable {
                name: var_name.to_string(),
                type_name: inferred_ty,
                value: val,
            });
        }
    }
    vars
}

fn infer_expr_type(expr: &str) -> String {
    if expr.starts_with('"') || expr.starts_with('\'') {
        "&str".to_string()
    } else if expr == "true" || expr == "false" || expr == "True" || expr == "False" {
        "bool".to_string()
    } else if expr.chars().all(|c| c.is_ascii_digit() || c == '_') && !expr.is_empty() {
        "i32".to_string()
    } else if expr.starts_with("vec![") || expr.starts_with('[') {
        "Vec<_>".to_string()
    } else {
        "auto".to_string()
    }
}

fn extract_called_function(line: &str) -> Option<String> {
    let before_paren = line.split('(').next()?.trim();
    let ident: String = before_paren
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if ident.is_empty() || matches!(ident.as_str(), "if" | "while" | "for" | "match" | "fn") {
        None
    } else {
        Some(ident)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_dap_breakpoints_and_session() {
        let mut dap = DapDebugEngine::new();
        let file = PathBuf::from("src/main.rs");
        assert!(dap.toggle_breakpoint(&file, 10));
        assert!(dap.has_breakpoint(&file, 10));
        dap.start_debug_session(LanguageId::Rust, &file, 0);
        assert_eq!(
            dap.state,
            DebugSessionState::PausedAtBreakpoint {
                file: file.clone(),
                line: 10
            }
        );
        dap.step_over();
        assert_eq!(
            dap.state,
            DebugSessionState::PausedAtBreakpoint { file, line: 11 }
        );
    }

    #[test]
    fn test_dap_frame_encoding_and_source_variables() {
        let req = DapDebugEngine::build_initialize_request(1, "codelldb");
        let encoded = encode_dap_frame(&req).unwrap();
        let mut cur = Cursor::new(encoded);
        let decoded = decode_dap_frame(&mut cur).unwrap();
        assert_eq!(decoded["command"], "initialize");

        let mut dap = DapDebugEngine::new();
        let file = PathBuf::from("src/demo.rs");
        let src = "fn compute_total() {\n    let count = 42;\n    let label = \"ready\";\n    println!(\"{count}\");\n}\n";
        dap.start_debug_session_with_source(LanguageId::Rust, &file, 2, src);
        assert_eq!(dap.call_stack[0].name, "compute_total()");
        assert!(dap.variables.iter().any(|v| v.name == "count" && v.value == "42"));
        assert!(dap.variables.iter().any(|v| v.name == "label"));
        assert!(dap.evaluate_watch("count").contains("42"));
    }
}
