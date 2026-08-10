-- Professional profile for each user.
--
-- A single jsonb document holds the portfolio a person (or an admin) curates:
-- headline, bio, location, visibility, social links, work experience, education,
-- and selected editorials. Kept as jsonb (not normalised tables) because it is
-- read and written whole, is edited as one form, and its shape is UI-owned.
--
-- `visibility` inside the document gates who sees the content ("public" by
-- default, "private" restricts it to the owner and admins); the column itself is
-- always present so reads never branch on NULL.

SET LOCAL search_path = meridian, public;

ALTER TABLE users
    ADD COLUMN IF NOT EXISTS profile jsonb NOT NULL DEFAULT '{}'::jsonb;

COMMENT ON COLUMN users.profile IS
  'Curated professional profile (headline, bio, social, experience, education, editorials, visibility). Edited by the account holder or an admin.';
