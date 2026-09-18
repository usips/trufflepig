"""Installer contracts with isolated personal stores and no live services."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

PLUGIN = Path(__file__).resolve().parents[1]


class AgentInstallTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="trufflepig-install-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.env = dict({k: v for k, v in os.environ.items() if k not in ("CLAUDE_CONFIG_DIR", "GROK_HOME")}, HOME=str(self.root), XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_STATE_HOME=str(self.root / "state"))

    def install(self, *args):
        return subprocess.run([str(PLUGIN / "install.sh"), *args], env=self.env, text=True, capture_output=True)

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

    def test_service_quotes_paths_and_replaces_children(self):
        spec = importlib.util.spec_from_file_location("install_agent", PLUGIN / "scripts/install_agent.py")
        module = importlib.util.module_from_spec(spec)
        sys.path.insert(0, str(PLUGIN / "scripts"))
        self.addCleanup(sys.path.remove, str(PLUGIN / "scripts"))
        spec.loader.exec_module(module)
        text = module.service_text('/home/a path/100%/trufflepig', Path('/home/a path/spool'))
        self.assertIn('ExecStart="/home/a path/100%%/trufflepig" system-serve', text)
        self.assertIn('Environment="TRUFFLEPIG_SPOOL_DIR=/home/a path/spool"', text)
        self.assertIn('KillMode=control-group', text)

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
