# glibc (Debian), not musl (Alpine): with musl the bundled SQLite is ~1.7x
# slower on large counts (measured).

# ---- Rust build stage ----
FROM rust:1.88-bookworm AS builder

WORKDIR /build
COPY . .
RUN cargo build --release --bin sazare-server

# ---- Runtime stage ----
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 sazare \
    && useradd --system --uid 10001 --gid sazare --no-create-home --shell /usr/sbin/nologin sazare

ARG CONFIG_FILE=config.example.yaml

COPY --from=builder /build/target/release/sazare-server /usr/local/bin/sazare-server
COPY config.example.yaml /etc/sazare/config.example.yaml
COPY ${CONFIG_FILE} /etc/sazare/config.yaml
COPY apps/ /apps/

COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

RUN mkdir -p /data && chown sazare:sazare /data

ENV SAZARE_DATA_DIR=/data
ENV SAZARE_HOST=0.0.0.0
ENV SAZARE_PORT=8080
ENV SAZARE_APPS_DIR=/apps

EXPOSE 8080

# Liveness for `docker ps` / orchestrators: GET /health (unauthenticated).
# Plain HTTP on SAZARE_PORT; with TLS configured in sazare, replace it.
# (bash's /dev/tcp: the slim image has no curl or wget.)
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/${SAZARE_PORT:-8080} && printf 'GET /health HTTP/1.0\\r\\nHost: localhost\\r\\n\\r\\n' >&3 && head -1 <&3 | grep -q ' 200 '"]

VOLUME ["/data"]

WORKDIR /etc/sazare

# Runs `sazare-server run` (foreground) as the unprivileged `sazare` user (uid
# 10001); arguments are passed on, e.g. `docker run ... --demo`.
ENTRYPOINT ["docker-entrypoint.sh"]
