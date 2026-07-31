# syntax=docker/dockerfile:1

# ---------------------------------------------------------------- planner --
# cargo-chef captures just the dependency graph, so the expensive dependency
# build below is cached against Cargo.toml alone. Editing source code then
# rebuilds in seconds instead of recompiling ~400 crates.
FROM rust:1.97-slim-bookworm AS chef
WORKDIR /build

# `utoipa-swagger-ui` fetches the Swagger UI bundle from its build script and
# shells out to curl to do it, which the slim image does not ship.
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-chef --locked --version ^0.1

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

# ------------------------------------------------------------------- web --
# The Leptos dashboard, compiled to WebAssembly. Its own stage so the WASM
# toolchain never lands in the server build — and so editing the API does not
# invalidate the frontend layer, or the other way round.
FROM chef AS web
RUN rustup target add wasm32-unknown-unknown \
    && cargo install trunk --locked --version ^0.21

COPY crates/core ./crates/core
COPY crates/web ./crates/web

WORKDIR /build/crates/web
RUN trunk build --release

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

# The compiled dashboard is embedded into the binary by `rust-embed`, so it has
# to be in place before the server is built.
COPY --from=web /build/crates/web/dist ./crates/web/dist

RUN cargo build --release --bin flagforge && \
    strip target/release/flagforge

# ---------------------------------------------------------------- runtime --
# Distroless: no shell, no package manager, nothing for an attacker who gets
# code execution to pivot with. One binary contains the API, the migrations and
# the dashboard.
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

WORKDIR /app
COPY --from=builder /build/target/release/flagforge /app/flagforge

# Never root, even inside a container.
USER nonroot:nonroot

ENV HOST=0.0.0.0 \
    PORT=8080 \
    APP_ENV=production \
    RUST_LOG=flagforge=info,flagforge_api=info,flagforge_storage=info,warn

EXPOSE 8080

# `exec` form, so the process is PID 1 and receives SIGTERM directly — which
# is what the graceful shutdown depends on.
ENTRYPOINT ["/app/flagforge"]
