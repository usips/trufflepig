# GPU navigation development benchmark

`manifest.json` freezes twelve natural-language navigation tasks: four Rust
tasks in Lunatic, four Luau tasks in Tales from Space, and four Dream Maker
tasks in tgstation. It is a development split. The manifest makes no held-out
claim.

Each task records the parent revision, selected source-file SHA-256 values, and
byte spans read from that parent snapshot. The repository checkout is the
corpus; the harness never copies or archives it. A changed selected file makes
grading fail closed.

The solver receives `solver_task(task)`, which contains the prompt, corpus,
language, and retrieval query. It does not contain labels, paths, revisions,
spans, or hashes. The grader retains those fields and credits source evidence
only from complete `show` responses whose emitted original bytes match the
frozen source.

The checked-in [solver prompt bundle](solver_prompts.json) is the public task
input for agents. Give agents that file, while retaining
`manifest.json` for the grader.

The paired trial contract is twelve tool calls, at most 12,000 emitted output
tokens, and 600 seconds per task. Input-token usage is recorded only when an
agent adapter observed it. Missing tokenizer support leaves emitted-token
costs unknown; bytes are not converted to token estimates.

For an adapter that invokes one tool subprocess per call, use the persistent
budget wrapper. It requires `tiktoken` and records raw stdout/stderr in the
JSONL log while delivering at most 900 `o200k_base` tokens per call:

```sh
PYTHONPATH=evaluation python3 -m gpu_navigation.trial_tool \
  --state STATE.json --log CALLS.jsonl -- command arg
```

The wrapper counts stdout and stderr together. It stops after twelve calls,
12,000 delivered output tokens, or 600 seconds. A clipped response is logged
as `truncated`, consumes the call and delivered tokens, and lets the agent
continue. A raw stream over the 1 MiB
capture ceiling is terminated and logged as `output_limit`.

Retrieval arms are `oldlexical`, `newfilefirstlexical`, `cuda`, and
`cuda_rerank`. The old arm points at the operator's existing
`~/.cargo/bin/trufflepig`; the other arms point at the build selected by the
operator, with `cuda_rerank` additionally requiring the reranker asset and
adding `--rerank` to `cuda`'s flags. Measured outcomes live separately in
[result.json](result.json). Solver arms are paired as `ordinarytools` and
`ordinarytools+trufflepig`.

An adapter writes a version-two trace with this shape:

```json
{
  "schema_version": 2,
  "workflow_version": "gpu-navigation-v1",
  "trials": [
    {
      "task_id": "…",
      "solver_arm": "ordinarytools",
      "retrieval_arm": "oldlexical",
      "events": [
        {"operation": "search", "response": {},
         "complete_delivery": true,
         "usage": {"tool_calls": 1, "stdout_bytes": 0,
                    "stderr_bytes": 0, "elapsed_seconds": 0.1,
                    "emitted_tokens": 10}}
      ]
    }
  ]
}
```

Grade a captured trace with:

```sh
PYTHONPATH=evaluation python3 -m gpu_navigation.replay TRACE.json
```

The command uses the adjacent manifest by default; `--manifest PATH` selects
another frozen task set. Capture the four retrieval arms with:

```sh
PYTHONPATH=evaluation python3 -m gpu_navigation.run_retrieval \
  --workspace WORKSPACE.toml --old-binary OLD --new-binary NEW \
  --old-cache OLD_CACHE --new-cache NEW_CACHE \
  --inference-config INFERENCE.toml \
  --arms oldlexical,newfilefirstlexical,cuda,cuda_rerank
```

Prepare the CUDA cache, and the reranker for `cuda_rerank`, before capturing
those arms. The runner records first-page file recall separately from
original bytes delivered by subsequent `show` calls. Each task searches with its
own corpus as `--root`, so a workspace home never hides another corpus's
files. Every call sets `TRUFFLEPIG_SYSTEM_DIR` to `<cache>/router`, so
daemons start from the measured binary rather than an installed per-user
router.

## Measured readiness

