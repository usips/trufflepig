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
```

The optional preload remains loaded until the model is released. A CUDA
provider failure remains visible; provider registration never silently falls
back to CPU.

`semantic-check` verifies pinned assets, publisher parity, and normalized
768-value outputs for the provider it actually runs. A CUDA result is admitted
only after a gate run records an actual CUDA execution trace. Until that run
exists, this repository makes no CUDA performance or retrieval-quality claim.
The existing [CPU result](../evaluation/semantic_gate/cpu_result.json) covers
two publisher inputs and establishes executable CPU parity only.

The [asset manifest](../evaluation/semantic_gate/model.json) pins the model and
tokenizer revisions. Use masked mean pooling, L2 normalization, and all 768
`f32` dimensions. Do not silently substitute another model or tokenizer.
[Publisher configuration and inference example](https://huggingface.co/jinaai/jina-embeddings-v2-base-code)

## Preparation and cache

Each canonical root owns preparation state and an evictable embedding cache.
Background preparation captures one published index generation and its source
regions, computes content keys, and submits only missing keys to the shared
worker. A content key includes model and tokenizer revisions, pooling,
normalization, dimensions, input version, and source bytes. A rename can reuse a
content vector while source occurrences are updated separately.

Preparation resumes after root-daemon restarts and retries transient worker
failures. An explicit `semantic prepare` also retries failed content; ordinary
search does not repeatedly reopen failed runs.

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
different source occurrences. No dimension truncation, quantization, ANN,
reranking, or machine-wide hard memory cap is part of this contract.

`--no-daemon` never starts or contacts the inference worker. An explicit
foreground preparation command acquires the same root preparation lease and
performs the work locally. Foreground CUDA requires `CUDA_VISIBLE_DEVICES` to
contain only the configured GPU UUID; the shared worker sets this mask itself.
See [CLI usage](cli.md) for preparation and worker
status commands.

## Worker lifecycle and diagnostics

`semantic worker status` reports whether the per-user worker is running, its
selected provider and GPU identity, whether the model is loaded, and pending
work. `semantic worker stop` asks it to release its model and exit. The worker
may unload an idle model; status and diagnostics report residency without
attributing whole-process RSS solely to model tensors.

Doctor probes inspect cached vector dimensions, finite normalized values, and
model/input provenance without starting inference. Cached vectors from another
pinned revision are unverified for the current model, even when their content
bytes remain reusable.
