"""Search steering: command classification and per-harness hook decisions."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

PLUGIN = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PLUGIN / "hooks"))
import trufflepig_checkout as checkout  # noqa: E402
import trufflepig_classify as classifier  # noqa: E402

# (command, expected class or None, fragment of the translated command)
# Drawn from real agent transcripts; `None` means the hook must stay silent.
CASES = [
    ('grep -rn "fn content_policy_required" crates/', "definition", "sym:content_policy_required file:crates/"),
    ('grep -n "pub struct SiteVerifyResponse" -B3 -A25 src/handlers.rs', "body", "sym:SiteVerifyResponse file:src/handlers.rs"),
    ('grep -n "fn route_table" -B3 -A12 src/http.rs', "body", "show HANDLE"),
    ('grep -rn "pub fn roster\\|pub(crate) fn roster" ledger/streams/', "definition", "sym:roster"),
    ("grep -rn 'fn step' crates/ 2>&1 | head -40", "definition", "sym:step"),
    ('git grep -n "write_integrity_atomic"', "references", "refs write_integrity_atomic"),
    ("git grep -n -E 'SearchQueue' -- '*.rs' | grep -vE 'use |//'", "references", "lang:rust"),
    ('grep -rln "item_destroyed\\|ItemDestroyed" --include=\'*.rs\' .', "references", "re:item_destroyed|ItemDestroyed lang:rust"),
    ('grep -rn "LUNATIC_THREADS" --include=*.rs .', "references", "refs LUNATIC_THREADS"),
    ("rg -n 'effect_door_command|door_command' crates --type rust", "references", "re:effect_door_command|door_command file:crates/ lang:rust"),
    ('grep -n "pub fn\\|pub async fn\\|fn \\|struct" src/fetcher.rs', "outline", "map src/fetcher.rs"),
    ('grep -n "^fn \\|#\\[test\\]" src/fetcher.rs', "outline", "map src/fetcher.rs"),
    ('grep -rn "fn parse\\|sha256\\|Bearer" crates/', "regex", "re:fn parse|sha256|Bearer"),
    ('grep -n "function logDecision" -A12 src/AbstractChecker.php', "body", "re:function logDecision file:src/AbstractChecker.php"),
    ("grep -rniw -e room -e rooms crates/", "concept", "room rooms file:crates/"),
    ("grep -rnF -e 'a.b' -e 'c(d' crates/", "regex", "re:a\\.b|c\\(d file:crates/"),
    ("find . -path ./target -prune -o -name '*navigation*' -print", "files", "navigation kind:file"),
    ('grep -rn "Ja4::new\\|HandshakeData\\|\\.ja4\\b" --include=*.rs src tests', "regex", "re:Ja4::new|HandshakeData"),
    ("rg -n --type rust '\\.route\\(|Router::new' crates", "regex", "file:crates/ lang:rust"),
    ("rg -n 'struct \\w+(Id|Key|Idx)\\b' crates --type rust", "regex", "re:struct \\w+(Id|Key|Idx)"),
    ('grep -rn "EquipmentSlot {\\|Vec<EquipmentSlot>" crates', "regex", "re:EquipmentSlot {|Vec<EquipmentSlot>"),
    ("grep -rln -i 'combust\\|hotspot\\|flame' crates/", "concept", "combust hotspot flame file:crates/"),
    ("find crates -name 'staff*.rs'", "files", "staff file:crates/ lang:rust kind:file"),
    ("rg --files -g '*.luau' crates", "files", "file:crates/ lang:luau kind:file"),
    ("cd sub && grep -rn 'fn step' .", "definition", "file:sub/"),
    # Never steered.
    ("cargo test 2>&1 | grep FAILED", None, ""),
    ("ls -la | grep -v total", None, ""),
    ("grep -n error /tmp/build.log", None, ""),
    ("grep -rn panic target/debug/", None, ""),
    ("grep -n TODO src/lib.rs", None, ""),
    ('grep -n "pub fn\\|spawn\\|InteractionRefused" src/lib.rs', None, ""),
    ('grep -n "immutable\\|proc/dismantle" code/turf.dm', None, ""),
    ('F=src/lib.rs; rg -n "^fn apply_row" $F', None, ""),
    ("for f in a b; do grep -rn x $f; done", None, ""),
    ("grep -q foo src/lib.rs && echo yes", None, ""),
    ("grep -c foo -r src/", None, ""),
    ("find . -name '*.rs' -newer Cargo.toml", None, ""),
    ("find . -type d -name target", None, ""),
    ("find . -name '*.orig' -delete", None, ""),
    ("git grep foo HEAD~3 -- src", None, ""),
    ("grep -rn 'fn step' /somewhere/else/", None, ""),
    ("python3 - <<'EOF'\nimport os; os.system('grep -rn x .')\nEOF", None, ""),
    ("grep -rn foo $(git ls-files)", None, ""),
    ("trufflepig-agent search 'sym:step'; grep -rn step .", None, ""),
    ("echo grep", None, ""),
]


class ClassifierTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="trufflepig-steer-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name) / "repo"
        for directory in ("crates", "src", "ledger/streams", "sub", "target/debug"):
            (self.root / directory).mkdir(parents=True)
        (self.root / "src/handlers.rs").write_text("")
        (self.root / "src/lib.rs").write_text("")
        self.cwd = os.getcwd()
        os.chdir(self.root)
        self.addCleanup(os.chdir, self.cwd)

    def test_transcript_commands(self):
        for command, kind, fragment in CASES:
            with self.subTest(command=command):
                found = classifier.classify(command, self.root, self.root)
                if kind is None:
                    self.assertEqual(found, [])
                    continue
                self.assertEqual([s.kind for s in found][:1], [kind])
                self.assertIn(fragment, found[0].hint)

    def test_regex_translation_keeps_filters_separate_from_pattern(self):
        found = classifier.classify("grep -rni 'spawn\\(entity' crates/", self.root, self.root)
        self.assertEqual(found[0].hint, "trufflepig-agent search 're:(?i)spawn(entity file:crates/'")

    def test_normalized_wildcards_do_not_become_names(self):
        self.assertEqual(classifier.definition_names(r"struct \w+Id"), [])
        self.assertEqual(classifier.definition_names(r"^\s*pub(crate)? fn\s+spawn_body"), ["spawn_body"])
        self.assertEqual(classifier.definition_names("impl.*Display for"), [])


class HookTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="trufflepig-steer-hook-")
        self.addCleanup(self.scratch.cleanup)
        base = Path(self.scratch.name)
        self.repo = base / "repo"
        (self.repo / "crates").mkdir(parents=True)
        (self.repo / ".git").mkdir()
        config = base / "config/trufflepig"
        config.mkdir(parents=True)
        (config / "space.toml").write_text(f'[workspace]\nname = "space"\n\n[members.lunatic]\npath = "{self.repo}"\n')
        (config / "workspaces.toml").write_text('workspaces = ["space.toml"]\n')
        self.state = base / "state"
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("TRUFFLEPIG", "CLAUDE"))}
        self.env.update(HOME=str(base), XDG_CONFIG_HOME=str(base / "config"), XDG_STATE_HOME=str(self.state))

    def run_hook(self, command, harness="claude", event="PreToolUse", cwd=None, mode=None, agent=""):
        payload = {"hook_event_name": event, "tool_name": "Bash", "tool_input": {"command": command},
                   "cwd": str(cwd or self.repo), "session_id": "s1", "agent_id": agent}
        env = dict(self.env, **({"TRUFFLEPIG_AGENT_STEER": mode} if mode else {}))
        return subprocess.run([sys.executable, str(PLUGIN / "hooks/steer-search.py"), harness],
                              input=json.dumps(payload), env=env, capture_output=True, text=True)

    def audit(self, harness="claude"):
        path = self.state / "trufflepig/agent-audit" / f"{harness}.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def write_call(self, signals, harness="claude"):
        path = self.state / "trufflepig/agent-audit" / f"{harness}.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a") as handle:
            handle.write(json.dumps({"ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "harness": harness,
                                     "cwd": str(self.repo), "verb": "search", "signals": signals}) + "\n")

    def test_claude_defaults_to_nudge_after_the_search_runs(self):
        pre = self.run_hook("grep -rn 'fn step' crates/")
        self.assertEqual((pre.returncode, pre.stdout), (0, ""))
        self.assertEqual(self.audit()[-1]["status"], "nudge")
        self.assertEqual(self.audit()[-1]["classes"], ["definition"])
        post = self.run_hook("grep -rn 'fn step' crates/", event="PostToolUse")
        output = json.loads(post.stdout)["hookSpecificOutput"]
        self.assertEqual(output["hookEventName"], "PostToolUse")
        self.assertIn("trufflepig-agent search 'sym:step file:crates/'", output["additionalContext"])
        # A repeated tip of the same class is rate limited per agent, not per session.
        self.assertEqual(self.run_hook("grep -rn 'fn walk' crates/", event="PostToolUse").stdout, "")
        self.assertNotEqual(self.run_hook("grep -rn 'fn walk' crates/", event="PostToolUse", agent="a2").stdout, "")
        self.assertEqual(len(self.audit()), 1, "PostToolUse must not log a second record")

    def test_strict_denies_strong_classes_with_the_equivalent_command(self):
        result = self.run_hook("grep -rn 'fn step' crates/", mode="strict")
        output = json.loads(result.stdout)["hookSpecificOutput"]
        self.assertEqual(output["permissionDecision"], "deny")
        self.assertIn("sym:step", output["permissionDecisionReason"])
        self.assertIn("tp-fallback", output["permissionDecisionReason"])
        # Weak classes are only nudged.
        self.assertEqual(self.run_hook("grep -rn 'a.*b' crates/", mode="strict").stdout, "")

    def test_strict_allows_explicit_and_evidence_based_fallback(self):
        marked = self.run_hook("grep -rn 'fn step' crates/  # tp-fallback: macro-generated", mode="strict")
        self.assertEqual(marked.stdout, "")
        self.assertEqual(self.audit()[-1]["fallback"], "explicit tp-fallback")
        self.write_call(["no_hits"])
        after_empty = self.run_hook("grep -rn 'fn walk' crates/", mode="strict")
        self.assertEqual(after_empty.stdout, "")
        self.assertEqual(self.audit()[-1]["fallback"], "trufflepig returned no hits")

    def test_legacy_block_for_other_harnesses_uses_exit_two(self):
        result = self.run_hook("grep -rn 'fn step' crates/", harness="kimi")
        self.assertEqual(result.returncode, 2)
        self.assertIn("allowed again after one trufflepig-agent call", result.stderr)
        recent = self.state / "trufflepig/agent-recent/kimi" / f"{checkout.cwd_key(str(self.repo))}-s1"
        recent.parent.mkdir(parents=True)
        recent.write_text("x")
        self.assertEqual(self.run_hook("grep -rn 'fn step' crates/", harness="kimi").returncode, 0)

    def test_pipe_filters_unindexed_directories_and_off_mode_are_silent(self):
        self.assertEqual(self.run_hook("cargo test 2>&1 | grep FAILED", mode="strict").stdout, "")
        outside = Path(self.scratch.name) / "elsewhere"
        outside.mkdir()
        self.assertEqual(self.run_hook("grep -rn 'fn step' .", cwd=outside, mode="strict").stdout, "")
        self.assertEqual(self.run_hook("grep -rn 'fn step' crates/", mode="off").stdout, "")
        self.assertEqual(self.audit(), [])

    def test_adoption_report_counts_escaped_searches_by_scope_and_class(self):
        self.run_hook("grep -rn 'fn step' crates/")
        self.run_hook("grep -rn 'LUNATIC_THREADS' crates/", agent="sub-1")
        self.run_hook("grep -rn 'fn walk' crates/", mode="strict")
        self.write_call([])
        result = subprocess.run([sys.executable, str(PLUGIN / "bin/trufflepig-audit"), "--adoption", "--json",
                                 str(self.state / "trufflepig/agent-audit")], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        rows = {row["scope"]: row for row in json.loads(result.stdout)["adoption"]}
        self.assertEqual(rows["main"]["classes"], {"definition": 1})
        self.assertEqual(rows["main"]["blocked"], 1)
        self.assertEqual(rows["subagent"]["classes"], {"references": 1})
        self.assertEqual((rows["total"]["trufflepig"], rows["total"]["escaped"], rows["total"]["adoption"]), (1, 2, 0.333))

    def test_nested_linked_worktree_is_its_own_checkout(self):
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True)
        subprocess.run(["git", "-C", str(self.repo), "-c", "user.name=t", "-c", "user.email=t@t",
                        "commit", "-q", "--allow-empty", "-m", "init"], check=True)
        worktree = self.repo / ".worktrees/feature"
        subprocess.run(["git", "-C", str(self.repo), "worktree", "add", "-q", str(worktree)], check=True)
        (worktree / "crates").mkdir()
        # Registry paths resolve through XDG_CONFIG_HOME, so probe in a child process.
        env =dict(os.environ, XDG_CONFIG_HOME=self.env["XDG_CONFIG_HOME"])
        probe = subprocess.run([sys.executable, "-c", (
            "import sys; sys.path.insert(0, sys.argv[1]); import trufflepig_checkout as k, trufflepig_classify as s; from pathlib import Path; "
            "c = k.indexed_checkout(Path(sys.argv[2])); print(c.checkout, c.member); "
            "print(s.classify(\"grep -rn 'fn step' crates/\", Path(sys.argv[2]).parent, c.checkout)[0].hint)"),
            str(PLUGIN / "hooks"), str(worktree / "crates")], env=env, capture_output=True, text=True)
        self.assertEqual(probe.returncode, 0, probe.stderr)
        checkout, hint = probe.stdout.splitlines()
        self.assertEqual(checkout, f"{worktree} lunatic")
        self.assertEqual(hint, "trufflepig-agent search 'sym:step file:crates/'")

    def test_runtime_config_selects_mode_per_harness(self):
        path = Path(self.env["XDG_CONFIG_HOME"]) / "trufflepig/agent-runtime.json"
        path.write_text(json.dumps({"steer": {"claude": "strict"}}))
        result = self.run_hook("grep -rn 'fn step' crates/")
        self.assertEqual(json.loads(result.stdout)["hookSpecificOutput"]["permissionDecision"], "deny")

    def test_session_context_only_inside_indexed_checkouts(self):
        env_file = Path(self.scratch.name) / "claude.env"
        payload = {"hook_event_name": "SessionStart", "session_id": "s1", "cwd": str(self.repo / "crates")}
        result = subprocess.run([sys.executable, str(PLUGIN / "hooks/claude-session.py")], input=json.dumps(payload),
                                env=dict(self.env, CLAUDE_ENV_FILE=str(env_file)), capture_output=True, text=True)
        context = json.loads(result.stdout)["hookSpecificOutput"]["additionalContext"]
        self.assertIn("lunatic", context)
        self.assertIn("trufflepig-agent search 'sym:Name'", context)
        payload["cwd"] = self.scratch.name
        result = subprocess.run([sys.executable, str(PLUGIN / "hooks/claude-session.py")], input=json.dumps(payload),
                                env=dict(self.env, CLAUDE_ENV_FILE=str(env_file)), capture_output=True, text=True)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
