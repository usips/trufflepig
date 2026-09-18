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

    def test_omp_session_environment_beats_claude_and_stale_marker(self):
        cwd = self.root / "repo dir"
        cwd.mkdir()
        self.marker(cwd).parent.mkdir(parents=True)
        self.marker(cwd).write_text("stale-session\n")
        self.env.update(OMPCODE="1", CLAUDECODE="1", TRUFFLEPIG_OMP_SESSION="omp-session-1")
        result = self.run_wrapper("search", "x", cwd=cwd)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.record()["session"], "omp-session-1")
        self.env["TRUFFLEPIG_SESSION"] = "explicit-env"
        self.run_wrapper("search", "x", cwd=cwd)
        self.assertEqual(self.record()["session"], "explicit-env")
        self.run_wrapper("search", "x", "--session", "explicit-cli", cwd=cwd)
        self.assertEqual(self.record()["session"], "explicit-cli")
        del self.env["TRUFFLEPIG_SESSION"]
        del self.env["TRUFFLEPIG_OMP_SESSION"]
        self.run_wrapper("search", "x", cwd=cwd)
        self.assertTrue(self.record()["session"].startswith("omp-"))

    def test_custom_agent_directory_and_profile(self):
        cases = [
            ({"PI_CODING_AGENT_DIR": str(self.root / "custom")}, self.root / "custom"),
            ({"PI_CONFIG_DIR": ".custom-omp"}, self.root / ".custom-omp/agent"),
            ({"OMP_PROFILE": "work", "PI_CODING_AGENT_DIR": "ignored"},
             self.root / ".omp/profiles/work/agent"),
            ({"PI_PROFILE": "legacy"}, self.root / ".omp/profiles/legacy/agent"),
            ({"OMP_PROFILE": "", "PI_PROFILE": "ignored"}, self.root / ".omp/agent"),
        ]
        original = self.env.copy()
        for values, expected in cases:
            with self.subTest(values=values):
                self.env = dict(original, **values)
                result = self.install()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((expected / "extensions/trufflepig-session.ts").resolve(),
                                 PLUGIN / "omp/session.ts")
                self.assertEqual((expected / "skills/trufflepig-code-search").resolve(),
                                 PLUGIN / "skills/trufflepig-code-search")
        explicit = self.root / "explicit path"
        result = subprocess.run([str(PLUGIN / "install.sh"), "--omp", "--omp-agent-dir", str(explicit)],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((explicit / "extensions/trufflepig-session.ts").exists())

    def test_omp_fallback_identity_without_marker(self):
        cwd = self.root / "empty dir"
        cwd.mkdir()
        self.env["OMPCODE"] = "1"
        self.run_wrapper("search", "x", cwd=cwd)
        session = self.record()["session"]
        self.assertTrue(session.startswith("omp-" + marker_key(cwd) + "-"), session)

    @unittest.skipUnless(shutil.which("bun"), "bun runs the TypeScript extension")
    def test_extension_attributes_cross_directory_calls_and_session_switch(self):
        repo = self.root / "repo"
        child = repo / "src"
        sibling = self.root / "sibling"
        child.mkdir(parents=True)
        sibling.mkdir()
        driver = self.root / "driver.ts"
        driver.write_text(f'''
import extension from {json.dumps(str(PLUGIN / "omp/session.ts"))};
const handlers: Record<string, any> = {{}};
extension({{ on: (event: string, handler: any) => {{ handlers[event] = handler; }} }} as any);
let id = "first-session";
const ctx = {{ cwd: {json.dumps(str(repo))}, sessionManager: {{ getSessionId: () => id }} }};
for (const cwd of {json.dumps([str(repo), str(child), str(sibling), str(repo)])}) {{
    const input = {{ command: "search", cwd, env: {{ KEEP: "yes" }} }};
    const result = await handlers.tool_call({{ toolName: "bash", input }}, ctx);
    if (result.input.env.KEEP !== "yes" || result.input.cwd !== cwd) throw Error("lost input");
    const run = Bun.spawnSync([{json.dumps(str(PLUGIN / "bin/trufflepig-agent"))}, "search", "x"],
        {{ cwd, env: {{ ...process.env, ...result.input.env, OMPCODE: "1" }} }});
    if (run.exitCode !== 0) throw Error(run.stderr.toString());
    id = "second-session";
}}
if (await handlers.tool_call({{ toolName: "read", input: {{}} }}, ctx) !== undefined)
    throw Error("changed non-shell tool");
''')
        result = subprocess.run(["bun", "run", str(driver)], env=self.env,
                                cwd=self.root, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        log = self.root / "state/trufflepig/agent-audit/omp.jsonl"
        rows = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual([row["session"] for row in rows],
                         ["first-session", "second-session", "second-session", "second-session"])


if __name__ == "__main__":
    unittest.main()
