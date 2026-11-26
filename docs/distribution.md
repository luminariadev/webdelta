# Distribution and Packaging

`webdelta` is distributed as standalone release archives, container images on GitHub Container Registry, and package manifests for Scoop and Homebrew.

## Prebuilt GitHub Release Archives

Every version tag builds static archives for four targets and attaches a `SHA256SUMS.txt` file on [GitHub Releases](https://github.com/luminariadev/webdelta/releases):

- `webdelta-v0.3.0-x86_64-unknown-linux-gnu.tar.gz`
- `webdelta-v0.3.0-x86_64-pc-windows-msvc.zip`
- `webdelta-v0.3.0-x86_64-apple-darwin.tar.gz`
- `webdelta-v0.3.0-aarch64-apple-darwin.tar.gz`

Verify downloads against `SHA256SUMS.txt`:

```bash
curl -fsSLO https://github.com/luminariadev/webdelta/releases/download/v0.3.0/webdelta-v0.3.0-x86_64-unknown-linux-gnu.tar.gz
curl -fsSLO https://github.com/luminariadev/webdelta/releases/download/v0.3.0/SHA256SUMS.txt
sha256sum -c --ignore-missing SHA256SUMS.txt
```

On Windows PowerShell:

```powershell
Get-FileHash .\webdelta-v0.3.0-x86_64-pc-windows-msvc.zip -Algorithm SHA256
```

## Scoop (Windows)

Install directly using the manifest in [packaging/scoop/webdelta.json](../packaging/scoop/webdelta.json):

```powershell
scoop install https://raw.githubusercontent.com/luminariadev/webdelta/main/packaging/scoop/webdelta.json
```

Or from a local checkout:

```powershell
scoop install ./packaging/scoop/webdelta.json
```

## Homebrew (macOS and Linux)

Install using the formula in [packaging/homebrew/webdelta.rb](../packaging/homebrew/webdelta.rb):

```bash
brew install --formula https://raw.githubusercontent.com/luminariadev/webdelta/main/packaging/homebrew/webdelta.rb
```

Or from a local checkout:

```bash
brew install --formula ./packaging/homebrew/webdelta.rb
```

## Docker / GHCR

Multi-stage public images are published to GitHub Container Registry on every release tag:

```bash
docker pull ghcr.io/luminariadev/webdelta:latest
docker pull ghcr.io/luminariadev/webdelta:0.3.0
```

See [docs/docker.md](docker.md) for volume mounts, environment variables, and `docker-compose.yml` usage.

## Publishing Crates to crates.io

The workspace is structured with explicit internal versions so crates can be published in dependency order:

```bash
cargo publish -p webdelta-browser
cargo publish -p webdelta-core
cargo publish -p webdelta-mcp
cargo publish -p webdelta-cli
```

Wait briefly between steps until crates.io indexes each dependency before publishing the next crate in the chain. You can validate the packaging tarball locally before publishing with `cargo package --workspace --locked --no-verify`.
