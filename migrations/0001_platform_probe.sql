CREATE TABLE IF NOT EXISTS rust_platform_migrations (
    id text PRIMARY KEY,
    applied_at timestamptz NOT NULL DEFAULT now(),
    build_version text NOT NULL
);

COMMENT ON TABLE rust_platform_migrations IS
    'Module 1 probe owned by the Rust target; no editorial data is written here.';

