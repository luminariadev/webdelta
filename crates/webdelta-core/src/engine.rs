use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::config::{Config, ExtractorConfig, HttpMethod, NormalizeConfig, WatchConfig};
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
    pub status_code: u16,
    pub elapsed_ms: u64,
    pub extracted_text: String,
    pub numeric_value: Option<f64>,
    pub content_hash: String,
}

/// Coordinates HTTP fetching, extraction, diffing, state storage, and notifications.
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
    pub async fn preview(
        url: &str,
        extractor: &ExtractorConfig,
        ignore_regex: &[String],
        normalize: &NormalizeConfig,
        timeout_secs: u64,
    ) -> Result<PreviewResult> {
        let fetcher = HttpFetcher::new()?;
        let empty_headers = BTreeMap::new();
        let ua = format!("webdelta/{}", env!("CARGO_PKG_VERSION"));
        let output = fetcher
            .fetch(FetchRequest {
                url,
                method: HttpMethod::Get,
                headers: &empty_headers,
                user_agent: &ua,
                timeout: Duration::from_secs(timeout_secs.max(1)),
                retries: 1,
                etag: None,
                last_modified: None,
            })
            .await?;

        let FetchOutput::Response {
            status,
            headers,
            body,
            elapsed_ms,
            ..
        } = output
        else {
            return Err(Error::Http(
                "unexpected 304 response during preview".to_string(),
            ));
        };

        if status >= 400 && !matches!(extractor, ExtractorConfig::Status { .. }) {
            return Err(Error::Http(format!(
                "preview target '{url}' returned HTTP {status}"
            )));
        }

        let extracted =
            extract_content(&body, status, &headers, extractor, ignore_regex, normalize)?;
        let numeric_value = parse_numeric(&extracted);
        let content_hash = sha256_hex(&extracted);

        Ok(PreviewResult {
            url: url.to_string(),
            status_code: status,
            elapsed_ms,
            extracted_text: extracted,
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
            Err(err) => return error_outcome(watch, None, 0, err.to_string()),
        };

        let fetch_res = self
            .fetcher
            .fetch(FetchRequest {
                url: &watch.url,
                method: watch.method,
                headers: &watch.headers,
                user_agent: &config.defaults.user_agent,
                timeout: Duration::from_secs(watch.effective_timeout_secs(&config.defaults)),
                retries: watch.effective_retries(&config.defaults),
                etag: previous.as_ref().and_then(|p| p.etag.as_deref()),
                last_modified: previous.as_ref().and_then(|p| p.last_modified.as_deref()),
            })
            .await;

        let output = match fetch_res {
            Ok(out) => out,
            Err(err) => return error_outcome(watch, None, 0, err.to_string()),
        };

        let checked_at = Utc::now();

        match output {
            FetchOutput::NotModified { elapsed_ms } => {
                if let Err(err) = self
                    .store
                    .update_latest_metadata(&watch.id, checked_at, 304, elapsed_ms, None, None)
                {
                    return error_outcome(watch, Some(304), elapsed_ms, err.to_string());
                }
                let prev_text = previous.as_ref().map(|p| p.extracted_text.clone());
                let prev_num = previous.as_ref().and_then(|p| p.numeric_value);
                CheckOutcome {
                    watch_id: watch.id.clone(),
                    watch_name: watch.display_name().to_string(),
                    url: watch.url.clone(),
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
            FetchOutput::Response {
                status,
                headers,
                body,
                etag,
                last_modified,
                elapsed_ms,
            } => {
                if status >= 400 && !matches!(watch.extractor, ExtractorConfig::Status { .. }) {
                    return error_outcome(
                        watch,
                        Some(status),
                        elapsed_ms,
                        format!("HTTP {status} returned from '{}'", watch.url),
                    );
                }

                let extracted = match extract_content(
                    &body,
                    status,
                    &headers,
                    &watch.extractor,
                    &watch.ignore_regex,
                    &watch.normalize,
                ) {
                    Ok(text) => text,
                    Err(err) => {
                        return error_outcome(watch, Some(status), elapsed_ms, err.to_string())
                    }
                };

                let numeric_value = parse_numeric(&extracted);
                let content_hash = sha256_hex(&extracted);

                let record = SnapshotRecord {
                    watch_id: watch.id.clone(),
                    checked_at,
                    status_code: status,
                    etag,
                    last_modified,
                    content_hash,
                    extracted_text: extracted.clone(),
                    numeric_value,
                    elapsed_ms,
                };

                match previous {
                    None => {
                        if let Err(err) = self
                            .store
                            .record_snapshot(&record, config.defaults.max_history)
                        {
                            return error_outcome(watch, Some(status), elapsed_ms, err.to_string());
                        }
                        CheckOutcome {
                            watch_id: watch.id.clone(),
                            watch_name: watch.display_name().to_string(),
                            url: watch.url.clone(),
                            status: CheckStatus::Initial,
                            status_code: Some(status),
                            elapsed_ms,
                            previous_value: None,
                            current_value: Some(extracted),
                            numeric_value,
                            numeric_delta: None,
                            diff: None,
                            matched_rules: Vec::new(),
                            notifications: Vec::new(),
                            error: None,
                        }
                    }
                    Some(prev) if prev.extracted_text == extracted => {
                        // Update `latest` metadata without appending duplicate history.
                        if let Err(err) = self.store.update_latest_metadata(
                            &watch.id,
                            checked_at,
                            status,
                            elapsed_ms,
                            record.etag.clone(),
                            record.last_modified.clone(),
                        ) {
                            return error_outcome(watch, Some(status), elapsed_ms, err.to_string());
                        }
                        CheckOutcome {
                            watch_id: watch.id.clone(),
                            watch_name: watch.display_name().to_string(),
                            url: watch.url.clone(),
                            status: CheckStatus::Unchanged,
                            status_code: Some(status),
                            elapsed_ms,
                            previous_value: Some(prev.extracted_text),
                            current_value: Some(extracted),
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
                            return error_outcome(watch, Some(status), elapsed_ms, err.to_string());
                        }

                        let delta = match (prev.numeric_value, numeric_value) {
                            (Some(p), Some(c)) => Some(compute_delta(p, c)),
                            _ => None,
                        };
                        let diff = compute_diff(&prev.extracted_text, &extracted, 3);

                        let eval = match evaluate_conditions(
                            &watch.conditions,
                            &prev.extracted_text,
                            &extracted,
                            delta.as_ref(),
                        ) {
                            Ok(res) => res,
                            Err(err) => {
                                return error_outcome(
                                    watch,
                                    Some(status),
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
                                status: CheckStatus::RuleNotMet,
                                status_code: Some(status),
                                elapsed_ms,
                                previous_value: Some(prev.extracted_text),
                                current_value: Some(extracted),
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
                                current_value: extracted.clone(),
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
                            status: CheckStatus::Changed,
                            status_code: Some(status),
                            elapsed_ms,
                            previous_value: Some(prev.extracted_text),
                            current_value: Some(extracted),
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

fn error_outcome(
    watch: &WatchConfig,
    status_code: Option<u16>,
    elapsed_ms: u64,
    message: String,
) -> CheckOutcome {
    CheckOutcome {
        watch_id: watch.id.clone(),
        watch_name: watch.display_name().to_string(),
        url: watch.url.clone(),
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
