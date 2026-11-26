# Contributing to webdelta

Thank you for contributing to `webdelta`. Please review the project guidelines below before opening a pull request.

## Repository Layout

- `crates/webdelta-core`: configuration parsing and validation, HTTP fetching, extractors, numeric parsing, diffing, rule evaluation, `redb` storage, and notifiers.
- `crates/webdelta-mcp`: `stdio` JSON-RPC 2.0 Model Context Protocol server wrapping `webdelta-core`.
- `crates/webdelta-cli`: `webdelta` binary, CLI subcommands, and polling scheduler.

## Development Workflow

1. Fork the repository and create a topic branch from `main`.
2. Keep changes focused and add unit or offline integration tests for any new behavior. Avoid tests that depend on external live websites or paid APIs.
3. Run the local quality checks before pushing:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

4. Use clear Conventional Commit prefixes (`feat:`, `fix:`, `docs:`, `test:`, `refactor:`, `ci:`, `chore:`).
5. Update `README.md`, `docs/`, and `CHANGELOG.md` under `[Unreleased]` when changing CLI flags, configuration fields, or MCP tool schemas.
