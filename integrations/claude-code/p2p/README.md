# Agents, summaries and verdicts

Conventions and examples for teams whose agents talk to each other over
nanace P2P. Nothing here is required by the server: these are page
conventions and prompts you can adapt.

| File | What it is |
|---|---|
| [summary-template.md](summary-template.md) | The page an agent writes after a P2P exchange (`P2P: …`) |
| [verdict-template.md](verdict-template.md) | The note the verdict agent writes (`Verdict: …`) |
| [verdict-agent.md](verdict-agent.md) | A Claude Code subagent that reads summaries and facts and writes verdicts |

## How the pieces fit

1. Universal agents (Claude Code on laptops, or agents in containers) split a
   task and ask each other questions with the `p2p_*` tools. Message text
   stays between their machines.
2. When a team's `Policy: P2P` lists a kind under `summaries.required_for_kinds`,
   the agent that received the answer records a summary page: title
   `P2P: <topic> (<trace id>)`, tag `trace-<trace id>`. The server's exchange
   journal shows whether such a page exists.
3. The verdict agent takes no part in P2P. It reads the summaries and the
   pages they link to, and writes `Verdict: <task>` notes.

## Running the verdict agent

Give it the nanace MCP server **with** `NANACE_ALLOW_WRITES=true` and
**without** `NANACE_ALLOW_P2P`: it then has no P2P tools at all, whatever its
prompt says.

```bash
claude mcp add nanace-verdict --scope user \
  --env NANACE_API_URL=https://kb.example.com \
  --env NANACE_TOKEN_FILE=/run/secrets/nanace-verdict.token \
  --env NANACE_ALLOW_WRITES=true \
  -- nanace-mcp
```

Copy `verdict-agent.md` to `.claude/agents/verdict.md` in the project (or to
`~/.claude/agents/`) and run it with the task key:
`Use the verdict agent for TASK-1`.

## What the code enforces, and what it does not

- A team member with the role `agent` creates pages and edits only pages
  created by its own account; it deletes nothing and cannot touch `Policy:`
  or `Wiki:` pages.
- "Its own account" is the point: agents that sign in as the **same**
  service account can edit each other's pages, verdicts included, and the
  audit log shows one actor for all of them. If the verdict must be
  protected from the agents it judges, give the verdict agent its own
  account.
- People with write access can edit summaries and verdicts. The audit log
  records who did.
