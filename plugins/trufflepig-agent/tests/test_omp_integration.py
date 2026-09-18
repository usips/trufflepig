"""omp installation, the shared marker-key contract, and per-cwd attribution."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

PLUGIN = Path(__file__).resolve().parents[1]


def marker_key(cwd: Path) -> str:
    # Pinned independently of the wrapper: the extension must produce this too.
    return hashlib.sha256(str(cwd).encode()).hexdigest()[:12]


class OmpIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="trufflepig-omp-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("TRUFFLEPIG", "CLAUDE", "CODEX", "KIMI", "MUSE",
                                         "AGENT_SESSION", "OMP", "PI_"))}
        self.env.update(HOME=str(self.root), XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_STATE_HOME=str(self.root / "state"), TMPDIR=str(self.root))
        self.fake = self.root / "fake cli"
        self.fake.write_text('''#!/usr/bin/env python3
import os, sys
sys.stdout.write('{"hits":[],"truncated":false}\\n')
''')
        self.fake.chmod(0o755)
        self.env["TRUFFLEPIG_BINARY"] = str(self.fake)

    def install(self):
        return subprocess.run([str(PLUGIN / "install.sh"), "--omp"],
                              env=self.env, text=True, capture_output=True)

    def marker(self, cwd: Path) -> Path:
        return self.root / "state/trufflepig/agent-sessions/omp" / marker_key(cwd)

    def record(self) -> dict:
        log = self.root / "state/trufflepig/agent-audit/omp.jsonl"
        return json.loads(log.read_text().splitlines()[-1])

    def run_wrapper(self, *args, cwd: Path):
        return subprocess.run([str(PLUGIN / "bin/trufflepig-agent"), *args],
                              env=self.env, cwd=cwd, capture_output=True)

    def test_install_links_skill_and_extension_under_omp_home(self):
        # omp's config root is ~/.omp, independent of XDG_CONFIG_HOME.
        skill = self.root / ".omp/agent/skills/trufflepig-code-search"
        extension = self.root / ".omp/agent/extensions/trufflepig-session.ts"
        for _ in range(2):
            result = self.install()
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(skill.resolve(), PLUGIN / "skills/trufflepig-code-search")
        self.assertEqual(extension.resolve(), PLUGIN / "omp/session.ts")
        self.assertFalse((self.root / ".agents/skills").exists())

    def test_install_refuses_unmanaged_destination(self):
        unmanaged = self.root / ".omp/agent/skills/trufflepig-code-search"
        unmanaged.mkdir(parents=True)
        (unmanaged / "SKILL.md").write_text("not ours\n")
        result = self.install()
        self.assertEqual(result.returncode, 2)
        self.assertIn("unmanaged destination", result.stderr)
        self.assertEqual((unmanaged / "SKILL.md").read_text(), "not ours\n")
        self.assertFalse((self.root / ".omp/agent/extensions").exists())

    def test_omp_reads_marker_even_when_claudecode_is_exported(self):
        # omp's shell tool exports both OMPCODE=1 and CLAUDECODE=1.
        cwd = self.root / "repo dir"
        cwd.mkdir()
        self.marker(cwd).parent.mkdir(parents=True)
        self.marker(cwd).write_text("omp-session-1\n")
        self.env.update(OMPCODE="1", CLAUDECODE="1")
        result = self.run_wrapper("search", "x", cwd=cwd)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.record()["harness"], self.record()["session"]),
                         ("omp", "omp-session-1"))
        self.run_wrapper("search", "x", "--session", "explicit-omp", cwd=cwd)
        self.assertEqual(self.record()["session"], "explicit-omp")

    def test_omp_fallback_identity_without_marker(self):
        cwd = self.root / "empty dir"
        cwd.mkdir()
        self.env["OMPCODE"] = "1"
        self.run_wrapper("search", "x", cwd=cwd)
        session = self.record()["session"]
        self.assertTrue(session.startswith("omp-" + marker_key(cwd) + "-"), session)

    @unittest.skipUnless(shutil.which("bun"), "bun runs the TypeScript extension")
    def test_extension_writes_wrapper_readable_marker_on_start_and_switch(self):
        cwd = self.root / "repo dir"
        cwd.mkdir()
        driver = self.root / "driver.ts"
        driver.write_text(f'''
import extension from {json.dumps(str(PLUGIN / "omp/session.ts"))};
const handlers: Record<string, (event: unknown, ctx: unknown) => unknown> = {{}};
extension({{ on: (event: string, handler: any) => {{ handlers[event] = handler; }} }} as any);
let id = "omp-session-1";
const ctx = {{ cwd: {json.dumps(str(cwd))}, sessionManager: {{ getSessionId: () => id }} }};
await handlers.session_start({{ type: "session_start" }}, ctx);
console.log(require("node:fs").readFileSync(process.argv[2], "utf8"));
id = "omp-session-2";
await handlers.session_switch({{ type: "session_switch", reason: "new" }}, ctx);
''')
        result = subprocess.run(["bun", "run", str(driver), str(self.marker(cwd))],
                                env=self.env, cwd=self.root, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "omp-session-1\n\n")
        self.env["OMPCODE"] = "1"
        self.run_wrapper("search", "x", cwd=cwd)
        self.assertEqual(self.record()["session"], "omp-session-2")


if __name__ == "__main__":
    unittest.main()
