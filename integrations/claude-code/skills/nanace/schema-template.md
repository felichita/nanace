> Rules for this team's wiki in nanace. Every agent (Claude Code `/nanace`, etc.) and person editing this team follows them. Pattern: LLM Wiki — raw sources stay in GitHub/Jira/Slack; this wiki is the compiled, interlinked layer; this page is the schema. Catalog: [[Wiki: Index]].

> **Note:** initialized automatically by `/nanace` on {{DATE}} because this team had no schema. It is the default template: review it and adapt it to the team.

## Page types (title prefix = type)

| Prefix | What it is | Example | Tag |
|---|---|---|---|
| `Wiki:` | Service pages: the index and this schema | `Wiki: Index` | `wiki` |
| `Topic:` | Synthesis of one area across tasks and repos: current state, history by task, open items | `Topic: Ingress migration` | `topic` |
| `<KEY>:` | Task / work log. KEY = Jira key (`PROJECT-2345`) or an upper-case slug when there is no ticket (`ONDUTY-2026-10-28`). One page per key | `PROJECT-2345: Move staging ingress to a new gateway` | `task` + lower-case key |
| `System:` | A shared system/tool/library: what it is, where it is configured, history, open items | `System: Artifact registry` | `system` |
| `Service:` | One application/service, from the team's angle | `Service: billing-api` | `service` |
| `Ref:` | Short stable reference (aliases, config layout, how-to) | `Ref: Shell aliases` | `ref` |
| `Source:` | Provenance/catalog of a raw source batch (PR lists, exports). Not edited after creation except for corrections | `Source: PRs 2026-08..10` | `source` |

Rules:
- The `<KEY>:` format is what `/nanace <key>` looks for (title must **start with the key**), so never rename a task page to another format.
- Titles are short names, not descriptions. Put the description in the first `>` line.
- English everywhere (nanace semantic search works best on English).

## Page templates

**Topic**
```
> One-paragraph summary of the area and its current state.
**Tasks:** [[KEY: …]], … · **Systems:** [[System: …]] · **Sources:** [[Source: …]]
## Current state
## History (by task, newest last; PR numbers + dates)
## Open items   (- [ ] …, each with its task link)
```

**Task (`<KEY>:`)** — created/appended by `/nanace`
```
> Goal in one sentence.
**Topic:** [[Topic: …]] · **Status:** open | done | deferred
## YYYY-MM-DD: what this session achieved
**Done** / **Decisions** / **Verification** / **Open items**
```

**System / Service**
```
> What it is, where it runs, URLs.
**Topic:** [[Topic: …]]
## Facts (config location, versions, access)
## History (dated, with task links)
## Open items
```

## Links and tags
- Link with `[[Exact Page Title]]`. Every page links to at least one Topic (or the Index); every Topic links to its tasks and systems.
- Tags: the type tag from the table + lower-case task keys + a few domain tags. No tool-specific tags (`claude-memory`, …).
- Never put secrets (passwords, tokens, keys, encrypted config values) on a page.

## Operations
- **Ingest** (new source: PRs, on-duty session, investigation): write/append the task page → update the affected Topic(s) and System/Service pages → add any new page to [[Wiki: Index]].
- **Query**: read [[Wiki: Index]] first, then the Topic, then tasks. A useful answer can be filed back as a Topic section or a `Ref:` page.
- **Lint** (periodically): duplicate pages for one key, pages missing from the Index, links to non-existent titles, open items already done, stale "current state".
- **Merging pages**: the API cannot delete. A merged page becomes a stub: title `Merged: <old title>`, tag `to-delete`, body `Merged into [[…]]`. Delete stubs by hand in the nanace UI.
