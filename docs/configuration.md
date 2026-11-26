# Configuration Reference

`webdelta` reads its watches, defaults, and notification channels from a TOML file (`webdelta.toml` by default, or passed via `--config <PATH>`).

## `[defaults]`

Global defaults applied whenever a `[[watches]]` entry omits the corresponding field.

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `interval` | string | `"15m"` | Polling interval for `webdelta watch`. Supported units: `s`, `m`, `h`, `d`. |
| `timeout_secs` | integer | `15` | Per-request HTTP timeout in seconds. |
| `retries` | integer | `2` | Retry count on transport errors, HTTP 429, or 5xx responses. |
| `user_agent` | string | `"webdelta/0.1.0"` | `User-Agent` header sent on HTTP checks. |
| `max_history` | integer | `100` | Maximum historical snapshots retained per watch in `redb`. |
| `concurrency` | integer | `8` | Maximum concurrent HTTP requests during batch checks. |

## `[[watches]]`

Each `[[watches]]` block defines one monitored URL or endpoint.

| Key | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `id` | string | Yes | Unique identifier using ASCII letters, digits, `.`, `-`, or `_`. |
| `name` | string | No | Display label used in alerts and JSON output. Defaults to `id`. |
| `url` | string | Yes | Target `http://` or `https://` URL. |
| `method` | string | No | `"GET"` (default), `"HEAD"`, or `"POST"`. |
| `headers` | table | No | Extra HTTP request headers (`key = "value"`). |
| `interval` | string | No | Overrides `defaults.interval` for this watch. |
| `timeout_secs` | integer | No | Overrides `defaults.timeout_secs`. |
| `retries` | integer | No | Overrides `defaults.retries`. |
| `ignore_regex` | array of strings | No | Regular expressions stripped from extracted text before comparison. |
| `notifiers` | array of strings | No | Names referencing entries in `[notifiers]`. |
| `enabled` | boolean | No | Set to `false` to skip during scheduled or batch checks. Defaults to `true`. |

## `[watches.extractor]`

Controls how comparable text is extracted from the HTTP response.

### `text` (default)

Extracts visible text. When the body looks like HTML, `<script>`, `<style>`, `<noscript>`, and `<template>` tags are skipped.

```toml
[watches.extractor]
type = "text"
```

### `css`

Queries HTML using a CSS selector. Set `attribute` to read an element attribute (such as `href` or `content`) instead of inner text, or `all = true` to join every match with newlines.

```toml
[watches.extractor]
type = "css"
selector = ".pricing-card .amount"
attribute = "data-price"
all = false
```

### `json_path`

Queries a JSON response using JSONPath. Scalar values render unquoted; objects and arrays render as formatted JSON.

```toml
[watches.extractor]
type = "json_path"
path = "$.data.price"
```

### `regex`

Extracts the full match (`group = 0`) or a specific capture group from the response body.

```toml
[watches.extractor]
type = "regex"
pattern = "Version:\\s+([0-9]+\\.[0-9]+\\.[0-9]+)"
group = 1
```

### `status`

Tracks the HTTP response status code, optionally paired with a single response header. Unlike content extractors, `status` treats 4xx and 5xx HTTP status codes as valid state transitions rather than check errors.

```toml
[watches.extractor]
type = "status"
header = "x-release-sha"
```

## `[watches.normalize]`

Controls whitespace and casing normalization applied after `ignore_regex` filtering.

```toml
[watches.normalize]
trim = true
collapse_whitespace = true
lowercase = false
```

## `[[watches.conditions]]`

When omitted, any difference from the previous snapshot triggers an alert (`type = "changed"`). When multiple conditions are specified, all of them must match (logical AND).

| Type | Fields | Behavior |
| :--- | :--- | :--- |
| `changed` | none | Triggers whenever extracted text differs from the previous snapshot. |
| `contains` | `value`, optional `case_sensitive` | Triggers when current text contains `value`. |
| `not_contains` | `value`, optional `case_sensitive` | Triggers when current text does not contain `value`. |
| `regex_match` | `pattern` | Triggers when current text matches `pattern`. |
| `numeric_lt` | `threshold` | Triggers when parsed number is strictly less than `threshold`. |
| `numeric_lte` | `threshold` | Triggers when parsed number is less than or equal to `threshold`. |
| `numeric_gt` | `threshold` | Triggers when parsed number is strictly greater than `threshold`. |
| `numeric_gte` | `threshold` | Triggers when parsed number is greater than or equal to `threshold`. |
| `drop_pct_at_least` | `percent` | Triggers when parsed number dropped by at least `percent`% vs previous snapshot. |
| `rise_pct_at_least` | `percent` | Triggers when parsed number rose by at least `percent`% vs previous snapshot. |

## `[notifiers.<name>]`

Environment variables formatted as `${VAR_NAME}` are expanded when alerts are sent.

### `webhook`

```toml
[notifiers.n8n]
type = "webhook"
url = "https://n8n.example.com/webhook/webdelta"
secret = "${WEBDELTA_WEBHOOK_SECRET}"

[notifiers.n8n.headers]
Authorization = "Bearer ${WEBHOOK_BEARER_TOKEN}"
```

### `telegram`

```toml
[notifiers.telegram]
type = "telegram"
bot_token = "${TELEGRAM_BOT_TOKEN}"
chat_id = "${TELEGRAM_CHAT_ID}"
```

### `discord` and `slack`

```toml
[notifiers.discord]
type = "discord"
webhook_url = "${DISCORD_WEBHOOK_URL}"

[notifiers.slack]
type = "slack"
webhook_url = "${SLACK_WEBHOOK_URL}"
```

### `ntfy`

```toml
[notifiers.ntfy]
type = "ntfy"
url = "https://ntfy.sh"
topic = "my-webdelta-topic"
priority = "high"
tags = ["bell", "price"]
token = "${NTFY_TOKEN}"
```
