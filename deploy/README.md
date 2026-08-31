# Meridian — production deployment runbook

Containerized deployment of the `meridian-web` fullstack app (Dioxus 0.7 WASM
client + Axum server) with its runtime dependencies: PostgreSQL, PgBouncer, and
RustFS (S3-compatible object storage).

All files referenced here live at the repo root unless noted:

| File | Purpose |
| --- | --- |
| `Dockerfile` | Multi-stage build of the fullstack release bundle → slim non-root runtime |
| `Dockerfile.migrate` | One-shot image that applies embedded SQLx migrations |
| `docker-compose.yml` | Full stack: postgres, pgbouncer, rustfs, createbuckets, migrate, app |
| `.env.docker.example` | Environment template (copy to `.env`, fill in secrets) |
| `.dockerignore` | Keeps the build context small |
| `deploy/postgres/docker-init/01-provision.sh` | First-init hook: creates roles/schema/extensions |

---

## 1. Prerequisites

- Docker with BuildKit (default on modern Docker). The `Dockerfile`s use
  `--mount=type=cache`, which requires BuildKit — export `DOCKER_BUILDKIT=1` if
  you are on an old Docker.
- Outbound network access during the build: `dx` downloads a matching
  `wasm-bindgen` + `wasm-opt`, and Cargo fetches crates.
- ~a few GB of disk and RAM for the Rust + WASM release compile.

## 2. Configure secrets

```sh
cp .env.docker.example .env
# Replace every placeholder. Generate values with:
#   openssl rand -hex 32   # passwords
#   openssl rand -hex 64   # MERIDIAN_AUTH_SECRET  (must be >= 64 chars)
```

`.env` is git-ignored. Never commit it.

## 3. Build the images

```sh
# App (fullstack) image. Optionally stamp the git sha into /build-info:
DOCKER_BUILDKIT=1 docker build \
  --build-arg GIT_SHA="$(git rev-parse --short HEAD)" \
  -t meridian-web:latest .

# Migration image:
DOCKER_BUILDKIT=1 docker build -f Dockerfile.migrate -t meridian-migrate:latest .
```

The exact production build command that runs inside `Dockerfile` is:

```sh
dx build --package meridian-web --web --fullstack --release
# → target/dx/meridian-web/release/web/{server, public/}
```

(`docker compose up --build` builds both images for you; the standalone commands
above are for CI or manual builds.)

## 4. First run

```sh
docker compose up -d --build
```

Ordering is handled by health checks and `depends_on`:

1. **postgres** starts and, on the first init of an empty data volume, runs
   `01-provision.sh` → creates the `meridian_owner/migrator/app/worker/readonly`
   roles, the `meridian` schema and grants (`deploy/postgres/roles.sql`), and the
   `pg_stat_statements` extension (`deploy/postgres/required-extensions.sql`).
2. **pgbouncer** and **rustfs** come up.
3. **createbuckets** creates the S3 buckets in RustFS.
4. **migrate** applies all 42 SQLx migrations as `meridian_migrator`, then exits 0.
5. **app** starts only after migrate + createbuckets complete successfully.

Watch progress:

```sh
docker compose logs -f migrate      # should end and exit 0
docker compose ps                   # app should become healthy
```

> The provisioning in step 1 runs **once**, when the `meridian-postgres-data`
> volume is empty. To re-provision from scratch: `docker compose down -v`
> (this destroys all data), then `up` again.

## 5. Health checks

- Liveness: `curl -fsS http://localhost:${APP_PORT:-3100}/health/live` → `{"status":"ok"}`
- Deep readiness (also probes Postgres pooled + direct and object storage):
  `curl -fsS http://localhost:${APP_PORT:-3100}/health/ready` → `status: "ready"`
  (HTTP 503 until every dependency is reachable)
- Build info: `curl http://localhost:${APP_PORT:-3100}/build-info`
- Prometheus metrics: `GET /metrics`

## 6. Applying migrations later (IMPORTANT: restart PgBouncer)

