"""Rollback failures preserve the installer error and continue recovery."""
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

from agent_install_case import AgentInstallCase, UNIT_NAMES


class AgentBoardRollbackErrorTests(AgentInstallCase):
    def prepare_units(self):
        self.service_shims()
        units = self.root / "config/systemd/user"
        units.mkdir(parents=True)
        previous = {name: f"previous {name}\n" for name in UNIT_NAMES}
        for name, text in previous.items():
            (units / name).write_text(text)
            self.systemctl("enable", name)
            self.systemctl("start", name)
        installer = self.load_installer("install_rollback_errors")
        return sys.modules[installer.apply_units.__module__], units, previous

    def apply_replacement(self, module, units, check_router):
        module.apply_units(str(self.root / "service-bin/trufflepig"), units,
                           Path(self.env["TRUFFLEPIG_SYSTEM_DIR"]), True, True, True,
                           "new router unit\n", "new board unit\n", check_router)

    def test_rollback_survives_a_dead_bus_and_reraises(self):
        module, units, previous = self.prepare_units()
        real_run = subprocess.run
        for mode in ("unreachable bus", "missing systemctl"):
            with self.subTest(mode=mode):
                for name in UNIT_NAMES:
                    self.systemctl("start", name)
                original = ValueError("original router readiness failure")
                disconnected = False
                attempts = []

                def fail_router(_, *, managed_restart=False):
                    nonlocal disconnected
                    disconnected = True
                    raise original

                def run(command, **kwargs):
                    if not disconnected or command[0] != "systemctl":
                        return real_run(command, **kwargs)
                    attempts.append((command, kwargs))
                    if mode == "missing systemctl":
                        raise FileNotFoundError("systemctl vanished during rollback")
                    if kwargs.get("check"):
                        raise subprocess.CalledProcessError(1, command,
                                                            stderr="Failed to connect to bus")
                    return subprocess.CompletedProcess(command, 1, "", "Failed to connect to bus")

                with patch.dict(os.environ, self.env), patch.object(module.subprocess, "run", run):
                    with self.assertRaises((OSError, ValueError, subprocess.CalledProcessError)) as caught:
                        self.apply_replacement(module, units, fail_router)

                self.assertIs(caught.exception, original)
                for name, text in previous.items():
                    self.assertEqual((units / name).read_text(), text)
                    self.assertIn(["systemctl", "--user", "restart", name],
                                  [command for command, _ in attempts])
                self.assertIn(["systemctl", "--user", "daemon-reload"],
                              [command for command, _ in attempts])
                self.assertTrue(all(kwargs.get("check") is False for _, kwargs in attempts))
                diagnostics = "\n".join(getattr(original, "rollback_errors", ()))
                self.assertIn("bus" if mode == "unreachable bus" else "vanished", diagnostics)

    def test_rollback_continues_after_enablement_query_failure(self):
        module, units, previous = self.prepare_units()
        previous_hashes = self.loaded_unit_hashes()
        original = ValueError("original router readiness failure")
        real_run = subprocess.run
        failed = False

        def fail_router(_, *, managed_restart=False):
            nonlocal failed
            failed = True
            raise original

        def run(command, **kwargs):
            if failed and command[:3] == ["systemctl", "--user", "is-enabled"]:
                return subprocess.CompletedProcess(command, 1, "masked\n", "")
            return real_run(command, **kwargs)

        with patch.dict(os.environ, self.env), patch.object(module.subprocess, "run", run):
            with self.assertRaises(ValueError) as caught:
                self.apply_replacement(module, units, fail_router)

        self.assertIs(caught.exception, original)
        self.assertIn("masked", "\n".join(getattr(original, "rollback_errors", ())))
        self.assertEqual(self.loaded_unit_hashes(), previous_hashes)
        self.assertEqual(self.active_units(), dict.fromkeys(UNIT_NAMES, True))
        for name, text in previous.items():
            self.assertEqual((units / name).read_text(), text)

    def test_initial_unit_write_failure_restores_prior_files(self):
        module, units, previous = self.prepare_units()
        previous_hashes = self.loaded_unit_hashes()
        original = PermissionError("cannot write new board unit")
        real_write = Path.write_text

        def write(path, text, *args, **kwargs):
            if path == units / UNIT_NAMES[1] and text == "new board unit\n":
                raise original
            return real_write(path, text, *args, **kwargs)

        with patch.dict(os.environ, self.env), patch.object(Path, "write_text", write):
            with self.assertRaises(PermissionError) as caught:
                self.apply_replacement(module, units, lambda _, **kwargs: None)

        self.assertIs(caught.exception, original)
        for name, text in previous.items():
            self.assertEqual((units / name).read_text(), text)
        self.assertEqual(self.loaded_unit_hashes(), previous_hashes)
        self.assertEqual(self.active_units(), dict.fromkeys(UNIT_NAMES, True))

    def test_restore_write_failure_is_attached_and_other_units_recover(self):
        module, units, previous = self.prepare_units()
        previous_hashes = self.loaded_unit_hashes()
        original = ValueError("original router readiness failure")
        real_write = Path.write_text
        attempts = []
        real_run = subprocess.run

        def write(path, text, *args, **kwargs):
            if path == units / UNIT_NAMES[1] and text == previous[UNIT_NAMES[1]]:
                raise PermissionError("cannot restore board unit")
            return real_write(path, text, *args, **kwargs)

        def run(command, **kwargs):
            attempts.append(command)
            return real_run(command, **kwargs)

        def fail_router(_, *, managed_restart=False):
            raise original

        with patch.dict(os.environ, self.env), patch.object(Path, "write_text", write):
            with patch.object(module.subprocess, "run", run):
                with self.assertRaises((OSError, ValueError)) as caught:
                    self.apply_replacement(module, units, fail_router)

        self.assertIs(caught.exception, original)
        self.assertIn("cannot restore board unit", "\n".join(getattr(original, "rollback_errors", ())))
        self.assertEqual((units / UNIT_NAMES[0]).read_text(), previous[UNIT_NAMES[0]])
        self.assertEqual(self.loaded_unit_hashes()[UNIT_NAMES[0]], previous_hashes[UNIT_NAMES[0]])
        self.assertEqual(self.active_units(), dict.fromkeys(UNIT_NAMES, True))
        for name in UNIT_NAMES:
            self.assertIn(["systemctl", "--user", "restart", name], attempts)


if __name__ == "__main__":
    unittest.main()
