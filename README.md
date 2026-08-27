# CookLangHub

A platform to create, edit and share recipes.

CookLangHub is a self-hosted web application for a group of friends and family
who cook together. It gives Recipes, Cookbooks, and collaborative editing in
cooking words. Forgejo and Git hold the authoritative state underneath, so
nobody needs to know a Git command to use it.

[Cooklang](https://cooklang.org) is the Recipe format.

## Requirements

- Docker and Docker Compose.
- 1 GB of free disk space for the first installation.

## Start the stack

1. Copy the example environment file and make a session key:

   ```sh
   cp .env.example .env
   openssl rand -hex 32
   ```

2. Put the key in `.env` as `COOKLANGHUB_SESSION_SECRET`.

3. Start both services:

   ```sh
   docker compose up --build
   ```

4. Open <http://localhost:8080> for CookLangHub.
   Open <http://localhost:3000> for Forgejo.

The stack starts from empty state. It needs no manual database step and no
install wizard.

## Prepare sign-in

Forgejo owns the accounts. Do these steps once, after the first start.

1. Make the first Forgejo account. Registration is off by default, so the
   administrator creates it:

   ```sh
   docker compose exec -u git forgejo forgejo admin user create      --username YOURNAME --email you@example.com --admin
   ```

2. Make an access token for that account:

   ```sh
   docker compose exec -u git forgejo forgejo admin user      generate-access-token --username YOURNAME --scopes write:user --raw
   ```

3. Register CookLangHub with Forgejo:

   ```sh
   docker compose exec app cooklanghub bootstrap --forgejo-admin-token TOKEN
   ```

4. Open <http://localhost:8080> and select **Sign in**.

Step 3 is repeatable. Running it again reuses the same OAuth application and
issues a new client secret, so Forgejo never collects duplicates.

Add more people with `forgejo admin user create`, or turn registration on in
Forgejo.

## Prepare the automation account

A Cookbook can follow a Recipe. It then moves to each new Version of that
Recipe, and each move makes one Version of the Cookbook. A dedicated Forgejo
account is the author of those Versions, so that nobody has their name on a
change they did not make.

Make one ordinary account and give CookLangHub one access token for it:

```sh
docker compose exec -u git forgejo forgejo admin user create \
  --username cooklanghub-bot --email bot@example.com
docker compose exec -u git forgejo forgejo admin user \
  generate-access-token --username cooklanghub-bot --scopes all --raw
```

Put the token in `.env` as `COOKLANGHUB_AUTOMATION_TOKEN` and start the
application again. It asks Forgejo who the token belongs to and records the
answer.

The account is an ordinary one. It gets write access to a Cookbook only when
a Recipe in that Cookbook follows updates, and it loses the access when the
last one stops. If you take the access away in Forgejo, the automation stops
and the Cookbook page says so. CookLangHub does not give the access again.

An installation with no Cookbook that follows a Recipe needs none of this.

## Health

`GET /health` reports each component separately, so an administrator can tell
an application fault from a Forgejo fault:

```json
{
  "status": "ok",
  "version": "0.1.0",
  "application": { "status": "ok", "detail": "the application answers requests" },
  "database":    { "status": "ok", "detail": "the operational database answers queries" },
  "forgejo":     { "status": "ok", "detail": "Forgejo 15.0.0" }
}
```

The endpoint answers with 200 when every component answers, and with 503 when
one does not.

## Diagnostics

`/admin/index` reports the six parts that can fail on their own: the
application, Forgejo, the webhook, the reconciliation, the automation, and
the parser. Each part carries its own state, the facts behind it, and what
to do when it is not working. An administrator therefore finds the cause of
a fault without a read of the source code.

Only an administrator sees the detail. Forgejo decides who that is.

The page also starts a reconciliation. The sweep reads Forgejo and Git
again, writes to neither, makes both indexes match, and moves every Cookbook
that follows a Recipe to the Version that the Recipe has now. It is safe at
any moment, and it is what brings the installation back after a Forgejo
outage.

No credential reaches the page. The automation token, the webhook secret,
and the session secret are used to ask a question and none of them is
rendered or logged.

## During a Forgejo outage

Forgejo is the authority for identity, permissions, and every repository. So
while it does not answer:

- Every page says that CookLangHub cannot reach Forgejo.
- CookLangHub refuses every edit, before it starts. Nothing half-finished
  reaches Git.
- No list shows a Recipe or a Cookbook. The index is a cache, and
  CookLangHub never shows that copy as the Recipes a person has now.
- A person can still sign out and still change the appearance. Both are held
  by this application alone.

When Forgejo answers again, open **Diagnostics** and select **Start a
reconciliation**.

## Backup, restore, and upgrade

`docs/operations.md` has the procedures. A backup covers the whole instance:
the Forgejo dump, the CookLangHub database, and `.env`. Forgejo holds the
users, the permissions, the Recipes, the Cookbooks, the forks, the
Suggestions, the Discussions, and the History, so a backup of the Recipe
repositories alone is not enough.

`docker-compose.yml` names one exact Forgejo release, and the integration
tests run against that same release. The tag never floats, so
`docker compose pull` cannot move an installation to a new major release.
An administrator decides when to upgrade, and backs up first.

## Telemetry

CookLangHub sends nothing to an external service. It has no analytics, no
tracking pixel, and no crash report service. The only host it talks to is
the Forgejo of this installation. Every page carries
`Content-Security-Policy: default-src 'self'`, so a browser cannot load a
script, a style, a font, or an image from another host.

The logs stay on the machine and carry no access token, no session secret,
no Git credential, and no Recipe content. `tests/diagnostics.rs` holds all
of this.

## Develop

The application needs Rust 1.97 or later.

```sh
cargo test
```

The integration tests start a disposable Forgejo container, so Docker must
run. Each test removes its container afterwards.

Environment variables, all with the `COOKLANGHUB_` prefix:

| Name | Default | Purpose |
| --- | --- | --- |
| `BIND` | `0.0.0.0:8080` | Address of the HTTP server |
| `DATABASE_URL` | `sqlite://data/cooklanghub.db?mode=rwc` | Operational state |
| `PUBLIC_URL` | `http://localhost:8080` | Where a browser reaches this application |
| `INTERNAL_URL` | same as `PUBLIC_URL` | Where Forgejo reaches this application, for the webhook |
| `FORGEJO_URL` | `http://localhost:3000` | Base URL that the app uses for the Forgejo API |
| `FORGEJO_PUBLIC_URL` | same as `FORGEJO_URL` | Base URL that a browser uses for Forgejo |
| `FORGEJO_NOREPLY_DOMAIN` | `noreply.localhost` | Address domain for a person who hides their email |
| `SESSION_SECRET` | none, required | Signs session cookies and encrypts stored credentials |
| `WEBHOOK_SECRET` | derived from `SESSION_SECRET` | Signs each Forgejo webhook body |
| `AUTOMATION_TOKEN` | none | Access token of the automation account, for a Cookbook that follows a Recipe |
| `COOKIE_SECURE` | `true` | Whether the session cookie carries `Secure` |
| `LOG_FORMAT` | `json` | `json` or `pretty` |
| `LOG` | `info,cooklanghub=debug` | Log filter |

## Sign-in

Forgejo is the identity provider. The browser receives only a CookLangHub
session cookie, which carries `HttpOnly`, `Secure`, and `SameSite=Lax`. The
Forgejo access token stays on the server and is encrypted with a key derived
from `COOKLANGHUB_SESSION_SECRET`.

Changing the session secret invalidates every stored credential. That signs
everybody out, which is the correct result of a rotated key.

## Recipes

A Recipe is one Forgejo repository with the topics `cooklang` and `recipe`.
It holds one `recipe.cook` file, and `main` is the published Recipe. The
title comes from the Cooklang metadata, so the application stores no second
copy of it.

Cooklang parsing uses `cooklang-rs` with all canonical extensions. The
converter knows the bundled English units and the German names in
`units/german.toml`. Without those names a timer such as `~{8%Min.}` would
be an error, and an error stops creation.

A Version carries the Forgejo identity of the person who made it, as author
and as committer. A person who hides their address gets the Forgejo no-reply
address, because History is readable by anybody who can read the Recipe.

The Recipe page shows the cooked Recipe: what to gather, then what to do.
The layout follows CookCLI, so a cook who knows that interface can read
this one. The gather lists put the name on the left and the amount in bold
on the right. Inside a step each Cooklang entity keeps its own color and
carries its own amount, so the sentence reads straight through and the eye
never leaves it to look an amount up. The Cooklang source stays one click
away.

## Finding a Recipe

**Recipes** holds two lists: **Mine** and **Shared with me**. **Explore**
holds every public Recipe, and it needs no account. Each list can be
searched by title and ordered by the most recent change or by the title.

Forgejo names the Recipes that a person may see, on every request. The
index only supplies the words on the card, so a row in the index is never
permission to see anything.

### The Recipe index

The title that a person sees lives inside `recipe.cook`, so no question that
Forgejo can answer finds a Recipe by its title. The table `recipe_index`
holds that title and the culinary facts that a card shows.

The index is a cache. You can delete it at any time, and the application
builds it again from Forgejo and Git. Three things keep it current:

- **One system webhook.** The bootstrap command registers it in Forgejo, for
  repository events and push events. Forgejo signs each body with
  HMAC-SHA256, and the application refuses a body whose signature does not
  match.
- **Reconciliation.** The application reads Forgejo again when it starts, and
  whenever an administrator asks for it on the Diagnostics page. It reads
  Forgejo and Git, and writes to neither.
- **The pages themselves.** A page reads a Recipe again when Forgejo reports
  a change that the index does not hold yet.

Forgejo must be able to reach this application for the webhook. Inside the
bundled stack a browser and Forgejo use different addresses, so set
`COOKLANGHUB_INTERNAL_URL` to the address that Forgejo uses.

Forgejo 15 answers `GET /api/v1/admin/hooks` with an empty list, even after
it made a system webhook. The application therefore records the identifier
of the webhook that it registered, and uses `GET /api/v1/admin/hooks/{id}`
to find it again. This is why a repeated bootstrap makes no second webhook.

## Looking at the pages

The tests say whether the application behaves. They do not say whether a
page reads well. To look at it:

```sh
docker compose up --build -d
npx playwright install chromium --only-shell   # once
npm run shot -- --session YOUR_SESSION_COOKIE
```

This writes a picture of every page into `screenshots/`, at a desktop size
and a telephone size, in both palettes, and it reports any page that
scrolls sideways or writes to the browser console. The pictures are not
kept in Git.

## Appearance

The design is the CookCLI design. CookLangHub is a Cooklang product, so it
should look and feel like the rest of the Cooklang tools, with a
collaborative backend behind it.

CookCLI is MIT licensed, copyright Alexey Dubovskoy, and the licence permits
this reuse. `NOTICE` records every file taken, and `LICENSE-MIT-cookcli`
carries the full text.

A page follows the operating system until a person chooses otherwise. The
control in the footer offers System, Light, and Dark. The choice lives in a
cookie and the server writes the class onto the page, so the right palette
is in the first byte of HTML and nothing flashes. The page carries no
script at all.

### Build the assets

    npm install
    npm run build

Node is a build-time dependency only. The runtime is the Rust binary plus
the files in `static/`. The Docker image builds the assets in its own stage,
so `docker compose up --build` needs no Node on the host.

## Design

Forgejo is authoritative for identity, permissions, visibility, forks, pull
requests, issues, stars, and watches. Git is authoritative for Recipe content
and History. This application keeps operational state only, and every piece of
it is rebuildable.

The application reaches Forgejo through its supported HTTP API, OAuth,
webhooks, and the Git protocols. It never opens the database of Forgejo and
never touches its repository storage.

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).
