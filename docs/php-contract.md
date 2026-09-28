# PHP and XenForo navigation

## PHP extraction

`.php` and `.phtml` map to `php` and use pinned `tree-sitter-php` 0.24.2.
Extraction records namespaces, classes, interfaces, traits, enums, functions,
methods, properties, constants, enum cases, parameters, variables, imports, and
identifier occurrences. Definition spans cover declaration nodes; occurrence
spans use original source bytes, including `$` for PHP variables. Parse statuses
are `complete`, `parse_error`, `invalid_encoding`, `incompatible_grammar`,
`cancelled`, and `depth_limit`; parse errors retain safe observations but do not
establish confident links.

Definition names stay short. Class-like declarations use their namespace as the
container, functions use their namespace, and members use the containing class's
fully qualified name. Namespace-relative and imported type/import path
occurrences use normalized fully qualified names without a leading slash;
explicit absolute names keep their leading slash. Normalized names can differ
from the text at the occurrence span.

Only reads of a unique, unambiguous parameter declared in the same function can
resolve. Assignment-bound variables remain candidates, even with one apparent
assignment, because the extractor does not establish branch visibility,
assignment order, or dynamic-variable behavior. Calls, imports, and type
references remain candidates or unresolved, even when one target is found,
including an absolute fully qualified type or import name matching exactly one
definition. The PHP and XenForo resolver can add bounded, source-derived
inheritance and literal `parent::method()` candidates as described below. These
candidates do not make extraction compiler-equivalent or prove runtime dispatch.
Trait-use references remain candidate or unresolved evidence; traits are not
expanded. A conditional XFCP helper declaration does not bind a generated
class. Cross-file lookup uses exact fully qualified names and does not guess
from a short class name.

## XenForo add-on metadata

The metadata resolver reads `addon.json` and `_data/class_extensions.xml` from
indexed source content. It does not read live activation state from the XenForo
database or discover files outside the index. Add-on requirement occurrences
span the quoted key in `addon.json`'s `require` object, not its version/value;
XML metadata occurrences span their attribute values. A class-extension
relationship cites the full `<extension>` element as its evidence span.

Known local add-on IDs in `addon.json`'s `require` object produce
`xenforo_addon_requirement_candidate` edges from the requiring add-on to the
required local add-on. Unknown requirements, including PHP or XenForo framework
requirements without a local add-on target, remain observations without an
add-on edge.

Each class-extension declaration has a `xenforo_class_extension` definition
and `xenforo_class_extension_candidate` links to exact indexed base-class
candidates. Its `to_class` occurrence separately lists matching PHP class
definitions. Provenance records `active`, `execute_order`, XML source, and
`runtime_xfcp_chain_unresolved`. An inactive declaration remains metadata but
has no active chain edge. The XML owner's add-on can differ from the add-on
defining `to_class`; the resolver keeps those owners distinct. A
`xenforo_addon_extension_dependency_candidate`, when emitted, links the XML
owner to exact indexed base owners and requires unique implementation and base
owner evidence. It does not infer the PHP implementation's owner from the XML
or class-name prefixes. Malformed extension XML invalidates the whole file;
when its owner is known, a file-scoped `inheritance_issue` cites its XML range,
and valid declarations in other files remain usable.

These edges are navigation evidence, not proof that an add-on is installed or
enabled, that XenForo will apply the declaration, or that an extension chain
will execute in the recorded order. The source resolver treats registrations
that pass the indexed metadata checks as applicable while constructing
candidates; it cannot read runtime enabled-add-on state or database collation.
`runtime_xfcp_chain_unresolved` continues to mark the gap between source
candidates and the runtime chain.

## Declared inheritance and literal parent calls

The inheritance resolver uses indexed PHP source and indexed XenForo
registration source only. It does not query the XenForo database, load classes,
run add-on code, or establish which add-ons are enabled. A candidate chain
assumes the indexed registrations that pass their source checks apply; this is
navigation evidence, not a claim about the live XFCP chain.

