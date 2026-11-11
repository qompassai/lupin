# Lupin
[![MCP Registry](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fregistry.modelcontextprotocol.io%2Fv0%2Fservers%2Fai.qompass%252Flupin%2Fversions&query=%24.servers%5B0%5D.server.version&label=MCP%20Registry)](https://registry.modelcontextprotocol.io/v0/servers/ai.qompass%2Flupin/versions)

An MCP server for the Revenue Alignment Engine: an opportunity ledger
and capability registry with lane-scoped access. Opportunities live in
one of two lanes — Qompass (`sfw`) or NTRanch (`nsfw`) — and a caller
scoped to one lane can never see the other lane's entries.

Lupin is a single Rust binary. It speaks MCP over stdio
(NDJSON JSON-RPC) to the client and works on plain files under a data
root — no database, no network listener. It is the ledger side of the
estate's revenue pipeline; the research-feed server
[phaedrus](https://github.com/qompassai/phaedrus) drafts candidates
into it.

## How it runs

- **Data root** (ledger, capability registry, scan inbox):
  `--root <path>` or the `REVENUE_ROOT` environment variable
  (default `~/workspace/revenue-alignment-20261019`).
- **Lane scope**: `--lane-scope operator|sfw` (default `operator`).
  The operator scope sees both lanes; an `sfw`-scoped caller can
  never see `nsfw` entries.
- Read-mostly by design: the only write files a *candidate* into the
  scan inbox (`opportunity_submit`). The validated ledger is never
  written by this server, and `capability_match` is advisory only —
  it never auto-aligns an opportunity.

## Tools

| Tool | What it returns |
| --- | --- |
| `capability_list` | The capability registry (the alignment vocabulary); lane scope enforced |
| `capability_match` | Advisory keyword match of free text against capabilities; never auto-aligns |
| `opportunity_get` | One ledger entry by id, including its raw block |
| `opportunity_list` | Ledger opportunities, optionally filtered by status; lane scope enforced |
| `opportunity_submit` | File a candidate into the scan inbox; never writes the ledger |
| `pipeline_summary` | Counts by status/lane plus parse health — the value-demonstration view |

## Client config

```json
{
  "mcpServers": {
    "lupin": {
      "command": "/path/to/lupin",
      "args": []
    }
  }
}
```

## Registry

`server.json` in this repo is the MCP Registry record for
`ai.qompass/lupin`. The server is distributed as an MCPB bundle
attached to the GitHub releases (Linux x64).

## License

Apache-2.0 — see [LICENSE](./LICENSE).
