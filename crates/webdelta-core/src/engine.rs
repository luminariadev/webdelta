use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tracing::debug;
use webdelta_browser::{looks_like_spa_shell, render_page, BrowserOptions};

use crate::config::{
    expand_env_vars, BrowserConfig, Config, ExtractorConfig, FetchEngine, HttpMethod,
    NormalizeConfig, WatchConfig,
};
use crate::diff::{compute_diff, DiffResult};
use crate::error::{Error, Result};
use crate::extract::extract_content;
use crate::fetch::{FetchOutput, FetchRequest, HttpFetcher};
use crate::notify::{send_notifications, AlertEvent, NotificationResult};
use crate::numeric::{compute_delta, parse_numeric, NumericDelta};
use crate::rules::evaluate_conditions;
use crate::store::{SnapshotRecord, StateStore};

/// Outcome classification for a single watch check.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Initial,
    Unchanged,
    NotModified,
    Changed,
    RuleNotMet,
    Error,
}

impl CheckStatus {
    /// Returns a lowercase string label for terminal and log display.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Unchanged => "unchanged",
            Self::NotModified => "not_modified",
            Self::Changed => "changed",
            Self::RuleNotMet => "rule_not_met",
            Self::Error => "error",
        }
    }
}

/// Complete report for a single watch check execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CheckOutcome {
    pub watch_id: String,
    pub watch_name: String,
    pub url: String,
    pub engine_used: String,
    pub status: CheckStatus,
    pub status_code: Option<u16>,
    pub elapsed_ms: u64,
    pub previous_value: Option<String>,
    pub current_value: Option<String>,
    pub numeric_value: Option<f64>,
    pub numeric_delta: Option<NumericDelta>,
    pub diff: Option<DiffResult>,
    pub matched_rules: Vec<String>,
    pub notifications: Vec<NotificationResult>,
    pub error: Option<String>,
}

/// Result returned by a one-off extraction preview without modifying stored state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PreviewResult {
    pub url: String,
    pub engine_used: String,
    pub status_code: u16,
    pub elapsed_ms: u64,
    pub extracted_text: String,
    pub numeric_value: Option<f64>,
    pub content_hash: String,
}

enum FetchExtractOutcome {
    NotModified {
        elapsed_ms: u64,
    },
    Extracted {
        status_code: u16,
        etag: Option<String>,
        last_modified: Option<String>,
        extracted_text: String,
        elapsed_ms: u64,
        engine_used: &'static str,
    },
}

/// Coordinates HTTP/browser fetching, extraction, diffing, state storage, and notifications.
#[derive(Debug, Clone)]
pub struct MonitorEngine {
    fetcher: HttpFetcher,
    store: StateStore,
}

impl MonitorEngine {
    /// Creates an engine backed by the given `StateStore`.
    pub fn new(store: StateStore) -> Result<Self> {
        Ok(Self {
            fetcher: HttpFetcher::new()?,
            store,
        })
    }

    /// Returns a reference to the underlying `StateStore`.
    pub fn store(&self) -> &StateStore {
        &self.store
    }

    /// Fetches a URL once and previews extraction and numeric parsing without saving state.
    #[allow(clippy::too_many_arguments)]
    pub async fn preview(
        url: &str,
        extractor: &ExtractorConfig,
        ignore_regex: &[String],
        normalize: &NormalizeConfig,
        timeout_secs: u64,
        engine: FetchEngine,
        browser: &BrowserConfig,
        headers: &BTreeMap<String, String>,
    ) -> Result<PreviewResult> {
        let fetcher = HttpFetcher::new()?;
        let ua = format!("webdelta/{}", env!("CARGO_PKG_VERSION"));
        let res = fetch_and_extract(
            &fetcher,
            url,
            HttpMethod::Get,
            headers,
            &ua,
            timeout_secs.max(1),
            1,
            None,
            None,
            extractor,
            ignore_regex,
            normalize,
            engine,
            browser,
        )
        .await?;

        let FetchExtractOutcome::Extracted {
            status_code,
            extracted_text,
            elapsed_ms,
            engine_used,
            ..
        } = res
        else {
            return Err(Error::Http(
                "unexpected 304 response during preview".to_string(),
            ));
        };

        let numeric_value = parse_numeric(&extracted_text);
        let content_hash = sha256_hex(&extracted_text);

        Ok(PreviewResult {
            url: url.to_string(),
            engine_used: engine_used.to_string(),
            status_code,
            elapsed_ms,
            extracted_text,
            numeric_value,
            content_hash,
        })
    }

