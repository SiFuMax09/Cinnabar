# syntax=docker/dockerfile:1

# ---- Chef base: Rust on Alpine (musl) with cargo-chef preinstalled ----------
FROM lukemathwalker/cargo-chef:latest-rust-alpine AS chef
# build-base/cmake/perl/clang: needed to compile aws-lc (rustls crypto provider)
RUN apk add --no-cache build-base musl-dev cmake perl clang
WORKDIR /app

# ---- Planner: compute the dependency recipe (changes only if manifests do) ---
FROM chef AS planner
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY .cargo/ ./.cargo/
COPY crates/ ./crates/
COPY src/ ./src/
COPY mc_data/ ./mc_data/
RUN cargo chef prepare --recipe-path recipe.json

# ---- Builder: cook dependencies (cached layer), then build the server -------
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
COPY rust-toolchain.toml ./
COPY .cargo/ ./.cargo/
COPY mc_data/ ./mc_data/
RUN cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
COPY src/ ./src/
RUN cargo build --release --bin cinnabar \
 && strip target/release/cinnabar

# ---- Runtime ----------------------------------------------------------------
FROM alpine:3.22 AS runtime

ARG VERSION=dev
ARG REVISION=unknown
ARG SOURCE=https://github.com/SiFuMax09/Cinnabar
LABEL org.opencontainers.image.title="Cinnabar" \
      org.opencontainers.image.description="Minecraft High Performance Redstone Server (Minecraft 26.2)" \
      org.opencontainers.image.source="${SOURCE}" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}"

# ca-certificates: Mojang skin lookup uses HTTPS (reqwest with rustls and the
# platform verifier, which reads the system trust store).
# "cinnabar" is the default user; "container" (uid 988, home /home/container) is
# what Pterodactyl Wings expects an image to provide.
RUN apk add --no-cache ca-certificates tzdata \
 && adduser -D -u 10001 -h /data cinnabar \
 && adduser -D -u 988 -h /home/container container \
 && mkdir -p /data /home/container \
 && chown cinnabar:cinnabar /data \
 && chown container:container /home/container

COPY --from=builder /app/target/release/cinnabar /usr/local/bin/cinnabar
COPY --chmod=0755 docker/entrypoint.sh /entrypoint.sh

ENV CINNABAR_LOG=info
USER cinnabar
WORKDIR /data
VOLUME ["/data"]
EXPOSE 25565/tcp
# The server saves and shuts down on SIGINT and SIGTERM.
STOPSIGNAL SIGINT
ENTRYPOINT ["/entrypoint.sh"]
