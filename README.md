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
| `FORGEJO_URL` | `http://localhost:3000` | Base URL that the app uses for the Forgejo API |
| `FORGEJO_PUBLIC_URL` | same as `FORGEJO_URL` | Base URL that a browser uses for Forgejo |
| `SESSION_SECRET` | none, required | Key that signs session cookies |
| `LOG_FORMAT` | `json` | `json` or `pretty` |
| `LOG` | `info,cooklanghub=debug` | Log filter |

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
