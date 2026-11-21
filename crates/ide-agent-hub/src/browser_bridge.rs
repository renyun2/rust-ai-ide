use crate::cli_discovery::{resolve_cursor_node_entry, strip_json_trailing_commas, user_home_dir};
use ide_lsp::find_executable_in_path;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

const BRIDGE_SCRIPT_FILENAME: &str = "chrome-devtools-persistent-mcp.mjs";

/// Lightweight stdio MCP proxy for `chrome-devtools-mcp`.
///
/// Why this exists:
/// When an AI CLI such as `cursor-agent -p` finishes a turn, it closes stdin of its
/// stdio MCP servers. In default (launch) mode, `chrome-devtools-mcp` responds to
/// shutdown by calling `browser.close()`, immediately closing the user's Chrome
/// window. When `chrome-devtools-mcp` runs with `--browserUrl`, its shutdown hook
/// only calls `browser.disconnect()`, leaving Chrome open for future turns and
/// future sessions.
///
/// This proxy keeps startup lazy (never opens Chrome during `initialize` or
/// `tools/list`), launches a detached persistent Chrome instance with
/// `--remote-debugging-port` on the first `tools/call` if one is not already
/// listening, and forwards all JSON-RPC traffic to `chrome-devtools-mcp`.
pub const CHROME_DEVTOOLS_BRIDGE_SCRIPT: &str = r#"#!/usr/bin/env node
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import readline from 'node:readline';
import { fileURLToPath } from 'node:url';

const DEFAULT_DEBUG_PORT = 9222;
const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const PORT_STATE_FILE = path.join(SCRIPT_DIR, 'chrome-debug-port.txt');
const USER_DATA_DIR =
  process.env.RUST_AI_IDE_CHROME_USER_DATA_DIR ||
  path.join(os.homedir(), '.cache', 'chrome-devtools-mcp', 'chrome-profile');

const STRIP_FLAGS_WITH_VALUE = new Set([
  '--browserUrl',
  '--browser-url',
  '-u',
  '--wsEndpoint',
  '--ws-endpoint',
  '-w',
]);
const STRIP_BOOLEAN_FLAGS = new Set([
  '--autoConnect',
  '--auto-connect',
  '--isolated',
]);

function filterPassthroughArgs(rawArgs) {
  const out = [];
  for (let i = 0; i < rawArgs.length; i += 1) {
    const arg = rawArgs[i];
    if (STRIP_BOOLEAN_FLAGS.has(arg)) {
      continue;
    }
    if (STRIP_FLAGS_WITH_VALUE.has(arg)) {
      i += 1;
      continue;
    }
    if (
      arg.startsWith('--browserUrl=') ||
      arg.startsWith('--browser-url=') ||
      arg.startsWith('--wsEndpoint=') ||
      arg.startsWith('--ws-endpoint=') ||
      arg.startsWith('--autoConnect=') ||
      arg.startsWith('--auto-connect=') ||
      arg.startsWith('--isolated=')
    ) {
      continue;
    }
    out.push(arg);
  }
  return out;
}

function httpGetJson(url, timeoutMs = 450) {
  return new Promise((resolve) => {
    const req = http.get(url, (res) => {
      if (res.statusCode !== 200) {
        res.resume();
        resolve(null);
        return;
      }
      let body = '';
      res.setEncoding('utf8');
      res.on('data', (chunk) => {
        body += chunk;
      });
      res.on('end', () => {
        try {
          resolve(JSON.parse(body));
        } catch {
          resolve(null);
        }
      });
    });
    req.setTimeout(timeoutMs, () => {
      req.destroy();
      resolve(null);
    });
    req.on('error', () => resolve(null));
  });
}

async function probeCdpPort(port) {
  const version = await httpGetJson(`http://127.0.0.1:${port}/json/version`, 450);
  return Boolean(version && (version.webSocketDebuggerUrl || version.Browser));
}

