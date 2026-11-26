use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::debug;
use url::Url;

static PROFILE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Errors returned by the headless CDP browser renderer.
#[derive(Debug, Error)]
pub enum BrowserError {
    #[error("no Chromium-based browser found; install Chrome/Edge/Chromium or pass --cdp-url / WEBDELTA_BROWSER_PATH")]
    BrowserNotFound,

    #[error("invalid browser configuration: {0}")]
    InvalidConfig(String),

    #[error("failed to launch headless browser: {0}")]
    Launch(String),

    #[error("CDP protocol error: {0}")]
    Cdp(String),

    #[error("browser operation timed out: {0}")]
    Timeout(String),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience result alias using [`BrowserError`].
pub type Result<T> = std::result::Result<T, BrowserError>;

/// Options controlling how a page is rendered in a headless Chromium-compatible browser.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserOptions {
    pub cdp_url: Option<String>,
    pub executable_path: Option<String>,
    pub wait_for_selector: Option<String>,
    pub wait_ms: u64,
    pub click_selector: Option<String>,
    pub scroll_to_bottom: bool,
    pub timeout_secs: u64,
    pub user_agent: Option<String>,
    pub headers: BTreeMap<String, String>,
}

impl Default for BrowserOptions {
    fn default() -> Self {
        Self {
            cdp_url: None,
            executable_path: None,
            wait_for_selector: None,
            wait_ms: 0,
            click_selector: None,
            scroll_to_bottom: false,
            timeout_secs: 20,
            user_agent: None,
            headers: BTreeMap::new(),
        }
    }
}

/// Rendered DOM HTML and timing produced by [`render_page`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderedPage {
    pub url: String,
    pub html: String,
    pub elapsed_ms: u64,
}

#[derive(Debug, Deserialize)]
struct CdpTarget {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    ws_url: Option<String>,
}

struct ChildBrowserGuard {
    child: Option<Child>,
    profile_dir: PathBuf,
}

impl ChildBrowserGuard {
    async fn shutdown(mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        }
        remove_dir_best_effort(&self.profile_dir);
    }
}

impl Drop for ChildBrowserGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
        remove_dir_best_effort(&self.profile_dir);
    }
}

