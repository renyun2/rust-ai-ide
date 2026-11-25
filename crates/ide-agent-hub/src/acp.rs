use crate::tools::resolve_safe_workspace_path;
use crate::types::{AgentWakeupCallback, StreamingAgentProgress};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000 | 0x0000_0400;

/// Google Antigravity / Zed ACP (Agent Client Protocol) JSON-RPC 2.0 标准协议构造与解析器
pub struct AcpProtocol;

impl AcpProtocol {
    pub fn build_initialize_request(id: u64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": {
                        "readTextFile": true,
                        "writeTextFile": true
                    },
                    "terminal": true
                }
            }
        })
    }

    pub fn build_session_new_request(id: u64, cwd: &Path) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/new",
            "params": {
                "cwd": cwd.to_string_lossy(),
                "mcpServers": []
            }
        })
    }

    pub fn build_session_prompt_request(
        id: u64,
        session_id: &str,
        prompt_text: &str,
        active_file: Option<&Path>,
    ) -> Value {
        let mut prompt_blocks = vec![json!({
            "type": "text",
            "text": prompt_text
        })];
        if let Some(path) = active_file {
            prompt_blocks.push(json!({
                "type": "resource_link",
                "uri": format!("file:///{}", path.to_string_lossy().replace('\\', "/")),
                "name": path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
            }));
        }
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": prompt_blocks
            }
        })
    }
}

