-- The Cookbook index. It is a cache and nothing else.
--
-- Forgejo holds the repositories and Git holds the Cookbook content. The one
-- fact that neither of them can answer quickly is the title that a person
-- sees, because that title is the first heading inside `README.md` and not
-- the repository name. This table keeps that title and the first words of
-- the description, so that a list needs one read of Forgejo instead of one
-- read for every Cookbook.
--
-- Every row is rebuildable. Deleting this table costs time and nothing more:
-- the application reads Forgejo and Git again and writes every row back.
--
-- This table never decides who may see a Cookbook. Forgejo decides that on
-- every request, and the index only supplies the words on the card.
CREATE TABLE cookbook_index (
    -- The Forgejo identifier of the repository. A rename changes the owner
    -- and the slug, so the identifier is what stays.
    repository_id INTEGER PRIMARY KEY,
    owner         TEXT    NOT NULL,
    slug          TEXT    NOT NULL,
    -- The first heading of `README.md`. A Cookbook with no heading falls
    -- back to its slug, so this column is never empty.
    title         TEXT    NOT NULL,
    -- What Forgejo reported when the application last looked. It shapes the
    -- lists. It never grants access.
    private       INTEGER NOT NULL,
    -- What Forgejo reported as the moment of the last change, exactly as
    -- Forgejo wrote it. The application compares it with the value that
    -- Forgejo reports now, and reads the Cookbook again only when it differs.
    updated_at    TEXT    NOT NULL,
    -- The first words of the description, as plain text, for the card. The
    -- description itself stays in `README.md`.
    summary       TEXT    NOT NULL DEFAULT '',
    indexed_at    INTEGER NOT NULL
);

CREATE UNIQUE INDEX cookbook_index_name  ON cookbook_index (owner, slug);
CREATE INDEX        cookbook_index_title ON cookbook_index (title COLLATE NOCASE);
