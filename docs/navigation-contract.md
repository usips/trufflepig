# Definition, reference, and outline navigation

By default, `show 'sym:'`, `refs`, and `map` answer from one published
snapshot. `--no-cache map` and `--no-cache refs` read current bytes as
described below. Every
word after the verb belongs to the query, so `show sym:X file:src/` and
`refs X lang:rust` need no quotes. `file:`/`-file:` values follow the
[retrieval contract](retrieval-contract.md) path-filter rules; `map PATH` treats
`PATH` as one `file:` value and outlines members when it names exactly one
indexed file. Qualification and import limits are in the
[language contract](language-contract.md).

## `show 'sym:NAME [file:P] [lang:L] [kind:K]'`

Candidates are definitions named `NAME`; `sym:A::b` keeps `b` owned by type or
trait `A` (`impl A`, `impl Trait for A`, class `A`) or under module path `A`.
They rank by kind tier (declarations, modules, members, then locals and
imports), then non-test before test sites, then the file sharing the most leading
directories with the invocation directory (a workspace command's cwd relative to
its member root; the root for a single repository), then path. `search 'sym:'`
ranks its hits the same way.

The best candidate is read as a verified handle read. When an import outranks
every declaration, `show` follows its spelled path (`use a::b as c`, `pub use
a::b`) through at most four imports, resolving `crate::`, `self::`, and
`super::` against the importing module, and reads the declaration reached.

JSON adds `definitions` (declarations), `imports` (import rows, when nonzero),
`also` (up to five `PATH:START-END KIND` locators, tied declarations first), and
for an import trail `import: {via: [{site, path, reexport}], followed,
candidates}`, where `site` is `PATH:LINE` and `candidates` counts declarations
tied at the reached one's rank. Lines output prints `definitions: N (+M
imports)`, `also:` rows, and `via: re-export of a::b at PATH:LINE -> ... (1 of
N)`, or `import of a::b at PATH:LINE (declaration not indexed)`. A missing name
fails with `no_definition`, naming active filters and, when they excluded every
candidate, the count and best match without them.

## `refs NAME [file:P] [lang:L] [kind:K]`

Sites are occurrences named `NAME`, reported as resolved, candidate, or
unresolved. `kind:` matches the occurrence role (`call`, `declaration`,
`import`, ...) or the kind of its resolved or candidate target; `refs A::b`
keeps sites whose target or a candidate is owned by `A`. Rows order the
declaration, other code, imports (including `use` bindings), then every test
site whatever its role, each tier in path order. Sites on one line with the same
role and resolution collapse into one row whose `repeats` field holds the count
(lines output appends `×N`). Coverage adds `reference_sites`, `reference_files`,
and `reference_sites_truncated` when the site cap was reached; the coverage
summary reads `refs T sites in F files` (`T+` when capped).

## `map PATH`

A file lists types, functions, methods, and constants in source order, then
fields, variants, properties, and `mod` declarations. It omits the whole-file
module row and items of Rust `extern` blocks, which extraction marks with the
`extern block` container. A prefix lists one `kind: "file"` row per indexed
file spanning the whole file, whose `name` lists its types, then top-level
functions, constants, and macros, then `mod` declarations, comma-separated and
ending in ` +N` when more did not fit. A path with no indexed file fails with
`no_indexed_path: no indexed file under PATH; nearest: ...`, naming up to three
same-basename files or the closest entries under the deepest existing ancestor.
In a workspace, such a miss counts as no home hits; the error is returned only
when no searched member has a row.

With `--no-cache`, `map PATH` instead outlines the current root-relative file
at that path, or the per-file rows of up to 256 current files under a directory
prefix, without consulting an index or result cache. Its outline uses the same
item ordering and exclusions; a larger directory is partial and suggests a
subdirectory. `refs NAME` reads only current files containing the name and
resolves candidates among them, so cross-file targets are name candidates rather
than indexed imports. Each handle is a revision-bound `read:path:` cursor, so
`show` reads those current bytes without retaining a result set. A changed file
returns `stale_source`; traversal and symlink escapes remain invalid. `show
sym:NAME` stays index-only: run `--no-cache search 'sym:NAME'` and show its handle.

Test sites are paths with a `tests/`, `test/`, `__tests__/`, `fixtures/`, or
`testdata/` directory, `tests.rs`, `test.rs`, `*_test.*`, `*_tests.*`,
`test_*`, `*.spec.*`, or `*.test.*` files, and items or occurrences inside a
`mod tests`/`mod test` scope.