function isTcpPortOpen(port, timeoutMs = 250) {
  return new Promise((resolve) => {
    const socket = net.createConnection({ host: '127.0.0.1', port });
    let settled = false;
    const finish = (open) => {
      if (!settled) {
        settled = true;
        socket.destroy();
        resolve(open);
      }
    };
    socket.setTimeout(timeoutMs);
    socket.once('connect', () => finish(true));
    socket.once('timeout', () => finish(false));
    socket.once('error', () => finish(false));
  });
}

function findFreePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.unref();
    srv.on('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const addr = srv.address();
      const port = typeof addr === 'object' && addr ? addr.port : DEFAULT_DEBUG_PORT;
      srv.close(() => resolve(port));
    });
  });
}

function readSavedPort() {
  try {
    const raw = fs.readFileSync(PORT_STATE_FILE, 'utf8').trim();
    const parsed = Number.parseInt(raw, 10);
    if (Number.isInteger(parsed) && parsed > 0 && parsed <= 65535) {
      return parsed;
    }
  } catch {
    // ignore missing or unreadable state file
  }
  return null;
}

function writeSavedPort(port) {
  try {
    fs.mkdirSync(path.dirname(PORT_STATE_FILE), { recursive: true });
    fs.writeFileSync(PORT_STATE_FILE, String(port), 'utf8');
  } catch {
    // non-fatal
  }
}

async function resolveDebugPort() {
  const envPortRaw = process.env.RUST_AI_IDE_CHROME_DEBUG_PORT;
  if (envPortRaw) {
    const parsed = Number.parseInt(envPortRaw.trim(), 10);
    if (Number.isInteger(parsed) && parsed > 0 && parsed <= 65535) {
      return parsed;
    }
  }

  const candidates = [];
  const saved = readSavedPort();
  if (saved) {
    candidates.push(saved);
  }
  if (!candidates.includes(DEFAULT_DEBUG_PORT)) {
    candidates.push(DEFAULT_DEBUG_PORT);
  }

  for (const port of candidates) {
    if (await probeCdpPort(port)) {
      return port;
    }
  }

  if (!(await isTcpPortOpen(DEFAULT_DEBUG_PORT))) {
    writeSavedPort(DEFAULT_DEBUG_PORT);
    return DEFAULT_DEBUG_PORT;
  }

  const freePort = await findFreePort();
  writeSavedPort(freePort);
  return freePort;
}

function readRegistryAppPath(hiveAndKey) {
  try {
    const out = execFileSync('reg', ['query', hiveAndKey, '/ve'], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      windowsHide: true,
    });
    for (const line of out.split(/\r?\n/)) {
      const match = line.match(/REG_(?:SZ|EXPAND_SZ)\s+(.+)$/);
      if (match && match[1]) {
        const candidate = match[1].trim().replace(/^"|"$/g, '');
        if (candidate && fs.existsSync(candidate)) {
          return candidate;
        }
      }
    }
  } catch {
    // ignore missing registry entry
  }
  return null;
}

function findChromeExecutable() {
  const override = process.env.RUST_AI_IDE_CHROME_EXECUTABLE;
  if (override && fs.existsSync(override)) {
    return override;
  }

  if (process.platform === 'win32') {
    const regKeys = [
      'HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\chrome.exe',
      'HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\chrome.exe',
    ];
    for (const key of regKeys) {
      const hit = readRegistryAppPath(key);
      if (hit) {
        return hit;
      }
    }

    const pf = process.env.ProgramFiles || 'C:\\Program Files';
    const pf86 = process.env['ProgramFiles(x86)'] || 'C:\\Program Files (x86)';
    const localAppData =
      process.env.LOCALAPPDATA || path.join(os.homedir(), 'AppData', 'Local');
    const candidates = [
      path.join(pf, 'Google', 'Chrome', 'Application', 'chrome.exe'),
      path.join(pf86, 'Google', 'Chrome', 'Application', 'chrome.exe'),
      path.join(localAppData, 'Google', 'Chrome', 'Application', 'chrome.exe'),
      path.join(pf86, 'Microsoft', 'Edge', 'Application', 'msedge.exe'),
      path.join(pf, 'Microsoft', 'Edge', 'Application', 'msedge.exe'),
      path.join(localAppData, 'Microsoft', 'Edge', 'Application', 'msedge.exe'),
    ];
    for (const candidate of candidates) {
      if (fs.existsSync(candidate)) {
        return candidate;
      }
    }
    return null;
  }

  if (process.platform === 'darwin') {
    const candidates = [
      '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
      '/Applications/Chromium.app/Contents/MacOS/Chromium',
      '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
    ];
    for (const candidate of candidates) {
      if (fs.existsSync(candidate)) {
        return candidate;
      }
    }
    return null;
  }

  const linuxCandidates = [
    'google-chrome',
    'google-chrome-stable',
    'chromium',
    'chromium-browser',
    'microsoft-edge',
  ];
  for (const bin of linuxCandidates) {
    try {
      const found = execFileSync('which', [bin], {
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
      }).trim();
      if (found) {
        return found.split(/\r?\n/)[0];
      }
    } catch {
      // continue
    }
  }
  return null;
}

