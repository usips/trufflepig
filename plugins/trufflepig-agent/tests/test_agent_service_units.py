"""Rendered systemd unit contracts: quoting, paths, and dependencies."""
import os
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

from agent_install_case import AgentInstallCase


class AgentServiceUnitTests(AgentInstallCase):
    def test_service_quotes_paths_and_replaces_children(self):
        module = self.load_installer("install_agent")
        canned = subprocess.CompletedProcess(args=[], returncode=0, stdout="/system-runtime\n", stderr="")
        with patch.object(module.subprocess, "run", return_value=canned):
            text = module.service_text('/home/a path/100%/trufflepig', Path('/home/a path/spool'))
        self.assertIn('ExecStart="/home/a path/100%%/trufflepig" system-serve', text)
        self.assertIn('Environment="TRUFFLEPIG_SPOOL_DIR=/home/a path/spool"', text)
        self.assertIn('KillMode=control-group', text)

    def test_board_service_is_foreground_and_shares_router_paths(self):
        module = self.load_installer("install_board_service")
        canned = subprocess.CompletedProcess(args=[], returncode=0,
                                             stdout=f"{self.root}/runtime with spaces/100%\n", stderr="")
        with patch.dict(os.environ, {"HOME": str(self.root),
                                     "TRUFFLEPIG_BOARD_DB": str(self.root / "data with spaces/100%/board.sqlite3"),
                                     "TRUFFLEPIG_SYSTEM_DIR": str(self.root / "runtime with spaces/100%")}, clear=True), \
                patch.object(module.subprocess, "run", return_value=canned):
            board = module.render_service("trufflepig-board.service", "/bin with spaces/100%/trufflepig")
            router = module.service_text("/bin with spaces/100%/trufflepig", self.root / "spool")
        self.assertIn('ExecStart="/bin with spaces/100%%/trufflepig" board-serve --listen 127.0.0.1:0', board)
        self.assertIn("Type=simple", board)
        self.assertIn("Restart=on-failure", board)
        self.assertIn("WantedBy=default.target", board)
        for line in ("After=trufflepig-system.service", "Wants=trufflepig-system.service",
                     "PartOf=trufflepig-system.service"):
            self.assertIn(line, board)
        for name in ("TRUFFLEPIG_BOARD_DB", "TRUFFLEPIG_SYSTEM_DIR"):
            line = next(line for line in board.splitlines() if line.startswith(f'Environment="{name}='))
            self.assertIn(line, router)
            self.assertIn("100%%", line)
        self.assertNotIn("TRUFFLEPIG_SPOOL_DIR", board)
        self.assertFalse((self.root / "runtime with spaces").exists(), "unit generation must not create a token")

    def test_rendered_unit_pins_binary_resolved_runtime_dir(self):
        module = self.load_installer("install_system_dir_threading")
        canned = Path("/canned/trufflepig/system")
        completed = subprocess.CompletedProcess(args=["/bin/trufflepig", "system", "dir"],
                                                returncode=0, stdout=f"{canned}\n", stderr="")
        with patch.object(module.subprocess, "run", return_value=completed) as run:
            text = module.service_text("/bin/trufflepig", None)
        run.assert_called_once_with(["/bin/trufflepig", "system", "dir"], stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, timeout=5)
        self.assertIn(f'Environment="TRUFFLEPIG_SYSTEM_DIR={canned}"', text)


if __name__ == "__main__":
    unittest.main()
