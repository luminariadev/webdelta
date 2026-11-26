# Changelog

All notable changes to `webdelta` are documented in this file.

The format follows Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.3.0] - 2026-11-26

### Added

- Embedded local web dashboard and REST API (`webdelta serve`) with live search filtering, manual checks, pause/resume watch toggles, colored unified diffs, inline SVG numeric sparklines, optional Bearer token auth (`--token` / `WEBDELTA_TOKEN`), and optional `--background-watch` scheduler.
- History export across `json`, `csv` (RFC 4180), and `rss` (RSS 2.0 XML) via `webdelta export`, HTTP `/api/watches/{id}/export`, and MCP `export_history`.
- Synthetic alert verification command (`webdelta test-notifier`) and MCP `test_notifier` tool.
- Three new MCP tools (`update_watch`, `test_notifier`, `export_history`), MCP Resources (`webdelta://watches`, redacted `webdelta://config`, `webdelta://history/{watch_id}`, `webdelta://diff/{watch_id}`), and Streamable HTTP transport (`webdelta mcp --http` plus `/mcp` mounted inside `webdelta serve`).
- Browser pre-extraction actions: `click_selector` and `scroll_to_bottom` across `BrowserOptions`, `BrowserConfig`, MCP schemas, and CLI (`--click-selector`, `--scroll-to-bottom`).
- Multi-stage `Dockerfile`, `.dockerignore`, `docker-compose.yml`, `docs/docker.md`, and `.github/workflows/docker.yml` for GHCR container publishing.

## [0.2.0] - 2026-11-26

### Added

- New `webdelta-browser` crate implementing a pure-Rust Chrome DevTools Protocol (CDP) renderer over WebSocket (`tokio-tungstenite`), supporting local Chrome/Edge/Chromium auto-discovery (`WEBDELTA_BROWSER_PATH` override) and remote `cdp_url` endpoints.
- Hybrid `engine` mode (`http`, `browser`, `auto`) across `WatchConfig`, `MonitorEngine`, MCP tools (`add_watch`, `preview_extract`, `list_watches`), and CLI (`--engine`, `--wait-selector`, `--wait-ms`, `--cdp-url`).
- Custom `-H` / `--request-header` flag on `webdelta preview`.
- SPA shell detection (`looks_like_spa_shell`) and offline JS/SPA CDP integration tests.
- Reproducible performance and memory benchmarks in `docs/benchmarks.md` plus `examples/spa-browser.toml`.
- Project status, release, license, MSRV, and MCP badges in `README.md`.

### Fixed

- Tightened `parse_numeric` to ignore semver tags (`v0.1.0`), IPv4 addresses, dates, and alphanumeric identifiers (`h1`, `sha256`) while preserving currency and European/Indonesian separator parsing.
- Scoped `--verbose` (`-v`) tracing logs to `webdelta_*` crates instead of emitting third-party HTML parser debug noise.

## [0.1.0] - 2026-11-26

### Added

- Workspace architecture split into `webdelta-core`, `webdelta-mcp`, and `webdelta-cli`.
- HTTP fetcher built on `reqwest` with `If-None-Match` (`ETag`), `If-Modified-Since`, custom headers, configurable timeouts, and exponential backoff retries on transient errors, HTTP 429, and 5xx responses.
- Content extraction modes: readable `text` (skipping `script`, `style`, `noscript`, and `template`), `css` (inner text, attributes, single or multi-match), `json_path`, `regex` (with capture group selection), and `status` (status code plus optional response header).
- Noise reduction via `ignore_regex` patterns and line-preserving whitespace/case normalization.
- Currency and numeric parser supporting US, Indonesian, and European separators (`$1,299.99`, `Rp 1.500.000`, `EUR 49,90`, signed numbers, and percentages) along with signed and percentage delta calculations.
- Unified diff engine backed by `similar` and rule evaluator supporting `changed`, `contains`, `not_contains`, `regex_match`, `numeric_lt`, `numeric_lte`, `numeric_gt`, `numeric_gte`, `drop_pct_at_least`, and `rise_pct_at_least`.
- Embedded transactional `redb` snapshot store with bounded history pruning.
- Notification channels: Webhook (with optional `X-WebDelta-Signature` HMAC-SHA256), Telegram, Discord, Slack, and ntfy, with `${VAR_NAME}` environment variable expansion.
- CLI (`webdelta`) with `init`, `validate`, `add`, `list`, `remove`, `preview`, `check` (including `--exit-code`), `diff`, `history`, continuous `watch` scheduler, and `--json` output across commands.
- Built-in JSON-RPC 2.0 `stdio` MCP server (`webdelta mcp`) exposing `list_watches`, `add_watch`, `remove_watch`, `check_now`, `get_diff`, `get_history`, and `preview_extract`.
- Offline unit, HTTP integration, CLI end-to-end, and MCP end-to-end test suites plus GitHub Actions CI and release workflows.
