# syntax=docker/dockerfile:1

FROM rust:1-slim-bookworm AS builder

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY examples ./examples

RUN cargo build --release --example catan --features=server,nn

FROM debian:bookworm-slim AS runtime

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/examples/catan /app/catan
COPY --from=builder /app/examples/catan/web /app/examples/catan/web
COPY Ads.txt /app/examples/catan/web/ads.txt
COPY checkpoints/catan-nexus-v3/model_iter_315.mpk /app/checkpoints/catan-nexus-v3/model_iter_315.mpk

EXPOSE 3000

ENTRYPOINT ["/app/catan"]
CMD ["serve", "--eval", "nexus-v3:/app/checkpoints/catan-nexus-v3/model_iter_315.mpk", "--human", "both"]
