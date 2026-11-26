use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::{watch, Mutex};
use tokio::time::Instant;
use tracing::{error, info};
use webdelta_core::{
    compute_delta, compute_diff, export_history, load_config, save_config, CheckStatus, Error,
    ExportFormat, MonitorEngine, Result, StateStore,
};
use webdelta_mcp::{is_bearer_authorized, mcp_route, McpServer};

const DASHBOARD_HTML: &str = include_str!("dashboard.html");

#[derive(Clone)]
struct AppState {
    config_path: PathBuf,
    store: StateStore,
    config_lock: Arc<Mutex<()>>,
    bearer_token: Option<String>,
}

enum ApiError {
    Core(Error),
    Unauthorized,
}

impl From<Error> for ApiError {
    fn from(value: Error) -> Self {
        Self::Core(value)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "unauthorized" })),
            )
                .into_response(),
            Self::Core(err) => {
                let status = match &err {
                    Error::Validation(msg) if msg.contains("not found") => StatusCode::NOT_FOUND,
                    Error::Validation(_) | Error::Config(_) => StatusCode::BAD_REQUEST,
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };
                (status, Json(json!({ "error": err.to_string() }))).into_response()
            }
        }
    }
}

#[derive(Deserialize, Default)]
struct LimitQuery {
    limit: Option<usize>,
}

#[derive(Deserialize, Default)]
struct ExportQuery {
    format: Option<String>,
    limit: Option<usize>,
}

/// Runs the embedded web dashboard, REST API, Streamable HTTP MCP endpoint, and optional scheduler.
pub async fn run_serve(
    config_path: PathBuf,
    db_path: PathBuf,
    bind: &str,
    bearer_token: Option<String>,
    background_watch: bool,
    concurrency_override: Option<usize>,
) -> Result<()> {
    let store = StateStore::open(&db_path)?;
    let mcp = McpServer::with_store(config_path.clone(), db_path.clone(), store.clone());
    let config_lock = Arc::new(Mutex::new(()));

    let state = AppState {
        config_path: config_path.clone(),
        store: store.clone(),
        config_lock: Arc::clone(&config_lock),
        bearer_token: bearer_token.clone(),
    };

    let api_router = Router::new()
        .route("/", get(get_dashboard))
        .route("/health", get(get_health))
        .route("/api/watches", get(api_list_watches))
        .route("/api/check", post(api_check_all))
        .route("/api/watches/{id}/check", post(api_check_one))
        .route("/api/watches/{id}/toggle", post(api_toggle_watch))
        .route("/api/watches/{id}/diff", get(api_get_diff))
        .route("/api/watches/{id}/history", get(api_get_history))
        .route("/api/watches/{id}/export", get(api_export_history))
        .with_state(state)
        .merge(mcp_route(mcp, bearer_token));

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let bg_handle = if background_watch {
        let bg_config_path = config_path.clone();
        let bg_store = store.clone();
        let mut bg_rx = shutdown_rx.clone();
        Some(tokio::spawn(async move {
            run_background_scheduler(bg_config_path, bg_store, concurrency_override, &mut bg_rx)
                .await;
        }))
    } else {
        None
    };

    let listener = TcpListener::bind(bind).await?;
    println!("webdelta dashboard listening on http://{bind}");

    axum::serve(listener, api_router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = shutdown_tx.send(true);
        })
        .await
        .map_err(Error::Io)?;

    if let Some(handle) = bg_handle {
        let _ = handle.await;
    }

    Ok(())
}

async fn run_background_scheduler(
    config_path: PathBuf,
    store: StateStore,
    concurrency_override: Option<usize>,
    shutdown: &mut watch::Receiver<bool>,
) {
    let Ok(engine) = MonitorEngine::new(store) else {
        error!("failed to initialize background monitor engine");
        return;
    };

    let mut next_runs: std::collections::HashMap<String, Instant> =
        std::collections::HashMap::new();

    loop {
        if *shutdown.borrow() {
            break;
        }

        if let Ok(config) = load_config(&config_path) {
            let now = Instant::now();
            let mut due_ids = Vec::new();
            let mut next_wake = now + Duration::from_secs(30);

            for watch in config.watches.iter().filter(|w| w.enabled) {
                let Ok(interval) = watch.effective_interval(&config.defaults) else {
                    continue;
                };
                let due_at = next_runs.entry(watch.id.clone()).or_insert(now);
                if *due_at <= now {
                    due_ids.push(watch.id.clone());
                    *due_at = now + interval;
                }
                if *due_at < next_wake {
                    next_wake = *due_at;
                }
            }

            if !due_ids.is_empty() {
                let concurrency = concurrency_override.unwrap_or(config.defaults.concurrency);
                match engine
                    .check_watches(&config, Some(&due_ids), concurrency, true)
                    .await
                {
                    Ok(outcomes) => {
                        for outcome in outcomes {
                            info!(
                                watch_id = %outcome.watch_id,
                                status = %outcome.status.as_str(),
                                elapsed_ms = outcome.elapsed_ms,
                                "background check completed"
                            );
                        }
                    }
                    Err(err) => {
                        error!(error = %err, "background check batch failed");
                    }
                }
            }

            let sleep_for = next_wake
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(10));
            tokio::select! {
                _ = tokio::time::sleep(sleep_for) => {}
                _ = shutdown.changed() => break,
            }
        } else {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                _ = shutdown.changed() => break,
            }
        }
    }
}

