# CPU semantic inference gate

The optional `semantic` feature uses the pinned publisher ONNX and tokenizer
assets in [model.json](model.json). Asset hashes are checked before model loading.
The engine uses masked mean pooling, L2 normalization and all 768 `f32` dimensions.
It accepts at most 8192 model tokens and reports excessive inputs instead of
silently truncating them. Inference uses two CPU threads and one item per batch.

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

Set `TRUFFLEPIG_MODEL_DIR` to the verified model directory for `--sem` queries.
The path-independent embedding cache keys include source text, model revision,
and the input/pooling version. Its SQLite page budget is 5 GiB, with conservative
entry limits and least-recently-used eviction in batches. The authoritative
index determines which cached content belongs to a search snapshot.
