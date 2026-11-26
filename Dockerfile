FROM rust:1-bookworm AS builder

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release --locked --bin webdelta

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/sh webdelta

COPY --from=builder /src/target/release/webdelta /usr/local/bin/webdelta

RUN mkdir -p /data && chown -R webdelta:webdelta /data
USER webdelta
WORKDIR /data

EXPOSE 8787

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8787/health || exit 1

ENTRYPOINT ["webdelta"]
CMD ["--config", "/data/webdelta.toml", "--db", "/data/state.redb", "serve", "--bind", "0.0.0.0:8787", "--background-watch"]
