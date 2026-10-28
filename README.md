# nanace

Multi-tenant markdown knowledge base with team-level RBAC and semantic search,
exposed to Claude through MCP. See [ARCHITECTURE.md](ARCHITECTURE.md).

## Quick start

```bash
cp .env.example .env          # set NANACE_JWT_SECRET (openssl rand -base64 48)
make run                      # starts PostgreSQL + Qdrant, then the API on :8080
```

The first start downloads the `bge-small-en-v1.5` model (~130 MB) into
`.fastembed_cache/`.

### Google sign-in

1. In Google Cloud Console create an OAuth client of type **Web application**
   with redirect URI `http://127.0.0.1:8080/api/v1/auth/google/callback`
   (use your `NANACE_PUBLIC_URL`).
2. Set `NANACE_GOOGLE_CLIENT_ID`, `NANACE_GOOGLE_CLIENT_SECRET`, and
   optionally `NANACE_GOOGLE_ALLOWED_DOMAINS` in `.env`.
3. Open <http://127.0.0.1:8080/auth/complete>, sign in, and create a
   personal access token (`read` scope is enough for Claude).

Without Google, `NANACE_DEV_LOGIN=true` gives a session for any email:

```bash
curl -s -X POST localhost:8080/api/v1/auth/dev-login \
  -H 'Content-Type: application/json' -d '{"email":"you@example.com"}'
```

## Connect Claude (MCP)

In the desktop app, choose **Connect to Claude** in the sidebar. It
registers the bundled MCP server with Claude Code (all projects) and/or
Claude Desktop, each with its own read-only token. Check **Allow Claude to
create and edit pages** to let Claude save notes ("save this to nanace").
To set it up by hand instead, build the MCP server once:

```bash
cargo build --release -p nanace-mcp
```

Claude Code, with a personal access token:

```bash
claude mcp add nanace \
  --env NANACE_API_URL=http://127.0.0.1:8080 \
  --env NANACE_TOKEN=nnc_pat_... \
  -- "$PWD/target/release/nanace-mcp"
```

Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "nanace": {
      "command": "/absolute/path/to/target/release/nanace-mcp",
      "env": {
        "NANACE_API_URL": "http://127.0.0.1:8080",
        "NANACE_TOKEN": "nnc_pat_..."
      }
    }
  }
}
```

Instead of `NANACE_TOKEN` you can set `NANACE_TOKEN_FILE` to a file that
holds the token (re-read on every request; keep it readable only by you), so
the token stays out of config files.

Against a development API you can use `NANACE_DEV_EMAIL=you@example.com`
instead of a token. `NANACE_ORG_ID` picks a default organization.
`NANACE_ALLOW_WRITES=true` (with a write-scoped token) adds the
`create_context` and `update_context` tools.

### Work-log skill for Claude Code

`integrations/claude-code/skills/nanace` is a Claude Code skill: `/nanace
PROJECT-2345` records what was done in the current session on the task's page
(appending a dated section, or creating the page). It treats each team as a
wiki: before writing it reads the team's `Wiki: Schema` page (page types, title
prefixes, templates, tags), and afterwards it updates the related `Topic:` page
and `Wiki: Index`. A team without a schema is initialized from
`schema-template.md`, and the new schema is marked as the default for review.
It needs the write tools (**Allow Claude to create and edit pages**).

Install or update the MCP server binary and the skill, and connect Claude Code
to your server, with one command (run it again later to update). Point it at
your server with `NANACE_API_URL`; the default is `http://127.0.0.1:8080`:

macOS / Linux:

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/felichita/nanace/HEAD/integrations/claude-code/install.sh)"

# with your server:
NANACE_API_URL=https://nanace.example.com \
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/felichita/nanace/HEAD/integrations/claude-code/install.sh)"
```

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/felichita/nanace/HEAD/integrations/claude-code/install.ps1 | iex"

# with your server, in a PowerShell window:
$env:NANACE_API_URL = 'https://nanace.example.com'
irm https://raw.githubusercontent.com/felichita/nanace/HEAD/integrations/claude-code/install.ps1 | iex
```

What the installer does:

- **`nanace-mcp`**: downloads it from the latest GitHub release
  (`NANACE_VERSION=v0.1.3` pins one), checks the `.sha256`, and installs it to
  `~/.local/bin` (Windows: `%LOCALAPPDATA%\nanace\bin`; override with
  `NANACE_BIN_DIR`). A `.nanace-mcp.version` file next to it records the release
  and checksum, so a re-run downloads nothing when the binary is current,
  updates it when a newer release exists, and repairs it when the file was
  changed. The replacement is atomic, so a running server is not affected.
  Published builds: macOS arm64, Linux x64/arm64 (glibc), Windows x64; on other
  platforms (for example Intel macOS) the binary is skipped — use the desktop
  app, which bundles it, or `cargo build --release -p nanace-mcp`.
  `NANACE_SKIP_BINARY=1` installs only the skill.
- **Skill**: copies it to `~/.claude/skills/nanace` (or
  `$CLAUDE_CONFIG_DIR/skills/nanace`) from `NANACE_REF` (default `main`). An
  older version is moved to `~/.claude/skill-backups/` first.
