# syntax=docker/dockerfile:1

FROM rust:1-slim-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY examples ./examples

RUN cargo build --release --example catan --features=server,nn

FROM debian:bookworm-slim AS runtime

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/examples/catan /app/catan
COPY --from=builder /app/examples/catan/web /app/examples/catan/web

EXPOSE 3000

ENTRYPOINT ["/app/catan"]
CMD ["serve", "--eval", "rollout", "--human", "both"]
