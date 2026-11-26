use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use webdelta_core::{
    CheckStatus, ConditionConfig, Config, DefaultsConfig, ExtractorConfig, HttpMethod,
    MonitorEngine, NormalizeConfig, NotifierConfig, StateStore, WatchConfig,
};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Default)]
struct MockState {
    html_body: String,
    etag: String,
    fail_first_n: usize,
    requests_seen: usize,
    webhooks: Vec<(BTreeMap<String, String>, Vec<u8>)>,
    telegram_calls: Vec<Vec<u8>>,
}

async fn start_mock_server(state: Arc<Mutex<MockState>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let state_ref = Arc::clone(&state);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let mut header_end = None;
                let mut content_length = 0usize;

                loop {
                    let n = match socket.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    if header_end.is_none() {
                        if let Some(pos) = find_double_crlf(&buf) {
                            header_end = Some(pos);
                            let head_str = String::from_utf8_lossy(&buf[..pos]);
                            for line in head_str.lines() {
                                if let Some((k, v)) = line.split_once(':') {
                                    if k.eq_ignore_ascii_case("content-length") {
                                        content_length = v.trim().parse().unwrap_or(0);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(end) = header_end {
                        if buf.len() >= end + 4 + content_length {
                            break;
                        }
                    }
                }

                let end = header_end.unwrap();
                let head = String::from_utf8_lossy(&buf[..end]).to_string();
                let body = buf[end + 4..end + 4 + content_length].to_vec();

                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or("");
                let mut parts = request_line.split_whitespace();
                let _method = parts.next().unwrap_or("GET");
                let path = parts.next().unwrap_or("/");

                let mut headers = BTreeMap::new();
                for line in lines {
                    if let Some((k, v)) = line.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }

                let response_bytes = if path == "/webhook" {
                    let mut guard = state_ref.lock().unwrap();
                    guard.webhooks.push((headers, body));
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec()
                } else if path.starts_with("/bot") && path.ends_with("/sendMessage") {
                    let mut guard = state_ref.lock().unwrap();
                    guard.telegram_calls.push(body);
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}".to_vec()
                } else {
                    let mut guard = state_ref.lock().unwrap();
                    guard.requests_seen += 1;
                    if guard.fail_first_n > 0 {
                        guard.fail_first_n -= 1;
                        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\nConnection: close\r\n\r\nerror".to_vec()
                    } else if headers
                        .get("if-none-match")
                        .is_some_and(|v| v == &guard.etag)
                    {
                        format!(
                            "HTTP/1.1 304 Not Modified\r\nETag: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            guard.etag
                        )
                        .into_bytes()
                    } else {
                        let resp_body = guard.html_body.clone();
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nETag: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            guard.etag,
                            resp_body.len(),
                            resp_body
                        )
                        .into_bytes()
                    }
                };

                let _ = socket.write_all(&response_bytes).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    format!("http://{addr}")
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

#[tokio::test]
async fn test_end_to_end_check_etag_diff_drop_pct_and_hmac_webhook() {
    let state = Arc::new(Mutex::new(MockState {
        html_body: r#"<html><body><div class="price">$200.00</div><span class="ts">12:00:00</span></body></html>"#.to_string(),
        etag: "\"v1\"".to_string(),
        ..Default::default()
    }));
    let base_url = start_mock_server(Arc::clone(&state)).await;

    let dir = tempdir().unwrap();
    let store = StateStore::open(&dir.path().join("state.redb")).unwrap();
    let engine = MonitorEngine::new(store).unwrap();

    let mut notifiers = BTreeMap::new();
    notifiers.insert(
        "n8n".to_string(),
        NotifierConfig::Webhook {
            url: format!("{base_url}/webhook"),
            headers: BTreeMap::new(),
            secret: Some("super-secret-key".to_string()),
        },
    );

    let watch = WatchConfig {
        id: "gpu-price".to_string(),
        name: Some("GPU Price".to_string()),
        url: format!("{base_url}/product"),
        method: HttpMethod::Get,
        headers: BTreeMap::new(),
        interval: Some("5m".to_string()),
        timeout_secs: Some(5),
        retries: Some(1),
        extractor: ExtractorConfig::Css {
            selector: ".price, .ts".to_string(),
            attribute: None,
            all: true,
        },
        ignore_regex: vec![r"\d{2}:\d{2}:\d{2}".to_string()],
        normalize: NormalizeConfig::default(),
        conditions: vec![ConditionConfig::DropPctAtLeast { percent: 20.0 }],
        notifiers: vec!["n8n".to_string()],
        enabled: true,
    };

    let config = Config {
        defaults: DefaultsConfig::default(),
        notifiers,
        watches: vec![watch.clone()],
    };

    let out1 = engine.check_watch(&watch, &config, true).await;
    assert_eq!(out1.status, CheckStatus::Initial);
    assert_eq!(out1.current_value.as_deref(), Some("$200.00"));
    assert_eq!(out1.numeric_value, Some(200.0));

    let out2 = engine.check_watch(&watch, &config, true).await;
    assert_eq!(out2.status, CheckStatus::NotModified);
    assert_eq!(out2.status_code, Some(304));

    {
        let mut g = state.lock().unwrap();
        g.etag = "\"v2\"".to_string();
        g.html_body = r#"<html><body><div class="price">$200.00</div><span class="ts">12:05:00</span></body></html>"#.to_string();
    }
    let out3 = engine.check_watch(&watch, &config, true).await;
    assert_eq!(out3.status, CheckStatus::Unchanged);

    {
        let mut g = state.lock().unwrap();
        g.etag = "\"v3\"".to_string();
        g.html_body = r#"<html><body><div class="price">$150.00</div><span class="ts">12:10:00</span></body></html>"#.to_string();
    }
    let out4 = engine.check_watch(&watch, &config, true).await;
    assert_eq!(out4.status, CheckStatus::Changed);
    assert_eq!(out4.numeric_value, Some(150.0));
    assert_eq!(out4.numeric_delta.as_ref().unwrap().diff, -50.0);
    assert_eq!(
        out4.numeric_delta.as_ref().unwrap().percent_change,
        Some(-25.0)
    );
    assert_eq!(out4.notifications.len(), 1);
    assert!(out4.notifications[0].ok);

    let webhooks = state.lock().unwrap().webhooks.clone();
    assert_eq!(webhooks.len(), 1);
    let (hdrs, body) = &webhooks[0];
    let sig_header = hdrs
        .get("x-webdelta-signature")
        .expect("missing HMAC header");

    let mut mac = HmacSha256::new_from_slice(b"super-secret-key").unwrap();
    mac.update(body);
    let expected_sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    assert_eq!(sig_header, &expected_sig);

    let payload: serde_json::Value = serde_json::from_slice(body).unwrap();
    assert_eq!(payload["watch_id"], "gpu-price");
    assert_eq!(payload["previous_value"], "$200.00");
    assert_eq!(payload["current_value"], "$150.00");
}

#[tokio::test]
async fn test_telegram_notifier_and_rule_not_met_suppression() {
    let state = Arc::new(Mutex::new(MockState {
        html_body: r#"<p>Out of Stock</p>"#.to_string(),
        etag: "\"t1\"".to_string(),
        ..Default::default()
    }));
    let base_url = start_mock_server(Arc::clone(&state)).await;

    let dir = tempdir().unwrap();
    let store = StateStore::open(&dir.path().join("state.redb")).unwrap();
    let engine = MonitorEngine::new(store).unwrap();

    let mut notifiers = BTreeMap::new();
    notifiers.insert(
        "tg".to_string(),
        NotifierConfig::Telegram {
            bot_token: "123:ABC".to_string(),
            chat_id: "99999".to_string(),
            api_base_url: Some(base_url.clone()),
        },
    );

    let watch = WatchConfig {
        id: "stock".to_string(),
        name: Some("Stock Tracker".to_string()),
        url: format!("{base_url}/stock"),
        method: HttpMethod::Get,
        headers: BTreeMap::new(),
        interval: None,
        timeout_secs: Some(5),
        retries: Some(0),
        extractor: ExtractorConfig::Css {
            selector: "p".to_string(),
            attribute: None,
            all: false,
        },
        ignore_regex: vec![],
        normalize: NormalizeConfig::default(),
        conditions: vec![ConditionConfig::Contains {
            value: "In Stock".to_string(),
            case_sensitive: false,
        }],
        notifiers: vec!["tg".to_string()],
        enabled: true,
    };

    let config = Config {
        defaults: DefaultsConfig::default(),
        notifiers,
        watches: vec![watch.clone()],
    };

    let r1 = engine.check_watch(&watch, &config, true).await;
    assert_eq!(r1.status, CheckStatus::Initial);

    {
        let mut g = state.lock().unwrap();
        g.etag = "\"t2\"".to_string();
        g.html_body = r#"<p>Coming Soon</p>"#.to_string();
    }
    let r2 = engine.check_watch(&watch, &config, true).await;
    assert_eq!(r2.status, CheckStatus::RuleNotMet);
    assert!(state.lock().unwrap().telegram_calls.is_empty());

    {
        let mut g = state.lock().unwrap();
        g.etag = "\"t3\"".to_string();
        g.html_body = r#"<p>In Stock Now</p>"#.to_string();
    }
    let r3 = engine.check_watch(&watch, &config, true).await;
    assert_eq!(r3.status, CheckStatus::Changed);
    assert_eq!(r3.notifications.len(), 1);
    assert!(r3.notifications[0].ok);

    let calls = state.lock().unwrap().telegram_calls.clone();
    assert_eq!(calls.len(), 1);
    let tg_json: serde_json::Value = serde_json::from_slice(&calls[0]).unwrap();
    assert_eq!(tg_json["chat_id"], "99999");
    assert!(tg_json["text"].as_str().unwrap().contains("In Stock Now"));
}

#[tokio::test]
async fn test_retry_recovery_after_transient_500() {
    let state = Arc::new(Mutex::new(MockState {
        html_body: r#"<div>Healthy</div>"#.to_string(),
        etag: "\"r1\"".to_string(),
        fail_first_n: 2,
        ..Default::default()
    }));
    let base_url = start_mock_server(Arc::clone(&state)).await;

    let dir = tempdir().unwrap();
    let store = StateStore::open(&dir.path().join("state.redb")).unwrap();
    let engine = MonitorEngine::new(store).unwrap();

    let watch = WatchConfig {
        id: "retry-test".to_string(),
        name: None,
        url: format!("{base_url}/health"),
        method: HttpMethod::Get,
        headers: BTreeMap::new(),
        interval: None,
        timeout_secs: Some(5),
        retries: Some(2),
        extractor: ExtractorConfig::Css {
            selector: "div".to_string(),
            attribute: None,
            all: false,
        },
        ignore_regex: vec![],
        normalize: NormalizeConfig::default(),
        conditions: vec![],
        notifiers: vec![],
        enabled: true,
    };

    let config = Config {
        defaults: DefaultsConfig::default(),
        notifiers: BTreeMap::new(),
        watches: vec![watch.clone()],
    };

    let res = engine.check_watch(&watch, &config, false).await;
    assert_eq!(res.status, CheckStatus::Initial);
    assert_eq!(res.status_code, Some(200));
    assert_eq!(res.current_value.as_deref(), Some("Healthy"));
    assert_eq!(state.lock().unwrap().requests_seen, 3);
}
