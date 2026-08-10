-- Module 8 (metrics) — one metric value, one scope, one hourly bucket, observed
-- once. Additive. First-write-wins per (metric, scope, bucket) so a gauge
-- sampled repeatedly in an hour keeps its start-of-hour reading; the unique
-- constraint is the enforcement. scope_id is '' for the newsroom-wide series (a
-- sentinel, not NULL, so the unique index actually covers it).

CREATE TABLE metric_snapshots (
    id uuid PRIMARY KEY,
    metric_key text NOT NULL,
    scope_type text NOT NULL CHECK (scope_type IN ('global', 'desk')),
    scope_id text NOT NULL DEFAULT '',
    bucket_start timestamptz NOT NULL,
    bucket_end timestamptz NOT NULL,
    value bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT uq_metric_snapshot_bucket UNIQUE (metric_key, scope_type, scope_id, bucket_start)
);

CREATE INDEX metric_snapshots_key_idx ON metric_snapshots (metric_key);
CREATE INDEX metric_snapshots_series_idx ON metric_snapshots (metric_key, scope_type, scope_id, bucket_start);

COMMENT ON TABLE metric_snapshots IS 'Point-in-time metric series (global/desk, hourly buckets); first observation per bucket wins.';
