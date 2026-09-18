# Trufflepig

Trufflepig is a local search engine for coding agents working in large
repositories. Instead of `grep` output that floods the context window, an agent
gets one ranked, token-budgeted page of hits with stable handles, then reads
exactly the source it needs, verified against the bytes on disk. Pre-alpha,
Linux only.

## What an agent gets

- **Bounded answers.** Every response fits a token budget, 600 `o200k_base`
  tokens by default, ranked file-first with one representative hit per file.
  Truncation and partial coverage are reported, never silent. ([CLI](docs/cli.md))
- **Handles instead of re-searching.** Each hit carries an immutable handle.
  `show` returns its source with the revision and a `verified:` footer; `more`
  pages; `ctx` and `refs` follow relationships; `map` outlines a module.
  Handles survive restarts and never drift to a newer query.
  ([retrieval contract](docs/retrieval-contract.md))
- **Structure, not just text.** Symbols, definitions, references, and
  resolved-versus-candidate relationships for Rust, TypeScript/JavaScript,
  Luau, and DreamMaker. Everything else stays text-searchable.
  ([language contract](docs/language-contract.md))
- **History without a checkout.** `hist`, `since`, `diff`, and `blame` over
  local Git objects with exact before/after reads. Needs Git 2.55 or newer.
  ([history contract](docs/history-contract.md))
- **Multi-repo workspaces**, opt-in semantic retrieval and reranking with a
  500 ms deadline and lexical fallback, and a per-session audit of every call.
  ([workspace](docs/workspace-contract.md), [semantic](docs/semantic-contract.md),
  [diagnostics](docs/diagnostics-contract.md))

## What is measured

The design makes no latency, memory, or retrieval-quality guarantee
([design](docs/DESIGN.md)). Current evidence is a development split, not a
held-out result:

- Oracle-assisted navigation replay, 12 tasks: mean file recall at 10 is 0.25
  with the old lexical lane, 0.46 with file-first lexical ranking, and 0.63
  with CUDA retrieval plus rerank; misses fall from 8 to 6 to 4.
  ([result.json](evaluation/gpu_navigation/result.json))
- A paired agent trial exists, but most assisted agents misused the wrapper,
  so it establishes no token-saving or task-success claim.
  ([agent_result.json](evaluation/gpu_navigation/agent_result.json))
- Indexing has a real cost: about 100 s cold for an 11,620-file repository in
  a debug build, with an index 17 to 21 times the source bytes.
  ([performance smoke](evaluation/performance/README.md))

The external Codebase-Memory study (roughly tenfold lower token use, 2.1-fold
fewer calls) motivates this work; it is not a Trufflepig result.

## Get started

```sh
cargo install --path . --locked
trufflepig --root . --no-daemon index
trufflepig --root . search 'sym:TokenBucket'
```

[Get started](docs/cli.md) · [Design and contracts](docs/DESIGN.md) ·
[Agent setup](plugins/trufflepig-agent/README.md) for Codex, Claude Code,
Kimi Code, Muse Code, and omp.

Licensed under [GPLv3 only](LICENSE).
