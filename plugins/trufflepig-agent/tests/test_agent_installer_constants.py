"""Installer constants and board failures checked against an actual Rust router."""
from pathlib import Path
import subprocess
import unittest

from agent_real_router_case import AgentRealRouterCase


class AgentInstallerConstantsTests(AgentRealRouterCase):
    def test_installer_constants_match_rust(self):
        module = self.load_installer("install_actual_constants")
        binary = self.real_trufflepig_binary()
        probe = subprocess.run([binary, "--board-api-version"], env=self.env, text=True,
                               capture_output=True, timeout=5)
        self.assertEqual(probe.returncode, 0, probe.stderr)
        self.assertEqual(probe.stdout, f"{module.BOARD_API}\n")
        self.assertEqual(probe.stderr, "")
        status = self.start_real_router(binary, module)
        self.assertEqual(status["board_api"], module.BOARD_API)
        self.assertEqual(status["schema_supported"], module.BOARD_SCHEMA)
        self.assertEqual(status["schema_file"], module.BOARD_SCHEMA)
        self.assertEqual(status["board_db"], self.env["TRUFFLEPIG_BOARD_DB"])
        self.assertTrue(Path(self.env["TRUFFLEPIG_BOARD_DB"]).is_file())

    def assert_real_router_only_install(self, *, config=None, refused_parent=False):
        module = self.load_installer("install_actual_board_failure")
        binary = self.real_trufflepig_binary()
        if config is not None:
            path = Path(self.env["XDG_CONFIG_HOME"]) / "trufflepig/board.toml"
            path.parent.mkdir(parents=True)
            path.write_text(config)
        if refused_parent:
            Path(self.env["TRUFFLEPIG_BOARD_DB"]).parent.write_text("not a directory\n")
        status = self.start_real_router(binary, module)
        self.assertIsNone(status.get("schema_file"))
        self.assertFalse(Path(self.env["TRUFFLEPIG_BOARD_DB"]).exists())
        self.service_shims()
        result = self.run_installer_main("--systemd", router_timeout=0.05)
        self.assertEqual(result.returncode, 0, result.stderr)
        if status.get("board_error"):
            self.assertIn(f"warning: router board_error: {status['board_error']}", result.stderr)
        self.assertTrue(self.active_units()["trufflepig-system.service"])

    def test_router_only_install_with_actual_broken_board_toml(self):
        self.assert_real_router_only_install(config="mode = [\n")

    def test_router_only_install_with_actual_remote_mode(self):
        self.assert_real_router_only_install(config='mode = "remote"\n')

    def test_router_only_install_with_actual_refused_parent(self):
        self.assert_real_router_only_install(refused_parent=True)


if __name__ == "__main__":
    unittest.main()