function resolveBinFromPackageDir(pkgDir) {
  const pkgJsonPath = path.join(pkgDir, 'package.json');
  if (!fs.existsSync(pkgJsonPath)) {
    return null;
  }
  try {
    const pkg = JSON.parse(fs.readFileSync(pkgJsonPath, 'utf8'));
    const relBin =
      (pkg.bin && pkg.bin['chrome-devtools-mcp']) ||
      'out/Default/gen/src/bin/chrome-devtools-mcp.js';
    const full = path.resolve(pkgDir, relBin);
    if (fs.existsSync(full)) {
      const mtimeMs = fs.statSync(pkgJsonPath).mtimeMs;
      return { path: full, mtimeMs };
    }
  } catch {
    // ignore malformed package.json
  }
  return null;
}

function findLocalChromeDevtoolsMcpEntry() {
  const override = process.env.RUST_AI_IDE_CHROME_DEVTOOLS_MCP_ENTRY;
  if (override && fs.existsSync(override)) {
    return override;
  }

  const hits = [];
  const npxRoots = [];
  const globalPkgDirs = [];

  if (process.platform === 'win32') {
    const localAppData =
      process.env.LOCALAPPDATA || path.join(os.homedir(), 'AppData', 'Local');
    const appData =
      process.env.APPDATA || path.join(os.homedir(), 'AppData', 'Roaming');
    npxRoots.push(path.join(localAppData, 'npm-cache', '_npx'));
    globalPkgDirs.push(path.join(appData, 'npm', 'node_modules', 'chrome-devtools-mcp'));
  } else {
    npxRoots.push(path.join(os.homedir(), '.npm', '_npx'));
    globalPkgDirs.push('/usr/local/lib/node_modules/chrome-devtools-mcp');
    globalPkgDirs.push('/usr/lib/node_modules/chrome-devtools-mcp');
  }

  for (const dir of globalPkgDirs) {
    const hit = resolveBinFromPackageDir(dir);
    if (hit) {
      hits.push(hit);
    }
  }

  for (const root of npxRoots) {
    if (!fs.existsSync(root)) {
      continue;
    }
    try {
      for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
        if (!entry.isDirectory()) {
          continue;
        }
        const pkgDir = path.join(root, entry.name, 'node_modules', 'chrome-devtools-mcp');
        const hit = resolveBinFromPackageDir(pkgDir);
        if (hit) {
          hits.push(hit);
        }
      }
    } catch {
      // ignore cache read errors
    }
  }

  hits.sort((a, b) => b.mtimeMs - a.mtimeMs);
  return hits.length > 0 ? hits[0].path : null;
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

let inFlightLaunch = null;

