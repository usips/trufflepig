"""Router executable and listener identity checks."""
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from agent_install_case import CURRENT_ROUTER_STATUS, AgentInstallCase, installer
from router_identity import RouterIdentityConflict, require_router_exclusive

require_router_identity = installer.require_router_identity


class RouterIdentityTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="router-identity-")
        self.addCleanup(self.scratch.cleanup)
        self.proc = Path(self.scratch.name) / "proc"
        self.proc.mkdir()
        self.runtime = Path("/run/test-trufflepig")
        self.advice = "restart the test router"
        (self.proc / "net").mkdir()
        (self.proc / "net/unix").write_text(
            "Num RefCount Protocol Flags Type St Inode Path\n", encoding="utf-8")

    def status(self):
        return {"process_id": 41, "executable": "/opt/bin/trufflepig",
                "executable_replaced": False}

    def add_listener(self, inode="1234", runtime=None, *, flags="00010000", kind="0001"):
        endpoint = (runtime or self.runtime) / "daemon.sock"
        with (self.proc / "net/unix").open("a", encoding="utf-8") as stream:
            stream.write(f"000: 00000002 00000000 {flags} {kind} 01 {inode} {endpoint}\n")

    def add_process(self, pid, inode, executable="/opt/bin/trufflepig", argv=None, uid=None):
        process = self.proc / str(pid)
        (process / "fd").mkdir(parents=True)
        uid = os.getuid() if uid is None else uid
        (process / "status").write_text(f"Name:\ttest\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n", encoding="ascii")
        argv = ["trufflepig", "system-serve"] if argv is None else argv
        (process / "cmdline").write_bytes(b"\0".join(argument.encode() for argument in argv))
        (process / "fd/3").symlink_to(f"socket:[{inode}]")
        (process / "exe").symlink_to(executable)

    def test_status_replacement_error_names_identity_runtime_and_advice(self):
        status = {**self.status(), "executable_replaced": True}
        with self.assertRaises(ValueError) as caught:
            require_router_identity(self.runtime, status, self.advice, proc_root=self.proc)
        message = str(caught.exception)
        for detail in ("41", "/opt/bin/trufflepig", str(self.runtime), self.advice):
            self.assertIn(detail, message)

    def test_status_deleted_suffix_is_rejected_even_without_proc_visibility(self):
        status = {**self.status(), "executable": "/opt/bin/trufflepig (deleted)"}
        missing_proc = self.proc / "missing"
        with self.assertRaisesRegex(ValueError, "replaced executable") as caught:
            require_router_identity(self.runtime, status, self.advice, proc_root=missing_proc)
        self.assertIn(str(self.runtime), str(caught.exception))

    def test_current_listener_identity_is_accepted(self):
        self.add_listener()
        self.add_process(41, "1234")
        require_router_identity(self.runtime, self.status(), self.advice, proc_root=self.proc)

    def test_same_process_owning_two_listener_inodes_is_deduplicated(self):
        self.add_listener(inode="1234")
        self.add_listener(inode="5678")
        self.add_process(41, "1234")
        (self.proc / "41/fd/4").symlink_to("socket:[5678]")
        require_router_identity(self.runtime, self.status(), self.advice, proc_root=self.proc)

    def test_duplicate_processes_owning_same_listener_are_rejected(self):
        self.add_listener(inode="1234")
        self.add_listener(inode="5678")
        self.add_process(41, "1234", "/opt/bin/one")
        self.add_process(42, "5678", "/opt/bin/two")
        with self.assertRaises(RouterIdentityConflict) as caught:
            require_router_identity(self.runtime, self.status(), self.advice, proc_root=self.proc)
        message = str(caught.exception)
        for detail in ("multiple router processes", "41", "42", "/opt/bin/one",
                       "/opt/bin/two", str(self.runtime), "systemctl --user stop "
                       "trufflepig-system.service", "kill -- 41 42", self.advice):
            self.assertIn(detail, message)

    def test_exclusive_preflight_returns_single_deleted_owner_for_repair(self):
        self.add_listener()
        self.add_process(41, "1234", "/opt/bin/trufflepig (deleted)")
        self.assertEqual(
            require_router_exclusive(self.runtime, self.advice, proc_root=self.proc),
            {41: "/opt/bin/trufflepig (deleted)"},
        )

    def test_single_deleted_listener_owner_is_rejected(self):
        self.add_listener()
        self.add_process(41, "1234", "/opt/bin/trufflepig (deleted)")
        with self.assertRaisesRegex(ValueError, "replaced executable") as caught:
            require_router_identity(self.runtime, self.status(), self.advice, proc_root=self.proc)
        self.assertIn("41", str(caught.exception))

    def test_other_runtime_listener_does_not_match(self):
        other = Path("/run/another-runtime")
        self.add_listener(runtime=other)
        self.add_process(41, "1234", "/opt/bin/trufflepig (deleted)")
        require_router_identity(self.runtime, self.status(), self.advice, proc_root=self.proc)

    def test_different_uid_process_does_not_match_listener(self):
        self.add_listener()
        self.add_process(42, "1234", "/opt/bin/trufflepig (deleted)", uid=os.getuid() + 1)
        require_router_identity(self.runtime, self.status(), self.advice, proc_root=self.proc)

    def test_runtime_path_with_spaces_and_non_ascii_matches_listener(self):
        runtime = Path("/run/test café router")
        self.add_listener(runtime=runtime)
        self.add_process(41, "1234", "/opt/bin/trufflepig (deleted)")
        with self.assertRaisesRegex(ValueError, "replaced executable") as caught:
            require_router_identity(runtime, self.status(), self.advice, proc_root=self.proc)
        self.assertIn(str(runtime), str(caught.exception))

    def test_accepted_socket_and_nonlistener_rows_do_not_match(self):
        self.add_listener(flags="00000000")
        self.add_listener(inode="5678", flags="00010000", kind="0002")
        self.add_process(41, "1234")
        self.add_process(42, "5678")
        require_router_identity(self.runtime, self.status(), self.advice, proc_root=self.proc)

    def test_shell_text_containing_system_serve_is_not_an_exact_argv_word(self):
        self.add_listener()
        self.add_process(41, "1234", "/opt/bin/trufflepig (deleted)",
                         argv=["sh", "-c", "echo system-serve"])
        require_router_identity(self.runtime, self.status(), self.advice, proc_root=self.proc)

    def test_missing_proc_is_a_best_effort_status_only_check(self):
        require_router_identity(self.runtime, self.status(), self.advice,
                                proc_root=self.proc / "unavailable")


