use crate::tools::{
    agent_tool_declarations, execute_workspace_tool, first_line_preview, summarize_tool_args,
};
use crate::types::{AgentWakeupCallback, AiApiConfig, AiApiFormat, StreamingAgentProgress};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn execute_curl_llm_streaming(
    cfg: &AiApiConfig,
    system_prompt: &str,
    user_prompt: &str,
    stream_buf: &Arc<Mutex<Option<StreamingAgentProgress>>>,
    cancel: &Arc<AtomicBool>,
    active_pid: &Arc<Mutex<Option<u32>>>,
    wakeup: Option<&AgentWakeupCallback>,
) -> Result<String, String> {
    let base = cfg.base_url.trim_end_matches('/');
    let (url, headers, body_val) = match cfg.format {
        AiApiFormat::OpenAiCompatible => {
            let endpoint = if base.ends_with("/chat/completions") {
                base.to_string()
            } else {
                format!("{base}/chat/completions")
            };
            let mut hdrs = vec![("Content-Type".to_string(), "application/json".to_string())];
            if !cfg.api_key.trim().is_empty() {
                hdrs.push((
                    "Authorization".to_string(),
                    format!("Bearer {}", cfg.api_key.trim()),
                ));
            }
            let body = json!({
                "model": cfg.model.trim(),
                "stream": true,
                "messages": [
                    { "role": "system", "content": system_prompt },
                    { "role": "user", "content": user_prompt }
                ]
            });
            (endpoint, hdrs, body)
        }
        AiApiFormat::Anthropic => {
            let endpoint = if base.ends_with("/messages") {
                base.to_string()
            } else {
                format!("{base}/messages")
            };
            let hdrs = vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("x-api-key".to_string(), cfg.api_key.trim().to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ];
            let body = json!({
                "model": cfg.model.trim(),
                "max_tokens": 4096,
                "stream": true,
                "system": system_prompt,
                "messages": [
                    { "role": "user", "content": user_prompt }
                ]
            });
            (endpoint, hdrs, body)
        }
        AiApiFormat::Gemini => {
            let endpoint = format!(
                "{}/models/{}:streamGenerateContent?alt=sse&key={}",
                base,
                cfg.model.trim(),
                cfg.api_key.trim()
            );
            let hdrs = vec![("Content-Type".to_string(), "application/json".to_string())];
            let body = json!({
                "systemInstruction": { "parts": [{ "text": system_prompt }] },
                "contents": [
                    { "role": "user", "parts": [{ "text": user_prompt }] }
                ]
            });
            (endpoint, hdrs, body)
        }
    };

    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "--no-buffer", "-X", "POST", &url]);
    for (k, v) in &headers {
        cmd.arg("-H").arg(format!("{k}: {v}"));
    }
    cmd.args(["--data-binary", "@-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 curl.exe: {e}"))?;
    *active_pid.lock() = Some(child.id());

    if let Some(mut stdin) = child.stdin.take() {
        let payload = serde_json::to_vec(&body_val).map_err(|e| e.to_string())?;
        let _ = stdin.write_all(&payload);
        let _ = stdin.flush();
    }

    let stdout = child.stdout.take().ok_or("Missing curl stdout")?;
    let stderr_handle = child.stderr.take().map(|err| {
        thread::spawn(move || {
            let mut s = String::new();
            let _ = BufReader::new(err).read_to_string(&mut s);
            s
        })
    });

    let mut assembled = String::new();
    let mut raw_fallback = String::new();
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();

    while let Ok(n) = reader.read_line(&mut line) {
        if n == 0 || cancel.load(Ordering::SeqCst) {
            break;
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            raw_fallback.push_str(trimmed);
            raw_fallback.push('\n');
            if let Some(data) = trimmed.strip_prefix("data:") {
                let data = data.trim();
                if data != "[DONE]" {
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        let thought_chunk = v["choices"][0]["delta"]["reasoning_content"]
                            .as_str()
                            .or_else(|| v["choices"][0]["delta"]["reasoning"].as_str())
                            .or_else(|| {
                                if v["type"] == "content_block_delta"
                                    && v["delta"]["type"] == "thinking_delta"
                                {
                                    v["delta"]["thinking"].as_str()
                                } else {
                                    None
                                }
                            });
                        if let Some(th) = thought_chunk {
                            StreamingAgentProgress::update_slot(stream_buf, |p| {
                                p.phase = "思考中...".to_string();
                                p.thought_text.push_str(th);
                            });
                            if let Some(cb) = wakeup {
                                cb();
                            }
                        }

                        let chunk = v["choices"][0]["delta"]["content"]
                            .as_str()
                            .or_else(|| v["choices"][0]["message"]["content"].as_str())
                            .or_else(|| {
                                if v["type"] == "content_block_delta" {
                                    v["delta"]["text"].as_str()
                                } else {
                                    None
                                }
                            })
                            .or_else(|| {
                                v["candidates"][0]["content"]["parts"][0]["text"].as_str()
                            });
                        if let Some(delta) = chunk {
                            assembled.push_str(delta);
                            let reply_snapshot = assembled.clone();
                            StreamingAgentProgress::update_slot(stream_buf, |p| {
                                p.phase = "正在生成回复...".to_string();
                                p.reply_text = reply_snapshot;
                            });
                            if let Some(cb) = wakeup {
                                cb();
                            }
                        } else if v["type"] == "error" {
                            if let Some(err_m) = v["error"]["message"].as_str() {
                                return Err(err_m.to_string());
                            }
                        }
                    }
                }
            }
        }
        line.clear();
    }

    let status = child.wait().map_err(|e| e.to_string())?;
    let err_str = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if !assembled.is_empty() {
        return Ok(assembled);
    }

    let raw = raw_fallback.trim();
    if let Ok(v) = serde_json::from_str::<Value>(raw) {
        if let Some(msg) = v["choices"][0]["message"]["content"].as_str() {
            return Ok(msg.to_string());
        }
        if let Some(msg) = v["content"][0]["text"].as_str() {
            return Ok(msg.to_string());
        }
        if let Some(msg) = v["candidates"][0]["content"]["parts"][0]["text"].as_str() {
            return Ok(msg.to_string());
        }
        if let Some(err_msg) = v["error"]["message"].as_str() {
            return Err(err_msg.to_string());
        }
    }

    if !err_str.is_empty() {
        Err(err_str)
    } else if !raw.is_empty() {
        Err(format!("API 返回非预期响应: {raw}"))
    } else if !status.success() {
        Err(format!("curl 退出码: {status}"))
    } else {
        Ok("（空响应）".to_string())
    }
}

