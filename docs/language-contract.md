# Language extraction and relationships

## Shared evidence model

An observed occurrence records original source span, spelling, enclosing scope,
and occurrence kind. A relationship separately records its target, state, source
evidence, and resolver provenance. States distinguish statically resolved,
candidate, and unresolved sites. An occurrence does not imply a resolved edge.

Only a documented static rule establishes resolution. Name-plus-arity and guessed
receiver types remain candidates even with one candidate. Shadowing, duplicate
declarations, conditional definitions, overloads, and missing module
configuration preserve ambiguity. Tests cover successful local resolutions and
false-resolution rejection; this is a local subset, not compiler-equivalent
resolution across any of the supported languages.

Rust, TypeScript/JavaScript, and Luau use pinned Tree-sitter grammars with compiled
reference queries. Parsing has a 500 ms cancellation budget; syntax-tree depth is
bounded at 256. Invalid UTF-8 in these languages is lexical-only. Parse errors
retain observed facts with unresolved references rather than proving new targets.

Tree-sitter ABI compatibility is a supported range. `Parser` implements `Sync`,
but parsing mutates it exclusively. Cancellation yields `None`, not an available
partial tree; reset before the next independent parse.
[Tree-sitter API](https://docs.rs/tree-sitter/0.25.10/tree_sitter/struct.Parser.html)

## Rust

Extraction records definitions, lexical containers, import syntax, local
bindings, and references. Trait implementations and conditional definitions stay
distinct. Local binding references and conservative unqualified direct calls
resolve when lexical scope establishes their binding. Qualified names, imported
targets, and receiver-dependent calls remain candidates or unresolved.

Rust module/import syntax does not constitute full workspace resolution. Macro
expansion, trait selection, and type inference are outside this subset.

## TypeScript and JavaScript

Extraction includes functions, classes, fields, named arrow functions, overloads,
imports, and local bindings, including JavaScript and JSX. Local resolution
preserves shadowing and type/value distinctions where represented by the grammar.

Only a unique explicit relative static ES source path, such as `./util.ts`, can
establish an imported module target. Extensionless paths and `.js` to `.ts`
substitution remain candidates: ignored files and module-resolution settings can
change the actual target. An imported module does not prove the target of every
imported call, re-export, or runtime member.

Nearest `tsconfig.json`/`jsconfig.json` supports JSONC comments/trailing commas,
`compilerOptions.baseUrl`, and single-wildcard `paths` as candidate evidence.
Configuration with `extends` does not certify those mappings. Full Node/bundler
resolution, package exports, and compiler project references are not implemented.

## Luau

Extraction records functions, table methods, local bindings, type declarations,
module export syntax, and `require` occurrences. Local bindings can resolve;
`require` calls and module exports remain runtime candidate evidence. A spelling
match does not prove that the builtin `require` is unshadowed or exports static.

String paths use relative candidates and nearest `.luaurc` aliases. An explicit
root `trufflepig.json` selecting `{"rojo_project":"default.project.json"}` enables
Rojo `$path` mapping candidates. Instance-path requires such as
`game.Service.Child` stay candidates. Unconfigured instance names, dynamic
exports, and runtime table mutation do not establish static targets.

## DreamMaker

DreamMaker uses bounded comment/string-aware recovery over original bytes, not
a complete grammar or preprocessor. File status is `recovered` or
`recovered_incomplete`; occurrences carry `dm-recovery` provenance. The subset
records absolute/nested declarations in indentation and brace forms, procs,
verbs, locals, macros, includes, overrides, `parent_type`, and observed signals.
Token/fact/candidate bounds can omit facts and are exposed through status or
provenance. Unrecognized headers and macro-generated declarations are not a
complete object tree. Byte recovery accepts non-UTF-8 source without shifting
canonical offsets.

Same-file lexical bindings can resolve. A prior same-type proc override may
resolve when the recovered file is complete and neither includes nor conditional
ordering interfere. Inherited parent calls and project-wide include order remain
unverified candidates, including explicit `parent_type` cases. Recovery does not
infer semantic inheritance merely from lexical path ancestry.
[Upstream parent-proc implementation](https://github.com/SpaceManiac/SpacemanDMM/blob/master/crates/dreammaker/src/objtree.rs#L600)

`SEND_SIGNAL`, `RegisterSignal`, `PROC_REF`, and related spellings expose observed
evidence; they do not prove a runtime call. Includes have candidate edges and
conditional evidence without preprocessor evaluation. No `dmdoc` JSON sidecar is
assumed or implemented.
[Upstream dmdoc entry point](https://github.com/SpaceManiac/SpacemanDMM/blob/master/crates/dmdoc/src/main.rs)
