# Build stage
FROM rust:1.85-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build
COPY . .
RUN cargo build --release --bin sazare-server

# Runtime stage
FROM alpine:3.21

RUN apk add --no-cache ca-certificates

COPY --from=builder /build/target/release/sazare-server /usr/local/bin/sazare-server
COPY config.example.yaml /etc/sazare/config.yaml

RUN mkdir -p /data

ENV SAZARE_DATA_DIR=/data
ENV SAZARE_HOST=0.0.0.0
ENV SAZARE_PORT=8080

EXPOSE 8080

VOLUME ["/data"]

ENTRYPOINT ["sazare-server"]
