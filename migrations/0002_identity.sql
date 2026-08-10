CREATE TABLE users (
    id uuid PRIMARY KEY,
    email text NOT NULL,
    handle text NOT NULL,
    display_name text NOT NULL,
    password_hash text NOT NULL,
    role text NOT NULL CHECK (role IN (
        'admin', 'editor_in_chief', 'managing_editor', 'section_editor',
        'reporter', 'copy_editor', 'fact_checker', 'producer',
        'standards_legal', 'audience_editor', 'contributor', 'viewer'
    )),
    department text,
    is_active boolean NOT NULL DEFAULT true,
    is_admin boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT users_email_length CHECK (length(email) BETWEEN 3 AND 320),
    CONSTRAINT users_handle_length CHECK (length(handle) BETWEEN 1 AND 80),
    CONSTRAINT users_display_name_length CHECK (length(display_name) BETWEEN 1 AND 160)
);

CREATE UNIQUE INDEX users_email_case_insensitive_uq ON users (lower(email));
CREATE UNIQUE INDEX users_handle_case_insensitive_uq ON users (lower(handle));
CREATE INDEX users_role_idx ON users (role);

CREATE TABLE sessions (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    family_id uuid NOT NULL,
    refresh_token_hash char(64) NOT NULL UNIQUE,
    user_agent varchar(400),
    ip_address inet,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    revoked_reason varchar(120),
    CONSTRAINT sessions_revocation_consistent CHECK (
        (revoked_at IS NULL AND revoked_reason IS NULL) OR revoked_at IS NOT NULL
    )
);

CREATE INDEX sessions_user_live_idx ON sessions (user_id, last_seen_at DESC)
    WHERE revoked_at IS NULL;
CREATE INDEX sessions_family_live_idx ON sessions (family_id)
    WHERE revoked_at IS NULL;
CREATE INDEX sessions_expiry_idx ON sessions (expires_at)
    WHERE revoked_at IS NULL;

CREATE TABLE password_reset_tokens (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash char(64) NOT NULL UNIQUE,
    expires_at timestamptz NOT NULL,
    used_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX password_reset_tokens_user_idx ON password_reset_tokens (user_id);
CREATE INDEX password_reset_tokens_unused_expiry_idx ON password_reset_tokens (expires_at)
    WHERE used_at IS NULL;

COMMENT ON TABLE users IS 'Module 2 identity records migrated from the legacy users contract; never seeded with demo data.';
COMMENT ON TABLE sessions IS 'Rotating refresh-token sessions. Only SHA-256 token hashes are persisted.';
COMMENT ON TABLE password_reset_tokens IS 'Single-use password reset token hashes; plaintext tokens are never persisted.';
