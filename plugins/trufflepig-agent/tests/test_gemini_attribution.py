import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


PLUGIN = Path(__file__).resolve().parents[1]


class GeminiAttributionTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="trufflepig-gemini-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        harness_prefixes = ("TRUFFLEPIG", "CODEX", "CLAUDE", "GROK", "KIMI", "MUSE", "OPENCODE", "GEMINI",
                            "AGENT_SESSION", "OMP", "PI_")
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(harness_prefixes)}
        self.env.update(HOME=str(self.root), XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_STATE_HOME=str(self.root / "state"), TMPDIR=str(self.root))
        self.fake = self.root / "fake-cli"
        self.fake.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
argv = sys.argv[1:]
pending = any(argv[index:index + 2] == ["board", "pending"] for index in range(len(argv) - 1))
capture = os.environ["CAPTURE"] + ".pending" if pending else os.environ["CAPTURE"]
Path(capture).write_text(json.dumps(argv))
sys.stdout.write('{"hits":[],"truncated":false}\\n')
''')
        self.fake.chmod(0o755)
        self.env.update(TRUFFLEPIG_BINARY=str(self.fake), CAPTURE=str(self.root / "capture.json"))

    def run_wrapper(self, *args):
        return subprocess.run([str(PLUGIN / "bin/trufflepig-agent"), *args], env=self.env,
                              cwd=self.root, capture_output=True)

    def record(self, client="gemini"):
        audit = self.root / "state/trufflepig/agent-audit" / f"{client}.jsonl"
        return json.loads(audit.read_text().splitlines()[-1])

    def test_harness_marker_without_session_remains_unattributed(self):
        self.env["GEMINI_CLI"] = "1"

        self.assertEqual(self.run_wrapper("search", "first").returncode, 0)
        first = self.record()
        self.assertEqual(first["harness"], "gemini")
        self.assertIsNone(first["session"])

        self.assertEqual(self.run_wrapper("search", "second").returncode, 0)
        self.assertIsNone(self.record()["session"])

    def test_native_session_is_used_if_available(self):
        self.env.update(GEMINI_CLI="1", GEMINI_SESSION_ID="gemini-session")

        self.assertEqual(self.run_wrapper("search", "query").returncode, 0)

        self.assertEqual(self.record()["session"], "gemini-session")
        argv = json.loads((self.root / "capture.json").read_text())
        self.assertNotIn("--session", argv)

    def test_native_marker_precedes_wrapper_hint_and_claim(self):
        self.env.update(GEMINI_CLI="1", GEMINI_SESSION_ID="gemini-session",
                        TRUFFLEPIG_AGENT_HARNESS="manual", TRUFFLEPIG_SESSION="configured-session")
        self.assertEqual(self.run_wrapper("search", "env").returncode, 0)
        self.assertEqual(self.record("gemini")["session"], "configured-session")
        self.assertEqual(self.record("gemini")["identity_source"], "observed")

        self.assertEqual(self.run_wrapper("--client", "cli", "--session", "cli-session",
                                          "search", "explicit").returncode, 0)
        record = self.record("gemini")
        self.assertEqual(record["session"], "cli-session")
        self.assertEqual(record["claimed_client"], "cli")
        self.assertEqual(self.record("gemini")["identity_source"], "observed")
        argv = json.loads((self.root / "capture.json").read_text())
        self.assertIn("--client", argv)
        self.assertEqual(argv[argv.index("--client") + 1], "cli")


if __name__ == "__main__":
    unittest.main()
