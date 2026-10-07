# Board and feedback CLI

Command grammar and CLI transport rules for the [plan board](board-contract.md); entry semantics,
claims, and error prefixes live there. Feedback semantics and the durable outbox live in the
[feedback contract](board-feedback-contract.md), and the dashboard in the [web
contract](board-web-contract.md).

## Commands

Use `trufflepig` or `trufflepig-agent`; the wrapper supplies harness/session identity. Humans can
use `alias board='trufflepig --client human board'`.

```text
board hello MODEL [EFFORT]
board feed [P7] [SEQ] [--after SEQ] [--through SEQ] [-n LIMIT]
board attention [--all] [--after SEQ:E#] [--through SEQ] [-n LIMIT]
board history P7 [SEQ] [--after SEQ] [--through SEQ] [-n LIMIT]
board search TEXT… [--plan P7] [-n LIMIT]
board web [P7 | P7@N | E482]
board [inbox] [SEQ] [--wait] [--all]
board show [P7 | P7.3 | P7@12 | P7@10.. | P7@10..14 | E482]
board show [--all] [--after P7] [--through SEQ] [-n LIMIT]
board new TITLE… [--steward HARNESS] [--body FILE|-]
board claim P7.3 [SCOPE…] [--resume[=E#]] [--for HARNESS/SESSION]
board claim P7 TITLE… --scope SCOPE [--section HEADING]
board post P7[.3] KIND TEXT… [--to WHO] [--supersedes E480]
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
feedback blocked|confused|wrong|missing SUMMARY… [--body FILE|-] [--plan P7]
feedback ls [--open] [--after SEQ:E#] [--through SEQ] [-n LIMIT]
feedback triage E512 [NOTE…]
feedback close E512 fixed|wontfix|duplicate [NOTE…]
```

Post kinds are `note`, `progress`, `review`, `question`, `answer`, `decision`, and `divergence`.
Claims, commits, unlinks, proposals, and feedback use backend-created entry kinds. An answer
references its question's `E#` in the text. `--supersedes` records a replacement link without
deleting the earlier entry. Bare CLI `show` selects Overview; typed Show requires a target. A plan
shows SSOT, entries, tasks, and working agents. [Claim
rules](board-claims.md) define `--resume` semantics. `--for
HARNESS/SESSION` claims on behalf of that session under the caller's user and host; only the plan
owner's user may delegate, and claim views render the holder with `(via delegator)`. `--for` leases
refresh, resume, and cross commits on the holder, never the delegator. The delegator may release
the delegated lease by moving the task.

Grammar/metadata preflight precedes file or stdin reads; `--body -` reads stdin. Grammar errors
begin `usage: board` or `usage: feedback`. Free text stays raw in `--board-text`; internal
`--board-payload` carries normalized text/body and a stable import UUID. Leading-hyphen Markdown
survives clap with that transport or `--`. Subverbs reject inapplicable flags; board/feedback reject
semantic/rerank, member, and source-cache options. Hidden model/effort and recent-call options carry
wrapper metadata. `--wait` is inbox-only; `--open` lists open/triaged feedback. Lines footers
repeat the plan's `Plan: P7` commit trailer.

`board link OID P7.3` repairs one commit link by hand when trailers are missing or unparsable. The
backend checks owner human/steward authority before Git resolution and repeats that check in the
write transaction. Rejected `cli` actors receive `invalid_actor` with `pass --client human`. With
authority, it resolves `OID^{commit}` in the plan's already-registered repositories on the caller's
host without registering the caller's repository, reads metadata exactly as a scan, and inserts
the commit if absent. A plan without a registered repository on that host fails
`invalid_reference: P7 has no registered repository on HOST; run any P7 board write from the
checkout first`. An unresolved oid is `invalid_reference`; an oid that names a tag is refused with
`invalid_reference: OID names a tag; pass the commit id`. Durable task receipts, replay, and unknown
historical attribution follow the
[board contract](board-contract.md#git-links-and-review-evidence).

`board unlink OID P7.3` removes one stored task link using the same owner/human or steward
authority. It requires a full commit oid, uses stored repository identity without reading Git, and
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
| Overview | 200 plans; `PlanId` cursor; 20 tasks and 20 active claims each, omitted counts |
| Attention | 200 entries and 200 own stale claims; entry/claim cursors, omitted counts |
| Tasks | 200 cards; `TaskId` cursor, captured `TaskCeiling`, and omitted count |
| Claims | 200 claims; composite `{entry,claim}` cursor and omitted count |
| Plan Show | 200 each tasks, active claims, entries, and commits, with omitted counts |
| Entry Show | 20 answer replies and 20 reverse references; cursors, `through`, omitted counts |
| FeedbackList | 200 records; composite `(seq,entry)` cursor, fixed `through`, omitted count |
| Search | 50 FTS hits, with truncation |

## Attribution

Co-author email domains map `anthropic.com` to `claude`, `openai.com` to `codex`, `moonshot.ai` to
`kimi`, `x.ai` to `grok`, `google.com` to `gemini`, `qwen.ai` to `qwen`, and `meta.com` to `muse`;
other addresses become `git:<email>`. The trailer name is a model claim. No co-author means
`human`; Muse/omp running Claude appears as `claude`. Claim/review vendor comes from the stored
model snapshot: Claude, GPT/Codex/ChatGPT, Kimi, Grok, Gemini, or Qwen; unknown models fall back
to harness. `cli` and `human` claims remain human even when a model was inherited.

Review JSON stores per-task hand links in each linked commit's `manual_links` array. Each receipt
has `repo_key`, `oid`, `task`, `entry`, `seq`, and `linked_by`. `linked_by` contains the linker's
`{user, host, harness, session}` identity, or `null` when historical attribution is unknown. `seq`
is the nullable durable link-event sequence. These receipts remain visible when `HARNESS` filters
review entries.
