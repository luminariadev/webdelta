# Model Context Protocol (MCP) Integration

`webdelta` includes a built-in Model Context Protocol server supporting both `stdio` and Streamable HTTP (`POST /mcp`), plus MCP Tools and MCP Resources.

## Starting the Server

### Standard Input / Output (`stdio`)

```bash
webdelta mcp --config webdelta.toml --db .webdelta/state.redb
```

Diagnostic logs are written exclusively to `stderr` so `stdout` carries only newline-delimited JSON-RPC 2.0 frames.

### Streamable HTTP (`POST /mcp`)

```bash
webdelta mcp --http 127.0.0.1:8788 --token "${WEBDELTA_MCP_TOKEN}"
```

When running the web dashboard (`webdelta serve`), `POST /mcp` is also mounted directly on the same server and shares the embedded `redb` database handle and Bearer token (`--token` / `WEBDELTA_TOKEN`).

## Client Configuration Snippet

Any MCP-compatible client can register `webdelta` over `stdio`:

```json
{
  "mcpServers": {
    "webdelta": {
      "command": "webdelta",
      "args": [
        "mcp",
        "--config",
        "/path/to/webdelta.toml",
        "--db",
        "/path/to/.webdelta/state.redb"
      ]
    }
  }
}
```

## Available Tools (10)

| Tool | Required Args | Optional Args | Description |
| :--- | :--- | :--- | :--- |
| `list_watches` | none | `include_disabled` | List configured watches, engine modes, and their latest recorded snapshots. |
| `add_watch` | `id`, `url` | `name`, `interval`, `engine`, `wait_for_selector`, `wait_ms`, `cdp_url`, `click_selector`, `scroll_to_bottom`, `extractor_type`, `selector`, `attribute`, `all`, `path`, `pattern`, `group`, `header`, `ignore_regex`, `conditions`, `notifiers` | Validate and add a new watch to `webdelta.toml`. |
| `update_watch` | `id` | `name`, `url`, `interval`, `enabled`, `engine`, `wait_for_selector`, `wait_ms`, `cdp_url`, `click_selector`, `scroll_to_bottom`, `extractor_type`, `selector`, `attribute`, `all`, `path`, `pattern`, `group`, `header`, `ignore_regex`, `conditions`, `notifiers` | Patch fields on an existing watch, validate, and persist changes. |
| `remove_watch` | `id` | `purge_history` | Remove a watch from config and optionally delete stored snapshots. |
| `check_now` | none | `watch_id`, `notify` | Check one watch or all enabled watches immediately (`notify` defaults to `false`). |
| `get_diff` | `watch_id` | `context_lines` | Return the unified diff and numeric delta between the two most recent snapshots. |
| `get_history` | `watch_id` | `limit` | Return stored snapshot history ordered newest first (default `10`, max `500`). |
| `preview_extract` | `url` | `engine`, `wait_for_selector`, `wait_ms`, `cdp_url`, `click_selector`, `scroll_to_bottom`, `extractor_type`, `selector`, `attribute`, `all`, `path`, `pattern`, `group`, `header`, `ignore_regex`, `timeout_secs` | Fetch or render a URL once and return extracted text and parsed number without saving state. |
| `test_notifier` | `notifier` | none | Send a synthetic test alert through a configured notifier (`webhook`, `telegram`, `discord`, `slack`, or `ntfy`). |
| `export_history` | `watch_id`, `format` | `limit` | Export stored snapshot history as `json`, `csv`, or `rss` (default `limit` `50`). |

## Available Resources

Clients supporting `resources/list`, `resources/templates/list`, and `resources/read` can inspect read-only monitor state directly:

| Resource URI / Template | MIME Type | Content |
| :--- | :--- | :--- |
| `webdelta://watches` | `application/json` | Configured watches and latest recorded snapshots. |
| `webdelta://config` | `text/plain` | Current `webdelta.toml` with literal notifier secrets and sensitive headers replaced by `<redacted>`. |
| `webdelta://history/{watch_id}` | `application/json` | Up to 50 most recent snapshots for `{watch_id}`. |
| `webdelta://diff/{watch_id}` | `application/json` | Latest unified diff and numeric delta for `{watch_id}`. |

## Example Tool Calls

JSON API preview over `stdin` or `POST /mcp`:

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"preview_extract","arguments":{"url":"https://api.example.com/v1/meta","extractor_type":"json_path","path":"$.version"}}}
```

Client-rendered SPA preview using `auto` fallback and pre-capture click action:

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"preview_extract","arguments":{"url":"https://app.example.com/pricing","engine":"auto","click_selector":"#accept-cookies","wait_for_selector":".price","extractor_type":"css","selector":".price"}}}
```

Each tool returns JSON inside `result.content[0].text` including `url`, `engine_used`, `status_code`, `elapsed_ms`, `extracted_text`, `numeric_value`, and `content_hash`.
