# Docker Deployment

`webdelta` provides a multi-stage `Dockerfile` and `docker-compose.yml` that run the embedded web dashboard, background scheduler, and Streamable HTTP MCP endpoint as a non-root user.

## Quick Start with Docker Compose

1. Create a local `./data` directory containing a valid `webdelta.toml` (you can start from `examples/price-tracker.toml` or run `webdelta init` locally):

```bash
mkdir -p data
cp examples/price-tracker.toml data/webdelta.toml
```

2. Optionally set environment variables for API authentication and notifier secrets:

```bash
export WEBDELTA_TOKEN="change-me-to-a-strong-token"
export WEBDELTA_WEBHOOK_SECRET="your-hmac-secret"
```

3. Build and start the container:

```bash
docker compose up -d --build
```

The service exposes port `8787`:

- Web dashboard: `http://127.0.0.1:8787/`
- Health endpoint: `http://127.0.0.1:8787/health`
- Streamable HTTP MCP endpoint: `http://127.0.0.1:8787/mcp`

## Prebuilt Container Images

Version tags automatically publish container images to GitHub Container Registry via `.github/workflows/docker.yml`:

```bash
docker pull ghcr.io/luminariadev/webdelta:latest
docker run -d -p 8787:8787 -v "$(pwd)/data:/data" -e WEBDELTA_TOKEN="secret" ghcr.io/luminariadev/webdelta:latest
```

## Health Check and Persistence

- State is persisted in `/data/state.redb` inside the mounted `./data` volume.
- Docker checks `GET /health` every 30 seconds and expects HTTP `200`.
- When `WEBDELTA_TOKEN` is set, every `/api/*` and `/mcp` request requires `Authorization: Bearer <token>`.
