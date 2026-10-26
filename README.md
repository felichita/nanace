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

With `NANACE_DEV_LOGIN=true`, get a token for any email:

```bash
curl -s -X POST localhost:8080/api/v1/auth/dev-login \
  -H 'Content-Type: application/json' -d '{"email":"you@example.com"}'
```

## Connect Claude (MCP)

Build the MCP server once:

```bash
cargo build --release -p nanace-mcp
```

Claude Code:

```bash
claude mcp add nanace \
  --env NANACE_API_URL=http://127.0.0.1:8080 \
  --env NANACE_DEV_EMAIL=you@example.com \
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
        "NANACE_DEV_EMAIL": "you@example.com"
      }
    }
  }
}
```

Use `NANACE_TOKEN` instead of `NANACE_DEV_EMAIL` against an API without dev
login, and `NANACE_ORG_ID` to pick a default organization.

## Development

```bash
make check              # fmt, clippy -D warnings, unit tests
make test-integration   # API, RLS and MCP tests against docker compose
make test-model         # real embedding model test
```