Migrations are embedded into `meridian-migrate` **at compile time**
(`sqlx::migrate!`). When you add a migration you must rebuild the image first,
or the new migration is silently skipped:

```sh
docker compose build migrate
docker compose run --rm migrate
```

**Then restart PgBouncer.** This stack pools in transaction mode with server-side
statement caching. After any `ALTER TABLE` migration, connections still holding a
cached plan fail with:

```
cached plan must not change result type
```

Clear them by restarting the pooler:

```sh
docker compose restart pgbouncer
```

(First-run migrations are safe without this — no plans are cached before the app
serves its first query — but always restart the pooler after migrating a live
stack.)

## 7. Seed the demo (iGEN News)

A fresh database has no users, so no one can sign in. The **full iGEN News demo
seed** — 50 India Global Expo sectors + 1,348 industries, a 35-person team with
complete profiles and roles, and ~2 months of articles/tasks/pitches/coverage — is
one command. It requires the schema to be migrated first (§4/§6).

Locally (or any host with `psql` + repo checkout):

```sh
export DATABASE_DIRECT_URL="postgres://USER:PASS@HOST:5432/DB"   # a direct (non-pooled) URL
scripts/seed-all.sh
# ...then optionally fill article content across ALL 50 sectors (needs the app up):
scripts/seed-all.sh --with-content --base-url https://your-host --per-sector 5
```

Inside the compose stack (run the SQL seeds through the postgres container):

```sh
for f in seed-sectors.sql seed-roles.sql seed-igennews.sql; do
  docker compose cp "scripts/$f" postgres:/tmp/$f
  docker compose exec postgres \
    psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -f /tmp/$f
done
```

After seeding, sign in as **`admin@igennews.com` / `DevPass123!`** (super-admin).
Every account and its role/capabilities are listed in `docs/IGENNEWS-ACCOUNTS.md`.

> **Seed with the owner/migrator role, not the application role.**
> `seed-igennews.sql` begins with `TRUNCATE`, which requires table ownership. In a
> deployment that separates roles the application role (`DATABASE_DIRECT_URL`)
> cannot truncate, and seeding with it fails on the first statement. Point
> `SEED_DATABASE_URL` at the owner/migrator role; `scripts/seed-all.sh` checks this
> before it writes anything and tells you which role to use.
>
> Re-running is safe: every step clears what it writes first, so applying the seed
> repeatedly leaves identical data.

> `seed-igennews.sql` is destructive to *people and editorial content*
> (TRUNCATE/DELETE) but never touches the sectors or industries. Re-running it is a
> clean reset. Verify against your data before running in production.

## 8. Teardown

```sh
docker compose down          # stop, keep data volumes
docker compose down -v       # stop and DELETE all data (postgres + rustfs)
```

---

## Assumptions & things to confirm

- **Base image tags**: `rust:1.95-bookworm` (matches `rust-toolchain.toml`'s
  1.95.0), `debian:bookworm-slim`, `postgres:18.4-bookworm`,
  `edoburu/pgbouncer:v1.25.2-p0`, `rustfs/rustfs:1.0.0-beta.9` (pre-1.0; treat as
  dev-grade), `amazon/aws-cli:2.17.20`. Bump/verify tags for your registry.
- **TLS / reverse proxy**: the app serves plain HTTP on `:3100`. Terminate TLS at
  a reverse proxy (nginx/Caddy/Traefik) in front. It already honours
  `X-Forwarded-For` / `X-Real-IP` for client IPs and sets HSTS-friendly security
  headers, but does **not** terminate TLS itself.
- **RustFS is pre-1.0**. For a hardened production object store, consider a
  managed S3 or MinIO; point `S3_ENDPOINT_URL` / `MEDIA_S3_ENDPOINT` and the
  credentials at it and drop the `rustfs`/`createbuckets` services.
- **Bucket creation** uses `aws s3 mb` with path-style addressing (the app's
  aws-sdk-s3 client uses `force_path_style(true)`); confirm your object store
  accepts it.
- **Single-node compose**. For multi-node, externalize Postgres/PgBouncer/object
  storage and run only the `app` (and the `migrate` job) as scalable services.
