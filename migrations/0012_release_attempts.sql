-- Module 9 (publishing) — a recorded attempt by the scheduled-publish pass to
-- send a release out. Additive. Keeps the last error, attempt count, and trigger
-- so a stuck schedule is diagnosable from the row rather than only from logs.

CREATE TABLE release_attempts (
    id uuid PRIMARY KEY,
    release_id uuid NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    attempt_number integer NOT NULL DEFAULT 1,
    status text NOT NULL CHECK (status IN ('succeeded', 'failed')),
    trigger text NOT NULL CHECK (trigger IN ('worker', 'queue', 'endpoint')),
    started_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz NOT NULL DEFAULT now(),
    error_code text,
    error_message text
);

CREATE INDEX release_attempts_release_idx ON release_attempts (release_id);
CREATE INDEX release_attempts_status_idx ON release_attempts (status);

COMMENT ON TABLE release_attempts IS 'One recorded publish attempt per release per pass; trigger distinguishes worker/queue/endpoint runs.';
