"""Router conflict preflight and transactional unit rollback."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT_DIR = Path(__file__).resolve().parents[1] / "scripts"
sys.path.insert(0, str(SCRIPT_DIR))
try:
    from router_identity import RouterIdentityConflict
    import systemd_units
finally:
    sys.path.remove(str(SCRIPT_DIR))


ROUTER = "trufflepig-system.service"
BOARD = "trufflepig-board.service"


class AgentRouterIdentityInstallTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="router-unit-conflict-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.unit_directory = self.root / "systemd/user"
        self.unit_directory.mkdir(parents=True)
        self.router_unit = self.unit_directory / ROUTER
        self.board_unit = self.unit_directory / BOARD
        self.router_unit.write_text("previous router unit\n")
        self.board_unit.write_text("previous board unit\n")
        self.runtime = self.root / "runtime"
        self.enabled = {ROUTER: "disabled", BOARD: "disabled"}
        self.active = {ROUTER: True, BOARD: True}
        self.commands = []

    def run_systemctl(self, command, **kwargs):
        command = list(command)
        self.commands.append(command)
        if command[:2] != ["systemctl", "--user"]:
            return subprocess.CompletedProcess(command, 0, "", "")
        verb = command[2]
        if verb in ("is-active", "is-enabled"):
            name = command[3]
            value = ("active" if self.active[name] else "inactive") if verb == "is-active" \
                else self.enabled[name]
            return subprocess.CompletedProcess(command, 0, value + "\n", "")
        if verb == "stop":
            self.active[command[3]] = False
            if command[3] == ROUTER:
                self.active[BOARD] = False
        elif verb == "start":
            self.active[command[3]] = True
        elif verb == "restart":
            self.active[command[3]] = True
        elif verb == "enable":
            name = command[-1]
            self.enabled[name] = "enabled"
            if "--now" in command:
                self.active[name] = True
        elif verb == "disable":
            name = command[-1]
            self.enabled[name] = "disabled"
            if "--now" in command:
                self.active[name] = False
        return subprocess.CompletedProcess(command, 0, "", "")

    def apply(self, check_router):
        return systemd_units.apply_units(
            "/opt/trufflepig", self.unit_directory, self.runtime, True,
            True, True, "new router unit\n", "new board unit\n", check_router,
        )

    def test_existing_duplicate_fails_before_unit_state_reads_or_mutations(self):
        conflict = RouterIdentityConflict("duplicate listener owners")
        with patch.object(systemd_units, "require_router_exclusive", side_effect=conflict) as preflight, \
                patch.object(systemd_units, "unit_active") as unit_active, \
                patch.object(systemd_units, "unit_enablement") as unit_enablement, \
                patch.object(systemd_units.subprocess, "run") as run:
            with self.assertRaises(RouterIdentityConflict):
                self.apply(lambda *_args, **_kwargs: self.fail("readiness must not run"))
        preflight.assert_called_once_with(
            self.runtime, "resolve the conflict, then rerun the installer with `--systemd`",
        )
        unit_active.assert_not_called()
        unit_enablement.assert_not_called()
        run.assert_not_called()
        self.assertEqual(self.router_unit.read_text(), "previous router unit\n")
        self.assertEqual(self.board_unit.read_text(), "previous board unit\n")

    def test_postactivation_conflict_restores_config_without_restarting_active_units(self):
        conflict = RouterIdentityConflict("router identity changed during activation")

        def raise_conflict(*_args, **_kwargs):
            raise conflict

        with patch.object(systemd_units, "require_router_exclusive", return_value={}), \
                patch.object(systemd_units.subprocess, "run", side_effect=self.run_systemctl):
            with self.assertRaises(RouterIdentityConflict) as caught:
                self.apply(raise_conflict)

        self.assertEqual(self.router_unit.read_text(), "previous router unit\n")
        self.assertEqual(self.board_unit.read_text(), "previous board unit\n")
        self.assertEqual(self.enabled, {ROUTER: "disabled", BOARD: "disabled"})
        self.assertFalse(self.active[BOARD])
        self.assertIn(
            ["systemctl", "--user", "enable", "--now", ROUTER], self.commands,
        )
        self.assertNotIn(["systemctl", "--user", "restart", ROUTER], self.commands)
        self.assertNotIn(["systemctl", "--user", "restart", BOARD], self.commands)
        diagnostics = "\n".join(caught.exception.rollback_errors)
        self.assertIn("after clearing the listed router PIDs", diagnostics)
        for name in (ROUTER, BOARD):
            self.assertIn(f"skipped restart of {name}", diagnostics)
            self.assertIn(f"systemctl --user restart {name}", diagnostics)

    def test_ordinary_readiness_failure_keeps_full_active_unit_rollback(self):
        def fail_readiness(*_args, **_kwargs):
            raise ValueError("router readiness failed")

        with patch.object(systemd_units, "require_router_exclusive", return_value={}), \
                patch.object(systemd_units.subprocess, "run", side_effect=self.run_systemctl):
            with self.assertRaisesRegex(ValueError, "router readiness failed"):
                self.apply(fail_readiness)

        self.assertEqual(self.router_unit.read_text(), "previous router unit\n")
        self.assertEqual(self.board_unit.read_text(), "previous board unit\n")
        self.assertEqual(self.enabled, {ROUTER: "disabled", BOARD: "disabled"})
        self.assertIn(["systemctl", "--user", "restart", ROUTER], self.commands)
        self.assertIn(["systemctl", "--user", "restart", BOARD], self.commands)


if __name__ == "__main__":
    unittest.main()
