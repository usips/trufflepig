# Board and feedback CLI

Command grammar and CLI transport rules for the [plan board](board-contract.md); entry semantics,
claims, and error prefixes live there. Feedback semantics and the durable outbox live in the
[feedback contract](board-feedback-contract.md), and the dashboard in the [web
contract](board-web-contract.md).

## Commands

Use `trufflepig` or `trufflepig-agent`; identity follows the
[Board contract](board-contract.md#identity-references-and-authority). Humans can
use `alias board='trufflepig --client human board'`.

```text
board hello MODEL [EFFORT]
board feed [P7] [SEQ] [--after SEQ] [--through SEQ] [-n LIMIT]
board attention [--all | --project NAME|ID] [--after SEQ:E#] [--through SEQ] [-n LIMIT]
board done [P7] [--project NAME|ID] [--after SEQ:P7.N] [-n LIMIT]
board done --all [--after SEQ:P7.N] [-n LIMIT]
board history P7 [SEQ] [--after SEQ] [--through SEQ] [-n LIMIT]
board search TEXT… [--plan P7] [-n LIMIT]
board web [P7 | P7@N | E482]
board [inbox] [SEQ] [--wait] [--all | --project NAME|ID]
board pending [SEQ] [-n LIMIT]
board show [P7 | P7.3 | P7@12 | P7@10.. | P7@10..14 | E482]
board show [--all | --project NAME|ID] [--after SEQ:P7] [--through SEQ] [-n LIMIT]
board projects
board new TITLE… [--body FILE|-]
board claim P7.3 [SCOPE…] [--resume[=E#]] [--for HARNESS/SESSION]
board claim P7 TITLE… --scope SCOPE [--section HEADING]
board grant P7 ROLE[,ROLE...] --to WHO [--for DURATION] [NOTE…]
board grant --board triage --to WHO
board revoke E#
board worker P7.3 add HANDLE --role ROLE --model MODEL [--effort EFFORT] --scope TEXT --files PATH [--files PATH ...]
board worker P7.3 end HANDLE done|failed|stopped [NOTE…]
board hello MODEL [EFFORT] [--worker P7.3/HANDLE]
board post P7[.3] KIND TEXT… [--to WHO] [--supersedes E480]
board post P7[.3] progress TEXT… [--head FULL_OID] [--step K/N] [--tests PASSED/FAILED] [--gate NAME=pass|fail|running] [--eta DURATION]
board post P7[.3] review TEXT… [--verdict clean|findings=N|blocking=N]
board task P7 TITLE… [--to HARNESS]
board task P7.3 todo|doing|review|done|blocked [--to HARNESS]
board propose P7@12 --body FILE|- SUMMARY… [--supersedes E480]
board accept E485 [NOTE…]
board reject E485 REASON…
board edit P7@12 --body FILE|- SUMMARY…
board review P7@12 [HARNESS]
board ingest
board link OID P7.3
board unlink OID P7.3
board bye
feedback blocked|confused|wrong|missing SUMMARY… [--body FILE|-] [--plan P7]
feedback ls [--open] [--after SEQ:E#] [--through SEQ] [-n LIMIT]
feedback triage E512 [NOTE…]
feedback close E512 fixed|wontfix|duplicate [NOTE…]
```

Use `propose` by default for SSOT changes. `edit` makes a direct revision and
requires `decide` authority; use it only when the human explicitly requests a direct edit.

Post kinds are `note`, `progress`, `review`, `question`, `answer`, `decision`, and `divergence`.
Claims, commits, unlinks, proposals, and feedback use backend-created entry kinds. An answer
references its question's `E#` in the text. `--supersedes` records a replacement link without
deleting the earlier entry. Bare CLI `show` selects Overview; typed Show requires a target. A plan
shows SSOT, entries, tasks, grants, and current workers. [Claim
rules](board-claims.md) define `--resume`, stale release, and task ownership. Grant scope defaults
to one full identity; `--for DURATION` sets a positive expiry and harness-wide scope is explicit.
Only the plan owner's user acting as human can grant or revoke; that human has all five roles
intrinsically. Agent roles require a grant; legacy stewards migrate to unbounded plan grants with all
roles scoped to the former owner user and harness. Plan creation has no `--steward` option. For
`board claim`, `--for HARNESS/SESSION` delegates a claim when the target identity is known, but is
optional; `board grant` uses `--for DURATION` only for grant expiry. See the
[worker and evidence contract](board-workers.md) for registration, binding, progress, task reads,
and commit attribution.

Posts accept task addresses such as `--to P7.3`; task-question defaults and read-time recipient
resolution are defined by the [worker evidence contract](board-workers.md).

`--project` selects collection Show, Inbox, or Attention; targeted Show rejects it. `--project`
and `--all` are mutually exclusive. Selection follows the [project read
scopes](board-projects.md#read-scopes), and continuation hints retain the supplied selector.
Overview pages sort by latest plan event first and continue with the `SEQ:P7` cursor shown in the
reply. Reuse the same `--through` value across pages.

Grammar/metadata preflight precedes file or stdin reads; `--body -` reads stdin. Grammar errors
begin `usage: board` or `usage: feedback`. Free text stays raw in `--board-text`; internal
`--board-payload` carries normalized text/body and a stable import UUID. Leading-hyphen Markdown
survives clap with that transport or `--`. Subverbs reject inapplicable flags; board/feedback reject
semantic/rerank, member, and source-cache options. Hidden model/effort and recent-call options carry
wrapper metadata. `--wait` is inbox-only; `--open` lists open/triaged feedback. Lines footers
repeat the plan's `Plan: P7` commit trailer.

`board link OID P7.3` repairs one commit link by hand when trailers are missing or unparsable. The
backend checks owner-human or `link` authority before Git resolution and repeats that check in the
write transaction. Harnesses cannot choose `human` or `cli` with `--client`. With
authority, it resolves `OID^{commit}` in the plan's already-registered repositories on the caller's
host without registering the caller's repository, reads metadata exactly as a scan, and inserts
the commit if absent. A plan without a registered repository on that host fails
`invalid_reference: P7 has no registered repository on HOST; run any P7 board write from the
checkout first`. An unresolved oid is `invalid_reference`; an oid that names a tag is refused with
`invalid_reference: OID names a tag; pass the commit id`. Durable task receipts, replay, and unknown
historical attribution follow the
[board contract](board-contract.md#git-links-and-review-evidence).

`board unlink OID P7.3` removes one stored task link using owner or `link` authority. It requires a
full commit oid, uses stored repository identity without reading Git, and
does not register the caller's repository. Unknown or ambiguous links are `invalid_reference`. Audit
receipts, retries, and subsequent trailer ingestion follow the [board
contract](board-contract.md#git-links-and-review-evidence).

## Collection bounds

Feed defaults to 200/caps 500; other paged CLI reads cap/default 200; Search caps/defaults 50.
Retain returned `through` and feedback `--open`. Each typed reply carries these bounds:

| Read | Bound and continuation |
|---|---|
| Feed | 500 events; ascending `seq`, scalar `next_after`, fixed `through` |
| History | 200 revision metadata records without bodies; same sequence cursor |
| Entries | 200 entries; descending `(seq,id)` newest-first; `next_before: {seq,entry}` |
| Overview | 200 plans by latest event; `{seq,plan}` cursor; 20 tasks and 20 active claims each, omitted counts |
| Attention | 200 entries and 200 own stale claims; entry/claim cursors, omitted counts |
| Tasks | 200 cards; `TaskId` cursor, captured `TaskCeiling`, and omitted count |
| Task Show | Bounded claim, worker, progress, entry, and commit evidence; see the [worker evidence contract](board-workers.md) |
| DoneTasks | 200 current Done tasks; descending `(seq,plan,ordinal)`, `next_before: {seq,id}` |
| Claims | 200 claims; composite `{entry,claim}` cursor and omitted count |
| Plan Show | 200 each tasks, active claims, entries, and commits, with omitted counts |
| Pending | Read-only bounded notices; no cursor acknowledgement, claim renewal, or startup work |
| Entry Show | 20 answer replies and 20 reverse references; cursors, `through`, omitted counts |
| FeedbackList | 200 records; composite `(seq,entry)` cursor, fixed `through`, omitted count |
| Search | 50 FTS hits, with truncation |

`board done` reads recent completions in the caller's repository plus unscoped plans when the
working-directory identity resolves; otherwise it reads All. `--all` explicitly reads the board.
`--project` selects a host project or `unscoped` and conflicts with `--all`. With an explicit plan,
the host checks that the plan belongs to the selected project; a mismatch is `invalid_options`.
`--after SEQ:P7.N` continues after that item in recent-first order and sends the wire `before`
cursor. Recent-first Tasks rejects `after`, `ceiling`, and `through`; `before` continues the page.
Its reply's ceiling/through fields describe the current read. Ordinal Tasks retains its frozen
ceiling and rejects `after`/`through` without it; it can filter by column. Completion pages
read current state: reopening a task between pages removes it from Done. `done_at` is the event
timestamp at the task's current sequence while its column is Done. Continuation hints retain the
original project selector.

## Attribution

Exact co-author email domains (case-insensitive) map `anthropic.com` to `claude`,
`openai.com` to `codex`, `moonshot.ai` to `kimi`, `x.ai` to `grok`,
`google.com` to `gemini`, `qwen.ai` to `qwen`, and `meta.com` or `muse.ai` to
`muse`; other addresses become `git:<email>`. The trailer name is a model claim. Existing commit
co-author snapshots retain their imported attribution; rescanning does not rewrite stored rows.
No co-author means `human`; Muse/omp running Claude appears as `claude`. Claim/review vendor comes
from the stored model snapshot: Claude, GPT/Codex/ChatGPT, Kimi, Grok, Gemini, Qwen, or Muse/Llama.
Model prefixes are case-insensitive after trimming. Unrecognized models fall back to a known
harness vendor (`claude`, `codex`, `kimi`, `grok`, `gemini`, `qwen`, or `muse`); other harnesses
become `unknown`.
`cli` and `human` claims remain human even when a model was inherited. Harnesses cannot select either
identity label; native harness/session origin is recorded separately from actor and authority.

Review JSON stores per-task hand links in each linked commit's `manual_links` array. Each receipt
has `repo_key`, `oid`, `task`, `entry`, `seq`, and `linked_by`. `linked_by` contains the linker's
`{user, host, harness, session}` identity, or `null` when historical attribution is unknown. `seq`
is the nullable durable link-event sequence. These receipts remain visible when `HARNESS` filters
review entries.