- **Claude Code connection** (interactive, asks before changing anything):
  connects Claude Code to the server at `NANACE_API_URL` (default
  `http://127.0.0.1:8080`, for example
  `NANACE_API_URL=https://nanace.example.com`). It opens
  `<server>/auth/complete`, where you sign in and create a token with scope
  `write`, reads the token with hidden input, checks it against the API, and
  saves it to `~/.config/nanace/claude-code.token` (Windows:
  `%LOCALAPPDATA%\nanace\claude-code.token`; override with
  `NANACE_TOKEN_FILE`), readable only by you. If you belong to several
  organizations it asks which one to use (or set `NANACE_ORG_ID`). Then it
  runs `claude mcp add nanace --scope user` with `NANACE_TOKEN_FILE`, so the
  token is never in a config file or a command line. A re-run reuses the saved
  token, and does nothing when Claude Code already uses this `nanace-mcp`
  (`NANACE_RECONNECT=1` connects again, e.g. after changing the server).
  If Claude Code runs another copy (such as the desktop app's, which
  **Connect to Claude** registers and the app updates), it asks before
  switching. Without a terminal, or with `NANACE_CONNECT=0`, it only prints the
  manual steps. New users need an account on the server and, to save pages, a
  team role of `writer` from an admin; the installer warns when that is missing.

To install the skill from a local clone instead of GitHub:

```bash
NANACE_BASE_URL="file://$PWD/integrations/claude-code/skills/nanace" integrations/claude-code/install.sh
```

## Desktop app

Requires Node 20+ and the Rust toolchain. With the API running:

```bash
make desktop-dev        # npm install + tauri dev
make desktop-build      # release bundle in apps/desktop/src-tauri/target/release/bundle
```

Sign in with Google (opens your browser) or, against a dev server, with any
email. **Import Claude memory**, in the sidebar or ⌘K, turns Claude Code's
memory files (`~/.claude/projects/*/memory`) into linked pages. The app stores a personal access token named "nanace desktop" in the
macOS Keychain (or the OS keyring).

## Docker

The `Docker` workflow publishes the backend image for `linux/amd64` and
`linux/arm64` for every `v*` tag (pull requests only build it):

```bash
docker pull ghcr.io/felichita/nanace-server:latest   # or :0.1.2, :0.1
```

The image runs `nanace-server` as a non-root user on port 8080 and has the
embedding model baked in, so it starts without internet access. It does not
contain `nanace-mcp`: Claude runs that on each user's machine (it ships with
the desktop app and the release archives) and points it at the server. Configure it with the same environment variables as
`.env.example` (`DATABASE_URL`, `QDRANT_URL`, `NANACE_JWT_SECRET`,
`NANACE_PUBLIC_URL`, ...); PostgreSQL must give it a non-superuser role (see
`deploy/postgres/init.sql`). Probes: `GET /livez` (liveness: the process
answers, no dependencies touched) and `GET /readyz` (readiness: PostgreSQL and
Qdrant answer; `/healthz` is the same).

Build it locally with `docker build -t nanace-server .`, or run the whole
stack (API server, PostgreSQL, Qdrant) with Docker Compose:

```bash
make up      # docker compose --profile app up -d --build; settings from .env
make logs    # follow the server logs
make down    # stop it (data volumes are kept)
```

The server listens on `127.0.0.1:8080`; set `NANACE_HOST_BIND=0.0.0.0` to
share it on your network.

### Logs and metrics

- **Logs:** `NANACE_LOG_FORMAT=json` writes one JSON object per line, for
  Loki or ELK (the default is `text`). Request logs carry the path only,
  never query strings.
- **Metrics:** `NANACE_METRICS_BIND=0.0.0.0:9090` serves Prometheus metrics
  on `/metrics` on that separate listener, so the public ingress never
  exposes them. Docker Compose serves them on `127.0.0.1:9090`.
  - `nanace_http_requests_total` and `nanace_http_request_duration_seconds`,
    by method, route pattern and status;
  - `nanace_embedding_duration_seconds`, by `passage` or `query`.

### Upgrades

By default the server upgrades the database when it starts:

- it applies the SQL migrations;
- it runs the application upgrades written in Rust (for example backfilling
  derived data);
- if `NANACE_EMBEDDER` now selects a different model, it reindexes.

On Kubernetes, run that as a separate step instead (an init container, a
Job, or a Helm pre-upgrade hook) and start the pods with
`NANACE_RUN_MIGRATIONS=false`. They then refuse to start on a database that
is behind or ahead of them:

```bash
nanace-server migrate --check   # report only; exit status 1 when something is pending
nanace-server migrate           # apply, then exit
```

To rebuild the vectors from PostgreSQL, for example after moving to a new or
empty Qdrant or changing the embedding model, run the `reindex` command. It
recreates each organization's collection; search for that organization is
incomplete until it finishes:

```bash
docker compose --profile app run --rm server reindex            # all organizations
docker compose --profile app run --rm server reindex --org <id>  # one organization
cargo run -p nanace-server -- reindex                            # from source, with .env
``` Unverified email login stays off in Compose unless
`NANACE_COMPOSE_DEV_LOGIN=true`.

## Releases

Prebuilt downloads are on the GitHub Releases page:

- **Desktop app:** macOS (Apple silicon and Intel), Windows x64, and Linux
  x64/arm64 (AppImage, deb, rpm).
- **Server and MCP archives:** Linux x64/arm64, macOS arm64, Windows x64.

To publish a release, push a version tag:

```bash
git tag v0.1.0 && git push origin v0.1.0
```

The `Release` workflow builds everything on native runners, uploads it to a
draft release, and publishes it when every build succeeds. Builds are not
code-signed yet.

## Development

```bash
make check              # fmt, clippy -D warnings, unit tests
make test-integration   # API, RLS and MCP tests (nanace_test DB + qdrant-test)
make test-model         # real embedding model test
make desktop-check      # desktop: tsc, vitest, clippy, Rust tests
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Copyright 2026 Fedor Nikolaev.
