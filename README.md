# iGEN News — Linux/Docker deployment

Everything needed to deploy **iGEN News** (India Global Expo News) on a Linux server
with Docker. The container image builds the Linux binary from source, so no
platform-specific binary is shipped — you deploy anywhere Docker runs.

## Stack

`docker-compose.yml` brings up the whole system:

- **app** — the Dioxus 0.7 fullstack server (built by `Dockerfile`), on `:3100`
- **postgres** — PostgreSQL 18
- **pgbouncer** — connection pooler (request traffic goes through it)
- **rustfs** — S3-compatible object storage (media)
- **migrate** — one-shot that applies `migrations/` (built by `Dockerfile.migrate`)
- **createbuckets** — one-shot that provisions the media bucket

## Quickstart

```bash
# 1. Configure — copy the template and fill in EVERY value
cp .env.docker.example .env
#    generate secrets:  openssl rand -hex 32   (passwords)
#                       openssl rand -hex 64   (MERIDIAN_AUTH_SECRET, >= 64 chars)

# 2. Build + start (the app image compiles the release binary — first build is slow)
docker compose up -d --build

# 3. Health
curl http://localhost:3100/health/live     # liveness
curl http://localhost:3100/health/ready     # deep readiness (DB + object store)

# 4. Seed the demo newsroom (once): 50 sectors + 1,348 industries, 35-person team,
#    ~2 months of published articles, feed, tasks, pitches.
for f in seed-sectors.sql seed-igennews.sql; do
  docker compose cp "scripts/$f" postgres:/tmp/$f
  docker compose exec postgres \
    psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -f /tmp/$f
done
```

Sign in at `http://<host>:3100/sign-in`. Every seeded account uses **`DevPass123!`**;
the super-admin is **`admin@igennews.com`**. Full account/role list:
[`docs/IGENNEWS-ACCOUNTS.md`](docs/IGENNEWS-ACCOUNTS.md).

## Environment

`.env.docker.example` documents every required variable (database URLs, the
PgBouncer split, S3/RustFS credentials, `MERIDIAN_AUTH_SECRET`, ports). The pooled
`DATABASE_URL` (pgbouncer) and direct `DATABASE_DIRECT_URL`/`MIGRATION_DATABASE_URL`
(postgres) must differ — the template already wires this for the compose network.

## Operational notes

- **After adding a migration on a running stack, restart PgBouncer** or queries fail
  with `cached plan must not change result type`:
  ```bash
  docker compose build migrate && docker compose run --rm migrate && docker compose restart pgbouncer
  ```
- Terminate TLS at a reverse proxy in front of the app (it honours `X-Forwarded-For`).
- The app serves plain HTTP on `:3100`.

Full runbook: [`deploy/README.md`](deploy/README.md). Rebuild the bundle outside
Docker with `dx build --package meridian-web --web --fullstack --release`.

> **Change `DevPass123!` and every `.env` secret before any real deployment.** The
> seed password and placeholders are for demo/bring-up only.