    /// Evaluates a single watch, records state transitions, and optionally dispatches alerts.
    pub async fn check_watch(
        &self,
        watch: &WatchConfig,
        config: &Config,
        send_alerts: bool,
    ) -> CheckOutcome {
        let previous = match self.store.get_latest(&watch.id) {
            Ok(prev) => prev,
            Err(err) => {
                return error_outcome(watch, watch.engine.as_str(), None, 0, err.to_string())
            }
        };

        let outcome = fetch_and_extract(
            &self.fetcher,
            &watch.url,
            watch.method,
            &watch.headers,
            &config.defaults.user_agent,
            watch.effective_timeout_secs(&config.defaults),
            watch.effective_retries(&config.defaults),
            previous.as_ref().and_then(|p| p.etag.as_deref()),
            previous.as_ref().and_then(|p| p.last_modified.as_deref()),
            &watch.extractor,
            &watch.ignore_regex,
            &watch.normalize,
            watch.engine,
            &watch.browser,
        )
        .await;

        let extracted_res = match outcome {
            Ok(res) => res,
            Err(err) => {
                return error_outcome(watch, watch.engine.as_str(), None, 0, err.to_string())
            }
        };

        let checked_at = Utc::now();

        match extracted_res {
            FetchExtractOutcome::NotModified { elapsed_ms } => {
                if let Err(err) = self
                    .store
                    .update_latest_metadata(&watch.id, checked_at, 304, elapsed_ms, None, None)
                {
                    return error_outcome(watch, "http", Some(304), elapsed_ms, err.to_string());
                }
                let prev_text = previous.as_ref().map(|p| p.extracted_text.clone());
                let prev_num = previous.as_ref().and_then(|p| p.numeric_value);
                CheckOutcome {
                    watch_id: watch.id.clone(),
                    watch_name: watch.display_name().to_string(),
                    url: watch.url.clone(),
                    engine_used: "http".to_string(),
                    status: CheckStatus::NotModified,
                    status_code: Some(304),
                    elapsed_ms,
                    previous_value: prev_text.clone(),
                    current_value: prev_text,
                    numeric_value: prev_num,
                    numeric_delta: None,
                    diff: None,
                    matched_rules: Vec::new(),
                    notifications: Vec::new(),
                    error: None,
                }
            }
            FetchExtractOutcome::Extracted {
                status_code,
                etag,
                last_modified,
                extracted_text,
                elapsed_ms,
                engine_used,
            } => {
                let numeric_value = parse_numeric(&extracted_text);
                let content_hash = sha256_hex(&extracted_text);

                let record = SnapshotRecord {
                    watch_id: watch.id.clone(),
                    checked_at,
                    status_code,
                    etag,
                    last_modified,
                    content_hash,
                    extracted_text: extracted_text.clone(),
                    numeric_value,
                    elapsed_ms,
                };

                match previous {
                    None => {
                        if let Err(err) = self
                            .store
                            .record_snapshot(&record, config.defaults.max_history)
                        {
                            return error_outcome(
                                watch,
                                engine_used,
                                Some(status_code),
                                elapsed_ms,
                                err.to_string(),
                            );
                        }
                        CheckOutcome {
                            watch_id: watch.id.clone(),
                            watch_name: watch.display_name().to_string(),
                            url: watch.url.clone(),
                            engine_used: engine_used.to_string(),
                            status: CheckStatus::Initial,
                            status_code: Some(status_code),
                            elapsed_ms,
                            previous_value: None,
                            current_value: Some(extracted_text),
                            numeric_value,
                            numeric_delta: None,
                            diff: None,
                            matched_rules: Vec::new(),
                            notifications: Vec::new(),
                            error: None,
                        }
                    }
                    Some(prev) if prev.extracted_text == extracted_text => {
                        if let Err(err) = self.store.update_latest_metadata(
                            &watch.id,
                            checked_at,
                            status_code,
                            elapsed_ms,
                            record.etag.clone(),
                            record.last_modified.clone(),
                        ) {
                            return error_outcome(
                                watch,
                                engine_used,
                                Some(status_code),
                                elapsed_ms,
                                err.to_string(),
                            );
                        }
                        CheckOutcome {
                            watch_id: watch.id.clone(),
                            watch_name: watch.display_name().to_string(),
                            url: watch.url.clone(),
                            engine_used: engine_used.to_string(),
                            status: CheckStatus::Unchanged,
                            status_code: Some(status_code),
                            elapsed_ms,
                            previous_value: Some(prev.extracted_text),
                            current_value: Some(extracted_text),
                            numeric_value,
                            numeric_delta: None,
                            diff: None,
                            matched_rules: Vec::new(),
                            notifications: Vec::new(),
                            error: None,
                        }
                    }
                    Some(prev) => {
                        if let Err(err) = self
                            .store
                            .record_snapshot(&record, config.defaults.max_history)
                        {
                            return error_outcome(
                                watch,
                                engine_used,
                                Some(status_code),
                                elapsed_ms,
                                err.to_string(),
                            );
                        }

                        let delta = match (prev.numeric_value, numeric_value) {
                            (Some(p), Some(c)) => Some(compute_delta(p, c)),
                            _ => None,
                        };
                        let diff = compute_diff(&prev.extracted_text, &extracted_text, 3);

                        let eval = match evaluate_conditions(
                            &watch.conditions,
                            &prev.extracted_text,
                            &extracted_text,
                            delta.as_ref(),
                        ) {
                            Ok(res) => res,
                            Err(err) => {
                                return error_outcome(
                                    watch,
                                    engine_used,
                                    Some(status_code),
                                    elapsed_ms,
                                    err.to_string(),
                                )
                            }
                        };

                        if !eval.triggered {
                            return CheckOutcome {
                                watch_id: watch.id.clone(),
                                watch_name: watch.display_name().to_string(),
                                url: watch.url.clone(),
                                engine_used: engine_used.to_string(),
                                status: CheckStatus::RuleNotMet,
                                status_code: Some(status_code),
                                elapsed_ms,
                                previous_value: Some(prev.extracted_text),
                                current_value: Some(extracted_text),
                                numeric_value,
                                numeric_delta: delta,
                                diff: Some(diff),
                                matched_rules: Vec::new(),
                                notifications: Vec::new(),
                                error: None,
                            };
                        }

                        let notifications = if send_alerts && !watch.notifiers.is_empty() {
                            let event = AlertEvent {
                                event: "watch.changed".to_string(),
                                watch_id: watch.id.clone(),
                                watch_name: watch.display_name().to_string(),
                                url: watch.url.clone(),
                                checked_at,
                                previous_value: prev.extracted_text.clone(),
                                current_value: extracted_text.clone(),
                                numeric_delta: delta.clone(),
                                diff_summary: diff.summary.clone(),
                                diff: diff.unified.clone(),
                                matched_rules: eval.reasons.clone(),
                            };
                            send_notifications(&config.notifiers, &watch.notifiers, &event).await
                        } else {
                            Vec::new()
                        };

                        CheckOutcome {
                            watch_id: watch.id.clone(),
                            watch_name: watch.display_name().to_string(),
                            url: watch.url.clone(),
                            engine_used: engine_used.to_string(),
                            status: CheckStatus::Changed,
                            status_code: Some(status_code),
                            elapsed_ms,
                            previous_value: Some(prev.extracted_text),
                            current_value: Some(extracted_text),
                            numeric_value,
                            numeric_delta: delta,
                            diff: Some(diff),
                            matched_rules: eval.reasons,
                            notifications,
                            error: None,
                        }
                    }
                }
            }
        }
    }

