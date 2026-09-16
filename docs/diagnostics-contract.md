# Diagnostics and explicit sessions

## Request and delivery identity

Each client request receives a UUID and creation timestamp before execution.
`--session ID` takes precedence over `TRUFFLEPIG_SESSION`; without either, the
request is unassigned. `--client NAME` supplies an optional client label. These
values travel in the daemon request, independently of process identity.

One client emission boundary covers ordinary results, help, version, usage and
execution errors, zero-budget silence, and broken pipes. Receipts count the exact
prepared UTF-8 with `o200k_base`, separately from bytes successfully accepted by
the stdout writer. Complete delivery requires all prepared bytes and successful
flush. A partial write, write-zero, or flush failure remains incomplete; source
and metadata identities from it cannot count as viewed or surfaced evidence.
Daemon transport framing and stderr are outside stdout receipt accounting.
Writer acceptance is an observation, not proof a human or agent understood it.

Events retain identities at emission time, so audits do not need surviving result
sets. Search telemetry records actual retrieval lanes, elapsed time, candidate
identity and original rank, coverage, and typed outcomes. A `--format lines`
delivery records emitted handles from the page's first column but no coverage
(`src/cli/emitted_evidence.rs:capture_emitted`). It does not invent
graph retrieval, confidence probabilities, billed usage, or token savings.

## Modes and retention

`--diagnostics off|metadata|detailed` defaults to `metadata`. Metadata contains
bounded context, outcomes, identities, timing, and retrieval observations. It
excludes source bodies, raw queries, command lines, transcripts, and unrestricted
error strings. `detailed` adds raw search queries, truncated at 4 KiB. It does not
turn on source-body or transcript collection. `off` disables request recording
and rejects new diagnostic sessions.

Storage lives in `diagnostics/` beneath the selected root cache. The directory
uses mode 0700; created files use 0600. JSONL segments rotate at 50 MiB, records
are capped at 64 KiB, and retention is thirty days. The 200 MiB capacity budget
includes session storage and reserves room for SQLite rollback. The session
database has a 72 MiB ceiling and a further 72 MiB rollback reserve; event
segments can therefore be evicted well before the nominal total limit.

Daemon appends use a 32-event queue. Lock acquisition waits at most 10 ms; client
best-effort recording waits at most 20 ms including initialization. Logging
failure does not replace successful search output. These are bounded waits,
not hard cancellation of filesystem writes already in progress. Drops,
truncation, malformed records, and segment eviction make observation windows
incomplete. Complete loss of an event cannot always be reconstructed; unknown
loss must not become a claim of exhaustive diagnostic coverage.

`forget-logs` deletes journal segments and session baselines/reports, advances the
journal epoch, and invalidates requests created before deletion. Buffered old
records cannot recreate forgotten sessions. The command itself leaves no new
request event. Later requests form a new observation window.

## Session baselines and net changes

`session start` reconciles and persists a baseline from one live publication:
generation, capture interval, per-file revision/status, original-byte line hashes,
and declaration/occurrence key and content fingerprints. Baselines contain no
source bodies or raw symbol names. They survive replacement of live index
contents and process restart. A persisted index epoch distinguishes recreated
indexes with reused generation numbers; those sessions still compare endpoint
fingerprints while reporting an incomplete publication observation window.
At most fifty sessions may remain open; aggregate
baseline storage is capped at 64 MiB within the diagnostic budget. Capacity
exhaustion rejects a new baseline explicitly.

`session end ID` reconciles again and computes net observed changes with the
shared histogram and occurrence-aware source differ. It retains duplicate
occurrences, follows unique exact-content file renames, and cannot prove symbol
deletion from failed or incomplete extraction. Publication observations separate
observed deletions from ignored/excluded coverage changes. Eligible untracked
files participate. Commits do not end sessions.

Ending a session removes its baseline and persists its report. Concurrent
sessions receive symmetric `overlapping_observation` labels, without exclusive
authorship. Abandoned sessions stay open and incomplete until explicitly ended
or expired. After thirty days, open baselines expire into `expired_incomplete`
notices; ended/expired notices remain for thirty days from their end.

Reports retain at most 128 file details, 128 source hunks per file, 32 occurrence
rows per file, eight candidates per row, 256 KiB of aggregate file examples, and
128 publication observations. Truncation is explicit. Edits never published by a
reconciliation cannot be observed; overwritten intermediate edits are not
reconstructed from endpoint fingerprints.

## Audits and observational limits

`audit` lists retained request/session summaries. `audit ID` includes one session's
net-change report and evidence ledger. Audits join server and delivery events by
request UUID, with at most 10,000 retained requests and 32 MiB of decoded event
records. They expose malformed/truncated records, eviction, drops, incomplete
receipts, and incomplete windows.

Ledger comparisons use exact preimage path/revision and emitted byte-span
overlap. Only complete delivery receipts qualify. Metadata discovery and viewed
source are separate; additions without preimages, uncertain correspondence,
coverage changes, and incomplete publication windows remain separate categories.
Byte overlap is an observational proxy, not proof of full understanding,
exclusive authorship, task success, or complete source coverage. The stricter
full-span evidence rule belongs to the [navigation replay](navigation-evaluation.md).
