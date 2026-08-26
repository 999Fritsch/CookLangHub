# CookLangHub

A self-hosted platform where friends and family write, share, and cook
Recipes together. Forgejo and Git hold the authoritative state. The
application gives a cooking interface over them.

Full specification: `PRD.md`. Tickets: GitHub milestone `prototype`.

## Architecture rules

These are not preferences. Breaking one breaks the product thesis.

- Forgejo is authoritative for identity, permissions, visibility, forks,
  pull requests, issues, stars, and watches. Git is authoritative for
  Recipe content and History.
- Never build a second authoritative store for Recipe or Cookbook data.
  Local SQLite holds operational state only, and all of it is rebuildable.
- Reach Forgejo only through its HTTP API, OAuth, webhooks, and the Git
  protocols. Never open its database. Never touch its repository storage.
- Git work goes through the Git adapter, never through ad hoc commands.
- Show the user cooking words. Recipe, Cookbook, Version, Variation,
  Suggestion, Discussion. Never branch, commit, fork, or pull request.
- When the state is one the interface cannot handle, diagnose it and offer
  **Open in Forgejo**. Never repair it silently.

## Stack

Rust, Axum, Askama templates, SQLite, `cooklang-rs`, CodeMirror 6.
Server-rendered HTML. Node is a build-time dependency only.

```sh
cargo test      # unit plus integration; integration needs Docker
docker compose up --build
```

Integration tests run against a real disposable Forgejo container. That is
the primary acceptance seam. Do not replace it with a mock.

## Visual identity

**The design is the CookCLI design.** CookLangHub is a Cooklang product, and
it should look and feel like the rest of the Cooklang tools with a
collaborative backend behind it. A cook who knows `cook server` must
recognise this immediately.

CookCLI is MIT licensed, copyright Alexey Dubovskoy, and the licence permits
this reuse. See `NOTICE` and `LICENSE-MIT-cookcli`.

These files are CookCLI's, copied:

    tailwind.config.js
    static/css/input.css
    static/css/custom-styles.css
    static/css/cooking-mode.css
    static/css/theme.src.css     (their inline dark-mode rules)

Rules:

1. **Copy, do not invent.** When CookCLI already styles something, use their
   class and their markup shape. Read their templates in the CookCLI
   repository before writing a new component. Only design from scratch when
   CookCLI has no equivalent, and then build it out of their existing
   classes.
2. **Keep the attribution.** An adapted file carries the CookCLI copyright
   notice in its header, and `NOTICE` records it.
3. **No external host.** No CDN, no Google Fonts, no remote image. The
   Content Security Policy is `default-src 'self'` and it must stay that
   way. This is why CookCLI's inline `<style>` and inline `<script>` were
   moved into served files.
4. **One color per Cooklang entity.** Ingredient is amber, cookware is
   green, timer is red, through the CookCLI classes `ingredient-badge`,
   `cookware-badge`, and `timer-badge`.

### Where this differs from CookCLI, and why

Do not "correct" these back.

- **The amount sits inside the step badge.** CookCLI puts a bare name in the
  step and lists every amount again in a line underneath. That line makes a
  cook move between the sentence and the list, so this project puts the
  amount in the badge and drops the line. This was a direct request.
- **The palette choice comes from the server.** CookCLI sets a `dark` class
  from `localStorage` in an inline script. Here a cookie carries the choice
  and the server writes the class, so nothing flashes and no inline script
  is needed. `scripts/build-theme-css.mjs` emits their rules a second time
  under `prefers-color-scheme` so that following the system needs no script.
- **CookCLI screens that do not exist here** (shopping list, pantry, search)
  are absent, and CookLangHub screens that CookCLI has no equivalent for
  (History, Suggestions, Discussions, Variations, Sharing) are built from
  CookCLI's own classes.

### Accessibility

Semantic HTML, a label on every control, a visible focus ring, full keyboard
operation, and a working mobile viewport are still expected.

WCAG AA contrast is **not** a gate for the prototype. This was decided
deliberately: matching CookCLI exactly matters more right now than meeting a
ratio, and most of their palette meets AA anyway. Do not darken a CookCLI
colour to chase a ratio. Revisit this before the platform is used by people
outside the first small group.

### Build

    npm install
    npm run build      # theme.css, then the Tailwind stylesheet

Node is a build-time dependency only. The runtime is the Rust binary plus
the files in `static/`.

## Reuse and attribution

CookCLI is MIT licensed, copyright Alexey Dubovskoy. This project is
AGPL-3.0-or-later, which can absorb MIT code. Adapting a CookCLI file
requires keeping its copyright notice in the file header and recording the
file in `NOTICE`. The highest value pieces are the CodeMirror Cooklang mode
and the cooking-oriented rendering.

## Writing

Issue text and user-facing copy follow ASD-STE100 Simplified Technical
English: short sentences, active voice, one word for one meaning, condition
before command, and the modals can, will, and must only.
