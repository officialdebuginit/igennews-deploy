SHELL := /bin/bash

LEGACY_ROOT := ../news-hub-studio
COMPOSE := docker compose -f $(LEGACY_ROOT)/infra/docker-compose.yml --env-file $(LEGACY_ROOT)/infra/.env

.PHONY: help infra-up infra-status provision-local check lint test test-live test-roles test-restore snapshot-api check-api-drift check-beyond-legacy flow-test ui-flow-test parity-auth parity parity-mutations parity-all migrate repair-ledger check-schema-drift server

help:
	@echo "Meridian Rust Module 1"
	@echo "  make infra-up      reuse the existing Postgres/PgBouncer/RustFS stack"
	@echo "  make infra-status  show shared infrastructure health"
	@echo "  make provision-local provision roles, migrate, and verify local privileges"
	@echo "  make check         check server and WASM targets"
	@echo "  make lint          run Clippy with warnings denied"
	@echo "  make test          run workspace tests"
	@echo "  make test-live     run live infrastructure probes with cleanup"
	@echo "  make test-roles    verify role SQL transactionally, then roll it back"
	@echo "  make test-restore  rehearse schema restore and forward migration safely"
	@echo "  make snapshot-api  update the reviewed legacy OpenAPI baseline"
	@echo "  make check-api-drift compare the live legacy API to the baseline"
	@echo "  make check-beyond-legacy  inventory the /api/v1 surface the oracle lacks"
	@echo "  make flow-test     drive every API flow against a running server"
	@echo "  make ui-flow-test  drive the real WASM UI in a browser (needs dx serve)"
	@echo "  make parity-auth   diff legacy vs Rust auth/error-body parity (no data needed)"
	@echo "  make parity        diff legacy vs Rust responses from parity-fixtures.json"
	@echo "  make parity-mutations  diff create/update/delete lifecycle chains"
	@echo "  make parity-all    run the whole parity suite (100% of the ledger)"
	@echo "  make migrate       run SQLx migrations with MIGRATION_DATABASE_URL"
	@echo "  make server        start Dioxus fullstack on :3100"

infra-up:
	$(COMPOSE) up -d

infra-status:
	$(COMPOSE) ps

provision-local:
	./scripts/provision-local-database.sh

check:
	cargo check -p meridian-web --features server
	cargo check -p meridian-web --features web --target wasm32-unknown-unknown

lint:
	cargo +stable clippy --workspace --all-targets --features server -- -D warnings

test:
	cargo test --workspace

test-live:
	@test -f .env.local || (echo "run make provision-local first"; exit 1)
	@set -a; source .env.local; set +a; \
	unset MIGRATION_DATABASE_URL MERIDIAN_OWNER_PASSWORD MERIDIAN_MIGRATOR_PASSWORD \
	MERIDIAN_WORKER_PASSWORD MERIDIAN_READONLY_PASSWORD; \
	cargo test -p meridian-platform --test live_infrastructure -- --ignored
	@set -a; source .env.local; set +a; \
	unset MIGRATION_DATABASE_URL MERIDIAN_OWNER_PASSWORD MERIDIAN_MIGRATOR_PASSWORD \
	MERIDIAN_WORKER_PASSWORD MERIDIAN_READONLY_PASSWORD; \
	cargo test -p meridian-identity --test live_session_rotation -- --ignored

test-roles:
	@set -a; source $(LEGACY_ROOT)/infra/.env; set +a; \
	psql "$$POSTGRES_URL" -X \
	-v owner_password=contract-owner -v migrator_password=contract-migrator \
	-v app_password=contract-app -v worker_password=contract-worker \
	-v readonly_password=contract-readonly \
	-c BEGIN -f deploy/postgres/roles.sql -f deploy/postgres/verify-roles.sql -c ROLLBACK

test-restore:
	./scripts/rehearse-migration-restore.sh

snapshot-api:
	./scripts/legacy-api-contract.sh --update

check-api-drift:
	./scripts/legacy-api-contract.sh --check

# The parity harness drives from the legacy ledger, so an operation the oracle
# does not have is never probed. This is the gate for that surface. Source-only:
# no database, no ports, so it belongs beside clippy in CI.
check-beyond-legacy:
	./scripts/check-beyond-legacy.sh

