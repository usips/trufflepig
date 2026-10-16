"""Core installer contracts: links, runtimes, and config stores."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

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

    def test_installed_wrapper_runs_by_absolute_path_outside_shell_path(self):
        result = self.install("--codex")
        self.assertEqual(result.returncode, 0, result.stderr)

        wrapper = self.root / ".local/bin/trufflepig-agent"
        fake_cli = self.root / "fake trufflepig"
        fake_cli.write_text(f'''#!{sys.executable}
import json
import os
import sys
from pathlib import Path
capture = Path(os.environ["CAPTURE"])
calls = json.loads(capture.read_text()) if capture.exists() else []
calls.append({{"args": sys.argv[1:], "session": os.environ.get("TRUFFLEPIG_SESSION")}})
capture.write_text(json.dumps(calls))
print(json.dumps({{"hits": [], "truncated": False}}))
''')
        fake_cli.chmod(0o755)
        capture = self.root / "forwarded-arguments.json"
        path = os.pathsep.join((str(Path(sys.executable).parent), "/usr/bin", "/bin"))
        self.assertNotIn(str(wrapper.parent), path.split(os.pathsep))
        env = dict(self.env, PATH=path, TRUFFLEPIG_BINARY=str(fake_cli),
                   TRUFFLEPIG_AGENT_HARNESS="codex", TRUFFLEPIG_SESSION="install-test-session",
                   CAPTURE=str(capture))
        result = subprocess.run([str(wrapper), "search", "installed-wrapper-probe"],
                                env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = json.loads(capture.read_text())
        forwarded = next(call for call in calls if "search" in call["args"])
        self.assertIn("installed-wrapper-probe", forwarded["args"])
        self.assertIn("--client", forwarded["args"])
        self.assertIn("codex", forwarded["args"])
        self.assertNotIn("--session", forwarded["args"])
        self.assertEqual(forwarded["session"], "install-test-session")
        self.assertTrue(any(call["args"][-2:] == ["board", "pending"] for call in calls))

        audit = self.root / "state/trufflepig/agent-audit/codex.jsonl"
        records = [json.loads(line) for line in audit.read_text().splitlines()]
        search_record = next(record for record in records if record["verb"] == "search")
        self.assertEqual(search_record["session"], "install-test-session")

        for profile in (".profile", ".bash_profile", ".bashrc", ".zprofile", ".zshrc"):
            self.assertFalse((self.root / profile).exists(), profile)

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

    def run_claude_check(self):
        """Run `install.sh --claude --check` in-process; return the patched router check and unit calls."""
        module = self.load_installer("install_claude_check_contract")
        runtime = self.root / "checked-runtime"
        applied_services = []
        completed = subprocess.CompletedProcess(["check_agent.py"], 0, "", "")
        # An inherited developer CLAUDE_CONFIG_DIR must neither receive the settings write nor leak in.
        developer = tempfile.TemporaryDirectory(prefix="developer-claude-")
        self.addCleanup(developer.cleanup)
        developer_settings = Path(developer.name) / "settings.json"
        developer_settings.write_text('{"model": "developer"}\n')
        with patch.dict(os.environ, {"CLAUDE_CONFIG_DIR": developer.name}), \
                patch.dict(os.environ, self.env, clear=True), \
                patch.object(sys, "argv", ["install.sh", "--claude", "--check", str(self.root)]), \
                patch.object(module.shutil, "which", return_value="/test/bin/trufflepig"), \
                patch.object(module, "require_board_api") as require_api, \
                patch.object(module, "system_runtime_path", return_value=runtime), \
                patch.object(module, "require_current_router") as require_router, \
                patch.object(module, "apply_units", side_effect=lambda *args: applied_services.append(args)), \
                patch.object(module.subprocess, "run", return_value=completed):
            self.assertEqual(module.main(), 0)
        self.assertEqual(developer_settings.read_text(), '{"model": "developer"}\n')
        self.assertTrue((self.root / "claude-config/settings.json").is_file())
        require_api.assert_called_once_with("/test/bin/trufflepig")
        self.assertEqual(applied_services, [])
        return require_router, runtime

    def test_claude_check_verifies_hooks_and_current_service_without_restart(self):
        require_router, runtime = self.run_claude_check()
        require_router.assert_called_once_with(runtime, require_schema=True, managed_unit=False)

    def test_claude_check_requires_unit_ownership_when_the_unit_file_exists(self):
        unit = self.root / "config/systemd/user/trufflepig-system.service"
        unit.parent.mkdir(parents=True)
        unit.write_text("installed unit\n")
        require_router, runtime = self.run_claude_check()
        require_router.assert_called_once_with(runtime, require_schema=True, managed_unit=True)


if __name__ == "__main__":
    unittest.main()
