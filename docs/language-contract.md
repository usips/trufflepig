# Language extraction and relationships

## Shared evidence model

An observed occurrence records original source span, spelling, enclosing scope,
and occurrence kind. A relationship separately records its target, state, source
evidence, and resolver provenance. States distinguish statically resolved,
candidate, and unresolved sites. An occurrence does not imply a resolved edge.

Only lexical scope, explicit import/module binding, or another documented static
rule establishes resolution. Name-plus-arity and guessed receiver types remain
candidates even when there is one candidate. Shadowing, duplicate declarations,
conditional definitions, overloads, and missing module configuration preserve
ambiguity. An implementation that leaves every reference unresolved is not
useful support: fixture acceptance requires successful local static cases and
false-resolution rejection cases.

Extraction queries compile against pinned grammar versions before indexing.
Tree-sitter ABI compatibility is a supported range. `Parser` implements `Sync`,
but parsing mutates it exclusively. Cancellation yields `None`, not an available
partial tree; reset before the next independent parse.
[Tree-sitter API](https://docs.rs/tree-sitter/0.25.10/tree_sitter/struct.Parser.html)

## Rust

Extract definitions, lexical containers, import syntax, local bindings, and
reference occurrences. Preserve distinct trait implementations and conditional
definitions. Resolve local binding references and conservative direct calls when
the lexical/module binding is established. Record Rust module declarations,
inline scopes, and imports without pretending to perform macro expansion,
trait selection, or type inference. Unsupported imports remain visible evidence.

## TypeScript and JavaScript

Extract functions, classes, fields, named arrow functions, overload declarations,
imports, and local binding references, including JavaScript and JSX. Document the
implemented module-resolution subset and relevant bundler configuration alongside
its tests. Preserve type/value distinctions and alias bindings where known.

Relative modules, explicit export/import bindings, and configured aliases can
establish targets when unique under that subset. Missing configuration,
unsupported re-export chains, dynamic imports, or unknown receivers remain
candidate or unresolved evidence. Do not silently assume complete Node or
TypeScript compiler semantics.

## Luau

Extract local/global functions, table methods, local bindings, type declarations,
and module exports. Resolve supported string `require` calls through relative
paths, `.luaurc` aliases, and explicitly configured Rojo mappings. Module export
tracking ties local tables/functions to their returned binding.

Rojo instance names require an actual mapping; basename similarity is candidate
evidence. Dynamic exports, runtime table mutation, and unknown receiver values
cannot establish a static target. Missing configuration is visible.

## DreamMaker

Extract absolute and nested declarations in indentation and brace forms,
proc/verb distinctions, macros, includes, override occurrences, `parent_type`,
and observed signal registrations/sends. Preserve source order and conditional
include evidence. A repeated proc name does not collapse distinct declarations.

Bounded comment/string-aware recovery handles unsupported declaration headers
and marks recovered facts. It must not interpret declarations inside comments or
strings. Original encoding and offsets remain intact.

Parent calls require established include/override ordering and semantic
inheritance. The previous implementation of a proc on the same type may precede
traversal to an inherited type; lexical path ancestry alone is insufficient.
[Upstream parent-proc implementation](https://github.com/SpaceManiac/SpacemanDMM/blob/master/crates/dreammaker/src/objtree.rs#L600)

Signal macros and `PROC_REF`/`TYPE_PROC_REF` patterns produce observed evidence;
they do not prove a runtime call. Conditional includes and macro expansion may
leave ordering or targets unresolved. `dmdoc` is not an assumed JSON sidecar; its
actual interface must be checked before any optional integration.
[Upstream dmdoc entry point](https://github.com/SpaceManiac/SpacemanDMM/blob/master/crates/dmdoc/src/main.rs)
