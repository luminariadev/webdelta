# Benchmarks

This document records reproducible performance, latency, and memory measurements for `webdelta` v0.2.0.

## Test Environment

- **OS**: Windows 11 Home Single Language (x86_64)
- **CPU**: 11th Gen Intel Core i5-11400H @ 2.70 GHz
- **Memory**: 15.7 GB RAM
- **Toolchain**: `rustc 1.100.0`, release profile (`opt-level = 3`, `lto = true`, `codegen-units = 1`, `strip = true`)
- **Binary size**: `webdelta.exe` = 6.64 MB

## Methodology

1. A multi-threaded HTTP/1.1 mock server bound to `127.0.0.1` served 100 distinct HTML pages (`/item/0` through `/item/99`) with `ETag` support and a client-side JavaScript SPA route (`/spa`) injecting `<span class="price">$249.00</span>` after a 30 ms timer.
2. Process wall-clock duration and `PeakWorkingSet64` memory were captured externally using `.NET` `System.Diagnostics.Process` and `Stopwatch` around `target\release\webdelta.exe`.
3. MCP latency was measured over an active `webdelta mcp` `stdio` process sending newline-delimited JSON-RPC 2.0 frames.

## Results

| Scenario | Command / Operation | Wall Time | Throughput | Peak RAM |
| :--- | :--- | :--- | :--- | :--- |
| Binary startup | `webdelta --version` (5-run average) | 25.2 ms | n/a | 2.56 MB |
| Config validation | `webdelta --config bench100.toml validate` | 51.8 ms | n/a | 2.55 MB |
| 100 HTTP + CSS checks | `webdelta check --concurrency 32 --json` (initial baseline + `redb` writes) | 303.9 ms | 329.0 checks/s | 13.95 MB |
| 100 `304 Not Modified` checks | `webdelta check --concurrency 32 --json` (conditional `If-None-Match`) | 203.0 ms | 492.5 checks/s | 13.79 MB |
| SPA render (`auto`) | `webdelta preview <url>/spa --engine auto --wait-selector .price --css .price` | 840.5 ms (818 ms CDP) | n/a | ephemeral Chrome |
| SPA render (`browser`) | `webdelta preview <url>/spa --engine browser --wait-selector .price --css .price` | 887.8 ms (863 ms CDP) | n/a | ephemeral Chrome |
| MCP `initialize` | JSON-RPC `initialize` over `stdio` | 12.4 ms | n/a | n/a |
| MCP `preview_extract` (SPA) | JSON-RPC `tools/call` with `engine: "auto"` | 843.6 ms | n/a | ephemeral Chrome |

## Observations

- Routine HTTP and conditional `304` checks stay under 14 MB peak working set memory even at 32-way concurrency against 100 targets.
- Conditional `If-None-Match` (`ETag`) caching cuts 100-target batch time by roughly 33% (`303.9 ms` down to `203.0 ms`) and skips unnecessary HTML parsing and history writes.
- In `auto` mode, static HTML and JSON endpoints pay zero browser overhead; only targets detected as unrendered SPA shells or missing expected selectors launch headless Chromium over CDP.
