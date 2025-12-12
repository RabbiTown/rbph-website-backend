# Build stage
FROM rust:1.76-slim-bullseye AS builder
WORKDIR /usr/src/app

# Install system deps for runtime (libssl for sqlx/postgres)
RUN apt-get update && apt-get install -y libssl-dev pkg-config ca-certificates && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY config.toml ./

RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/src/app/target/release/rbph-website-backend ./rbph-website-backend
COPY config.toml ./config.toml
EXPOSE 9999
CMD ["./rbph-website-backend"]
