# Optional semantic retrieval

## Executable admission gate

`--sem` explicitly requests the semantic lane. Ordinary search does not download
or invoke a model. Missing artifacts produce an actionable unavailable response;
partial embeddings report successful regions and embedding failures separately.

The implemented candidate is the published `jina-embeddings-v2-base-code` ONNX
artifact using Rust fastembed/ORT on CPU. Admission requires real inference, numerical
parity, and resource check. Pin model and tokenizer revisions, use masked mean
pooling, normalize, and retain all 768 `f32` dimensions. This candidate has no
established retrieval advantage on DreamMaker or Luau.
[Publisher configuration and inference example](https://huggingface.co/jinaai/jina-embeddings-v2-base-code)

The [asset manifest](../evaluation/semantic_gate/model.json) pins hashes, and the
[recorded CPU result](../evaluation/semantic_gate/cpu_result.json) reports the
executed two-input publisher cosine check, tolerance, runtime, timings, and peak
memory. This check passes for those inputs and establishes neither held-out
retrieval quality nor long-input resource bounds.
Compare against published reference inference on the same text and tokenizer
options. A blocked download or missing runtime is an unavailable gate, not a
successful inference test. Do not silently substitute another model.

Pooling and dimension handling are model-specific. CodeRankEmbed's published
pooling configuration uses CLS, and does not establish arbitrary dimension
truncation support.
[Pooling configuration](https://huggingface.co/nomic-ai/CodeRankEmbed/blob/main/1_Pooling/config.json)
Licensing declarations remain [for Josh's review](licensing-notes.md).

## Snapshot and cache contract

Compute the query embedding before opening the index read snapshot. Join stored
vectors to source occurrences from that one snapshot; a completion for stale
source cannot attach itself to a replacement occurrence.

Embedding input contains bounded region source text only, without source paths
or derived symbol context. Version the template and key content reuse by model and
tokenizer revisions, pooling, normalization, dimensions, and input bytes. A rename
can reuse content vectors, but still requires source occurrence updates.

Stream exact filtered vector search through a bounded top-k heap. Keep CPU
inference concurrency explicit and bounded. Cache retention is evictable with a
5 GiB default; evicted vectors reduce reported coverage until recomputed. Do not
claim dimension truncation, quantization, ANN, reranking, or GPU execution without
separate implementation and evaluation.

The engine verifies asset checksums and publisher parity when opened. It uses
two inference threads, one input per batch, and mutable serial access per engine;
inputs exceeding 8,192 model tokens fail explicitly. The daemon retains one engine;
an exclusive per-root inference lease rejects competing processes with
`semantic_busy`. Different roots can load separate engines, so no machine-wide
memory cap is promised.

Query inference precedes the read snapshot. Candidate region embeddings are
computed or reused synchronously while streaming that snapshot. Semantic and
lexical results merge round-robin with stable lane ordering; there is no
background embedding queue. Query coverage reports successful `semantic_regions`,
`semantic_total_regions`, `semantic_failures`, and completely embedded
`semantic_files` under the query filters. The index-only status counter remains
zero because embedding state lives in the evictable content cache.

## Idle residency

The daemon unloads its engine after ten minutes without semantic activity at the
next idle tick. Model destruction precedes release of the inference lease.
Activity is recorded after the request reaches idle processing, so a long request
does not immediately expire its own engine. A later semantic request reloads the
pinned model under the same lease contract.

While loaded, idle processing emits residency observations at thirty-second
intervals to the diagnostic queue. Linux process RSS is measured for the whole
process, not attributed exclusively to model tensors. Busy requests can postpone
sampling and unloading. In-flight ORT cancellation is not implemented.
[Doctor probes](index-contract.md#bounded-diagnostics-probes) inspect persisted
vector dimensions, finite normalized values, and model/input provenance without
starting inference. Cached vectors from another pinned revision are unverified
for the current model, not corruption merely because they remain reusable.