fn check_auth(headers: &HeaderMap, state: &AppState) -> std::result::Result<(), ApiError> {
    if is_bearer_authorized(headers, state.bearer_token.as_deref()) {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

async fn get_dashboard() -> impl IntoResponse {
    Html(DASHBOARD_HTML)
}

async fn get_health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn api_list_watches(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    check_auth(&headers, &state)?;

    let config = if state.config_path.exists() {
        load_config(&state.config_path)?
    } else {
        Default::default()
    };

    let mut watches = Vec::with_capacity(config.watches.len());
    for watch in &config.watches {
        let latest = state.store.get_latest(&watch.id)?;
        watches.push(json!({
            "id": watch.id,
            "name": watch.display_name(),
            "url": watch.url,
            "interval": watch.interval.as_deref().unwrap_or(&config.defaults.interval),
            "enabled": watch.enabled,
            "engine": watch.engine,
            "extractor": watch.extractor,
            "conditions": watch.conditions,
            "latest": latest,
        }));
    }

    Ok(Json(json!({ "watches": watches })).into_response())
}

async fn api_check_all(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    check_auth(&headers, &state)?;

    let config = load_config(&state.config_path)?;
    let engine = MonitorEngine::new(state.store.clone())?;
    let outcomes = engine
        .check_watches(&config, None, config.defaults.concurrency, true)
        .await?;

    Ok(Json(json!({
        "count": outcomes.len(),
        "changed": outcomes.iter().filter(|o| o.status == CheckStatus::Changed).count(),
        "errors": outcomes.iter().filter(|o| o.status == CheckStatus::Error).count(),
        "results": outcomes,
    }))
    .into_response())
}

async fn api_check_one(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    check_auth(&headers, &state)?;

    let config = load_config(&state.config_path)?;
    let engine = MonitorEngine::new(state.store.clone())?;
    let ids = vec![id];
    let mut outcomes = engine.check_watches(&config, Some(&ids), 1, true).await?;

    let outcome = outcomes
        .pop()
        .ok_or_else(|| Error::Validation("watch not found".to_string()))?;
    Ok(Json(outcome).into_response())
}

async fn api_toggle_watch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    check_auth(&headers, &state)?;

    let _guard = state.config_lock.lock().await;
    let mut config = load_config(&state.config_path)?;
    let watch = config
        .watches
        .iter_mut()
        .find(|w| w.id == id)
        .ok_or_else(|| Error::Validation(format!("watch '{id}' not found")))?;

    watch.enabled = !watch.enabled;
    let new_state = watch.enabled;
    save_config(&state.config_path, &config)?;

    Ok(Json(json!({
        "id": id,
        "enabled": new_state,
    }))
    .into_response())
}

async fn api_get_diff(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    check_auth(&headers, &state)?;

    let Some((prev, curr)) = state.store.get_latest_pair(&id)? else {
        return Ok(Json(json!({
            "watch_id": id,
            "has_diff": false,
            "message": "fewer than two snapshots recorded"
        }))
        .into_response());
    };

    let diff = compute_diff(&prev.extracted_text, &curr.extracted_text, 3);
    let numeric_delta = match (prev.numeric_value, curr.numeric_value) {
        (Some(p), Some(c)) => Some(compute_delta(p, c)),
        _ => None,
    };

    Ok(Json(json!({
        "watch_id": id,
        "has_diff": prev.extracted_text != curr.extracted_text,
        "previous": prev,
        "current": curr,
        "numeric_delta": numeric_delta,
        "diff": diff,
    }))
    .into_response())
}

async fn api_get_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<LimitQuery>,
) -> std::result::Result<Response, ApiError> {
    check_auth(&headers, &state)?;

    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let snapshots = state.store.get_history(&id, limit)?;
    Ok(Json(json!({
        "watch_id": id,
        "count": snapshots.len(),
        "snapshots": snapshots,
    }))
    .into_response())
}

async fn api_export_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<ExportQuery>,
) -> std::result::Result<Response, ApiError> {
    check_auth(&headers, &state)?;

    let format = ExportFormat::from_str(query.format.as_deref().unwrap_or("json"))?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let records = state.store.get_history(&id, limit)?;
    let config = if state.config_path.exists() {
        load_config(&state.config_path)?
    } else {
        Default::default()
    };
    let watch = config.watches.iter().find(|w| w.id == id);
    let content = export_history(watch, &id, &records, format)?;

    let disposition = format!("attachment; filename=\"{id}.{}\"", format.extension());
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, format.content_type().to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        content,
    )
        .into_response())
}
