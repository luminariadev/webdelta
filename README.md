# webdelta

A single-binary website and API change monitor written in Rust, with noise filtering, numeric price rules, webhook and chat notifications, and a built-in Model Context Protocol (MCP) server.

## Why webdelta

Most change trackers either require Python plus a heavy headless browser stack on every check, or fire noisy alerts whenever an embedded timestamp or counter shifts. `webdelta` focuses on fast HTTP checks, clean extraction, and actionable transitions:

- **Single native binary**: ships with an embedded ACID `redb` database and TOML configuration, requiring no external database, Redis, or runtime.
- **Targeted extraction**: extract readable text, CSS selectors (or element attributes), JSONPath fields, regular expressions, or HTTP status codes plus headers.
- **Noise filtering**: strip dynamic timestamps, session tokens, or rotating banners with `ignore_regex` and line-level whitespace normalization before diffing.
- **Numeric and price conditions**: parses currency formats (`$1,299.99`, `Rp 1.500.000`, `EUR 49,90`, `-12.5`, `99.95%`) to trigger alerts on percentage drops, rises, or numeric thresholds.
- **Bandwidth-friendly HTTP**: sends `If-None-Match` (`ETag`) and `If-Modified-Since` headers automatically and retries transient 429/5xx errors with exponential backoff.
- **Automation-ready**: supports `--json` output, script-friendly `--exit-code` (`0` unchanged, `2` changed, `1` error), signed HMAC-SHA256 webhooks for n8n, Telegram, Discord, Slack, ntfy, and a built-in `stdio` MCP server.

## Installation

### Build from Source

Requires Rust 1.82 or newer:

```bash
git clone https://github.com/luminariadev/webdelta.git
cd webdelta
cargo install --path crates/webdelta-cli
```

Prebuilt release archives for Linux, Windows, and macOS are also published with SHA-256 checksums on the GitHub Releases page whenever a version tag is pushed.

## Quickstart

Create a starter `webdelta.toml` and verify it:

```bash
webdelta init
webdelta validate
```

Preview a selector before saving a watch:

```bash
webdelta preview https://example.com --css h1
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
notifiers = ["n8n"]

[watches.extractor]
type = "json_path"
path = "$.version"

[[watches.conditions]]
type = "changed"
```

See [docs/configuration.md](docs/configuration.md) for every extractor, rule type, and notifier option, plus runnable samples in [examples/price-tracker.toml](examples/price-tracker.toml) and [examples/api-drift.toml](examples/api-drift.toml).

## CLI Reference

Global options available on every command: `--config <PATH>` (default `webdelta.toml`), `--db <PATH>` (default `.webdelta/state.redb`), `--json`, and `--verbose`.

| Command | Purpose |
| :--- | :--- |
| `webdelta init [--force]` | Write a starter `webdelta.toml` configuration file. |
| `webdelta validate` | Validate TOML syntax, URLs, selectors, regexes, and notifier links. |
| `webdelta add <id> <url> [flags]` | Add a watch with `--css`, `--jsonpath`, `--regex`, or `--status` and optional rules. |
| `webdelta list` | List configured watches and their latest stored values. |
| `webdelta remove <id> [--purge-history]` | Remove a watch and optionally delete its stored history. |
| `webdelta preview <url> [flags]` | Fetch a URL once and print extracted text and numeric parse without saving state. |
| `webdelta check [ids...] [--exit-code]` | Run checks once; with `--exit-code`, returns `2` when triggered changes occur. |
| `webdelta diff <id> [--context 3]` | Print unified diff and numeric delta between the two latest snapshots. |
| `webdelta history <id> [--limit 20]` | Show recorded snapshots for a watch, newest first. |
| `webdelta watch [--concurrency 8]` | Run the interval scheduler until interrupted. |
| `webdelta mcp` | Start the Model Context Protocol `stdio` server. |

## Webhooks and n8n

Webhook notifiers send a structured JSON event containing `watch_id`, `previous_value`, `current_value`, `numeric_delta`, `diff`, and `matched_rules`, optionally signed with `X-WebDelta-Signature: sha256=<hex>`.

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

Exposed tools: `list_watches`, `add_watch`, `remove_watch`, `check_now`, `get_diff`, `get_history`, and `preview_extract`. See [docs/mcp.md](docs/mcp.md) for parameter schemas.

## Development and Testing

All unit, integration, CLI, and MCP tests run completely offline using ephemeral local mock HTTP servers:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Roadmap

- **v0.1.0**: Pure Rust HTTP engine, CSS/JSONPath/Regex/Text/Status extractors, numeric parser, `redb` history, notifiers, CLI, scheduler, and MCP server.
- **v0.2.0**: Optional CDP headless browser fallback (`auto` mode) for JavaScript-rendered pages, plus reproducible memory and throughput benchmarks.
- **v0.3.0**: Embedded local web dashboard (`webdelta serve`) with side-by-side visual diffs and CSV/JSON/RSS history export.

## License

MIT. See [LICENSE](LICENSE).
