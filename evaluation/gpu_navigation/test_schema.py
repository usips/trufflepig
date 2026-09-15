import copy
import json
import tempfile
from pathlib import Path
import unittest

from .schema import (
    MAX_EMITTED_TOKENS,
    MAX_OUTPUT_TOKENS_PER_CALL,
    MAX_TOOL_CALLS,
    TASK_TIMEOUT_SECONDS,
    grade_task,
    load_manifest,
    solver_task,
    validate_manifest,
)


class GpuNavigationSchemaTests(unittest.TestCase):
    def test_manifest_has_balanced_development_tasks(self):
        manifest = load_manifest()
        counts = {name: sum(task["corpus"] == name for task in manifest["tasks"])
                  for name in manifest["corpora"]}
        self.assertEqual(counts, {"lunatic": 4, "tales-from-space": 4, "tgstation": 4})
        self.assertFalse(manifest["held_out_claim"])

    def test_solver_view_redacts_grading_coordinates(self):
        task = load_manifest()["tasks"][0]
        view = solver_task(task, arm="cuda")
        self.assertEqual(view["id"], task["id"])
        self.assertEqual(view["retrieval_arm"], "cuda")
        for private in ("labels", "snapshot", "sha256", "start", "end"):
            self.assertNotIn(private, view)
        self.assertNotIn(".rs", str(view))

    def test_checked_in_solver_bundle_matches_public_views(self):
        package = Path(__file__).parent
        public = json.loads((package / "solver_prompts.json").read_text())
        manifest = load_manifest()
        self.assertEqual(public["tasks"], [solver_task(task) for task in manifest["tasks"]])
        keys = set()
        def collect(value):
            if isinstance(value, dict):
                keys.update(value)
                for child in value.values():
                    collect(child)
            elif isinstance(value, list):
                for child in value:
                    collect(child)
        collect(public)
        for private in ("labels", "snapshot", "sha256", "parent_revision", "start", "end"):
            self.assertNotIn(private, keys)

    def test_invalid_task_hash_or_split_is_rejected(self):
        manifest = load_manifest()
        bad = copy.deepcopy(manifest)
        bad["tasks"][0]["labels"][0]["sha256"] = "0" * 64
        with self.assertRaises(ValueError):
            validate_manifest(bad)
        bad = copy.deepcopy(manifest)
        bad["split"] = "held-out"
        with self.assertRaises(ValueError):
            validate_manifest(bad)

    def test_grade_requires_show_bytes_and_rejects_gaps(self):
        with tempfile.TemporaryDirectory(dir="/home/josh/.cache/codex-tmp") as directory:
            root = Path(directory)
            (root / "source.rs").write_bytes(b"alpha\nbeta\ngamma\n")
            task = {
                "id": "fake",
                "corpus": "lunatic",
                "labels": [{"path": "source.rs", "start": 0, "end": 16,
                             "sha256": "0" * 64,
                             "provenance": "read-evidence-parent-snapshot"}],
                "snapshot": {"parent_revision": "fake", "files": []},
            }
            roots = {"lunatic": root}
            search = {"operation": "search", "response": {
                "hits": [{"member": "lunatic", "file": "file:source.rs", "handle": "set:1"}]}}
            show = {"operation": "show", "response": {
                "member": "lunatic", "repository": str(root), "path": "source.rs",
                "revision": "rev", "verified": True,
                "lines": [{"start": 0, "end": 6, "encoding": "utf8",
                            "text": "alpha\n"},
                           {"start": 12, "end": 18, "encoding": "utf8",
                            "text": "gamma\n"}]}}
            result = grade_task(task, [search, show], roots)
            self.assertEqual(result["metadata_discovered"], 1)
            self.assertEqual(result["source_evidenced"], 0)
            self.assertEqual(result["status"], "incomplete")

    def test_limits_are_the_paired_trial_contract(self):
        self.assertEqual((MAX_TOOL_CALLS, MAX_EMITTED_TOKENS,
                          MAX_OUTPUT_TOKENS_PER_CALL, TASK_TIMEOUT_SECONDS),
                         (12, 12000, 900, 600))


if __name__ == "__main__":
    unittest.main()
