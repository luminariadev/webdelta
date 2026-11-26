use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tracing::{debug, warn};
use webdelta_core::{
    compute_delta, compute_diff, export_history, load_config, redact_config_toml, save_config,
    test_notifier, BrowserConfig, ConditionConfig, Config, Error, ExportFormat, ExtractorConfig,
    FetchEngine, HttpMethod, MonitorEngine, NormalizeConfig, Result, StateStore, WatchConfig,
};

const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";
const SUPPORTED_PROTOCOLS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

/// JSON-RPC 2.0 server exposing `webdelta` tools and resources over MCP (`stdio` and HTTP).
#[derive(Clone)]
pub struct McpServer {
    config_path: PathBuf,
    db_path: PathBuf,
    store: Arc<Mutex<Option<StateStore>>>,
}

#[derive(Debug, Deserialize)]
struct RpcRequest {
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct ExtractorParams {
    #[serde(default)]
    extractor_type: Option<String>,
    #[serde(default)]
    selector: Option<String>,
    #[serde(default)]
    attribute: Option<String>,
    #[serde(default)]
    all: Option<bool>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    group: Option<usize>,
    #[serde(default)]
    header: Option<String>,
}

impl McpServer {
    /// Creates a new MCP server that opens `StateStore` lazily on first database call.
    pub fn new(config_path: PathBuf, db_path: PathBuf) -> Self {
        Self {
            config_path,
            db_path,
            store: Arc::new(Mutex::new(None)),
        }
    }

    /// Creates a new MCP server sharing an already-open `StateStore`.
    pub fn with_store(config_path: PathBuf, db_path: PathBuf, store: StateStore) -> Self {
        Self {
            config_path,
            db_path,
            store: Arc::new(Mutex::new(Some(store))),
        }
    }

    async fn get_store(&self) -> Result<StateStore> {
        let mut guard = self.store.lock().await;
        if let Some(existing) = guard.as_ref() {
            return Ok(existing.clone());
        }
        let opened = StateStore::open(&self.db_path)?;
        *guard = Some(opened.clone());
        Ok(opened)
    }

    /// Processes a single JSON-RPC 2.0 input line and returns an optional JSON response line.
    pub async fn handle_line(&self, line: &str) -> Option<String> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }

        let req: RpcRequest = match serde_json::from_str(trimmed) {
            Ok(parsed) => parsed,
            Err(err) => {
                return Some(serialize_error(
                    Value::Null,
                    -32700,
                    format!("parse error: {err}"),
                ));
            }
        };

        let id = req.id.clone();
        if req.jsonrpc.as_deref() != Some("2.0") {
            return Some(serialize_error(
                id.unwrap_or(Value::Null),
                -32600,
                "invalid request: jsonrpc must be '2.0'".to_string(),
            ));
        }

        let Some(method) = req.method else {
            return Some(serialize_error(
                id.unwrap_or(Value::Null),
                -32600,
                "invalid request: missing method".to_string(),
            ));
        };

        // JSON-RPC notifications have no `id` and expect no reply.
        let Some(req_id) = id else {
            debug!(method = %method, "received MCP notification");
            return None;
        };

        let response = match method.as_str() {
            "initialize" => self.handle_initialize(req_id, req.params),
            "ping" => serialize_result(req_id, json!({})),
            "tools/list" => serialize_result(req_id, json!({ "tools": tool_definitions() })),
            "tools/call" => self.handle_tools_call(req_id, req.params).await,
            "resources/list" => serialize_result(req_id, json!({ "resources": resource_list() })),
            "resources/templates/list" => {
                serialize_result(req_id, json!({ "resourceTemplates": resource_templates() }))
            }
            "resources/read" => self.handle_resources_read(req_id, req.params).await,
            other => serialize_error(req_id, -32601, format!("method not found: {other}")),
        };

