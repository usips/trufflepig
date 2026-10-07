"""Unmanaged router readiness distinguishes stale sockets from live endpoints."""
import socket
from unittest.mock import patch

from agent_install_case import AgentInstallCase


class AgentRouterEndpointTests(AgentInstallCase):
    def stale_endpoint(self):
        runtime = self.root / "r"
        runtime.mkdir()
        path = runtime / "daemon.sock"
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
            listener.bind(str(path))
            listener.listen()
        self.assertTrue(path.is_socket())
        return runtime

    def test_stale_socket_counts_as_absent(self):
        module = self.load_installer("install_stale_socket")
        runtime = self.stale_endpoint()
        with patch.object(module, "router_status", wraps=module.router_status) as probe:
            module.require_current_router(runtime, allow_absent=True, timeout=0.03)
        self.assertEqual(probe.call_count, 0, "an absent unmanaged router needs no readiness polls")
        self.assertFalse(module.router_endpoint_present(runtime))

    def test_managed_stale_socket_still_requires_a_listener(self):
        module = self.load_installer("install_managed_stale_socket")
        runtime = self.stale_endpoint()
        with self.assertRaisesRegex(ValueError, "did not become ready"):
            module.require_current_router(runtime, timeout=0.03)

    def test_non_socket_endpoint_remains_present(self):
        module = self.load_installer("install_non_socket_endpoint")
        runtime = self.root / "r"
        runtime.mkdir()
        (runtime / "daemon.sock").write_text("unmanaged file\n")
        self.assertTrue(module.router_endpoint_present(runtime))

    def test_live_endpoint_remains_present(self):
        module = self.load_installer("install_live_endpoint")
        path = self.serve_router_status([])
        self.assertTrue(module.router_endpoint_present(path.parent))
