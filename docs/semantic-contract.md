# Optional semantic retrieval

## Opt-in, runtime, and admission

Semantic retrieval is opt-in. Build with `semantic` for CPU inference or
`semantic-cuda` for CUDA inference. A workspace may set `[semantic] enabled =
true`; `--sem` enables the lane for a request and `--no-sem` overrides that
setting. Without opt-in, search remains lexical and structural and does not download,
load, or invoke a model.

A single per-user inference worker owns the model and ONNX Runtime session. It
selects CPU or CUDA from `$HOME/.config/trufflepig/inference.toml` (with model
and runtime paths also accepted from their environment variables). CUDA
configuration selects a GPU by UUID and resolves its current ordinal at worker
startup. The CUDA target is an NVIDIA RTX 4090 with 24 GiB of VRAM. ORT arena
bytes and the 16 GiB measurement target are recorded separately; neither is a
hard total-memory bound for the worker or machine.

The runtime configuration uses absolute paths. `TRUFFLEPIG_INFERENCE_CONFIG`
can select an isolated configuration for evaluation:

```toml
provider = "cuda"
gpu_uuid = "GPU-xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
model_dir = "/absolute/path/to/verified/model"
runtime_library = "/absolute/path/to/libonnxruntime.so"
cuda_preload_library = "/absolute/path/to/libcudnn.so"
arena_bytes = 10737418240
rerank_model_dir = "/absolute/path/to/verified/reranker"
rerank_gpu_uuid = "GPU-yyyyyyyy-yyyy-yyyy-yyyy-yyyyyyyyyyyy"
```

The optional preload remains loaded until the model is released. A CUDA
provider failure remains visible; provider registration never silently falls
back to CPU.

`semantic-check` verifies pinned assets, publisher parity, and normalized
768-value outputs for the provider it actually runs. A CUDA result is admitted
as executable provider evidence only when the gate also records an actual CUDA
execution trace. The checked-in
[CUDA evidence](../evaluation/semantic_gate/cuda_result.json) records the
initial short-input CPU/CUDA parity, a final 4090 validation at 4,096 input
tokens, 8,192 padded tokens, 8 inputs, and a 10 GiB ORT arena, plus a trace with
1,185 CUDA kernel launches. Its bounded 256-region sample reaches 55.837647x
the measured CPU throughput and samples 7.59 GiB of process GPU memory. These
figures cover 39--512-byte regions and provide no performance estimate for a
full 4,096-byte corpus. The existing [CPU result](../evaluation/semantic_gate/cpu_result.json)
covers two publisher inputs and establishes executable CPU parity only.

The development retrieval replay reports CUDA mean file Recall@10 `0.333333`
against `0.458333` for `newfilefirstlexical`. The concurrent latency probe
reports p95 `6.08325262699509` seconds against a 5-second limit; serial p95 is
`1.4897110809979495` seconds but includes 6 nonready responses. The default
enable check therefore fails, and semantic retrieval remains opt-in.