async function ensureChromeRunning(port) {
  if (await probeCdpPort(port)) {
    return;
  }
  if (inFlightLaunch) {
    return await inFlightLaunch;
  }
  inFlightLaunch = (async () => {
    try {
      if (await probeCdpPort(port)) {
        return;
      }
      const chromeExe = findChromeExecutable();
      if (!chromeExe) {
        throw new Error(
          'No Chrome/Edge executable found. Set RUST_AI_IDE_CHROME_EXECUTABLE or install Google Chrome.',
        );
      }
      fs.mkdirSync(USER_DATA_DIR, { recursive: true });
      const chromeArgs = [
        `--remote-debugging-port=${port}`,
        `--user-data-dir=${USER_DATA_DIR}`,
        '--no-first-run',
        '--no-default-browser-check',
        '--hide-crash-restore-bubble',
        '--disable-backgrounding-occluded-windows',
      ];
      let chromeExited = false;
      const chromeProc = spawn(chromeExe, chromeArgs, {
        detached: true,
        stdio: 'ignore',
        windowsHide: false,
      });
      chromeProc.once('exit', () => {
        chromeExited = true;
      });
      chromeProc.unref();

      const deadline = Date.now() + 15_000;
      while (Date.now() < deadline) {
        if (await probeCdpPort(port)) {
          const pages = await httpGetJson(`http://127.0.0.1:${port}/json/list`, 450);
          if (Array.isArray(pages)) {
            writeSavedPort(port);
            return;
          }
        }
        if (chromeExited && !(await probeCdpPort(port))) {
          // Chrome on Windows/macOS sometimes hands off to an already-running
          // instance sharing the same user-data-dir and exits quickly; keep polling briefly.
          await delay(150);
        } else {
          await delay(100);
        }
      }
      throw new Error(`Timed out waiting for Chrome DevTools on http://127.0.0.1:${port}`);
    } finally {
      inFlightLaunch = null;
    }
  })();
  return await inFlightLaunch;
}

function isToolCallMessage(line) {
  const trimmed = line.trim();
  if (!trimmed.startsWith('{') && !trimmed.startsWith('[')) {
    return false;
  }
  try {
    const parsed = JSON.parse(trimmed);
    if (Array.isArray(parsed)) {
      return parsed.some((item) => item && item.method === 'tools/call');
    }
    return Boolean(parsed && parsed.method === 'tools/call');
  } catch {
    return false;
  }
}

function spawnChromeDevtoolsMcp(port, passthroughArgs) {
  const args = [
    ...filterPassthroughArgs(passthroughArgs),
    '--browserUrl',
    `http://127.0.0.1:${port}`,
  ];
  const localEntry = findLocalChromeDevtoolsMcpEntry();
  if (localEntry) {
    return spawn(process.execPath, [localEntry, ...args], {
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    });
  }
  if (process.platform === 'win32') {
    return spawn('cmd.exe', ['/d', '/c', 'npx', '-y', 'chrome-devtools-mcp@latest', ...args], {
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    });
  }
  return spawn('npx', ['-y', 'chrome-devtools-mcp@latest', ...args], {
    stdio: ['pipe', 'pipe', 'pipe'],
  });
}

async function main() {
  const passthroughArgs = process.argv.slice(2);
  const rl = readline.createInterface({
    input: process.stdin,
    crlfDelay: Infinity,
  });

  const serverReadyPromise = (async () => {
    const port = await resolveDebugPort();
    const child = spawnChromeDevtoolsMcp(port, passthroughArgs);
    child.stdout.pipe(process.stdout);
    child.stderr.pipe(process.stderr);
    child.on('exit', (code) => {
      process.exit(code ?? 0);
    });
    child.on('error', (err) => {
      process.stderr.write(`[chrome-devtools-persistent-mcp] spawn error: ${err.message}\n`);
      process.exit(1);
    });
    return { child, port };
  })();

  let queue = Promise.resolve();

  rl.on('line', (line) => {
    queue = queue.then(async () => {
      const { child, port } = await serverReadyPromise;
      if (isToolCallMessage(line)) {
        try {
          await ensureChromeRunning(port);
        } catch (err) {
          process.stderr.write(
            `[chrome-devtools-persistent-mcp] failed to ensure Chrome is running: ${err?.message || err}\n`,
          );
        }
      }
      if (!child.stdin.destroyed && child.stdin.writable) {
        child.stdin.write(`${line}\n`);
      }
    });
  });

  const closeChildStdin = async () => {
    await queue;
    const { child } = await serverReadyPromise;
    if (!child.stdin.destroyed) {
      child.stdin.end();
    }
  };

  rl.on('close', () => {
    void closeChildStdin();
  });

  for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    process.on(sig, () => {
      void serverReadyPromise.then(({ child }) => {
        if (!child.killed) {
          child.kill(sig);
        }
      });
    });
  }
}

