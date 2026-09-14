"""Member provenance, bounded oracle navigation, and evidence accounting."""

import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from records import UsageRecord
from workspace_replay import (WorkspaceRunner, member_roots, navigate, replay, source_evidence,
                              validate_manifest, verify_expected)


class WorkspaceReplayTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(dir="/home/josh/.cache/codex-tmp")
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.roots = {name: self.base / name for name in ("engine", "pack")}
        for root in self.roots.values():
            root.mkdir()
            (root / "same.rs").write_bytes(b"first\nsecond\nthird\n")
        self.task = dict(id="member-evidence", query="needle", expected=[
            dict(member="pack", path="same.rs")])
        self.calls = []

    def hit(self, member="pack", handle="set:1", start=0, end=6):
        return dict(member=member, path="same.rs", revision="revision", handle=handle,
                    start=start, end=end, member_rank=4)

    def page(self, hits=None, **extra):
        return dict(hits=hits if hits is not None else [self.hit()],
                    members={name: str(path) for name, path in self.roots.items()}, **extra)

    def source(self, member="pack", start=0, end=6, **extra):
        text = (self.roots[member] / "same.rs").read_bytes()[start:end].decode()
        return dict(member=member, repository=str(self.roots[member]), path="same.rs",
                    revision="revision", verified=True,
                    lines=[dict(start=start, end=end, encoding="utf8", text=text)], **extra)

    def run_responses(self, responses):
        responses = copy.deepcopy(responses)
        def run(operation, argument):
            self.calls.append((operation, argument))
            response = responses.pop(0)
            status = response.pop("_status", "ok")
            delivered = response.pop("_complete", True)
            return response, status, delivered, UsageRecord(
                1, 20, 0, 0.01, 10, "o200k_base", not delivered)
        return navigate(self.task, self.roots, run)

    def test_same_path_in_different_member_is_not_evidence(self):
        result = self.run_responses([self.page([self.hit("engine")])])
        self.assertEqual(result["outcome"]["metadata_discovered"], 0)
        self.assertEqual(result["outcome"]["source_evidenced"], 0)
        self.assertEqual(result["outcome"]["missed_evidence"], self.task["expected"])

    def test_metadata_does_not_count_as_source(self):
        result = self.run_responses([self.page(), {"_status": "stale_source"}])
        self.assertEqual(result["outcome"]["metadata_discovered"], 1)
        self.assertEqual(result["outcome"]["source_evidenced"], 0)
        self.assertEqual(result["events"][0]["identities"][0]["member_rank"], 4)
        self.assertTrue(result["outcome"]["evidence_cost_censored"])

    def test_wrong_repository_or_member_cannot_credit_source(self):
        selected = dict(self.hit(), repository=str(self.roots["pack"]))
        source = self.source("engine")
        self.assertEqual(source_evidence(source, selected, self.roots), [])
        source["member"] = "pack"
        self.assertEqual(source_evidence(source, selected, self.roots), [])
        page = self.page()
        page["members"]["pack"] = str(self.roots["engine"])
        result = self.run_responses([page])
        self.assertEqual(result["outcome"]["metadata_discovered"], 0)

    def test_complementary_members_require_individual_reads(self):
        self.task["expected"].append(dict(member="engine", path="same.rs"))
        result = self.run_responses([
            self.page([self.hit(), self.hit("engine", "set:2")]),
            self.source(), self.source("engine")])
        self.assertEqual(result["outcome"]["source_evidenced"], 2)
        self.assertEqual(result["outcome"]["tokens_to_complete_evidence"], 30)
        self.assertEqual(result["outcome"]["status"], "complete")

    def test_frozen_followup_reserves_result_page_and_retains_cost(self):
        self.task["followup_queries"] = ["documents append"]
        self.task["expected"].append(dict(member="engine", path="same.rs"))
        result = self.run_responses([
            self.page(next="set@1"), self.source(), self.page([], next="set@2"),
            self.page([self.hit("engine", "engine:1")]), self.source("engine")])
        self.assertIn(("search", "documents append"), self.calls)
        self.assertEqual(sum(operation in ("search", "more") for operation, _ in self.calls), 3)
        self.assertEqual(result["outcome"]["usage"]["tool_calls"], 5)

    def test_byte_evidence_follows_only_returned_continuation(self):
        self.task["expected"][0].update(start=0, end=12)
        result = self.run_responses([self.page([self.hit(end=18)]),
                                     self.source(next="read:immutable@6"),
                                     self.source(start=6, end=12)])
        self.assertEqual(self.calls[-1], ("show", "read:immutable@6"))
        self.assertEqual(result["outcome"]["status"], "complete")

    def test_required_context_checks_member_and_subject_identity(self):
        self.task["requires_ctx"] = True
        context = dict(member="pack", repository=str(self.roots["pack"]),
                       hit=self.hit(), relationships=[dict(kind="calls")], truncated=False)
        result = self.run_responses([self.page(), self.source(), context])
        self.assertTrue(result["outcome"]["context_satisfied"])
        self.assertEqual(self.calls[-1], ("ctx", "set:1"))
        context["hit"]["revision"] = "changed"
        result = self.run_responses([self.page(), self.source(), context])
        self.assertFalse(result["outcome"]["context_satisfied"])

    def test_partial_delivery_and_changed_source_preserve_misses(self):
        for source in (self.source(_complete=False), self.source()):
            if "_complete" not in source:
                source["lines"][0]["text"] = "other\n"
            result = self.run_responses([self.page(), source])
            self.assertEqual(result["outcome"]["source_evidenced"], 0)
            self.assertTrue(result["outcome"]["evidence_cost_censored"])

    def test_page_and_call_caps_preserve_censored_spent_cost(self):
        result = self.run_responses([self.page([], next=f"set@{i}") for i in range(3)])
        self.assertEqual(result["outcome"]["stop_reason"], "page_limit")
        self.task["expected"][0].update(start=0, end=18)
        result = self.run_responses([self.page([self.hit(end=18)])] +
                                    [self.source(next=f"read:immutable@{i}") for i in range(7)])
        self.assertEqual(result["outcome"]["stop_reason"], "call_limit")
        self.assertEqual(result["outcome"]["usage"]["output_tokens"], 80)

    def test_schema_requires_named_members_and_confined_paths(self):
        manifest = dict(schema_version=1, tasks=[self.task])
        validate_manifest(manifest, self.roots)
        for label in [dict(member="missing", path="same.rs"),
                      dict(member="pack", path="../engine/same.rs"),
                      dict(member="pack", path="same.rs", start=1)]:
            bad = copy.deepcopy(manifest)
            bad["tasks"][0]["expected"] = [label]
            with self.assertRaises(ValueError):
                validate_manifest(bad, self.roots)

    def test_expected_file_fingerprint_detects_mutation(self):
        snapshot = verify_expected(self.task, self.roots)
        self.task["expected"][0]["sha256"] = snapshot[0]["sha256"]
        (self.roots["pack"] / "same.rs").write_bytes(b"changed\n")
        with self.assertRaises(ValueError):
            verify_expected(self.task, self.roots)

    def test_workspace_relative_paths_and_runner_flags(self):
        workspace = self.base / "workspace.toml"
        workspace.write_text("[workspace]\nname='test'\n[members.pack]\npath='pack'\n")
        self.assertEqual(member_roots(workspace), {"pack": self.roots["pack"]})
        runner = WorkspaceRunner("binary", workspace, self.roots["pack"],
                                 self.base / "cache", True)
        self.assertIn("--workspace", runner.prefix)
        self.assertIn("--no-daemon", runner.prefix)
        self.assertEqual(runner.prefix[runner.prefix.index("--budget") + 1], "600")

    def test_real_manifest_and_workflow_have_bounded_authored_queries(self):
        base = Path(__file__).parent
        manifest = json.loads((base / "manifests/workspace-space.json").read_text())
        roots = {name: self.base / name for name in ("lunatic", "tales-from-space", "tgstation")}
        validate_manifest(manifest, roots)
        self.assertTrue(all(label.get("sha256") for task in manifest["tasks"] for label in task["expected"]))
        workflow = json.loads((base / "workflows/workspace-navigation-v1.json").read_text())
        self.assertEqual(workflow["label"], "oracle-assisted navigation replay")
        self.assertEqual(workflow["response_budget_tokens"], 600)
        self.assertEqual(workflow["maximum_result_pages"], 3)
        self.assertEqual(workflow["maximum_tool_calls"], 8)

    def test_mutation_after_task_keeps_observed_records(self):
        workspace = self.base / "workspace.toml"
        workspace.write_text("[workspace]\nname='test'\n[members.pack]\npath='pack'\n")
        self.task["expected"][0]["sha256"] = verify_expected(self.task, self.roots)[0]["sha256"]
        def run(operation, argument):
            (self.roots["pack"] / "same.rs").write_bytes(b"changed\n")
            return self.page([]), "ok", True, UsageRecord(1, 20, 0, 0.01)
        with patch("workspace_replay.WorkspaceRunner", return_value=run):
            result = replay(dict(schema_version=1, tasks=[self.task]), workspace,
                            self.roots["pack"], Path("binary"))
        record = result["records"][0]
        self.assertEqual(record["outcome"]["status"], "invalid_snapshot")
        self.assertEqual(len(record["events"]), 1)
        self.assertTrue(record["outcome"]["evidence_cost_censored"])


if __name__ == "__main__":
    unittest.main()
