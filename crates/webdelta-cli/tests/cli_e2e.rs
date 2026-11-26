use std::process::Command;
use std::sync::{Arc, Mutex};

use assert_cmd::Command as AssertCommand;
use serde_json::Value;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct MockServer {
    base_url: String,
    body: Arc<Mutex<String>>,
    _rt: tokio::runtime::Runtime,
}

impl MockServer {
    fn start(initial_body: &str) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let body = Arc::new(Mutex::new(initial_body.to_string()));
        let body_ref = Arc::clone(&body);

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
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
            body,
            _rt: rt,
        }
    }

    fn set_body(&self, next: &str) {
        *self.body.lock().unwrap() = next.to_string();
    }
}

fn webdelta_cmd(cfg: &std::path::Path, db: &std::path::Path) -> AssertCommand {
    let mut std_cmd = Command::new(env!("CARGO_BIN_EXE_webdelta"));
    std_cmd.arg("--config").arg(cfg).arg("--db").arg(db);
    AssertCommand::from_std(std_cmd)
}

#[test]
fn test_cli_full_lifecycle() {
    let server =
        MockServer::start(r#"<html><body><span class="price">$100.00</span></body></html>"#);
    let dir = tempdir().unwrap();
    let cfg = dir.path().join("webdelta.toml");
    let db = dir.path().join("state.redb");

    webdelta_cmd(&cfg, &db).arg("init").assert().success();

    webdelta_cmd(&cfg, &db)
        .args(["validate", "--json"])
        .assert()
        .success();

    // Remove starter example watch before adding our local mock watch.
    webdelta_cmd(&cfg, &db)
        .args(["remove", "example-site"])
        .assert()
        .success();

    webdelta_cmd(&cfg, &db)
        .args([
            "add",
            "product",
            &server.base_url,
            "--css",
            ".price",
            "--drop-pct",
            "10",
            "--interval",
            "5m",
        ])
        .assert()
        .success();

    let list_out = webdelta_cmd(&cfg, &db)
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(list_out.status.success());
    let list_json: Value = serde_json::from_slice(&list_out.stdout).unwrap();
    assert_eq!(list_json["watches"].as_array().unwrap().len(), 1);
    assert_eq!(list_json["watches"][0]["id"], "product");

    let prev_out = webdelta_cmd(&cfg, &db)
        .args(["preview", &server.base_url, "--css", ".price", "--json"])
        .output()
        .unwrap();
    assert!(prev_out.status.success());
    let prev_json: Value = serde_json::from_slice(&prev_out.stdout).unwrap();
    assert_eq!(prev_json["extracted_text"], "$100.00");
    assert_eq!(prev_json["numeric_value"], 100.0);

    // Initial baseline check -> exit 0 even with --exit-code.
    webdelta_cmd(&cfg, &db)
        .args(["check", "--exit-code"])
        .assert()
        .code(0);

    // Unchanged check -> exit 0.
    webdelta_cmd(&cfg, &db)
        .args(["check", "--exit-code"])
        .assert()
        .code(0);

    // Drop price by 20% -> exit 2 with --exit-code.
    server.set_body(r#"<html><body><span class="price">$80.00</span></body></html>"#);
    webdelta_cmd(&cfg, &db)
        .args(["check", "--exit-code"])
        .assert()
        .code(2);

    let diff_out = webdelta_cmd(&cfg, &db)
        .args(["diff", "product", "--json"])
        .output()
        .unwrap();
    assert!(diff_out.status.success());
    let diff_json: Value = serde_json::from_slice(&diff_out.stdout).unwrap();
    assert_eq!(diff_json["has_diff"], true);
    assert_eq!(diff_json["numeric_delta"]["diff"], -20.0);

    let hist_out = webdelta_cmd(&cfg, &db)
        .args(["history", "product", "--json"])
        .output()
        .unwrap();
    assert!(hist_out.status.success());
    let hist_json: Value = serde_json::from_slice(&hist_out.stdout).unwrap();
    assert_eq!(hist_json["count"], 2);

    let csv_out = webdelta_cmd(&cfg, &db)
        .args(["export", "product", "--format", "csv"])
        .output()
        .unwrap();
    assert!(csv_out.status.success());
    let csv_text = String::from_utf8_lossy(&csv_out.stdout);
    assert!(csv_text.starts_with("checked_at,status_code,"));
    assert!(csv_text.contains("$80.00"));

    let rss_out = webdelta_cmd(&cfg, &db)
        .args(["export", "product", "--format", "rss"])
        .output()
        .unwrap();
    assert!(rss_out.status.success());
    let rss_text = String::from_utf8_lossy(&rss_out.stdout);
    assert!(rss_text.contains("<rss version=\"2.0\">"));

    let cfg_body = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        format!(
            "{cfg_body}\n[notifiers.mock]\ntype = \"webhook\"\nurl = \"{}/webhook\"\n",
            server.base_url
        ),
    )
    .unwrap();

    webdelta_cmd(&cfg, &db)
        .args(["test-notifier", "mock", "--json"])
        .assert()
        .success();

    webdelta_cmd(&cfg, &db)
        .args(["remove", "product", "--purge-history"])
        .assert()
        .success();

    let list_after = webdelta_cmd(&cfg, &db)
        .args(["list", "--json"])
        .output()
        .unwrap();
    let list_after_json: Value = serde_json::from_slice(&list_after.stdout).unwrap();
    assert_eq!(list_after_json["watches"].as_array().unwrap().len(), 0);
}