# End-to-end flow test against a running server (PORT=3199 by default). Reports a
# check that cannot run as BLOCKED, never as a pass — a suite that goes green
# while skipping half the product is worse than one that fails.
flow-test:
	python3 ./scripts/flow-test.py

# Browser-level UI test: drives the real WASM app with Playwright, as a user
# would — click, type, navigate. Needs `dx serve` on :3100 (the plain server
# binary does not serve the WASM bundle) and Playwright with a Chrome channel:
#   cd <scratch> && npm i playwright && node scripts/ui-flow-test.js
# Distinct from flow-test, which speaks HTTP to the API and cannot see hydration,
# routing, event handlers or rendering.
ui-flow-test:
	node ./scripts/ui-flow-test.js

# Data-free: needs both servers up (legacy :8000, Rust :3100), no seed data.
parity-auth:
	./scripts/differential-parity.sh --auth

# Data-driven: needs both servers up and a login that exists on each — set
# LEGACY_USER/LEGACY_PASS + RUST_USER/RUST_PASS (or the shared PARITY_USER/PASS).
parity:
	./scripts/differential-parity.sh --fixtures

# Mutating-surface value parity: create/update/delete lifecycle chains
# (contracts/parity-lifecycles.json), each self-cleaning. Same credentials as
# `parity`. Exits non-zero on any unexpected divergence.
parity-mutations:
	python3 ./scripts/parity-mutations.py

# The whole differential-parity suite: auth-ordering + read fixtures + mutating
# lifecycle chains. Together these cover 100% of the 161-operation legacy ledger.
#
# BOTH servers need MERIDIAN_EXPOSE_RESET_TOKENS=true, not just the oracle — the
# password-reset chain captures the token from each server's own response, so if
# only one exposes it the capture is empty and confirm diverges 204|400. Start the
# Rust side with:
#   PORT=3100 IP=127.0.0.1 MERIDIAN_EXPOSE_RESET_TOKENS=true ./target/debug/meridian-web
# (built with --features server; a plain `cargo test --workspace` overwrites that
# binary with a featureless one that panics on launch).
#
# For full coverage the legacy oracle must run with dev config so uploads and
# reset-tokens work; the exact relaunch is:
#   MERIDIAN_DATABASE_URL="sqlite:///$$(pwd)/../news-hub-studio/backend/data/oracle.db" \
#   MERIDIAN_EXPOSE_RESET_TOKENS=true MERIDIAN_ENVIRONMENT=development \
#   MERIDIAN_S3_ACCESS_KEY_ID=debuginit MERIDIAN_S3_SECRET_ACCESS_KEY=debuginit-secret-key \
#   MERIDIAN_S3_BUCKET=meridian-media \
#   ../news-hub-studio/backend/.venv/bin/uvicorn app.main:app \
#     --app-dir ../news-hub-studio/backend --port 8000
parity-all: parity-auth parity parity-mutations
	@echo "differential-parity suite complete."

migrate:
	@test -n "$$MIGRATION_DATABASE_URL" || (echo "MIGRATION_DATABASE_URL is required"; exit 1)
	cargo run -p meridian-platform --bin meridian-migrate

# Fails when the meridian schema holds objects no migration created — the
# signature of the legacy oracle having been started against the shared
# PostgreSQL. Reports only; prints the SQL for a human to run.
check-schema-drift:
	@test -n "$$MIGRATION_DATABASE_URL" || (echo "MIGRATION_DATABASE_URL is required"; exit 1)
	./scripts/check-schema-drift.sh

# Reconciles the SQLx ledger with a schema that is already in place (restored
# dump, hand-applied migration, truncated ledger). Reports by default; only
# records migrations whose objects it can verify. Never runs a migration.
repair-ledger:
	@test -n "$$MIGRATION_DATABASE_URL" || (echo "MIGRATION_DATABASE_URL is required"; exit 1)
	./scripts/repair-migration-ledger.sh $(ARGS)

server:
	@test -f .env.local || (echo "run make provision-local first"; exit 1)
	@set -a; source .env.local; set +a; \
	unset MIGRATION_DATABASE_URL MERIDIAN_OWNER_PASSWORD MERIDIAN_MIGRATOR_PASSWORD \
	MERIDIAN_WORKER_PASSWORD MERIDIAN_READONLY_PASSWORD; \
	dx serve --package meridian-web --web --fullstack true --port 3100