/// 真实 ACP (Agent Client Protocol) JSON-RPC 2.0 Stdio 客户端（支持流式更新 + 服务端读写文件工具回调）
pub fn execute_acp_stdio_session(
    acp_cmd_line: &str,
    root: &Path,
    prompt_text: &str,
    active_file: Option<&Path>,
    stream_buf: &Arc<Mutex<Option<StreamingAgentProgress>>>,
    active_pid: &Arc<Mutex<Option<u32>>>,
    modified_inbox: Option<&Arc<Mutex<Vec<PathBuf>>>>,
    wakeup: Option<&AgentWakeupCallback>,
) -> Result<(String, Vec<PathBuf>), String> {
    let parts: Vec<&str> = acp_cmd_line.split_whitespace().collect();
    if parts.is_empty() {
        return Err("ACP 命令为空".to_string());
    }
    crate::cli_discovery::suppress_agy_background_updater_window();
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let lower = parts[0].to_ascii_lowercase();
        if lower.ends_with(".cmd") || lower.ends_with(".bat") {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(parts[0]);
            if parts.len() > 1 {
                c.args(&parts[1..]);
            }
            c
        } else {
            let mut c = Command::new(parts[0]);
            if parts.len() > 1 {
                c.args(&parts[1..]);
            }
            c
        }
    };
    #[cfg(not(target_os = "windows"))]
    let mut cmd = {
        let mut c = Command::new(parts[0]);
        if parts.len() > 1 {
            c.args(&parts[1..]);
        }
        c
    };
    cmd.current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 ACP Agent `{acp_cmd_line}`: {e}"))?;
    *active_pid.lock() = Some(child.id());

    let mut stdin = child.stdin.take().ok_or("Missing ACP stdin")?;
    let stdout = child.stdout.take().ok_or("Missing ACP stdout")?;
    let mut reader = BufReader::new(stdout);

    let send_json = |stdin: &mut std::process::ChildStdin, val: &Value| -> Result<(), String> {
        let mut s = serde_json::to_string(val).map_err(|e| e.to_string())?;
        s.push('\n');
        stdin.write_all(s.as_bytes()).map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())
    };

    let init_req = AcpProtocol::build_initialize_request(1);
    send_json(&mut stdin, &init_req)?;

    let mut prompt_sent = false;
    let mut assembled = String::new();
    let mut line = String::new();
    let mut session_modified = Vec::new();

    while let Ok(n) = reader.read_line(&mut line) {
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            if let Ok(msg) = serde_json::from_str::<Value>(trimmed) {
                let id = msg.get("id").and_then(|v| v.as_u64());
                let method = msg.get("method").and_then(|v| v.as_str());

                if id == Some(1) && method.is_none() {
                    if let Some(err) = msg.get("error") {
                        let _ = child.kill();
                        return Err(format!("ACP initialize error: {err}"));
                    }
                    let new_req = AcpProtocol::build_session_new_request(2, root);
                    send_json(&mut stdin, &new_req)?;
                } else if id == Some(2) && method.is_none() {
                    if let Some(err) = msg.get("error") {
                        let _ = child.kill();
                        return Err(format!("ACP session/new error: {err}"));
                    }
                    let session_id = msg["result"]["sessionId"]
                        .as_str()
                        .unwrap_or("default")
                        .to_string();
                    if !prompt_sent {
                        prompt_sent = true;
                        let prompt_req = AcpProtocol::build_session_prompt_request(
                            3,
                            &session_id,
                            prompt_text,
                            active_file,
                        );
                        send_json(&mut stdin, &prompt_req)?;
                    }
                } else if id == Some(3) && method.is_none() {
                    if let Some(err) = msg.get("error") {
                        let _ = child.kill();
                        return Err(format!("ACP session/prompt error: {err}"));
                    }
                    if assembled.is_empty() {
                        if let Some(text) = msg["result"]["output"].as_str() {
                            assembled.push_str(text);
                        }
                    }
                    break;
                } else if let Some(m) = method {
                    if m == "session/update" {
                        let update = &msg["params"]["update"];
                        let su_kind = update["sessionUpdate"].as_str().unwrap_or("");
                        let txt = update["content"]["text"]
                            .as_str()
                            .or_else(|| update["text"].as_str());
                        if su_kind.contains("thought") {
                            if let Some(t) = txt {
                                StreamingAgentProgress::update_slot(stream_buf, |p| {
                                    p.phase = "思考中...".to_string();
                                    p.thought_text.push_str(t);
                                });
                                if let Some(cb) = wakeup {
                                    cb();
                                }
                            }
                        } else if su_kind.contains("tool") || su_kind == "plan" {
                            let label = update["title"]
                                .as_str()
                                .or_else(|| update["toolCallId"].as_str())
                                .or(txt)
                                .unwrap_or("执行工具步骤");
                            StreamingAgentProgress::update_slot(stream_buf, |p| {
                                p.phase = "正在执行步骤...".to_string();
                                p.push_execution_step(label);
                            });
                            if let Some(cb) = wakeup {
                                cb();
                            }
                        } else if let Some(chunk) = txt {
                            assembled.push_str(chunk);
                            let reply_snapshot = assembled.clone();
                            StreamingAgentProgress::update_slot(stream_buf, |p| {
                                p.phase = "正在生成回复...".to_string();
                                p.reply_text = reply_snapshot;
                            });
                            if let Some(cb) = wakeup {
                                cb();
                            }
                        }
                    } else if m == "fs/read_text_file" {
                        if let Some(req_id) = msg.get("id") {
                            let path_str = msg["params"]["path"].as_str().unwrap_or("");
                            StreamingAgentProgress::update_slot(stream_buf, |p| {
                                p.phase = "正在读取工作区文件...".to_string();
                                p.push_execution_step(format!("读取 {path_str}"));
                            });
                            if let Some(cb) = wakeup {
                                cb();
                            }
                            let resp = match resolve_safe_workspace_path(root, path_str)
                                .and_then(|p| fs::read_to_string(&p).map_err(|e| e.to_string()))
                            {
                                Ok(content) => json!({"jsonrpc": "2.0", "id": req_id, "result": {"content": content}}),
                                Err(e) => json!({"jsonrpc": "2.0", "id": req_id, "error": {"code": -32000, "message": e}}),
                            };
                            let _ = send_json(&mut stdin, &resp);
                        }
                    } else if m == "fs/write_text_file" {
                        if let Some(req_id) = msg.get("id") {
                            let path_str = msg["params"]["path"].as_str().unwrap_or("");
                            let content = msg["params"]["content"].as_str().unwrap_or("");
                            let resp = match resolve_safe_workspace_path(root, path_str).and_then(|p| {
                                if let Some(parent) = p.parent() {
                                    let _ = fs::create_dir_all(parent);
                                }
                                fs::write(&p, content).map_err(|e| e.to_string())?;
                                if !session_modified.contains(&p) {
                                    session_modified.push(p.clone());
                                }
                                if let Some(inbox) = modified_inbox {
                                    inbox.lock().push(p);
                                }
                                Ok(())
                            }) {
                                Ok(()) => json!({"jsonrpc": "2.0", "id": req_id, "result": Value::Null}),
                                Err(e) => json!({"jsonrpc": "2.0", "id": req_id, "error": {"code": -32000, "message": e}}),
                            };
                            let _ = send_json(&mut stdin, &resp);
                        }
                    }
                }
            }
        }
        line.clear();
    }

    let _ = child.kill();
    let _ = child.wait();
    if assembled.trim().is_empty() {
        Err("ACP 会话未返回有效文本内容".to_string())
    } else {
        Ok((assembled, session_modified))
    }
}
