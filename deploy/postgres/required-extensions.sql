-- Module 1 extension policy: explicit, reviewable, and idempotent.
-- Run as the migrator/owner, never as the web application role.
CREATE EXTENSION IF NOT EXISTS pg_stat_statements;

-- Add pg_trgm, citext, or pgcrypto only in the migration that introduces a
-- measured use for it. Never enumerate pg_available_extensions here.

