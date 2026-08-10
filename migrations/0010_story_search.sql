-- Module 7 (search) — full-text search over stories, in PostgreSQL itself (no
-- external engine, per the architecture). A generated tsvector keeps the index
-- in lockstep with the row, and a GIN index makes the match fast.
--
-- The searchable surface is the text metadata (title, dek, category); the jsonb
-- block body is not decomposed here — extracting plain text from the block model
-- is its own step and is deferred.

ALTER TABLE stories ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        to_tsvector('english',
            coalesce(title, '') || ' ' || coalesce(dek, '') || ' ' || coalesce(category, ''))
    ) STORED;

CREATE INDEX stories_search_idx ON stories USING GIN (search_tsv);

COMMENT ON COLUMN stories.search_tsv IS 'Generated full-text index over title, dek, and category; queried with plainto_tsquery.';