        Some(response)
    }

    fn handle_initialize(&self, id: Value, params: Option<Value>) -> String {
        let requested_version = params
            .as_ref()
            .and_then(|p| p.get("protocolVersion"))
            .and_then(Value::as_str);

        let protocol_version = match requested_version {
            Some(ver) if SUPPORTED_PROTOCOLS.contains(&ver) => ver,
            _ => DEFAULT_PROTOCOL_VERSION,
        };

        serialize_result(
            id,
            json!({
                "protocolVersion": protocol_version,
                "capabilities": {
                    "tools": {},
                    "resources": {}
                },
                "serverInfo": {
                    "name": "webdelta",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )
    }

    async fn handle_tools_call(&self, id: Value, params: Option<Value>) -> String {
        let Some(params_obj) = params.and_then(|v| v.as_object().cloned()) else {
            return serialize_error(
                id,
                -32602,
                "invalid params: expected object with 'name'".to_string(),
            );
        };

        let Some(tool_name) = params_obj.get("name").and_then(Value::as_str) else {
            return serialize_error(
                id,
                -32602,
                "invalid params: missing string field 'name'".to_string(),
            );
        };

        let args = params_obj
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        if tool_name == "test_notifier" {
            return self.handle_test_notifier_call(id, args).await;
        }

        let outcome = match tool_name {
            "list_watches" => self.tool_list_watches(args).await,
            "add_watch" => self.tool_add_watch(args).await,
            "update_watch" => self.tool_update_watch(args).await,
            "remove_watch" => self.tool_remove_watch(args).await,
            "check_now" => self.tool_check_now(args).await,
            "get_diff" => self.tool_get_diff(args).await,
            "get_history" => self.tool_get_history(args).await,
            "preview_extract" => self.tool_preview_extract(args).await,
            "export_history" => self.tool_export_history(args).await,
            other => Err(Error::Validation(format!("unknown tool '{other}'"))),
        };

        match outcome {
            Ok(payload) => tool_json_response(id, &payload, false),
            Err(err) => {
                warn!(tool = %tool_name, error = %err, "MCP tool call failed");
                serialize_result(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": err.to_string() }],
                        "isError": true
                    }),
                )
            }
        }
    }

    async fn handle_test_notifier_call(&self, id: Value, args: Value) -> String {
        #[derive(Deserialize)]
        struct Args {
            notifier: String,
        }
        let parsed: Args = match parse_args(args) {
            Ok(v) => v,
            Err(err) => {
                return serialize_result(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": err.to_string() }],
                        "isError": true
                    }),
                );
            }
        };
        let config = match self.load_or_empty_config() {
            Ok(c) => c,
            Err(err) => {
                return serialize_result(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": err.to_string() }],
                        "isError": true
                    }),
                );
            }
        };

        let result = test_notifier(&config.notifiers, &parsed.notifier).await;
        let is_err = !result.ok;
        let payload = serde_json::to_value(&result).unwrap_or_else(|_| json!({}));
        tool_json_response(id, &payload, is_err)
    }

    async fn handle_resources_read(&self, id: Value, params: Option<Value>) -> String {
        let Some(uri) = params
            .as_ref()
            .and_then(|p| p.get("uri"))
            .and_then(Value::as_str)
        else {
            return serialize_error(
                id,
                -32602,
                "invalid params: missing string field 'uri'".to_string(),
            );
        };

        match self.read_resource_uri(uri).await {
            Ok((mime_type, text)) => serialize_result(
                id,
                json!({
                    "contents": [{
                        "uri": uri,
                        "mimeType": mime_type,
                        "text": text
                    }]
                }),
            ),
            Err(Error::Validation(msg)) => serialize_error(id, -32602, msg),
            Err(err) => serialize_error(id, -32603, err.to_string()),
        }
    }

    async fn read_resource_uri(&self, uri: &str) -> Result<(&'static str, String)> {
        if uri == "webdelta://watches" {
            let val = self
                .tool_list_watches(json!({ "include_disabled": true }))
                .await?;
            let text = serde_json::to_string_pretty(&val).unwrap_or_else(|_| val.to_string());
            return Ok(("application/json", text));
        }

        if uri == "webdelta://config" {
            let cfg = self.load_or_empty_config()?;
            let text = redact_config_toml(&cfg)?;
            return Ok(("text/plain", text));
        }

        if let Some(watch_id) = uri.strip_prefix("webdelta://history/") {
            if watch_id.is_empty() {
                return Err(Error::Validation(
                    "missing watch_id in history URI".to_string(),
                ));
            }
            let val = self
                .tool_get_history(json!({ "watch_id": watch_id, "limit": 50 }))
                .await?;
            let text = serde_json::to_string_pretty(&val).unwrap_or_else(|_| val.to_string());
            return Ok(("application/json", text));
        }

        if let Some(watch_id) = uri.strip_prefix("webdelta://diff/") {
            if watch_id.is_empty() {
                return Err(Error::Validation(
                    "missing watch_id in diff URI".to_string(),
                ));
            }
            let val = self.tool_get_diff(json!({ "watch_id": watch_id })).await?;
            let text = serde_json::to_string_pretty(&val).unwrap_or_else(|_| val.to_string());
            return Ok(("application/json", text));
        }

        Err(Error::Validation(format!("unknown resource URI: {uri}")))
    }

    fn load_or_empty_config(&self) -> Result<Config> {
        if !self.config_path.exists() {
            return Ok(Config::default());
        }
        load_config(&self.config_path)
    }

    async fn tool_list_watches(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize, Default)]
        struct Args {
            #[serde(default)]
            include_disabled: bool,
        }
        let parsed: Args = parse_args(args)?;
        let config = self.load_or_empty_config()?;
        let store = self.get_store().await?;

        let mut watches_out = Vec::new();
        for watch in &config.watches {
            if !parsed.include_disabled && !watch.enabled {
                continue;
            }
            let latest = store.get_latest(&watch.id)?;
            watches_out.push(json!({
                "id": watch.id,
                "name": watch.display_name(),
                "url": watch.url,
                "interval": watch.interval.as_deref().unwrap_or(&config.defaults.interval),
                "enabled": watch.enabled,
                "engine": watch.engine,
                "extractor": watch.extractor,
                "conditions": watch.conditions,
                "notifiers": watch.notifiers,
                "latest": latest,
            }));
        }

        Ok(json!({
            "config_path": self.config_path.display().to_string(),
            "count": watches_out.len(),
            "watches": watches_out,
        }))
    }

    async fn tool_add_watch(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            id: String,
            url: String,
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            interval: Option<String>,
            #[serde(default)]
            engine: FetchEngine,
            #[serde(default)]
            wait_for_selector: Option<String>,
            #[serde(default)]
            wait_ms: Option<u64>,
            #[serde(default)]
            cdp_url: Option<String>,
            #[serde(default)]
            click_selector: Option<String>,
            #[serde(default)]
            scroll_to_bottom: bool,
            #[serde(flatten)]
            extractor: ExtractorParams,
            #[serde(default)]
            ignore_regex: Vec<String>,
            #[serde(default)]
            conditions: Vec<ConditionConfig>,
            #[serde(default)]
            notifiers: Vec<String>,
        }

        let parsed: Args = parse_args(args)?;
        let extractor = build_extractor(&parsed.extractor)?;
        let mut config = self.load_or_empty_config()?;

        if config.watches.iter().any(|w| w.id == parsed.id) {
            return Err(Error::Validation(format!(
                "watch '{}' already exists",
                parsed.id
            )));
        }

        let watch = WatchConfig {
            id: parsed.id,
            name: parsed.name,
            url: parsed.url,
            method: HttpMethod::Get,
            headers: BTreeMap::new(),
            interval: parsed.interval,
            timeout_secs: None,
            retries: None,
            engine: parsed.engine,
            browser: BrowserConfig {
                wait_for_selector: parsed.wait_for_selector,
                wait_ms: parsed.wait_ms,
                cdp_url: parsed.cdp_url,
                click_selector: parsed.click_selector,
                scroll_to_bottom: parsed.scroll_to_bottom,
            },
            extractor,
            ignore_regex: parsed.ignore_regex,
            normalize: NormalizeConfig::default(),
            conditions: parsed.conditions,
            notifiers: parsed.notifiers,
            enabled: true,
        };

        config.watches.push(watch.clone());
        save_config(&self.config_path, &config)?;

        Ok(json!({
            "added": true,
            "watch": watch,
        }))
    }

    async fn tool_update_watch(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            id: String,
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            url: Option<String>,
            #[serde(default)]
            interval: Option<String>,
            #[serde(default)]
            enabled: Option<bool>,
            #[serde(default)]
            engine: Option<FetchEngine>,
            #[serde(default)]
            wait_for_selector: Option<String>,
            #[serde(default)]
            wait_ms: Option<u64>,
            #[serde(default)]
            cdp_url: Option<String>,
            #[serde(default)]
            click_selector: Option<String>,
            #[serde(default)]
            scroll_to_bottom: Option<bool>,
            #[serde(flatten)]
            extractor: ExtractorParams,
            #[serde(default)]
            ignore_regex: Option<Vec<String>>,
            #[serde(default)]
            conditions: Option<Vec<ConditionConfig>>,
            #[serde(default)]
            notifiers: Option<Vec<String>>,
        }

        let parsed: Args = parse_args(args)?;
        let mut config = load_config(&self.config_path)?;

        let idx = config
            .watches
            .iter()
            .position(|w| w.id == parsed.id)
            .ok_or_else(|| Error::Validation(format!("watch '{}' not found", parsed.id)))?;

        if let Some(name) = parsed.name {
            config.watches[idx].name = Some(name);
        }
        if let Some(url) = parsed.url {
            config.watches[idx].url = url;
        }
        if let Some(interval) = parsed.interval {
            config.watches[idx].interval = Some(interval);
        }
        if let Some(enabled) = parsed.enabled {
            config.watches[idx].enabled = enabled;
        }
        if let Some(engine) = parsed.engine {
            config.watches[idx].engine = engine;
        }
        if let Some(sel) = parsed.wait_for_selector {
            config.watches[idx].browser.wait_for_selector = Some(sel);
        }
        if let Some(ms) = parsed.wait_ms {
            config.watches[idx].browser.wait_ms = Some(ms);
        }
        if let Some(cdp) = parsed.cdp_url {
            config.watches[idx].browser.cdp_url = Some(cdp);
        }
        if let Some(click) = parsed.click_selector {
            config.watches[idx].browser.click_selector = Some(click);
        }
        if let Some(scroll) = parsed.scroll_to_bottom {
            config.watches[idx].browser.scroll_to_bottom = scroll;
        }
        if parsed.extractor.extractor_type.is_some() {
            config.watches[idx].extractor = build_extractor(&parsed.extractor)?;
        }
        if let Some(ignore) = parsed.ignore_regex {
            config.watches[idx].ignore_regex = ignore;
        }
        if let Some(conds) = parsed.conditions {
            config.watches[idx].conditions = conds;
        }
        if let Some(nots) = parsed.notifiers {
            config.watches[idx].notifiers = nots;
        }

        let updated = config.watches[idx].clone();
        save_config(&self.config_path, &config)?;

        Ok(json!({
            "updated": true,
            "watch": updated,
        }))
    }

    async fn tool_remove_watch(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            id: String,
            #[serde(default)]
            purge_history: bool,
        }
        let parsed: Args = parse_args(args)?;
        let mut config = load_config(&self.config_path)?;
        let initial_len = config.watches.len();
        config.watches.retain(|w| w.id != parsed.id);

        if config.watches.len() == initial_len {
            return Err(Error::Validation(format!(
                "watch '{}' not found",
                parsed.id
            )));
        }

        save_config(&self.config_path, &config)?;
        let purged = if parsed.purge_history {
            let store = self.get_store().await?;
            store.purge_watch(&parsed.id)?
        } else {
            0
        };

        Ok(json!({
            "removed": parsed.id,
            "purged_snapshots": purged,
        }))
    }

    async fn tool_check_now(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize, Default)]
        struct Args {
            #[serde(default)]
            watch_id: Option<String>,
            #[serde(default)]
            notify: bool,
        }
        let parsed: Args = parse_args(args)?;
        let config = load_config(&self.config_path)?;
        let store = self.get_store().await?;
        let engine = MonitorEngine::new(store)?;

        let filter = parsed.watch_id.map(|id| vec![id]);
        let outcomes = engine
            .check_watches(
                &config,
                filter.as_deref(),
                config.defaults.concurrency,
                parsed.notify,
            )
            .await?;

        Ok(json!({
            "count": outcomes.len(),
            "results": outcomes,
        }))
    }

    async fn tool_get_diff(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            watch_id: String,
            #[serde(default = "default_context_lines")]
            context_lines: usize,
        }
        let parsed: Args = parse_args(args)?;
        let store = self.get_store().await?;

        let Some((prev, curr)) = store.get_latest_pair(&parsed.watch_id)? else {
            return Ok(json!({
                "watch_id": parsed.watch_id,
                "has_diff": false,
                "message": "fewer than two snapshots recorded for this watch",
            }));
        };

        let diff = compute_diff(
            &prev.extracted_text,
            &curr.extracted_text,
            parsed.context_lines,
        );
        let numeric_delta = match (prev.numeric_value, curr.numeric_value) {
            (Some(p), Some(c)) => Some(compute_delta(p, c)),
            _ => None,
        };

        Ok(json!({
            "watch_id": parsed.watch_id,
            "has_diff": prev.extracted_text != curr.extracted_text,
            "previous": prev,
            "current": curr,
            "numeric_delta": numeric_delta,
            "diff": diff,
        }))
    }

    async fn tool_get_history(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            watch_id: String,
            #[serde(default = "default_history_limit")]
            limit: usize,
        }
        let parsed: Args = parse_args(args)?;
        let store = self.get_store().await?;
        let snapshots = store.get_history(&parsed.watch_id, parsed.limit.clamp(1, 500))?;

        Ok(json!({
            "watch_id": parsed.watch_id,
            "count": snapshots.len(),
            "snapshots": snapshots,
        }))
    }

    async fn tool_preview_extract(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            url: String,
            #[serde(default)]
            engine: FetchEngine,
            #[serde(default)]
            wait_for_selector: Option<String>,
            #[serde(default)]
            wait_ms: Option<u64>,
            #[serde(default)]
            cdp_url: Option<String>,
            #[serde(default)]
            click_selector: Option<String>,
            #[serde(default)]
            scroll_to_bottom: bool,
            #[serde(flatten)]
            extractor: ExtractorParams,
            #[serde(default)]
            ignore_regex: Vec<String>,
            #[serde(default = "default_timeout_secs")]
            timeout_secs: u64,
        }
        let parsed: Args = parse_args(args)?;
        let extractor = build_extractor(&parsed.extractor)?;
        let browser = BrowserConfig {
            wait_for_selector: parsed.wait_for_selector,
            wait_ms: parsed.wait_ms,
            cdp_url: parsed.cdp_url,
            click_selector: parsed.click_selector,
            scroll_to_bottom: parsed.scroll_to_bottom,
        };
        let result = MonitorEngine::preview(
            &parsed.url,
            &extractor,
            &parsed.ignore_regex,
            &NormalizeConfig::default(),
            parsed.timeout_secs,
            parsed.engine,
            &browser,
            &BTreeMap::new(),
        )
        .await?;

        Ok(serde_json::to_value(result).unwrap_or_else(|_| json!({})))
    }

    async fn tool_export_history(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            watch_id: String,
            format: String,
            #[serde(default = "default_export_limit")]
            limit: usize,
        }
        let parsed: Args = parse_args(args)?;
        let format = ExportFormat::from_str(&parsed.format)?;
        let store = self.get_store().await?;
        let records = store.get_history(&parsed.watch_id, parsed.limit.clamp(1, 500))?;
        let config = self.load_or_empty_config()?;
        let watch = config.watches.iter().find(|w| w.id == parsed.watch_id);
        let content = export_history(watch, &parsed.watch_id, &records, format)?;

        Ok(json!({
            "watch_id": parsed.watch_id,
            "format": parsed.format,
            "count": records.len(),
            "content": content,
        }))
    }
}