class RouterReadinessIdentityTests(AgentInstallCase):
    def test_ready_router_identity_is_checked_once_at_each_success_return(self):
        module = self.load_installer("install_router_identity_ready")
        runtime = Path(self.env["TRUFFLEPIG_SYSTEM_DIR"])
        for require_schema in (False, True):
            with self.subTest(require_schema=require_schema):
                with patch.object(module, "router_endpoint_present", return_value=True), \
                        patch.object(module, "router_status", return_value=CURRENT_ROUTER_STATUS), \
                        patch.object(module, "require_router_identity") as identity:
                    module.require_current_router(runtime, require_schema=require_schema)
                identity.assert_called_once_with(
                    runtime, CURRENT_ROUTER_STATUS,
                    "restart trufflepig-system.service with the current trufflepig binary")

    def test_stale_identity_cannot_return_ready(self):
        module = self.load_installer("install_router_identity_stale")
        runtime = Path(self.env["TRUFFLEPIG_SYSTEM_DIR"])
        status = {**CURRENT_ROUTER_STATUS, "process_id": 99,
                  "executable": "/opt/bin/trufflepig (deleted)",
                  "executable_replaced": True}
        with patch.object(module, "router_endpoint_present", return_value=True), \
                patch.object(module, "router_status", return_value=status):
            with self.assertRaisesRegex(ValueError, "replaced executable") as caught:
                module.require_current_router(runtime)
        for detail in ("99", "/opt/bin/trufflepig (deleted)", str(runtime),
                       "restart trufflepig-system.service"):
            self.assertIn(detail, str(caught.exception))


if __name__ == "__main__":
    unittest.main()
