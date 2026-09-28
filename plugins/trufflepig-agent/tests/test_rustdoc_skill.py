"""Rustdoc enrichment preserves scope, explicit relationships, and build failures."""

import argparse
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1] / "skills/trufflepig-code-search/scripts"
sys.path.insert(0, str(SCRIPTS))
import rustdoc_build
from rustdoc_query import query_document


def item(ident, name, kind, body, docs=None, crate_id=0):
    return {"id": ident, "name": name, "crate_id": crate_id, "docs": docs,
            "span": {"filename": "src/lib.rs", "begin": [ident + 1, 1],
                     "end": [ident + 2, 1]}, "inner": {kind: body}}


def document():
    values = [
        item(0, "sample", "module", {"items": [1, 2, 3]}),
        item(1, "Token", "struct", {"impls": [4, 5, 6, 8]}, "A bounded access token."),
        item(2, "Token", "struct", {"impls": []}, "A different token."),
        item(3, "Decode", "trait", {"implementations": [4]}),
        item(4, None, "impl", {"trait": {"path": "Decode", "id": 3},
                              "for": {"resolved_path": {"path": "Token", "id": 1}},
                              "items": [7], "is_synthetic": False, "blanket_impl": None}),
        item(5, None, "impl", {"trait": {"path": "Send", "id": 20},
                              "items": [], "is_synthetic": True, "blanket_impl": None}),
        item(6, None, "impl", {"trait": {"path": "Borrow", "id": 21},
                              "items": [], "is_synthetic": False,
                              "blanket_impl": {"generic": "T"}}),
        item(7, "decode", "function", {"sig": {"inputs": [], "output": None}}),
        item(8, None, "impl", {"trait": None, "items": [], "is_synthetic": False,
                              "blanket_impl": None}),
        item(9, "External", "struct", {"impls": []}, "bounded access token", crate_id=1),
    ]
    return {"format_version": 61, "root": 0,
            "index": {str(i["id"]): i for i in values},
            "paths": {"1": {"path": ["sample", "Token"]},
                      "2": {"path": ["sample", "nested", "Token"]},
                      "3": {"path": ["sample", "Decode"]}}}


class RustdocQueryTests(unittest.TestCase):
    def test_docs_search_excludes_external_items(self):
        result = query_document(document(), "search", "BOUNDED token")
        self.assertEqual(result["matches"], 1)
        self.assertEqual(result["results"][0]["path"], "sample::Token")

    def test_exact_selection_preserves_ambiguity_and_paginates(self):
        result = query_document(document(), "item", "Token", limit=1)
        self.assertEqual(result["matches"], 2)
        self.assertEqual(result["next_offset"], 1)
        second = query_document(document(), "item", "Token", limit=1, offset=1)
        self.assertNotEqual(result["results"][0]["id"], second["results"][0]["id"])
        exact = query_document(document(), "item", "sample::nested::Token")
        self.assertEqual(exact["matches"], 1)
        self.assertEqual(exact["results"][0]["id"], 2)

    def test_impls_follow_ids_instead_of_matching_type_names(self):
        result = query_document(document(), "impls", "sample::nested::Token")
        self.assertEqual(result["matched_owners"], 1)
        self.assertEqual(result["matches"], 0)
        result = query_document(document(), "impls", "sample::Token")
        self.assertEqual({i["id"] for i in result["results"]}, {4, 8})

    def test_trait_implementors_and_trait_filter(self):
        result = query_document(document(), "impls", "Decode")
        self.assertEqual(result["results"][0]["id"], 4)
        result = query_document(document(), "impls", "Token", trait="sample::Decode")
        self.assertEqual(result["matches"], 1)
        self.assertEqual(result["results"][0]["detail"]["items_preview"][0]["name"], "decode")

    def test_blanket_and_synthetic_are_explicit_opt_in(self):
        result = query_document(document(), "impls", "Token", include_blanket=True, limit=20)
        self.assertEqual({i["id"] for i in result["results"]}, {4, 5, 6, 8})

    def test_missing_paths_and_long_content_are_labelled(self):
        doc = document()
        doc["index"]["7"]["docs"] = "x" * 900
        doc["index"]["7"]["inner"]["function"]["generics"] = "x" * 3000
        result = query_document(doc, "item", "decode")["results"][0]
        self.assertIsNone(result["path"])
        self.assertTrue(result["docs_truncated"])
        self.assertTrue(result["detail_truncated"])
        self.assertLessEqual(len(result["detail_json_excerpt"]), 2400)

    def test_unknown_schema_is_rejected(self):
        doc = document()
        doc["format_version"] = 999
        with self.assertRaisesRegex(ValueError, "Unsupported rustdoc JSON"):
            query_document(doc, "search", "Token")


class RustdocBuildTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix="trufflepig-rustdoc-test-")
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.manifest = self.root / "Cargo.toml"
        self.manifest.write_text('[package]\nname="sample"\nversion="0.1.0"\n')
        self.metadata = {"workspace_root": str(self.root), "workspace_members": ["sample-id"],
                         "target_directory": str(self.root / "target"),
                         "packages": [{"id": "sample-id", "name": "sample",
                                       "manifest_path": str(self.manifest),
                                       "targets": [{"name": "sample", "kind": ["lib"]}]}]}
        self.artifact = self.root / "target/test-host/doc/sample.json"
        self.artifact.parent.mkdir(parents=True)
        self.args = argparse.Namespace(manifest_path=str(self.manifest), toolchain="nightly",
                                       target=None, package=None, bin=None,
                                       features="extra", no_default_features=True)

    def capture(self, command, cwd):
        if "rustc" in command:
            return "rustc test\nhost: test-host"
        return json.dumps(self.metadata)

    def test_success_records_configuration_and_new_artifact(self):
        self.artifact.write_text("old data")

        def build(command, cwd, package_id, crate):
            self.assertFalse(self.artifact.exists())
            self.assertIn("--locked", command)
            self.assertIn("--no-default-features", command)
            self.assertEqual(command[command.index("--features") + 1], "extra")
            self.artifact.write_text(json.dumps(document()))
            return ["extra"]

        with patch.object(rustdoc_build, "capture", self.capture), \
                patch.object(rustdoc_build, "run_rustdoc", build):
            doc, provenance = rustdoc_build.build_document(self.args)
        self.assertEqual(doc["format_version"], 61)
        self.assertEqual(provenance["resolved_features"], ["extra"])
        self.assertEqual(provenance["target"], "test-host")
        self.assertEqual(provenance["source_root"], str(self.root))
        self.assertEqual(len(provenance["artifact_sha256"]), 64)

    def test_failure_never_returns_previous_artifact(self):
        self.artifact.write_text(json.dumps(document()))
        with patch.object(rustdoc_build, "capture", self.capture), \
                patch.object(rustdoc_build, "run_rustdoc",
                             side_effect=subprocess.CalledProcessError(101, ["cargo"])):
            with self.assertRaises(subprocess.CalledProcessError):
                rustdoc_build.build_document(self.args)
        self.assertFalse(self.artifact.exists())

    def test_virtual_workspace_requires_explicit_package(self):
        metadata = copy.deepcopy(self.metadata)
        other = copy.deepcopy(metadata["packages"][0])
        other.update(id="other-id", name="other", manifest_path=str(self.root / "other/Cargo.toml"))
        metadata["packages"].append(other)
        metadata["workspace_members"].append("other-id")
        virtual = self.root / "workspace/Cargo.toml"
        with self.assertRaisesRegex(ValueError, "--package"):
            rustdoc_build.select_package(metadata, virtual, None)
        self.assertEqual(rustdoc_build.select_package(metadata, virtual, "other"), other)


if __name__ == "__main__":
    unittest.main()