#[derive(Clone)]
struct HttpMcpState {
    server: McpServer,
    bearer_token: Option<String>,
}

/// Builds an `axum::Router` exposing only `POST /mcp` (Streamable HTTP) for merging into another router.
pub fn mcp_route(server: McpServer, bearer_token: Option<String>) -> Router {
    let state = HttpMcpState {
        server,
        bearer_token,
    };
    Router::new()
        .route("/mcp", post(http_mcp_post))
        .with_state(state)
}

/// Builds an `axum::Router` exposing `POST /mcp` (Streamable HTTP) and `GET /health`.
pub fn mcp_http_router(server: McpServer, bearer_token: Option<String>) -> Router {
    mcp_route(server, bearer_token).route("/health", get(http_health))
}

async fn http_health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn http_mcp_post(
    State(state): State<HttpMcpState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    if !is_bearer_authorized(&headers, state.bearer_token.as_deref()) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "application/json")],
            json!({ "error": "unauthorized" }).to_string(),
        )
            .into_response();
    }

    match state.server.handle_line(&body).await {
        Some(reply) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            reply,
        )
            .into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Checks `Authorization: Bearer <token>` against `expected` when authentication is enabled.
pub fn is_bearer_authorized(headers: &HeaderMap, expected: Option<&str>) -> bool {
    let Some(token) = expected else {
        return true;
    };
    let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    auth.strip_prefix("Bearer ")
        .is_some_and(|provided| provided.trim() == token)
}

