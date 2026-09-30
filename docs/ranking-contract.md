# Free-text ranking

How ordinary (non-`sym:`, non-`re:`) search orders files. Fixed snapshot,
query and options determine the order ([retrieval contract](retrieval-contract.md)).

## Lanes and fusion

Ordinary queries collapse each retrieval lane to its first region per file
before a 1,000-file lane cap. Lanes run in this order
([`concept_query`](../src/search/concept_query.rs)):

- exact definitions of the whole text; for `A::b`, the definitions of `b`
  that `sym:A::b` resolves ([`qualified_name`](../src/search/qualified_name.rs));
- identifier occurrences: for a single identifier-shaped token (with `_`,
  `::`, a lower-to-upper case change, or letters and digits), files with an
  occurrence of that name (for `A::b`, of `b` in files that also use `A`), most
  uses first, represented by the region of the first declaration or use;
- for two or more terms, a phrase lane and an all-terms (one region) lane;
- the any-term lane and filenames, as for every free-text query.

Ranks combine with reciprocal rank fusion (`k = 60`), with stable path ties.
Lane weights are 3 for an identifier query's exact declarations (not locals,
parameters or imports), 2 for phrase matches, and 1 otherwise. Filename
evidence has weight 2 for an exact normalized stem, 1 for all query tokens in
the basename, and 0.5 for partial path matches. An identifier query ranks files
in tiers before score: a file whose stem is the token (`tick_rate.rs`), then
files declaring it, then files using, binding or importing it, then the rest.
Phrase evidence keeps its lane weight in reciprocal-rank fusion and does not
change these identifier ordering tiers.

The returned hit for each file comes from its strongest evidence across lanes.
Phrase evidence shares the top representative tier with named and declared
evidence when identifier tiers apply; identifier uses follow, then absent
evidence. For other queries, phrase hits precede hits without phrase evidence.
Equal-tier evidence keeps the earlier lane, then the earlier rank within that
lane. This representative hit supplies the file's region and the evidence used
by reranking. A text quoted as one `"…"` literal (no inner quotes) runs the
phrase lane alone.

## Path prior

A path prior multiplies each lane contribution of test paths and Markdown
files (including `AGENTS.md`, `CLAUDE.md`) by 0.7: a demoted file at lane rank
`r` scores like an undemoted one at rank `1.43r + 26`. A test path
([`path_class`](../src/search/path_class.rs)) has a `tests/`, `__tests__/`,
`spec/` or `testdata/` directory, or a `test/` one not followed by `src/`; a
`tests` or `*_tests` stem; a `.test.` or `.spec.` name; or is `*_test.go`,
`*_test.py`, `test_*.py` or `*_spec.rb`. Bare `test` stems, other `*_test`
stems and `fixtures/` directories are production paths. Tests keep full weight
when the query names tests (`test`, `tests`, `testing`, `spec`, `fixture`) or a
`file:` component is a test directory; docs keep it when the query names docs
(`doc`, `docs`, `documentation`, `readme`, `markdown`, `md`, `agents`,
`claude`, `guide`), sets `lang:text`, or `file:` selects a doc path or `docs/`.
`kind:file` queries and, outside identifier queries, files whose basename holds
every query term are never demoted ([`path_prior`](../src/search/path_prior.rs));
demoted files remain eligible. Semantic file ranks then fuse with the combined
ranking at equal weight, the prior applying once to them and the identifier
tiers holding; literal queries skip them. An opt-in rerank stage then reorders
the top 32 fused files; see the [semantic contract](semantic-contract.md).
