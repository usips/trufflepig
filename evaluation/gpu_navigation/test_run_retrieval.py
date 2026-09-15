import base64
import hashlib
import json
from pathlib import Path
import os
import stat
import tempfile
import unittest
from unittest.mock import patch

from .run_retrieval import (
    PAGE_BUDGET,
    RetrievalRunner,
    hit_member_path,
    page_recall,
    run_task,
    validate_cache_separation,
)


class RetrievalRunnerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(dir="/home/josh/.cache/codex-tmp")
        self.base = Path(self.directory.name)
        self.root = self.base / "lunatic"
        self.root.mkdir()
        self.source = self.root / "source.rs"
        self.source.write_bytes(b"alpha\nbeta\ngamma\n")
        self.roots = {"lunatic": self.root}
        digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        self.task = {
            "id": "fixture",
            "corpus": "lunatic",
            "language": "rust",
            "intent": "mechanism",
            "prompt": "find the fixture",
            "query": "fixture",
            "labels": [{"path": "source.rs", "start": 0, "end": 11,
                         "sha256": digest,
                         "provenance": "read-evidence-parent-snapshot"}],
            "snapshot": {"parent_revision": "parent", "files": [
                {"path": "source.rs", "sha256": digest}
            ]},
        }

    def tearDown(self):
        self.directory.cleanup()

    def test_page_recall_uses_compact_emitted_hits_and_first_five_or_ten(self):
        response = {"hits": [
            {"member": "lunatic", "file": "file:///elsewhere/wrong.rs"},
            {"member": "lunatic", "file": f"file://{self.root}/source.rs"},
        ]}
        metrics = page_recall(self.task, response, self.roots)
        self.assertEqual(metrics["file_recall_at_5"], 1)
        self.assertEqual(metrics["file_recall_at_10"], 1)
        self.assertEqual(metrics["hits_emitted"], 2)
        self.assertEqual(hit_member_path(response["hits"][1], self.roots),
                         ("lunatic", "source.rs"))

    def test_show_evidence_requires_complete_original_bytes(self):
        repository = str(self.root)
        source_lines = [{"start": 0, "end": 6, "encoding": "utf8", "text": "alpha\n"},
                        {"start": 6, "end": 11, "encoding": "utf8", "text": "beta\n"}]

        class FakeRunner:
            arm = "newfilefirstlexical"

            def __init__(self, complete=True):
                self.complete = complete

            def __call__(self, operation, argument, *, budget):
                if operation == "search":
                    return {"operation": operation, "argument": argument, "status": "ok",
                            "complete_delivery": True, "response": {"hits": [
                                {"member": "lunatic", "file": f"file://{self_root}/source.rs",
                                 "handle": "set:1"}
                            ]}, "stdout_bytes": 10, "stderr_bytes": 0,
                            "emitted_tokens": 3, "latency_seconds": 0.1}
                return {"operation": operation, "argument": argument, "status": "ok",
                        "complete_delivery": self.complete, "response": {
                            "member": "lunatic", "repository": repository,
                            "path": "source.rs", "revision": "revision", "verified": True,
                            "lines": source_lines,
                        }, "stdout_bytes": 10, "stderr_bytes": 0,
                        "emitted_tokens": 3, "latency_seconds": 0.1}

        self_root = self.root
        record = run_task(self.task, self.roots, FakeRunner())
        self.assertEqual(record["show_validation"]["label_coverage"]["source_evidenced"], 1)
        self.assertEqual(record["show_validation"]["label_overlap_count"], 1)
        self.assertEqual(record["status"], "ok")
        incomplete = run_task(self.task, self.roots, FakeRunner(complete=False))
        self.assertEqual(incomplete["show_validation"]["label_coverage"]["source_evidenced"], 0)
        self.assertEqual(incomplete["show_validation"]["label_overlap_count"], 0)

    def test_changed_selected_source_fails_closed_after_capture(self):
        class MutatingRunner:
            arm = "oldlexical"

            def __call__(self, operation, argument, *, budget):
                self_outer.source.write_bytes(b"changed\n")
                return {"operation": operation, "argument": argument, "status": "ok",
                        "complete_delivery": True, "response": {"hits": []},
                        "stdout_bytes": 2, "stderr_bytes": 0, "emitted_tokens": 1,
                        "latency_seconds": 0.01}

        self_outer = self
        record = run_task(self.task, self.roots, MutatingRunner())
        self.assertFalse(record["snapshot_valid"])
        self.assertEqual(record["status"], "invalid_snapshot")
        self.assertIsNone(record["page"]["file_recall_at_10"])

    def test_arm_flags_and_page_budget_are_explicit(self):
        lexical = RetrievalRunner(Path("binary"), Path("workspace"), self.root,
                                   self.base / "cache", "newfilefirstlexical",
                                   self.base / "inference.toml", lambda value: len(value))
        command = lexical.command("search", "query", PAGE_BUDGET)
        self.assertIn("--no-sem", command)
        self.assertNotIn("--no-daemon", command)
        self.assertEqual(command[-2:], ["search", "query"])
        cuda = RetrievalRunner(Path("binary"), Path("workspace"), self.root,
                               self.base / "cache", "cuda", self.base / "inference.toml",
                               lambda value: len(value))
        command = cuda.command("search", "query", PAGE_BUDGET)
        self.assertIn("--sem", command)
        self.assertNotIn("--no-daemon", command)

    def test_capture_retains_raw_response_and_error_with_exact_counts(self):
        binary = self.base / "fake-binary"
        binary.write_text("#!/usr/bin/env python3\n"
                          "import json, os, sys\n"
                          "print(json.dumps({'hits': []}))\n"
                          "print(os.environ['TRUFFLEPIG_INFERENCE_CONFIG'], file=sys.stderr)\n")
        binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
        runner = RetrievalRunner(binary, self.base / "workspace", self.root,
                                 self.base / "cache", "oldlexical",
                                 self.base / "inference.toml", lambda value: len(value))
        event = runner("search", "query", budget=PAGE_BUDGET)
        self.assertEqual(json.loads(event["raw_stdout"]), {"hits": []})
        self.assertIn("inference.toml", event["raw_stderr"])
        self.assertEqual(event["emitted_tokens"],
                         len(event["raw_stdout"]) + len(event["raw_stderr"]))
        self.assertEqual(base64.b64decode(event["raw_stdout_b64"]).decode(),
                         event["raw_stdout"])

    def test_cache_paths_must_not_overlap(self):
        validate_cache_separation(self.base / "old-cache", self.base / "new-cache")
        with self.assertRaises(ValueError):
            validate_cache_separation(self.base / "cache", self.base / "cache")
        with self.assertRaises(ValueError):
            validate_cache_separation(self.base / "cache", self.base / "cache" / "old")


if __name__ == "__main__":
    unittest.main()
