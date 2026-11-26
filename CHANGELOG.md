# Changelog

All notable changes to `webdelta` are documented in this file.

The format follows Keep a Changelog, and this project adheres to Semantic Versioning.

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
