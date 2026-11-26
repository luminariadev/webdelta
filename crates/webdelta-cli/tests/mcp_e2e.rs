use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct MockApiServer {
    base_url: String,
    json_body: Arc<Mutex<String>>,
    _rt: tokio::runtime::Runtime,
}

impl MockApiServer {
    fn start(initial_json: &str) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let json_body = Arc::new(Mutex::new(initial_json.to_string()));
        let body_ref = Arc::clone(&json_body);

        let addr = rt.block_on(async move {
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
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            current.len(),
                            current
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                        let _ = socket.shutdown().await;
                    });
                }
            });
            addr
        });

        Self {
            base_url: format!("http://{addr}"),
            json_body,
            _rt: rt,
        }
    }

    fn set_body(&self, next: &str) {
        *self.json_body.lock().unwrap() = next.to_string();
    }
}

fn send_and_recv(
    stdin: &mut impl Write,
    reader: &mut BufReader<std::process::ChildStdout>,
    msg: &Value,
) -> Value {
    let encoded = serde_json::to_string(msg).unwrap();
    writeln!(stdin, "{encoded}").unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    let n = reader.read_line(&mut line).unwrap();
    assert!(n > 0, "unexpected EOF from MCP stdout");
    serde_json::from_str(&line).expect("MCP stdout line must be valid JSON")
}

#[test]
fn test_mcp_stdio_end_to_end() {
    let server = MockApiServer::start(r#"{"item":"SSD","price":199.50}"#);
    let dir = tempdir().unwrap();
    let cfg = dir.path().join("webdelta.toml");
    let db = dir.path().join("state.redb");

    let mut child = Command::new(env!("CARGO_BIN_EXE_webdelta"))
        .arg("--config")
        .arg(&cfg)
        .arg("--db")
        .arg(&db)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let init = send_and_recv(
        &mut stdin,
        &mut reader,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "integration-suite", "version": "0.1" }
            }
        }),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "webdelta");

    // Notification produces no output line.
    writeln!(
        stdin,
        "{}",
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .unwrap()
    )
    .unwrap();
    stdin.flush().unwrap();

    let list_tools = send_and_recv(
        &mut stdin,
        &mut reader,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    );
    let tools = list_tools["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 7);

    let preview_resp = send_and_recv(
        &mut stdin,
        &mut reader,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "preview_extract",
                "arguments": {
                    "url": server.base_url,
                    "extractor_type": "json_path",
                    "path": "$.price"
                }
            }
        }),
    );
    assert_eq!(preview_resp["result"]["isError"], false);
    let preview_payload: Value = serde_json::from_str(
        preview_resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview_payload["extracted_text"], "199.5");
    assert_eq!(preview_payload["numeric_value"], 199.5);

    let add_resp = send_and_recv(
        &mut stdin,
        &mut reader,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "add_watch",
                "arguments": {
                    "id": "ssd-api",
                    "url": server.base_url,
                    "extractor_type": "json_path",
                    "path": "$.price"
                }
            }
        }),
    );
    assert_eq!(add_resp["result"]["isError"], false);

    let list_watches_resp = send_and_recv(
        &mut stdin,
        &mut reader,
        &json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": { "name": "list_watches", "arguments": {} }
        }),
    );
    let list_payload: Value = serde_json::from_str(
        list_watches_resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(list_payload["count"], 1);

    let check1 = send_and_recv(
        &mut stdin,
        &mut reader,
        &json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": { "name": "check_now", "arguments": { "watch_id": "ssd-api" } }
        }),
    );
    let check1_payload: Value =
        serde_json::from_str(check1["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(check1_payload["results"][0]["status"], "initial");

    server.set_body(r#"{"item":"SSD","price":149.50}"#);
    let check2 = send_and_recv(
        &mut stdin,
        &mut reader,
        &json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": "check_now", "arguments": { "watch_id": "ssd-api" } }
        }),
    );
    let check2_payload: Value =
        serde_json::from_str(check2["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(check2_payload["results"][0]["status"], "changed");

    let diff_resp = send_and_recv(
        &mut stdin,
        &mut reader,
        &json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": { "name": "get_diff", "arguments": { "watch_id": "ssd-api" } }
        }),
    );
    let diff_payload: Value =
        serde_json::from_str(diff_resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(diff_payload["has_diff"], true);
    assert_eq!(diff_payload["numeric_delta"]["diff"], -50.0);

    let hist_resp = send_and_recv(
        &mut stdin,
        &mut reader,
        &json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": { "name": "get_history", "arguments": { "watch_id": "ssd-api" } }
        }),
    );
    let hist_payload: Value =
        serde_json::from_str(hist_resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(hist_payload["count"], 2);

    let remove_resp = send_and_recv(
        &mut stdin,
        &mut reader,
        &json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "remove_watch",
                "arguments": { "id": "ssd-api", "purge_history": true }
            }
        }),
    );
    let remove_payload: Value = serde_json::from_str(
        remove_resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(remove_payload["removed"], "ssd-api");
    assert!(remove_payload["purged_snapshots"].as_u64().unwrap() >= 2);

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "MCP server did not exit cleanly on EOF");
}
