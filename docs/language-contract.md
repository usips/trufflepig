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

Rust, TypeScript/JavaScript, and Luau use pinned Tree-sitter grammars with
compiled reference queries. C# and PHP use pinned `tree-sitter-c-sharp` and
`tree-sitter-php` grammars. Parsing has a 500 ms cancellation budget;
syntax-tree depth is bounded at 256. Invalid UTF-8
leaves lexical indexing available but no structural facts, with
`invalid_encoding` status. Parse errors retain recognized syntax observations
with unresolved references rather than proving new targets.

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

The Codex skill's optional [rustdoc workflow](../plugins/trufflepig-agent/skills/trufflepig-code-search/references/rustdoc.md)
provides compiled item documentation and explicit trait/implementation links for
one Cargo target and feature configuration. This evidence remains outside the
search index and does not upgrade body occurrences to resolved relationships.
Current source is verified separately before relying on generated spans.

## C#

The `.cs` extension maps to `csharp`; `cs` and `c#` are query aliases. Extraction
records namespaces, classes, interfaces, structs, records, enums, delegates,
methods, constructors, properties, fields, locals, parameters, and identifier
occurrences with original-byte spans. Razor `.cshtml` files remain text-searchable
without C# structural extraction.

The pinned `tree-sitter-c-sharp` 0.23.5 grammar reports `parse_error` on BTCPayServer
list/slice property patterns such as `[.. { } multis]`; recognized facts remain
partial and references at those sites remain unresolved.

Lexical local and parameter references may resolve only when one visible binding
is unique. Overloads, members, partial type merges, inheritance, qualified names,
and receiver-dependent or method calls remain candidates or unresolved. The
subset does not provide compiler-equivalent binding resolution.

## PHP

`.php` and `.phtml` map to `php` and use pinned `tree-sitter-php` 0.24.2.
Extraction records namespaces, classes, interfaces, traits, enums, functions,
methods, properties, constants, enum cases, parameters, variables, imports, and
identifier occurrences with original-byte spans. Inheritance and trait-use
references remain candidate or unresolved evidence. The bounded PHP/XenForo
resolver adds source-derived inheritance and literal `parent::method()`
candidates without proving live XFCP state or general member dispatch. See the
[PHP and XenForo contract](php-contract.md) for exact limits and relationships.

## TypeScript and JavaScript

Extraction includes functions, classes, fields, named arrow functions, overloads,
imports, and local bindings. TypeScript covers `.ts`, `.tsx`, `.mts`, and `.cts`;
JavaScript covers `.js`, `.jsx`, `.mjs`, and `.cjs`. Local resolution preserves
shadowing and type/value distinctions where represented by the grammar. Functions
assigned to static member properties are recorded; calls through those properties
remain candidates because receiver binding is not inferred.

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
