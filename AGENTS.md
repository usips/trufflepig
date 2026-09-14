# Trufflepig — agent guide

Trufflepig is a pre-alpha search engine for agentic coders navigating
complex repositories. Favor clear contracts, searchable names, and a
small, coherent implementation.

No compatibility layers. Change callers with their contracts; remove
obsolete paths instead of preserving a bad design.

## Verification and commits

- Start with the narrowest relevant test, for example
  `cargo test <name_substring>`. Expand verification when the change
  crosses boundaries or leaves uncertainty.
- Run full suites sparingly, at meaningful checkpoints. Do not repeat
  broad checks without a new change, failure, or unresolved concern.
- Add tests for meaningful behavior and regressions, not to mirror the
  implementation. Complete relevant checks before committing.
- Use Conventional Commits: `type(scope): subject`, imperative, no
  period, at most 50 characters for the complete subject. Put details in
  the body. Include a `Co-authored-by: Name <email>` trailer identifying
  the contributing agent with its actual attribution identity.

## Code organization

- Aim for 5–15 files and 3–10 subdirectories per directory; avoid
  directories approaching 50 entries. Keep depth at most seven levels.
  These are organization guides, not reasons to add empty structure.
- Prefer files under 300 lines; keep them under 1,000. Keep agent guides
  and documentation files at most 200 lines; split by responsibility.
- Use Rust's `xyz.rs` with `xyz/` for child modules, never `mod.rs`.
  Keep unit tests inline or in the module's `tests.rs` child; do not use
  `#[path]` to relocate modules or put unit tests in the crate's
  integration-test directory.
- Use distinct, descriptive file, type, and shared-helper names so an
  agent can find definitions and uses with a repository search. Prefer
  unique basenames apart from conventional entry points and test files;
  avoid generic names such as `Data`, `State`, and `utils`.
- Wrap reusable domain concepts in structs or newtypes instead of loose
  primitives. Reuse existing wrappers and keep their operations together.

## Data and allocation

- Prefer sized, inline representations when the size or bound is known.
  Keep structs cache-friendly; avoid heap ownership and its `Drop` cost
  without a concrete need.
- When heap allocation is appropriate, predict capacity from available
  input sizes or domain bounds and reserve accordingly; avoid repeated
  growth and speculative oversized buffers. When growth is unavoidable,
  grow in large steps rather than one element at a time.
- Do not use `&'static`, leaks, or static storage to hold dynamic data.
  Documented interners are the exception; explain their ownership and
  lifetime contract.

## Comments and documentation

- Keep module documentation to eight lines and item documentation to
  four lines where possible. State contracts, invariants, and useful
  references, not change history or repository policy.
- Repository rules live in `AGENTS.md`; `docs/*.md` is authoritative for
  design contracts. Proactively fix stale docs. Link to sources rather than
  duplicating them or inventing documents and tooling.
- Write documentation as present-tense contracts. Give each fact one
  home and reference it elsewhere with normal Markdown links or
  `path:function` references, never `@` imports. Avoid milestones,
  dates, and screenshots in documentation of record.
- Follow applicable nested `AGENTS.md` files when present.

## Licensing

Licensing belongs exclusively to Josh. Never edit `LICENSE`,
`ATTRIBUTION`, or license declarations. Flag licensing questions and
leave those files and declarations unchanged.
