# iGEN News — deployment (pre-built binary)

Runtime deployment package for **iGEN News** (India Global Expo News). It ships the
**pre-built Linux binary** and the Docker/compose setup to run it on a server — **no
source code, no compile step**. The container images just package the binary.

## Contents

| Path | What it is |
|---|---|
| `bin/server` | The fullstack app server — **pre-built for `linux/arm64`**. |
| `bin/public/` | The web assets it serves (sibling of `server`). |
| `bin/meridian-migrate` | The migration runner — pre-built for `linux/arm64`; embeds all migrations. |
| `Dockerfile` | Runtime image for the app (copies `bin/server` + `bin/public`). |
| `Dockerfile.migrate` | Runtime image for the migration one-shot. |
| `docker-compose.yml` | The full stack: app + Postgres + PgBouncer + RustFS + migrate + bucket init. |
| `.env.example` | Every required variable (fill in before deploy). |
| `scripts/` | Database seeds. `seed-all.sh` runs all three in order (`seed-sectors.sql`, `seed-roles.sql`, `seed-igennews.sql`); `SEED-GAPS.md` records coverage. |
| `deploy/README.md`, `ACCOUNTS.md` | Full runbook + the seeded accounts/roles. |

## Deploy

```bash
# 1. Configure
cp .env.example .env          # fill EVERY value — openssl rand -hex 32 / 64

# 2. Bring up the stack (images just package the binary — the build is instant)
docker compose up -d --build

# 3. Health
curl http://localhost:3100/health/live      # liveness
curl http://localhost:3100/health/ready      # DB + object store

# 4. Seed the demo newsroom (once)
for f in seed-sectors.sql seed-roles.sql seed-igennews.sql; do
  docker compose cp "scripts/$f" postgres:/tmp/$f
  docker compose exec postgres \
    psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -f /tmp/$f
done
```

> **Seed with the owner/migrator role, not the application role.**
> `seed-igennews.sql` begins with `TRUNCATE`, which requires table ownership. In a
> deployment that separates roles the application role (`DATABASE_DIRECT_URL`)
> cannot truncate, and seeding with it fails on the first statement. Point
> `SEED_DATABASE_URL` at the owner/migrator role; `scripts/seed-all.sh` checks this
> before it writes anything and tells you which role to use.
>
> Re-running is safe: every step clears what it writes first, so applying the seed
> repeatedly leaves identical data.

Sign in at `http://<host>:3100/sign-in`: **`admin@igennews.com` / `DevPass123!`**
(super-admin). All accounts + roles: [`ACCOUNTS.md`](ACCOUNTS.md). Full runbook:
[`deploy/README.md`](deploy/README.md).

## Notes

- The binaries are **`linux/arm64`** (built on Apple Silicon — good for arm64 Linux
  servers such as AWS Graviton, Oracle Ampere, Hetzner/Scaleway ARM). For **x86_64 /
  amd64** hosts, an amd64 build is needed — rebuild with `docker build --platform
  linux/amd64 …` from the private source repo (`igennews`), or ask for an amd64 bundle.
- After bringing up new migrations, restart PgBouncer (cached-plan caveat — see the
  runbook).
- Terminate TLS at a reverse proxy in front; the app serves plain HTTP on `:3100`.

> **Change `DevPass123!` and every `.env` secret before any real deployment.**
