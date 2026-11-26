use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::config::{expand_env_vars_checked, NotifierConfig};
use crate::error::{Error, Result};
use crate::numeric::{compute_delta, NumericDelta};

type HmacSha256 = Hmac<Sha256>;

/// Structured alert event sent to configured notification channels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertEvent {
    pub event: String,
    pub watch_id: String,
    pub watch_name: String,
    pub url: String,
    pub checked_at: DateTime<Utc>,
    pub previous_value: String,
    pub current_value: String,
    pub numeric_delta: Option<NumericDelta>,
    pub diff_summary: String,
    pub diff: String,
    pub matched_rules: Vec<String>,
}

/// Delivery outcome for a single notifier invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotificationResult {
    pub notifier: String,
    pub kind: String,
    pub ok: bool,
    pub status_code: Option<u16>,
    pub error: Option<String>,
}

/// Dispatches `event` to each named notifier in `target_names`.
pub async fn send_notifications(
    notifiers: &BTreeMap<String, NotifierConfig>,
    target_names: &[String],
    event: &AlertEvent,
) -> Vec<NotificationResult> {
    if target_names.is_empty() {
        return Vec::new();
    }

    let client = match Client::builder().build() {
        Ok(c) => c,
        Err(err) => {
            return target_names
                .iter()
                .map(|name| NotificationResult {
                    notifier: name.clone(),
                    kind: "unknown".to_string(),
                    ok: false,
                    status_code: None,
                    error: Some(format!("failed to create notification HTTP client: {err}")),
                })
                .collect();
        }
    };

    let mut results = Vec::with_capacity(target_names.len());
    for name in target_names {
        let Some(cfg) = notifiers.get(name) else {
            results.push(NotificationResult {
                notifier: name.clone(),
                kind: "unknown".to_string(),
                ok: false,
                status_code: None,
                error: Some(format!("notifier '{name}' not found in configuration")),
            });
            continue;
        };

        let res = dispatch_single(&client, name, cfg, event).await;
        results.push(res);
    }

    results
}

/// Sends a synthetic test alert event to a single named notifier.
pub async fn test_notifier(
    notifiers: &BTreeMap<String, NotifierConfig>,
    name: &str,
) -> NotificationResult {
    let event = AlertEvent {
        event: "watch.test".to_string(),
        watch_id: "test".to_string(),
        watch_name: "Test Notification".to_string(),
        url: "https://example.com".to_string(),
        checked_at: Utc::now(),
        previous_value: "$100.00".to_string(),
        current_value: "$85.00".to_string(),
        numeric_delta: Some(compute_delta(100.0, 85.0)),
        diff_summary: "+1 / -1 lines".to_string(),
        diff: "--- previous\n+++ current\n@@ -1 +1 @@\n-$100.00\n+$85.00\n".to_string(),
        matched_rules: vec!["manual test notification".to_string()],
    };
    let targets = vec![name.to_string()];
    send_notifications(notifiers, &targets, &event)
        .await
        .into_iter()
        .next()
        .unwrap_or_else(|| NotificationResult {
            notifier: name.to_string(),
            kind: "unknown".to_string(),
            ok: false,
            status_code: None,
            error: Some("no notification result returned".to_string()),
        })
}

async fn dispatch_single(
    client: &Client,
    name: &str,
    config: &NotifierConfig,
    event: &AlertEvent,
) -> NotificationResult {
    let kind = notifier_kind(config).to_string();
    match dispatch_inner(client, config, event).await {
        Ok(status_code) => NotificationResult {
            notifier: name.to_string(),
            kind,
            ok: true,
            status_code: Some(status_code),
            error: None,
        },
        Err(err) => NotificationResult {
            notifier: name.to_string(),
            kind,
            ok: false,
            status_code: None,
            error: Some(err.to_string()),
        },
    }
}

