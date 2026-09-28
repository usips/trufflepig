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
definition. Inheritance and trait-use references remain candidate or unresolved
evidence; PHP extraction does not expand traits, infer dynamic dispatch or
autoload results, or resolve generated XenForo XFCP chains. A conditional XFCP
helper declaration or `parent` call does not bind the generated class or chain.
Cross-file lookup uses exact fully qualified names and does not guess from a
short class name.

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

Class-extension metadata produces a `xenforo_class_extension_candidate` from
the declared implementation class to the declared base class. The edge carries
`xenforo_class_extensions_xml` provenance, the recorded `active` and
`execute_order` values, and the `runtime_xfcp_chain_unresolved` marker. An
inactive declaration is retained as metadata but produces no active target
edge. When exact indexed class lookup identifies both owning add-ons, the
resolver can also emit `xenforo_addon_extension_dependency_candidate` from
the implementation add-on to the base-class owner's add-on. It does not infer
ownership from class-name prefixes.

These edges are navigation evidence, not proof that an add-on is installed or
enabled, that XenForo will apply the declaration, or that an extension chain
will execute in the recorded order. Conditional XFCP class generation,
runtime database configuration, and the final inheritance chain are unresolved.
Use `sym:` with a short class name and `file:` to find a definition, then inspect
`ctx` or `refs` candidate details and read the manifest/XML source.

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
