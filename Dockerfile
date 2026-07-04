# syntax=docker/dockerfile:1.7
FROM rust:1.95-bookworm AS builder

WORKDIR /src
COPY . .
ENV SQLX_OFFLINE=true
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked && \
    cp target/release/rbph-website-backend /tmp/rbph-website-backend

FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --system --gid 10001 rbph && \
    useradd --system --uid 10001 --gid rbph --home-dir /app --create-home rbph && \
    install -d -o rbph -g rbph /data/assets

WORKDIR /app
COPY --from=builder /tmp/rbph-website-backend /usr/local/bin/rbph-website-backend

USER rbph
EXPOSE 9999
ENTRYPOINT ["rbph-website-backend"]
