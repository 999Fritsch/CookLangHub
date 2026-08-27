# Operations

This page is for the administrator of an installation. It gives the backup
procedure, the restore procedure, and the Forgejo upgrade procedure.

Read `README.md` first for the installation itself.

## What holds what

Two stores hold the state of an installation, and only one of them is
authoritative.

**Forgejo holds everything that matters.** Accounts, passwords, permissions,
visibility, Recipes, Cookbooks, forks, Suggestions, Discussions, Favorites,
and the whole History of every Recipe. Git holds the Recipe content inside
the repositories that Forgejo serves.

**CookLangHub holds operational state only.** One SQLite file with the
session rows, the OAuth client, the webhook registration, the automation
credential, the Recipe index, the Cookbook index, and the diagnostics
counters. Every row is rebuildable, because CookLangHub reads Forgejo and
Git again and writes each row back.

CookLangHub never opens the Forgejo database and never touches the Forgejo
repository storage. It reaches Forgejo through the HTTP API, OAuth,
webhooks, and Git. The backup procedure obeys the same rule: Forgejo makes
its own dump with its own command.

### What a restore brings back, and from where

| State | Comes from |
| --- | --- |
| Users, passwords, permissions | The Forgejo dump |
| Recipes, Cookbooks, and all History | The Forgejo dump |
| Forks and their lineage | The Forgejo dump |
| Suggestions and Discussions | The Forgejo dump |
| Favorites and Watches | The Forgejo dump |
| Sessions | The CookLangHub database |
| The OAuth client and the webhook | The CookLangHub database |
| The automation credential | The CookLangHub database |
| The Recipe index and the Cookbook index | Rebuildable |
| The diagnostics counters | Rebuildable |

A backup of the Recipe repositories alone is not enough. It holds no
account, no permission, no Suggestion, and no Discussion. Back up the whole
instance.

### What is rebuildable

The Recipe index, the Cookbook index, and the diagnostics counters are a
cache. If you lose the CookLangHub database, the installation still holds
every Recipe, every Cookbook, and every Version, because Forgejo and Git
hold them. Do three things to get the rest back.

1. Register the OAuth client and the system webhook again:

   ```sh
   docker compose exec app cooklanghub bootstrap --forgejo-admin-token TOKEN
   ```

2. Put the automation token in `.env` again, and start the application
   again.

3. Open **Diagnostics** and select **Start a reconciliation**.

Everybody must sign in again, because the session rows are gone.

## Back up the whole instance

Do these steps in this order. Forgejo keeps running, because the dump
command runs inside it. The application is stopped, so nobody can write
while the backup runs.

1. Stop the application, so that no new Version arrives during the backup:

   ```sh
   docker compose stop app
   ```

2. Make the Forgejo dump with the Forgejo command. The dump holds the
   database, the repositories, the attachments, and the configuration:

   ```sh
   docker compose exec -u git forgejo forgejo dump --file /tmp/forgejo-dump.zip
   docker compose cp forgejo:/tmp/forgejo-dump.zip ./forgejo-dump.zip
   docker compose exec -u git forgejo rm /tmp/forgejo-dump.zip
   ```

   The dump is as large as the instance, so make it in `/tmp` and copy it
   out. A dump inside `/data` goes into the next dump as well.

3. Copy the CookLangHub database:

   ```sh
   docker compose cp app:/data/cooklanghub.db ./cooklanghub.db
   ```

4. Copy `.env`. It carries the session key, and that key decrypts every
   stored credential. Keep it as safe as the dump itself.

5. Start the application again:

   ```sh
   docker compose start app
   ```

6. Open **Diagnostics** and read the six cards. Every card must say
   **Working**, or **Not in use** for a part that this installation does not
   use.

Keep the three files together. A dump without its `.env` gives Forgejo
back, and it gives no CookLangHub credential back.

## Restore the whole instance

A Forgejo dump holds four things: `app.ini`, `data/`, `repos/`, and
`forgejo-db.sql`. In the bundled stack `data/` is `/data/gitea`, and it
carries the SQLite database, the avatars, and the keys. `repos/` is
`/data/git/repositories`. `forgejo-db.sql` is the same database as portable
SQL, and a stack with PostgreSQL or MySQL restores that file with the tool
of that database instead.

