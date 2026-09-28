#!/usr/bin/env python3
"""Query freshly built rustdoc items without adding them to the Trufflepig index."""

import argparse
import json
import sys
import subprocess

from rustdoc_build import build_document


SUPPORTED_FORMAT = 61


class RustdocIndex:
    def __init__(self, document):
        if document.get("format_version") != SUPPORTED_FORMAT:
            raise ValueError(f"Unsupported rustdoc JSON format {document.get('format_version')}; "
                             f"helper supports {SUPPORTED_FORMAT}. Use a compatible toolchain "
                             "or update and test the reader before consuming this schema.")
        self.items = document["index"]
        self.paths = document["paths"]
        self.crate_id = self.items[str(document["root"])]["crate_id"]

    def path(self, item):
        path = self.paths.get(str(item["id"]), {}).get("path")
        return "::".join(path) if path else None

    def local_items(self):
        return [i for i in self.items.values() if i["crate_id"] == self.crate_id]

    def select(self, query, exact=False):
        terms = query.casefold().split()
        selected = []
        for item in self.local_items():
            name = item.get("name")
            if not name:
                continue
            path = self.path(item) or ""
            if exact:
                match = query in (name, path)
            else:
                text = f"{name} {path} {item.get('docs') or ''}".casefold()
                match = all(term in text for term in terms)
            if match:
                selected.append(item)
        return sorted(selected, key=lambda i: (
            (i["name"] or "").casefold() != query.casefold(),
            self.path(i) or i["name"],
            json.dumps(i.get("span"), sort_keys=True), str(i["id"])))

    def implementations(self, owners, include_blanket, trait):
        ids = set()
        for item in owners:
            kind, body = next(iter(item["inner"].items()))
            if kind in ("struct", "enum", "union", "primitive"):
                ids.update(map(str, body.get("impls", [])))
            elif kind == "trait":
                ids.update(map(str, body.get("implementations", [])))
        implementations = [self.items[i] for i in ids if i in self.items]
        implementations = [i for i in implementations if "impl" in i["inner"]]
        if not include_blanket:
            implementations = [i for i in implementations
                               if not i["inner"]["impl"].get("is_synthetic")
                               and i["inner"]["impl"].get("blanket_impl") is None]
        if trait:
            def matches_trait(item):
                reference = item["inner"]["impl"].get("trait") or {}
                path = self.paths.get(str(reference.get("id")), {}).get("path", [])
                return trait in (reference.get("path"), "::".join(path))
            implementations = [i for i in implementations if matches_trait(i)]
        return sorted(implementations, key=lambda i: (
            json.dumps(i.get("span"), sort_keys=True), str(i["id"])))

    def summary(self, item, details=False):
        kind, body = next(iter(item["inner"].items()))
        docs = item.get("docs") or ""
        result = {"id": item["id"], "name": item["name"], "path": self.path(item),
                  "kind": kind, "span": item.get("span"), "docs": docs[:800],
                  "docs_truncated": len(docs) > 800}
        if details:
            # Preserve rustdoc's type representation instead of inventing Rust syntax.
            detail = dict(body) if isinstance(body, dict) else body
            if isinstance(detail, dict):
                for key in ("impls", "implementations", "items", "variants"):
                    if key in detail:
                        ids = detail.pop(key)
                        detail[f"{key}_count"] = len(ids)
                        detail[f"{key}_preview"] = [
                            {"id": i, "name": self.items.get(str(i), {}).get("name")}
                            for i in ids[:12]]
            encoded = json.dumps(detail, ensure_ascii=True, separators=(",", ":"))
            if len(encoded) <= 2400:
                result["detail"] = detail
            else:
                result["detail_json_excerpt"] = encoded[:2400]
            result["detail_truncated"] = len(encoded) > 2400
        return result


def query_document(document, mode, query, limit=3, offset=0, include_blanket=False, trait=None):
    index = RustdocIndex(document)
    owners = index.select(query, exact=mode != "search")
    selected = index.implementations(owners, include_blanket, trait) if mode == "impls" else owners
    page = selected[offset:offset + limit]
    result = {"query": query, "mode": mode, "matches": len(selected), "offset": offset,
              "results": [index.summary(i, details=mode != "search") for i in page],
              "next_offset": offset + limit if offset + limit < len(selected) else None}
    if mode == "impls":
        result["matched_owners"] = len(owners)
        result["include_blanket_and_synthetic"] = include_blanket
        result["trait_filter"] = trait
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("search", "item", "impls"))
    parser.add_argument("query", help="search words, or exact item name/canonical path")
    parser.add_argument("--manifest-path", default="Cargo.toml")
    parser.add_argument("--package", help="workspace package; required for ambiguous workspaces")
    parser.add_argument("--bin", help="document this binary instead of the library")
    parser.add_argument("--toolchain", default="nightly", help="installed rustup toolchain")
    parser.add_argument("--target", help="target triple; defaults explicitly to the compiler host")
    parser.add_argument("--features", default="")
    parser.add_argument("--no-default-features", action="store_true")
    parser.add_argument("--include-blanket", action="store_true")
    parser.add_argument("--trait", help="filter impls by exact trait name or canonical path")
    parser.add_argument("--limit", type=int, default=3)
    parser.add_argument("--offset", type=int, default=0)
    args = parser.parse_args()
    if not args.query.strip() or not 1 <= args.limit <= 20 or args.offset < 0:
        parser.error("query must be nonempty; limit must be 1..20; offset must be nonnegative")
    if args.mode != "impls" and (args.trait or args.include_blanket):
        parser.error("--trait and --include-blanket apply only to impls")
    try:
        document, provenance = build_document(args)
        result = query_document(document, args.mode, args.query, args.limit,
                                args.offset, args.include_blanket, args.trait)
        print(json.dumps({"provenance": provenance, **result}, ensure_ascii=True, indent=2))
    except (OSError, ValueError, KeyError, TypeError, StopIteration,
            subprocess.CalledProcessError) as error:
        print(f"rustdoc enrichment unavailable: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
