# Evaluation and acceptance

## Frozen retrieval cases

[evaluation/manifests/fixtures.json](../evaluation/manifests/fixtures.json) pins
every source file by SHA-256 and labels relevant existing byte spans. The
[replay harness](../evaluation/replay.py) verifies hashes before and after runs;
a changed corpus fails instead of quietly updating relevance. Labels are
authored regression cases, not independent human grades or held-out evidence.

Run the ordinary `rg`/read baseline before ranking changes:

```sh
python3 evaluation/replay.py
python3 evaluation/replay.py --trufflepig target/debug/trufflepig
```

The second command compares both retrieval systems on identical frozen bytes,
using a ten-hit, 10,000-token Trufflepig cap to isolate retrieval from default
response packing. Default-budget behavior has separate contract regressions.
The baseline performs the declared literal/regex search and reads a bounded
current-file excerpt from the first match. It is a reproducible tool replay,
not a substitute for a capable agent control. Search protocol bytes and read
bytes are reported as bytes; neither is labeled another model's token count.
Reports go to stdout and are not committed as claims of agent success.

## External corpus splits

[evaluation/manifests/corpora.json](../evaluation/manifests/corpora.json) reserves
`lunatic` and `tgstation` for development, and `tales-from-space`, `Baystation12`,
and `CEV-Eris` for held-out tasks. Rust/TypeScript held-out subsystems and time
periods lack independent-repository coverage and must be reported as such.
Reserved corpora have no fabricated task IDs, commits, or relevance labels.

For an admitted task, freeze a parent commit and source-file hashes before
searching. Commit titles and changed files can suggest tasks and candidate
labels, but changed files are incomplete ground truth. Reviewers validate
relevant existing spans in the parent snapshot; post-change code is excluded.
Group related fixes and query variants within one split to prevent leakage.

Record repository, parent revision, task/group IDs, split, language, intent,
query, labeled spans, label provenance, and exclusion reasons. Freeze evaluation
manifests before ranking tuning. Empty held-out manifests mean no held-out
conclusion is available.

## Metrics and acceptance

Report file recall at 5 and 10, span recall at 10, misses, unavailable responses,
truncated searches, and per-query latency by language and intent. Recall labels
are sets, so repeated matches cannot increase coverage. A miss remains in the
denominator. Token-to-first-relevant-span and tokens-to-recall require a named
tokenizer, complete responses, and explicit treatment of misses.

Contract fixtures must not regress. Assess held-out retrieval recall and agent
task success before interpreting token or call savings. Higher token cost is a
regression: for a permitted relative increase `epsilon`, require
`new_cost <= baseline_cost * (1 + epsilon)`; misses never become free successes.
No arbitrary three-percent non-regression claim follows from twenty tasks.

For agent trials, pair the same capable agent and task snapshot with ordinary
search/read tools versus Trufflepig available. Count all input/output tokens,
tool calls, failures, retries, and missed tasks. Use independent grading of
patch success, paired trials, and confidence intervals. Predeclare sample size
and analysis; report inconclusive results honestly.

## Required behavioral coverage

- Identity: interleaved clients, consecutive searches, restart, expiry, eviction,
  moved/deleted source, and stale graph generations.
- Reads: concurrent writes, traversal, symlink escape, unusual filenames,
  CRLF/BOM/invalid encoding, and original-byte span correctness.
- Output: tiny budgets, complete serialized token counts, stable snapshot ranking,
  and separate index/parser/semantic/truncation completeness.
- Updates: unchanged callers after target edits, configuration/grammar changes,
  failed transactions, interruption, checkout-sized changes, missed watch events,
  and stale embedding completion.
- Languages: definitions and observed-reference recall, valid local resolutions,
  and false-resolution rejection. DreamMaker additionally covers nested/brace
  syntax, overrides, semantic parents, conditional includes, macros, signals,
  recovery, and encoding.

## Resource measurements

Measure cold/warm indexing, edit-to-query visibility, query latency, peak memory,
disk growth, and concurrent agents. Record corpus size, hardware, cache state,
runtime configuration, and excluded files. Parsing feasibility and a small FTS5
transaction check do not establish repository-scale performance. The semantic
lane additionally requires its [inference gate](semantic-contract.md).

The [development-corpus smoke](../evaluation/performance/README.md) records
single-sample debug indexing, memory, disk, coverage, and reconciliation costs.
Those measurements expose current limitations and are not acceptance thresholds
or held-out retrieval results.
