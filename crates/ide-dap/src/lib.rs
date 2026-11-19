use ide_core::LanguageId;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
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
    pub variables: Vec<DebugVariable>,
    pub watches: Vec<String>,
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
            variables: Vec::new(),
            watches: vec![
                "self.active_agent".to_string(),
                "workspace.dirty_buffers()".to_string(),
            ],
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

    pub fn start_debug_session(&mut self, lang: LanguageId, file: &Path, cursor_line: usize) {
        self.active_adapter = lang
            .default_dap_adapter()
            .unwrap_or("codelldb")
            .to_string();

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

        self.call_stack = vec![
            StackFrameItem {
                id: 1,
                name: format!("{}::active_frame()", lang.display_name()),
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

        self.variables = vec![
            DebugVariable {
                name: "language".to_string(),
                type_name: "LanguageId".to_string(),
                value: format!("{:?}", lang),
            },
            DebugVariable {
                name: "adapter".to_string(),
                type_name: "&str".to_string(),
                value: format!("\"{}\"", self.active_adapter),
            },
            DebugVariable {
                name: "breakpoint_line".to_string(),
                type_name: "usize".to_string(),
                value: (hit_line + 1).to_string(),
            },
        ];
    }

    pub fn step_over(&mut self) {
        if let DebugSessionState::PausedAtBreakpoint { file, line } = &mut self.state {
            *line += 1;
            if let Some(top) = self.call_stack.first_mut() {
                top.line = *line + 1;
            }
            let _ = file;
        }
    }

    pub fn stop(&mut self) {
        self.state = DebugSessionState::Inactive;
        self.call_stack.clear();
        self.variables.clear();
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
        out
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
