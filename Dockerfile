# Runtime image for iGEN News — runs the PRE-BUILT Linux fullstack server binary.
# No source, no compile: it just packages bin/server + bin/public into a slim image.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --create-home --home-dir /app --shell /usr/sbin/nologin app
WORKDIR /app
# The bundle: `server` (executable) with `public/` as a sibling dir (assets resolve
# at <exe dir>/public). Both are pre-built for linux/arm64.
COPY --chown=app:app bin/server /app/server
COPY --chown=app:app bin/public /app/public
USER app
ENV IP=0.0.0.0 PORT=3100 RUST_LOG=meridian=info,tower_http=info
EXPOSE 3100
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=5 \
  CMD curl -fsS "http://127.0.0.1:${PORT:-3100}/health/live" || exit 1
ENTRYPOINT ["/app/server"]
