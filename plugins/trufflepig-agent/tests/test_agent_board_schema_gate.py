"""Router-only installation permits a board that has no database yet."""
from pathlib import Path
import unittest

from agent_install_case import CURRENT_ROUTER_STATUS, AgentInstallCase


class AgentBoardSchemaGateTests(AgentInstallCase):
    def test_router_only_install_tolerates_board_error(self):
        self.service_shims()
        status = dict(CURRENT_ROUTER_STATUS)
        status.pop("schema_file")
        self.serve_router_status(lambda: status)
        failures = (
            ("broken board.toml", "invalid_options: invalid board.toml"),
            ("remote mode", "remote board backend is unavailable"),
            ("refused parent", "cannot create board database parent"),
            ("slow writer", "board writer is still starting"),
        )
        for name, error in failures:
            with self.subTest(case=name):
                status["board_error"] = error
                database = Path(self.env["TRUFFLEPIG_BOARD_DB"])
                self.assertFalse(database.exists())
                result = self.run_installer_main("--systemd", router_timeout=0.03)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"warning: router board_error: {error}", result.stderr)
                self.assertTrue(self.active_units()["trufflepig-system.service"])
                self.assertFalse(database.exists())
                self.assertFalse((self.root / "config/systemd/user/trufflepig-board.service").exists())

    def test_router_only_without_database_requires_only_the_board_api(self):
        self.service_shims()
        self.serve_router_status([{"board_api": CURRENT_ROUTER_STATUS["board_api"]}])
        result = self.run_installer_main("--systemd", router_timeout=0.03)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_board_install_and_existing_database_require_current_schema(self):
        self.service_shims()
        status = dict(CURRENT_ROUTER_STATUS)
        self.serve_router_status(lambda: status)
        database = Path(self.env["TRUFFLEPIG_BOARD_DB"])
        bad_schemas = (
            {"schema_file": None},
            {"schema_file": CURRENT_ROUTER_STATUS["schema_supported"] - 1},
            {"schema_file": CURRENT_ROUTER_STATUS["schema_supported"] + 1},
            {"schema_supported": CURRENT_ROUTER_STATUS["schema_supported"] + 1},
        )
        for existing in (False, True):
            if existing:
                database.parent.mkdir(parents=True)
                database.touch()
            for change in bad_schemas:
                for args in (("--systemd", "--board"), ("--board",), ("--systemd",)):
                    if args == ("--systemd",) and not existing:
                        continue
                    with self.subTest(existing=existing, args=args, status=change):
                        status.clear()
                        status.update(CURRENT_ROUTER_STATUS, **change)
                        result = self.run_installer_main(*args, router_timeout=0.03)
                        self.assertEqual(result.returncode, 2, result.stderr)
                        self.assertIn("schema", result.stderr)
                        self.assertFalse((self.root / "config/systemd/user/trufflepig-board.service").exists())

    def test_router_only_install_still_rejects_the_wrong_board_api(self):
        self.service_shims()
        self.serve_router_status([{"board_api": CURRENT_ROUTER_STATUS["board_api"] - 1,
                                   "board_error": "board is unavailable"}])
        result = self.run_installer_main("--systemd", router_timeout=0.03)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("reports board_api", result.stderr)
        self.assertIn("rerun plugins/trufflepig-agent/install.sh --systemd", result.stderr)


if __name__ == "__main__":
    unittest.main()
