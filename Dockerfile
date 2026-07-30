# syntax=docker/dockerfile:1

# ---------------------------------------------------------------- planner --
# cargo-chef captures just the dependency graph, so the expensive dependency
# build below is cached against Cargo.toml alone. Editing source code then
# rebuilds in seconds instead of recompiling ~400 crates.
FROM rust:1.97-slim-bookworm AS chef
WORKDIR /build
RUN cargo install cargo-chef --locked --version ^0.1

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

# ----------------------------------------------------------------- builder --
FROM chef AS builder

# Queries are verified against the committed .sqlx cache, so the image builds
# with no database in reach.
ENV SQLX_OFFLINE=true

COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
COPY .sqlx ./.sqlx

RUN cargo build --release --bin flagforge && \
    strip target/release/flagforge

# ---------------------------------------------------------------- runtime --
# Distroless: no shell, no package manager, nothing for an attacker who gets
# code execution to pivot with. The binary is statically linked against
# everything except libc and ships with CA certificates for TLS to Postgres.
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

WORKDIR /app
COPY --from=builder /build/target/release/flagforge /app/flagforge

# Never root, even inside a container.
USER nonroot:nonroot

ENV HOST=0.0.0.0 \
    PORT=8080 \
    APP_ENV=production \
    RUST_LOG=flagforge_api=info,flagforge_storage=info,warn

EXPOSE 8080

# `exec` form, so the process is PID 1 and receives SIGTERM directly — which
# is what the graceful shutdown depends on.
ENTRYPOINT ["/app/flagforge"]
