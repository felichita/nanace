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

Build the MCP server once:

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

## Development

```bash
make check              # fmt, clippy -D warnings, unit tests
make test-integration   # API, RLS and MCP tests against docker compose
make test-model         # real embedding model test
```