fn notifier_kind(config: &NotifierConfig) -> &'static str {
    match config {
        NotifierConfig::Webhook { .. } => "webhook",
        NotifierConfig::Telegram { .. } => "telegram",
        NotifierConfig::Discord { .. } => "discord",
        NotifierConfig::Slack { .. } => "slack",
        NotifierConfig::Ntfy { .. } => "ntfy",
    }
}

async fn dispatch_inner(
    client: &Client,
    config: &NotifierConfig,
    event: &AlertEvent,
) -> Result<u16> {
    match config {
        NotifierConfig::Webhook {
            url,
            headers,
            secret,
        } => send_webhook(client, url, headers, secret.as_deref(), event).await,
        NotifierConfig::Telegram {
            bot_token,
            chat_id,
            api_base_url,
        } => send_telegram(client, bot_token, chat_id, api_base_url.as_deref(), event).await,
        NotifierConfig::Discord { webhook_url } => send_discord(client, webhook_url, event).await,
        NotifierConfig::Slack { webhook_url } => send_slack(client, webhook_url, event).await,
        NotifierConfig::Ntfy {
            url,
            topic,
            priority,
            tags,
            token,
        } => {
            send_ntfy(
                client,
                url,
                topic,
                priority.as_deref(),
                tags,
                token.as_deref(),
                event,
            )
            .await
        }
    }
}

async fn send_webhook(
    client: &Client,
    raw_url: &str,
    raw_headers: &BTreeMap<String, String>,
    raw_secret: Option<&str>,
    event: &AlertEvent,
) -> Result<u16> {
    let url = expand_env_vars_checked(raw_url, true)?;
    let body_bytes = serde_json::to_vec(event)
        .map_err(|err| Error::Notify(format!("failed to serialize webhook event: {err}")))?;

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    for (k, v) in raw_headers {
        let expanded = expand_env_vars_checked(v, true)?;
        let h_name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|err| Error::Notify(format!("invalid webhook header '{k}': {err}")))?;
        let h_val = HeaderValue::from_str(&expanded).map_err(|err| {
            Error::Notify(format!("invalid webhook header value for '{k}': {err}"))
        })?;
        headers.insert(h_name, h_val);
    }

    if let Some(secret_tpl) = raw_secret {
        let secret = expand_env_vars_checked(secret_tpl, true)?;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .map_err(|err| Error::Notify(format!("invalid HMAC secret: {err}")))?;
        mac.update(&body_bytes);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        let sig_val = HeaderValue::from_str(&signature)
            .map_err(|err| Error::Notify(format!("invalid signature header: {err}")))?;
        headers.insert(HeaderName::from_static("x-webdelta-signature"), sig_val);
    }

    let resp = client
        .post(&url)
        .headers(headers)
        .body(body_bytes)
        .send()
        .await
        .map_err(|err| Error::Notify(format!("webhook request failed: {err}")))?;

    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(Error::Notify(format!(
            "webhook returned HTTP {status}: {}",
            truncate(&text, 200)
        )));
    }
    Ok(status)
}

async fn send_telegram(
    client: &Client,
    raw_token: &str,
    raw_chat_id: &str,
    raw_base_url: Option<&str>,
    event: &AlertEvent,
) -> Result<u16> {
    let token = expand_env_vars_checked(raw_token, true)?;
    let chat_id = expand_env_vars_checked(raw_chat_id, true)?;
    let base = match raw_base_url {
        Some(b) => expand_env_vars_checked(b, true)?,
        None => "https://api.telegram.org".to_string(),
    };
    let endpoint = format!("{}/bot{}/sendMessage", base.trim_end_matches('/'), token);
    let text = truncate(&format_human_message(event), 3900);

    let payload = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "disable_web_page_preview": true
    });

    post_json(client, &endpoint, HeaderMap::new(), &payload, "telegram").await
}