    /// Checks selected watches (or all enabled watches) concurrently in config order.
    pub async fn check_watches(
        &self,
        config: &Config,
        filter_ids: Option<&[String]>,
        concurrency: usize,
        send_alerts: bool,
    ) -> Result<Vec<CheckOutcome>> {
        let selected: Vec<(usize, WatchConfig)> = match filter_ids {
            Some(ids) if !ids.is_empty() => {
                for id in ids {
                    if !config.watches.iter().any(|w| &w.id == id) {
                        return Err(Error::Validation(format!("watch '{id}' not found")));
                    }
                }
                config
                    .watches
                    .iter()
                    .enumerate()
                    .filter(|(_, w)| ids.iter().any(|id| id == &w.id))
                    .map(|(idx, w)| (idx, w.clone()))
                    .collect()
            }
            _ => config
                .watches
                .iter()
                .enumerate()
                .filter(|(_, w)| w.enabled)
                .map(|(idx, w)| (idx, w.clone()))
                .collect(),
        };

        let sem = Arc::new(Semaphore::new(concurrency.max(1)));
        let cfg = Arc::new(config.clone());
        let mut handles = Vec::with_capacity(selected.len());

        for (idx, watch) in selected {
            let permit_sem = Arc::clone(&sem);
            let engine = self.clone();
            let cfg_ref = Arc::clone(&cfg);
            handles.push(tokio::spawn(async move {
                let _permit = permit_sem.acquire_owned().await.expect("semaphore open");
                let outcome = engine.check_watch(&watch, &cfg_ref, send_alerts).await;
                (idx, outcome)
            }));
        }

        let mut indexed = Vec::with_capacity(handles.len());
        for handle in handles {
            let pair = handle
                .await
                .map_err(|err| Error::Http(format!("check task panicked: {err}")))?;
            indexed.push(pair);
        }
        indexed.sort_by_key(|(idx, _)| *idx);
        Ok(indexed.into_iter().map(|(_, outcome)| outcome).collect())
    }
}

