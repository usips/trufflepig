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
        self.main_pid = 4242
        self.commands = []

    def run_systemctl(self, command, **kwargs):
        command = list(command)
        self.commands.append(command)
        if command[:2] != ["systemctl", "--user"]:
            return subprocess.CompletedProcess(command, 0, "", "")
        verb = command[2]
        if verb == "show":
            return subprocess.CompletedProcess(command, 0, f"{self.main_pid}\n", "")
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

    def apply(self, check_router, *, install_router=True, install_board=True):
        return systemd_units.apply_units(
            "/opt/trufflepig", self.unit_directory, self.runtime, True,
            install_router, install_board,
            "new router unit\n" if install_router else None,
            "new board unit\n" if install_board else None,
            check_router,
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

    def test_managed_stray_listener_fails_preflight_before_unit_mutations(self):
        stray = 999999999
        with patch.object(systemd_units, "require_router_exclusive", return_value={stray: "/opt/bin/trufflepig"}), \
                patch.object(systemd_units, "unit_active") as unit_active, \
                patch.object(systemd_units.subprocess, "run", side_effect=self.run_systemctl):
            with self.assertRaisesRegex(RouterIdentityConflict, f"router PID {stray} runs outside {ROUTER}"):
                self.apply(lambda *_args, **_kwargs: self.fail("readiness must not run"))
        unit_active.assert_not_called()
        self.assertEqual(self.commands, [["systemctl", "--user", "show", "-p", "MainPID", "--value", ROUTER]])
        self.assertEqual(self.router_unit.read_text(), "previous router unit\n")

    def test_managed_listener_owned_by_the_unit_passes_preflight(self):
        with patch.object(systemd_units, "require_router_exclusive",
                          return_value={self.main_pid: "/opt/bin/trufflepig"}), \
                patch.object(systemd_units.subprocess, "run", side_effect=self.run_systemctl):
            self.apply(lambda *_args, **_kwargs: None)
        self.assertEqual(self.router_unit.read_text(), "new router unit\n")
        stop = ["systemctl", "--user", "stop", ROUTER]
        enable_now = ["systemctl", "--user", "enable", "--now", ROUTER]
        self.assertLess(self.commands.index(stop), self.commands.index(enable_now))
        self.assertNotIn(["systemctl", "--user", "restart", ROUTER], self.commands)

    def assert_router_activation_failure_recovers(self, failed_operation, *, install_router=True):
        failed_once = False

        def run(command, **kwargs):
            nonlocal failed_once
            command = list(command)
            if command == ["systemctl", "--user", failed_operation, ROUTER] and not failed_once:
                failed_once = True
                self.commands.append(command)
                error = subprocess.CalledProcessError(1, command, stderr=f"router {failed_operation} failed")
                if kwargs.get("check"):
                    raise error
                return subprocess.CompletedProcess(command, 1, "", error.stderr)
            return self.run_systemctl(command, **kwargs)

        with patch.object(systemd_units.subprocess, "run", side_effect=run):
            with self.assertRaises(subprocess.CalledProcessError) as caught:
                self.apply(lambda *_args, **_kwargs: None,
                           install_router=install_router, install_board=True)

        self.assertIn(f"router {failed_operation} failed", caught.exception.stderr)
        self.assertTrue(failed_once)
        self.assertEqual(self.router_unit.read_text(), "previous router unit\n")
        self.assertEqual(self.board_unit.read_text(), "previous board unit\n")
        self.assertEqual(self.active, {ROUTER: True, BOARD: True})
        router_stop = ["systemctl", "--user", "stop", ROUTER]
        router_start = ["systemctl", "--user", "start", ROUTER]
        self.assertGreaterEqual(self.commands.count(router_stop), 2, self.commands)
        self.assertGreaterEqual(self.commands.count(router_start), 1)
        recovery_stop = max(index for index, command in enumerate(self.commands)
                            if command == router_stop)
        recovery_start = max(index for index, command in enumerate(self.commands)
                             if command == router_start)
        self.assertLess(recovery_stop, recovery_start)
        self.assertNotIn(["systemctl", "--user", "restart", ROUTER], self.commands)

    def test_router_stop_failure_restores_previous_unit_and_running_state(self):
        self.assert_router_activation_failure_recovers("stop", install_router=False)

    def test_router_start_failure_restores_previous_unit_and_running_state(self):
        self.assert_router_activation_failure_recovers("start", install_router=False)

    def test_board_only_upgrade_stops_and_starts_router_before_restarting_board(self):
        with patch.object(systemd_units.subprocess, "run", side_effect=self.run_systemctl):
            self.apply(lambda *_args, **_kwargs: None, install_router=False, install_board=True)

        router_stop = ["systemctl", "--user", "stop", ROUTER]
        router_start = ["systemctl", "--user", "start", ROUTER]
        board_stop = ["systemctl", "--user", "stop", BOARD]
        board_enable = ["systemctl", "--user", "enable", "--now", BOARD]
        self.assertLess(self.commands.index(board_stop), self.commands.index(router_stop))
        self.assertLess(self.commands.index(router_stop), self.commands.index(router_start))
        self.assertLess(self.commands.index(router_start), self.commands.index(board_enable))
        self.assertTrue(self.active[ROUTER])
        self.assertTrue(self.active[BOARD])
        self.assertNotIn(["systemctl", "--user", "restart", ROUTER], self.commands)
        self.assertNotIn(["systemctl", "--user", "restart", BOARD], self.commands)

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
        stop = ["systemctl", "--user", "stop", ROUTER]
        enable_now = ["systemctl", "--user", "enable", "--now", ROUTER]
        self.assertLess(self.commands.index(stop), self.commands.index(enable_now))
        self.assertNotIn(["systemctl", "--user", "restart", ROUTER], self.commands)
        self.assertNotIn(["systemctl", "--user", "restart", BOARD], self.commands)
        diagnostics = "\n".join(caught.exception.rollback_errors)
        self.assertIn("after clearing the listed router PIDs", diagnostics)
        self.assertIn(f"skipped restart of {ROUTER}", diagnostics)
        self.assertIn(f"systemctl --user stop {ROUTER} && systemctl --user start {ROUTER}", diagnostics)
        self.assertIn(f"skipped restart of {BOARD}", diagnostics)
        self.assertIn(f"systemctl --user restart {BOARD}", diagnostics)

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
        self.assertIn(["systemctl", "--user", "stop", ROUTER], self.commands)
        self.assertIn(["systemctl", "--user", "start", ROUTER], self.commands)
        self.assertIn(["systemctl", "--user", "restart", BOARD], self.commands)


if __name__ == "__main__":
    unittest.main()
