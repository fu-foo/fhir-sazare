# ---- Rust build stage ----
FROM rust:1.88-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build
COPY . .
RUN cargo build --release --bin sazare-server

# ---- Runtime stage ----
FROM alpine:3.21

RUN apk add --no-cache ca-certificates su-exec \
    && addgroup -S -g 10001 sazare \
    && adduser -S -D -H -u 10001 -G sazare sazare

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

VOLUME ["/data"]

WORKDIR /etc/sazare

# Runs `sazare-server run` (foreground) as the unprivileged `sazare` user (uid
# 10001); arguments are passed on, e.g. `docker run ... --demo`.
ENTRYPOINT ["docker-entrypoint.sh"]
