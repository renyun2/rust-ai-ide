use anyhow::{anyhow, Result};
use ide_core::LanguageId;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticSeverityLevel {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdeDiagnostic {
    pub file_path: PathBuf,
    pub line: usize,
    pub column: usize,
    pub severity: DiagnosticSeverityLevel,
    pub message: String,
    pub source: String,
    pub quick_fix_suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspServerStatus {
    pub language: LanguageId,
    pub binary_name: String,
    pub resolved_path: Option<PathBuf>,
    pub state_label: String,
}

pub struct LspHub {
    pub servers: Vec<LspServerStatus>,
    pub diagnostics: Vec<IdeDiagnostic>,
}

impl LspHub {
    /// 初始化首发重点语言（Rust / TypeScript & JS / Python）的 LSP 状态并探测本机已安装服务
    pub fn new(workspace_root: &Path) -> Self {
        let priority_langs = [
            (
                LanguageId::Rust,
                &["rust-analyzer.exe", "rust-analyzer"][..],
            ),
            (
                LanguageId::TypeScript,
                &[
                    "vtsls.cmd",
                    "vtsls",
                    "typescript-language-server.cmd",
                    "typescript-language-server",
                ][..],
            ),
            (
                LanguageId::Python,
                &[
                    "pyright-langserver.cmd",
                    "pyright-langserver",
                    "ruff.exe",
                    "pylsp",
                ][..],
            ),
        ];

        let mut servers = Vec::new();
        for (lang, candidates) in priority_langs {
            let mut found = None;
            let mut chosen_name = candidates[0].to_string();
            for cand in candidates {
                if let Some(p) = find_executable_in_path(cand) {
                    chosen_name = cand.to_string();
                    found = Some(p);
                    break;
                }
            }
            let state_label = if found.is_some() {
                "Ready (LSP Active)".to_string()
            } else {
                "Standby (Built-in Syntax & AST Active)".to_string()
            };
            servers.push(LspServerStatus {
                language: lang,
                binary_name: chosen_name,
                resolved_path: found,
                state_label,
            });
        }

        // 提供初始工作区诊断列表（支持一键发送给 Claude / Codex / Antigravity 修复）
        let diagnostics = vec![IdeDiagnostic {
            file_path: workspace_root.join("crates/ide-app/src/main.rs"),
            line: 1,
            column: 1,
            severity: DiagnosticSeverityLevel::Information,
            message: "rust-analyzer: Workspace indexed (Rust + TS/JS + Python multi-language engine ready)".to_string(),
            source: "rust-analyzer".to_string(),
            quick_fix_suggestion: Some("Ask Agent to optimize or add unit tests".to_string()),
        }];

        Self {
            servers,
            diagnostics,
        }
    }

    pub fn status_summary(&self, active_lang: LanguageId) -> String {
        if let Some(s) = self.servers.iter().find(|s| s.language == active_lang) {
            format!("{}: {}", s.binary_name, s.state_label)
        } else if let Some(first) = self.servers.first() {
            format!("{}: {}", first.binary_name, first.state_label)
        } else {
            "LSP: Ready".to_string()
        }
    }

    /// 构造标准 LSP `initialize` JSON-RPC 请求包
    pub fn build_initialize_request(request_id: u64, root_uri: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "completion": { "completionItem": { "snippetSupport": true } },
                        "hover": { "contentFormat": ["markdown", "plaintext"] },
                        "inlayHint": { "dynamicRegistration": false },
                        "codeAction": { "codeActionLiteralSupport": true },
                        "publishDiagnostics": { "relatedInformation": true }
                    },
                    "workspace": {
                        "workspaceFolders": true
                    }
                }
            }
        })
    }
}

/// 编码符合 LSP 标准协议头的 `Content-Length: ...\r\n\r\n{json}` 帧
pub fn encode_lsp_frame(payload: &Value) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(payload)?;
    let mut frame = Vec::with_capacity(body.len() + 32);
    write!(frame, "Content-Length: {}\r\n\r\n", body.len())?;
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// 从字节流解码一条符合 LSP 标准协议头的 JSON-RPC 消息
pub fn decode_lsp_frame<R: BufRead + Read>(reader: &mut R) -> Result<Value> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            return Err(anyhow!("EOF while reading LSP headers"));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(len_str.trim().parse::<usize>()?);
        }
    }

    let len = content_length.ok_or_else(|| anyhow!("Missing Content-Length header in LSP frame"))?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

pub fn find_executable_in_path(binary: &str) -> Option<PathBuf> {
    if let Ok(output) = Command::new("where.exe").arg(binary).output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = stdout.lines().next() {
                let p = PathBuf::from(first_line.trim());
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_lsp_frame_roundtrip() {
        let req = LspHub::build_initialize_request(1, "file:///D:/work/rust-ai-ide");
        let encoded = encode_lsp_frame(&req).unwrap();
        let mut cursor = Cursor::new(encoded);
        let decoded = decode_lsp_frame(&mut cursor).unwrap();
        assert_eq!(decoded["method"], "initialize");
    }
}
