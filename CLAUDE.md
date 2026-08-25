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

The palette follows CookCLI and cooklang.org so that the Cooklang ecosystem
reads as one family.

**`static/tokens.css` is the single source of truth.** Every color, size,
radius, and font stack lives there. Never write a literal color anywhere
else. Use `var(--...)`.

Five rules:

1. **No external host.** No CDN, no Google Fonts, no remote image. The
   Content Security Policy is `default-src 'self'` and it must stay that
   way. Self-hosting a font file is the correct way to add one.
2. **Measure contrast.** Every foreground and background pair must reach
   WCAG 2.1 AA, which is 4.5:1 for normal text. Each pair in `tokens.css`
   carries its measured ratio in a comment. Change a value, measure again.
3. **`--brand` is not a text color.** `#ff6b35` gives 2.84:1 and fails AA.
   It is for rules, gradients, and placeholder fills. Text and buttons use
   `--brand-ink` and `--brand-fill`.
4. **One color per Cooklang entity.** Ingredient is amber, cookware is
   green, timer is red. This mapping comes from CookCLI, so a cook who
   knows that interface can read this one. Do not reuse these hues for
   anything else.
5. **It must not look like a code forge.** Recipe pages are culinary
   first. History, Suggestions, and Variations use quiet neutral chrome,
   never the visual language of a source control tool.

Accessibility is a shipping requirement, not a later ticket: semantic HTML,
a label on every control, a visible focus ring, full keyboard operation,
and a working mobile viewport.

### Where the design differs from CookCLI, and why

Do not "correct" these back.

- CookCLI puts white text on `#ff6b35` and uses `orange-700` on
  `light-orange`. Both fail WCAG AA. This project darkens the foreground.
- CookCLI uses gradients on buttons, pills, and navigation. This project
  uses flat fills, because a flat fill has a contrast ratio that can be
  measured. The one gradient kept is the three-stop rule on a Recipe card,
  which is decoration and carries no text.
- cooklang.org loads Inter, Lora, and JetBrains Mono from Google Fonts.
  This project serves its fonts itself.
- CookCLI has no History, Suggestion, Discussion, Variation, or Sharing
  interface. Those screens have no upstream precedent, so design them
  quietly from the tokens rather than inventing new decoration.

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
