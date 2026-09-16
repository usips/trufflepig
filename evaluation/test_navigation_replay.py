"""Navigation accounting under pagination, complementary evidence, and mutation."""

from pathlib import Path
import json
import subprocess
from unittest.mock import patch
import tempfile
import unittest

from navigation_replay import CommandRunner, navigate, verified_lines
from replay import verify_snapshot
from records import UsageRecord


class NavigationReplayTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(dir="/home/josh/.cache/codex-tmp")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "a.rs").write_bytes(b"first\nsecond\nthird\n")
        self.task = dict(id="navigation", query="needle", relevant=[
            dict(path="a.rs", start=0, end=5)])
        self.calls = []

    def hit(self, start=0, end=5, handle="set:1"):
        data = (self.root / "a.rs").read_bytes()
        return dict(file=(self.root / "a.rs").as_uri(), start_line=data[:start].count(b"\n") + 1,
                    end_line=data[:end-1].count(b"\n") + 1, handle=handle)

    def source(self, start=0, end=6, **kwargs):
        return dict(path="a.rs", revision="revision-a", verified=True,
                    lines=[dict(start=start, end=end, encoding="utf8",
                                text=(self.root / "a.rs").read_bytes()[start:end].decode())], **kwargs)

    def run_responses(self, responses):
        def run(operation, argument):
            self.calls.append((operation, argument))
            response = responses.pop(0)
            status = response.pop("_status", "ok")
            complete = response.pop("_complete", True)
            return response, status, complete, UsageRecord(
                1, 20, 0, 0.01, 10, "o200k_base", not complete)
        return navigate(self.task, self.root, run)

    def test_metadata_discovery_is_not_source_evidence(self):
        result = self.run_responses([{"hits": [self.hit()]}, {"_status": "stale_source"}])
        self.assertEqual(result["outcome"]["metadata_discovered"], 1)
        self.assertEqual(result["outcome"]["source_evidenced"], 0)
        self.assertTrue(result["outcome"]["evidence_cost_censored"])
        self.assertEqual(result["outcome"]["usage"]["output_tokens"], 20)

    def test_three_pages_retain_original_ranks_and_follow_handles(self):
        result = self.run_responses([
            {"hits": [self.hit(12, 17)], "next": "set@1"},
            {"hits": [self.hit(6, 12)], "next": "set@2"},
            {"hits": [self.hit(handle="set:3")]}, self.source()])
        self.assertEqual(self.calls, [("search", "needle"), ("more", "set@1"),
                                     ("more", "set@2"), ("show", "set:3")])
        self.assertEqual(result["events"][2]["original_ranks"], [3])
        self.assertEqual(result["outcome"]["status"], "complete")
        self.assertEqual(result["outcome"]["tokens_to_complete_evidence"], 40)

    def test_fourth_page_is_censored(self):
        result = self.run_responses([{"hits": [], "next": f"set@{i}"} for i in range(3)])
        self.assertEqual(len(self.calls), 3)
        self.assertEqual(result["outcome"]["stop_reason"], "page_limit")
        self.assertTrue(result["outcome"]["evidence_cost_censored"])

    def test_complementary_evidence_needs_both_reads(self):
        self.task["relevant"].append(dict(path="a.rs", start=12, end=17))
        result = self.run_responses([
            {"hits": [self.hit(), self.hit(12, 17, "set:2")]},
            self.source(), self.source(12, 18)])
        self.assertEqual(result["outcome"]["source_evidenced"], 2)
        self.assertEqual(result["outcome"]["tokens_to_first_evidence"], 20)
        self.assertEqual(result["outcome"]["tokens_to_complete_evidence"], 30)

    def test_source_continuation_uses_returned_identity(self):
        self.task["relevant"] = [dict(path="a.rs", start=0, end=12)]
        result = self.run_responses([{"hits": [self.hit(0, 18)]},
                                   self.source(next="read-immutable-remaining"),
                                   self.source(6, 12)])
        self.assertEqual(self.calls[-1], ("show", "read-immutable-remaining"))
        self.assertEqual(result["outcome"]["status"], "complete")

    def test_required_ctx_cannot_be_replaced_by_source(self):
        self.task["requires_ctx"] = True
        result = self.run_responses([{"hits": [self.hit()]}, self.source(),
                                   {"relationships": [{"kind": "calls"}], "truncated": False}])
        self.assertEqual(self.calls[-1], ("ctx", "set:1"))
        self.assertTrue(result["outcome"]["context_satisfied"])
        self.assertEqual(result["outcome"]["status"], "complete")

    def test_empty_or_truncated_context_remains_incomplete(self):
        self.task["requires_ctx"] = True
        result = self.run_responses([{"hits": [self.hit()]}, self.source(),
                                   {"relationships": []}])
        self.assertEqual(result["outcome"]["status"], "incomplete")
        self.assertFalse(result["outcome"]["context_satisfied"])

    def test_changed_revision_cannot_credit_continuation(self):
        self.task["relevant"] = [dict(path="a.rs", start=0, end=12)]
        source = self.source(6, 12)
        source["revision"] = "revision-b"
        result = self.run_responses([{"hits": [self.hit(0, 18)]},
                                     self.source(next="continuation"), source])
        self.assertEqual(result["outcome"]["source_evidenced"], 0)

    def test_wrong_source_path_cannot_credit_compact_hit(self):
        source = self.source()
        source["path"] = "other.rs"
        result = self.run_responses([{"hits": [self.hit()]}, source])
        self.assertEqual(result["outcome"]["source_evidenced"], 0)

    def test_changed_bytes_cannot_credit_same_coordinates(self):
        source = self.source()
        source["lines"][0]["text"] = "other\n"
        self.assertEqual(verified_lines(self.root, source, dict(path="a.rs", revision="revision-a")), [])

    def test_partial_delivery_never_credits_source(self):
        source = self.source(_complete=False)
        result = self.run_responses([{"hits": [self.hit()]}, source])
        self.assertEqual(result["outcome"]["source_evidenced"], 0)
        self.assertTrue(result["outcome"]["usage"]["token_cost_censored"])

    def test_eight_call_limit_preserves_spent_cost(self):
        self.task["relevant"] = [dict(path="a.rs", start=0, end=18)]
        result = self.run_responses([{"hits": [self.hit(0, 18)]}] +
                                   [self.source(next=f"immutable-{i}") for i in range(7)])
        self.assertEqual(len(self.calls), 8)
        self.assertEqual(result["outcome"]["stop_reason"], "call_limit")
        self.assertEqual(result["outcome"]["usage"]["output_tokens"], 80)
        self.assertIsNone(result["outcome"]["tokens_to_complete_evidence"])

    def test_overlapping_fragments_do_not_fill_a_gap(self):
        self.task["relevant"] = [dict(path="a.rs", start=0, end=18)]
        result = self.run_responses([{"hits": [self.hit(0, 18)]},
                                   self.source(0, 6, next="tail"), self.source(12, 18)])
        self.assertEqual(result["outcome"]["source_evidenced"], 0)


