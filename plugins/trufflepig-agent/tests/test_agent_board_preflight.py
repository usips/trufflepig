"""Board service preflight contracts: database and runtime validation."""
import json
import unittest

from agent_install_case import CURRENT_ROUTER_STATUS, AgentInstallCase


class AgentBoardPreflightTests(AgentInstallCase):
    def test_service_preflight_rejects_relative_board_path_before_mutation(self):
        capture = self.service_shims()
        self.env["TRUFFLEPIG_BOARD_DB"] = "relative.sqlite3"
        for flag in ("--board", "--systemd"):
            with self.subTest(flag=flag):
                result = self.install(flag)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("TRUFFLEPIG_BOARD_DB must be absolute", result.stderr)
                self.assertFalse((self.root / ".local/bin").exists())
                self.assertFalse((self.root / "config/systemd").exists())
                self.assertFalse(capture.exists())

    def test_later_board_install_rejects_different_router_database_before_mutation(self):
        capture = self.service_shims()
        runtime = self.root / "router-runtime"
        pinned = self.root / "custom data/100%/board.sqlite3"
        self.env.update(TRUFFLEPIG_SYSTEM_DIR=str(runtime), TRUFFLEPIG_BOARD_DB=str(pinned),
                        XDG_DATA_HOME=str(self.root / "default-data"))
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        first = self.install("--systemd")
        self.assertEqual(first.returncode, 0, first.stderr)
        unit = self.root / "config/systemd/user/trufflepig-system.service"
        previous = unit.read_text()
        capture.write_text("")
        for marker in (False, True):
            if marker:
                runtime.mkdir(mode=0o700, exist_ok=True)
                (runtime / "board-backend.json").write_text(json.dumps({"database": str(pinned)}))
            for candidate in (None, str(self.root / "other/board.sqlite3")):
                if candidate is None:
                    self.env.pop("TRUFFLEPIG_BOARD_DB", None)
                else:
                    self.env["TRUFFLEPIG_BOARD_DB"] = candidate
                with self.subTest(marker=marker, candidate=candidate):
                    result = self.install("--board")
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("board database mismatch", result.stderr)
                    self.assertIn(f"set TRUFFLEPIG_BOARD_DB={pinned}", result.stderr)
                    self.assertEqual(unit.read_text(), previous)
                    self.assertFalse(unit.with_name("trufflepig-board.service").exists())
                    self.assertEqual(capture.read_text(), "")

    def test_board_service_pin_accepts_canonical_database_alias(self):
        self.service_shims()
        pinned = self.root / "existing.sqlite3"
        pinned.touch()
        alias = self.root / "alias.sqlite3"
        alias.symlink_to(pinned)
        runtime = self.root / "runtime"
        runtime.mkdir()
        (runtime / "board-backend.json").write_text(json.dumps({"database": str(pinned)}))
        self.env.update(TRUFFLEPIG_SYSTEM_DIR=str(runtime), TRUFFLEPIG_BOARD_DB=str(alias))
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        result = self.install("--board")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_later_board_install_rejects_changed_router_runtime(self):
        capture = self.service_shims()
        runtime = self.root / "custom-runtime"
        self.env["TRUFFLEPIG_SYSTEM_DIR"] = str(runtime)
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        first = self.install("--systemd")
        self.assertEqual(first.returncode, 0, first.stderr)
        capture.write_text("")
        del self.env["TRUFFLEPIG_SYSTEM_DIR"]
        self.env["XDG_RUNTIME_DIR"] = str(self.root / "different-runtime")
        result = self.install("--board")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(f"set TRUFFLEPIG_SYSTEM_DIR={runtime}", result.stderr)
        self.assertFalse((self.root / "config/systemd/user/trufflepig-board.service").exists())
        self.assertEqual(capture.read_text(), "")

    def test_router_unit_unsets_apply_after_later_environment_assignments(self):
        self.service_shims()
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        unit = self.root / "config/systemd/user/trufflepig-system.service"
        unit.parent.mkdir(parents=True)
        unit.write_text('[Service]\nUnsetEnvironment = TRUFFLEPIG_BOARD_DB\n'
                        'Environment = "TRUFFLEPIG_BOARD_DB=/different.sqlite3"\n')
        result = self.install("--board")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_router_drop_in_whitespace_cannot_hide_database_or_environment_file(self):
        capture = self.service_shims()
        self.serve_router_status([CURRENT_ROUTER_STATUS])
        first = self.install("--systemd")
        self.assertEqual(first.returncode, 0, first.stderr)
        capture.write_text("")
        drop_in = self.root / "config/systemd/user/trufflepig-system.service.d/override.conf"
        drop_in.parent.mkdir()
        for directive, message in [('Environment = "TRUFFLEPIG_BOARD_DB=/different.sqlite3"', 'set TRUFFLEPIG_BOARD_DB=/different.sqlite3'),
                                   ('EnvironmentFile = /unknown.env', 'cannot validate board service EnvironmentFile')]:
            drop_in.write_text('[Service]\n' + directive + '\n')
            result = self.install("--board")
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn(message, result.stderr)
            self.assertFalse((self.root / "config/systemd/user/trufflepig-board.service").exists())
            self.assertEqual(capture.read_text(), "")


if __name__ == "__main__":
    unittest.main()
