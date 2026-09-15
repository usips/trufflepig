# Navigation evaluation

The [navigation replay](../evaluation/navigation_replay.py) complements the
[retrieval replay](evaluation-contract.md) with 600-token compact, file-first
responses, at most three result pages including the first search, and eight
total tool calls. Search entries carry a percent-encoded `file` URI, inclusive
line span, and immutable handle; source text is retrieved with `show`.
Its label is **oracle-assisted navigation replay**: relevance labels choose
which returned handles to read. It measures tool navigation mechanisms, not
agent judgment, task success, billed usage, or token savings.

```sh
python3 evaluation/navigation_replay.py --trufflepig target/debug/trufflepig
python3 evaluation/navigation_replay.py --trufflepig target/debug/trufflepig \
  --manifest evaluation/manifests/navigation.json
python3 -m unittest discover -s evaluation -p 'test_*.py'
```

The default uses the existing frozen retrieval corpus. The
[navigation fixtures](../evaluation/manifests/navigation.json) add complementary
source requirements, result pagination, source continuations, and required call
context. Regression tests inject revision changes, changed emitted bytes,
incomplete delivery, gaps in source coverage, and exhausted page/call limits.
These synthetic cases are authored labels, not independently graded evidence.

## Versioned records and workflow

[records.py](../evaluation/records.py) defines runner-neutral, JSON-serializable
version-one task, event, outcome, and usage records. Tasks carry a query,
original-byte relevance spans, snapshot hashes, and an optional `requires_ctx`.
Events preserve emitted identities, original result ranks, operation, typed
status, delivery completeness, coverage, truncation, and measured usage.
Outcomes separate metadata discovery from fully covered source evidence and
required context. A miss or exhausted limit remains an incomplete outcome.

The [navigation-v1 workflow](../evaluation/workflows/navigation-v1.json) defines
the selection, stopping, and accounting contract independently of an agent
runner. Paired development-task trials use the sample and arms in the
[evaluation contract](evaluation-contract.md); this replay remains a
runner-neutral navigation measurement. A runner may populate input-token usage
only from actual observations; the CLI replay leaves it unknown.

## Evidence and costs

A hit overlapping a labeled span counts as metadata discovery. Source evidence
requires the entire labeled byte span to be covered by emitted source lines
from successful, completely delivered responses. Each source response must
verify the selected metadata revision, and its emitted text must match the
frozen source at its original byte coordinates. Multiple reads may jointly
cover a label; repeated or overlapping lines do not fill gaps. Search metadata
and `ctx` relationships do not count as source evidence.

A task requiring context additionally needs a successful, untruncated `ctx`
response with observed relationships for a selected handle. Context is a
mechanical navigation requirement, not a grade of relationship relevance.
Continuations pass their returned target unchanged to `show`; stale revisions
and unavailable reads remain visible failures. Workspace result sets retain
each member owner and publication generation, so `more` preserves the original
cross-repository file order after the first page.

The harness measures complete captured stdout bytes, separate stderr bytes,
wall time, and tool calls. Pipe capture is delivery to this local runner; it
does not assert delivery to another agent. A timeout preserves partial bytes
and marks delivery incomplete. Setup indexing is outside task usage.

When Python `tiktoken` is available, output tokens count the exact emitted UTF-8
with `o200k_base`, including framing. Without it, token counts are null and token
costs are censored; byte counts are never converted into estimated tokens.
Tokens to first evidence and complete evidence are cumulative observed output
costs. Misses retain spent cost and null completion cost. Incomplete delivery,
unknown tokens, and incomplete navigation retain explicit censoring.
