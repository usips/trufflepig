"""Actual-binary router probes with isolated storage and bounded process cleanup."""
import os
from pathlib import Path
import shutil
import signal
import subprocess
import time

from agent_install_case import AgentInstallCase


class AgentRealRouterCase(AgentInstallCase):
    def real_trufflepig_binary(self):
        override = os.environ.get("TRUFFLEPIG_TEST_BINARY")
        binary = override or shutil.which("trufflepig")
        self.assertIsNotNone(binary, "trufflepig is required; set TRUFFLEPIG_TEST_BINARY")
        path = Path(binary).resolve()
        self.assertTrue(path.is_file() and os.access(path, os.X_OK), f"not an executable: {path}")
        return str(path)

    def start_real_router(self, binary, module):
        self.env.update(XDG_CACHE_HOME=str(self.root / "cache"),
                        XDG_DATA_HOME=str(self.root / "data"),
                        TRUFFLEPIG_SPOOL_DIR=str(self.root / "spool"))
        log_path = self.root / "real-router.log"
        with log_path.open("w") as log:
            router = subprocess.Popen([binary, "system-serve"], env=self.env,
                                      stdout=log, stderr=subprocess.STDOUT, start_new_session=True)

        def stop_router():
            if router.poll() is None:
                os.killpg(router.pid, signal.SIGTERM)
                try:
                    router.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(router.pid, signal.SIGKILL)
                    router.wait(timeout=3)

        self.addCleanup(stop_router)
        runtime = Path(self.env["TRUFFLEPIG_SYSTEM_DIR"])
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and router.poll() is None:
            status = module.router_status(runtime, deadline)
            if status is not None:
                return status
            time.sleep(0.01)
        self.fail(f"actual router did not answer status: {log_path.read_text()[-2000:]}")
