"""Validate wave evidence through real Git commits and push hooks."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/check_wave_commits.py"
MESSAGE = (
    "test(board): check wave commit evidence\n\n"
    "Red-first: `test_rejects_missing_node_counts` fails on the parent.\n\n"
    "Suites: lib 1207/0/0 (full); board 312/0/0; Python 114 OK;\n"
    "assets node20 2/0, node21 2/0, node22 2/0\n\n"
    "Plan: P6\n"
    "Plan-Task: P6.1\n"
    "Co-authored-by: gpt-6.1-sol <noreply@openai.com>\n"
)


class WaveCommitCheckerTests(unittest.TestCase):
    def setUp(self):
        scratch = Path(os.environ.get("TMPDIR", Path.home() / ".cache/codex-tmp"))
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="wave-commits-", dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name) / "repo with spaces"
        self.script = SCRIPT
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith("GIT_")}
        self.env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
                        GIT_AUTHOR_NAME="Fixture", GIT_AUTHOR_EMAIL="fixture@example.invalid",
                        GIT_COMMITTER_NAME="Fixture", GIT_COMMITTER_EMAIL="fixture@example.invalid")
        subprocess.run(["git", "init", "--quiet", "--initial-branch=main", str(self.repo)],
                       env=self.env, check=True, capture_output=True, timeout=20)
        self.base = self.commit("fixture root")

    def git(self, *args, cwd=None, input=None):
        return subprocess.run(["git", *args], cwd=cwd or self.repo, env=self.env,
                              input=input, capture_output=True, text=True,
                              check=True, timeout=20).stdout.strip()

    def commit(self, message, cwd=None):
        self.git("-c", "core.hooksPath=/dev/null", "commit", "--quiet", "--allow-empty",
                 "--file=-", input=message, cwd=cwd)
        return self.git("rev-parse", "HEAD", cwd=cwd)

    def check(self, *args, cwd=None):
        return subprocess.run([sys.executable, str(self.script), *args], cwd=cwd or self.repo,
                              env=self.env, capture_output=True, text=True, timeout=20)

    def hook_path(self, cwd=None):
        return Path(self.git("rev-parse", "--path-format=absolute", "--git-path",
                             "hooks/pre-push", cwd=cwd))

    def remote(self):
        remote = Path(self.temporary.name) / "remote with spaces.git"
        self.git("init", "--quiet", "--bare", str(remote))
        self.git("remote", "add", "origin", str(remote))
        self.git("push", "--quiet", "origin", "main")
        return remote

    def run_hook(self, updates, remote, cwd=None):
        return subprocess.run([str(self.hook_path(cwd)), "origin", str(remote)],
                              cwd=cwd or self.repo, env=self.env, input=updates,
                              capture_output=True, text=True, timeout=20)

    def assert_accepts(self, message):
        parent = self.git("rev-parse", "HEAD")
        oid = self.commit(message)
        result = self.check(f"{parent}..{oid}")
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertEqual(result.stdout, "ok\n")

    def test_rejects_missing_node_counts(self):
        oid = self.commit(MESSAGE.replace("assets node20 2/0, node21 2/0, node22 2/0",
                                          "Node 20/21/22 2/2/2"))
        result = self.check(f"{self.base}..{oid}")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(f"{oid} Suites:", result.stdout)

    def test_accepts_template_commit(self):
        oid = self.commit(MESSAGE)
        result = self.check(f"{self.base}..{oid}")
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertEqual(result.stdout, "ok\n")

    def test_flags_split_trailers(self):
        oid = self.commit(MESSAGE.replace("Plan-Task: P6.1\n", "Plan-Task: P6.1\n\n"))
        result = self.check(f"{self.base}..{oid}")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(f"{oid} trailers:", result.stdout)

    def test_accepts_adjacent_red_first_and_suites(self):
        self.assert_accepts(MESSAGE.replace("fails on the parent.\n\nSuites:",
                                            "fails on the parent;\npasses here.\nSuites:"))

    def test_accepts_single_character_description(self):
        self.assert_accepts(MESSAGE.replace("test(board): check wave commit evidence", "fix(x): a"))

    def test_accepts_bare_test_names_and_future_model_ids(self):
        for model in ("GPT-6", "future-model-alpha"):
            with self.subTest(model=model):
                self.assert_accepts(MESSAGE.replace("`test_rejects_missing_node_counts`",
                                                    "board::rejects_missing_node_counts")
                                    .replace("gpt-6.1-sol", model))

    def test_accepts_explained_inapplicable_suites(self):
        self.assert_accepts(MESSAGE.replace(
            "Suites: lib 1207/0/0 (full); board 312/0/0; Python 114 OK;\n"
            "assets node20 2/0, node21 2/0, node22 2/0",
            "Suites: lib n/a (no Rust changes); board n/a (no board changes);\n"
            "Python n/a (documentation only); assets n/a (documentation only)"))

    def test_rejects_known_harness_names(self):
        for harness in ("Codex", "Claude Code", "Muse Code", "CODEX CLI", "human"):
            with self.subTest(harness=harness):
                parent = self.git("rev-parse", "HEAD")
                oid = self.commit(MESSAGE.replace("gpt-6.1-sol", harness))
                result = self.check(f"{parent}..{oid}")
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(f"{oid} Co-authored-by:", result.stdout)

    def test_rejects_incomplete_commit_evidence(self):
        cases = [
            ("subject", "test(board): check wave commit evidence", "invalid subject"),
            ("subject", "test(board): check wave commit evidence", "test(board): " + "x" * 40),
            ("subject", "test(board): check wave commit evidence", "fix(x): a."),
            ("body", "fails on the parent.", "x" * 73),
            ("Plan", "Plan: P6\n", ""),
            ("Plan-Task", "Plan-Task: P6.1\n", ""),
            ("Plan-Task", "Plan-Task: P6.1", "Plan-Task: P6.bad"),
            ("Plan-Task", "Plan: P6", "Plan: P7"),
            ("Co-authored-by", "Co-authored-by: gpt-6.1-sol <noreply@openai.com>\n", ""),
            ("Co-authored-by", "<noreply@openai.com>", "<not-an-email>"),
            ("Red-first", "Red-first:", "No-red-first:"),
            ("Red-first", "`test_rejects_missing_node_counts`", "E0432"),
            ("Suites", "lib 1207/0/0 (full)", "lib 1207/0/0"),
            ("Suites", "board 312/0/0; ", ""),
            ("Suites", "Python 114 OK", "Python n/a"),
            ("Suites", ", node22 2/0", ""),
            ("Suites", "node21 2/0", "node21 n/a ()"),
        ]
        for rule, before, after in cases:
            with self.subTest(rule=rule, after=after):
                parent = self.git("rev-parse", "HEAD")
                oid = self.commit(MESSAGE.replace(before, after))
                result = self.check(f"{parent}..{oid}")
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(f"{oid} {rule}:", result.stdout)

    def test_reports_each_commit_and_all_violations(self):
        first = self.commit(MESSAGE.replace("gpt-6.1-sol", "Codex"))
        second = self.commit(MESSAGE.replace("Plan: P6", "Plan: P7"))
        result = self.check(f"{self.base}..{second}")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(f"{first} Co-authored-by:", result.stdout)
        self.assertIn(f"{second} Plan-Task:", result.stdout)
        self.assertNotIn("ok\n", result.stdout)

    def test_invalid_revision_fails_closed(self):
        result = self.check("missing..HEAD")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("git:", result.stdout)

    def test_empty_range_succeeds(self):
        result = self.check(f"{self.base}..{self.base}")
        self.assertEqual((result.returncode, result.stdout), (0, "ok\n"))

    def test_hook_preserves_unrelated_or_symlinked_hooks(self):
        hook = self.hook_path()
        original = "#!/bin/sh\nexit 7\n"
        for symlink in (False, True):
            with self.subTest(symlink=symlink):
                target = hook.with_name("user-hook") if symlink else hook
                target.write_text(original)
                target.chmod(0o700)
                if symlink:
                    hook.symlink_to(target)
                result = self.check("--install-hook")
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("refusing to overwrite unrelated hook", result.stdout)
                self.assertEqual(target.read_text(), original)
                self.assertEqual(target.stat().st_mode & 0o777, 0o700)
                hook.unlink()

    def test_hook_new_branch_excludes_remote_history_and_quotes_paths(self):
        remote = self.remote()
        copied = Path(self.temporary.name) / "checker path 'quote'" / SCRIPT.name
        copied.parent.mkdir()
        copied.write_text(SCRIPT.read_text())
        self.script = copied
        result = self.check("--install-hook")
        self.assertEqual((result.returncode, result.stdout), (0, "ok\n"), result.stderr)
        self.assertEqual(self.check("--install-hook").returncode, 0)
        oid = self.commit(MESSAGE)
        pushed = subprocess.run(["git", "push", "origin", "HEAD:refs/heads/new-compliant"],
                                cwd=self.repo, env=self.env, capture_output=True,
                                text=True, timeout=20)
        self.assertEqual(pushed.returncode, 0, pushed.stderr + pushed.stdout)
        self.assertEqual(self.git("--git-dir", str(remote), "rev-parse",
                                  "refs/heads/new-compliant"), oid)

    def test_hook_checks_all_updates_and_skips_deletions(self):
        remote = self.remote()
        self.assertEqual(self.check("--install-hook").returncode, 0)
        good = self.commit(MESSAGE)
        update = f"refs/heads/main {good} refs/heads/main {self.base}\n"
        result = self.run_hook(update, remote)
        self.assertEqual((result.returncode, result.stdout), (0, "ok\n"), result.stderr)
        deletion = f"(delete) {'0' * 40} refs/heads/gone {self.base}\n"
        result = self.run_hook(deletion, remote)
        self.assertEqual((result.returncode, result.stdout), (0, "ok\n"), result.stderr)
        bad = self.commit(MESSAGE.replace("Plan-Task: P6.1\n", ""))
        result = self.run_hook(update + deletion +
                               f"refs/heads/bad {bad} refs/heads/bad {'0' * 40}\n", remote)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(f"{bad} Plan-Task:", result.stdout)

    def test_hook_uses_linked_worktree_git_directory(self):
        linked = Path(self.temporary.name) / "linked worktree"
        self.git("worktree", "add", "--quiet", "--detach", str(linked), self.base)
        result = self.check("--install-hook", cwd=linked)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertTrue((linked / ".git").is_file())
        self.assertEqual(self.hook_path(linked), self.hook_path())
        self.assertTrue(self.hook_path(linked).is_file())

    def test_hook_fails_closed_for_missing_revision_data(self):
        remote = self.remote()
        self.assertEqual(self.check("--install-hook").returncode, 0)
        oid = self.commit(MESSAGE)
        for updates, destination in [
            (f"main {oid} main {'1' * 40}\n", remote),
            (f"main {oid} main {'0' * 40}\n", remote.with_name("missing.git")),
            ("malformed update\n", remote),
        ]:
            with self.subTest(updates=updates, destination=destination):
                result = self.run_hook(updates, destination)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("hook git:", result.stdout)


if __name__ == "__main__":
    unittest.main()
