-- N-level sector taxonomy: sub-sectors become a tree within a sector.
--
-- Extends the two-level model (migration 0041) so an industry can nest to arbitrary
-- depth — Sector › Industry › Sub-industry › … — which a writer can build and file a
-- story against before publishing. The tree is scoped to one sector (desk): a node's
-- parent is another sub_sector in the SAME desk (enforced in the service layer).
--
-- A story still points at a single node via stories.sub_sector_id (any level, leaf or
-- internal); its full taxonomy path (root→node, under the sector) drives breadcrumbs
-- and SEO structured data. Slugs stay unique per sector, so paths are unambiguous.

SET LOCAL search_path = meridian, public;

ALTER TABLE sub_sectors
    ADD COLUMN parent_id uuid REFERENCES sub_sectors(id) ON DELETE CASCADE;

-- Deleting a node removes its whole subtree (CASCADE); stories under any removed node
-- keep their desk and have sub_sector_id nulled by the stories FK's ON DELETE SET NULL.
CREATE INDEX sub_sectors_parent_idx ON sub_sectors (parent_id);
