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
| `FORGEJO_URL` | `http://localhost:3000` | Base URL that the app uses for the Forgejo API |
| `FORGEJO_PUBLIC_URL` | same as `FORGEJO_URL` | Base URL that a browser uses for Forgejo |
| `FORGEJO_NOREPLY_DOMAIN` | `noreply.localhost` | Address domain for a person who hides their email |
| `SESSION_SECRET` | none, required | Signs session cookies and encrypts stored credentials |
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
Each Cooklang entity keeps its own color, the same mapping that CookCLI
uses, so ingredient is amber, cookware is green, and a time is red. The
Cooklang source stays one click away.

Everything a person wrote reaches the page as text, and the template
escapes it, so a Recipe that contains markup shows those characters and
cannot run. An address becomes a link only when it starts with `http` or
`https`. No page asks the browser for an image on another host.

## Addresses

Forgejo reports `clone_url` and `html_url` built from its own `ROOT_URL`.
The application does not use either. It builds a Git address from
`FORGEJO_URL`, which is how this process reaches Forgejo, and a browser
address from `FORGEJO_PUBLIC_URL`. In the bundled stack these differ, and
using the reported value would break every push.

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
