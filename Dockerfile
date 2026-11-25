# ---- Rust build stage ----
FROM rust:1.88-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build
COPY . .
RUN cargo build --release --bin sazare-server

# ---- Runtime stage ----
FROM alpine:3.21

RUN apk add --no-cache ca-certificates

ARG CONFIG_FILE=config.example.yaml

COPY --from=builder /build/target/release/sazare-server /usr/local/bin/sazare-server
COPY config.example.yaml /etc/sazare/config.example.yaml
COPY ${CONFIG_FILE} /etc/sazare/config.yaml
COPY apps/ /apps/

RUN mkdir -p /data

ENV SAZARE_DATA_DIR=/data
ENV SAZARE_HOST=0.0.0.0
ENV SAZARE_PORT=8080
ENV SAZARE_APPS_DIR=/apps

EXPOSE 8080

VOLUME ["/data"]

WORKDIR /etc/sazare

ENTRYPOINT ["sazare-server"]