class RunnerDeliveryTests(unittest.TestCase):
    def test_exact_text_counter_includes_error_response_newline(self):
        seen = []
        def counter(text):
            seen.append(text)
            return 5
        runner = CommandRunner("binary", Path("."), Path("cache"), counter)
        completed = subprocess.CompletedProcess([], 1, b'not-json\n', b'failure\n')
        with patch("navigation_replay.subprocess.run", return_value=completed):
            _, status, complete, usage = runner("search", "query")
        self.assertEqual(seen, ["not-json\n"])
        self.assertEqual(status, "invalid_response")
        self.assertTrue(complete)
        self.assertEqual(usage.stdout_bytes, 9)
        self.assertEqual(usage.stderr_bytes, 8)
        self.assertEqual(usage.output_tokens, 5)

    def test_timeout_retains_partial_cost_but_incomplete_delivery(self):
        runner = CommandRunner("binary", Path("."), Path("cache"), lambda _: 2)
        error = subprocess.TimeoutExpired([], 30, output=b"{}", stderr=b"partial")
        with patch("navigation_replay.subprocess.run", side_effect=error):
            _, status, complete, usage = runner("show", "handle")
        self.assertEqual(status, "timeout")
        self.assertFalse(complete)
        self.assertEqual(usage.stdout_bytes, 2)
        self.assertTrue(usage.token_cost_censored)

    def test_authored_navigation_manifest_remains_frozen(self):
        manifest_path = Path(__file__).parent / "manifests/navigation.json"
        manifest = json.loads(manifest_path.read_text())
        verify_snapshot((manifest_path.parent / manifest["root"]).resolve(), manifest)
        workflow = json.loads((Path(__file__).parent / "workflows/navigation-v1.json").read_text())
        self.assertEqual(workflow["response_budget_tokens"], 600)
        self.assertEqual(workflow["maximum_result_pages"], 3)
        self.assertEqual(workflow["maximum_tool_calls"], 8)


class NeutralUsageTests(unittest.TestCase):
    def test_unknown_tokens_remain_unknown(self):
        usage = UsageRecord(1, 100, 0, 0.1)
        self.assertIsNone(usage.output_tokens)
        self.assertTrue(usage.token_cost_censored)

    def test_token_count_requires_tokenizer(self):
        with self.assertRaises(ValueError):
            UsageRecord(1, 100, 0, 0.1, output_tokens=10)


if __name__ == "__main__":
    unittest.main()
