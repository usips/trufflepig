"""OpenCode skill and plugin links, runtime configuration, and wrapper attribution."""
import json
import subprocess

from agent_install_case import PLUGIN, AgentInstallCase

SKILLS = ("trufflepig-code-search", "trufflepig-plan-board")


class OpenCodeInstallTests(AgentInstallCase):
    def setUp(self):
        super().setUp()
        self.config = self.root / "config/opencode"

    def test_installs_skills_and_session_plugin_with_shared_runtime(self):
        runtime = self.root / "disk runtime"
        for _ in range(2):
            result = self.install("--opencode", "--runtime-dir", str(runtime))
            self.assertEqual(result.returncode, 0, result.stderr)

        for name in SKILLS:
            self.assertEqual((self.config / "skills" / name).resolve(), PLUGIN / "skills" / name)
        self.assertEqual((self.config / "plugins/trufflepig-session.ts").resolve(),
                         PLUGIN / "opencode/session.ts")
        config = json.loads((self.root / "config/trufflepig/agent-runtime.json").read_text())
        self.assertEqual(config["runtime_dir"], str(runtime))
        self.assertEqual(config["spool_dir"], str(runtime / "spool"))

    def test_existing_unmanaged_plugin_prevents_partial_install(self):
        destination = self.config / "plugins/trufflepig-session.ts"
        destination.parent.mkdir(parents=True)
        destination.write_text("// keep user plugin\n")

        result = self.install("--opencode")

        self.assertEqual(result.returncode, 2)
        self.assertIn("unmanaged destination", result.stderr)
        self.assertEqual(destination.read_text(), "// keep user plugin\n")
        self.assertFalse((self.config / "skills").exists())
        self.assertFalse((self.root / ".local/bin").exists())


class OpenCodeAttributionTests(AgentInstallCase):
    def setUp(self):
        super().setUp()
        for key in [key for key in self.env if key.startswith(
                ("CODEX", "CLAUDE", "GROK", "KIMI", "MUSE", "GEMINI", "OMP", "OPENCODE", "AGENT"))]:
            del self.env[key]
        fake = self.root / "fake-cli"
        fake.write_text('#!/usr/bin/env python3\nimport sys\nsys.stdout.write(\'{"hits":[]}\\n\')\n')
        fake.chmod(0o755)
        self.env.update(TRUFFLEPIG_BINARY=str(fake), OPENCODE="1", CLAUDECODE="1")

    def record(self, *args):
        result = subprocess.run([str(PLUGIN / "bin/trufflepig-agent"), "search", *args],
                                env=self.env, cwd=self.root, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        audit = self.root / "state/trufflepig/agent-audit/opencode.jsonl"
        return json.loads(audit.read_text().splitlines()[-1])

    def test_marker_precedes_claude_and_plugin_session_attributes_calls(self):
        self.env["TRUFFLEPIG_OPENCODE_SESSION"] = "ses_child"
        record = self.record("with plugin")
        self.assertEqual(record["harness"], "opencode")
        self.assertEqual(record["session"], "ses_child")


if __name__ == "__main__":
    import unittest

    unittest.main()
