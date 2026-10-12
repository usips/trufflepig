"""Gemini's native skill links and shared runtime configuration."""
import json

from agent_install_case import PLUGIN, AgentInstallCase


class GeminiInstallTests(AgentInstallCase):
    def setUp(self):
        super().setUp()
        self.gemini_home = self.root / "Gemini CLI home"
        self.env["GEMINI_CLI_HOME"] = str(self.gemini_home)

    def test_installs_both_skills_and_configures_shared_runtime(self):
        runtime = self.root / "disk runtime"
        for _ in range(2):
            result = self.install("--gemini", "--runtime-dir", str(runtime))
            self.assertEqual(result.returncode, 0, result.stderr)

        skills = self.gemini_home / ".gemini/skills"
        for name in ("trufflepig-code-search", "trufflepig-plan-board"):
            self.assertEqual((skills / name).resolve(), PLUGIN / "skills" / name)
        config = json.loads((self.root / "config/trufflepig/agent-runtime.json").read_text())
        self.assertEqual(config["runtime_dir"], str(runtime))
        self.assertEqual(config["spool_dir"], str(runtime / "spool"))

    def test_existing_native_skill_destination_prevents_partial_install(self):
        destination = self.gemini_home / ".gemini/skills/trufflepig-plan-board"
        destination.mkdir(parents=True)
        marker = destination / "SKILL.md"
        marker.write_text("keep user skill")

        result = self.install("--gemini")

        self.assertEqual(result.returncode, 2)
        self.assertIn("unmanaged destination", result.stderr)
        self.assertEqual(marker.read_text(), "keep user skill")
        self.assertFalse((self.root / ".local/bin").exists())

    def test_default_destination_uses_the_gemini_cli_home(self):
        self.env.pop("GEMINI_CLI_HOME")
        result = self.install("--gemini")
        self.assertEqual(result.returncode, 0, result.stderr)
        skills = self.root / ".gemini/skills"
        for name in ("trufflepig-code-search", "trufflepig-plan-board"):
            self.assertEqual((skills / name).resolve(), PLUGIN / "skills" / name)


if __name__ == "__main__":
    import unittest

    unittest.main()