void main().catch((err) => {
  process.stderr.write(`[chrome-devtools-persistent-mcp] fatal: ${err?.stack || err}\n`);
  process.exit(1);
});
"#;

/// Returns the app storage directory (`%APPDATA%\rust-ai-ide` on Windows,
/// `$XDG_CONFIG_HOME/rust-ai-ide` or `~/.config/rust-ai-ide` on Unix).
pub fn default_agent_support_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .or_else(|| std::env::var_os("LOCALAPPDATA"))
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|p| PathBuf::from(p).join("rust-ai-ide"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|p| p.join("rust-ai-ide"))
    }
}

/// Ensures the persistent Chrome DevTools MCP bridge script exists on disk and is
/// up to date, returning its path.
pub fn ensure_bridge_script_at(support_dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(support_dir)
        .map_err(|e| format!("创建浏览器桥接目录失败 ({}): {e}", support_dir.display()))?;
    let script_path = support_dir.join(BRIDGE_SCRIPT_FILENAME);
    let needs_write = match fs::read_to_string(&script_path) {
        Ok(existing) => existing != CHROME_DEVTOOLS_BRIDGE_SCRIPT,
        Err(_) => true,
    };
    if needs_write {
        fs::write(&script_path, CHROME_DEVTOOLS_BRIDGE_SCRIPT)
            .map_err(|e| format!("写入浏览器桥接脚本失败 ({}): {e}", script_path.display()))?;
    }
    Ok(script_path)
}

fn tokenize_command_line(entry: &Value) -> Vec<String> {
    let mut tokens = Vec::new();
    if let Some(cmd) = entry.get("command").and_then(|v| v.as_str()) {
        tokens.extend(cmd.split_whitespace().map(|s| s.to_string()));
    }
    if let Some(args) = entry.get("args").and_then(|v| v.as_array()) {
        for a in args {
            if let Some(s) = a.as_str() {
                tokens.push(s.to_string());
            }
        }
    }
    tokens
}

fn is_chrome_devtools_mcp_entry(tokens: &[String]) -> bool {
    tokens.iter().any(|tok| {
        let lower = tok.to_ascii_lowercase();
        lower == "chrome-devtools-mcp"
            || lower.starts_with("chrome-devtools-mcp@")
            || lower.contains("/chrome-devtools-mcp")
            || lower.contains("\\chrome-devtools-mcp")
            || lower.ends_with(BRIDGE_SCRIPT_FILENAME)
    })
}

fn has_explicit_connection_or_isolation_flag(tokens: &[String]) -> bool {
    tokens.iter().any(|tok| {
        let lower = tok.to_ascii_lowercase();
        lower == "--browserurl"
            || lower == "--browser-url"
            || lower == "-u"
            || lower.starts_with("--browserurl=")
            || lower.starts_with("--browser-url=")
            || lower == "--wsendpoint"
            || lower == "--ws-endpoint"
            || lower == "-w"
            || lower.starts_with("--wsendpoint=")
            || lower.starts_with("--ws-endpoint=")
            || lower == "--autoconnect"
            || lower == "--auto-connect"
            || lower.starts_with("--autoconnect=")
            || lower.starts_with("--auto-connect=")
            || lower == "--isolated"
            || lower.starts_with("--isolated=")
    })
}