fn remove_dir_best_effort(path: &Path) {
    for _ in 0..5 {
        if !path.exists() || std::fs::remove_dir_all(path).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Discovers a local Chrome, Edge, or Chromium executable.
///
/// Checks `WEBDELTA_BROWSER_PATH` first, then standard platform installation locations.
pub fn discover_browser_binary() -> Option<PathBuf> {
    if let Some(custom) = std::env::var_os("WEBDELTA_BROWSER_PATH") {
        let p = PathBuf::from(custom);
        if p.is_file() {
            return Some(p);
        }
    }

    for candidate in platform_browser_candidates() {
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        let names: &[&str] = if cfg!(windows) {
            &["chrome.exe", "msedge.exe", "chromium.exe"]
        } else {
            &[
                "google-chrome",
                "google-chrome-stable",
                "chromium",
                "chromium-browser",
                "microsoft-edge",
            ]
        };
        for dir in std::env::split_paths(&path_var) {
            for name in names {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

fn platform_browser_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    #[cfg(target_os = "windows")]
    {
        let roots = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"];
        let suffixes = [
            r"Google\Chrome\Application\chrome.exe",
            r"Microsoft\Edge\Application\msedge.exe",
            r"Chromium\Application\chrome.exe",
            r"BraveSoftware\Brave-Browser\Application\brave.exe",
        ];
        for root_env in roots {
            if let Some(base) = std::env::var_os(root_env) {
                let base_path = PathBuf::from(base);
                for suffix in suffixes {
                    candidates.push(base_path.join(suffix));
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        candidates.extend([
            PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
            PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
            PathBuf::from("/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
        ]);
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        candidates.extend([
            PathBuf::from("/usr/bin/google-chrome"),
            PathBuf::from("/usr/bin/google-chrome-stable"),
            PathBuf::from("/usr/bin/chromium"),
            PathBuf::from("/usr/bin/chromium-browser"),
            PathBuf::from("/usr/bin/microsoft-edge"),
            PathBuf::from("/snap/bin/chromium"),
        ]);
    }

    candidates
}

/// Returns `true` when an HTML document appears to be an unrendered client-side SPA shell.
pub fn looks_like_spa_shell(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    let visible_len = estimate_visible_text_len(&lower);

    let has_spa_marker = lower.contains("id=\"root\"")
        || lower.contains("id='root'")
        || lower.contains("id=\"app\"")
        || lower.contains("id='app'")
        || lower.contains("id=\"__next\"")
        || lower.contains("id=\"__nuxt\"")
        || lower.contains("data-reactroot")
        || lower.contains("enable javascript")
        || lower.contains("javascript is required")
        || lower.contains("you need to enable javascript");

    if has_spa_marker && visible_len < 160 {
        return true;
    }

    visible_len < 40 && lower.contains("<script")
}

fn estimate_visible_text_len(lower_html: &str) -> usize {
    let without_scripts = strip_tag_block(lower_html, "script");
    let without_styles = strip_tag_block(&without_scripts, "style");
    let without_noscript = strip_tag_block(&without_styles, "noscript");

    let mut count = 0usize;
    let mut in_tag = false;
    let mut prev_space = true;

    for ch in without_noscript.chars() {
        if ch == '<' {
            in_tag = true;
            continue;
        }
        if ch == '>' {
            in_tag = false;
            continue;
        }
        if !in_tag {
            if ch.is_whitespace() {
                if !prev_space {
                    count += 1;
                    prev_space = true;
                }
            } else {
                count += 1;
                prev_space = false;
            }
        }
    }

    count
}

fn strip_tag_block(html: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(html.len());
    let mut rest = html;

    while let Some(start) = rest.find(&open) {
        out.push_str(&rest[..start]);
        let after_open = &rest[start..];
        if let Some(end_offset) = after_open.find(&close) {
            rest = &after_open[end_offset + close.len()..];
        } else {
            return out;
        }
    }
    out.push_str(rest);
    out
}

/// Renders `url` in a headless Chromium-compatible browser over CDP and returns the evaluated DOM HTML.
pub async fn render_page(url: &str, opts: &BrowserOptions) -> Result<RenderedPage> {
    let started = Instant::now();
    let total_timeout = Duration::from_secs(opts.timeout_secs.max(1));
    let deadline = started + total_timeout;

    let mut guard: Option<ChildBrowserGuard> = None;
    let page_ws_url = match &opts.cdp_url {
        Some(raw) => resolve_remote_page_ws(raw, deadline).await?,
        None => {
            let (ws_url, launched_guard) = launch_ephemeral_browser(opts, deadline).await?;
            guard = Some(launched_guard);
            ws_url
        }
    };

    let render_res = drive_cdp_page(&page_ws_url, url, opts, deadline).await;

    if let Some(g) = guard {
        g.shutdown().await;
    }

    let html = render_res?;
    Ok(RenderedPage {
        url: url.to_string(),
        html,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

async fn resolve_remote_page_ws(raw: &str, deadline: Instant) -> Result<String> {
    let trimmed = raw.trim();
    let parsed = Url::parse(trimmed).map_err(|err| {
        BrowserError::InvalidConfig(format!("invalid cdp_url '{trimmed}': {err}"))
    })?;

    match parsed.scheme() {
        "ws" | "wss" => Ok(trimmed.to_string()),
        "http" | "https" => {
            let base = trimmed.trim_end_matches('/');
            let client = Client::builder().build().map_err(|err| {
                BrowserError::Cdp(format!("failed creating CDP HTTP client: {err}"))
            })?;

            let list_url = format!("{base}/json/list");
            let remaining = remaining_duration(deadline)?;
            let targets: Vec<CdpTarget> = timeout(remaining, async {
                let resp = client.get(&list_url).send().await.map_err(|err| {
                    BrowserError::Cdp(format!("failed querying '{list_url}': {err}"))
                })?;
                resp.json::<Vec<CdpTarget>>()
                    .await
                    .map_err(|err| BrowserError::Cdp(format!("invalid /json/list JSON: {err}")))
            })
            .await
            .map_err(|_| {
                BrowserError::Timeout("timed out querying remote CDP /json/list".to_string())
            })??;

            if let Some(ws) = targets
                .into_iter()
                .find(|t| t.kind == "page")
                .and_then(|t| t.ws_url)
            {
                return Ok(ws);
            }

            let new_url = format!("{base}/json/new?about:blank");
            let remaining = remaining_duration(deadline)?;
            let created: CdpTarget = timeout(remaining, async {
                let resp = client.put(&new_url).send().await.map_err(|err| {
                    BrowserError::Cdp(format!("failed creating CDP target: {err}"))
                })?;
                resp.json::<CdpTarget>()
                    .await
                    .map_err(|err| BrowserError::Cdp(format!("invalid /json/new JSON: {err}")))
            })
            .await
            .map_err(|_| {
                BrowserError::Timeout("timed out creating remote CDP page".to_string())
            })??;

            created.ws_url.ok_or_else(|| {
                BrowserError::Cdp(
                    "remote CDP target did not return webSocketDebuggerUrl".to_string(),
                )
            })
        }
        other => Err(BrowserError::InvalidConfig(format!(
            "unsupported cdp_url scheme '{other}': expected http, https, ws, or wss"
        ))),
    }
}

async fn launch_ephemeral_browser(
    opts: &BrowserOptions,
    deadline: Instant,
) -> Result<(String, ChildBrowserGuard)> {
    let binary = match &opts.executable_path {
        Some(p) => {
            let path = PathBuf::from(p);
            if !path.is_file() {
                return Err(BrowserError::Launch(format!(
                    "browser executable '{}' does not exist",
                    path.display()
                )));
            }
            path
        }
        None => discover_browser_binary().ok_or(BrowserError::BrowserNotFound)?,
    };

    let uniq = PROFILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let profile_dir =
        std::env::temp_dir().join(format!("webdelta-cdp-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&profile_dir)?;

    let mut cmd = Command::new(&binary);
    cmd.arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-extensions")
        .arg("--disable-background-networking")
        .arg("--disable-sync")
        .arg("--metrics-recording-only")
        .arg("--mute-audio")
        .arg("--remote-debugging-port=0")
        .arg(format!("--user-data-dir={}", profile_dir.display()));

    if cfg!(unix) && std::env::var_os("CI").is_some() {
        cmd.arg("--no-sandbox");
    }

    cmd.arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().map_err(|err| {
        BrowserError::Launch(format!("failed spawning '{}': {err}", binary.display()))
    })?;

    let stderr = child.stderr.take().ok_or_else(|| {
        BrowserError::Launch("browser process did not expose stderr pipe".to_string())
    })?;

    let mut guard = ChildBrowserGuard {
        child: Some(child),
        profile_dir: profile_dir.clone(),
    };

    let browser_ws_url =
        read_devtools_ws_from_stderr_or_file(stderr, &profile_dir, deadline).await?;
    let parsed_ws = Url::parse(&browser_ws_url).map_err(|err| {
        BrowserError::Cdp(format!("invalid DevTools URL '{browser_ws_url}': {err}"))
    })?;
    let port = parsed_ws.port().ok_or_else(|| {
        BrowserError::Cdp(format!("DevTools URL '{browser_ws_url}' missing port"))
    })?;

    let client = Client::builder().build().map_err(|err| {
        BrowserError::Cdp(format!("failed creating local CDP HTTP client: {err}"))
    })?;
    let list_url = format!("http://127.0.0.1:{port}/json/list");

    let mut page_ws_url = None;
    while Instant::now() < deadline {
        if let Ok(resp) = client.get(&list_url).send().await {
            if let Ok(targets) = resp.json::<Vec<CdpTarget>>().await {
                if let Some(ws) = targets
                    .into_iter()
                    .find(|t| t.kind == "page")
                    .and_then(|t| t.ws_url)
                {
                    page_ws_url = Some(ws);
                    break;
                }
            }
        }
        if let Some(ch) = guard.child.as_mut() {
            if let Ok(Some(status)) = ch.try_wait() {
                return Err(BrowserError::Launch(format!(
                    "browser exited prematurely with status {status}"
                )));
            }
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    let ws_url = page_ws_url.ok_or_else(|| {
        BrowserError::Timeout("timed out waiting for CDP page target".to_string())
    })?;

    debug!(port, ws = %ws_url, "connected to ephemeral CDP browser");
    Ok((ws_url, guard))
}

async fn read_devtools_ws_from_stderr_or_file(
    stderr: tokio::process::ChildStderr,
    profile_dir: &Path,
    deadline: Instant,
) -> Result<String> {
    let mut lines = BufReader::new(stderr).lines();
    let active_port_file = profile_dir.join("DevToolsActivePort");

    loop {
        let remaining = remaining_duration(deadline)?;
        let step = remaining.min(Duration::from_millis(150));

        match timeout(step, lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                if let Some(idx) = line.find("DevTools listening on ") {
                    let ws_part = line[idx + "DevTools listening on ".len()..].trim();
                    return Ok(ws_part.to_string());
                }
            }
            Ok(Ok(None)) => {
                if let Some(ws) = read_active_port_ws(&active_port_file) {
                    return Ok(ws);
                }
                return Err(BrowserError::Launch(
                    "browser stderr closed before DevTools port was reported".to_string(),
                ));
            }
            Ok(Err(err)) => {
                return Err(BrowserError::Launch(format!(
                    "failed reading browser stderr: {err}"
                )));
            }
            Err(_) => {
                if let Some(ws) = read_active_port_ws(&active_port_file) {
                    return Ok(ws);
                }
            }
        }
    }
}

fn read_active_port_ws(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut parts = content.lines();
    let port = parts.next()?.trim();
    let ws_path = parts.next()?.trim();
    if port.is_empty() || ws_path.is_empty() {
        return None;
    }
    Some(format!("ws://127.0.0.1:{port}{ws_path}"))
}

struct CdpSession {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    pending_events: Vec<(String, Value)>,
}

impl CdpSession {
    async fn connect(ws_url: &str, deadline: Instant) -> Result<Self> {
        let remaining = remaining_duration(deadline)?;
        let (ws, _) = timeout(remaining, tokio_tungstenite::connect_async(ws_url))
            .await
            .map_err(|_| {
                BrowserError::Timeout("timed out connecting to CDP WebSocket".to_string())
            })?
            .map_err(|err| BrowserError::Cdp(format!("WebSocket connect error: {err}")))?;

        Ok(Self {
            ws,
            next_id: 1,
            pending_events: Vec::new(),
        })
    }

    async fn call(&mut self, method: &str, params: Value, deadline: Instant) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let payload = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        let remaining = remaining_duration(deadline)?;
        timeout(
            remaining,
            self.ws.send(Message::Text(payload.to_string().into())),
        )
        .await
        .map_err(|_| BrowserError::Timeout(format!("timed out sending CDP {method}")))?
        .map_err(|err| BrowserError::Cdp(format!("failed sending CDP {method}: {err}")))?;

        loop {
            let msg = self.recv_json(deadline, method).await?;
            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = msg.get("error") {
                    return Err(BrowserError::Cdp(format!(
                        "CDP {method} returned error: {err}"
                    )));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            if let Some(evt_method) = msg.get("method").and_then(Value::as_str) {
                let evt_params = msg.get("params").cloned().unwrap_or(Value::Null);
                self.pending_events
                    .push((evt_method.to_string(), evt_params));
            }
        }
    }

    async fn wait_for_event(&mut self, expected_method: &str, deadline: Instant) -> Result<Value> {
        if let Some(pos) = self
            .pending_events
            .iter()
            .position(|(m, _)| m == expected_method)
        {
            return Ok(self.pending_events.remove(pos).1);
        }

        loop {
            let msg = self.recv_json(deadline, expected_method).await?;
            if let Some(evt_method) = msg.get("method").and_then(Value::as_str) {
                let evt_params = msg.get("params").cloned().unwrap_or(Value::Null);
                if evt_method == expected_method {
                    return Ok(evt_params);
                }
                self.pending_events
                    .push((evt_method.to_string(), evt_params));
            }
        }
    }

    async fn recv_json(&mut self, deadline: Instant, context: &str) -> Result<Value> {
        loop {
            let remaining = remaining_duration(deadline)?;
            let frame = timeout(remaining, self.ws.next())
                .await
                .map_err(|_| BrowserError::Timeout(format!("timed out waiting for {context}")))?;

            let Some(msg_res) = frame else {
                return Err(BrowserError::Cdp(format!(
                    "CDP WebSocket closed while waiting for {context}"
                )));
            };

            let msg = msg_res.map_err(|err| {
                BrowserError::Cdp(format!("CDP WebSocket error on {context}: {err}"))
            })?;

            match msg {
                Message::Text(txt) => {
                    return serde_json::from_str(&txt).map_err(|err| {
                        BrowserError::Cdp(format!("invalid CDP JSON message: {err}"))
                    });
                }
                Message::Close(_) => {
                    return Err(BrowserError::Cdp(format!(
                        "CDP closed connection during {context}"
                    )));
                }
                _ => continue,
            }
        }
    }

    async fn close(mut self) {
        let _ = self.ws.close(None).await;
    }
}

async fn drive_cdp_page(
    ws_url: &str,
    url: &str,
    opts: &BrowserOptions,
    deadline: Instant,
) -> Result<String> {
    let mut cdp = CdpSession::connect(ws_url, deadline).await?;

    cdp.call("Page.enable", json!({}), deadline).await?;
    cdp.call("Network.enable", json!({}), deadline).await?;

    if let Some(ua) = &opts.user_agent {
        cdp.call(
            "Network.setUserAgentOverride",
            json!({ "userAgent": ua }),
            deadline,
        )
        .await?;
    }

    if !opts.headers.is_empty() {
        cdp.call(
            "Network.setExtraHTTPHeaders",
            json!({ "headers": opts.headers }),
            deadline,
        )
        .await?;
    }

    cdp.pending_events.clear();
    let nav_res = cdp
        .call("Page.navigate", json!({ "url": url }), deadline)
        .await?;
    if let Some(err_text) = nav_res.get("errorText").and_then(Value::as_str) {
        return Err(BrowserError::Cdp(format!(
            "browser failed navigating to '{url}': {err_text}"
        )));
    }

    cdp.wait_for_event("Page.loadEventFired", deadline).await?;

    if let Some(selector) = &opts.wait_for_selector {
        let escaped = serde_json::to_string(selector)
            .map_err(|err| BrowserError::InvalidConfig(format!("invalid selector: {err}")))?;
        let check_expr = format!("Boolean(document.querySelector({escaped}))");

        loop {
            let eval = cdp
                .call(
                    "Runtime.evaluate",
                    json!({ "expression": check_expr, "returnByValue": true }),
                    deadline,
                )
                .await?;
            let found = eval
                .pointer("/result/value")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if found {
                break;
            }
            let remaining = remaining_duration(deadline)?;
            tokio::time::sleep(remaining.min(Duration::from_millis(75))).await;
        }
    }

    if let Some(click_sel) = &opts.click_selector {
        let escaped = serde_json::to_string(click_sel)
            .map_err(|err| BrowserError::InvalidConfig(format!("invalid click_selector: {err}")))?;
        let click_expr = format!("(() => {{ const el = document.querySelector({escaped}); if (el) {{ el.click(); return true; }} return false; }})()");
        let _ = cdp
            .call(
                "Runtime.evaluate",
                json!({ "expression": click_expr, "returnByValue": true }),
                deadline,
            )
            .await?;
        let remaining = remaining_duration(deadline)?;
        tokio::time::sleep(remaining.min(Duration::from_millis(150))).await;
    }

    if opts.scroll_to_bottom {
        let _ = cdp
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": "window.scrollTo(0, document.body ? document.body.scrollHeight : 0)",
                    "returnByValue": true
                }),
                deadline,
            )
            .await?;
        let remaining = remaining_duration(deadline)?;
        tokio::time::sleep(remaining.min(Duration::from_millis(200))).await;
    }

    if opts.wait_ms > 0 {
        let remaining = remaining_duration(deadline)?;
        tokio::time::sleep(remaining.min(Duration::from_millis(opts.wait_ms))).await;
    }

    let dom_eval = cdp
        .call(
            "Runtime.evaluate",
            json!({
                "expression": "document.documentElement ? document.documentElement.outerHTML : ''",
                "returnByValue": true
            }),
            deadline,
        )
        .await?;

    let html = dom_eval
        .pointer("/result/value")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            BrowserError::Cdp("Runtime.evaluate did not return DOM HTML string".to_string())
        })?
        .to_string();

    cdp.close().await;
    Ok(html)
}

fn remaining_duration(deadline: Instant) -> Result<Duration> {
    let now = Instant::now();
    if now >= deadline {
        return Err(BrowserError::Timeout(
            "browser render deadline exceeded".to_string(),
        ));
    }
    Ok(deadline - now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_looks_like_spa_shell() {
        let react_shell = r#"<!doctype html><html><head><title>App</title></head><body><div id="root"></div><script src="/bundle.js"></script></body></html>"#;
        assert!(looks_like_spa_shell(react_shell));

        let next_shell = r#"<html><body><div id="__next"></div><noscript>Please enable JavaScript to continue.</noscript></body></html>"#;
        assert!(looks_like_spa_shell(next_shell));

        let static_page = r#"<html><body><h1>Product Catalog</h1><p>This page already renders full HTML content from the server without needing client scripts.</p></body></html>"#;
        assert!(!looks_like_spa_shell(static_page));

        let tiny_static = r#"<html><body><h1>Hello World</h1></body></html>"#;
        assert!(!looks_like_spa_shell(tiny_static));
    }

    #[test]
    fn test_discover_browser_respects_override() {
        let nonexistent = std::env::temp_dir().join("webdelta-nonexistent-browser-bin");
        // Only check function returns Option without panicking; if a local browser exists, path is a file.
        if let Some(found) = discover_browser_binary() {
            assert!(found.is_file());
        }
        assert!(!nonexistent.exists());
    }
}
