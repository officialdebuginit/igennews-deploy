-- Per-sector SMTP: each desk can carry its own outbound mail server, so a sector
-- sends from and manages its own email. One row per desk; managed by a desk lead
-- (desks.manage). The password is write-only from the API's perspective (never
-- serialized back) — encrypt-at-rest is a noted follow-up.
CREATE TABLE meridian.desk_smtp_settings (
    desk_id      uuid PRIMARY KEY REFERENCES meridian.desks(id) ON DELETE CASCADE,
    host         text NOT NULL,
    port         integer NOT NULL DEFAULT 587,
    username     text,
    password     text,
    from_address text NOT NULL,
    from_name    text,
    use_starttls boolean NOT NULL DEFAULT true,
    active       boolean NOT NULL DEFAULT true,
    updated_by_id uuid REFERENCES meridian.users(id) ON DELETE SET NULL,
    updated_at   timestamptz NOT NULL DEFAULT now()
);
