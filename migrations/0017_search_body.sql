-- Search over the story body (gap D3).
--
-- `0010` indexed title, dek and category only, deferring the jsonb block body
-- because extracting plain text from the block model is its own step. That step
-- is here: a story whose search term appears only in its copy was previously
-- unfindable, which is the common case for a reporter looking for their own work.
--
-- Extraction uses `jsonb_path_query_array(body, '$[*].text')`, which pulls just
-- the `text` of each block. Casting that array to text leaves JSON punctuation in
-- the string, which the tsvector tokeniser discards — and it avoids indexing the
-- block *keys* ("type", "heading", "paragraph") as content, which a naive
-- `body::text` would. Both functions are IMMUTABLE, as a generated column
-- requires; a set-returning `jsonb_array_elements` would not be allowed here.
--
-- The fields are also now weighted, so `ts_rank` means something: a term in the
-- headline should outrank the same term buried in the copy. Previously every
-- field ranked identically, which made ranking cosmetic.
--
--   A = title   B = dek   C = category   D = body
--
-- Dropping the column drops the GIN index with it, so the index is recreated.
-- Re-adding a STORED generated column recomputes it for every existing row, so
-- no backfill or reindex is needed.

SET LOCAL search_path = meridian, public;

DROP INDEX IF EXISTS stories_search_idx;

ALTER TABLE stories DROP COLUMN IF EXISTS search_tsv;

ALTER TABLE stories ADD COLUMN search_tsv tsvector
    GENERATED ALWAYS AS (
        setweight(to_tsvector('english', coalesce(title, '')), 'A')
        || setweight(to_tsvector('english', coalesce(dek, '')), 'B')
        || setweight(to_tsvector('english', coalesce(category, '')), 'C')
        || setweight(
            to_tsvector(
                'english',
                coalesce(jsonb_path_query_array(body, '$[*].text')::text, '')
            ),
            'D'
        )
    ) STORED;

CREATE INDEX stories_search_idx ON stories USING GIN (search_tsv);

COMMENT ON COLUMN stories.search_tsv IS
  'Generated, weighted full-text index over title (A), dek (B), category (C) and the block body text (D); queried with plainto_tsquery and ranked with ts_rank.';