fn execute_curl_json_post(
    url: &str,
    headers: &[(String, String)],
    body: &Value,
    active_pid: &Arc<Mutex<Option<u32>>>,
) -> Result<Value, String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-X", "POST", url]);
    for (k, v) in headers {
        cmd.arg("-H").arg(format!("{k}: {v}"));
    }
    cmd.args(["--data-binary", "@-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 curl.exe: {e}"))?;
    *active_pid.lock() = Some(child.id());

    if let Some(mut stdin) = child.stdin.take() {
        let payload = serde_json::to_vec(body).map_err(|e| e.to_string())?;
        let _ = stdin.write_all(&payload);
        let _ = stdin.flush();
    }

    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    *active_pid.lock() = None;
    let out_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let err_str = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if out_str.is_empty() {
        return Err(if !err_str.is_empty() {
            err_str
        } else {
            format!("curl 请求失败 ({})", output.status)
        });
    }

    let v: Value = serde_json::from_str(&out_str)
        .map_err(|e| format!("解析 API JSON 失败: {e}\n原始响应: {out_str}"))?;
    if let Some(err_msg) = v["error"]["message"].as_str() {
        return Err(err_msg.to_string());
    }
    Ok(v)
}

#[allow(clippy::too_many_arguments)]
fn run_tool_step_with_progress(
    root: &Path,
    turn: usize,
    max_turns: usize,
    name: &str,
    args: &Value,
    ckpt_backups: &mut HashMap<PathBuf, Option<String>>,
    modified_inbox: &Arc<Mutex<Vec<PathBuf>>>,
    stream_buf: &Arc<Mutex<Option<StreamingAgentProgress>>>,
    wakeup: Option<&AgentWakeupCallback>,
    step_logs: &mut Vec<String>,
) -> String {
    let summary = summarize_tool_args(args);
    let step_desc = format!("⚙ [{}/{}] {}({})", turn + 1, max_turns, name, summary);
    step_logs.push(step_desc);
    let live_step = if summary.trim().is_empty() {
        format!("执行 {name}")
    } else {
        format!("{name} {summary}")
    };
    StreamingAgentProgress::update_slot(stream_buf, |p| {
        p.phase = format!("正在执行 `{name}`...");
        p.push_execution_step(live_step);
    });
    if let Some(cb) = wakeup {
        cb();
    }

    let out = execute_workspace_tool(root, name, args, ckpt_backups, modified_inbox);
    let preview = first_line_preview(&out);
    step_logs.push(format!("  ✓ {name} -> {preview}"));
    StreamingAgentProgress::update_slot(stream_buf, |p| {
        p.phase = "正在分析结果...".to_string();
        p.push_execution_step(format!("✓ {name} -> {preview}"));
    });
    if let Some(cb) = wakeup {
        cb();
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub fn execute_agent_tool_loop(
    cfg: &AiApiConfig,
    root: &Path,
    system_prompt: &str,
    user_prompt: &str,
    ckpt_backups: &mut HashMap<PathBuf, Option<String>>,
    modified_inbox: &Arc<Mutex<Vec<PathBuf>>>,
    stream_buf: &Arc<Mutex<Option<StreamingAgentProgress>>>,
    cancel: &Arc<AtomicBool>,
    active_pid: &Arc<Mutex<Option<u32>>>,
    wakeup: Option<&AgentWakeupCallback>,
) -> Result<(String, Vec<String>), String> {
    let decls = agent_tool_declarations();
    let mut step_logs: Vec<String> = Vec::new();
    let max_turns = 8usize;

    match cfg.format {
        AiApiFormat::OpenAiCompatible => {
            let base = cfg.base_url.trim_end_matches('/');
            let url = if base.ends_with("/chat/completions") {
                base.to_string()
            } else {
                format!("{base}/chat/completions")
            };
            let mut hdrs = vec![("Content-Type".to_string(), "application/json".to_string())];
            if !cfg.api_key.trim().is_empty() {
                hdrs.push((
                    "Authorization".to_string(),
                    format!("Bearer {}", cfg.api_key.trim()),
                ));
            }
            let tools: Vec<Value> = decls
                .iter()
                .map(|d| json!({ "type": "function", "function": d }))
                .collect();
            let mut msgs = vec![
                json!({ "role": "system", "content": system_prompt }),
                json!({ "role": "user", "content": user_prompt }),
            ];

            for turn in 0..max_turns {
                if cancel.load(Ordering::SeqCst) {
                    return Err("Cancelled".to_string());
                }
                let body = json!({
                    "model": cfg.model.trim(),
                    "messages": msgs,
                    "tools": tools,
                    "tool_choice": "auto"
                });
                let resp = execute_curl_json_post(&url, &hdrs, &body, active_pid)?;
                let msg = &resp["choices"][0]["message"];
                if let Some(rc) = msg["reasoning_content"].as_str().or_else(|| msg["reasoning"].as_str()) {
                    if !rc.trim().is_empty() {
                        StreamingAgentProgress::update_slot(stream_buf, |p| {
                            p.phase = "思考中...".to_string();
                            if !p.thought_text.is_empty() && !p.thought_text.ends_with('\n') {
                                p.thought_text.push('\n');
                            }
                            p.thought_text.push_str(rc.trim());
                        });
                    }
                }
                let content = msg["content"].as_str().unwrap_or("").to_string();
                let tool_calls = msg["tool_calls"].as_array().cloned().unwrap_or_default();

                if tool_calls.is_empty() {
                    return Ok((content, step_logs));
                }

                msgs.push(msg.clone());
                for tc in tool_calls {
                    let call_id = tc["id"].as_str().unwrap_or("call_1");
                    let name = tc["function"]["name"].as_str().unwrap_or("");
                    let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                    let args: Value =
                        serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));

                    let out = run_tool_step_with_progress(
                        root,
                        turn,
                        max_turns,
                        name,
                        &args,
                        ckpt_backups,
                        modified_inbox,
                        stream_buf,
                        wakeup,
                        &mut step_logs,
                    );

                    msgs.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": out
                    }));
                }
            }
            Ok((
                "已达到单轮最大工具调用步数限制，以上为当前阶段执行结果。".to_string(),
                step_logs,
            ))
        }
        AiApiFormat::Anthropic => {
            let base = cfg.base_url.trim_end_matches('/');
            let url = if base.ends_with("/messages") {
                base.to_string()
            } else {
                format!("{base}/messages")
            };
            let hdrs = vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("x-api-key".to_string(), cfg.api_key.trim().to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ];
            let tools: Vec<Value> = decls
                .iter()
                .map(|d| {
                    json!({
                        "name": d["name"],
                        "description": d["description"],
                        "input_schema": d["parameters"]
                    })
                })
                .collect();
            let mut msgs = vec![json!({ "role": "user", "content": user_prompt })];

            for turn in 0..max_turns {
                if cancel.load(Ordering::SeqCst) {
                    return Err("Cancelled".to_string());
                }
                let body = json!({
                    "model": cfg.model.trim(),
                    "max_tokens": 4096,
                    "system": system_prompt,
                    "tools": tools,
                    "messages": msgs
                });
                let resp = execute_curl_json_post(&url, &hdrs, &body, active_pid)?;
                let blocks = resp["content"].as_array().cloned().unwrap_or_default();
                let mut text_parts = Vec::new();
                let mut tool_uses = Vec::new();
                for b in &blocks {
                    match b["type"].as_str() {
                        Some("thinking") => {
                            if let Some(th) = b["thinking"].as_str() {
                                if !th.trim().is_empty() {
                                    StreamingAgentProgress::update_slot(stream_buf, |p| {
                                        p.phase = "思考中...".to_string();
                                        if !p.thought_text.is_empty() && !p.thought_text.ends_with('\n') {
                                            p.thought_text.push('\n');
                                        }
                                        p.thought_text.push_str(th.trim());
                                    });
                                }
                            }
                        }
                        Some("text") => {
                            if let Some(t) = b["text"].as_str() {
                                text_parts.push(t.to_string());
                            }
                        }
                        Some("tool_use") => tool_uses.push(b.clone()),
                        _ => {}
                    }
                }
                if tool_uses.is_empty() {
                    return Ok((text_parts.join("\n"), step_logs));
                }
                msgs.push(json!({ "role": "assistant", "content": blocks }));
                let mut tool_results = Vec::new();
                for tu in tool_uses {
                    let id = tu["id"].as_str().unwrap_or("tu_1");
                    let name = tu["name"].as_str().unwrap_or("");
                    let args = &tu["input"];
                    let out = run_tool_step_with_progress(
                        root,
                        turn,
                        max_turns,
                        name,
                        args,
                        ckpt_backups,
                        modified_inbox,
                        stream_buf,
                        wakeup,
                        &mut step_logs,
                    );
                    tool_results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": out
                    }));
                }
                msgs.push(json!({ "role": "user", "content": tool_results }));
            }
            Ok(("已达到单轮最大工具调用步数限制。".to_string(), step_logs))
        }
        AiApiFormat::Gemini => {
            let base = cfg.base_url.trim_end_matches('/');
            let url = format!(
                "{}/models/{}:generateContent?key={}",
                base,
                cfg.model.trim(),
                cfg.api_key.trim()
            );
            let hdrs = vec![("Content-Type".to_string(), "application/json".to_string())];
            let tools = json!([{ "functionDeclarations": decls }]);
            let mut contents = vec![json!({ "role": "user", "parts": [{ "text": user_prompt }] })];

            for turn in 0..max_turns {
                if cancel.load(Ordering::SeqCst) {
                    return Err("Cancelled".to_string());
                }
                let body = json!({
                    "systemInstruction": { "parts": [{ "text": system_prompt }] },
                    "tools": tools,
                    "contents": contents
                });
                let resp = execute_curl_json_post(&url, &hdrs, &body, active_pid)?;
                let cand_content = &resp["candidates"][0]["content"];
                let parts = cand_content["parts"].as_array().cloned().unwrap_or_default();
                let mut text_parts = Vec::new();
                let mut fn_calls = Vec::new();
                for p in &parts {
                    if p["thought"].as_bool() == Some(true) {
                        if let Some(th) = p["text"].as_str() {
                            StreamingAgentProgress::update_slot(stream_buf, |p| {
                                p.phase = "思考中...".to_string();
                                if !p.thought_text.is_empty() && !p.thought_text.ends_with('\n') {
                                    p.thought_text.push('\n');
                                }
                                p.thought_text.push_str(th.trim());
                            });
                        }
                    } else if let Some(t) = p["text"].as_str() {
                        text_parts.push(t.to_string());
                    }
                    if p.get("functionCall").is_some() {
                        fn_calls.push(p["functionCall"].clone());
                    }
                }
                if fn_calls.is_empty() {
                    return Ok((text_parts.join("\n"), step_logs));
                }
                contents.push(cand_content.clone());
                let mut resp_parts = Vec::new();
                for fc in fn_calls {
                    let name = fc["name"].as_str().unwrap_or("");
                    let args = &fc["args"];
                    let out = run_tool_step_with_progress(
                        root,
                        turn,
                        max_turns,
                        name,
                        args,
                        ckpt_backups,
                        modified_inbox,
                        stream_buf,
                        wakeup,
                        &mut step_logs,
                    );
                    resp_parts.push(json!({
                        "functionResponse": {
                            "name": name,
                            "response": { "result": out }
                        }
                    }));
                }
                contents.push(json!({ "role": "user", "parts": resp_parts }));
            }
            Ok(("已达到单轮最大工具调用步数限制。".to_string(), step_logs))
        }
    }
}
