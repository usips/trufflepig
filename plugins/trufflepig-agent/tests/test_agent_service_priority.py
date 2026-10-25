"""Router/daemon CPU split: unit contract, slice install, legacy drop-ins, runtime weights."""
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

from agent_install_case import CURRENT_ROUTER_STATUS, PLUGIN, AgentInstallCase

LEGACY = ("# Deprioritize background agent/indexer daemons below the desktop.\n"
          "[Service]\nNice=10\nCPUWeight=10\n")
RESET = ["systemctl", "--user", "set-property", "--runtime", "trufflepig-system.service", "CPUWeight="]


class AgentServicePriorityTests(AgentInstallCase):
    def seed_layout(self):
        units = self.root / "config/systemd/user"
        for unit, extra in (("trufflepig-system.service", "override.conf"),
                            ("trufflepig-board.service", "192-168-lan.conf")):
            drop_ins = units / f"{unit}.d"
            drop_ins.mkdir(parents=True)
            (drop_ins / "10-nice.conf").write_text(LEGACY)
            (drop_ins / extra).write_text("[Service]\nEnvironment=KEEP=1\n")
        runtime = self.root / "run-user"
        control = runtime / "systemd/user.control/trufflepig-system.service.d"
        control.mkdir(parents=True)
        (control / "50-CPUWeight.conf").write_text(
            "# This is a drop-in unit file extension, created via \"systemctl set-property\"\n"
            "[Service]\nCPUWeight=10\n")
        self.env["XDG_RUNTIME_DIR"] = str(runtime)
        return units

    def calls(self, capture):
        return [json.loads(line) for line in capture.read_text().splitlines()]

    def test_router_unit_keeps_default_weight_and_restarts_on_exit_75(self):
        module = self.load_installer("install_priority_unit")
        canned = subprocess.CompletedProcess(args=[], returncode=0, stdout="/system-runtime\n", stderr="")
        with patch.object(module.subprocess, "run", return_value=canned):
            text = module.service_text("/bin/trufflepig", None)
        for line in ("Environment=TRUFFLEPIG_ROUTER_SUPERVISOR=systemd",
                     "Environment=TRUFFLEPIG_DAEMON_SLICE=app-trufflepig.slice",
                     "Restart=on-failure", "RestartForceExitStatus=75", "KillMode=control-group"):
            self.assertIn(line, text.splitlines())
        for key in ("Nice=", "CPUWeight=", "Delegate="):
            self.assertFalse(any(line.startswith(key) for line in text.splitlines()), key)
        slice_text = (PLUGIN / "systemd/app-trufflepig.slice").read_text()
        self.assertIn("[Slice]\nCPUWeight=10\nIOWeight=10\n", slice_text)

    def test_router_install_writes_slice_and_clears_legacy_priority(self):
        capture = self.service_shims()
        units = self.seed_layout()
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        result = self.install("--systemd")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((units / "app-trufflepig.slice").read_text(),
                         (PLUGIN / "systemd/app-trufflepig.slice").read_text())
        for unit, extra in (("trufflepig-system.service", "override.conf"),
                            ("trufflepig-board.service", "192-168-lan.conf")):
            self.assertFalse((units / f"{unit}.d/10-nice.conf").exists(), unit)
            self.assertTrue((units / f"{unit}.d" / extra).is_file(), extra)
        self.assertIn("removed legacy priority drop-in", result.stdout)
        calls = self.calls(capture)
        reload = calls.index(["systemctl", "--user", "daemon-reload"])
        reset = calls.index(RESET)
        stop = calls.index(["systemctl", "--user", "stop", "trufflepig-system.service"])
        self.assertLess(reload, reset)
        self.assertLess(reset, stop)
        self.assertNotIn(RESET[:-2] + ["trufflepig-board.service", "CPUWeight="], calls,
                         "only overridden units are reset")

    def test_foreign_drop_in_with_the_legacy_name_is_kept(self):
        self.service_shims()
        units = self.seed_layout()
        custom = units / "trufflepig-system.service.d/10-nice.conf"
        custom.write_text("[Service]\nNice=10\nEnvironment=MINE=1\n")
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        result = self.install("--systemd")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(custom.read_text(), "[Service]\nNice=10\nEnvironment=MINE=1\n")
        self.assertIn("sets more than priority", result.stderr)
        self.assertFalse((units / "trufflepig-board.service.d/10-nice.conf").exists())

    def test_failed_activation_restores_drop_ins_slice_and_runtime_weight(self):
        capture = self.service_shims()
        units = self.seed_layout()
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        self.env["FAIL_ENABLE"] = "1"
        result = self.install("--systemd")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse((units / "app-trufflepig.slice").exists())
        for unit in ("trufflepig-system.service", "trufflepig-board.service"):
            self.assertEqual((units / f"{unit}.d/10-nice.conf").read_text(), LEGACY)
        calls = self.calls(capture)
        restored = RESET[:-1] + ["CPUWeight=10"]
        self.assertIn(restored, calls)
        self.assertLess(calls.index(RESET), calls.index(restored))


if __name__ == "__main__":
    unittest.main()