async fn send_discord(client: &Client, raw_url: &str, event: &AlertEvent) -> Result<u16> {
    let url = expand_env_vars_checked(raw_url, true)?;
    let content = truncate(&format_human_message(event), 1900);
    let payload = serde_json::json!({ "content": content });
    post_json(client, &url, HeaderMap::new(), &payload, "discord").await
}

async fn send_slack(client: &Client, raw_url: &str, event: &AlertEvent) -> Result<u16> {
    let url = expand_env_vars_checked(raw_url, true)?;
    let text = truncate(&format_human_message(event), 3500);
    let payload = serde_json::json!({ "text": text });
    post_json(client, &url, HeaderMap::new(), &payload, "slack").await
}

async fn send_ntfy(
    client: &Client,
    raw_url: &str,
    raw_topic: &str,
    priority: Option<&str>,
    tags: &[String],
    raw_token: Option<&str>,
    event: &AlertEvent,
) -> Result<u16> {
    let base = expand_env_vars_checked(raw_url, true)?;
    let topic = expand_env_vars_checked(raw_topic, true)?;
    let endpoint = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        topic.trim_start_matches('/')
    );

    let mut headers = HeaderMap::new();
    let title = format!("webdelta: {}", event.watch_name);
    if let Ok(val) = HeaderValue::from_str(&title) {
        headers.insert(HeaderName::from_static("title"), val);
    }
    if let Some(p) = priority {
        if let Ok(val) = HeaderValue::from_str(p) {
            headers.insert(HeaderName::from_static("priority"), val);
        }
    }
    if !tags.is_empty() {
        let joined = tags.join(",");
        if let Ok(val) = HeaderValue::from_str(&joined) {
            headers.insert(HeaderName::from_static("tags"), val);
        }
    }
    if let Some(tok_tpl) = raw_token {
        let tok = expand_env_vars_checked(tok_tpl, true)?;
        let auth = format!("Bearer {tok}");
        let val = HeaderValue::from_str(&auth)
            .map_err(|err| Error::Notify(format!("invalid ntfy token header: {err}")))?;
        headers.insert(AUTHORIZATION, val);
    }

    let body = format_human_message(event);
    let resp = client
        .post(&endpoint)
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|err| Error::Notify(format!("ntfy request failed: {err}")))?;

    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(Error::Notify(format!(
            "ntfy returned HTTP {status}: {}",
            truncate(&text, 200)
        )));
    }
    Ok(status)
}

async fn post_json(
    client: &Client,
    url: &str,
    headers: HeaderMap,
    payload: &serde_json::Value,
    label: &str,
) -> Result<u16> {
    let resp = client
        .post(url)
        .headers(headers)
        .json(payload)
        .send()
        .await
        .map_err(|err| Error::Notify(format!("{label} request failed: {err}")))?;

    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(Error::Notify(format!(
            "{label} returned HTTP {status}: {}",
            truncate(&text, 200)
        )));
    }
    Ok(status)
}

fn format_human_message(event: &AlertEvent) -> String {
    let mut msg = format!(
        "webdelta alert: {} ({})\nURL: {}\nPrevious: {}\nCurrent: {}",
        event.watch_name,
        event.watch_id,
        event.url,
        truncate(&event.previous_value, 240),
        truncate(&event.current_value, 240),
    );
    if let Some(delta) = &event.numeric_delta {
        if let Some(pct) = delta.percent_change {
            msg.push_str(&format!("\nDelta: {:+.2} ({:+.2}%)", delta.diff, pct));
        } else {
            msg.push_str(&format!("\nDelta: {:+.2}", delta.diff));
        }
    }
    if !event.matched_rules.is_empty() {
        msg.push_str(&format!("\nRules: {}", event.matched_rules.join(", ")));
    }
    if !event.diff.trim().is_empty() {
        msg.push_str(&format!("\n\n{}", truncate(&event.diff, 1000)));
    }
    msg
}

fn truncate(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let prefix: String = input.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{prefix}...")
}
