"""Regression checks for frozen-corpus integrity and honest recall accounting."""

import hashlib
from pathlib import Path
import tempfile
import unittest

import replay


class RecallAccountingTests(unittest.TestCase):
    def setUp(self):
        self.task = {"relevant": [{"path": "a.rs", "start": 10, "end": 20}]}

    def test_wrong_span_in_relevant_file_remains_a_span_miss(self):
        result = {"hits": [{"path": "a.rs", "start": 30, "end": 40}]}
        metrics = replay.recall_metrics(self.task, result)
        self.assertEqual(metrics["file_recall_at_5"], 1)
        self.assertEqual(metrics["span_recall_at_10"], 0)
        self.assertTrue(metrics["miss"])

    def test_duplicates_cannot_inflate_recall(self):
        self.task["relevant"].append({"path": "b.rs", "start": 0, "end": 5})
        result = {"hits": [{"path": "a.rs", "start": 10, "end": 20}] * 10}
        metrics = replay.recall_metrics(self.task, result)
        self.assertEqual(metrics["span_recall_at_10"], 0.5)
        self.assertEqual(metrics["file_recall_at_10"], 0.5)

    def test_unavailable_and_empty_results_keep_misses(self):
        metrics = replay.recall_metrics(self.task, {"hits": [], "status": "unavailable"})
        self.assertTrue(metrics["miss"])
        self.assertEqual(metrics["span_recall_at_10"], 0)

    def test_half_open_boundaries_do_not_overlap(self):
        result = {"hits": [{"path": "a.rs", "start": 20, "end": 21}]}
        self.assertTrue(replay.recall_metrics(self.task, result)["miss"])


class FrozenSnapshotTests(unittest.TestCase):
    def setUp(self):
        scratch = Path("/home/josh/.cache/codex-tmp")
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="eval-test-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        (self.root / "a.rs").write_bytes(b"fn hello() {}\n")
        self.manifest = {
            "schema_version": 1,
            "snapshot": {"files": {"a.rs": hashlib.sha256(b"fn hello() {}\n").hexdigest()}},
            "tasks": [{"id": "hello", "relevant": [
                {"path": "a.rs", "start": 3, "end": 8}]}],
        }

    def test_source_change_invalidates_frozen_labels(self):
        replay.verify_snapshot(self.root, self.manifest)
        (self.root / "a.rs").write_bytes(b"fn world() {}\n")
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            replay.verify_snapshot(self.root, self.manifest)

    def test_added_source_cannot_silently_change_corpus(self):
        (self.root / "answer.rs").write_text("fn post_change_answer() {}")
        with self.assertRaisesRegex(ValueError, "file set changed"):
            replay.verify_snapshot(self.root, self.manifest)

    def test_labels_outside_frozen_source_are_rejected(self):
        self.manifest["tasks"][0]["relevant"][0]["end"] = 999
        with self.assertRaisesRegex(ValueError, "outside source bytes"):
            replay.verify_snapshot(self.root, self.manifest)

    def test_paths_cannot_leave_frozen_root(self):
        with self.assertRaisesRegex(ValueError, "escapes corpus root"):
            replay.source_path(self.root, "../outside.rs")


if __name__ == "__main__":
    unittest.main()
