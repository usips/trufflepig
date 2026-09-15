# Semantic inference gate evidence

The optional `semantic` feature uses the pinned publisher ONNX and tokenizer
assets in [model.json](model.json), and the optional reranker uses the pinned
assets in [reranker.json](reranker.json). Asset hashes are checked before model loading.
The engine uses masked mean pooling, L2 normalization and all 768 `f32` dimensions.
Each input accepts at most 4096 model tokens; a call accepts at most 8 inputs and
8192 padded tokens. Excessive inputs fail explicitly instead of being truncated.

Install the manifest assets preserving their relative paths, and provide a
compatible ONNX Runtime shared library using `ORT_DYLIB_PATH`. Run:

```sh
cargo run --features semantic -- semantic-check /path/to/model
```

The check embeds the two inputs in the [publisher example](https://huggingface.co/jinaai/jina-embeddings-v2-base-code/blob/516f4baf13dec4ddddda8631e019b5737c8bc250/README.md).
Their cosine must differ from `0.7281748759529421` by at most `0.002`.
The JSON report includes load time, total time and process peak RSS. The small
input gate rejects peak RSS over 4 GiB. Model initialization also runs parity
before serving requests. This check establishes executable CPU parity only;
it does not establish retrieval quality, long-input limits, or corpus latency.

## CUDA evidence

The [CUDA result](cuda_result.json) records the numerical gate, bounded 4090
benchmark, profiler presence, and admission outcomes. The initial short-input
gate passes publisher CPU/CUDA parity (`0.7281747460365295` and
`0.7281746864318848`) and cached/batch comparisons above `0.999`.

The final validation uses a 10 GiB ORT arena, 4096-token inputs, 8192 padded
tokens, and batches of at most 8. It processes 256 distinct source regions of
39--512 bytes and measures 602.352941 items/s on CUDA versus 10.787577 on CPU
(55.837647x); sampled process GPU memory peaks at 7.59 GiB. The mixed five-input
shape `[4096,1024,1024,1024,1024]` is greedily split into `[4096,1024]` and
`[1024,1024,1024]`, with each call bounded at 8192 padded tokens. The external
`trufflepig-worker.nsys-rep` trace is present and records 1185 CUDA kernels.

These measurements cover the listed short source regions and supply no
performance estimate for a full 4096-byte corpus. The development retrieval
replay gives CUDA Recall@10 `0.333333` versus `0.458333` for
`newfilefirstlexical`. Concurrent latency has p95 `6.08325262699509` seconds
against a 5-second limit; serial p95 is `1.4897110809979495` seconds but has 6
nonready responses. Default enable therefore fails and semantic remains opt-in.

Set `TRUFFLEPIG_MODEL_DIR` to the verified model directory for `--sem` queries.
The path-independent embedding cache keys include source text, model revision,
and the input/pooling version. Its SQLite page budget is 5 GiB, with conservative
entry limits and least-recently-used eviction in batches. The authoritative
index determines which cached content belongs to a search snapshot.
