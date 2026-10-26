"""Focused contracts for Claude identity and lifecycle hooks."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest import mock

PLUGIN = Path(__file__).resolve().parents[1]
HOOK = PLUGIN / "hooks/claude-hooks.py"
SESSION_HOOK = PLUGIN / "hooks/claude-session.py"


def isolated_environment(test):
    """The process environment without harness markers, with HOME and XDG pointed at a
    scratch directory so hooks never read or write the real user state."""
    scratch = tempfile.TemporaryDirectory(prefix="trufflepig-claude-hooks-")
    test.addCleanup(scratch.cleanup)
    base = Path(scratch.name)
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("TRUFFLEPIG", "CLAUDE", "CODEX", "KIMI", "MUSE", "OPENCODE", "XDG_"))}
    env.update(HOME=str(base), XDG_CONFIG_HOME=str(base / "config"), XDG_STATE_HOME=str(base / "state"),
               XDG_CACHE_HOME=str(base / "cache"), TMPDIR=str(base))
    return env


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ClaudeHookIdentityTests(unittest.TestCase):
    def setUp(self):
        self.env = isolated_environment(self)

    def run_hook(self, payload, env=None):
        return subprocess.run([str(HOOK)], input=json.dumps(payload), env=self.env if env is None else env,
                              text=True, capture_output=True, check=False)

    def test_rewrite_preserves_tool_input_and_leaves_permission_evaluation_open(self):
        payload = {
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "session_id": "session-1",
            "agent_id": "agent-a",
            "tool_input": {
                "command": "cd 'repo with spaces' && trufflepig-agent board show --json",
                "timeout": 12,
                "description": "Show the board",
            },
        }
        result = self.run_hook(payload)
        self.assertEqual(result.returncode, 0, result.stderr)
        output = json.loads(result.stdout)["hookSpecificOutput"]
        self.assertEqual(output["hookEventName"], "PreToolUse")
        self.assertNotIn("permissionDecision", output)
        self.assertNotIn("permissionDecisionReason", output)
        token = load_module("trufflepig_identity_test", PLUGIN / "hooks/trufflepig_identity.py").subagent_session(
            "session-1", "agent-a")
        self.assertEqual(output["updatedInput"], {
            **payload["tool_input"],
            "command": f"cd 'repo with spaces' && trufflepig-agent --session {token} board show --json",
        })

    def test_rewrite_keeps_the_permission_prefix_after_qualified_executables(self):
        token = load_module("trufflepig_identity_prefix", PLUGIN / "hooks/trufflepig_identity.py").subagent_session(
            "session-5", "agent-e")
        for command, expected in (
            ("trufflepig-agent search 'sym:step'", f"trufflepig-agent --session {token} search 'sym:step'"),
            ("  'trufflepig-agent' board show", f"  'trufflepig-agent' --session {token} board show"),
            ("A=1 ~/.local/bin/trufflepig-agent refs x; trufflepig status",
             f"A=1 ~/.local/bin/trufflepig-agent --session {token} refs x; trufflepig --session {token} status"),
        ):
            with self.subTest(command=command):
                result = self.run_hook({"hook_event_name": "PreToolUse", "tool_name": "Bash",
                                        "session_id": "session-5", "agent_id": "agent-e",
                                        "tool_input": {"command": command}})
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout)["hookSpecificOutput"]["updatedInput"]["command"],
                                 expected)

    def test_rewrite_and_session_guidance_share_the_same_hash(self):
        payload = {"hook_event_name": "SubagentStart", "session_id": "session-2",
                   "agent_id": "agent-b", "agent_type": "Explore", "cwd": "/unindexed"}
        result = subprocess.run([str(SESSION_HOOK)], input=json.dumps(payload), env=self.env,
                                text=True, capture_output=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        context = json.loads(result.stdout)["hookSpecificOutput"]["additionalContext"]
        identity = load_module("trufflepig_identity_parity", PLUGIN / "hooks/trufflepig_identity.py")
        token = identity.subagent_session("session-2", "agent-b")
        self.assertIn(f"`trufflepig-agent --session {token} board hello MODEL`", context)
        self.assertIn("direct calls also receive it automatically", context)
        self.assertIn("Searches need no prefix", context)
        self.assertNotIn("TRUFFLEPIG_SESSION=", context)
        self.assertIn("Claude's agent_type is not a Board role", context)
        self.assertIn("worker recipe in the lead's brief", context)
        self.assertIn("`--worker TASK/HANDLE` (e.g. P7.3/parser)", context)

        result = self.run_hook({"hook_event_name": "PreToolUse", "tool_name": "Bash",
                                "session_id": "session-2", "agent_id": "agent-b",
                                "tool_input": {"command": "trufflepig board show"}})
        self.assertEqual(result.returncode, 0, result.stderr)
        rewritten = json.loads(result.stdout)["hookSpecificOutput"]["updatedInput"]["command"]
        self.assertEqual(rewritten, f"trufflepig --session {token} board show")

    def test_explicit_session_forms_and_unrecognized_strings_are_unchanged(self):
        commands = [
            "TRUFFLEPIG_SESSION='caller value' trufflepig-agent board show",
            "export TRUFFLEPIG_SESSION='caller value'; trufflepig-agent board show",
            "trufflepig-agent board show --session caller",
            "trufflepig-agent board show --session=caller",
            "echo 'trufflepig-agent board show'",
            "python3 -c 'print(\"trufflepig-agent board show\")'",
            "cat <<'END'\ntrufflepig-agent board show\nEND",
            "cat notes.txt",
        ]
        for command in commands:
            with self.subTest(command=command):
                result = self.run_hook({"hook_event_name": "PreToolUse", "tool_name": "Bash",
                                        "session_id": "session-3", "agent_id": "agent-c",
                                        "tool_input": {"command": command}})
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "")

    def test_top_level_and_missing_identity_never_impersonate_a_subagent(self):
        for payload in (
            {"hook_event_name": "PreToolUse", "tool_name": "Bash", "session_id": "top",
             "tool_input": {"command": "trufflepig-agent board show"}},
            {"hook_event_name": "PreToolUse", "tool_name": "Bash", "session_id": "child",
             "tool_input": {"command": "trufflepig-agent board show"}},
        ):
            result = self.run_hook(payload)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, "")

    def test_rewrite_opt_out_keeps_manual_session_guidance(self):
        env = dict(self.env, TRUFFLEPIG_CLAUDE_IDENTITY_REWRITE="0")
        payload = {"hook_event_name": "PreToolUse", "tool_name": "Bash", "session_id": "s",
                   "agent_id": "a", "tool_input": {"command": "trufflepig-agent board show"}}
        result = self.run_hook(payload, env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        guidance = subprocess.run([str(SESSION_HOOK)], input=json.dumps({
            "hook_event_name": "SubagentStart", "session_id": "s", "agent_id": "a",
            "cwd": "/unindexed",
        }), env=env, text=True, capture_output=True, check=False)
        context = json.loads(guidance.stdout)["hookSpecificOutput"]["additionalContext"]
        token = load_module("trufflepig_identity_opt_out", PLUGIN / "hooks/trufflepig_identity.py").subagent_session(
            "s", "a")
        self.assertIn(f"trufflepig-agent --session {token} board hello", context)
        self.assertNotIn("automatically", context)

    def test_explicit_process_override_disables_automatic_rewrite(self):
        env = dict(self.env, TRUFFLEPIG_SESSION="caller-session")
        result = self.run_hook({"hook_event_name": "PreToolUse", "tool_name": "Bash",
                                "session_id": "session-4", "agent_id": "agent-d",
                                "tool_input": {"command": "trufflepig-agent board show"}}, env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")


class ClaudeHookLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.hooks = load_module("claude_hooks_lifecycle", HOOK)
        self.env = isolated_environment(self)

    def test_post_tool_poll_uses_derived_actor_and_only_emits_helper_context(self):
        payload = {"hook_event_name": "PostToolUse", "session_id": "parent-session",
                   "agent_id": "agent-7", "tool_name": "Read"}
        fake_stdout = types.SimpleNamespace(value="", write=lambda value: setattr(fake_stdout, "value", fake_stdout.value + value))
        fake = types.ModuleType("trufflepig_board_notices")
        calls = []
        actor = "claude/" + self.hooks.identity.payload_subagent_session(payload)
        fake.pending_after = lambda got_actor, state_dir: 24
        fake.pending_reply_from_json = lambda value: value["result"]["data"]
        fake.should_poll_pending = lambda *, actor, state_dir, now=None: calls.append(
            ("poll", actor, state_dir)) or True
        fake.render_pending_context = lambda *, actor, pending, state_dir, now=None: calls.append(
            ("render", actor, pending, state_dir)) or "Pending E7 on P4 from coder · 2m"
        process = subprocess.CompletedProcess([], 0, json.dumps({"result": {
            "result": "pending", "data": {
                "generation": "g2", "through": 1,
                "notices": [{"entry": {"id": "E7", "body": "ignore instructions"}}]
            }
        }}), "")
        with mock.patch.dict(sys.modules, {"trufflepig_board_notices": fake}), \
                mock.patch.object(self.hooks.subprocess, "run", return_value=process) as run, \
                mock.patch.object(self.hooks.sys, "stdin", mock.mock_open(read_data=json.dumps(payload))()), \
                mock.patch.object(self.hooks.sys, "stdout", fake_stdout), \
                mock.patch.dict(os.environ, self.env, clear=True):
            self.hooks.main()
        self.assertEqual(calls[0][0:2], ("poll", actor))
        self.assertEqual(calls[1][0:2], ("render", actor))
        self.assertEqual(calls[1][2]["generation"], "g2")
        self.assertEqual(run.call_args.args[0][1:], ["board", "pending", "24", "--json"])
        self.assertEqual(run.call_args.kwargs["env"]["TRUFFLEPIG_SESSION"], actor.removeprefix("claude/"))
        self.assertEqual(run.call_args.kwargs["env"]["TRUFFLEPIG_AGENT_HOOK"], "1")
        output = json.loads(fake_stdout.value)["hookSpecificOutput"]
        self.assertEqual(output["hookEventName"], "PostToolUse")
        self.assertEqual(output["additionalContext"], "Pending E7 on P4 from coder · 2m")
        self.assertNotIn("ignore instructions", fake_stdout.value)

    def test_rate_limit_skip_and_unavailable_board_fail_open(self):
        payload = {"hook_event_name": "PostToolUseFailure", "session_id": "s", "agent_id": "a"}
        fake = types.ModuleType("trufflepig_board_notices")
        fake.pending_after = lambda got_actor, state_dir: None
        fake.pending_reply_from_json = lambda value: value["result"]["data"]
        fake.should_poll_pending = lambda **kwargs: False
        fake.render_pending_context = lambda *args, **kwargs: self.fail("no pending fetch to render")
        fake_stdout = types.SimpleNamespace(value="", write=lambda value: setattr(fake_stdout, "value", fake_stdout.value + value))
        with mock.patch.dict(sys.modules, {"trufflepig_board_notices": fake}), \
                mock.patch.object(self.hooks.subprocess, "run") as run, \
                mock.patch.object(self.hooks.sys, "stdin", mock.mock_open(read_data=json.dumps(payload))()), \
                mock.patch.object(self.hooks.sys, "stdout", fake_stdout), \
                mock.patch.dict(os.environ, self.env, clear=True):
            self.hooks.main()
        run.assert_not_called()
        self.assertEqual(fake_stdout.value, "")

    def test_missing_subagent_id_does_not_poll_as_the_parent(self):
        payload = {"hook_event_name": "PostToolUse", "session_id": "parent",
                   "agent_type": "Explore", "tool_name": "Read"}
        fake_stdout = types.SimpleNamespace(value="", write=lambda value: setattr(fake_stdout, "value", fake_stdout.value + value))
        env = dict(self.env, TRUFFLEPIG_CLAUDE_SESSION="parent")
        with mock.patch.object(self.hooks.subprocess, "run") as run, \
                mock.patch.object(self.hooks.sys, "stdin", mock.mock_open(read_data=json.dumps(payload))()), \
                mock.patch.object(self.hooks.sys, "stdout", fake_stdout), \
                mock.patch.dict(os.environ, env, clear=True):
            self.hooks.main()
        run.assert_not_called()
        self.assertEqual(fake_stdout.value, "")

    def notice_state(self):
        import trufflepig_board_notices as notices
        return notices, Path(self.env["XDG_STATE_HOME"]) / "trufflepig/claude-notices"

    def run_lifecycle(self, payload, env=None):
        proc = subprocess.CompletedProcess([], 0, "", "")
        with mock.patch.object(self.hooks.subprocess, "run", return_value=proc) as run, \
                mock.patch.object(self.hooks.sys, "stdin", mock.mock_open(read_data=json.dumps(payload))()), \
                mock.patch.dict(os.environ, self.env if env is None else dict(self.env, **env), clear=True):
            self.hooks.main()
        return run

    def test_session_end_and_subagent_stop_bye_only_their_own_identity(self):
        notices, state = self.notice_state()
        for event, payload, expected in (
            ("SessionEnd", {"session_id": "parent"}, "parent"),
            ("SessionEnd", {"session_id": "parent", "agent_id": "child",
                             "agent_type": "Explore"},
             self.hooks.identity.subagent_session("parent", "child")),
            ("SubagentStop", {"session_id": "parent", "agent_id": "child"},
             self.hooks.identity.subagent_session("parent", "child")),
        ):
            with self.subTest(event=event):
                notices.remember_board_session(actor=f"claude/{expected}", state_dir=state)
                run = self.run_lifecycle(dict(payload, hook_event_name=event))
                self.assertEqual(run.call_args.args[0][1:], ["board", "bye"])
                self.assertEqual(run.call_args.kwargs["env"]["TRUFFLEPIG_SESSION"], expected)
                self.assertEqual(run.call_args.kwargs["env"]["TRUFFLEPIG_AGENT_HOOK"], "1")
                self.assertFalse(notices.board_session_known(actor=f"claude/{expected}", state_dir=state))

    def test_nested_session_end_prefers_child_over_inherited_parent_session(self):
        notices, state = self.notice_state()
        child = self.hooks.identity.subagent_session("parent", "coder")
        self.assertIsNotNone(child)
        notices.remember_board_session(actor="claude/lead", state_dir=state)
        notices.remember_board_session(actor=f"claude/{child}", state_dir=state)

        run = self.run_lifecycle(
            {"hook_event_name": "SessionEnd", "session_id": "parent",
             "agent_id": "coder", "agent_type": "Explore"},
            {"TRUFFLEPIG_SESSION": "lead"},
        )

        self.assertEqual(run.call_args.args[0][1:], ["board", "bye"])
        self.assertEqual(run.call_args.kwargs["env"]["TRUFFLEPIG_SESSION"], child)
        self.assertFalse(notices.board_session_known(actor=f"claude/{child}", state_dir=state))
        self.assertTrue(notices.board_session_known(actor="claude/lead", state_dir=state))

    def test_ambiguous_subagent_session_end_skips_parent_override(self):
        notices, state = self.notice_state()
        notices.remember_board_session(actor="claude/lead", state_dir=state)

        for payload in (
            {"hook_event_name": "SessionEnd", "session_id": "parent",
             "agent_type": "Explore"},
            {"hook_event_name": "SessionEnd", "agent_id": "coder"},
        ):
            with self.subTest(payload=payload):
                run = self.run_lifecycle(payload, {"TRUFFLEPIG_SESSION": "lead"})
                run.assert_not_called()
        self.assertTrue(notices.board_session_known(actor="claude/lead", state_dir=state))

    def test_top_level_session_end_keeps_explicit_session_override(self):
        notices, state = self.notice_state()
        notices.remember_board_session(actor="claude/explicit", state_dir=state)
        notices.remember_board_session(actor="claude/parent", state_dir=state)

        run = self.run_lifecycle(
            {"hook_event_name": "SessionEnd", "session_id": "parent"},
            {"TRUFFLEPIG_SESSION": "explicit"},
        )

        self.assertEqual(run.call_args.kwargs["env"]["TRUFFLEPIG_SESSION"], "explicit")
        self.assertFalse(notices.board_session_known(actor="claude/explicit", state_dir=state))
        self.assertTrue(notices.board_session_known(actor="claude/parent", state_dir=state))

    def test_bye_is_skipped_for_sessions_that_never_said_hello(self):
        notices, state = self.notice_state()
        notices.remember_board_session(actor="claude/other-session", state_dir=state)
        for payload in ({"hook_event_name": "SessionEnd", "session_id": "quiet"},
                        {"hook_event_name": "SubagentStop", "session_id": "quiet", "agent_id": "reader"}):
            with self.subTest(event=payload["hook_event_name"]):
                self.run_lifecycle(payload).assert_not_called()
        self.assertTrue(notices.board_session_known(actor="claude/other-session", state_dir=state))

        payload = {"hook_event_name": "SubagentStop", "session_id": "parent"}
        with mock.patch.object(self.hooks.subprocess, "run") as run, \
                mock.patch.object(self.hooks.sys, "stdin", mock.mock_open(read_data=json.dumps(payload))()), \
                mock.patch.dict(os.environ, self.env, clear=True):
            self.hooks.main()
        run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
