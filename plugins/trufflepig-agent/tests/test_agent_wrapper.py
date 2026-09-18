"""Exercise the wrapper boundary using an executable stand-in for the CLI."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

PLUGIN = Path(__file__).resolve().parents[1]


class AgentWrapperTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="trufflepig-wrapper-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("TRUFFLEPIG", "CODEX", "CLAUDE", "GROK", "KIMI", "MUSE", "AGENT_SESSION"))}
        self.env.update(HOME=str(self.root), XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_STATE_HOME=str(self.root / "state"), TMPDIR=str(self.root),
                        CODEX_THREAD_ID="thread-one")
        self.fake = self.root / "fake cli"
        self.fake.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
Path(os.environ["CAPTURE"]).write_text(json.dumps({"argv": sys.argv[1:], "spool": os.environ.get("TRUFFLEPIG_SPOOL_DIR")}))
sys.stdout.buffer.write(os.environ.get("RESPONSE", '{"hits":[],"truncated":false}\\n').encode())
sys.stderr.buffer.write(os.environ.get("STDERR", "").encode())
sys.exit(int(os.environ.get("EXIT", "0")))
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

    def test_claude_hook_session_beats_inherited_codex_markers(self):
        self.env.update(CLAUDECODE="1", TRUFFLEPIG_CLAUDE_SESSION="claude-session",
                        CLAUDE_SESSION_ID="not-a-documented-shell-session")
        self.run_wrapper("search", "x")
        self.assertEqual(self.record("claude")["session"], "claude-session")
        self.env["TRUFFLEPIG_SESSION"] = "explicit"
        self.run_wrapper("search", "y")
        self.assertEqual(self.record("claude")["session"], "explicit")
        self.run_wrapper("--session", "cli-session", "search", "z")
        self.assertEqual(self.record("claude")["session"], "cli-session")

    def test_claude_without_hook_has_visible_fallback_identity(self):
        self.env["CLAUDE_CODE_CHILD_SESSION"] = "1"
        self.run_wrapper("search", "x")
        self.assertTrue(self.record("claude")["session"].startswith("claude-"))

    def test_grok_session_and_explicit_override(self):
        self.env["GROK_SESSION_ID"] = "grok-session"
        self.assertEqual(self.run_wrapper("search", "x").returncode, 0)
        self.assertEqual(self.record("grok")["session"], "grok-session")
        self.run_wrapper("--session", "explicit-session", "search", "y")
        self.assertEqual(self.record("grok")["session"], "explicit-session")

    def test_flags_after_command_and_cli_attribution_win(self):
        self.env.update(TRUFFLEPIG_AGENT_HARNESS="muse", TRUFFLEPIG_SESSION="env-session")
        args = ["search", "quoted 'query' $(literal)", "--json", "--session=cli-session", "--client", "custom"]
        result = self.run_wrapper(*args)
        self.assertEqual(result.returncode, 0)
        argv = self.capture()["argv"]
        self.assertNotIn("--format", argv)
        self.assertEqual(argv[-len(args):], args)
        self.assertEqual(self.record("custom")["session"], "cli-session")
        self.assertEqual(self.record("custom")["args"], [args[1]])

    def test_environment_overrides_detection(self):
        self.env.update(TRUFFLEPIG_AGENT_HARNESS="kimi", TRUFFLEPIG_SESSION="explicit")
        self.run_wrapper("search", "x")
        self.assertEqual(self.record("kimi")["session"], "explicit")

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
