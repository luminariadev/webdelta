# webdelta

[![CI](https://github.com/luminariadev/webdelta/actions/workflows/ci.yml/badge.svg)](https://github.com/luminariadev/webdelta/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/luminariadev/webdelta?display_name=tag)](https://github.com/luminariadev/webdelta/releases)
[![License](https://img.shields.io/github/license/luminariadev/webdelta)](LICENSE)
[![Rust 1.82+](https://img.shields.io/badge/rust-1.82%2B-orange)](https://www.rust-lang.org)
[![MCP stdio + HTTP](https://img.shields.io/badge/MCP-stdio%20%2B%20HTTP-blue)](docs/mcp.md)

A single-binary website and API change monitor written in Rust, with hybrid HTTP and headless CDP rendering, an embedded web dashboard, noise filtering, numeric price rules, webhook and chat notifications, and a built-in Model Context Protocol (MCP) server.

## Why webdelta

Most change trackers either launch a heavy browser stack for every URL check or fire false alerts whenever an embedded timestamp or counter changes. `webdelta` keeps routine polling in fast native HTTP and only invokes a headless Chromium browser when a page requires JavaScript rendering:

- **Single native binary**: ships with an embedded ACID `redb` database, TOML configuration, HTTP engine, CDP client, web dashboard, and MCP server, with zero external runtime or database dependencies.
- **Hybrid `http`, `browser`, and `auto` engines**: polls static HTML and JSON APIs over native HTTP with `If-None-Match` (`ETag`) and `If-Modified-Since`, and switches to local Chrome/Edge/Chromium or a remote `--cdp-url` when `auto` detects a client-rendered SPA shell (with optional `--click-selector` and `--scroll-to-bottom`).
- **Embedded web dashboard and exports**: `webdelta serve` exposes a local UI with diff highlighting, numeric SVG sparklines, manual checks, pause/resume toggles, and JSON/CSV/RSS exports.
- **Targeted extraction**: extract readable text, CSS selectors (or element attributes), JSONPath fields, regular expressions, or HTTP status codes plus headers.
- **Noise filtering**: strip dynamic timestamps, session tokens, or rotating banners with `ignore_regex` and line-preserving whitespace normalization before diffing.
- **Numeric and price conditions**: parses currency formats (`$1,299.99`, `Rp 1.500.000`, `EUR 49,90`, `-12.5`, `99.95%`) while ignoring semver tags, IP addresses, and dates, triggering alerts on percentage drops, rises, or thresholds.
- **Automation-ready**: supports `--json` output, script-friendly `--exit-code` (`0` unchanged, `2` changed, `1` error), signed HMAC-SHA256 webhooks for n8n, Telegram, Discord, Slack, ntfy, and a built-in `stdio` + Streamable HTTP MCP server.

## Preview

Embedded web dashboard (`webdelta serve`) tracking HTTP, hybrid `auto`, and paused watches:

![webdelta local web dashboard showing monitored watches](docs/assets/dashboard-overview.png)

| History & Numeric Sparkline | Unified Diff & Delta |
| :---: | :---: |
| ![History modal with SVG sparkline and JSON/CSV/RSS export controls](docs/assets/dashboard-history.png) | ![Unified diff modal showing numeric delta and highlighted line changes](docs/assets/dashboard-diff.png) |

Real CLI diff output after consecutive price checks:

```text
$ webdelta diff workstation-gpu
Watch: workstation-gpu
Previous (2026-11-26 16:50:30 UTC): $1,149.00
Current  (2026-11-26 16:50:32 UTC): $1,049.00
Numeric delta: -100.00 (-8.70%)
Summary: +1 / -1 lines
--- previous
+++ current
@@ -1 +1 @@
-$1,149.00
\ No newline at end of file
+$1,049.00
\ No newline at end of file
```

## Installation

### Prebuilt Binaries

Download an archive for Linux (`x86_64`), Windows (`x86_64`), or macOS (`x86_64` and Apple Silicon) together with `SHA256SUMS.txt` from [GitHub Releases](https://github.com/luminariadev/webdelta/releases).

### Scoop (Windows)

```powershell
scoop install https://raw.githubusercontent.com/luminariadev/webdelta/main/packaging/scoop/webdelta.json
```

### Homebrew (macOS / Linux)

```bash
brew install --formula https://raw.githubusercontent.com/luminariadev/webdelta/main/packaging/homebrew/webdelta.rb
```

### Docker / GHCR

Pull the published public container image or run with Docker Compose:

```bash
docker pull ghcr.io/luminariadev/webdelta:latest
```

Or run the dashboard, background polling scheduler, and HTTP `/mcp` endpoint via Compose:

```bash
mkdir -p data
cp examples/price-tracker.toml data/webdelta.toml
docker compose up -d --build
```

### Build from Source

Requires Rust 1.82 or newer:

```bash
git clone https://github.com/luminariadev/webdelta.git
cd webdelta
cargo install --path crates/webdelta-cli
```

See [docs/distribution.md](docs/distribution.md) for checksum verification, package manifests, and [docs/docker.md](docs/docker.md) for container configuration.

## Quickstart

Create a starter `webdelta.toml` and verify it:

```bash
webdelta init
webdelta validate
```

Preview a selector before saving a watch (use `--engine auto` or `--engine browser` for JavaScript-rendered pages):

```bash
webdelta preview https://www.rust-lang.org --css title
webdelta preview https://spa.example.com --engine auto --wait-selector ".price" --css ".price"
```

Add a watch with a numeric drop rule and run checks:

```bash
webdelta add laptop-price https://shop.example.com/item --css ".price" --drop-pct 10 --interval 15m
webdelta check
webdelta diff laptop-price
webdelta history laptop-price
```

Run the continuous polling scheduler in the foreground (stops cleanly on `Ctrl+C`):

```bash
webdelta watch
```

## Example Configuration

```toml
[defaults]
interval = "15m"
timeout_secs = 15
retries = 2
max_history = 100
concurrency = 8

[notifiers.n8n]
type = "webhook"
url = "https://n8n.example.com/webhook/webdelta-alerts"
secret = "${WEBDELTA_WEBHOOK_SECRET}"

[[watches]]
id = "api-version"
name = "Catalog API Version"
url = "https://api.example.com/v1/meta"
interval = "5m"
engine = "http"
notifiers = ["n8n"]

[watches.extractor]
type = "json_path"
path = "$.version"

[[watches.conditions]]
type = "changed"
```

See [docs/configuration.md](docs/configuration.md) for every extractor, browser option, rule type, and notifier, plus runnable samples in [examples/price-tracker.toml](examples/price-tracker.toml), [examples/api-drift.toml](examples/api-drift.toml), and [examples/spa-browser.toml](examples/spa-browser.toml).

## CLI Reference

Global options available on every command: `--config <PATH>` (default `webdelta.toml`), `--db <PATH>` (default `.webdelta/state.redb`), `--json`, and `--verbose`.

| Command | Purpose |
| :--- | :--- |
| `webdelta init [--force]` | Write a starter `webdelta.toml` configuration file. |
| `webdelta validate` | Validate TOML syntax, URLs, selectors, regexes, browser options, and notifier links. |
| `webdelta add <id> <url> [flags]` | Add a watch with `--css`, `--jsonpath`, `--regex`, `--status`, `--engine`, and alert rules. |
| `webdelta list` | List configured watches and their latest stored values. |
| `webdelta remove <id> [--purge-history]` | Remove a watch and optionally delete its stored history. |
| `webdelta preview <url> [flags]` | Fetch or render a URL once and print extracted text and numeric parse without saving state. |
| `webdelta check [ids...] [--exit-code]` | Run checks once; with `--exit-code`, returns `2` when triggered changes occur. |
| `webdelta diff <id> [--context 3]` | Print unified diff and numeric delta between the two latest snapshots. |
| `webdelta history <id> [--limit 20]` | Show recorded snapshots for a watch, newest first. |
| `webdelta export <id> [--format json\|csv\|rss]` | Export stored history as JSON, CSV, or RSS 2.0 XML. |
| `webdelta test-notifier <name>` | Send a synthetic test alert to verify a configured notifier. |
| `webdelta watch [--concurrency 8]` | Run the interval scheduler until interrupted. |
| `webdelta serve [--bind 127.0.0.1:8787]` | Launch the local web dashboard, REST API, and HTTP `/mcp` endpoint (add `--background-watch` to poll on schedule). |
| `webdelta mcp [--http <bind>]` | Start the Model Context Protocol server (`stdio` by default, or Streamable HTTP with `--http`). |

## Benchmarks

Measured on `webdelta` v0.2.0 (`6.64 MB`, Windows 11, Intel Core i5-11400H, local HTTP/1.1 mock server, `--concurrency 32`):

| Workload | Wall Time | Throughput | Peak RAM |
| :--- | :--- | :--- | :--- |
| Cold CLI startup (`--version`) | 25.2 ms | n/a | 2.56 MB |
| Validate 100 watches (`validate`) | 51.8 ms | n/a | 2.55 MB |
| 100 HTTP + CSS checks + `redb` writes | 303.9 ms | 329.0 checks/s | 13.95 MB |
| 100 conditional `304 Not Modified` checks | 203.0 ms | 492.5 checks/s | 13.79 MB |
| Single JS/SPA render via CDP (`--engine auto`) | 840.5 ms | n/a | ephemeral Chrome |

See [docs/benchmarks.md](docs/benchmarks.md) for the exact test setup and additional metrics.

## Web Dashboard

Launch the embedded local web dashboard (and optional background scheduler) from the same binary:

```bash
webdelta serve --bind 127.0.0.1:8787 --background-watch
```

Pass `--token <secret>` (or `WEBDELTA_TOKEN`) when exposing the server beyond `127.0.0.1`. The dashboard provides live watch filtering, one-click checks, pause/resume controls, syntax-colored unified diffs, numeric SVG sparklines, and CSV/JSON/RSS downloads.

## Webhooks and n8n

Webhook notifiers send a structured JSON event containing `watch_id`, `previous_value`, `current_value`, `numeric_delta`, `diff`, and `matched_rules`, optionally signed with `X-WebDelta-Signature: sha256=<hex>`. Verify a notifier before running production checks:

```bash
webdelta test-notifier n8n
```

See [docs/webhooks-n8n.md](docs/webhooks-n8n.md) and the importable workflow at [examples/n8n-workflow.json](examples/n8n-workflow.json).

## MCP Server

Start the embedded `stdio` MCP server for any MCP-compatible client:

```json
{
  "mcpServers": {
    "webdelta": {
      "command": "webdelta",
      "args": ["mcp", "--config", "webdelta.toml", "--db", ".webdelta/state.redb"]
    }
  }
}
```

Or expose Streamable HTTP (`POST /mcp`) for remote workflows:

```bash
webdelta mcp --http 127.0.0.1:8788 --token "${WEBDELTA_MCP_TOKEN}"
```

Exposed tools (10): `list_watches`, `add_watch`, `update_watch`, `remove_watch`, `check_now`, `get_diff`, `get_history`, `preview_extract`, `test_notifier`, and `export_history`, plus read-only MCP resources under `webdelta://`. See [docs/mcp.md](docs/mcp.md) for schemas and URIs.

## Development and Testing

All unit, integration, CLI, and MCP tests run offline using ephemeral local mock HTTP servers:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Roadmap

- **v0.1.0**: Pure Rust HTTP engine, CSS/JSONPath/Regex/Text/Status extractors, numeric parser, `redb` history, notifiers, CLI, scheduler, and MCP server.
- **v0.2.0**: Hybrid `http`/`browser`/`auto` engine with pure-Rust CDP headless browser rendering, SPA detection, stricter numeric parsing, and reproducible benchmarks.
- **v0.3.0**: Embedded web dashboard (`webdelta serve`), JSON/CSV/RSS history export, MCP `update_watch`/`test_notifier`/`export_history` + Resources + Streamable HTTP, CDP click/scroll actions, and Docker packaging.
- **v0.4.0**: Crates.io publication, Homebrew/Scoop/Winget packages, and optional CDP browser session pooling for high-frequency SPA monitoring.

## License

MIT. See [LICENSE](LICENSE).