fn extract_preserved_mcp_args(tokens: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        let lower = tok.to_ascii_lowercase();
        let file_name = Path::new(tok)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(tok.as_str())
            .to_ascii_lowercase();

        let is_wrapper = matches!(
            file_name.as_str(),
            "cmd" | "cmd.exe" | "npx" | "npx.cmd" | "node" | "node.exe"
        ) || matches!(lower.as_str(), "/d" | "/c" | "-y" | "--yes")
            || lower == "chrome-devtools-mcp"
            || lower.starts_with("chrome-devtools-mcp@")
            || lower.ends_with("chrome-devtools-mcp.js")
            || lower.ends_with(BRIDGE_SCRIPT_FILENAME);

        if is_wrapper {
            i += 1;
            continue;
        }
        out.push(tok.clone());
        i += 1;
    }
    out
}

/// Updates `~/.cursor/mcp.json` (when `chrome-devtools` is configured as stock
/// `chrome-devtools-mcp` without explicit `--browserUrl`/`--wsEndpoint`/`--autoConnect`/
/// `--isolated`) to invoke the persistent bridge script so Chrome stays open across
/// Cursor CLI turns and sessions.
///
/// Returns `Ok(true)` if the file was modified, `Ok(false)` if no update was needed.
pub fn sync_cursor_mcp_persistent_chrome_config(
    mcp_json_path: &Path,
    bridge_script_path: &Path,
    node_command: &str,
) -> Result<bool, String> {
    if !mcp_json_path.is_file() {
        return Ok(false);
    }
    let raw = fs::read_to_string(mcp_json_path)
        .map_err(|e| format!("读取 {} 失败: {e}", mcp_json_path.display()))?;
    let cleaned = strip_json_trailing_commas(&raw);
    let mut root: Value = serde_json::from_str(&cleaned)
        .map_err(|e| format!("解析 {} 失败: {e}", mcp_json_path.display()))?;

    let Some(servers) = root.get_mut("mcpServers").and_then(|v| v.as_object_mut()) else {
        return Ok(false);
    };
    let Some(entry) = servers.get_mut("chrome-devtools") else {
        return Ok(false);
    };

    let tokens = tokenize_command_line(entry);
    if !is_chrome_devtools_mcp_entry(&tokens) || has_explicit_connection_or_isolation_flag(&tokens) {
        return Ok(false);
    }

    let bridge_str = bridge_script_path.to_string_lossy().into_owned();
    let preserved_args = extract_preserved_mcp_args(&tokens);
    let mut new_args = Vec::with_capacity(1 + preserved_args.len());
    new_args.push(bridge_str.clone());
    new_args.extend(preserved_args);

    let already_synced = entry.get("command").and_then(|v| v.as_str()) == Some(node_command)
        && entry
            .get("args")
            .and_then(|v| v.as_array())
            .is_some_and(|arr| {
                arr.len() == new_args.len()
                    && arr
                        .iter()
                        .zip(new_args.iter())
                        .all(|(a, b)| a.as_str() == Some(b.as_str()))
            });
    if already_synced {
        return Ok(false);
    }

    let existing_env = entry
        .get("env")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_else(Map::new);

    let mut obj = entry.as_object().cloned().unwrap_or_else(Map::new);
    obj.insert("command".to_string(), Value::String(node_command.to_string()));
    obj.insert(
        "args".to_string(),
        Value::Array(new_args.into_iter().map(Value::String).collect()),
    );
    if !obj.contains_key("env") {
        obj.insert("env".to_string(), Value::Object(existing_env));
    } else {
        obj.insert("env".to_string(), json!(existing_env));
    }
    *entry = Value::Object(obj);

    let serialized = serde_json::to_string_pretty(&root)
        .map_err(|e| format!("序列化 {} 失败: {e}", mcp_json_path.display()))?;
    fs::write(mcp_json_path, format!("{serialized}\n"))
        .map_err(|e| format!("写入 {} 失败: {e}", mcp_json_path.display()))?;
    Ok(true)
}

