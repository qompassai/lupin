# Changelog

## 1.1.0 — 2026-11-27

- First publication to the MCP Registry as `ai.qompass/lupin` (Registry onboarding scoped by the release sweep record, ~/workspace/release-sweep-20261127/RECORD.md, held item 7). server.json now carries the cargo package entry only (`lupin-mcp` 1.1.0, stdio): the MCPB bundle entry named the 1.0.0 bundle by SHA-256 and no 1.1.0 bundle exists, so it is retired rather than carried forward stale.
- README gains the `mcp-name: ai.qompass/lupin` ownership marker the Registry verifies against the crates.io package.
- No tool-surface or behaviour change from 1.0.0.

## 1.0.0 — crates.io publication (2026-11-15)

- Published to crates.io as `lupin-mcp` 1.0.0. The bare name `lupin` is held by an unrelated crate, so the package takes the fleet `-mcp` suffix (as slipway-mcp, broadside-mcp, periscope-mcp). Package rename only: the binary stays `lupin` (pinned via `[[bin]]`), the MCP server name stays `lupin`, and the tool surface is unchanged from the 2026-10-31 release below.
- server.json gains a cargo package entry (`lupin-mcp` 1.0.0, stdio) alongside the existing MCPB bundle package.

## 1.0.0 — 2026-10-31

- rmcp modern-only migration landed (rmcp 3.8.0, protocol 2026-07-28; WS2-W1 gate pass).
- Toolchain pin swept to nightly-2026-10-30 (rustc 1.101.0-nightly 92b5b8c87, clippy 0.1.101).
- Gates on the new pin: cargo test 26 passed / 0 failed; clippy --all-targets -D warnings clean; fmt --check clean; no future-incompatibilities reported.
- server.json advanced to 1.0.0 (bundle identifier now points at the v1.0.0 release path; fileSha256 still names the 0.1.0 bundle — MCPB repackage + MCP Registry republish are the pending primo-side leg, not claimed here). The crates.io bare name `lupin` is blocked; the 1.0 route is GitHub + MCPB + Registry.
