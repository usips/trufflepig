"""Board wrapper privacy, protocol metadata, and complete skill installation."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

PLUGIN = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PLUGIN / "bin"))
from trufflepig_board_audit import AUDIT_TAIL_BYTES, recent_calls, tail_records


class BoardWrapperTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="trufflepig-board-wrapper-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("TRUFFLEPIG", "CODEX", "CLAUDE", "GROK", "KIMI", "MUSE", "OMP", "PI_", "AGENT_SESSION"))}
        self.env.update(HOME=str(self.root), XDG_STATE_HOME=str(self.root / "state"),
                        XDG_CONFIG_HOME=str(self.root / "config"), TMPDIR=str(self.root),
                        CODEX_THREAD_ID="board-session", CAPTURE=str(self.root / "capture"))
        fake = self.root / "cli"
        fake.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
Path(os.environ["CAPTURE"]).write_text(json.dumps(sys.argv[1:]))
Path(os.environ["CAPTURE"] + ".steer").write_text(json.dumps(os.environ.get("TRUFFLEPIG_AGENT_STEER")))
sys.stdout.write(os.environ.get("RESPONSE", '{"status":"ok"}\\n'))
sys.stderr.write(os.environ.get("STDERR", ""))
sys.exit(int(os.environ.get("EXIT", "0")))
''')
        fake.chmod(0o755)
        self.env["TRUFFLEPIG_BINARY"] = str(fake)

    def run_wrapper(self, *args):
        return subprocess.run([str(PLUGIN / "bin/trufflepig-agent"), *args],
                              env=self.env, cwd=self.root, capture_output=True)

    def argv(self):
        return json.loads((self.root / "capture").read_text())

    def records(self):
        path = self.root / "state/trufflepig/agent-audit/codex.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()]

    def test_claim_environment_applies_only_to_board_and_feedback(self):
        self.env.update(TRUFFLEPIG_AGENT_MODEL="gpt-6-luna", TRUFFLEPIG_AGENT_EFFORT="xhigh")
        for verb, words in (("board", ["inbox"]), ("feedback", ["missing", "Feature"]), ("search", ["needle"])):
            with self.subTest(verb=verb):
                result = self.run_wrapper(verb, *words)
                self.assertEqual(result.returncode, 0, result.stderr)
                argv = self.argv()
                self.assertEqual("--agent-model" in argv, verb != "search")
                self.assertEqual("--agent-effort" in argv, verb != "search")
                if verb != "search":
                    self.assertEqual(argv[argv.index("--agent-model") + 1], "gpt-6-luna")
                    self.assertEqual(argv[argv.index("--agent-effort") + 1], "xhigh")
        self.run_wrapper("board", "inbox", "--agent-model=explicit", "--agent-effort", "low")
        self.assertNotIn("--agent-model", self.argv())
        self.assertEqual(self.argv().count("--agent-effort"), 1)
        self.assertEqual(self.argv()[self.argv().index("--agent-effort") + 1], "low")

    def test_board_flags_are_values_and_private_text_is_redacted_on_errors(self):
        secret = "PRIVATE_PLAN_TEXT"
        self.env.update(RESPONSE=json.dumps({"error": f"invalid_body: {secret}"}) + "\n",
                        STDERR=f"error: unexpected --board-text={secret}\n", EXIT="2")
        calls = [
            ["--body", "/private/body.md", "board", "new", secret, "--steward", "claude"],
            ["board", "post", "P7", "progress", secret, "--scope", secret, "--section=" + secret],
            ["board", "post", "P7", "progress", "--board-text=" + secret],
            ["board", "post", "P7", "progress", "--board-text", secret],
            ["board", "post", "P7", "progress", "--", "- " + secret],
            ["board", secret.lower(), secret],
            ["feedback", "blocked", secret, "--body=/private/body.md"],
            ["feedback", "close", "E12", "fixed", secret],
            ["search", "needle", "--board-text=" + secret, "--body", "/private/body.md", "--recent-calls", secret],
        ]
        for args in calls:
            with self.subTest(args=args):
                result = self.run_wrapper(*args)
                self.assertEqual(result.returncode, 2)
                # Delivery remains byte-for-byte intact even when auditing redacts.
                self.assertIn(secret.encode(), result.stdout)
                record = self.records()[-1]
                serialized = json.dumps(record)
                self.assertNotIn(secret, serialized)
                self.assertNotIn(secret.lower(), serialized)
                self.assertNotIn("/private/body.md", serialized)
                self.assertEqual((record["error"], record["stderr"]), ("invalid_body:", "error:"))
        self.assertEqual(self.records()[0]["verb"], "board")
        self.assertEqual(self.records()[1]["args"], ["post", "P7"])
        self.assertEqual(self.records()[7]["args"], ["close", "E12"])

    def test_feedback_attaches_five_same_session_calls_with_rust_schema(self):
        self.env.update(RESPONSE='{"error":"workspace_unavailable: PRIVATE_SOURCE","truncated":true,'
                        '"coverage":{"semantic_status":"unavailable"}}\n', EXIT="2")
        for index in range(7):
            self.run_wrapper("search", f"needle-{index}")
        self.run_wrapper("--session", "unrelated", "search", "unrelated query")
        self.run_wrapper("feedback", "blocked", "Search unavailable")
        argv = self.argv()
        payload = argv[argv.index("--recent-calls") + 1]
        calls = json.loads(payload)
        self.assertEqual([call["args"] for call in calls], [[f"needle-{index}"] for index in range(2, 7)])
        self.assertLessEqual(len(payload.encode("utf-8")), 2048)
        expected = {"verb", "args", "exit_code", "error_prefix", "truncated", "coverage"}
        for call in calls:
            self.assertEqual(set(call), expected)
            self.assertEqual(call["exit_code"], 2)
            self.assertEqual(call["error_prefix"], "workspace_unavailable:")
            self.assertIs(call["truncated"], True)
            self.assertIsInstance(call["coverage"], str)
            self.assertEqual(json.loads(call["coverage"])["root"]["semantic_status"], "unavailable")
        self.assertNotIn("PRIVATE_SOURCE", payload)

    def test_explicit_recent_calls_are_preserved_and_never_audited(self):
        self.run_wrapper("feedback", "wrong", "summary", "--recent-calls", "EXPLICIT_PRIVATE")
        self.assertEqual(self.argv().count("--recent-calls"), 1)
        self.assertEqual(self.argv()[self.argv().index("--recent-calls") + 1], "EXPLICIT_PRIVATE")
        self.assertNotIn("EXPLICIT_PRIVATE", json.dumps(self.records()[-1]))

    def test_feedback_reads_and_closes_do_not_receive_report_only_metadata(self):
        for args in (("ls",), ("close", "E12", "fixed")):
            with self.subTest(args=args):
                self.run_wrapper("feedback", *args)
                self.assertNotIn("--recent-calls", self.argv())

    def test_feedback_captures_effective_caller_steering(self):
        config = self.root / "config/trufflepig/agent-runtime.json"
        config.parent.mkdir(parents=True)
        config.write_text(json.dumps({"steer": {"claude": "strict", "default": "off"}}))
        self.run_wrapper("feedback", "blocked", "unavailable")
        self.assertEqual(json.loads((self.root / "capture.steer").read_text()), "off")
        self.env["TRUFFLEPIG_AGENT_STEER"] = "NUDGE"
        self.run_wrapper("--client", "claude", "feedback", "blocked", "unavailable")
        self.assertEqual(json.loads((self.root / "capture.steer").read_text()), "nudge")
        del self.env["TRUFFLEPIG_AGENT_STEER"]
        self.run_wrapper("--client", "claude", "feedback", "missing", "feature")
        self.assertEqual(json.loads((self.root / "capture.steer").read_text()), "strict")
        config.write_text(json.dumps({"steer": {"default": "off"}}))
        self.run_wrapper("--client", "claude", "feedback", "missing", "feature")
        self.assertEqual(json.loads((self.root / "capture.steer").read_text()), "off")
        config.write_text("{}")
        self.run_wrapper("--client", "claude", "feedback", "missing", "feature")
        self.assertEqual(json.loads((self.root / "capture.steer").read_text()), "nudge")

    def test_feedback_reports_off_for_harnesses_without_steering_hooks(self):
        config = self.root / "config/trufflepig/agent-runtime.json"
        config.parent.mkdir(parents=True)
        config.write_text(json.dumps({"steer": {"codex": "strict", "default": "block"}}))
        for explicit in (None, "BLOCK"):
            if explicit:
                self.env["TRUFFLEPIG_AGENT_STEER"] = explicit
            for harness in ("codex", "grok", "omp", "unknown"):
                with self.subTest(harness=harness, explicit=explicit):
                    result = self.run_wrapper("--client", harness, "feedback", "blocked", "unavailable")
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(json.loads((self.root / "capture.steer").read_text()), "off")

    def test_legacy_board_audit_is_sanitized_before_feedback_attachment(self):
        root = self.root / "audit"
        root.mkdir()
        record = dict(harness="codex", session="board-session", ts="2026-10-03T12:00:00+0000",
                      verb="board", args=["post", "P7", "question", "PRIVATE_PLAN"],
                      error="invalid_body: PRIVATE_PLAN", stderr="PRIVATE_PLAN", exit_code=2)
        (root / "codex.jsonl").write_text(json.dumps(record) + "\n")
        with (root / "codex.jsonl").open("a") as handle:
            for index in range(8):
                handle.write(json.dumps({**record, "verb": "hook:steer", "ts_ns": 100 + index}) + "\n")
        payload = recent_calls([root, root], "codex", "board-session")
        self.assertNotIn("PRIVATE_PLAN", payload)
        self.assertEqual(json.loads(payload)[0]["args"], ["post", "P7"])
        self.assertEqual(len(json.loads(payload)), 1)

    def test_recent_calls_keep_five_records_with_unicode_and_oversized_metadata(self):
        roots = [self.root / "primary", self.root / "fallback"]
        for root in roots:
            root.mkdir()
        for index in range(7):
            record = dict(harness="codex", session="board-session", ts_ns=index,
                          verb="search", args=[str(index) + "☃\\\"" * 300] * 3,
                          coverage={"member" + str(n): {"state": "searched", "rerank_status": "unavailable"} for n in range(100)},
                          exit_code=0)
            with (roots[index % 2] / "codex.jsonl").open("a") as handle:
                handle.write(json.dumps(record) + "\n")
        payload = recent_calls(roots, "codex", "board-session")
        calls = json.loads(payload)
        self.assertEqual(len(calls), 5)
        self.assertLessEqual(len(payload.encode("utf-8")), 2048)
        self.assertTrue(calls[-1]["args"][0].startswith("6"))

    def test_audit_tail_does_not_load_lifetime_logs_or_partial_rows(self):
        path = self.root / "large.jsonl"
        stale = json.dumps({"private": "old"}) + "\n"
        valid = json.dumps({"verb": "search"}) + "\n"
        path.write_text(stale * 10000 + "x" * AUDIT_TAIL_BYTES + "\n" + valid)
        self.assertEqual(tail_records(path), [{"verb": "search"}])


class BoardInstallTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="trufflepig-board-install-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("TRUFFLEPIG", "CLAUDE", "GROK", "KIMI", "MUSE", "OMP", "PI_"))}
        self.env.update(HOME=str(self.root), XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_STATE_HOME=str(self.root / "state"), TMPDIR=str(self.root))

    def test_each_harness_and_project_receives_both_skills(self):
        project = self.root / "project"
        result = subprocess.run([str(PLUGIN / "install.sh"), "--codex", "--grok", "--kimi", "--claude", "--omp",
                                 "--project", str(project)], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        for root in (self.root / ".agents", self.root / ".grok", self.root / ".kimi-code",
                     self.root / ".claude", self.root / ".omp/agent", project / ".agents"):
            for name in ("trufflepig-code-search", "trufflepig-plan-board"):
                self.assertEqual((root / "skills" / name).resolve(), PLUGIN / "skills" / name)

    def test_muse_installs_each_skill_and_manifest_declares_them(self):
        shim = self.root / "bin/muse"
        shim.parent.mkdir()
        shim.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
with Path(os.environ["MUSE_CAPTURE"]).open("a") as handle:
    handle.write(json.dumps(sys.argv[1:]) + "\\n")
''')
        shim.chmod(0o755)
        env = dict(self.env, PATH=f"{shim.parent}:{self.env['PATH']}", MUSE_CAPTURE=str(self.root / "muse-calls"))
        result = subprocess.run([str(PLUGIN / "install.sh"), "--muse"], env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in (self.root / "muse-calls").read_text().splitlines()]
        self.assertEqual([Path(call[2]).name for call in calls], ["trufflepig-code-search", "trufflepig-plan-board"])
        self.assertTrue(all(call[:2] == ["skills", "install"] and call[3:] == ["--scope", "user", "--force", "--json"] for call in calls))
        manifest = json.loads((PLUGIN / ".muse-plugin/plugin.json").read_text())
        self.assertEqual([skill["id"] for skill in manifest["capabilities"]["skills"]],
                         ["trufflepig-code-search", "trufflepig-plan-board"])
        for skill in manifest["capabilities"]["skills"]:
            self.assertTrue((PLUGIN / skill["path"]).is_file())
            self.assertIs(skill["enabledDefault"], True)

    def test_second_skill_conflict_prevents_partial_install(self):
        conflicting = self.root / ".agents/skills/trufflepig-plan-board"
        conflicting.mkdir(parents=True)
        (conflicting / "SKILL.md").write_text("user content")
        result = subprocess.run([str(PLUGIN / "install.sh"), "--codex"], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("unmanaged destination", result.stderr)
        self.assertFalse((self.root / ".local/bin").exists())
        self.assertFalse((conflicting.parent / "trufflepig-code-search").exists())

    def test_service_pins_board_path_from_override_xdg_and_passwd(self):
        scripts = str(PLUGIN / "scripts")
        sys.path.insert(0, scripts)
        self.addCleanup(sys.path.remove, scripts)
        spec = importlib.util.spec_from_file_location("board_install_agent", PLUGIN / "scripts/install_agent.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        canned = subprocess.CompletedProcess(args=[], returncode=0, stdout="/system-runtime\n", stderr="")
        with patch.dict(os.environ, {"HOME": "/sandbox/home"}, clear=True), \
                patch.object(module.pwd, "getpwuid", return_value=SimpleNamespace(pw_dir="/passwd/home")), \
                patch.object(module.subprocess, "run", return_value=canned):
            self.assertIn('Environment="TRUFFLEPIG_BOARD_DB=/passwd/home/.local/share/trufflepig/board.sqlite3"',
                          module.service_text("/bin/trufflepig", None))
            os.environ["XDG_DATA_HOME"] = "relative/data"
            self.assertIn('Environment="TRUFFLEPIG_BOARD_DB=/passwd/home/.local/share/trufflepig/board.sqlite3"',
                          module.service_text("/bin/trufflepig", None))
            os.environ["XDG_DATA_HOME"] = "/data/home with spaces"
            self.assertIn('Environment="TRUFFLEPIG_BOARD_DB=/data/home with spaces/trufflepig/board.sqlite3"',
                          module.service_text("/bin/trufflepig", None))
            os.environ["TRUFFLEPIG_BOARD_DB"] = "/explicit/100%/board.sqlite3"
            self.assertIn('Environment="TRUFFLEPIG_BOARD_DB=/explicit/100%%/board.sqlite3"',
                          module.service_text("/bin/trufflepig", None))
            os.environ["TRUFFLEPIG_BOARD_DB"] = ""
            with self.assertRaisesRegex(ValueError, "must not be empty"):
                module.service_text("/bin/trufflepig", None)


if __name__ == "__main__":
    unittest.main()
