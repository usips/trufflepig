# Licensing notes retained for owner review

These excerpts retain the existing declarations verbatim. Josh owns their review;
they do not define retrieval architecture or establish model suitability. The
CodeRankEmbed declaration and model-family distinctions require owner review.
The supported design contracts are linked from [the overview](DESIGN.md).

```text
- Sidecar (opt-in): SpacemanDMM's `dmdoc` JSON for full-fidelity object tree + doc comments
  (`///`, `/**`, `//!`, `/*!`). Never link the `dreammaker` crate (GPL-3); run it as a process.
```

```text
| Embedder (fast) | `nomic-ai/CodeRankEmbed` 768-d | `EmbeddingGemma-300M`, `Qwen3-Embedding-0.6B` | needs its query prefix; Apache |
```

```text
`jina-code-embeddings-1.5b` is strong but CC-BY-NC: allowed as a user-configured model, never a
shipped default. Model id + dims + quant + template version live in the vector file header and the
embedding-cache path; the daemon refuses to mix.
```

```text
- **SpacemanDMM is GPL-3.** `dmdoc` JSON via subprocess only. `tree-sitter-dm` is WIP; expect to
  contribute fixes upstream and vendor a pinned commit.
```

```text
### 14.16 Licensing & distribution

- **SpacemanDMM (`dreammaker` crate) is GPL-3** — subprocess only. `tree-sitter-dm`: check the
  license file of the pinned commit before vendoring.
- **jina-code-embeddings** is CC-BY-NC; **nomic**, **Qwen3**, **EmbeddingGemma** (Gemma terms —
  read them), **bge** are permissive-ish. Ship model *choices*, not model weights; download on
  `init`.
- **ONNX Runtime binaries** are MIT but the CUDA EP pulls NVIDIA libraries with their own terms.
- **Vendored grammars** each carry a license; bundle a `THIRD_PARTY.md`.
- **Static linking** of SQLite (public domain), tantivy (MIT), usearch (Apache-2) is fine; keep the
  dependency tree auditable with `cargo deny`.
```