1. Put `.env` back, then remove the old state:

   ```sh
   docker compose down --volumes
   ```

2. Make the containers and the volumes, then stop them again:

   ```sh
   docker compose up --detach
   docker compose stop
   ```

3. Unpack the dump:

   ```sh
   unzip forgejo-dump.zip -d ./restore
   ```

4. Remove the empty Forgejo state and put the dump in its place. Each
   destination must not exist. `docker compose cp` then makes it, as a copy
   of the directory that goes in:

   ```sh
   docker compose run --rm --user root --entrypoint sh forgejo \
     -c "rm -rf /data/gitea /data/git/repositories && mkdir -p /data/git"
   docker compose cp ./restore/data forgejo:/data/gitea
   docker compose cp ./restore/repos forgejo:/data/git/repositories
   docker compose run --rm --user root --entrypoint sh forgejo \
     -c "chown -R 1000:1000 /data"
   ```

5. Restore the CookLangHub database:

   ```sh
   docker compose cp ./cooklanghub.db app:/data/cooklanghub.db
   ```

6. Start everything:

   ```sh
   docker compose up --detach
   ```

7. Check that the accounts came back:

   ```sh
   docker compose exec -u git forgejo forgejo admin user list
   ```

8. Open **Diagnostics**. Read the Forgejo card and the webhook card first.
   If the webhook card says that Forgejo does not hold the webhook, run the
   bootstrap command again.

9. Select **Start a reconciliation**, and check that the Recipe count and
   the Cookbook count match what the installation had.

## Upgrade Forgejo

CookLangHub is tested against one Forgejo LTS release, and
`docker-compose.yml` names that exact release. The tag never floats, so
`docker compose pull` cannot move the installation to a new major release.
You decide when to upgrade.

The Diagnostics page reports the release that is running and the release
that CookLangHub was tested against. If the two majors differ, the Forgejo
card says so.

Forgejo supports an upgrade from one LTS release to the next LTS release.
Do not skip a major release.

1. Read the Forgejo release notes for every release between the one you run
   and the one you want. Look for a change to the API, to the webhook
   format, or to the OAuth flow.

2. Back up the whole instance with the procedure above. Do not continue
   without a backup.

3. Change the image tag in `docker-compose.yml` to the new release:

   ```yaml
   services:
     forgejo:
       image: codeberg.org/forgejo/forgejo:15.0.8
   ```

4. Change `FORGEJO_TAG` in `tests/support/mod.rs` to the same release, and
   run `cargo test`. The integration tests run against a disposable Forgejo
   of that release, so they say whether the adapter still behaves. A test
   holds the two values together, and it fails while they differ.

5. Pull and start:

   ```sh
   docker compose pull forgejo
   docker compose up --detach
   ```

6. Open **Diagnostics**. Every card must say **Working**, or **Not in use**.
   Read the webhook card: an upgrade can leave the system webhook behind. If
   Forgejo does not hold it, run the bootstrap command again.

7. Select **Start a reconciliation**, then open a Recipe, a Cookbook, and a
   Discussion, and check that each one reads correctly.

If a step fails, stop the stack, restore the backup, and report the fault.

## Forgejo outage

While Forgejo does not answer:

- Every page says that CookLangHub cannot reach Forgejo.
- CookLangHub refuses every edit. Nothing half-finished reaches Git.
- No list shows a Recipe or a Cookbook. CookLangHub keeps a copy of the
  titles to make a search fast, and it never shows that copy as the Recipes
  a person has now.
- `GET /health` answers with 503 and names the component that failed.

When Forgejo answers again, open **Diagnostics** and select **Start a
reconciliation**. The sweep reads Forgejo and Git again, writes to neither,
and makes both indexes match. A Cookbook that follows a Recipe then moves to
the Version that the Recipe has now.

## Telemetry

CookLangHub sends nothing to an external service. It has no analytics, no
tracking pixel, and no crash report service. The only host it talks to is
the Forgejo of this installation.

The logs stay on the machine. They are JSON lines by default, and no log
line carries an access token, a session secret, a Git credential, or the
content of a Recipe.

Every page carries `Content-Security-Policy: default-src 'self'`, so a
browser cannot load a script, a style, a font, or an image from another
host. Tests hold all of this.
