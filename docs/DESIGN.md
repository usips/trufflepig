# Trufflepig design

Trufflepig locates source spans for agents exploring complex repositories. Exact
identifiers, lexical search, structural navigation, and verified follow-up reads
are the core. Correct source identity takes priority over compact output.

The required scope is a Linux CLI operating locally on CPU, with Rust,
TypeScript/JavaScript, C#, PHP, Luau, and DreamMaker extraction. Tests, documentation,
configuration, and uncovered regions of source files remain searchable. Optional
semantic retrieval requires an explicit `--sem` request.

## Contracts

[CLI usage](cli.md) describes runnable commands and current operational limits.

- [Search, handles, reads, and output](retrieval-contract.md)
- [Free-text lanes, evidence tiers, and path prior](ranking-contract.md)
- [Definition, reference, and outline navigation](navigation-contract.md)
- [Authoritative storage and publication](index-contract.md)
- [Index database revision, recovery, and reclamation](index-storage-contract.md)
- [Cache ownership, daemon lifecycle, and transport](runtime-contract.md)
- [Router binary replacement, read fallback, and daemon placement](router-lifecycle.md)
- [Plan revisions, task claims, commit links, and storage](board-contract.md)
- [Board and feedback command grammar](board-cli-contract.md)
- [Feedback filing, triage, and the durable outbox](board-feedback-contract.md)
- [Local board HTTP, authentication, and live views](board-web-contract.md)
- [Board web UI behavior](board-web-ui.md)
- [Historical navigation and local Git objects](history-contract.md)
- [Diagnostic retention, sessions, and delivery evidence](diagnostics-contract.md)
- [Language extraction and relationship evidence](language-contract.md)
- [PHP extraction and XenForo metadata candidates](php-contract.md)
- [Semantic inference gate and resource limits](semantic-contract.md)
- [Evaluation and acceptance](evaluation-contract.md)
- [Retained licensing declarations for Josh](licensing-notes.md)

These documents specify required behavior. Executable tests and evaluation
reports establish what the implementation actually verifies; a requirement alone
is not evidence of support or measured performance.

## Workspace crates

The repository root is the `trufflepig` binary package and the Cargo workspace.
Library crates live under `crates/`; each depends only on crates listed before it.

| Crate | Owns | Workspace dependencies |
| --- | --- | --- |
| `tp-extract` | tree-sitter and DreamMaker extraction | none |
| `tp-core` | error codes, identities, output, deadlines, request context, process, Git and spool transport, build identity | none |
| `tp-semantic` | embeddings, rerank, the inference worker; `semantic` and `semantic-cuda` features | `tp-core` |
| `tp-index` | store, publication, sources, search, results, semantic preparation, probes | `tp-core`, `tp-extract`, `tp-semantic` |
| `tp-history` | Git history, blame, and source diffs | `tp-core`, `tp-extract`, `tp-index` |
| `tp-board` | plan board, feedback, board web UI and its assets | `tp-core` |
| `trufflepig` (root) | `cli`, `daemon`, `system`, `workspace`, `diagnostics`, `main.rs` | all of the above |

The root package forwards `semantic` and `semantic-cuda` to `tp-index` and
`tp-semantic`. Each library exposes only what other crates use; helpers that
other crates' tests need sit behind that crate's `test-support` feature, which
dependents enable through `[dev-dependencies]`. The root `build.rs` hashes every
workspace source into the build id, and `src/cli/emission.rs:execute` installs
it as `crates/tp-core/src/build_identity.rs:BuildIdentity` at startup, so
libraries read it at runtime and do not rebuild on unrelated edits.
`crates/tp-index/build.rs` derives the extraction-cache grammar versions from
the workspace `Cargo.lock`.

## Scope boundaries

Repository content is data, never executable instructions. Trufflepig does not
edit source, answer questions in generated prose, upload source, or provide a
complete compiler/type-checker model. Candidates remain visible and distinct
from statically resolved relationships.
The board stores agent-authored plan text and evidence; Trufflepig generates none.

The live SQLite WAL database with FTS5 publishes source and graph facts together.
A separate history database retains immutable Git facts and captured-tip traversal
views; diagnostics retain bounded observations and explicit session baselines.
A live result identifies its source revision and index generation. Historical
changes identify commit, blob, path, and original-byte span on each side. These
databases have independent transaction boundaries. The board has its own durable
database, independent of source, history, and diagnostic caches.

Tantivy, custom mmap stores, vector quantization, dimension truncation, ANN,
HyDE, PageRank, LSP/SCIP, and MCP are outside the required scope. An opt-in
cross-encoder rerank stage reorders the top 32 fused files; see the
[semantic contract](semantic-contract.md) for its bounds and fallback. All-ref
traversal, historical ranking/embeddings, cochange/hotness, agent transcript
adapters, and paired-agent trial orchestration remain outside this release.
GPU support cannot block CPU use. No latency, memory, hit-count, or retrieval
quality guarantee follows from the design.
Agent harness integration lives in
[plugins/trufflepig-agent](../plugins/trufflepig-agent/README.md): a shared
skill, a wrapper command that tags requests and writes per-call audit records,
and a summarizer that flags struggling sessions.

## Evidence and limits

Codebase-Memory reports approximately tenfold lower token use and 2.1-fold fewer
calls, with exploration quality scores of 0.83 versus 0.92. Its author-graded,
one-model exploration study motivates evaluation; it does not demonstrate equal
patch success. [Study and limitations](https://arxiv.org/html/2603.27277v1#S4.SS1)

The FTS5 feasibility experiment establishes basic identifier expansion and
transactional behavior only. Repository-scale performance and retrieval quality
require the [evaluation protocol](evaluation-contract.md).

Explicit multi-repository retrieval and owner routing follow the
[workspace contract](workspace-contract.md). Each member retains its own index.
