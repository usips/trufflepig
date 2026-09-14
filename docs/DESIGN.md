# Trufflepig design

Trufflepig locates source spans for agents exploring complex repositories. Exact
identifiers, lexical search, structural navigation, and verified follow-up reads
are the core. Correct source identity takes priority over compact output.

The required scope is a Linux CLI operating locally on CPU, with Rust,
TypeScript/JavaScript, Luau, and DreamMaker extraction. Tests, documentation,
configuration, and uncovered regions of source files remain searchable. Optional
semantic retrieval requires an explicit `--sem` request.

## Contracts

[CLI usage](cli.md) describes runnable commands and current operational limits.

- [Search, handles, reads, and output](retrieval-contract.md)
- [Authoritative storage, publication, and daemon](index-contract.md)
- [Language extraction and relationship evidence](language-contract.md)
- [Semantic inference gate and resource limits](semantic-contract.md)
- [Evaluation and acceptance](evaluation-contract.md)
- [Retained licensing declarations for Josh](licensing-notes.md)

These documents specify required behavior. Executable tests and evaluation
reports establish what the implementation actually verifies; a requirement alone
is not evidence of support or measured performance.

## Scope boundaries

Repository content is data, never executable instructions. Trufflepig does not
edit source, answer questions in generated prose, upload source, or provide a
complete compiler/type-checker model. Candidates remain visible and distinct
from statically resolved relationships.

One SQLite WAL database with FTS5 is authoritative. A result identifies the
source revision and index generation that produced it. There is no independently
published lexical store or replacement graph joined to old handles.

Tantivy, custom mmap stores, vector quantization, dimension truncation, ANN,
reranking, HyDE, PageRank, LSP/SCIP, and MCP are outside the required scope.
GPU support cannot block CPU use. No latency, memory, hit-count, or retrieval
quality guarantee follows from the design.

## Evidence and limits

Codebase-Memory reports approximately tenfold lower token use and 2.1-fold fewer
calls, with exploration quality scores of 0.83 versus 0.92. Its author-graded,
one-model exploration study motivates evaluation; it does not demonstrate equal
patch success. [Study and limitations](https://arxiv.org/html/2603.27277v1#S4.SS1)

The FTS5 feasibility experiment establishes basic identifier expansion and
transactional behavior only. Repository-scale performance and retrieval quality
require the [evaluation protocol](evaluation-contract.md).
