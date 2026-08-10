# syntax=docker/dockerfile:1.7
#
# Production image for the `meridian-web` crate — a Dioxus 0.7 *fullstack* app
# (WASM client + Axum server) built with the Dioxus CLI (`dx`).
#
# Build (from the repo root):
#   DOCKER_BUILDKIT=1 docker build -t meridian-web:latest .
#   # optional: embed the git sha reported by /build-info
#   docker build --build-arg GIT_SHA="$(git rev-parse --short HEAD)" -t meridian-web:latest .
#
# BuildKit is required (default on modern Docker) for the `--mount=type=cache`
# lines. The dx build fetches a matching `wasm-bindgen` + `wasm-opt` on first run,
# so the build needs network access.
#
# ---------------------------------------------------------------------------
# Stage 1 — builder: Rust 1.95 toolchain + dioxus-cli 0.7.9, builds the bundle.
# ---------------------------------------------------------------------------
# Pinned: the workspace rust-toolchain.toml pins 1.95.0 (rustup will honour it),
# and Cargo.lock pins dioxus 0.7.9 — dioxus-cli must match exactly.
FROM rust:1.95-bookworm AS builder

# Build dependencies:
#   cmake, perl, clang, libclang-dev — aws-lc-rs / aws-lc-sys (pulled in via
#     jsonwebtoken's aws_lc_rs feature and rustls) compile C + assembly.
#   pkg-config, git — general build-script needs.
# reqwest/sqlx use rustls (not OpenSSL) and the `image` crate uses pure-Rust
# codecs, so no libssl-dev / libjpeg-dev are required.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      cmake perl clang libclang-dev pkg-config git ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# The wasm client target. rust-toolchain.toml also declares it, but adding it
# explicitly fails fast if the toolchain cannot be provisioned.
RUN rustup target add wasm32-unknown-unknown

# dioxus-cli must match the `dioxus` crate version (0.7.9). `cargo install` builds
# it from source (slow but reproducible). Faster alternative if you add it first:
#   cargo install cargo-binstall && cargo binstall -y dioxus-cli@0.7.9
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install dioxus-cli --version 0.7.9 --locked

WORKDIR /app

# Copy the whole workspace. `.dockerignore` keeps target/, .git, media dirs, etc.
# out of the context. The build needs the full source tree, migrations/, assets,
# config/brand.yaml (include_str!'d) and Cargo.lock.
COPY . .

# Optional: embed the git sha; server.rs reads option_env!("GIT_SHA") for /build-info.
ARG GIT_SHA=""
ENV GIT_SHA=${GIT_SHA}

# Build the fullstack release bundle. Mirrors the dev command
#   dx serve --package meridian-web --web --fullstack true --port 3100
# Output lands at target/dx/meridian-web/release/web/{server, public/}.
# target/ and the cargo registry are cache mounts (not persisted into the layer),
# so the artifacts are copied out to /out within the same RUN.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    dx build --package meridian-web --web --fullstack --release \
 && mkdir -p /out \
 && cp -r target/dx/meridian-web/release/web/. /out/

# ---------------------------------------------------------------------------
# Stage 2 — runtime: slim Debian, non-root, serves the bundled server + assets.
# ---------------------------------------------------------------------------
# bookworm-slim matches the builder's glibc (both Debian 12), so the dynamically
# linked server binary runs unchanged.
FROM debian:bookworm-slim AS runtime

# ca-certificates: outbound HTTPS (ninjs ingest, webhook delivery).
# curl: container HEALTHCHECK.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --create-home --home-dir /app --shell /usr/sbin/nologin app

WORKDIR /app

# The dx bundle: `server` (executable) with `public/` as a SIBLING directory.
# dioxus-server resolves assets at <exe dir>/public, so this layout matters.
COPY --from=builder --chown=app:app /out/ /app/

USER app

# The bundled server reads IP + PORT (dioxus_cli_config); default is 127.0.0.1:8080.
# Bind to 0.0.0.0 so the port is reachable from outside the container.
ENV IP=0.0.0.0 \
    PORT=3100 \
    RUST_LOG=meridian=info,tower_http=info

EXPOSE 3100

# Liveness only (always 200 while the process is up). Deep readiness — which also
# checks Postgres + object storage — is at /health/ready; see deploy/README.md.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=5 \
  CMD curl -fsS "http://127.0.0.1:${PORT:-3100}/health/live" || exit 1

# Path is absolute: `public/` is resolved relative to the executable's directory.
ENTRYPOINT ["/app/server"]