/// Runs the MCP server over standard input and standard output until EOF.
pub async fn run_stdio(config_path: PathBuf, db_path: PathBuf) -> Result<()> {
    let server = McpServer::new(config_path, db_path);
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();

    while let Some(line) = lines.next_line().await? {
        if let Some(reply) = server.handle_line(&line).await {
            stdout.write_all(reply.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

/// Runs the Streamable HTTP MCP server on `bind` until `Ctrl+C`.
pub async fn run_http(
    config_path: PathBuf,
    db_path: PathBuf,
    bind: &str,
    bearer_token: Option<String>,
) -> Result<()> {
    let store = StateStore::open(&db_path)?;
    let server = McpServer::with_store(config_path, db_path, store);
    let app = mcp_http_router(server, bearer_token);

    let listener = TcpListener::bind(bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(Error::Io)?;

    Ok(())
}

fn tool_json_response(id: Value, payload: &Value, is_error: bool) -> String {
    let text = serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string());
    serialize_result(
        id,
        json!({
            "content": [{ "type": "text", "text": text }],
            "isError": is_error
        }),
    )
}

fn build_extractor(params: &ExtractorParams) -> Result<ExtractorConfig> {
    let kind = params
        .extractor_type
        .as_deref()
        .unwrap_or("text")
        .to_ascii_lowercase();
    match kind.as_str() {
        "text" => Ok(ExtractorConfig::Text),
        "css" => {
            let selector = params.selector.clone().ok_or_else(|| {
                Error::Validation("'selector' is required when extractor_type is 'css'".to_string())
            })?;
            Ok(ExtractorConfig::Css {
                selector,
                attribute: params.attribute.clone(),
                all: params.all.unwrap_or(false),
            })
        }
        "json_path" | "jsonpath" => {
            let path = params.path.clone().ok_or_else(|| {
                Error::Validation(
                    "'path' is required when extractor_type is 'json_path'".to_string(),
                )
            })?;
            Ok(ExtractorConfig::JsonPath { path })
        }
        "regex" => {
            let pattern = params.pattern.clone().ok_or_else(|| {
                Error::Validation(
                    "'pattern' is required when extractor_type is 'regex'".to_string(),
                )
            })?;
            Ok(ExtractorConfig::Regex {
                pattern,
                group: params.group,
            })
        }
        "status" => Ok(ExtractorConfig::Status {
            header: params.header.clone(),
        }),
        other => Err(Error::Validation(format!(
            "unsupported extractor_type '{other}': expected text, css, json_path, regex, or status"
        ))),
    }
}

fn parse_args<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|err| Error::Validation(format!("invalid tool arguments: {err}")))
}

fn default_context_lines() -> usize {
    3
}

fn default_history_limit() -> usize {
    10
}

fn default_export_limit() -> usize {
    50
}

fn default_timeout_secs() -> u64 {
    15
}

fn serialize_result(id: Value, result: Value) -> String {
    serde_json::to_string(&RpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    })
    .expect("serializing valid JSON-RPC response")
}

fn serialize_error(id: Value, code: i64, message: String) -> String {
    serde_json::to_string(&RpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(RpcError { code, message }),
    })
    .expect("serializing valid JSON-RPC error")
}

