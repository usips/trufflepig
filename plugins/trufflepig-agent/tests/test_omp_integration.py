"""omp installation, extension key contract, and per-cwd session attribution."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

PLUGIN = Path(__file__).resolve().parents[1]


class OmpIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="trufflepig-omp-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        # omp is detected from OMPCODE or a parent named omp; clear any
        # inherited omp/codex/claude markers so detection is deterministic.
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("TRUFFLEPIG", "CLAUDE", "CODEX", "KIMI", "MUSE",
                                         "AGENT_SESSION", "OMP"))}
        self.env.pop("OMPCODE", None)
        self.env.pop("PI_SESSION_FILE", None)
        self.env.update(HOME=str(self.root), XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_STATE_HOME=str(self.root / "state"), TMPDIR=str(self.root))
        self.fake = self.root / "fake cli"
        self.fake.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
Path(os.environ["CAPTURE"]).write_text(json.dumps({"argv": sys.argv[1:]}))
sys.stdout.buffer.write(os.environ.get("RESPONSE", '{"hits":[],"truncated":false}\\n').encode())
sys.exit(int(os.environ.get("EXIT", "0")))
''')
        self.fake.chmod(0o755)
        self.env.update(TRUFFLEPIG_BINARY=str(self.fake), CAPTURE=str(self.root / "capture.json"))

    def install(self):
        return subprocess.run([str(PLUGIN / "install.sh"), "--omp"],
                              env=self.env, text=True, capture_output=True)

    def omp_key(self, cwd: str) -> str:
        return hashlib.sha256(str(cwd).encode()).hexdigest()[:12]

    def seed_marker(self, cwd: str, session: str) -> Path:
        marker = self.root / "state/trufflepig/agent-sessions/omp" / self.omp_key(str(cwd))
        marker.parent.mkdir(parents=True, exist_ok=True)
        marker.write_text(session + "\n")
        return marker

    def record(self, harness="omp"):
        base = self.root / "state/trufflepig/agent-audit"
        return json.loads((base / f"{harness}.jsonl").read_text().splitlines()[-1])

    def run_wrapper(self, *args, cwd=None):
        return subprocess.run([str(PLUGIN / "bin/trufflepig-agent"), *args],
                              env=self.env, cwd=cwd or self.root, capture_output=True)

    def test_omp_key_is_sha256_truncated(self):
        self.assertEqual(self.omp_key("/home/kcrawley/projects/trufflepig"),
                         hashlib.sha256(b"/home/kcrawley/projects/trufflepig").hexdigest()[:12])
        # The key differs from the other harnesses' blake2s keys for the same cwd.
        self.assertNotEqual(self.omp_key("/x"),
                            hashlib.blake2s(b"/x", digest_size=6).hexdigest())

        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        # omp's config root is ~/.omp, independent of XDG_CONFIG_HOME.
        skill = self.root / ".omp/agent/skills/trufflepig-code-search"
        ext = self.root / ".omp/agent/extensions/trufflepig-session.ts"
        self.assertTrue(skill.is_symlink())
        self.assertEqual(skill.resolve(), PLUGIN / "skills/trufflepig-code-search")
        self.assertTrue(ext.is_symlink())
        self.assertEqual(ext.resolve(), PLUGIN / "omp/session.ts")
        # The skill did NOT land in the codex ~/.agents/skills location.
        self.assertFalse((self.root / ".agents/skills").exists())
        # Idempotent on a second run.
        again = self.install()
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertEqual(skill.resolve(), PLUGIN / "skills/trufflepig-code-search")

    def test_install_refuses_unmanaged_destination(self):
        unmanaged = self.root / ".omp/agent/skills/trufflepig-code-search"
        unmanaged.mkdir(parents=True)
        (unmanaged / "SKILL.md").write_text("not ours\n")
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unmanaged destination", result.stderr)
        # The pre-existing file was not replaced or removed.
        self.assertTrue((unmanaged / "SKILL.md").exists())

    def test_omp_env_detects_and_reads_per_cwd_marker(self):
        cwd = self.root / "repo dir"
        cwd.mkdir()
        self.seed_marker(cwd, "omp-session-1")
        self.env["OMPCODE"] = "1"
        result = self.run_wrapper("search", "x", cwd=cwd)
        self.assertEqual(result.returncode, 0, result.stderr)
        record = self.record("omp")
        self.assertEqual(record["harness"], "omp")
        self.assertEqual(record["session"], "omp-session-1")

    def test_omp_fallback_identity_without_marker(self):
        cwd = self.root / "empty dir"
        cwd.mkdir()
        self.env["OMPCODE"] = "1"
        self.run_wrapper("search", "x", cwd=cwd)
        record = self.record("omp")
        self.assertEqual(record["harness"], "omp")
        self.assertTrue(record["session"].startswith("omp-"))
        self.assertIn(self.omp_key(str(cwd)), record["session"])

    def test_cli_session_overrides_omp_marker(self):
        cwd = self.root / "repo dir"
        cwd.mkdir()
        self.seed_marker(cwd, "omp-session-1")
        self.env["OMPCODE"] = "1"
        self.run_wrapper("search", "x", "--session", "explicit-omp", cwd=cwd)
        self.assertEqual(self.record("omp")["session"], "explicit-omp")


if __name__ == "__main__":
    unittest.main()
