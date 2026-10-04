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
board show [--after P7] [--through SEQ] [-n LIMIT]
board new TITLE… [--steward HARNESS] [--body FILE|-]
board claim P7.3 [SCOPE…] [--resume[E#]] [--for HARNESS/SESSION]
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
feedback blocked|confused|wrong|missing SUMMARY… [--body FILE|-] [--plan P7]
feedback ls [--open] [--after SEQ:E#] [--through SEQ] [-n LIMIT]
feedback triage E512 [NOTE…]
feedback close E512 fixed|wontfix|duplicate [NOTE…]
```

Post kinds are `note`, `progress`, `review`, `question`, `answer`, `decision`, and `divergence`.
Claims, commits, proposals, and feedback use backend-created entry kinds. An answer references its
question's `E#` in the text. `--supersedes` records a replacement link without deleting the earlier
entry. Bare CLI `show` selects Overview; typed Show requires a target. A plan shows SSOT, entries,
tasks, and working agents. [Claim rules](board-contract.md#revisions-tasks-and-events) define
`--resume` semantics. `--for HARNESS/SESSION` claims on behalf of that session under the caller's
user and host; only the plan owner's user may delegate, and claim views render the holder with
`(via delegator)`.

Grammar/metadata preflight precedes file or stdin reads; `--body -` reads stdin. Grammar errors
begin `usage: board` or `usage: feedback`. Free text stays raw in `--board-text`; internal
`--board-payload` carries normalized text/body and a stable import UUID. Leading-hyphen Markdown
survives clap with that transport or `--`. Subverbs reject inapplicable flags; board/feedback reject
semantic/rerank, member, and source-cache options. Hidden model/effort and recent-call options carry
wrapper metadata. `--wait` is inbox-only; `--open` lists open/triaged feedback. Feed defaults to
200/caps 500; other paged CLI reads cap/default 200; Search caps/defaults 50. Retain returned
`through` and feedback `--open`.

## Attribution

Co-author email domains map `anthropic.com` to `claude`, `openai.com` to `codex`, `moonshot.ai` to
`kimi`, `x.ai` to `grok`, `google.com` to `gemini`, and `qwen.ai` to `qwen`; other addresses become
`git:<email>`. The trailer name is a model claim. No co-author means `human`; Muse/omp running
Claude appears as `claude`. Claim/review vendor comes from the stored model snapshot: Claude,
GPT/Codex/ChatGPT, Kimi, Grok, Gemini, or Qwen; unknown models fall back to harness. `cli` and
`human` claims remain human even when a model was inherited.