Ordinary PHP `extends` names use namespace/import resolution followed by exact
indexed class lookup. XF metadata alias mapping is separate: supported literal
alias hooks in indexed `XF.php` canonicalize only metadata base names; an absent
hook means identity, while a present unsupported hook blocks mapping. The
hooks are not run and aliases are not inferred. PHP import aliases are resolved
before matching an `extends XFCP_*` parent; metadata alias rules never rewrite
PHP imports. The raw XML `to_class` spelling defines extender identity and its
generated proxy spelling.

Framework extension declarations produce `framework_parent_candidate` edges
from an implementation class to its immediate predecessor candidate. Ordinary
PHP declarations produce `php_extends_candidate` edges. Duplicate actual PHP
class identities block framework/proxy targets and emit `inheritance_issue`;
metadata lookup can retain the matching class candidates. Reusing one actual
implementation under different raw proxy names blocks composition for that
base and creates no self-edges. Missing classes are omitted, never spliced into
a shortcut to a later ancestor.

Only XML `active="0"` and `active="1"` are recognized. Zero remains metadata
without an active edge; one is eligible source evidence; absent or other values
remain unknown and block composition. Negative `execute_order` is invalid and
blocks composition for that base. Strictly different valid numeric orders
establish order;
equal orders remain candidate alternatives marked by `priority_tie`, without a
database-collation or lexical tie-break. Missing, nonnumeric, or negative order
produces an `inheritance_issue`. If base alias mapping fails, independent raw
implementation evidence remains available but does not compose into a chain.

A literal `parent::method()` call can produce a `php_parent_call_candidate` to
the nearest ancestor's own concrete `public` or `protected` declaration of that
method. The search follows candidate framework and ordinary PHP parent edges.
It does not bind `$this->method()`, `static::method()`, dynamic calls, or other
general member references. It does not flatten trait methods or search
interfaces. A private or abstract method declaration, trait boundary, incomplete
or conditional class/method declaration, parse failure, missing parent source,
or ambiguous parent path is an `inheritance_issue` barrier; traversal does not
guess past it. Duplicate or reused proxy names and unknown extension `active`
state also produce issue evidence when anchored. Conditional or generated
runtime class definitions do not become source declarations.

The three parent-link kinds remain candidate evidence; naming a target class
does not establish the exact runtime chain. `inheritance_issue` reports
incomplete evidence, not an alternative parent. Expansion is bounded to 64
classes per chain including the base, 64 parent-call candidates, and 64 visited
classes per traversal including the caller. A 200,000-item derived-stage budget
covers bounded preparation and publication and is charged before retaining
edges and candidate IDs. `ctx` includes at most 100 graph edges within its
normal token budget, so a context response can end before the full candidate
graph. Reaching a bound or a source barrier is reported as incomplete evidence,
not as proof that no parent exists. A global derived-stage failure aborts the
staged publication, leaving the last complete publication available with no
partial expansion. Internal `php_markers` remain private and do not appear as
definitions, occurrences, relationships, or provenance. Public relationship
kinds and provenance, including `runtime_xfcp_chain_unresolved`, remain visible.

Use `sym:` with a short class name and `file:` to find a definition, then inspect
`ctx` or `refs` candidate details and read the source registrations.

## Real checkout acceptance

The XenForo acceptance script reads the checkout in place and leaves its source
read-only. It exercises search, `show`, `map`, `refs`, and `ctx` across XenForo
core and add-on files, and reports aggregate PHP parse statuses. Index coverage
follows the normal scanner ignore policy and source bounds: ignored files are
not indexed or counted, so the report describes published PHP files rather than
every PHP file on disk. The gate requires at least 5,000 indexed PHP files, at
least 95% `complete` status, and complete status for its core and add-on fixture
files. Its JSON report includes the status counts and failed checks. Build the
binary separately. With `--new-build`, the runner scopes the system directory
and spool to the cache root, starts private root and system services with that
binary, waits for index publication, then routes CLI requests through its
private router. It stops and reaps only those two owned processes when the run
ends.

Run it with a dedicated cache under the disk-backed scratch root:

```sh
python3 evaluation/php_xenforo.py \
  --binary target/release/trufflepig \
  --xf-root /home/josh/Source/xf_kiwifarms \
  --cache-root /home/josh/.cache/codex-tmp/xf-php-final \
  --new-build
```

The script keeps its index and temporary files under the supplied cache root.
The caller removes that dedicated cache directory after the run when its results
are no longer needed.
