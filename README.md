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

Against a development API you can use `NANACE_DEV_EMAIL=you@example.com`
instead of a token. `NANACE_ORG_ID` picks a default organization.
`NANACE_ALLOW_WRITES=true` (with a write-scoped token) adds the
`create_context` and `update_context` tools.

### Work-log skill for Claude Code

`integrations/claude-code/skills/nanace` is a Claude Code skill: `/nanace
PROJECT-2345` records what was done in the current session on the task's page
(appending a dated section, or creating the page). It needs the write tools
(**Allow Claude to create and edit pages**). Install it for all projects:

```bash
mkdir -p ~/.claude/skills && cp -r integrations/claude-code/skills/nanace ~/.claude/skills/
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
