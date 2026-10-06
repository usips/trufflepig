"""Core installer contracts: links, runtimes, and config stores."""
import json
from pathlib import Path
import subprocess
import sys
import unittest

from agent_install_case import PLUGIN, AgentInstallCase


class AgentInstallTests(AgentInstallCase):
    def test_personal_and_quoted_project_installs_are_idempotent(self):
        project = self.root / "project with spaces"
        for _ in range(2):
            result = self.install("--codex", "--project", str(project))
            self.assertEqual(result.returncode, 0, result.stderr)
        skill = PLUGIN / "skills/trufflepig-code-search"
        self.assertEqual((self.root / ".agents/skills/trufflepig-code-search").resolve(), skill)
        self.assertEqual((project / ".agents/skills/trufflepig-code-search").resolve(), skill)
        config = json.loads((self.root / "config/trufflepig/agent-runtime.json").read_text())
        self.assertEqual(config["spool_dir"], str(self.root / ".cache/codex-tmp/trufflepig-agent/spool"))
        # The wrapper's sibling module must remain importable through its symlink.
        result = subprocess.run([str(self.root / ".local/bin/trufflepig-agent"), "--help"],
                                env=dict(self.env, TRUFFLEPIG_BINARY="/nonexistent"), capture_output=True)
        self.assertEqual(result.returncode, 127, result.stderr)
        self.assertNotIn(b"ModuleNotFoundError", result.stderr)

    def test_unmanaged_skill_prevents_partial_installation(self):
        skill = self.root / ".agents/skills/trufflepig-code-search"
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text("user content")
        result = self.install("--codex")
        self.assertEqual(result.returncode, 2)
        self.assertIn("unmanaged destination", result.stderr)
        self.assertEqual((skill / "SKILL.md").read_text(), "user content")
        self.assertFalse((self.root / ".local/bin").exists())

    def test_grok_install_respects_home_and_reuses_skill(self):
        grok = self.root / "grok home"
        self.env["GROK_HOME"] = str(grok)
        for _ in range(2):
            result = self.install("--grok")
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((grok / "skills/trufflepig-code-search").resolve(),
                         PLUGIN / "skills/trufflepig-code-search")
        self.assertFalse((self.root / ".agents").exists())
        self.assertFalse((grok / "config.toml").exists())
        config = json.loads((self.root / "config/trufflepig/agent-runtime.json").read_text())
        self.assertTrue(Path(config["runtime_dir"]).is_dir())

    def test_runtime_directory_rejects_ram_tmp(self):
        result = self.install("--codex", "--runtime-dir", "/tmp/trufflepig-test")
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.root / ".local/bin").exists())

    def test_board_install_selector_is_explicit_in_help(self):
        result = self.install("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--board", result.stdout)
        self.assertFalse((self.root / ".local/bin").exists())

    def test_check_reports_broken_runtime(self):
        fake = self.root / "failing wrapper"
        fake.write_text('#!/bin/sh\necho "daemon: unexpected argument --format" >&2\nexit 2\n')
        fake.chmod(0o755)
        result = subprocess.run([sys.executable, str(PLUGIN / "scripts/check_agent.py"),
                                 "--wrapper", str(fake), str(self.root)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("member daemons", result.stderr)


if __name__ == "__main__":
    unittest.main()