#[allow(clippy::too_many_arguments)]
async fn fetch_and_extract(
    fetcher: &HttpFetcher,
    url: &str,
    method: HttpMethod,
    headers: &BTreeMap<String, String>,
    user_agent: &str,
    timeout_secs: u64,
    retries: usize,
    etag: Option<&str>,
    last_modified: Option<&str>,
    extractor: &ExtractorConfig,
    ignore_regex: &[String],
    normalize: &NormalizeConfig,
    engine: FetchEngine,
    browser: &BrowserConfig,
) -> Result<FetchExtractOutcome> {
    if engine == FetchEngine::Browser {
        let rendered = render_via_browser(url, timeout_secs, user_agent, headers, browser).await?;
        let extracted_text = extract_content(
            &rendered.html,
            200,
            &BTreeMap::new(),
            extractor,
            ignore_regex,
            normalize,
        )?;
        return Ok(FetchExtractOutcome::Extracted {
            status_code: 200,
            etag: None,
            last_modified: None,
            extracted_text,
            elapsed_ms: rendered.elapsed_ms,
            engine_used: "browser",
        });
    }

    let output = fetcher
        .fetch(FetchRequest {
            url,
            method,
            headers,
            user_agent,
            timeout: Duration::from_secs(timeout_secs.max(1)),
            retries,
            etag,
            last_modified,
        })
        .await?;

    let (status, resp_headers, body, etag, last_modified, elapsed_ms) = match output {
        FetchOutput::NotModified { elapsed_ms } => {
            return Ok(FetchExtractOutcome::NotModified { elapsed_ms });
        }
        FetchOutput::Response {
            status,
            headers,
            body,
            etag,
            last_modified,
            elapsed_ms,
        } => (status, headers, body, etag, last_modified, elapsed_ms),
    };

    if status >= 400 && !matches!(extractor, ExtractorConfig::Status { .. }) {
        return Err(Error::Http(format!("HTTP {status} returned from '{url}'")));
    }

    let http_extract = extract_content(
        &body,
        status,
        &resp_headers,
        extractor,
        ignore_regex,
        normalize,
    );

    if engine == FetchEngine::Auto && !matches!(extractor, ExtractorConfig::Status { .. }) {
        let needs_browser = looks_like_spa_shell(&body)
            || match &http_extract {
                Ok(text) => text.trim().is_empty(),
                Err(Error::Extract(_)) => true,
                Err(_) => false,
            };

        if needs_browser {
            debug!(url = %url, "auto engine falling back to headless browser");
            let rendered =
                render_via_browser(url, timeout_secs, user_agent, headers, browser).await?;
            let extracted_text = extract_content(
                &rendered.html,
                status,
                &resp_headers,
                extractor,
                ignore_regex,
                normalize,
            )?;
            return Ok(FetchExtractOutcome::Extracted {
                status_code: status,
                etag,
                last_modified,
                extracted_text,
                elapsed_ms: elapsed_ms + rendered.elapsed_ms,
                engine_used: "browser",
            });
        }
    }

    let extracted_text = http_extract?;
    Ok(FetchExtractOutcome::Extracted {
        status_code: status,
        etag,
        last_modified,
        extracted_text,
        elapsed_ms,
        engine_used: "http",
    })
}

async fn render_via_browser(
    url: &str,
    timeout_secs: u64,
    user_agent: &str,
    headers: &BTreeMap<String, String>,
    browser: &BrowserConfig,
) -> Result<webdelta_browser::RenderedPage> {
    let opts = BrowserOptions {
        cdp_url: browser.cdp_url.as_deref().map(expand_env_vars),
        executable_path: None,
        wait_for_selector: browser.wait_for_selector.clone(),
        wait_ms: browser.wait_ms.unwrap_or(0),
        timeout_secs: timeout_secs.max(10),
        user_agent: Some(user_agent.to_string()),
        headers: headers.clone(),
    };
    render_page(url, &opts)
        .await
        .map_err(|err| Error::Http(format!("browser render error: {err}")))
}

fn error_outcome(
    watch: &WatchConfig,
    engine_used: &str,
    status_code: Option<u16>,
    elapsed_ms: u64,
    message: String,
) -> CheckOutcome {
    CheckOutcome {
        watch_id: watch.id.clone(),
        watch_name: watch.display_name().to_string(),
        url: watch.url.clone(),
        engine_used: engine_used.to_string(),
        status: CheckStatus::Error,
        status_code,
        elapsed_ms,
        previous_value: None,
        current_value: None,
        numeric_value: None,
        numeric_delta: None,
        diff: None,
        matched_rules: Vec::new(),
        notifications: Vec::new(),
        error: Some(message),
    }
}

/// Computes lowercase hex SHA-256 of `input`.
pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}