The checked-in [result.json](result.json) contains historical, preliminary
captures at measured commit `3835a69`. That runner queried every task from the
first workspace member (Lunatic) and could use an installed per-user router,
so Tales from Space and tgstation tasks could only miss. Its Recall@5 /
Recall@10 means are 0.25 / 0.25 for old lexical, 0.2917 / 0.4583 for
file-first lexical, and 0.3333 / 0.3333 for CUDA fusion. They are not results
from the corrected runner, and corrected-root results for these twelve tasks
are not checked in.

The checked-in runner selects each task's corpus root, computes per-task file
Recall@5 and Recall@10 from first-page hits, and averages those recalls across
valid tasks. Its per-arm output artifacts retain raw task events and `show`
byte validation and record the measured binary SHA-256. The separate
`cuda_rerank` figures under `rerank_measurement` in [result.json](result.json)
are also historical: the artifact identifies an uncommitted rerank tree based
on `6c50a74` and records hashes for the measured binaries. In that session,
file-first lexical scored 0.2917 / 0.5417 and CUDA fusion 0.3333 / 0.3333 for
Recall@5 / Recall@10; reranking scored 0.5417 / 0.625, with four Recall@10
misses versus five and eight. Workspace reranking processes three members
sequentially; that session reports mean search latency rising from about 1.1 s
to about 2.7 s at the 600-token page.

Prepared search p95 is 1.49 seconds across 100 serial requests, with six
responses reporting incomplete semantic readiness. Six concurrent clients over
20 waves give p95 6.08 seconds, above the five-second gate. No request errors
or responses above the 600-token budget occur in these measurements.

Run the latency measurement after preparation, without competing benchmarks:

```sh
PYTHONPATH=evaluation python3 -m gpu_navigation.measure_latency \
  --helper /absolute/path/to/configured-trufflepig --output latency.json
```

The helper selects the workspace, cache, runtime configuration, and semantic
opt-in. Twelve warmup requests are excluded; raw responses retain readiness
and output accounting. A latency threshold alone cannot pass a readiness gate.

## Concept-audit manifest

[`../manifests/concept-audit.json`](../manifests/concept-audit.json) freezes
twenty Lunatic tasks drawn from agent-transcript concept-search misses and
their review: bare identifiers (`molar_mass`, `InteractionRefused`,
`awake_only`, `tick_rate`), a plain and a quoted literal message (`unknown item
slot`), `outside an occurrence` labelled with its seventeen production refusal
sites, docs named by file (`AGENTS.md`, `dev-http.md`), a qualified
`RecordStore::new`, and multi-word concept queries, with controls that
already ranked well. Audit misses for `ModOwner` and the handler-route default
are absent: neither exists at the frozen revision. It is development data with
the same schema; pass it with `--manifest`. Its snapshot hashes match Lunatic
`29709d66`, so run it against a checkout or `git archive` of that revision.

An earlier, separate one-session ranking comparison on this manifest recorded
top-1 hits of 12/20 for the old ranking and 18/20 for the updated ranking.
Treat those counts as session-local evidence: no per-task artifact for that
comparison is checked in, and the retrieval runner reports first-page file
Recall@5/10 rather than MRR or top-1. CUDA and rerank were not measured for
that comparison.

## Real-agent trials

Twelve Luna xhigh agents per arm attempted the navigation tasks. Separate Astra
medium graders scored answer correctness against source. The unblinded,
development-only scores are 31/48 for ordinary tools and 18/48 for the assisted
arm; [agent_result.json](agent_result.json) preserves per-task outcomes and costs.

These scores cannot isolate the GPU search effect. Only two assisted agents
successfully searched through the prescribed frozen CUDA helper. Another used
a different binary invocation; most encountered command or wrapper errors and
fell back to ordinary tools. Two answered the wrong corpus. Lower output costs
therefore do not establish token savings. Rejected extra attempts are retained,
and the original over-budget B05 is replaced by a fresh B05R after the ledger
locking fix. Full model token usage remains unknown.
