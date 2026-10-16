"""Exercise the wrapper boundary using an executable stand-in for the CLI."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from importlib.machinery import SourceFileLoader
from unittest.mock import patch

PLUGIN = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PLUGIN / "bin"))
from trufflepig_audit_response import parse_lines_response  # noqa: E402
from trufflepig_board_notices import pending_after  # noqa: E402


class HarnessResolutionTests(unittest.TestCase):
    def setUp(self):
        loader = SourceFileLoader("trufflepig_agent_wrapper_test", str(PLUGIN / "bin/trufflepig-agent"))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        self.wrapper = importlib.util.module_from_spec(spec)
        loader.exec_module(self.wrapper)

    def test_native_environment_priority_matches_rust_and_beats_claims(self):
        with patch.dict(os.environ, {
            "GROK_SESSION_ID": "grok-session", "OMPCODE": "1", "CLAUDECODE": "1",
            "CODEX_THREAD_ID": "codex-thread", "TRUFFLEPIG_AGENT_HARNESS": "kimi",
        }, clear=True):
            self.assertEqual(self.wrapper.detect_native_harness(), "grok")
            self.assertEqual(self.wrapper.detect_harness("human"), "grok")
        with patch.dict(os.environ, {"OMPCODE": "1", "CLAUDECODE": "1"}, clear=True):
            self.assertEqual(self.wrapper.detect_native_harness(), "omp")

    def test_nearest_process_fallback_precedes_wrapper_hint(self):
        processes = {
            "/proc/101/comm": "python\n", "/proc/101/stat": "101 (python) S 202 0\n",
            "/proc/202/comm": "codex\n", "/proc/202/stat": "202 (codex) S 303 0\n",
            "/proc/303/comm": "claude\n", "/proc/303/stat": "303 (claude) S 1 0\n",
        }

        def read_text(path, *args, **kwargs):
            return processes[str(path)]

        with (patch.dict(os.environ, {"TRUFFLEPIG_AGENT_HARNESS": "kimi"}, clear=True),
              patch.object(self.wrapper.os, "getppid", return_value=101),
              patch.object(self.wrapper.Path, "read_text", autospec=True, side_effect=read_text)):
            self.assertEqual(self.wrapper.detect_native_harness(), "codex")
            self.assertEqual(self.wrapper.detect_harness("claude"), "codex")


class NextFooterTests(unittest.TestCase):
    def test_bare_cursor_from_every_footer_shape(self):
        handle = "0" * 32
        shapes = {
            "next: 4f2a@20": "4f2a@20",
            "next: more 4f2a@20\nhint: -n 100 raises the page size": "4f2a@20",
            "next: more 4f2a@20 (-n N raises the page size)": "4f2a@20",
            "next: more 4f2a@20 (-b N raises the token budget)": "4f2a@20",
            "next: more 4f2a@20": "4f2a@20",
            "next: show read:9c1e@4096": "read:9c1e@4096",
            "next: read:9c1e@4096": "read:9c1e@4096",
        }
        for footer, cursor in shapes.items():
            with self.subTest(footer=footer):
                page = f"{handle}:1\tsrc/a.rs:3-9\trun\n  4: fn run() {{\ncoverage: indexed 3/3\n{footer}\n"
                self.assertEqual(parse_lines_response(page.encode())["next"], cursor)


class CoverageLineTests(unittest.TestCase):
    def coverage(self, line):
        response = parse_lines_response(f"coverage: {line}\n".encode())
        members = {row["member"]: (row["state"], row["partial"], row["truncated"])
                   for row in response.get("coverage", [])} if isinstance(response.get("coverage"), list) else {}
        return members, response.get("annotations", []), response.get("scope")

    def test_member_states(self):
        shapes = {
            "lunatic complete": ("lunatic", ("searched", False, False)),
            "lunatic partial (20 unsearched: fact_limit) truncated": ("lunatic", ("searched", True, True)),
            "lunatic@lunatic-w3-L1 warming \u2192 served from lunatic index (3 files differ)":
                ("lunatic@lunatic-w3-L1", ("parent_fallback", False, False)),
            "lunatic@L1 warming \u2192 served from lunatic index (1 file differs)":
                ("lunatic@L1", ("parent_fallback", False, False)),
            "lunatic@L1 warming \u2192 served from lunatic index (no files differ)":
                ("lunatic@L1", ("parent_fallback", False, False)),
            "lunatic@L1 warming \u2192 served from lunatic index (differences unknown) truncated":
                ("lunatic@L1", ("parent_fallback", False, True)),
            "lunatic@L1 warming (no parent index)": ("lunatic@L1", ("warming", False, False)),
            "lunatic@L1 unavailable (database is locked)": ("lunatic@L1", ("unavailable", False, False)),
            "tgstation timed out": ("tgstation", ("timed_out", False, False)),
        }
        for line, (name, fields) in shapes.items():
            with self.subTest(line=line):
                self.assertEqual(self.coverage(line)[0], {name: fields})

    def test_diagnosis_parts_are_annotations_not_members(self):
        annotations = [
            "file:web/ matched 0 indexed paths (nearest: crates/web/src/lib.rs)",
            "-file:tests/ matched 0 indexed paths",
            "lang:markdown matched 0 files",
            "filters file:web/ lang:rust matched 0 files",
            "refs 14 sites in 5 files",
        ]
        for annotation in annotations:
            with self.subTest(annotation=annotation):
                members, found, scope = self.coverage(f"lunatic complete; {annotation}; scope home")
                self.assertEqual(members, {"lunatic": ("searched", False, False)})
                self.assertEqual((found, scope), ([annotation], "home"))
        members, found, scope = self.coverage(
            "lunatic@L1 warming \u2192 served from lunatic index (no files differ); refs 3 sites in 1 files; "
            "semantic partial; scope home (warming; ws:all searches 3 members)")
        self.assertEqual(list(members), ["lunatic@L1"])
        self.assertEqual(found, ["refs 3 sites in 1 files"])
        self.assertEqual(scope, "home (warming; ws:all searches 3 members)")


class AgentWrapperTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="trufflepig-wrapper-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("TRUFFLEPIG", "CODEX", "CLAUDE", "GROK", "KIMI", "MUSE", "OPENCODE", "AGENT_SESSION",
                                           "OMP", "PI_"))}
        self.env.update(HOME=str(self.root), XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_STATE_HOME=str(self.root / "state"), TMPDIR=str(self.root),
                        CODEX_THREAD_ID="thread-one")
        self.fake = self.root / "fake cli"
        self.fake.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
argv = sys.argv[1:]
pending = ("--json" in argv and "--limit" in argv and
           any(argv[index:index + 2] == ["board", "pending"] for index in range(len(argv) - 1)))
if pending:
    with Path(os.environ["CAPTURE"] + ".pending").open("a") as handle:
        handle.write(json.dumps(argv) + "\\n")
    stdout = os.environ.get("PENDING_RESPONSE", '{"api":10,"result":{"result":"pending","data":{"notices":[],"after":null,"through":0,"server_now":0,"generation":"0000000000000000","omitted":0}}}\\n')
    stderr = os.environ.get("PENDING_STDERR", "")
    exit_code = int(os.environ.get("PENDING_EXIT", "0"))
else:
    Path(os.environ["CAPTURE"]).write_text(json.dumps({"argv": argv, "spool": os.environ.get("TRUFFLEPIG_SPOOL_DIR")}))
    stdout = os.environ.get("RESPONSE", '{"hits":[],"truncated":false}\\n')
    stderr = os.environ.get("STDERR", "")
    exit_code = int(os.environ.get("EXIT", "0"))
sys.stdout.buffer.write(stdout.encode())
sys.stderr.buffer.write(stderr.encode())
sys.exit(exit_code)
''')
        self.fake.chmod(0o755)
        self.env.update(TRUFFLEPIG_BINARY=str(self.fake), CAPTURE=str(self.root / "capture.json"))

    def run_wrapper(self, *args, cwd=None):
        return subprocess.run([str(PLUGIN / "bin/trufflepig-agent"), *args],
                              env=self.env, cwd=cwd or self.root, capture_output=True)

    def record(self, harness="codex", base=None):
        base = base or self.root / "state/trufflepig/agent-audit"
        return json.loads((base / f"{harness}.jsonl").read_text().splitlines()[-1])

    def capture(self):
        return json.loads((self.root / "capture.json").read_text())

    def pending_calls(self):
        path = self.root / "capture.json.pending"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def test_thread_groups_directories_and_separates_threads(self):
        other = self.root / "other directory"
        other.mkdir()
        self.run_wrapper("search", "token refill")
        self.run_wrapper("show", "handle", cwd=other)
        self.env["CODEX_THREAD_ID"] = "thread-two"
        self.run_wrapper("search", "token refill")
        result = subprocess.run([str(PLUGIN / "bin/trufflepig-audit"), "--harness", "codex", "--json"],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        sessions = {s["session"]: s["calls"] for s in json.loads(result.stdout)["sessions"]}
        self.assertEqual(sessions, {"thread-one": 2, "thread-two": 1})

    def test_claude_agent_override_beats_inherited_parent_session(self):
        self.env.update(CLAUDECODE="1", TRUFFLEPIG_CLAUDE_SESSION="parent-session",
                        CLAUDE_CODE_SESSION_ID="parent-session",
                        CLAUDE_SESSION_ID="not-a-documented-shell-session")
        self.run_wrapper("search", "x")
        self.assertEqual(self.record("claude")["session"], "parent-session")
        self.assertNotIn("--session", self.capture()["argv"])
        self.env["TRUFFLEPIG_SESSION"] = "claude-agent-stable"
        self.run_wrapper("search", "y")
        self.assertEqual(self.record("claude")["session"], "claude-agent-stable")
        self.run_wrapper("search", "y-again")
        self.assertEqual(self.record("claude")["session"], "claude-agent-stable")
        self.run_wrapper("--session", "cli-session", "search", "z")
        self.assertEqual(self.record("claude")["session"], "cli-session")

    def test_claude_without_session_source_remains_unattributed(self):
        self.env["CLAUDE_CODE_CHILD_SESSION"] = "1"
        self.run_wrapper("search", "x")
        self.assertIsNone(self.record("claude")["session"])
        self.assertNotIn("--session", self.capture()["argv"])
        self.assertEqual(self.pending_calls(), [])

    def test_grok_session_and_explicit_override(self):
        self.env["GROK_SESSION_ID"] = "grok-session"
        self.assertEqual(self.run_wrapper("search", "x").returncode, 0)
        self.assertEqual(self.record("grok")["session"], "grok-session")
        self.assertNotIn("--session", self.capture()["argv"])
        self.run_wrapper("--session", "explicit-session", "search", "y")
        self.assertEqual(self.record("grok")["session"], "explicit-session")
        argv = self.capture()["argv"]
        self.assertEqual(argv[argv.index("--session") + 1], "explicit-session")

    def test_flags_after_command_and_cli_attribution_win(self):
        self.env.update(TRUFFLEPIG_AGENT_HARNESS="muse", TRUFFLEPIG_SESSION="env-session")
        args = ["search", "quoted 'query' $(literal)", "--json", "--session=cli-session", "--client", "custom"]
        result = self.run_wrapper(*args)
        self.assertEqual(result.returncode, 0)
        argv = self.capture()["argv"]
        self.assertNotIn("--format", argv)
        self.assertEqual(argv[-len(args):], args)
        self.assertEqual(self.record("codex")["session"], "cli-session")
        self.assertEqual(self.record("codex")["claimed_client"], "custom")
        self.assertEqual(self.record("codex")["args"], [args[1]])

    def test_explicit_client_claim_is_preserved_inside_observed_harness(self):
        result = self.run_wrapper("--client", "human", "search", "x")
        self.assertEqual(result.returncode, 0, result.stderr)
        argv = self.capture()["argv"]
        client_index = argv.index("--client")
        self.assertEqual(argv[client_index + 1], "human")
        self.assertEqual(self.record("codex")["harness"], "codex")
        self.assertEqual(self.record("codex")["claimed_client"], "human")

    def test_environment_overrides_detection(self):
        self.env.update(TRUFFLEPIG_AGENT_HARNESS="kimi", TRUFFLEPIG_SESSION="explicit")
        self.run_wrapper("search", "x")
        self.assertEqual(self.record("codex")["session"], "explicit")
        self.assertNotIn("--session", self.capture()["argv"])
        self.assertNotIn("--session", self.pending_calls()[0])

    def test_wrapper_harness_hint_is_used_without_native_markers(self):
        del self.env["CODEX_THREAD_ID"]
        self.env.update(TRUFFLEPIG_AGENT_HARNESS="kimi", TRUFFLEPIG_SESSION="explicit")
        self.run_wrapper("search", "x")
        self.assertEqual(self.record("kimi")["session"], "explicit")

    def test_passive_notice_adds_bounded_json_and_uses_a_separate_watermark(self):
        now = int(time.time())
        pending = {
            "notices": [{"seq": 24, "kind": "entry", "reason": "question",
                         "entry": {"id": "E24", "plan": "P7", "kind": "question",
                                   "body": "PRIVATE_BOARD_BODY", "refs": ["P7.3"],
                                   "model": "gpt-6-luna", "created_at": now - 120}}],
            "after": None, "through": 25, "server_now": now,
            "generation": "dddddddddddddddd", "omitted": 0,
        }
        self.env.update(RESPONSE='{"status":"ok","hits":[],"truncated":false}\n',
                        PENDING_RESPONSE=json.dumps({"api": 10, "result": {
                            "result": "pending", "data": pending}}) + "\n")
        result = self.run_wrapper("--json", "search", "needle")
        self.assertEqual(result.returncode, 0, result.stderr)
        output = json.loads(result.stdout)
        self.assertEqual(output["hits"], [])
        metadata = output["board_pending"]
        self.assertEqual(metadata["notices"], [{"seq": 24, "kind": "question",
                                                "entry_id": "E24", "task": "P7.3"}])
        self.assertNotIn("PRIVATE_BOARD_BODY", result.stdout.decode())
        self.assertLessEqual(len(json.dumps(metadata).encode()), 384)
        self.assertEqual(pending_after("codex/thread-one", self.root / "state/trufflepig/board-notices"), 25)
        calls = self.pending_calls()
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][-2:], ["board", "pending"])
        self.assertEqual(calls[0][calls[0].index("--limit") + 1], "8")
        self.assertNotIn("--session", calls[0])
        self.assertNotIn("--session", self.capture()["argv"])

        second = self.run_wrapper("--json", "search", "again")
        self.assertEqual(json.loads(second.stdout), {"status": "ok", "hits": [], "truncated": False})
        self.assertEqual(len(self.pending_calls()), 1)

    def test_lines_notice_uses_one_fixed_safe_footer(self):
        pending = {"notices": [{"seq": 9, "kind": "entry", "reason": "claim",
                                "entry": {"id": "E9", "kind": "claim", "body": "private"}}],
                   "after": None, "through": 9, "server_now": 1000,
                   "generation": "eeeeeeeeeeeeeeee", "omitted": 0}
        self.env.update(RESPONSE="Search finished", PENDING_RESPONSE=json.dumps({"api": 10,
                        "result": {"result": "pending", "data": pending}}))
        result = self.run_wrapper("search", "needle")
        self.assertEqual(result.stdout,
                         b"Search finished\nboard: 1 pending notice; run trufflepig-agent board pending\n")
        self.assertNotIn(b"private", result.stdout)

    def test_empty_success_can_carry_notice_footer(self):
        pending = {"notices": [{"seq": 9, "kind": "event", "reason": "worker",
                                "event": {"seq": 9, "kind": "worker", "summary": "private"}}],
                   "after": None, "through": 9, "server_now": 1000,
                   "generation": "ffffffffffffffff", "omitted": 0}
        self.env.update(RESPONSE="", PENDING_RESPONSE=json.dumps({"api": 10,
                        "result": {"result": "pending", "data": pending}}))
        result = self.run_wrapper("search", "quiet")
        self.assertEqual(result.stdout,
                         b"board: 1 pending notice; run trufflepig-agent board pending\n")

    def test_pending_failure_or_primary_help_preserves_output(self):
        self.env.update(RESPONSE='{"status":"ok","hits":[]}\n', PENDING_EXIT="3",
                        PENDING_STDERR="private router failure\n")
        result = self.run_wrapper("--json", "search", "needle")
        self.assertEqual((result.returncode, result.stdout, result.stderr),
                         (0, b'{"status":"ok","hits":[]}\n', b""))
        self.assertEqual(len(self.pending_calls()), 1)

        self.env.pop("PENDING_EXIT")
        self.env.pop("PENDING_STDERR")
        (self.root / "capture.json.pending").unlink()
        help_result = self.run_wrapper("--help")
        self.assertEqual(help_result.stdout, b'{"status":"ok","hits":[]}\n')
        self.assertEqual(self.pending_calls(), [])

    def test_failed_primary_never_attempts_pending(self):
        self.env.update(RESPONSE='{"error":"bad flag"}\n', STDERR="diagnostic\n", EXIT="2")
        result = self.run_wrapper("search", "x")
        self.assertEqual((result.returncode, result.stdout, result.stderr),
                         (2, b'{"error":"bad flag"}\n', b"diagnostic\n"))
        self.assertEqual(self.pending_calls(), [])

    def test_invalid_json_success_is_preserved_without_text_footer(self):
        self.env["RESPONSE"] = '{"partial":\n'
        result = self.run_wrapper("--json", "search", "x")
        self.assertEqual((result.returncode, result.stdout, result.stderr),
                         (0, b'{"partial":\n', b""))
        self.assertEqual(self.pending_calls(), [])

    def test_existing_json_notice_metadata_is_preserved_without_repolling(self):
        response = '{"status":"ok","board_pending":{"existing":true}}\n'
        self.env["RESPONSE"] = response
        result = self.run_wrapper("--json", "search", "x")
        self.assertEqual(result.stdout, response.encode())
        self.assertEqual(self.pending_calls(), [])

    def test_session_hook_markers_do_not_create_actor_identity(self):
        # The hook also starts the router through systemctl or the CLI; shims keep
        # both inert so the test never starts the real user service.
        shims = self.root / "shim"
        shims.mkdir()
        for name, status in (("systemctl", 1), ("trufflepig", 0)):
            (shims / name).write_text(f"#!/bin/sh\nexit {status}\n")
            (shims / name).chmod(0o755)
        hook_env = dict(self.env, PATH=f"{shims}:{self.env['PATH']}")
        payload = json.dumps({"session_id": "kimi-session", "cwd": str(self.root)})
        hook = subprocess.run(["sh", str(PLUGIN / "hooks/session-start.sh"), "kimi", "start"],
                              input=payload, env=hook_env, capture_output=True, text=True)
        self.assertEqual((hook.returncode, hook.stdout), (0, ""), hook.stderr)
        del self.env["CODEX_THREAD_ID"]
        self.env["KIMI_CODE_HOME"] = str(self.root / "kimi")
        self.run_wrapper("search", "x")
        self.assertIsNone(self.record("kimi")["session"])
        other = self.root / "other directory"
        other.mkdir()
        self.run_wrapper("search", "x", cwd=other)
        self.assertIsNone(self.record("kimi")["session"])
        subprocess.run(["sh", str(PLUGIN / "hooks/session-start.sh"), "kimi", "end"],
                       input=payload, env=hook_env, capture_output=True, text=True)
        self.run_wrapper("search", "y")
        self.assertIsNone(self.record("kimi")["session"])
        self.env["KIMI_SESSION_ID"] = "native-kimi-session"
        self.run_wrapper("search", "z")
        self.assertEqual(self.record("kimi")["session"], "native-kimi-session")

    def test_codex_session_fallback_and_harness_isolation(self):
        del self.env["CODEX_THREAD_ID"]
        self.env.update(CODEX_SESSION_ID="codex-session", KIMI_SESSION_ID="unrelated")
        self.run_wrapper("search", "x")
        self.assertEqual(self.record()["session"], "codex-session")

    def test_preserves_cli_errors_and_output(self):
        self.env.update(RESPONSE='{"error":"bad flag"}\n', STDERR="diagnostic\n", EXIT="2")
        result = self.run_wrapper("search", "x")
        self.assertEqual((result.returncode, result.stdout, result.stderr),
                         (2, b'{"error":"bad flag"}\n', b'diagnostic\n'))
        self.assertIn("error", self.record()["signals"])

    def test_show_continuation_audit_does_not_count_source_rows_as_hits(self):
        self.env["RESPONSE"] = "a.rs revision 1-2\n1\tfn main() {}\nnext: read:cursor\ntruncated: true\nverified: true\n"
        self.run_wrapper("show", "handle")
        record = self.record()
        self.assertEqual(record["hits"], 0)
        self.assertTrue(record["has_next"])
        self.assertTrue(record["truncated"])

    def test_lines_page_snippets_and_scope_are_not_hits_or_members(self):
        handle = "0" * 32
        self.env["RESPONSE"] = (
            f"{handle}:1\tlunatic/src/a.rs:3-9\trun\n  4: fn run() {{\n"
            "coverage: lunatic partial (2 unsearched); semantic unavailable; scope home (ws:all adds 2 members)\n"
        )
        self.run_wrapper("search", "run")
        record = self.record()
        self.assertEqual(record["hits"], 1)
        self.assertEqual(set(record["coverage"]), {"lunatic"}, record["coverage"])
        self.assertIn("semantic_degraded", record["signals"])

    def test_runtime_spool_and_disk_fallback(self):
        runtime = self.root / "disk runtime"
        config = self.root / "config/trufflepig/agent-runtime.json"
        config.parent.mkdir(parents=True)
        config.write_text(json.dumps({"runtime_dir": str(runtime), "spool_dir": str(runtime / "spool")}))
        blocked = self.root / "not-a-directory"
        blocked.write_text("block")
        self.env["XDG_STATE_HOME"] = str(blocked)
        result = self.run_wrapper("search", "x")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.capture()["spool"], str(runtime / "spool"))
        self.assertEqual(self.record(base=runtime / "agent-audit")["session"], "thread-one")
        self.env["TRUFFLEPIG_SPOOL_DIR"] = str(self.root / "override")
        self.run_wrapper("search", "y")
        self.assertEqual(self.capture()["spool"], self.env["TRUFFLEPIG_SPOOL_DIR"])

    def test_audit_orders_primary_and_fallback_by_time(self):
        self.run_wrapper("search", "x")
        primary = self.root / "state/trufflepig/agent-audit/codex.jsonl"
        record = self.record()
        record.update(ts="2026-09-16T10:00:00+0000", hits=1, signals=[])
        primary.write_text(json.dumps(record) + "\n")
        fallback = self.root / f"trufflepig-{os.getuid()}/agent-audit/codex.jsonl"
        fallback.parent.mkdir(parents=True)
        record.update(ts="2026-09-16T10:00:01+0000", verb="show", hits=0)
        fallback.write_text(json.dumps(record) + "\n")
        result = subprocess.run([str(PLUGIN / "bin/trufflepig-audit"), "--json"],
                                env=self.env, capture_output=True, text=True)
        row = json.loads(result.stdout)["sessions"][0]
        self.assertLess(row["first"], row["last"])
        self.assertEqual(row["follow_through"], "1/1")

    def test_audit_does_not_count_same_directory_twice(self):
        runtime = self.root / f"trufflepig-{os.getuid()}"
        self.env["TRUFFLEPIG_AGENT_LOG_DIR"] = str(runtime / "agent-audit")
        self.run_wrapper("search", "x")
        result = subprocess.run([str(PLUGIN / "bin/trufflepig-audit"), "--json"],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(json.loads(result.stdout)["records"], 1)


if __name__ == "__main__":
    unittest.main()
