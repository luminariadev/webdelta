use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, IF_MODIFIED_SINCE, IF_NONE_MATCH, USER_AGENT,
};
use reqwest::{Client, Method};

use crate::config::HttpMethod;
use crate::error::{Error, Result};

/// Parameters describing an HTTP fetch operation.
#[derive(Debug, Clone)]
pub struct FetchRequest<'a> {
    pub url: &'a str,
    pub method: HttpMethod,
    pub headers: &'a BTreeMap<String, String>,
    pub user_agent: &'a str,
    pub timeout: Duration,
    pub retries: usize,
    pub etag: Option<&'a str>,
    pub last_modified: Option<&'a str>,
}

/// Result returned by [`HttpFetcher::fetch`].
#[derive(Debug, Clone)]
pub enum FetchOutput {
    NotModified {
        elapsed_ms: u64,
    },
    Response {
        status: u16,
        headers: BTreeMap<String, String>,
        body: String,
        etag: Option<String>,
        last_modified: Option<String>,
        elapsed_ms: u64,
    },
}

/// Reusable async HTTP client with conditional request and retry support.
#[derive(Debug, Clone)]
pub struct HttpFetcher {
    client: Client,
}

impl HttpFetcher {
    /// Creates a new fetcher backed by a pooled HTTP client.
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .build()
            .map_err(|err| Error::Http(format!("failed to build HTTP client: {err}")))?;
        Ok(Self { client })
    }

    /// Executes an HTTP request with retries on transient errors, 429, and 5xx responses.
    pub async fn fetch(&self, req: FetchRequest<'_>) -> Result<FetchOutput> {
        let method = match req.method {
            HttpMethod::Get => Method::GET,
            HttpMethod::Head => Method::HEAD,
            HttpMethod::Post => Method::POST,
        };

        let mut header_map = HeaderMap::new();
        let ua_value = HeaderValue::from_str(req.user_agent).map_err(|err| {
            Error::Validation(format!("invalid user_agent '{}': {err}", req.user_agent))
        })?;
        header_map.insert(USER_AGENT, ua_value);

        for (k, v) in req.headers {
            let name = HeaderName::from_bytes(k.as_bytes())
                .map_err(|err| Error::Validation(format!("invalid header name '{k}': {err}")))?;
            let value = HeaderValue::from_str(v).map_err(|err| {
                Error::Validation(format!("invalid header value for '{k}': {err}"))
            })?;
            header_map.insert(name, value);
        }

        if matches!(req.method, HttpMethod::Get | HttpMethod::Head) {
            if let Some(etag) = req.etag {
                if let Ok(val) = HeaderValue::from_str(etag) {
                    header_map.entry(IF_NONE_MATCH).or_insert(val);
                }
            }
            if let Some(lm) = req.last_modified {
                if let Ok(val) = HeaderValue::from_str(lm) {
                    header_map.entry(IF_MODIFIED_SINCE).or_insert(val);
                }
            }
        }

        let max_attempts = req.retries.saturating_add(1);
        let started = Instant::now();

        for attempt in 0..max_attempts {
            let response_res = self
                .client
                .request(method.clone(), req.url)
                .headers(header_map.clone())
                .timeout(req.timeout)
                .send()
                .await;

            match response_res {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if status == 304 {
                        return Ok(FetchOutput::NotModified {
                            elapsed_ms: started.elapsed().as_millis() as u64,
                        });
                    }

                    let is_retryable = status == 429 || (500..=599).contains(&status);
                    if is_retryable && attempt + 1 < max_attempts {
                        tokio::time::sleep(backoff_delay(attempt)).await;
                        continue;
                    }

                    let mut headers = BTreeMap::new();
                    for (name, value) in resp.headers() {
                        if let Ok(val_str) = value.to_str() {
                            headers.insert(name.as_str().to_ascii_lowercase(), val_str.to_string());
                        }
                    }

                    let etag = headers.get("etag").cloned();
                    let last_modified = headers.get("last-modified").cloned();
                    let body = resp.text().await.map_err(|err| {
                        Error::Http(format!(
                            "failed reading response body from '{}': {err}",
                            req.url
                        ))
                    })?;

                    return Ok(FetchOutput::Response {
                        status,
                        headers,
                        body,
                        etag,
                        last_modified,
                        elapsed_ms: started.elapsed().as_millis() as u64,
                    });
                }
                Err(err) => {
                    if attempt + 1 < max_attempts {
                        tokio::time::sleep(backoff_delay(attempt)).await;
                        continue;
                    }
                    return Err(Error::Http(format!(
                        "request to '{}' failed after {} attempt(s): {err}",
                        req.url, max_attempts
                    )));
                }
            }
        }

        Err(Error::Http(format!(
            "request to '{}' exhausted retries",
            req.url
        )))
    }
}

fn backoff_delay(attempt: usize) -> Duration {
    let shift = (attempt as u32).min(5);
    let ms = 200u64.saturating_mul(1u64 << shift).min(5_000);
    Duration::from_millis(ms)
}