The [asset manifest](../evaluation/semantic_gate/model.json) pins the model and
tokenizer revisions; the reranker's assets are pinned separately in
[`evaluation/semantic_gate/reranker.json`](../evaluation/semantic_gate/reranker.json).
Use masked mean pooling, L2 normalization, and all 768
`f32` dimensions. Do not silently substitute another model or tokenizer.
[Publisher configuration and inference example](https://huggingface.co/jinaai/jina-embeddings-v2-base-code)

## Optional reranking

Reranking is opt-in per request via `--rerank` / `--no-rerank`, or a
workspace's persistent `[semantic] rerank = true`; see [CLI usage](cli.md).
The pinned cross-encoder is `rozgo/bge-reranker-v2-m3`, an ONNX export of the
Apache-2.0 BAAI weights; its manifest is
[`evaluation/semantic_gate/reranker.json`](../evaluation/semantic_gate/reranker.json).
Licensing review for these weights is tracked separately and is not part of
this contract.

The shared per-user worker owns the reranker alongside the embedding model.
`rerank_model_dir` selects its verified asset directory; an optional
`rerank_gpu_uuid` names a second GPU for CUDA, and the worker's
`CUDA_VISIBLE_DEVICES` mask lists both the embedding and rerank GPU UUIDs.
Without `rerank_gpu_uuid`, the reranker shares `gpu_uuid`.

Each rerank request scores at most 32 documents against one query, with a
4,096-byte bound per document and per query, truncating tokenized pairs at
1,024 tokens, in batches of 8, under a 1,500 ms query deadline. The stage
reorders only the top 32 files from the fused lexical/semantic ranking:
scored hits sort by descending rerank score, with ties breaking by fused
rank; hits the reranker could not score keep their fused order below every
scored hit.

A timeout, missing model, or provider failure keeps the fused order
unchanged and reports `rerank_status` (`ready`, `unavailable`, or `skipped`)
and, when unavailable, `rerank_reason` in coverage, along with
`rerank_window` counting scored hits. `--no-daemon` never reranks, since it
never contacts the inference worker.

## Preparation and cache

Each canonical root owns preparation state and an evictable embedding cache.
Background preparation captures one published index generation and its source
regions, computes content keys, and submits only missing keys to the shared
worker. A content key includes model and tokenizer revisions, pooling,
normalization, dimensions, input version, and source bytes. A rename can reuse a
content vector while source occurrences are updated separately.

Preparation resumes after root-daemon restarts and retries transient worker
failures. An explicit `semantic prepare` reconciles terminal `completed` and
`capacity` runs against the current cache and retries failed content. Ordinary
scheduling does not reopen terminal runs or repeatedly retry failed content.

Source-region retrieval is cache-only. Search never embeds candidate regions on
the query path. Query preparation may obtain one query vector from the worker;
the query vector is computed before opening the index read snapshot. Stored
vectors join occurrences from that one snapshot, so a completion for stale
source cannot attach to a replacement occurrence.

Every actual inference call accepts at most 8 inputs and 8,192 padded tokens;
each input is bounded at 4,096 model tokens. Excessive inputs fail explicitly.
Worker admission also bounds queued input bytes and serialized requests.
Semantic retrieval has a 500 ms query deadline. A timeout, pending vector,
missing asset, provider failure, or unavailable worker returns lexical and
structural results with an explicit semantic status and coverage issue.

The embedding cache has a 5 GiB default SQLite page budget and evicts by
recency. Eviction lowers semantic coverage until preparation recomputes the
vector. Cache state is keyed by the root and content identity; it never merges
different source occurrences. No dimension truncation, quantization, ANN, or
machine-wide hard memory cap is part of this contract; the [rerank
stage](#optional-reranking) below is opt-in and separate from embedding
preparation.

`--no-daemon` never starts or contacts the inference worker. An explicit
foreground preparation command acquires the same root preparation lease and
performs the work locally. Foreground CUDA requires `CUDA_VISIBLE_DEVICES` to
equal the comma-joined `gpu_uuid` and `rerank_gpu_uuid` list; the shared worker
sets this mask itself.
See [CLI usage](cli.md) for preparation and worker
status commands.

## Worker lifecycle and diagnostics

`semantic worker status` reports whether the per-user worker is running, its
selected provider and GPU identity, whether the model is loaded, and pending
work. `semantic worker stop` asks it to release its model and exit. The worker
may unload an idle model; status and diagnostics report residency without
attributing whole-process RSS solely to model tensors.

After a model-load failure, the worker retries lazily after 500 ms, doubles the
cooldown after each failure, and caps it at 30 seconds. The latest load error
remains visible while the cooldown is active; a successful load clears it.

Doctor probes inspect cached vector dimensions, finite normalized values, and
model/input provenance without starting inference. Cached vectors from another
pinned revision are unverified for the current model, even when their content
bytes remain reusable.
