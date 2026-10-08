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
import trufflepig_shell as shell  # noqa: E402

# (command, expected class or None, fragment of the translated command)
# Drawn from real agent transcripts; `None` means the hook must stay silent.
# `{root}` is the checkout's absolute path.
CASES = [
    ('grep -rn "fn content_policy_required" crates/', "definition", "sym:content_policy_required file:crates/"),
    ('grep -n "pub struct SiteVerifyResponse" -B3 -A25 src/handlers.rs', "body", "show 'sym:SiteVerifyResponse file:src/handlers.rs'"),
    ('grep -n "fn route_table" -B3 -A12 src/http.rs', "body", "trufflepig-agent show 'sym:route_table"),
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
    ('grep -n "function logDecision" -A12 src/AbstractChecker.php', "body", "show 'sym:logDecision file:src/AbstractChecker.php'"),
    ("grep -rniw -e room -e rooms crates/", "concept", "room rooms file:crates/"),
    ("grep -rnF -e 'a.b' -e 'c(d' crates/", "regex", "re:a\\.b|c\\(d file:crates/"),
    ("find . -path ./target -prune -o -name '*navigation*' -print", "files", "navigation kind:file"),
    ('grep -rn "Ja4::new\\|HandshakeData\\|\\.ja4\\b" --include=*.rs src tests', "regex", "re:Ja4::new|HandshakeData"),
    ("rg -n --type rust '\\.route\\(|Router::new' crates", "regex", "file:crates/ lang:rust"),
    ("rg -n 'struct \\w+(Id|Key|Idx)\\b' crates --type rust", "regex", "re:struct \\w+(Id|Key|Idx)"),
    ('grep -rn "EquipmentSlot {\\|Vec<EquipmentSlot>" crates', "regex", "re:EquipmentSlot {|Vec<EquipmentSlot>"),
    ("grep -rln -i 'combust\\|hotspot\\|flame' crates/", "concept", "combust hotspot flame file:crates/"),
    ("rg -n 'public record struct Wallet' -g '*.cs' src", "definition", "sym:Wallet file:src/ lang:csharp"),
    ("rg -n 'public record class Wallet' -g '*.cs' src", "definition", "sym:Wallet file:src/ lang:csharp"),
    ("rg -n 'namespace BTCPayServer' --glob '*.cs' src", "definition", "sym:BTCPayServer file:src/ lang:csharp"),
    ("rg -n 'public delegate void InvoiceChangedHandler' -g '*.cs' src", "definition", "sym:InvoiceChangedHandler file:src/ lang:csharp"),
    ("rg -n 'function createInvoice' -g '*.cjs' src", "definition", "sym:createInvoice file:src/ lang:js"),
    ("rg -n 'function createInvoice' -g '*.mjs' src", "definition", "sym:createInvoice file:src/ lang:js"),
    ("rg -n 'function createInvoice' -g '*.cts' src", "definition", "sym:createInvoice file:src/ lang:ts"),
    ("rg -n 'class RefreshDelegation' -g '*.php' src", "definition", "sym:RefreshDelegation file:src/ lang:php"),
    ("find crates -name 'staff*.rs'", "files", "staff file:crates/ lang:rust kind:file"),
    ("rg --files -g '*.luau' crates", "files", "file:crates/ lang:luau kind:file"),
    ("cd sub && grep -rn 'fn step' .", "definition", "file:sub/"),
    # Lunatic audit, 2026-09.
    ("grep -rn \"fn fixture_sim\" --include='*.rs' src", "definition", "'sym:fixture_sim file:src/ lang:rust'"),
    ("grep -rn 'fn fixture_sim\\|fn test_sim' --include='*.rs' src | head -2", "definition", "sym:fixture_sim"),
    ('grep -rn "pub struct ScriptStorage" -A30 script/ | head -50', "body", "show 'sym:ScriptStorage file:script/'"),
    ('grep -n "fn write_json" -A40 crates/xtask/src/dev.rs', "body", "show 'sym:write_json file:crates/xtask/src/dev.rs'"),
    ("sed -n 120,200p crates/lunatic-server/src/body.rs", "read",
     "trufflepig-agent show path:crates/lunatic-server/src/body.rs:120-200"),
    ("sed -n '90,110p' crates/lunatic-core/src/protocol.rs", "read", "show path:crates/lunatic-core/src/protocol.rs:90-110"),
    ("sed -n -e '18,30p;64,72p' src/lib.rs", "read", "show path:src/lib.rs:18-30"),
    ("sed -n 25p src/lib.rs", "read", "show path:src/lib.rs:25-25"),
    ("cat -n crates/cmd/commands_world.rs | sed -n 113,560p", "read", "show path:crates/cmd/commands_world.rs:113-560"),
    ("cat src/lib.rs", "read", "trufflepig-agent map src/lib.rs"),
    ("cat -n src/lib.rs src/handlers.rs", "read", "map src/lib.rs and trufflepig-agent map src/handlers.rs"),
    ("cd {root}/crates && sed -n 1,80p ../src/lib.rs", "read", "show path:src/lib.rs:1-80"),
    ("sed -n 25,50p src/lib.rs; trufflepig-agent -n 20 search 'sym:step'", None, ""),
    ("git -C {root} grep -n write_integrity_atomic", "references", "refs write_integrity_atomic"),
    ("git -C sub grep -n 'fn step'", "definition", "sym:step file:sub/"),
    ('git --no-pager grep -n "write_integrity_atomic"', "references", "refs write_integrity_atomic"),
    ("git -c color.ui=never grep -n -E 'SearchQueue' -- '*.rs'", "references", "lang:rust"),
    ("cd {root}/ledger && git grep -n write_integrity_atomic streams", "references", "file:ledger/streams/"),
    ("find . -name dispatch_order.rs", "files", "dispatch_order kind:file"),
    ("sed -n 1,40p src/lib.rs 2>/dev/null", "read", "show path:src/lib.rs:1-40"),
    ("cat src/lib.rs 2>&1", "read", "map src/lib.rs"),
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
    ("trufflepig-agent refs mark_carrier_panel 2>&1 | head -80", None, ""),
    ("sed -i 's/a/b/' src/lib.rs", None, ""),
    ("sed -n '/fn main/,/^}/p' src/lib.rs", None, ""),
    ("sed -n 1,20p docs/notes.md", None, ""),
    ("sed -n 1,20p target/debug/build.rs", None, ""),
    ("sed -n 1,20p src/lib.rs | grep fn", None, ""),
    ("cat src/lib.rs | sed -n 1,20p | grep fn", None, ""),
    ("git log -p | sed -n 1,40p", None, ""),
    ("cat Cargo.toml", None, ""),
    ("cat src/lib.rs | wc -l", None, ""),
    ("cat /tmp/build.log", None, ""),
    ("cat src/*.rs", None, ""),
    ("git -C /somewhere/else grep -n write_integrity_atomic", None, ""),
    ("git --git-dir=/x/.git grep -n write_integrity_atomic", None, ""),
    ("git --no-pager log --oneline | grep fix", None, ""),
    # Copies, not reads.
    ("cat src/lib.rs > /tmp/copy.rs", None, ""),
    ("sed -n 1,40p src/lib.rs > out.rs", None, ""),
    ("sed -n 1,40p src/lib.rs >> out.rs", None, ""),
    ("cat -n src/lib.rs | sed -n 1,5p > out.txt", None, ""),
    ("cat src/lib.rs &> out.txt", None, ""),
]


class ClassifierTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="trufflepig-steer-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name) / "repo"
        for directory in ("crates", "src", "ledger/streams", "sub", "target/debug", "docs"):
            (self.root / directory).mkdir(parents=True)
        (self.root / "src/handlers.rs").write_text("")
        (self.root / "src/lib.rs").write_text("")
        self.cwd = os.getcwd()
        os.chdir(self.root)
        self.addCleanup(os.chdir, self.cwd)

    def test_transcript_commands(self):
        for command, kind, fragment in CASES:
            command = command.replace("{root}", str(self.root))
            with self.subTest(command=command):
                found = classifier.classify(command, self.root, self.root)
                if kind is None:
                    self.assertEqual(found, [])
                    continue
                self.assertEqual([s.kind for s in found][:1], [kind])
                self.assertIn(fragment, found[0].hint)

    def test_search_directory_follows_cd_and_git_c(self):
        self.assertEqual(shell.search_directory("cd sub && grep -rn x .", self.root), self.root / "sub")
        self.assertEqual(shell.search_directory(f"git -C {self.root}/ledger --no-pager grep x", Path("/")),
                         self.root / "ledger")
        self.assertEqual(shell.search_directory("git --git-dir=/x grep x", self.root), self.root)
        self.assertEqual(shell.search_directory("grep -rn x . ; cd sub", self.root), self.root)
        self.assertEqual(shell.search_directory(f"echo prep && cd {self.root} && grep -rn x src", Path("/")),
                         self.root)
        self.assertEqual(shell.search_directory(f"git -C {self.root}/sub status && grep -rn x src", self.root),
                         self.root)

    def test_existence_checks_use_the_command_directory(self):
        # The hook process runs elsewhere; `streams` exists only under the `cd` target.
        self.addCleanup(os.chdir, os.getcwd())
        os.chdir("/")
        found = classifier.classify("cd ledger && git grep -n write_integrity_atomic streams", self.root, self.root)
        self.assertEqual(found[0].hint, "trufflepig-agent refs write_integrity_atomic  "
                                        "(or search 're:write_integrity_atomic file:ledger/streams/')")

    def test_regex_translation_keeps_filters_separate_from_pattern(self):
        found = classifier.classify("grep -rni 'spawn\\(entity' crates/", self.root, self.root)
        self.assertEqual(found[0].hint, "trufflepig-agent search 're:(?i)spawn(entity file:crates/'")

    def test_normalized_wildcards_do_not_become_names(self):
        self.assertEqual(classifier.definition_names(r"struct \w+Id"), [])
        self.assertEqual(classifier.definition_names(r"^\s*pub(crate)? fn\s+spawn_body"), ["spawn_body"])
        self.assertEqual(classifier.definition_names("impl.*Display for"), [])

    def test_csharp_language_aliases_are_canonicalized(self):
        for language in ("csharp", "cs", "c#"):
            with self.subTest(language=language):
                type_arg = "'c#'" if language == "c#" else language
                found = classifier.classify(f"rg -n 'class InvoiceService' --type {type_arg} src", self.root, self.root)
                self.assertEqual(found[0].hint, "trufflepig-agent search 'sym:InvoiceService file:src/ lang:csharp'")

    def test_php_extension_aliases_are_canonicalized(self):
        for suffix in ("php", "phtml"):
            with self.subTest(suffix=suffix):
                found = classifier.classify(
                    f"rg -n 'function refreshDelegation' -g '*.{suffix}' src", self.root, self.root
                )
                self.assertEqual(
                    found[0].hint,
                    "trufflepig-agent search 'sym:refreshDelegation file:src/ lang:php'",
                )


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

    def run_hook(self, command, harness="claude", event="PreToolUse", cwd=None, mode=None, agent="",
                 tool="Bash", tool_input=None):
        payload = {"hook_event_name": event, "tool_name": tool, "tool_input": tool_input or {"command": command},
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
        self.assertIn("is indexed by trufflepig", output["additionalContext"])
        # Every later search of the class gets a one-line tip; each agent starts with the full one.
        repeated = self.context(self.run_hook("grep -rn 'fn walk' crates/", event="PostToolUse"))
        self.assertEqual(repeated, "trufflepig: definition search `fn walk` -> "
                                   "trufflepig-agent search 'sym:walk file:crates/'")
        other = self.context(self.run_hook("grep -rn 'fn walk' crates/", event="PostToolUse", agent="a2"))
        self.assertIn("is indexed by trufflepig", other)
        self.assertEqual(len(self.audit()), 1, "PostToolUse must not log a second record")

    def test_hook_uses_configured_mode_and_environment_override(self):
        config = Path(self.env["XDG_CONFIG_HOME"]) / "trufflepig/agent-runtime.json"
        config.write_text(json.dumps({"steer": {"claude": "strict", "default": "off"}}))
        result = self.run_hook("grep -rn 'fn step' crates/")
        self.assertEqual(json.loads(result.stdout)["hookSpecificOutput"]["permissionDecision"], "deny")
        self.assertEqual(self.audit()[-1]["options"]["mode"], "strict")
        override = self.run_hook("grep -rn 'fn step' crates/", mode="NUDGE")
        self.assertEqual(override.stdout, "")
        self.assertEqual(self.audit()[-1]["options"]["mode"], "nudge")

    def context(self, result, event="PostToolUse"):
        output = json.loads(result.stdout)["hookSpecificOutput"]
        self.assertEqual(output["hookEventName"], event)
        return output["additionalContext"]

    def test_line_range_reads_are_nudged_toward_show(self):
        (self.repo / "crates/body.rs").write_text("")
        self.run_hook("sed -n 120,200p crates/body.rs")
        self.assertEqual(self.audit()[-1]["classes"], ["read"])
        tip = self.context(self.run_hook("sed -n 120,200p crates/body.rs", event="PostToolUse"))
        self.assertIn("- read `crates/body.rs:120-200` -> trufflepig-agent show path:crates/body.rs:120-200", tip)
        # Reads are nudged, never blocked.
        self.assertEqual(self.run_hook("sed -n 1,5p crates/body.rs", mode="strict").stdout, "")
        legacy = self.run_hook("cat crates/body.rs", harness="kimi")
        self.assertEqual(legacy.returncode, 0)
        self.assertIn("trufflepig-agent map crates/body.rs", legacy.stdout)

    def test_piped_and_chained_trufflepig_calls_get_a_tip(self):
        piped = self.context(self.run_hook("trufflepig-agent refs mark_carrier_panel 2>&1 | head -80",
                                           event="PostToolUse"))
        self.assertIn("drops its coverage and `next:` footer", piped)
        again = self.context(self.run_hook("trufflepig-agent --help | head -30", event="PostToolUse"))
        self.assertEqual(again, "trufflepig: run `trufflepig-agent` as its own Bash call; "
                                "pipes and chains hide its footer.")
        chained = self.context(self.run_hook("sed -n 25,50p crates/a.rs; trufflepig-agent search 'sym:step'",
                                             event="PostToolUse", agent="a2"))
        self.assertIn("without `;`, `&&`", chained)
        for quiet in ("trufflepig-agent refs step 2>&1", f"cd {self.repo} && trufflepig-agent search 'sym:step'"):
            self.assertEqual(self.run_hook(quiet, event="PostToolUse", agent="a3").stdout, "", quiet)
        # The tip is delivered once, after the call ran, and never logged as an escaped search.
        self.assertEqual(self.run_hook("trufflepig-agent refs x | head").stdout, "")
        self.assertEqual(self.audit(), [])
        self.assertEqual(self.run_hook("trufflepig-agent refs x | head", event="PostToolUse", mode="off").stdout, "")

    def test_agent_briefs_that_say_grep_get_lead_facing_context(self):
        brief = {"description": "find config", "subagent_type": "Explore",
                 "prompt": "Find where the web root is set. Just grep for sccache/RUSTC_WRAPPER in crates/."}
        result = self.run_hook("", tool="Agent", tool_input=brief)
        context = self.context(result, "PreToolUse")
        self.assertIn("says `Just grep for`", context)
        self.assertIn("lunatic is indexed by trufflepig", context)
        self.assertNotIn("permissionDecision", result.stdout)
        self.assertIn("ask subagents for", self.context(self.run_hook("", tool="Agent", tool_input=brief), "PreToolUse"))
        for prompt in ("Search with trufflepig-agent; never use grep for code.",
                       "Use trufflepig-agent instead of grep for definitions.",
                       "Summarize docs/plan.md."):
            quiet = self.run_hook("", tool="Agent", tool_input=dict(brief, prompt=prompt))
            self.assertEqual(quiet.stdout, "", prompt)
        outside = Path(self.scratch.name) / "elsewhere"
        outside.mkdir()
        self.assertEqual(self.run_hook("", tool="Agent", tool_input=brief, cwd=outside).stdout, "")
        self.assertEqual(self.run_hook("", tool="Agent", tool_input=brief, event="PostToolUse").stdout, "")

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
        self.run_hook("sed -n 1,40p crates/a.rs")
        self.write_call([])
        result = subprocess.run([sys.executable, str(PLUGIN / "bin/trufflepig-audit"), "--adoption", "--json",
                                 str(self.state / "trufflepig/agent-audit")], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        rows = {row["scope"]: row for row in json.loads(result.stdout)["adoption"]}
        self.assertEqual(rows["main"]["classes"], {"definition": 1, "read": 1})
        self.assertEqual(rows["main"]["blocked"], 1)
        self.assertEqual((rows["main"]["reads"], rows["total"]["reads"]), (1, 1))
        self.assertEqual(rows["subagent"]["classes"], {"references": 1})
        self.assertEqual((rows["total"]["trufflepig"], rows["total"]["escaped"], rows["total"]["adoption"]), (1, 2, 0.333))

    def test_suggestions_move_into_the_checkout_the_search_ran_in(self):
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True)
        subprocess.run(["git", "-C", str(self.repo), "-c", "user.name=t", "-c", "user.email=t@t",
                        "commit", "-q", "--allow-empty", "-m", "init"], check=True)
        worktree = Path(self.scratch.name) / "worktrees/lunatic-w4-COL"
        subprocess.run(["git", "-C", str(self.repo), "worktree", "add", "-q", str(worktree)], check=True)
        (worktree / "crates").mkdir()
        for command in (f"cd {worktree} && grep -rn 'fn tick' crates", f"git -C {worktree} grep -n 'fn tick' crates"):
            tip = self.context(self.run_hook(command, event="PostToolUse", agent=command))
            self.assertIn(f"-> cd {worktree} && trufflepig-agent search 'sym:tick file:crates/'", tip)
        # The same checkout needs no `cd`; a session outside any checkout does.
        same = self.context(self.run_hook("cd crates && grep -rn 'fn tick' .", event="PostToolUse", agent="s"))
        self.assertIn("-> trufflepig-agent search 'sym:tick file:crates/'", same)
        outside = Path(self.scratch.name) / "elsewhere"
        outside.mkdir()
        tip = self.context(self.run_hook(f"cd {self.repo} && grep -rn 'fn tick' crates", event="PostToolUse",
                                         cwd=outside))
        self.assertIn(f"-> cd {self.repo} && trufflepig-agent search", tip)

    def test_unrelated_commands_do_not_choose_the_search_checkout(self):
        outside = Path(self.scratch.name) / "elsewhere"
        outside.mkdir()
        tip = self.context(self.run_hook(f"echo prep && cd {self.repo} && grep -rn 'fn tick' crates",
                                         event="PostToolUse", cwd=outside, agent="echo-cd-search"))
        self.assertIn(f"-> cd {self.repo} && trufflepig-agent search 'sym:tick file:crates/'", tip)

    def test_malformed_payloads_never_fail_the_tool_call(self):
        for payload in ('{"cwd": 123, "tool_name": "Bash", "tool_input": {"command": "grep -rn x ."}}',
                        '{"tool_name": "Bash", "tool_input": {"command": ["grep", 5]}, "cwd": {}}', "[]", "{"):
            result = subprocess.run([sys.executable, str(PLUGIN / "hooks/steer-search.py"), "claude"],
                                    input=payload, env=self.env, capture_output=True, text=True)
            self.assertEqual((result.returncode, result.stdout), (0, ""), payload)
        self.assertIn("steering unavailable", subprocess.run(
            [sys.executable, str(PLUGIN / "hooks/steer-search.py"), "claude"],
            input='{"cwd": 123, "tool_name": "Bash", "tool_input": {"command": "grep -rn x ."}}',
            env=self.env, capture_output=True, text=True).stderr)

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

    def test_subagents_receive_search_context_at_start(self):
        payload = {"hook_event_name": "SubagentStart", "session_id": "abc123",
                   "cwd": str(self.repo / "crates"), "agent_id": "agent-abc123",
                   "agent_type": "Explore", "transcript_path": "/home/user/.claude/projects/project/abc123.jsonl"}
        env_file = Path(self.scratch.name) / "claude.env"
        env_file.write_text("export TRUFFLEPIG_CLAUDE_SESSION=abc123\n")
        result = subprocess.run([sys.executable, str(PLUGIN / "hooks/claude-session.py")], input=json.dumps(payload),
                                env=dict(self.env, CLAUDE_ENV_FILE=str(env_file)), capture_output=True, text=True)
        output = json.loads(result.stdout)["hookSpecificOutput"]
        self.assertEqual(output["hookEventName"], "SubagentStart")
        self.assertIn("TRUFFLEPIG_SESSION=claude-agent-", output["additionalContext"])
        self.assertIn("including `board hello`", output["additionalContext"])
        self.assertIn("trufflepig-agent show 'sym:Name'", output["additionalContext"])
        self.assertIn("Only an unpublished linked-worktree home in a workspace can serve from its member's parent index",
                      output["additionalContext"])
        self.assertNotIn("briefing subagents", output["additionalContext"])
        self.assertEqual(env_file.read_text(), "export TRUFFLEPIG_CLAUDE_SESSION=abc123\n",
                         "subagents must not rewrite the parent's session environment")
        payload["cwd"] = self.scratch.name
        result = subprocess.run([sys.executable, str(PLUGIN / "hooks/claude-session.py")], input=json.dumps(payload),
                                env=self.env, capture_output=True, text=True)
        unindexed = json.loads(result.stdout)["hookSpecificOutput"]["additionalContext"]
        self.assertIn("TRUFFLEPIG_SESSION=claude-agent-", unindexed)
        self.assertNotIn("trufflepig-agent show 'sym:Name'", unindexed)
        result = subprocess.run([sys.executable, str(PLUGIN / "hooks/claude-session.py")],
                                input=json.dumps(dict(payload, cwd=str(self.repo))),
                                env=dict(self.env, TRUFFLEPIG_AGENT_STEER="off"), capture_output=True, text=True)
        steered_off = json.loads(result.stdout)["hookSpecificOutput"]["additionalContext"]
        self.assertIn("TRUFFLEPIG_SESSION=claude-agent-", steered_off)
        self.assertNotIn("trufflepig-agent show 'sym:Name'", steered_off)

    def test_session_context_only_inside_indexed_checkouts(self):
        env_file = Path(self.scratch.name) / "claude.env"
        payload = {"hook_event_name": "SessionStart", "session_id": "s1", "cwd": str(self.repo / "crates")}
        result = subprocess.run([sys.executable, str(PLUGIN / "hooks/claude-session.py")], input=json.dumps(payload),
                                env=dict(self.env, CLAUDE_ENV_FILE=str(env_file)), capture_output=True, text=True)
        context = json.loads(result.stdout)["hookSpecificOutput"]["additionalContext"]
        self.assertIn("lunatic", context)
        self.assertIn("trufflepig-agent show 'sym:Name'", context)
        self.assertIn("show path:path/to/file.rs:120-200", context)
        self.assertIn("briefing subagents", context)
        payload["cwd"] = self.scratch.name
        result = subprocess.run([sys.executable, str(PLUGIN / "hooks/claude-session.py")], input=json.dumps(payload),
                                env=dict(self.env, CLAUDE_ENV_FILE=str(env_file)), capture_output=True, text=True)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
