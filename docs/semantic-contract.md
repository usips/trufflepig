# Optional semantic retrieval

## Executable admission gate

`--sem` explicitly requests the semantic lane. Ordinary search does not download
or invoke a model. Missing artifacts produce an actionable unavailable response;
partial embeddings report covered and eligible source-region counts separately.

The candidate is the published `jina-embeddings-v2-base-code` ONNX artifact using
Rust fastembed/ORT on CPU. Admission requires a real small inference, numerical
parity, and resource check. Pin model and tokenizer revisions, use masked mean
pooling, normalize, and retain all 768 `f32` dimensions. This candidate has no
established retrieval advantage on DreamMaker or Luau.
[Publisher configuration and inference example](https://huggingface.co/jinaai/jina-embeddings-v2-base-code)

Record the exact artifact hashes, runtime versions, input cases, tolerances,
embedding dimensions, finite values, norms, observed timings, and peak memory.
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

Embedding input includes bounded source content and stable language/kind context
without source paths. Version the template and key content reuse by model and
tokenizer revisions, pooling, normalization, dimensions, and input bytes. A rename
can reuse content vectors, but still requires source occurrence updates.

Stream exact filtered vector search through a bounded top-k heap. Keep CPU
inference concurrency explicit and bounded. Cache retention is evictable with a
5 GiB default; evicted vectors reduce reported coverage until recomputed. Do not
claim dimension truncation, quantization, ANN, reranking, or GPU execution without
separate implementation and evaluation.
