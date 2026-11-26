use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct MockTargetServer {
    base_url: String,
    body: Arc<Mutex<String>>,
}

impl MockTargetServer {
    async fn start(initial_body: &str) -> Self {
        let body = Arc::new(Mutex::new(initial_body.to_string()));
        let body_ref = Arc::clone(&body);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let body_inner = Arc::clone(&body_ref);
                tokio::spawn(async move {
                    let mut tmp = [0u8; 2048];
                    let _ = socket.read(&mut tmp).await;
                    let current = body_inner.lock().unwrap().clone();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        current.len(),
                        current
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            body,
        }
    }

    fn set_body(&self, next: &str) {
        *self.body.lock().unwrap() = next.to_string();
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_local_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn test_serve_dashboard_api_export_and_streamable_http_mcp() {
    let target =
        MockTargetServer::start(r#"<html><body><span class="price">$120.00</span></body></html>"#)
            .await;
    let dir = tempdir().unwrap();
    let cfg = dir.path().join("webdelta.toml");
    let db = dir.path().join("state.redb");

    std::fs::write(
        &cfg,
        format!(
            r#"[defaults]
interval = "15m"
timeout_secs = 10
retries = 0

[[watches]]
id = "demo"
name = "Demo Item"
url = "{}"

[watches.extractor]
type = "css"
selector = ".price"
"#,
            target.base_url
        ),
    )
    .unwrap();

    let port = free_local_port();
    let bind = format!("127.0.0.1:{port}");
    let base = format!("http://{bind}");
    let token = "test-secret";

    let child = Command::new(env!("CARGO_BIN_EXE_webdelta"))
        .arg("--config")
        .arg(&cfg)
        .arg("--db")
        .arg(&db)
        .args(["serve", "--bind", &bind, "--token", token])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _guard = ChildGuard(child);

    let client = reqwest::Client::new();
    let mut ready = false;
    for _ in 0..40 {
        if let Ok(resp) = client.get(format!("{base}/health")).send().await {
            if resp.status().is_success() {
                ready = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ready, "webdelta serve did not become healthy in time");

    let dash_resp = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(dash_resp.status().as_u16(), 200);
    let html = dash_resp.text().await.unwrap();
    assert!(html.contains("webdelta Dashboard"));

    let unauth_api = client
        .get(format!("{base}/api/watches"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth_api.status().as_u16(), 401);

    let unauth_mcp = client
        .post(format!("{base}/mcp"))
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(unauth_mcp.status().as_u16(), 401);

    let auth_header = format!("Bearer {token}");
    let watches_resp = client
        .get(format!("{base}/api/watches"))
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap();
    assert_eq!(watches_resp.status().as_u16(), 200);
    let watches_json: Value = watches_resp.json().await.unwrap();
    assert_eq!(watches_json["watches"].as_array().unwrap().len(), 1);

    let c1: Value = client
        .post(format!("{base}/api/watches/demo/check"))
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(c1["status"], "initial");

    target.set_body(r#"<html><body><span class="price">$90.00</span></body></html>"#);
    let c2: Value = client
        .post(format!("{base}/api/watches/demo/check"))
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(c2["status"], "changed");

    let diff_json: Value = client
        .get(format!("{base}/api/watches/demo/diff"))
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(diff_json["has_diff"], true);
    assert_eq!(diff_json["numeric_delta"]["diff"], -30.0);

    let csv_text = client
        .get(format!("{base}/api/watches/demo/export?format=csv"))
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(csv_text.contains("$90.00"));

    let rss_text = client
        .get(format!("{base}/api/watches/demo/export?format=rss"))
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(rss_text.contains("<rss version=\"2.0\">"));

    let toggle_json: Value = client
        .post(format!("{base}/api/watches/demo/toggle"))
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(toggle_json["enabled"], false);

    // Verify Streamable HTTP MCP over the same server & shared StateStore.
    let mcp_init: Value = client
        .post(format!("{base}/mcp"))
        .header("Authorization", &auth_header)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-03-26" }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(mcp_init["result"]["serverInfo"]["name"], "webdelta");

    let mcp_tools: Value = client
        .post(format!("{base}/mcp"))
        .header("Authorization", &auth_header)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(mcp_tools["result"]["tools"].as_array().unwrap().len(), 10);

    let mcp_list: Value = client
        .post(format!("{base}/mcp"))
        .header("Authorization", &auth_header)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "list_watches", "arguments": { "include_disabled": true } }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let list_payload: Value =
        serde_json::from_str(mcp_list["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(list_payload["count"], 1);
    assert_eq!(list_payload["watches"][0]["enabled"], false);
}