fn resource_list() -> Value {
    json!([
        {
            "uri": "webdelta://watches",
            "name": "Configured Watches",
            "description": "JSON list of configured watches and their latest recorded snapshots.",
            "mimeType": "application/json"
        },
        {
            "uri": "webdelta://config",
            "name": "Redacted Configuration",
            "description": "Current webdelta.toml configuration with literal secrets redacted.",
            "mimeType": "text/plain"
        }
    ])
}

fn resource_templates() -> Value {
    json!([
        {
            "uriTemplate": "webdelta://history/{watch_id}",
            "name": "Watch Snapshot History",
            "description": "Recorded snapshot history for a watch ID.",
            "mimeType": "application/json"
        },
        {
            "uriTemplate": "webdelta://diff/{watch_id}",
            "name": "Watch Latest Diff",
            "description": "Unified diff and numeric delta between the two most recent snapshots.",
            "mimeType": "application/json"
        }
    ])
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "list_watches",
            "description": "List configured web and API watches along with their latest recorded snapshots.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "include_disabled": {
                        "type": "boolean",
                        "description": "Include watches where enabled is false. Defaults to false."
                    }
                }
            }
        },
        {
            "name": "add_watch",
            "description": "Add a new web or API watch to the webdelta configuration file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Unique watch identifier (ASCII letters, digits, '.', '-', '_')." },
                    "url": { "type": "string", "description": "Target HTTP or HTTPS URL." },
                    "name": { "type": "string", "description": "Optional human-friendly display name." },
                    "interval": { "type": "string", "description": "Polling interval such as '15m', '1h', or '30s'." },
                    "engine": {
                        "type": "string",
                        "enum": ["http", "browser", "auto"],
                        "description": "Fetch engine: 'http' (default), 'browser' (headless CDP), or 'auto' (HTTP with SPA browser fallback)."
                    },
                    "wait_for_selector": { "type": "string", "description": "CSS selector to wait for when using 'browser' or 'auto' engine." },
                    "wait_ms": { "type": "integer", "description": "Extra milliseconds to wait after page load in browser mode." },
                    "cdp_url": { "type": "string", "description": "Optional remote CDP HTTP or WebSocket URL." },
                    "click_selector": { "type": "string", "description": "Optional CSS selector clicked before reading DOM in browser mode." },
                    "scroll_to_bottom": { "type": "boolean", "description": "Scroll to page bottom before reading DOM in browser mode." },
                    "extractor_type": {
                        "type": "string",
                        "enum": ["text", "css", "json_path", "regex", "status"],
                        "description": "Extraction mode. Defaults to 'text'."
                    },
                    "selector": { "type": "string", "description": "CSS selector when extractor_type is 'css'." },
                    "attribute": { "type": "string", "description": "Optional HTML attribute to extract when using 'css'." },
                    "all": { "type": "boolean", "description": "Extract all CSS matches joined by newline." },
                    "path": { "type": "string", "description": "JSONPath expression when extractor_type is 'json_path'." },
                    "pattern": { "type": "string", "description": "Regex pattern when extractor_type is 'regex'." },
                    "group": { "type": "integer", "description": "Capture group index for 'regex'." },
                    "header": { "type": "string", "description": "Optional header name for 'status'." },
                    "ignore_regex": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Regex patterns stripped from extracted text before comparison."
                    },
                    "conditions": {
                        "type": "array",
                        "items": { "type": "object" },
                        "description": "Alert rule objects such as {\"type\": \"drop_pct_at_least\", \"percent\": 10}."
                    },
                    "notifiers": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Notifier names defined in configuration."
                    }
                },
                "required": ["id", "url"]
            }
        },
        {
            "name": "update_watch",
            "description": "Patch fields on an existing watch (interval, enabled status, engine, extractor, browser options, or rules).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Existing watch identifier to update." },
                    "name": { "type": "string" },
                    "url": { "type": "string" },
                    "interval": { "type": "string" },
                    "enabled": { "type": "boolean" },
                    "engine": {
                        "type": "string",
                        "enum": ["http", "browser", "auto"]
                    },
                    "wait_for_selector": { "type": "string" },
                    "wait_ms": { "type": "integer" },
                    "cdp_url": { "type": "string" },
                    "click_selector": { "type": "string" },
                    "scroll_to_bottom": { "type": "boolean" },
                    "extractor_type": {
                        "type": "string",
                        "enum": ["text", "css", "json_path", "regex", "status"]
                    },
                    "selector": { "type": "string" },
                    "attribute": { "type": "string" },
                    "all": { "type": "boolean" },
                    "path": { "type": "string" },
                    "pattern": { "type": "string" },
                    "group": { "type": "integer" },
                    "header": { "type": "string" },
                    "ignore_regex": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "conditions": {
                        "type": "array",
                        "items": { "type": "object" }
                    },
                    "notifiers": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["id"]
            }
        },
        {
            "name": "remove_watch",
            "description": "Remove a watch from the configuration and optionally purge stored snapshots.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Watch identifier to remove." },
                    "purge_history": { "type": "boolean", "description": "Delete stored snapshots for this watch." }
                },
                "required": ["id"]
            }
        },
        {
            "name": "check_now",
            "description": "Execute a check immediately for a specific watch or all enabled watches.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "watch_id": { "type": "string", "description": "Optional watch id. Omit to check all enabled watches." },
                    "notify": { "type": "boolean", "description": "Dispatch configured alerts when changes match conditions. Defaults to false." }
                }
            }
        },
        {
            "name": "get_diff",
            "description": "Compare the two most recent stored snapshots for a watch and return unified diff and numeric delta.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "watch_id": { "type": "string", "description": "Watch identifier to inspect." },
                    "context_lines": { "type": "integer", "description": "Unified diff context radius. Defaults to 3." }
                },
                "required": ["watch_id"]
            }
        },
        {
            "name": "get_history",
            "description": "Fetch recorded snapshot history for a watch, ordered newest first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "watch_id": { "type": "string", "description": "Watch identifier to query." },
                    "limit": { "type": "integer", "description": "Maximum number of snapshots to return. Defaults to 10." }
                },
                "required": ["watch_id"]
            }
        },
        {
            "name": "preview_extract",
            "description": "Fetch a URL once and test content extraction and numeric parsing without saving state.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "Target HTTP or HTTPS URL." },
                    "engine": {
                        "type": "string",
                        "enum": ["http", "browser", "auto"],
                        "description": "Fetch engine: 'http' (default), 'browser' (headless CDP), or 'auto' (HTTP with SPA browser fallback)."
                    },
                    "wait_for_selector": { "type": "string" },
                    "wait_ms": { "type": "integer" },
                    "cdp_url": { "type": "string" },
                    "click_selector": { "type": "string" },
                    "scroll_to_bottom": { "type": "boolean" },
                    "extractor_type": {
                        "type": "string",
                        "enum": ["text", "css", "json_path", "regex", "status"]
                    },
                    "selector": { "type": "string" },
                    "attribute": { "type": "string" },
                    "all": { "type": "boolean" },
                    "path": { "type": "string" },
                    "pattern": { "type": "string" },
                    "group": { "type": "integer" },
                    "header": { "type": "string" },
                    "ignore_regex": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "timeout_secs": { "type": "integer", "description": "Request timeout in seconds. Defaults to 15." }
                },
                "required": ["url"]
            }
        },
        {
            "name": "test_notifier",
            "description": "Send a synthetic test alert event through a configured notifier.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "notifier": { "type": "string", "description": "Notifier name defined under [notifiers] in webdelta.toml." }
                },
                "required": ["notifier"]
            }
        },
        {
            "name": "export_history",
            "description": "Export stored snapshot history for a watch as JSON, CSV, or RSS XML.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "watch_id": { "type": "string", "description": "Watch identifier to export." },
                    "format": {
                        "type": "string",
                        "enum": ["json", "csv", "rss"],
                        "description": "Export output format."
                    },
                    "limit": { "type": "integer", "description": "Maximum snapshots to export. Defaults to 50." }
                },
                "required": ["watch_id", "format"]
            }
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_mcp_protocol_tools_resources_and_update() {
        let dir = tempdir().unwrap();
        let server = McpServer::new(
            dir.path().join("webdelta.toml"),
            dir.path().join("state.redb"),
        );

        let init_reply = server
            .handle_line(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#,
            )
            .await
            .unwrap();
        let init_json: Value = serde_json::from_str(&init_reply).unwrap();
        assert_eq!(init_json["id"], 1);
        assert_eq!(init_json["result"]["protocolVersion"], "2025-03-26");
        assert!(init_json["result"]["capabilities"]["resources"].is_object());

        assert!(server
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await
            .is_none());

        let list_reply = server
            .handle_line(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
            .await
            .unwrap();
        let list_json: Value = serde_json::from_str(&list_reply).unwrap();
        let tools = list_json["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 10);

        let res_list = server
            .handle_line(r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#)
            .await
            .unwrap();
        let res_list_json: Value = serde_json::from_str(&res_list).unwrap();
        assert_eq!(
            res_list_json["result"]["resources"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let tmpl_list = server
            .handle_line(r#"{"jsonrpc":"2.0","id":4,"method":"resources/templates/list"}"#)
            .await
            .unwrap();
        let tmpl_json: Value = serde_json::from_str(&tmpl_list).unwrap();
        assert_eq!(
            tmpl_json["result"]["resourceTemplates"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let add_reply = server
            .handle_line(
                r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"add_watch","arguments":{"id":"demo","url":"https://example.com"}}}"#,
            )
            .await
            .unwrap();
        let add_json: Value = serde_json::from_str(&add_reply).unwrap();
        assert_eq!(add_json["result"]["isError"], false);

        let upd_reply = server
            .handle_line(
                r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"update_watch","arguments":{"id":"demo","interval":"5m","enabled":false}}}"#,
            )
            .await
            .unwrap();
        let upd_json: Value = serde_json::from_str(&upd_reply).unwrap();
        assert_eq!(upd_json["result"]["isError"], false);
        let upd_body: Value =
            serde_json::from_str(upd_json["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(upd_body["watch"]["interval"], "5m");
        assert_eq!(upd_body["watch"]["enabled"], false);

        let read_watches = server
            .handle_line(
                r#"{"jsonrpc":"2.0","id":7,"method":"resources/read","params":{"uri":"webdelta://watches"}}"#,
            )
            .await
            .unwrap();
        let rw_json: Value = serde_json::from_str(&read_watches).unwrap();
        let watches_txt = rw_json["result"]["contents"][0]["text"].as_str().unwrap();
        assert!(watches_txt.contains("demo"));

        let read_cfg = server
            .handle_line(
                r#"{"jsonrpc":"2.0","id":8,"method":"resources/read","params":{"uri":"webdelta://config"}}"#,
            )
            .await
            .unwrap();
        let cfg_json: Value = serde_json::from_str(&read_cfg).unwrap();
        assert_eq!(cfg_json["result"]["contents"][0]["mimeType"], "text/plain");
    }
}
