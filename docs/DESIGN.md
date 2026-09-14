# trufflepig — DESIGN.md

> An agentic code search engine. Agents `trufflepig <thing>` instead of `grep` + `tree` + `cat`,
> and get a ranked, token-budgeted list of `path:start-end  kind  signature` they can act on.

Status: design draft (2026-09). Rust. Single binary + per-repo daemon. Targets Rust, TypeScript,
Luau, DreamMaker first; anything tree-sitter can parse second.

---

## 0. One-paragraph thesis

Coding agents burn most of their tokens *finding* code, not editing it. Every `cat` of a 900-line
file to read a 40-line function is waste; every `grep` returning 300 lines of noise is waste; every
`tree` of a 4,000-file repo is waste. trufflepig maintains a hot, always-fresh index of a project
(symbols, references, lexical, semantic, graph) and answers one question well: **given this thing
I'm looking for, which spans of which files should I read, in what order, and why?** The answer is
formatted for an LLM's token budget, not a human's screen. The 2026 evidence says indexed retrieval
buys ~10x fewer tokens and ~2x fewer tool calls at a modest accuracy cost versus free exploration;
trufflepig exists to keep the savings and close the accuracy gap by *aiming reads* rather than
replacing them.

## 1. Non-goals

- Not an editor, not a refactoring engine (Serena-style symbol editing is out of scope).
- Not a type checker. Cross-file resolution is heuristic-first, with optional LSP/SCIP enrichment.
- Not a cloud service. No code egress, ever. No telemetry.
- Not a chat/RAG answerer. It returns locations, signatures, and edges — never prose summaries
  (a summary is a hallucination surface and costs the same tokens as the code).
- Not a general document search. Docs are indexed only insofar as they point at code.

## 2. Design principles (ordered; earlier wins conflicts)

1. **Output tokens are the product.** Every byte emitted must change what the agent does next.
2. **Never block a query on the GPU.** Structural/lexical results first; semantic results merge in
   when ready and are labelled when they're stale.
3. **Ranges, never lines. IDs, never re-typed paths.** Every hit is `path:start-end` plus a short id
   that later commands accept.
4. **Content-hash everything.** Files, symbols, chunks, embeddings, query packs. Identical bytes are
   never processed twice — across files, across worktrees, across time.
5. **A language is a data pack, not a code path.** Adding a language = adding query files + a
   small resolver. The core never special-cases a language.
6. **Deterministic output.** Same index + same query = byte-identical output. Agents get confused by
   result shuffling more than by imperfect ranking.
7. **Degrade, don't fail.** Partial parses, missing models, no GPU, no git — all yield *some* answer.
8. **Local only. Untrusted repo.** Repo contents are data, never instructions or executables.

---

## 3. The agent contract

### 3.1 CLI surface

Everything an agent does fits in six verbs. Flags are short because the agent pays for them.

```
trufflepig <query...>                 # the main verb; flags optional
trufflepig show <id|path[:a-b]>       # print just that span, line-numbered. Replaces cat.
trufflepig ctx <id|sym>               # signature + doc + callers + callees + types used + mentions
trufflepig refs <sym> [--group file]  # who references this symbol; grouped/ranked by file
trufflepig map [path] [-b N]          # PageRank'd repo map, budgeted. For session-start prompts.
trufflepig more                       # next page of the last result set (per repo, TTL'd)
```

Housekeeping (not for agents): `init`, `index`, `status`, `serve`, `stop`, `doctor`, `eval`, `mcp`.

Query prefixes (all optional; the engine infers intent without them):

```
sym:parse_header      exact symbol name (case-sensitive)
re:'fn \w+_header'    regex over file contents (ripgrep engine)
refs:TokenBucket      references to a symbol
callers:handle_upgrade / callees:handle_upgrade
file:src/net/         path prefix filter        lang:dm  kind:struct  in:comments  in:docs
```

Global flags: `-b/--budget <tokens>` (default 600), `-n <max hits>`, `--json`, `--no-sem`,
`--think` (HyDE expansion), `--cwd` (path filter base), `--stale-ok`.

### 3.2 Output grammar (the load-bearing part)

```
<header>   := 'trufflepig' SP <query> SP 'budget:' N SP 'idx:' ('fresh'|'warming'|'stale') NL
<hit>      := '#' id SP path ':' start '-' end SP kind SP signature SP '[' tags ']' NL
              ( SP{4} note NL )*                  ; 0..k context lines, rank-dependent
              ( SP{4} lineno SP{2} code NL )*
<trailer>  := '…+' N SP '(' 'trufflepig more' ')' NL
tags       := (sem|lex|sym|ref|re|doc|hub|test|gen|~)  ; '~' = span may be stale
note       := '→ see ' path[:line]  |  '×' N ' defs'  |  'ref←' id  |  doc first line
```

Rules:

- Paths are repo-root-relative, `/`-separated, never absolute. Shared prefixes across consecutive
  hits are elided as `…/ws.rs` only when unambiguous within the result set.
- `signature` is the *declaration line*, normalized: collapse whitespace, strip `pub(crate)` noise
  to `pub`, truncate to 96 chars with `…`. Never the matched line.
- Line numbers are 1-based and computed on raw bytes (CRLF counts as one line) so they agree with
  `sed -n 'a,bp'` and with the agent's edit tools.
- Rank 1 gets up to 8 context lines; ranks 2–3 get up to 3; the rest get 0–1. Context lines are the
  matched region, not the span head, unless the match *is* the signature.
- `#id` is two or three base32 chars, unique within the result set, stable across `more` pages.
  Result sets live in the daemon for 10 minutes, keyed by repo root; `show a1` resolves against the
  most recent set. Multiple concurrent agents get disjoint id prefixes (`a1..`, `b1..`) by client.
- No ANSI escapes unless stdout is a TTY. No spinners, no progress on stdout ever.
- Exit 0 on "no hits" (agents treat non-zero as tool failure and retry blindly). Exit 2 on usage
  error, 3 on index unavailable.
- stderr carries at most one line (`warming: 12,340/40,102 chunks embedded`).
- `--json` emits one object per line (NDJSON), same fields, for scripts.

### 3.3 Budgeting

- Token estimate = `tiktoken-rs` o200k count × 1.15 safety factor (Claude's tokenizer is not
  public; o200k lands within ~10–15% on code). Cache counts per emitted line.
- Fill by rank: header, then hits with their context allocation, then the trailer. If a hit's
  one-liner alone doesn't fit, stop and emit the trailer.
- Budget is per *response*, not per query: `more` gets a fresh budget.
- `show` has its own cap (default 200 lines; `-b` overrides) and prints `…` markers with the
  omitted range so the agent can `show path:a-b` for the rest.

### 3.4 `show` format

```
src/net/ws.rs:212-260  fn handle_upgrade  (hash ok)
212│ pub async fn handle_upgrade(&mut self, req: Request) -> Result<Response, Error> {
213│     if req.headers.contains("upgrade") {
```

`│` after the line number, single space, raw code (tabs preserved). This format never changes;
agents learn it once. If the file's hash differs from the index, the live file is printed and the
header says `(changed since index; lines may have moved)`.

---

## 4. Architecture

