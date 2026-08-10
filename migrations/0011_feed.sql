-- Module 7 (feed) — the newsroom feed: posts, plus per-user likes and reposts
-- (toggle join tables) and threaded replies. Additive.
--
-- likes/reposts/replies on feed_posts are denormalised counters maintained
-- alongside their rows, as the legacy model documents: the feed lists many posts
-- per request and each card shows all three counts.

CREATE TABLE feed_posts (
    id uuid PRIMARY KEY,
    author_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind text NOT NULL DEFAULT 'note',
    content text NOT NULL,
    story_id uuid REFERENCES stories(id) ON DELETE SET NULL,
    asset_id uuid REFERENCES assets(id) ON DELETE SET NULL,
    likes integer NOT NULL DEFAULT 0,
    reposts integer NOT NULL DEFAULT 0,
    replies integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX feed_posts_author_idx ON feed_posts (author_id);
CREATE INDEX feed_posts_kind_idx ON feed_posts (kind);
CREATE INDEX feed_posts_created_idx ON feed_posts (created_at DESC);

CREATE TABLE feed_likes (
    post_id uuid NOT NULL REFERENCES feed_posts(id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (post_id, user_id)
);

CREATE TABLE feed_reposts (
    post_id uuid NOT NULL REFERENCES feed_posts(id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (post_id, user_id)
);

CREATE TABLE feed_replies (
    id uuid PRIMARY KEY,
    post_id uuid NOT NULL REFERENCES feed_posts(id) ON DELETE CASCADE,
    author_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    body text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX feed_replies_post_idx ON feed_replies (post_id);

COMMENT ON TABLE feed_posts IS 'Newsroom feed posts; likes/reposts/replies are denormalised counters maintained with their join/child rows.';
COMMENT ON TABLE feed_likes IS 'Per-user like toggle on a feed post.';
COMMENT ON TABLE feed_reposts IS 'Per-user repost toggle on a feed post (not a new post).';
COMMENT ON TABLE feed_replies IS 'Threaded replies to a feed post; distinct from story comments.';
