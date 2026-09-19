"""Claude installation and per-session environment propagation contracts."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest

PLUGIN = Path(__file__).resolve().parents[1]


class ClaudeIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="trufflepig-claude-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("TRUFFLEPIG", "CLAUDE", "CODEX", "KIMI", "MUSE", "OMP", "PI_"))}
        self.env.update(HOME=str(self.root), XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_STATE_HOME=str(self.root / "state"), TMPDIR=str(self.root))

    def install(self):
        return subprocess.run([str(PLUGIN / "install.sh"), "--claude"],
                              env=self.env, text=True, capture_output=True)

    def hook(self, session, env_file, event="SessionStart"):
        return subprocess.run([str(PLUGIN / "hooks/claude-session.py")],
                              input=json.dumps(dict(session_id=session, hook_event_name=event)),
                              env=dict(self.env, CLAUDE_ENV_FILE=str(env_file)), text=True, capture_output=True)

    def test_install_preserves_settings_and_merges_once(self):
        config = self.root / "custom claude config"
        self.env["CLAUDE_CONFIG_DIR"] = str(config)
        config.mkdir()
        path = config / "settings.json"
        original = {"model": "user-selected", "permissions": {"deny": ["Bash(rm *)"]},
                    "hooks": {"SessionStart": [{"matcher": "resume", "hooks": [
                        {"type": "command", "command": "existing-hook"}]}],
                        "Stop": [{"hooks": [{"type": "command", "command": "other-hook"}]}]},
                    "sandbox": {"enabled": True, "filesystem": {"allowWrite": ["/existing"]}}}
        path.write_text(json.dumps(original))
        path.chmod(0o600)
        for _ in range(2):
            result = self.install()
            self.assertEqual(result.returncode, 0, result.stderr)
        settings = json.loads(path.read_text())
        self.assertEqual(settings["model"], original["model"])
        self.assertEqual(settings["permissions"], original["permissions"])
        self.assertEqual(settings["hooks"]["Stop"], original["hooks"]["Stop"])
        self.assertEqual(settings["hooks"]["SessionStart"][0], original["hooks"]["SessionStart"][0])
        self.assertEqual(len(settings["hooks"]["SessionStart"]), 2)
        self.assertNotIn("matcher", settings["hooks"]["SessionStart"][1])
        self.assertTrue(settings["sandbox"]["enabled"])
        self.assertEqual(len(settings["sandbox"]["filesystem"]["allowWrite"]), 2)
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual((config / "skills/trufflepig-code-search").resolve(),
                         PLUGIN / "skills/trufflepig-code-search")
        self.assertFalse((self.root / ".agents/skills").exists())
        # The SessionStart hook command must resolve to an installed executable.
        hook = self.root / ".local/bin/trufflepig-claude-session"
        self.assertEqual(hook.resolve(), PLUGIN / "hooks/claude-session.py")

    def test_invalid_settings_fail_before_installing_files(self):
        config = self.root / ".claude"
        config.mkdir()
        path = config / "settings.json"
        path.write_text('{"broken":')
        result = self.install()
        self.assertEqual(result.returncode, 2)
        self.assertEqual(path.read_text(), '{"broken":')
        self.assertFalse((self.root / ".local/bin").exists())

    def test_hook_preserves_other_exports_and_quotes_session_as_data(self):
        env_file = self.root / "environment with spaces"
        env_file.write_text("export EXISTING=preserved\n")
        session = "session'$(touch injected);\nnew line"
        result = self.hook(session, env_file)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, "", ""))
        command = '. ' + shlex.quote(str(env_file)) + '; exec python3 -c ' + shlex.quote(
            'import json,os; print(json.dumps([os.environ["EXISTING"],os.environ["TRUFFLEPIG_CLAUDE_SESSION"]]))')
        result = subprocess.run(["sh", "-c", command], cwd=self.root, env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), ["preserved", session])
        self.assertFalse((self.root / "injected").exists())

    def test_parallel_sessions_have_independent_environment_files(self):
        first, second = self.root / "first.env", self.root / "second.env"
        self.hook("first-session", first)
        self.hook("second-session", second)
        self.assertIn("first-session", first.read_text())
        self.assertNotIn("second-session", first.read_text())
        self.assertIn("second-session", second.read_text())
        self.hook("cleared-session", first)
        result = subprocess.run(["sh", "-c", '. "$1"; printf %s "$TRUFFLEPIG_CLAUDE_SESSION"',
                                 "sh", str(first)], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.stdout, "cleared-session")

    def test_missing_env_file_and_non_start_events_are_harmless(self):
        result = subprocess.run([str(PLUGIN / "hooks/claude-session.py")],
                                input='{"hook_event_name":"SessionStart","session_id":"abc"}',
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        path = self.root / "unused"
        self.hook("abc", path, "SessionEnd")
        self.assertFalse(path.exists())
        result = self.hook("abc", self.root / "missing/parent/environment")
        self.assertEqual(result.returncode, 0)
        self.assertIn("attribution unavailable", result.stderr)


if __name__ == "__main__":
    unittest.main()
