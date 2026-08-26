-- The Recipe index. It is a cache and nothing else.
--
-- Forgejo holds the repositories and Git holds the Recipe content. The one
-- fact that neither of them can answer quickly is the title that a person
-- sees, because that title lives inside `recipe.cook` and not in the
-- repository name. This table keeps that title, and the few culinary facts
-- that a card shows, so that a list needs one read of Forgejo instead of one
-- read for every Recipe.
--
-- Every row is rebuildable. Deleting this table costs time and nothing more:
-- the reconciliation reads Forgejo and Git again and writes every row back.
--
-- This table never decides who may see a Recipe. Forgejo decides that on
-- every request, and the index only supplies the words on the card.
CREATE TABLE recipe_index (
    -- The Forgejo identifier of the repository. A rename changes the owner
    -- and the slug, so the identifier is what stays.
    repository_id INTEGER PRIMARY KEY,
    owner         TEXT    NOT NULL,
    slug          TEXT    NOT NULL,
    -- The title from the Cooklang metadata. A Recipe with no title falls
    -- back to its slug, so this column is never empty.
    title         TEXT    NOT NULL,
    -- What Forgejo reported when the application last looked. It shapes the
    -- lists. It never grants access.
    private       INTEGER NOT NULL,
    -- What Forgejo reported as the moment of the last change, exactly as
    -- Forgejo wrote it. The application compares it with the value that
    -- Forgejo reports now, and reads the Recipe again only when it differs.
    updated_at    TEXT    NOT NULL,
    -- Culinary facts for the card. All of them come from `recipe.cook`.
    servings      TEXT,
    tags          TEXT    NOT NULL DEFAULT '',
    ingredients   INTEGER NOT NULL DEFAULT 0,
    -- Whether the Recipe has a photo, so a card can show it without asking
    -- Forgejo for every Recipe on the page. The photo itself is never here.
    thumbnail     INTEGER NOT NULL DEFAULT 0,
    indexed_at    INTEGER NOT NULL
);

CREATE UNIQUE INDEX recipe_index_name  ON recipe_index (owner, slug);
CREATE INDEX        recipe_index_title ON recipe_index (title COLLATE NOCASE);

-- The system webhook that Forgejo posts to.
--
-- One row, because one installation talks to one Forgejo. The secret is
-- encrypted with the installation key, the same way the OAuth client secret
-- is. Losing this row costs a run of the bootstrap command.
CREATE TABLE webhook (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    forgejo_hook_id INTEGER NOT NULL,
    target_url      TEXT    NOT NULL,
    -- Encrypted. Forgejo signs each body with it.
    secret          BLOB    NOT NULL,
    updated_at      INTEGER NOT NULL
);
