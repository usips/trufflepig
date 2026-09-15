# Trufflepig agent plugin

One directory that serves both Kimi Code and Muse Code:

- `skills/trufflepig-code-search/SKILL.md` follows the shared Agent Skills
  format that both harnesses auto-discover (`~/.kimi-code/skills/`,
  `.agents/skills/` in a project, Muse's personal skill store).
- `.muse-plugin/plugin.json` is the native Muse plugin manifest for builds
  that ship `muse plugins`; skills-only installs work everywhere.
- `bin/trufflepig-agent` wraps the CLI: it injects `--json`, `--client`,
  `--session`, and `--diagnostics detailed`, passes the response through
  unchanged, and appends one audit record per call.
- `bin/trufflepig-audit` summarizes those records per harness session and
  flags sessions where the tool struggled.
- `hooks/session-start.sh` records the harness session id per working
  directory so calls group by real session; `kimi/hooks.toml` wires it into
  Kimi, the Muse manifest wires it into Muse.

## Install

```sh
plugins/trufflepig-agent/install.sh --kimi --muse --kimi-hooks
plugins/trufflepig-agent/install.sh --project ~/Source/lunatic   # per-repo alternative
```

Requires `trufflepig` on `PATH` (`cargo install --path . --locked --features
semantic-cuda`) and `~/.local/bin` on `PATH`. Muse users can validate with
`muse skills validate plugins/trufflepig-agent/skills/trufflepig-code-search`.

## Audit logs

Every wrapper call appends a JSON line to
`$XDG_STATE_HOME/trufflepig/agent-audit/<harness>.jsonl`
(default `~/.local/state/trufflepig/agent-audit/`) with the verb, arguments,
exit code, latency, response status, hit count, truncation, per-member
`semantic_status`/`rerank_status`, and derived signals: `error`, `no_hits`,
`truncated`, `budget`, `stale`, `usage_error`, `semantic_degraded`,
`rerank_unavailable`, `slow`, `repeat_query`.

```sh
trufflepig-audit                  # one row per session, STRUGGLING flags
trufflepig-audit --since 24 --calls
trufflepig-audit --harness kimi --json
```

With the semantic lane enabled a search always returns cosine-ranked files, so
`no_hits` only fires for lexical, `sym:`, and `re:` searches; read
`repeat_query`, `follow_through`, and the daemon ledger for semantic misses.

The wrapper also tags each request with the harness and session, so the
daemon-side journal (`trufflepig audit`, `trufflepig audit SESSION`) joins
delivery receipts, retrieval lanes, and viewed-source evidence for the same
calls. Together the two views answer "is the tool struggling": repeated or
empty searches, budget and stale-handle errors, degraded lanes, slow calls,
and searches whose results were never opened.

## Environment

- `TRUFFLEPIG_AGENT_HARNESS` overrides harness detection (`kimi`, `muse`).
- `TRUFFLEPIG_SESSION` overrides the session id.
- `TRUFFLEPIG_AGENT_LOG_DIR` relocates the audit log directory.
- `TRUFFLEPIG_AGENT_DIAGNOSTICS` sets the daemon diagnostics mode
  (`detailed` by default, `metadata`, or `off`).
- `TRUFFLEPIG_BINARY` points at a specific trufflepig executable.