fn resolve_node_command(cursor_bin: Option<&Path>) -> String {
    #[cfg(target_os = "windows")]
    let path_node = find_executable_in_path("node.exe").or_else(|| find_executable_in_path("node"));
    #[cfg(not(target_os = "windows"))]
    let path_node = find_executable_in_path("node");

    if path_node.is_some() {
        return "node".to_string();
    }
    if let Some(bin) = cursor_bin {
        if let Some((node_exe, _)) = resolve_cursor_node_entry(bin) {
            return node_exe.to_string_lossy().into_owned();
        }
    }
    "node".to_string()
}

/// Ensures the persistent Chrome DevTools MCP bridge script is installed and
/// `~/.cursor/mcp.json` (if present and using stock `chrome-devtools-mcp`) points to it.
pub fn ensure_cursor_persistent_chrome_mcp(cursor_bin: Option<&Path>) {
    let Some(support_dir) = default_agent_support_dir() else {
        return;
    };
    let Ok(bridge_path) = ensure_bridge_script_at(&support_dir) else {
        return;
    };
    let Some(home) = user_home_dir() else {
        return;
    };
    let mcp_json = home.join(".cursor").join("mcp.json");
    let node_cmd = resolve_node_command(cursor_bin);
    let _ = sync_cursor_mcp_persistent_chrome_config(&mcp_json, &bridge_path, &node_cmd);
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn sync_upgrades_stock_chrome_devtools_mcp_and_preserves_other_servers_and_flags() {
        let dir = std::env::temp_dir().join(format!("rust_ai_ide_mcp_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let mcp_path = dir.join("mcp.json");
        let bridge_path = ensure_bridge_script_at(&dir).unwrap();

        fs::write(
            &mcp_path,
            r#"{
  "mcpServers": {
    "chrome-devtools": {
      "command": "npx -y chrome-devtools-mcp@latest",
      "args": ["--viewport", "1280x720"],
      "env": { "CUSTOM": "1" }
    },
    "rea": {
      "command": "cmd.exe",
      "args": ["/d", "/c", "npx", "-y", "rea-agents@6.5.0", "mcp"]
    }
  }
}"#,
        )
        .unwrap();

        let changed = sync_cursor_mcp_persistent_chrome_config(&mcp_path, &bridge_path, "node")
            .expect("sync succeeds");
        assert!(changed);

        let updated: Value = serde_json::from_str(&fs::read_to_string(&mcp_path).unwrap()).unwrap();
        let cd = &updated["mcpServers"]["chrome-devtools"];
        assert_eq!(cd["command"].as_str(), Some("node"));
        assert_eq!(
            cd["args"],
            json!([
                bridge_path.to_string_lossy().as_ref(),
                "--viewport",
                "1280x720"
            ])
        );
        assert_eq!(cd["env"]["CUSTOM"].as_str(), Some("1"));
        assert_eq!(
            updated["mcpServers"]["rea"]["args"],
            json!(["/d", "/c", "npx", "-y", "rea-agents@6.5.0", "mcp"])
        );

        // Second sync is a clean no-op
        let changed_again =
            sync_cursor_mcp_persistent_chrome_config(&mcp_path, &bridge_path, "node").unwrap();
        assert!(!changed_again);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_respects_explicit_browser_url_or_isolated_mode() {
        let dir = std::env::temp_dir().join(format!("rust_ai_ide_mcp_skip_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let mcp_path = dir.join("mcp.json");
        let bridge_path = dir.join(BRIDGE_SCRIPT_FILENAME);

        let original = r#"{
  "mcpServers": {
    "chrome-devtools": {
      "command": "npx",
      "args": ["-y", "chrome-devtools-mcp@latest", "--browserUrl", "http://127.0.0.1:9333"]
    }
  }
}"#;
        fs::write(&mcp_path, original).unwrap();

        let changed =
            sync_cursor_mcp_persistent_chrome_config(&mcp_path, &bridge_path, "node").unwrap();
        assert!(!changed);
        assert_eq!(fs::read_to_string(&mcp_path).unwrap(), original);

        let _ = fs::remove_dir_all(&dir);
    }
}
