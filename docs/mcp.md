# Model Context Protocol (MCP) Integration

`webdelta` includes a built-in Model Context Protocol server over standard input and standard output (`stdio`), requiring no separate runtime or daemon.

## Starting the Server

```bash
webdelta mcp --config webdelta.toml --db .webdelta/state.redb
```

Diagnostic logs are written exclusively to `stderr` so `stdout` carries only newline-delimited JSON-RPC 2.0 frames.

## Client Configuration Snippet

Any MCP-compatible client can register `webdelta` with a standard server entry:

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

## Available Tools

| Tool | Required Args | Optional Args | Description |
| :--- | :--- | :--- | :--- |
| `list_watches` | none | `include_disabled` | List configured watches and their latest recorded snapshots. |
| `add_watch` | `id`, `url` | `name`, `interval`, `extractor_type`, `selector`, `attribute`, `all`, `path`, `pattern`, `group`, `header`, `ignore_regex`, `conditions`, `notifiers` | Validate and add a new watch to `webdelta.toml`. |
| `remove_watch` | `id` | `purge_history` | Remove a watch from config and optionally delete stored snapshots. |
| `check_now` | none | `watch_id`, `notify` | Check one watch or all enabled watches immediately (`notify` defaults to `false`). |
| `get_diff` | `watch_id` | `context_lines` | Return the unified diff and numeric delta between the two most recent snapshots. |
| `get_history` | `watch_id` | `limit` | Return stored snapshot history ordered newest first (default `10`, max `500`). |
| `preview_extract` | `url` | `extractor_type`, `selector`, `attribute`, `all`, `path`, `pattern`, `group`, `header`, `ignore_regex`, `timeout_secs` | Fetch a URL once and return extracted text and parsed number without saving state. |

## Example Tool Call

Request over `stdin`:

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"preview_extract","arguments":{"url":"https://api.example.com/v1/meta","extractor_type":"json_path","path":"$.version"}}}
```

The tool returns JSON inside `result.content[0].text` containing `url`, `status_code`, `elapsed_ms`, `extracted_text`, `numeric_value`, and `content_hash`.
