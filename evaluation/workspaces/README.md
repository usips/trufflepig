# Workspace navigation replay

The runner exercises member-qualified discovery and immutable source reads with
the same 600-token response budget, three result pages, and eight total calls as
the ordinary navigation replay. Its explicit label is **oracle-assisted navigation
replay**: authored expected paths guide hit selection and frozen followup queries.
This measures navigation mechanisms, not agent effectiveness or feature placement.

```sh
python3 evaluation/workspace_replay.py \
  --binary target/debug/trufflepig \
  --workspace evaluation/workspaces/space.toml \
  --root ~/Source/lunatic \
  --manifest evaluation/manifests/workspace-space.json \
  --cache ~/.cache/codex-tmp/trufflepig-space-eval \
  --no-daemon
```

The example configuration uses the local `~/Source` checkouts. Prime member
indexes before measuring navigation when they are large. Each invocation has a
30-second timeout; a timeout retains partial bytes and censored cost. The runner
uses the supplied cache, or the normal workspace cache when omitted. It creates
no temporary cache and never places artifacts in `/tmp`.

Install the optional Python `tiktoken` package to count exact emitted UTF-8 with
`o200k_base`. Without it, output bytes and elapsed time remain measured while token
counts remain unknown and costs censored. No billed or input-token estimates are
invented. The CLI receives `--budget 600` on every call.

## Manifest and records

Version-one manifests contain a `tasks` array. Each task has a unique `id`, frozen
`query`, optional `followup_queries` array, optional `requires_ctx` boolean, and
nonempty `expected` labels. Each label requires `member` and root-relative `path`.
Optional `start` and `end` original-byte offsets require full-span coverage;
without offsets, any verified emitted bytes in the expected file count as
path-level source evidence. Optional `sha256` rejects a changed labeled file.

Only labeled-file fingerprints are frozen. Unrelated checkout content may change
retrieval ordering, so records do not claim a complete repository snapshot.
The real manifest covers crayons/writing and airlocks/atmospherics using existing
source paths. It preserves misses; it does not require every query to match every
member. A frozen `documents append` followup supplies a complementary engine
query for the crayon task. Expected paths do not assert cross-language symbol
equivalence or implementation ownership.

Records carry versioned task, event, outcome, and usage objects. Event identities
retain member, root, path, revision, handle, and original member rank when emitted.
Metadata discovery requires matching member/root provenance. Source evidence
additionally requires complete successful delivery, matching immutable identity,
and exact emitted bytes checked against the configured member. Required `ctx`
checks the selected subject identity and a complete nonempty relationship list.

Search and `more` share the three-page limit. The runner reserves one page for
each remaining frozen followup query. `show` follows only returned immutable
continuations, and all operations share the eight-call limit. Outcomes preserve
missing labels, observed spent cost, stop reasons, and censoring.

The complete runner-neutral procedure lives in
[`workspace-navigation-v1.json`](../workflows/workspace-navigation-v1.json).
Run its narrow regression suite with:

```sh
python3 -m unittest discover -s evaluation -p test_workspace_replay.py
```

## Recorded local run

[space_result.json](space_result.json) records three real-checkout tasks. All
commands completed, but only one of eight authored file labels was discovered
and viewed; none of the three tasks completed within three pages. These misses
remain in the record. A separate ordinary `crayons` query from Lunatic returned
pack and tgstation matches on its first page, with zero Lunatic matches reported.

The initial three-member foreground index/query took 267.58 seconds and reached
133,844 KiB peak process RSS. These are debug-build observations, not latency or
memory acceptance thresholds. Index coverage reported exclusions and extraction
failures. The navigation run used published indexes and background coordination;
semantic inference was disabled. Python token counts were unavailable, so costs
remain censored. The CLI independently enforced its 600-token output budget.