```
            ┌────────────────────────── trufflepig (CLI, thin client) ──────────────────────────┐
            │  parse args → connect unix socket → send Query → stream Response → format/exit    │
            └───────────────────────────────────┬───────────────────────────────────────────────┘
                                                │ $XDG_RUNTIME_DIR/trufflepig/<root-hash>.sock
┌───────────────────────────────────────────────▼──────────────────────────────────────────────┐
│ trufflepigd (per repo root)                                                                  │
│                                                                                              │
│  Watcher ──▶ Reconciler ──▶ Parse workers (rayon, tree-sitter) ──▶ Extract (query packs)     │
│  (notify)    (git+stat)          │                                     │                     │
│                                  ▼                                     ▼                     │
│                          FileTable ◀──────────── SymbolTable ◀── EdgeTable ◀── Mentions      │
│                               (SQLite, WAL)                                                  │
│                                  │                     │                   │                 │
│                                  ▼                     ▼                   ▼                 │
│                            tantivy (BM25)      vectors.i8 (mmap)      petgraph (RAM)          │
│                                                    ▲                   PageRank               │
│                          Embed queue (GPU, background, priority-aware) ─┘                     │
│                                                                                              │
│  Query planner ──▶ [sym | lex | rg | vec | graph] ──▶ RRF ──▶ rerank(top50) ──▶ boosts ──▶   │
│                                                        budget packer ──▶ result set cache    │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

### 4.1 Components

| Component | Responsibility | Crates |
|---|---|---|
| Walker | enumerate files honoring ignore rules; detect binary/minified/oversized | `ignore`, `content_inspector` |
| Hasher | blake3 of file bytes; blake3 of normalized chunk text | `blake3` |
| Watcher | fs events → debounced batches → reconciler | `notify`, `notify-debouncer-full` |
| Reconciler | on start / on batch: diff FileTable vs disk (stat fast-path, hash slow-path, git hint) | `gix` or `git2` |
| Parser pool | one `tree_sitter::Parser` per thread; language by extension/shebang/`.dme` membership | `tree-sitter`, grammar crates |
| Extractor | run query pack `.scm` files; produce Symbols, Refs, Imports, Comments | `tree-sitter` queries |
| Resolver | per-language import/type-path resolution; produce Edges with confidence | in-tree, per pack |
| Mentions | scan comments/docs for path-like tokens, `see`, backticked identifiers → Edges | `regex`, `aho-corasick` |
| Store | files, symbols, edges, chunks, result-set cache | `rusqlite` (bundled) |
| Lexical | BM25 over name/signature/doc/body/path fields with identifier tokenizer | `tantivy` |
| Exact | regex/literal over live files, joined to symbols | `grep-regex`, `grep-searcher` |
| Vectors | int8 MRL-truncated embeddings, mmap'd; brute-force SIMD or HNSW | `memmap2`, `simsimd`, `usearch` |
| Embedder | GPU/CPU local inference; batching; cache | `fastembed` (ort) or `candle` |
| Reranker | cross-encoder over fused top-k | `fastembed::TextRerank` |
| Graph | adjacency in RAM; PageRank; personalized PageRank from query seeds | `petgraph` |
| Planner | intent detection, fan-out, fusion, boosts, packer | in-tree |
| Daemon | socket server, sessions, idle timeout, GPU model lifecycle | `tokio`, `interprocess` |
| CLI | arg parsing, formatting, daemon spawn | `clap`, `tiktoken-rs` |

### 4.2 Index pipeline (per changed file)

1. Watcher event (or reconcile diff) → debounce 75 ms → coalesce per path → batch.
2. Re-stat; if size+mtime match FileTable and file not "racy" (mtime ≥ last index write − 2 s), skip.
3. Read bytes. Binary (NUL in first 8 KiB) → record as asset, stop. Oversized (per-language cap)
   or minified (avg line > 400 chars) → record, lexical-only at most, stop.
4. blake3 → if equal to stored hash, touch stat cache, stop.
5. Decode: UTF-8, else lossy with a flag (DM codebases contain Latin-1). Keep byte offsets.
6. Parse with tree-sitter (timeout 500 ms; on timeout keep the partial tree).
7. Run extraction queries → Symbols (kind, name, fqname, byte span, line span, signature, doc,
   container, visibility, flags{test,generated}), Refs (name, span, enclosing symbol), Imports,
   Comments.
8. Chunk = symbol (plus a file-level "preamble" chunk: imports + top comment + module doc).
   Chunk text = template (see §7.2). Chunk hash = blake3(lang ‖ template_version ‖ text).
9. Single SQLite transaction: replace file's symbols/refs/edges/chunks. Tantivy: delete by file id,
   add docs; commit on a 250 ms timer, not per file.
10. For each chunk hash missing from the embedding cache → enqueue (low priority). Present hashes →
    link existing vector.
11. Mark graph dirty; PageRank recompute on a 2 s debounce.
12. Resolver pass for the file's refs (needs the symbol table of *other* files; runs after the batch).

### 4.3 Query pipeline

1. Parse query: prefixes, flags, free text. Intent = `Identifier` if the text matches
   `^[\w:.$/#]+$` and contains `_`/`::`/`.`/camelCase boundary or exists as a symbol name;
   `Regex` if `re:`; else `Concept`. Mixed (`"TokenBucket refill logic"`) = both.
2. Fan out (all in parallel, each capped at 20 ms; slow lanes are dropped, not awaited):
   - `sym`: exact + prefix + fuzzy(≤1 edit, only if exact empty) over symbol names.
   - `lex`: tantivy BM25 over fields (name×4, signature×2, doc×1.5, body×1, path×1).
   - `re`: ripgrep lane only for `Regex` intent or quoted literals.
   - `vec`: embed query (with model's query prefix) → top 100 by cosine; HyDE adds a 2nd vector.
   - `graph`: seeds = symbols named in the query → personalized PageRank neighborhood.
3. Fuse with Reciprocal Rank Fusion (k = 60), per lane weights by intent
   (Identifier: sym 3, lex 1.5, vec 0.5, graph 1; Concept: vec 2, lex 1.5, sym 1, graph 1).
4. Rerank top 50 with the cross-encoder (query, chunk-text) — skipped for `Identifier` intent when
   the `sym` lane returned an exact hit (it's already right).
5. Boosts (multiplicative, small): global PageRank ×(1+0.3·pr_norm); definition over override
   ×1.2; de-boost `test` ×0.6 unless query mentions test; `gen`/vendored ×0.3; path prior from
   `--cwd` ×1.3.
6. Group: multiple definitions of the same fqname (DM overrides, cfg'd Rust fns, TS overloads)
   collapse to one hit with `×N defs`; `show` on it lists them.
7. Pack to budget (§3.3). Store the full ranked set for `more`/`show`/`ctx`.

### 4.4 Daemon lifecycle

- CLI computes repo root (walk up for `.trufflepig.toml`, else `.git`, else cwd), hashes it, tries
  the socket. Missing/dead → spawn `trufflepigd --root <root>` detached (double fork + setsid,
  stdio → `.trufflepig/daemon.log`), wait ≤ 300 ms for the socket, then query.
- The daemon serves structural queries immediately after a fast reconcile (stat walk), even while
  the full parse/embedding backlog drains. Query threads preempt indexing threads; the GPU queue is
  priority-aware (query embeds jump ahead of backlog batches).
- Idle timeout 30 min (configurable). On exit: flush tantivy, checkpoint WAL, save vector index.
- Version handshake: daemon reports its build hash; a mismatched CLI tells it to exit and respawns.

---

## 5. Storage

```
<root>/.trufflepig/            (add to .gitignore via `init`)
  config.toml                  # per-repo overrides
  index.sqlite (+ -wal, -shm)  # files, symbols, refs, edges, chunks, mentions, sessions
  lex/                         # tantivy index
  vec/<model-id>/vectors.i8    # N×D int8, row = chunk row id; header: model, dims, scale
  vec/<model-id>/hnsw.usearch  # only when N > 200k
  daemon.log
~/.cache/trufflepig/
  models/                      # downloaded ONNX/safetensors + tokenizer.json
  emb/<model-id>/<template-v>/ # global embedding cache: blake3(chunk) → int8 vector (shared across
                               #   repos and worktrees; LMDB or a sharded append-only file)
$XDG_RUNTIME_DIR/trufflepig/<root-hash>.sock
```

### 5.1 SQLite schema (abridged)

```sql
files(id PK, path UNIQUE, lang, size, mtime_ns, blake3, flags, indexed_at, in_dme BOOL);
symbols(id PK, file_id, kind, name, fqname, start_byte, end_byte, start_line, end_line,
        sig, doc, container_id, vis, flags, chunk_id, pagerank REAL);
  INDEX symbols(name); INDEX symbols(fqname); INDEX symbols(file_id);
refs(id PK, file_id, name, start_byte, end_byte, line, enclosing_symbol_id, resolved_symbol_id,
     confidence REAL, kind);          -- kind: call|type|field|import|macro|signal|proc_ref
edges(src_symbol_id, dst_symbol_id, kind, confidence, PRIMARY KEY(src,dst,kind));
mentions(src_file_id, src_symbol_id, dst_file_id, dst_symbol_id, text, line);
chunks(id PK, blake3 UNIQUE, symbol_id, text_len, vec_row INT NULL);
imports(file_id, spec, resolved_file_id, line);
sessions(client, created, json);      -- result sets for `more`/`show`, TTL 10 min
meta(key, value);                     -- schema_version, template_version, model_id, last_commit
```

Pragmas: `journal_mode=WAL`, `synchronous=NORMAL`, `mmap_size=1GiB`, `temp_store=MEMORY`,
`busy_timeout=5000`. One writer (the daemon); CLI never opens the DB directly except `--offline`.

### 5.2 Vector file

Header (64 B): magic, version, model id hash, dims, count, quant scale, template version.
Body: `count × dims` int8, row-aligned to 64 B. Deletions are tombstoned in `chunks.vec_row`;
compaction rewrites when tombstones > 20%. Brute-force search = one pass with `simsimd` dot
product on int8 (≈ 1 ms per 100k×512 on one core; parallelize by row block).

---

## 6. Language packs

A pack is a directory `packs/<lang>/` compiled into the binary (and overridable from
`~/.config/trufflepig/packs/`):

```
pack.toml            # metadata, extensions, caps, keyword stoplist, doc-comment markers
defs.scm             # captures: @def.<kind> @name [@sig] [@doc] [@container]
refs.scm             # @ref.call @ref.type @ref.field @ref.macro ...
imports.scm          # @import.spec [@import.alias]
comments.scm         # @comment @doc_comment
resolver.rs          # tiny: fn resolve_import(spec, from_file) -> Vec<PathBuf>; fn fqname(...)
```

`pack.toml`:

```toml
name = "rust"
extensions = ["rs"]
grammar = "tree-sitter-rust"
max_file_bytes = 2_000_000
keywords = ["fn","let","impl","pub","use","mod","struct","enum","trait","match","if","else"]
doc_comment = ["///", "//!", "/**", "/*!"]
test_markers = ["#[test]", "#[cfg(test)]", "tests/"]
generated_markers = ["@generated", "DO NOT EDIT", "target/"]
signature_stop = ["{", "=", ";", "where"]          # cut the signature line here
```

Capture conventions (enforced at pack load; missing captures are a pack error, not silent):

- `@def.fn @def.struct @def.enum @def.trait @def.impl @def.type @def.const @def.var @def.proc
  @def.verb @def.class @def.interface @def.module @def.macro` with a sibling `@name`.
- `@sig` optional; default = source from node start to first `signature_stop` or newline.
- `@doc` optional; default = contiguous preceding comment lines matching `doc_comment`.
- `@container` optional; default = nearest enclosing `@def.*` by byte span.

### 6.1 Rust

- Grammar: `tree-sitter-rust`. FQName = crate ‖ module path ‖ [impl target ‖] name.
  Module path from file path using `mod.rs`/`foo.rs`/`foo/` rules and `#[path]` when present.
- Resolver: `use` trees expanded (globs recorded as `imports.spec="foo::*"` and resolved lazily by
  name lookup within that module); `pub use` re-exports followed one hop.
- Impl blocks: `impl Foo` / `impl Trait for Foo` become `@def.impl` containers; methods get
  fqname `crate::mod::Foo::method`. The same type's impls across files are linked with a
  `same_type` edge so `ctx Foo` lists all of them.
- Method calls: `x.foo()` resolved by (name, arity) against methods of *any* type, confidence
  1/candidates; if the receiver's type is a local `let x: T` or a field with a declared type,
  confidence 0.9.
- Optional precision: `rust-analyzer scip .` output ingested as high-confidence edges (batch,
  opt-in, hours-long on big workspaces).

### 6.2 TypeScript

- Grammars: `tree-sitter-typescript` (`.ts .mts .cts`) and its `tsx` language (`.tsx`). `.js/.jsx`
  through the same pack with `tree-sitter-javascript`.
- Resolver: parse every `tsconfig.json` (with `extends`), honor `baseUrl`/`paths`/project
  references; Node ESM/CJS rules (`./foo.js` → `foo.ts`; directory → `index.ts`); follow
  `export * from` / `export { a as b } from` barrels up to 8 hops with cycle guard.
- Default exports get the file stem as name, flagged `default`.
- JSX: capitalized JSX tag names are `@ref.type` to components.
- `declare module`, `.d.ts`, overload signatures: recorded, grouped as `×N defs`, never embedded
  individually (embed the implementation signature only).

### 6.3 Luau

- Grammar: `tree-sitter-luau` (JohnnyMorganz; pinned — the polychromatist grammar has different
  node names). `.luau .lua .server.lua .client.lua`.
- Definitions: `local function f`, `function M.f`, `function M:f` (method; record `self`),
  `type T =`, `export type T =`, and table-field functions `f = function()` / `f = function(...)`
  inside a table constructor assigned to a module-level local.
- Module table tracking: the value of the file's final `return X` is the export surface; `X.f`
  defs are exported, others are local. FQName = module path ‖ name.
- Resolver: `require(...)` forms: `script.Parent.Foo`, `game.ReplicatedStorage.X.Y`,
  `Packages.Foo` (Wally), string paths `"./foo"` and `"@pkg/foo"`. Rojo `*.project.json` maps
  instance trees to disk; `init.luau` = directory module. Without a Rojo file, fall back to
  basename matching with low confidence.
- OOP idiom: `local C = {}; C.__index = C; function C.new()` → `@def.class C`; `setmetatable(x, C)`
  → `instance_of` edge. Good enough for ranking.

### 6.4 DreamMaker

- Grammar: `tree-sitter-dm` (FeudeyTF; WIP, tested on SS13 codebases). Expect ERROR nodes; extract
  from whatever parsed. Vendor the grammar as a crate via `cc` (no crates.io release as of writing).
- FQName = the *absolute type path*. DM allows both absolute (`/obj/item/gun/proc/fire()`) and
  nested-indentation forms:
  ```
  /obj/item
      gun
          proc/fire()
  ```
  The extractor walks the tree accumulating path segments; `proc/` and `verb/` segments mark a
  *definition*; a matching path without `proc/` is an *override*. Store both, flag
  `definition|override`, group under one fqname (`×N defs`), rank definition first.
- Type tree: build `/datum` → `/atom` → … inheritance from all files; resolve `..()` to the parent
  type's proc; resolve `src.foo()`, `L.foo()` where `var/mob/living/L` declares a type (DM's typed
  vars make this tractable).
- `.dme`: parse `#include` lines to flag files as `in_dme` (rank included files above stray ones).
- Signals (modern SS13): `RegisterSignal(x, COMSIG_FOO, PROC_REF(bar))`, `TYPE_PROC_REF`,
  `SEND_SIGNAL(x, COMSIG_FOO, ...)` and legacy `.proc/bar` strings → `signal` edges keyed by the
  `COMSIG_*` define; `#define COMSIG_*` lines become `@def.macro` so `refs:COMSIG_FOO` works.
- Exclude from lexical/semantic by default: `.dmm` (maps; enormous), `.dmi` (PNG), `.dmf`, `.dmb`,
  `.rsc`. Index `.dmm`/`.dmi` as *assets* by name only so `trufflepig file:icons/obj/guns` works.
- Sidecar (opt-in): SpacemanDMM's `dmdoc` JSON for full-fidelity object tree + doc comments
  (`///`, `/**`, `//!`, `/*!`). Never link the `dreammaker` crate (GPL-3); run it as a process.

### 6.5 Docs & config

`.md .txt .rst .adoc` and CLAUDE.md/AGENTS.md/README chunked by heading; `.toml .json .yaml`
indexed lexically only. Docs are ranked below code unless `in:docs` or the query is a Concept with
zero code hits above threshold.

---

## 7. Semantic layer

### 7.1 What gets embedded

One chunk per symbol; one preamble chunk per file. Never fixed windows. Never whole files.
Symbols under 3 tokens of body (trivial getters) still get a chunk — it's cheap and exactness
matters — but their vectors are weighted 0.7 at fusion.

### 7.2 Chunk text template (version-stamped; bump → global re-embed)

```
{lang} {kind} {fqname}
{path}
{signature}
{doc_comment (first 6 lines)}
{body (first 40 lines, tail 8 lines if longer; middle elided as '…')}
identifiers: {split camel/snake identifiers, deduped, ≤ 40}
```

The `identifiers:` line is deliberate: it lets small models bridge "rate limit" ↔ `TokenBucket`.

### 7.3 Models (all local; selected by `doctor`, overridable)

| Role | Default | Alternatives | Notes |
|---|---|---|---|
| Embedder (fast) | `nomic-ai/CodeRankEmbed` 768-d | `EmbeddingGemma-300M`, `Qwen3-Embedding-0.6B` | needs its query prefix; Apache |
| Embedder (quality) | `Qwen3-Embedding-4B` (MRL→512) | `Qwen3-Embedding-8B`, `nomic-embed-code` | last-token pooling; instruction-aware |
| Reranker | `bge-reranker-v2-m3` (fastembed-native) | `Qwen3-Reranker-0.6B` (needs ONNX export or candle) | ≤ 512-token pairs |
| HyDE (opt) | `Qwen2.5-Coder-1.5B` Q8 | any GGUF | `--think` only |

`jina-code-embeddings-1.5b` is strong but CC-BY-NC: allowed as a user-configured model, never a
shipped default. Model id + dims + quant + template version live in the vector file header and the
embedding-cache path; the daemon refuses to mix.

### 7.4 Inference

- `fastembed` over `ort` with the CUDA execution provider; CPU fallback with int8 ONNX exports.
  Alternative: `candle` directly (no C++ FFI, Flash-Attention on CUDA) when a model isn't in ONNX.
- Dynamic batching: sort pending chunks by token length, batch to a token budget (not a count),
  pad minimally. Backlog batches are preempted by query embeds.
- Query embedding is cached (LRU 1k) by (model, prefix, text).
- Vectors are L2-normalized, MRL-truncated, *re-normalized*, then scalar-quantized to int8 with a
  per-model global scale learned from the first 10k vectors.

### 7.5 HyDE (`--think`)

Prompt the small coder model: "Write the signature and first lines of the function most likely
matching: <query>. Language: <dominant repo language>." Embed the output as a second query vector.
Fuse via RRF only; never feed HyDE text to the lexical lane (hallucinated identifiers would match
real ones).

---

## 8. Graph

Nodes = symbols (+ files for `mentions`/`imports`). Edges = `calls`, `refs_type`, `refs_field`,
`imports`, `implements`, `overrides`, `same_type`, `signal`, `mentions`, `contains`.

- `contains` is excluded from PageRank. `mentions` is weighted 2.0 (a human wrote "see X").
- Edge weight = confidence. Edges whose destination name has more than 12 candidate definitions
  (`new`, `len`, `Initialize`, `attack_hand`…) are dropped from PageRank unless resolved with
  confidence ≥ 0.8.
- Global PageRank (d = 0.85, 30 iterations) recomputed on debounce; personalized PageRank at query
  time seeded from symbols named in the query (this is what makes "things related to X" rank).
- `ctx <id>` = symbol + doc + top 8 callers + top 8 callees + types used + mentions in/out, each as a
  one-liner with ids. One screen, ≈ 400 tokens.
- `map` = per-file top symbols by PageRank, files ordered by PageRank mass, budgeted. With `--cwd`
  or a path argument it's personalized toward that subtree.

## 9. Incrementality & worktrees

- Reconcile on daemon start: `git status --porcelain=v2 -z --untracked-files=all` once (not per
  file) gives changed + untracked; compare `HEAD` to `meta.last_commit` for checkout-scale changes;
  everything else trusts the stat cache. No git → full stat walk (fast; hashing only on stat miss).
- Worktrees share the global embedding cache; a second worktree of a 1M-line repo costs parse time
  only (seconds), zero GPU.
- Renames are free: same chunk hashes, new paths.
- Deleted files: cascade delete in SQLite, tantivy delete-by-term, vector tombstone.
- Live-file guard: `show`/`ctx` re-hash the file before printing; mismatch → print live content with
  a header flag and enqueue re-index at high priority.

## 10. Performance targets (16-core desktop, NVMe)

| Metric | Target |
|---|---|
| Cold structural index, 1M LOC mixed | < 60 s to first query; < 20 s typical |
| Cold embeddings, 1M LOC (≈ 150k chunks), GPU | < 10 min background (fast model) |
| Single-file update → queryable | < 50 ms |
| Query p50 / p95, no semantic | 15 ms / 40 ms |
| Query p50 / p95, embed + rerank | 60 ms / 150 ms |
| `show` | < 5 ms |
| Daemon RSS, 1M LOC, excluding model | < 1 GB |
| Output for default budget | ≤ 600 tokens, ≥ 8 hits |

## 11. Crate manifest (names; pin versions at bootstrap)

Core: `clap`, `tokio`, `interprocess`, `rusqlite` (bundled), `tantivy`, `tree-sitter`,
`tree-sitter-rust`, `tree-sitter-typescript`, `tree-sitter-javascript`, `tree-sitter-luau`,
vendored `tree-sitter-dm` (via `cc`), `ignore`, `grep-regex`, `grep-searcher`, `notify`,
`notify-debouncer-full`, `blake3`, `memmap2`, `simsimd`, `usearch` (feature-gated), `petgraph`,
`rayon`, `regex`, `aho-corasick`, `tiktoken-rs`, `serde`/`serde_json`, `rkyv` (IPC), `gix`.
Inference: `fastembed` (+`ort` CUDA EP) and/or `candle-core`/`candle-nn`/`candle-transformers`,
`tokenizers`, `hf-hub`; optional `llama-cpp-2` or `mistralrs` for `--think`.
Dev: `criterion`, `insta` (snapshot the output format), `proptest` (tokenizer/packer).

## 12. Evaluation (build this at M1, before ranking work)

- Corpus: the user's own repos + 3 public ones per language (an SS13 codebase for DM, a Rojo
  project for Luau, a TS monorepo, a Rust workspace).
- Queries: mine `git log`: for each fix/feature commit with ≥ 1 and ≤ 6 changed source files, the
  query is the commit title (and, separately, the first body sentence). Ground truth = changed files
  and changed symbols.
- Metrics: file recall@5 / @10, symbol recall@10, **tokens-to-first-hit** (tokens emitted before
  the first ground-truth file appears), **tokens-to-recall@5**, and query latency. Report per
  language and per intent class.
- Agent-in-the-loop (weekly, small): 20 tasks with Claude Code / others, measuring total input
  tokens, tool calls, and pass rate with and without trufflepig in CLAUDE.md.
- Regression gate: `trufflepig eval` runs the offline suite in CI; ranking changes must not reduce
  tokens-to-recall@5 by > 3% on any language.

## 13. Milestones

- **M0** walker, hasher, SQLite, Rust+TS packs, `sym:`/`re:` lanes, output format + packer, `show`.
  Ship to yourself; use it daily.
- **M1** daemon + watcher + reconcile, tantivy lane, RRF, `more`, eval harness, CLAUDE.md snippet.
- **M2** embeddings (fast model), global cache, reranker, `idx:warming`, `--think`.
- **M3** graph, PageRank, mentions, `ctx`, `refs`, `map`, grouping of multi-defs.
- **M4** Luau pack + Rojo resolver; DM pack + type tree + signals + `.dme`; dmdoc sidecar.
- **M5** LSP/SCIP enrichment (rust-analyzer, tsserver, luau-lsp), MCP mode, pack override dir.


---

## 14. Gotchas

Each item: the trap, then the rule. Items marked ★ have bitten comparable tools in the wild.

### 14.1 Output & tokens

- ★ **JSON by default.** Braces, quotes, and keys cost 30–40% more tokens for identical
  information. Line-oriented text default; NDJSON only behind `--json`.
- ★ **ANSI escapes in piped output.** Agents capture stdout; every `\x1b[32m` is tokens and
  confusion. Colour only when `isatty(stdout)`. Same for spinners and progress bars.
- **Absolute paths.** `/home/josh/src/ephyra/crates/…` repeats 30 tokens per hit. Repo-relative
  always; accept absolute on input.
- **Echoing the query.** The agent already has it. One short header line, nothing more.
- **Printing the matched line instead of the signature.** The matched line is often `}` or a
  comment fragment. The signature is what lets the agent decide.
- **Signatures with 300-char `where` clauses** (Rust generics, TS mapped types). Truncate at 96
  chars with `…`; `show` has the full thing.
- **Duplicate spans.** Three lexical hits inside one function are one result.
- **Cutting a line mid-token when packing.** Pack whole lines; if the next line doesn't fit, stop.
- **Token estimate drift.** o200k ≠ Claude's tokenizer; keep the 15% safety factor and expose
  `--budget` so the agent can go lower. Never emit more than the budget; under is fine.
- **Trailing whitespace and blank context lines.** Strip trailing whitespace; collapse runs of
  blank lines in context to one.
- **Nondeterministic ordering.** Equal scores must tie-break on (path, start_line) so two runs
  produce identical bytes. fp16 embedding jitter is real; quantize scores to 1e-4 before sorting.
- **"No results" with a non-zero exit.** Agents interpret it as tool failure and retry the same
  call. Exit 0, print `no hits — try re:<pattern> or drop lang:/file: filters` (one line).
- **Verbose "did you mean".** One line, max two alternatives.
- **Line numbers that disagree with the agent's editor.** Count lines on raw bytes; CRLF = 1 line;
  a lone CR is not a line break (matches `sed`, `nl`, editors). Never normalize line endings before
  counting. UTF-8 BOM does not shift line numbers but does shift byte offsets — strip from spans.
- **Changing the `show` format.** Agents pattern-match `212│`. Freeze it; version any change.
- **`show` of a 3,000-line symbol** (DM `Initialize` monsters, generated TS). Cap at 200 lines with
  explicit `… (lines 400–2900 omitted; show path:400-600)` markers.
- **Path prefix elision that's ambiguous** (`…/mod.rs` — which one?). Elide only when the elided
  form is unique within the result set; otherwise print the full relative path.
- **Unicode paths / RTL / zero-width characters** in filenames (rare, but repos have them). Print
  as-is; never "sanitize" to a different string the agent can't `cat`.
- **Grouping that hides the thing they wanted.** `×12 defs` must still show the top-ranked
  definition's location on the same line.

### 14.2 tree-sitter

- ★ **Language ABI mismatch.** Grammars generated with tree-sitter CLI 0.20 don't load in the 0.22+
  runtime, and vice versa. Pin the runtime and regenerate vendored grammars with the matching CLI.
  `tree-sitter-dm` is a fresh 0.25-series grammar; check every other grammar's `tree_sitter`
  dependency before mixing.
- **`LanguageFn` vs `language()` API churn.** Bindings changed twice in 2024–25. Wrap grammar
  loading in one `fn lang(id) -> Language` so churn is isolated.
- **External scanners.** Luau, TypeScript, and DM grammars have `scanner.c`; forgetting to compile
  it yields silently wrong parses (strings, indentation blocks). Vendor `src/scanner.c` and build
  with `cc`.
- ★ **Pathological files hang the parser.** Minified JS, 40k-line generated tables, `.dmm` maps if
  they leak into a code pack. Always set a parse timeout (`set_timeout_micros` / cancellation
  flag) and a size cap. Keep the partial tree on timeout.
- **Recursion on deep trees.** 10k-element array literals blow the stack with recursive walkers.
  Use `TreeCursor` iteratively; spawn parse threads with a 64 MiB stack anyway.
- **`Parser` is `!Sync`.** One per thread (`thread_local!` in the rayon pool); one per language
  per thread, or reset the language each time.
- **Retaining trees.** A `Tree` holds the whole node graph; keep only extracted data. On a 1M-LOC
  repo, retained trees are gigabytes.
- **Query predicates.** `#eq?`/`#match?` are evaluated by the Rust bindings only when you use
  the text-provider `matches()` API; `#set!`/custom predicates need manual handling. Test each pack
  query with a fixture corpus.
- **Queries that compile on one grammar version and not the next.** Node types get renamed.
  Compile every pack query at startup and in CI; fail loudly with the offending pattern.
- **ERROR/MISSING nodes.** Extract inside them anyway (tree-sitter recovers well), but never assign
  a `container` across an ERROR boundary — you'll attach a method to the wrong impl.
- **Byte vs char vs UTF-16 columns.** tree-sitter gives bytes; LSP wants UTF-16 code units;
  humans want chars. Store bytes; convert at the edge; never mix.
- **Injections.** Lua inside strings, `<script>` in HTML, SQL in Rust strings, TS in `.vue`/
  `.svelte`. Out of scope for M0; if added, injected symbols must carry the host file's line base.
- **Incremental parsing isn't worth it** for a file indexer. Whole-file re-parse is ~ms; `tree.edit`
  bookkeeping is where bugs live.
- **`tags.scm` shipped with grammars is inconsistent** across grammars. Write your own capture
  set with the conventions in §6.

### 14.3 Rust

- ★ **`mod` resolution**: `foo.rs` vs `foo/mod.rs` vs `#[path = "…"]` vs inline `mod foo { }`.
  Handle all four or fqnames are wrong for half the workspace.
- **Workspaces**: same symbol names across crates (`Config`, `Error`). FQName must start with the
  crate name (read `Cargo.toml` `[package].name`; `-` → `_`).
- **`#[cfg]` duplicates**: two `fn foo` in one file under different cfgs. Both are definitions;
  group, don't dedupe.
- **Impl blocks spread across files** (`impl Foo` in 12 files). Link with `same_type`; `ctx Foo`
  must show them all.
- **Derive and attribute macros create invisible impls** (`Clone`, `Serialize`, `#[tokio::main]`).
  Don't try to resolve them; do treat `#[derive(X)]` as a `refs_type` to trait `X`.
- **`macro_rules!`-generated items** are unresolvable from the AST. Index the macro definition and
  every invocation as a `macro` ref so `refs:my_macro!` works.
- **Trait method dispatch.** `x.write()` matches `Write::write` on every type. Confidence
  1/candidates; cap the fan-out; rely on the local `let x: T` heuristic.
- **Glob imports `use foo::*`.** Record the glob; resolve names lazily by lookup in that module.
- **`target/`, `vendor/`, `build.rs` output, `include!`.** Never walk `target/`; `include!`'d
  files under `OUT_DIR` don't exist in the tree — skip silently.
- **Doc comments**: `///`, `//!`, `#[doc = "…"]`, and `#[doc(hidden)]`. All four are docs; the
  attribute form appears in generated code.
- **`tests/`, `examples/`, `benches/`** are separate crates with their own roots. Flag `test`.
- **rust-analyzer as a sidecar** takes minutes and gigabytes on large workspaces and may trigger
  `cargo check`. Batch `rust-analyzer scip .` opt-in only; never spawn it on query.
- **`Self`** in signatures: substitute the impl target for display (`Self::new` → `Foo::new`).

### 14.4 TypeScript / JavaScript

- ★ **`tsconfig.json` `paths`/`baseUrl`/`extends`/project references**; monorepos have twelve of
  them. Parse them all; a path alias unresolved = no import edges = no graph.
- ★ **Barrel re-exports.** `export * from './foo'` chains eight deep. Follow with a hop limit and a
  cycle guard, or `refs:` returns the barrel instead of the definition.
- **Extension resolution.** `import './x.js'` resolves to `x.ts` under ESM; `x` → `x.ts` →
  `x.tsx` → `x/index.ts`. Also `.mts`/`.cts`.
- **Two grammars.** `typescript` and `tsx` are different languages in the same crate; pick by
  extension or JSX in `.js` mis-parses.
- **`node_modules`, `dist/`, `build/`, `.next/`, `coverage/`, `*.min.js`, `*.map`.** Exclude by
  default. Someone will commit `dist/`; the minified-line heuristic catches it.
- **`.d.ts`** files are huge, generated, and full of overloads. Symbol-only, never embedded, ranked
  below implementations.
- **Anonymous default exports** (`export default function () {}`, `export default {…}`). Name by
  file stem and flag; otherwise they vanish from `sym:`.
- **Overloads** produce N declarations + 1 implementation; group.
- **Object-literal and class-field methods** (`foo: () => …`, `foo = async () => …`) are the
  dominant style in some codebases; without a capture for them half the "functions" are missing.
- **Decorators, namespaces, `declare global`, `declare module 'x'`.** Capture as defs with the
  right kind; `declare module` augmentations make the same name appear in many files.
- **Dynamic `import()` and CJS `require()`**: import edges with lower confidence.
- **Bundler aliases** (vite `resolve.alias`, webpack) that aren't in tsconfig. Provide a manual
  alias map in `.trufflepig/config.toml`.
- **Symbol-name collisions** (`index`, `handler`, `default`, `Props`). PageRank and path priors
  are the only defense; don't try to be clever.
- **Type-only imports** (`import type`) shouldn't create `calls` edges.

### 14.5 Luau

- ★ **`require(script.Parent.Foo)` is not a path.** It's a Roblox instance path; disk mapping comes
  from `*.project.json` (Rojo). Without it, resolve by basename at confidence 0.4 and say so.
- **`init.luau` / `init.lua` = the directory.** `require(Parent.Foo)` where `Foo/init.luau` exists.
- **`.server.lua` / `.client.lua` / `.server.luau`** suffixes are Rojo semantics; strip for module
  naming, keep as a flag.
- **Wally `Packages.Foo`** resolves into `Packages/_Index/…` — vendored; index names only.
- **String requires** (`require("./foo")`, `require("@pkg/x")`) in Lune/standalone Luau; both
  forms coexist in one repo.
- **Module table tracking.** Exports are fields of whatever `return`s at the end; `function M.f`
  vs `function M:f` (implicit `self`; display with `:`) vs `M.f = function`. A module might build
  the table in a loop (`for name, fn in pairs(impl) do M[name] = fn end`) — accept the loss.
- **OOP idioms are conventions, not syntax.** `Class.__index = Class`, `setmetatable`, `.new`.
  Pattern-match the common three forms; don't attempt generality.
- **Two tree-sitter-luau grammars** with different node names (polychromatist vs JohnnyMorganz).
  Pin one; the other exists in `nvim-treesitter` and will be what someone's editor uses.
- **Long strings and long comments** (`[[…]]`, `--[==[ … ]==]`) need the external scanner.
- **`--!strict`, `--!native`** directive comments are not docs.
- **Roblox globals** (`game`, `workspace`, `Instance.new`, `Enum.*`) are an enormous API surface.
  Never try to resolve them; optionally load the API dump as read-only symbols for `sym:`.
- **Type annotations** (`: {x: number}`, generics `<T>`) are real definitions when `type`/`export
  type`; inline annotations are `refs_type`.
- **`.rbxlx`/`.rbxmx`** XML may contain embedded scripts. Out of scope; note it in `doctor`.

### 14.6 DreamMaker (yes, really)

- ★ **The preprocessor is half the language.** `#include` from the `.dme` decides what compiles;
  `#define` macros (`SIGNAL_HANDLER`, `PROC_REF`, `CALLBACK`, `list()` wrappers) appear everywhere;
  `#ifdef` blocks carry alternative definitions. Tree-sitter sees none of this. Accept partial
  trees, extract macro *uses* as refs, and index `#define` lines as `@def.macro`.
- ★ **Two block syntaxes.** Indentation-based (tabs significant) and brace-based coexist, sometimes
  in one file. Mixed tabs/spaces produce ERROR nodes. Keep going.
- ★ **Overrides are re-definitions with the same path.** `/mob/living/proc/death()` is defined
  once and overridden in 40 files. Definition vs override is decided by the presence of `proc/`
  (or `verb/`) in the path. Group by fqname; rank definition first; `refs:` must include overrides.
- **Nested-indentation type paths.** The fqname is only knowable by walking ancestors and
  concatenating segments; a naïve "name of the node" gives `fire()` with no type.
- **`..()` (parent call) and `.()`.** `..()` is an edge to the nearest ancestor type's definition
  of the same proc, which requires the type tree, which requires all files. Resolver runs after
  the batch, not per file.
- **Typed vars are your friend.** `var/mob/living/L` declares a type; `L.death()` resolves at
  0.9 confidence. Untyped (`var/L`) is 1/candidates.
- **Global procs** (`/proc/foo()`) vs type procs; `world`, `usr`, `src`, `global.` prefixes.
- **`.dmm` files are enormous** (tens of MB of tile dictionaries), `.dmi` are PNGs, `.dmb`/`.rsc`
  are build outputs. Assets by name only; never lex/embed. `.dme` itself: parse `#include` only.
- **Windows-first encoding.** CRLF everywhere, UTF-8 BOMs, and genuine Latin-1 bytes in old
  codebases. Decode lossily, keep byte offsets from the original bytes, count lines on raw bytes.
- **String interpolation `"[src] says [msg]"`.** Bracket expressions inside strings are code; the
  lexical tokenizer must not treat `[src]` as a bracket token soup.
- **`<<` / `>>` are output/input operators**, not shifts. Only matters for lexical stopwords.
- **Signal bus.** `RegisterSignal`/`SEND_SIGNAL` with `COMSIG_*` defines and `PROC_REF(x)` /
  `TYPE_PROC_REF(/type, x)` / legacy `.proc/x` strings are the real call graph in modern SS13.
  Extract them as `signal` edges keyed on the define name, or callers/callees are empty.
- **Extreme name repetition.** `Initialize`, `examine`, `attack_hand`, `update_icon` have hundreds
  of definitions. `sym:examine` must group by type path and rank by PageRank + `in_dme` +
  definition-first, or it's 400 lines of noise.
- **Scale.** SS13 codebases are 1–3M lines and 50–100k procs. Everything must be O(changed files);
  PageRank must be sparse and debounced.
- **SpacemanDMM is GPL-3.** `dmdoc` JSON via subprocess only. `tree-sitter-dm` is WIP; expect to
  contribute fixes upstream and vendor a pinned commit.
- **Documentation comments.** dmdoc's `///` and `/**` (preceding) and `//!` and `/*!` (inside)
  conventions; treat all as docs.
- **`proc` names can collide with `var` names** on the same type. Kinds are part of identity.

### 14.7 File walking, ignore rules, filesystem

- ★ **Untracked files must be indexed.** The agent just created `foo.rs`; if you only trust
  `git ls-files`, it's invisible. Respect `.gitignore` but walk the working tree.
- **`.trufflepig/` must be ignored by itself and by git.** `init` appends to `.gitignore`; the
  walker hard-excludes its own directory even if it isn't.
- **Ignore file zoo.** `.gitignore` (nested), `.git/info/exclude`, global gitignore, `.ignore`,
  `.rgignore`, plus a `.trufflepigignore`. The `ignore` crate handles the first five; add the last.
- **Symlink loops and symlinks out of the repo.** Don't follow symlinks by default; if followed,
  dedupe by canonical path with a visited set.
- **Case-insensitive filesystems** (macOS, Windows): `Foo.ts` and `foo.ts` are one file. Normalize
  paths per-platform for identity, print the on-disk case.
- **Path separators.** Store `/`; convert on Windows at the edge.
- ★ **mtime is a hint, not truth.** `git checkout` sets mtime to now; `touch` lies; clock skew;
  1-second granularity on some filesystems. Use size+mtime only to skip hashing, and apply git's
  "racy" rule: if mtime is within 2 s of the last index write, hash anyway.
- **Huge files.** Lockfiles, `package-lock.json`, `.dmm`, generated `*.pb.rs`, SQL dumps. Per-pack
  byte caps; over-cap files are recorded (so `file:` works) but never parsed/embedded.
- **Binary detection.** NUL in the first 8 KiB; also check `content_inspector`. A `.ts` file that's
  actually MPEG-TS video exists in the wild.
- **Minified detection.** Average line length > 400 or a single line > 20 KB → lexical-only.
- **Generated-file markers.** `@generated`, `DO NOT EDIT`, `AUTOGENERATED`, `// <auto-generated`,
  `#![allow(clippy::all)]` at top of a 10k-line file. Flag `gen`, de-boost.
- **Nested repos and submodules.** A `.git` inside the tree means another project; index it
  (it's on disk) but flag `submodule` and de-boost unless the query path filter points into it.
- **Repo root detection.** `.trufflepig.toml` > `.git` (file or dir; worktrees have a `.git`
  *file*) > cwd. Agents run from subdirectories; `--cwd` filtering is relative to the root.
- **Files being written.** Atomic-save editors do write-temp + rename; naive tools write in place
  and the watcher fires mid-write. Debounce 75 ms, re-hash before commit, and if the parse looks
  truncated (ERROR at EOF), retry once after 150 ms.
- **Editor droppings.** Vim swap `.foo.rs.swp`, `4913` test files, Emacs `#foo#`/`.#foo`, JetBrains
  `.idea/`, `__pycache__`. Ignore list ships with the binary.

### 14.8 Watching & incrementality

- ★ **inotify watch limits.** Linux default `max_user_watches` is often 8192–65536; a 100k-directory
  repo exceeds it silently. Detect `ENOSPC`, fall back to polling the stat cache every 2 s, and
  print the `sysctl` one-liner in `doctor`.
- ★ **Event storms.** `git checkout`, `git rebase`, `cargo build` (if `target/` isn't excluded from
  the *watch*, not just the walk), `npm install`. Coalesce per path; if a batch exceeds 2,000 paths,
  drop the batch and run a reconcile instead.
- **Watch `target/`/`node_modules` = death.** Excluded directories must be pruned from the watch
  set, not just filtered after events (notify's recursive mode watches everything).
- **`.git/` internals** generate thousands of events; exclude the directory from watching but
  poll `HEAD`/`ORIG_HEAD` for the checkout-detection path.
- **Rename events arrive as two events** (or one with a cookie, platform-dependent). Content
  hashing makes it not matter: old path deleted, new path added, chunks reused.
- **Deleted files leave orphans** in tantivy segments, the vector file, and the graph. Cascade
  deletes and tombstones; compact when tombstones > 20%; recompute PageRank.
- **Stale spans after edits.** Between an edit and re-index, spans point at moved lines. Flag `~`
  on any hit whose file mtime > indexed_at; `show` re-hashes and prints live.
- **Reconcile cost on cold daemon start.** A full hash walk of a 1M-LOC repo is seconds of NVMe;
  fine. A full *parse* is not fine before first query — serve from the existing index, reconcile,
  then re-parse only diffs.
- **Two daemons for one repo** (race on spawn). Take an exclusive lock on `.trufflepig/lock`
  before binding; the loser exits.
- **Chunk hash must include** language, pack version, and template version, or a pack fix leaves
  stale chunks matching old hashes forever.
- **Embedding cache key must include** model id, dims, quant scheme, and query/passage prefix
  version.
- **Worktrees share `~/.cache` but not `.trufflepig/`.** Never put the SQLite file in the common
  git dir; never share it across worktrees (different HEADs, different files).

### 14.9 Embeddings, models, GPU

- ★ **Asymmetric prefixes.** CodeRankEmbed needs `Represent this query for searching relevant
  code: `; Qwen3-Embedding takes an instruction; jina-code has per-task prefixes. Wrong or missing
  prefix silently drops recall by double digits. Prefix config lives with the model entry.
- ★ **Pooling.** BERT-style models mean-pool; Qwen3/jina-code use last-token (EOS) pooling.
  Getting this wrong produces plausible-looking garbage vectors. Verify with a 20-pair sanity test
  at model install (`doctor`).
- **Normalize, truncate, re-normalize.** MRL slicing changes the norm; cosine on un-renormalized
  slices is wrong.
- **int8 quantization needs a scale.** Learn a global scale from the first 10k vectors, store it
  in the header, and re-quantize if the distribution drifts (check on compaction).
- **Sequence length.** 8192 max but a 2,000-line proc is 30k tokens. Head/tail template (§7.2) and
  a hard truncation with the signature first, so the most informative part survives.
- **Padding waste.** Sort by length before batching; batch by token budget. A 1-line getter padded
  to 8192 is 100x wasted compute.
- ★ **ort CUDA EP versioning.** The ORT build, CUDA toolkit, and cuDNN major versions must match
  exactly; Arch's packaged `onnxruntime` may lack CUDA. Use ort's `download-binaries` with the
  CUDA feature, or ship candle for the CUDA path and keep ort for CPU.
- **fastembed's default quantized BGE-M3 is CPU-only** (documented: passing the CUDA EP fails).
  Assume the same for any int8 export; keep separate fp16 ONNX exports for GPU.
- **Thread oversubscription.** ORT intra-op threads × rayon threads = 256 threads on a 32-thread
  CPU. Set ORT intra-op = 4 and inter-op = 1 in the daemon; rayon owns parsing.
- **GPU is shared** with the user's other local models. Pick the device with the most free VRAM
  at daemon start; expose `TRUFFLEPIG_DEVICE=cuda:1|cpu`; never allocate more than needed for the
  batch (no persistent 8 GB arena for a 300M model).
- **CUDA context init is 1–3 s.** Do it in the daemon at start, never in the CLI, never lazily on
  the first query (the first query becomes a timeout).
- **Model download on first run** is hundreds of MB to GB from HF Hub, with rate limits and
  occasional 5xx. Download in `init`/`doctor`, print progress on stderr only there, support
  `HF_HOME`, `HF_ENDPOINT`, and an offline mode that degrades to lexical.
- **Custom architectures** (nomic-bert rotary, JinaBERT ALiBi) need `trust_remote_code` in Python
  land; in Rust you need an ONNX export or a candle port that exists. Check before promising a
  model in the docs.
- **Model upgrades = full re-embed.** Refuse to mix; run the new model into a new `vec/<id>/`
  directory in the background and switch atomically when caught up.
- **Reranker cost is quadratic-ish** in pair length. Truncate chunk text to 512 tokens for
  reranking; cap candidates at 50; skip rerank when the sym lane has an exact hit.
- **Rerankers rank; they don't filter.** A confident reranker still puts *something* first for a
  nonsense query. Keep the RRF score and drop hits whose fused score is below a floor.
- **HyDE hallucinates real-looking identifiers.** Vector lane only; never lexical.
- **Comment-language mismatch.** Comments in German/Russian/Portuguese repos vs English queries.
  Multilingual embedders (Qwen3, jina-v3) handle it; CodeRankEmbed less so. Note in `doctor`.
- **Embedding the same trivial text 5,000 times** (`fn new() -> Self { Self::default() }`). The
  content-hash cache makes it free; without it you'd embed it 5,000 times.
- **Backlog starvation.** If queries always preempt, a busy session never finishes embedding.
  Reserve 30% of GPU batches for backlog.

### 14.10 Vector search

- **Brute force is the right default** below ~200k vectors; HNSW build time and recall tuning
  aren't worth it. Above that, `usearch` with `ef_search` ≥ 128.
- **Filtering + ANN.** `lang:dm` on an HNSW index post-filters and returns 3 results. Over-fetch
  10×, or brute-force within the filtered row set (cheap when the filter is selective).
- **HNSW deletions are soft** in usearch; rebuild on compaction.
- **mmap alignment.** Rows aligned to 64 B or SIMD loads fault/slow down.
- **Concurrent append while querying.** Append-only file + atomic count bump in the header;
  readers snapshot the count.
- **Score scales differ per model.** Never threshold on raw cosine across models; threshold on
  fused rank or per-model calibrated quantiles.

### 14.11 Lexical (tantivy)

- ★ **Default tokenizer lowercases and splits on non-alphanumerics** — good — but also drops the
  original identifier. `parse_header` must index as `parse_header`, `parse`, `header`;
  `TokenBucket` as `TokenBucket`, `tokenbucket`, `token`, `bucket`. Custom tokenizer, no stemming,
  no English stopwords.
- **Language keywords are stopwords.** `fn`, `let`, `if`, `proc`, `var`, `local`, `function`
  match everything. Per-pack stoplist applied at query time for the body field, not the name field.
- **Digits matter.** `sha256`, `v2`, `utf8`. Don't strip them.
- **Path field tokenization.** Split on `/`, `_`, `-`, `.` so `file:net/ws` works via lexical too.
- **One `IndexWriter` per index.** The lock file survives crashes; on daemon start, remove a stale
  `.tantivy-writer.lock` only after confirming no other daemon holds `.trufflepig/lock`.
- **Commit latency.** Commits are not free; batch on a timer. Readers need `reload()` after
  commit; keep a reader per query batch, not per query.
- **BM25 length normalization** punishes long symbols. Lower `b` (0.5) for the body field; the
  name/signature fields are short anyway.
- **Fuzzy queries explode.** Levenshtein ≤ 1 on the name field only, only when exact is empty,
  only for terms ≥ 5 chars.
- **Schema changes require a full rebuild.** Version the schema in `meta`; rebuild in the
  background into `lex.new/` and swap.
- **Phrase queries vs identifier splitting.** `"token bucket"` should match `TokenBucket`; run the
  phrase against the split tokens with slop 1.

### 14.12 SQLite

- **Per-symbol inserts.** 100k inserts in autocommit take minutes; in one transaction per file,
  seconds. Prepared, cached statements.
- **Indexes before bulk load.** Create indexes after the initial load; keep them for incremental.
- **WAL on network/synced filesystems** (NFS, Dropbox, iCloud) is broken. `doctor` warns; refuse
  to run there, or use a global cache dir keyed by root hash instead.
- **`busy_timeout`.** Only the daemon writes; but `--offline` CLI reads must not fail on a WAL
  checkpoint. 5 s timeout, `PRAGMA query_only` for the CLI.
- **WAL growth.** Checkpoint on idle and on exit; `wal_autocheckpoint` at 4k pages.
- **Bundled vs system SQLite.** `rusqlite` `bundled` feature; the system library on the user's
  distro may be too old for `RETURNING` or newer FTS.
- **Foreign-key cascades are slow** on bulk deletes; delete by file id with explicit statements.
- **Don't store spans as text.** Integers; the packer formats them.
- **Result-set cache growth.** TTL 10 min, max 50 sets; store compactly (ids + scores).

### 14.13 Graph & ranking

- ★ **PageRank hubs are `new`, `len`, `unwrap`, `Initialize`.** Unresolved or over-resolved edges
  make junk hubs. Drop edges to names with > 12 candidate definitions unless resolved ≥ 0.8;
  weight by confidence; exclude `contains`.
- **Dangling nodes** (symbols with no out-edges) sink rank; standard fix — redistribute.
- **PageRank on 100k nodes** is milliseconds; on 1M with dense DM signal edges it's seconds — keep
  it sparse (top-k edges per node by confidence) and debounced.
- **Personalized PageRank seeds** must be exact-name matches; seeding from fuzzy matches ranks the
  neighborhood of the wrong symbol.
- **Recency via `git log` per file** is O(files) subprocess spawns. One `git log --name-only
  --format=%ct` walk at index time, cached, refreshed on HEAD change.
- **Test files dominate concept queries** ("rate limit" → 15 test cases). De-boost unless the
  query mentions tests or `--tests`.
- **Query too short** (`fix`, `bug`, `the`). Below 3 chars or all-stopwords: return the
  personalized map instead, one line saying so.
- **Intent misclassification.** `parse header` (Concept) vs `parse_header` (Identifier). Always
  run both lanes; intent only sets weights.
- **RRF `k`.** 60 is the standard; lower makes top-1 of any lane dominate. Tune on the eval set,
  then freeze.
- **Boost stacking.** Multiplicative boosts compound into 4× swings. Cap the total boost at 2× and
  the total de-boost at 0.25×.
- **Case sensitivity.** `sym:` exact is case-sensitive; lexical is not; DM and Luau are
  case-sensitive languages. Don't "helpfully" fold case in `sym:`.
- **Path filters and cwd.** `file:src/` from a subdirectory: relative to root, always; document
  it in `--help` in one line.

### 14.14 Daemon, IPC, processes

- ★ **Unix socket path limit is 108 bytes** (`sun_path`). A socket inside a deep repo path
  fails with a confusing `ENAMETOOLONG`. Use `$XDG_RUNTIME_DIR/trufflepig/<hash>.sock`
  (fallback `/tmp/trufflepig-<uid>/`).
- ★ **The daemon dies with the agent.** Spawned from an agent's Bash tool, it inherits the process
  group and is killed when the tool call ends. Double-fork, `setsid`, close/redirect all fds,
  ignore `SIGHUP`.
- **Daemon stdout pollutes the agent's output** if not redirected. Redirect to `daemon.log` before
  exec; rotate at 10 MB.
- **Stale socket files after a crash.** Connect fails with `ECONNREFUSED` → unlink and respawn.
  Check the lock file's pid is alive before unlinking.
- **Version skew.** Agent updates the binary mid-session; the old daemon keeps serving. Handshake
  build hash; mismatch → graceful `stop` + respawn.
- **Head-of-line blocking.** One 10k-chunk embedding batch on the GPU blocks the query embed
  behind it. Separate queue with priority; batches are ≤ 200 ms of GPU time each.
- **Idle unload of the model** saves VRAM but the next query pays 2 s. Unload after 30 min idle,
  not 30 s; make it configurable; never unload while a backlog exists.
- **Memory growth.** tantivy segments, retained trees, result-set caches, HF tokenizer caches.
  Track RSS; merge segments on idle; drop trees after extraction.
- **fd limits.** notify + tantivy + mmap + sockets on a 100k-file repo can hit 1024. Raise
  `RLIMIT_NOFILE` soft limit to the hard limit at start.
- **Multiple agents, multiple repos.** One daemon per root; a global registry file lists them so
  `trufflepig status` and `stop --all` work.
- **Windows.** Named pipes, no `setsid`, path separators. Either support it properly or say
  Linux/macOS only in the README. Don't half-support it.
- **Agent tool timeouts.** Claude Code's default Bash timeout is minutes, others are shorter.
  Never block over 2 s on the query path; return partial with `idx:warming`.

### 14.15 Agents & integration

- ★ **Agents will keep using grep.** Without a line in CLAUDE.md/AGENTS.md they never learn.
  `trufflepig init` writes two lines: what it is, and "prefer `trufflepig <q>` then
  `trufflepig show <id>` over grep/cat". Two lines, not twenty (the user has measured this).
- **MCP tool descriptions are paid on every turn.** If shipping MCP, five tools, one sentence
  each, no examples in the description. The CLI is the primary surface.
- **Quoting.** Agents produce `trufflepig rate limit bypass`, `trufflepig "rate limit"`,
  `trufflepig 'fn handle_upgrade'`. Join remaining args with spaces; treat flags anywhere; `--`
  ends flags.
- **Stateless callers + `more`.** The agent doesn't hold a cursor. Daemon keeps the last set per
  repo per client (client = tty or `TRUFFLEPIG_SESSION` env, else "default"); `more` and `show`
  resolve against it; ids collide across clients only if you let them (prefix per client).
- **Ids that look like code** (`#a1`). Fine in text; in `--json` they're a field.
- **Agents paste `show` output back into edits**, including the `212│` gutter. Choose a gutter
  character that's unlikely in code (`│` U+2502) so a stray paste is obvious and greppable.
- **Prompt injection via repo content.** Comments can say "ignore previous instructions". You
  will print comments. Don't act on them (the tool has no actions), don't strip them (the agent
  needs them), do keep `show` output clearly delimited by the header.
- **Path traversal.** `show ../../etc/passwd` and `show /etc/passwd` must fail: resolve, canonicalize,
  require the result to be inside the root (or an explicitly configured extra root).
- **Regex DoS.** The `regex`/ripgrep engine is linear-time, but 10 MB of results is not.
  Cap `re:` output lines (500) and time (1 s); say `truncated`.
- **First run in a fresh clone.** `trufflepig foo` in an unindexed repo must answer in seconds
  with structural results, not print an indexing wall and exit. Index synchronously up to a
  size threshold; beyond it, spawn the daemon and answer from what's parsed so far.
- **Agents run in parallel worktrees** and each one's first query spawns a daemon and a parse.
  Cheap enough if the embedding cache is global; make sure it is.
- **Locale/encoding of the agent's shell.** Output is UTF-8; `│` is 3 bytes; a `LANG=C` shell
  still passes bytes through. Fine, but never emit anything that needs a terminal font.

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

---

## 15. Open questions

1. Single global daemon multiplexing repos vs one per root? Per root is simpler and isolates
   failures; global saves one GPU model load per repo. Start per-root; share the model via a
   separate `trufflepig-inference` process if VRAM becomes the constraint.
2. tantivy vs SQLite FTS5 trigram for the lexical lane. tantivy wins on ranking and tokenizer
   control; FTS5 wins on "one file, one dependency". Decide by M1 with a benchmark on an SS13 tree.
3. How much of the DM type tree to build in-tree vs lean on `dmdoc` JSON. Start in-tree (paths,
   inheritance, typed vars); fall back to `dmdoc` for macro-heavy files.
4. Whether `map` output should be the session-start context by default, given the user's pasted-
   prompt workflow. Probably yes at ~1,500 tokens, personalized by the task's path filter.
5. LSP enrichment as always-on background vs on-demand. On-demand only until the daemon's resource
   envelope is measured with rust-analyzer attached to a 500k-LOC workspace.
