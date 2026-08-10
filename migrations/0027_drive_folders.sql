-- Drive folders: a simple hierarchy for organising media assets.
--
-- Folders are lightweight containers (name + optional parent) so the asset bank
-- reads like a file drive. Each asset may live in at most one folder; deleting a
-- folder re-files its assets to the root (folder_id → NULL) rather than deleting
-- them, and cascades to its subfolders. Ids are app-provided (uuid v7), matching
-- every other table.

SET LOCAL search_path = meridian, public;

CREATE TABLE drive_folders (
    id         uuid PRIMARY KEY,
    name       text NOT NULL,
    parent_id  uuid REFERENCES drive_folders(id) ON DELETE CASCADE,
    created_by uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE assets
    ADD COLUMN folder_id uuid REFERENCES drive_folders(id) ON DELETE SET NULL;

CREATE INDEX idx_assets_folder ON assets (folder_id);
CREATE INDEX idx_drive_folders_parent ON drive_folders (parent_id);

COMMENT ON TABLE drive_folders IS
  'Lightweight folders for organising media assets in the Drive; assets.folder_id points here.';
