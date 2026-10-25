#!/usr/bin/env python3
"""Claude Bash identity rewriting (`--session` insertion) and bounded Board lifecycle hooks."""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys

HOOKS = Path(__file__).resolve().parent
PLUGIN = HOOKS.parent
sys.path.insert(0, str(HOOKS))
sys.path.insert(0, str(PLUGIN / "bin"))
import trufflepig_identity as identity  # noqa: E402

ASSIGNMENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*=")
WRAPPERS = {"trufflepig", "trufflepig-agent"}
PENDING_TIMEOUT = 2.5
BYE_TIMEOUT = 0.65


def shell_segments(command: str) -> list[tuple[int, int, str]] | None:
    """Return source spans split only at unquoted shell control operators."""
    result: list[tuple[int, int, str]] = []
    start = 0
    operator = ""
    quote: str | None = None
    escaped = False
    substitution_depth = 0
    comment = False
    index = 0
    while index < len(command):
        char = command[index]
        if comment:
            if char in "\r\n":
                comment = False
            else:
                index += 1
                continue
        if escaped:
            escaped = False
            index += 1
            continue
        if quote == "'":
            if char == "'":
                quote = None
            index += 1
            continue
        if quote == '"':
            if char == "\\" and index + 1 < len(command):
                escaped = True
            elif char == '"':
                quote = None
            index += 1
            continue
        if quote == "`":
            if char == "\\" and index + 1 < len(command):
                escaped = True
            elif char == "`":
                quote = None
            index += 1
            continue
        if char == "\\":
            escaped = True
            index += 1
            continue
        if char in "'\"`":
            quote = char
            index += 1
            continue
        if command.startswith("<<", index):
            return None
        if command.startswith("$(", index):
            substitution_depth += 1
            index += 2
            continue
        if substitution_depth:
            if char == "(":
                substitution_depth += 1
            elif char == ")":
                substitution_depth -= 1
            index += 1
            continue
        if char == "#" and (index == 0 or command[index - 1].isspace()):
            comment = True
            index += 1
            continue
        if char in ";&|\r\n":
            end = index
            if end > start or not result:
                result.append((start, end, operator))
            if index + 1 < len(command) and command[index + 1] == char and char in "&|":
                operator = command[index:index + 2]
                index += 2
            else:
                operator = char
                index += 1
            start = index
            continue
        index += 1
    if quote is not None or escaped or substitution_depth:
        return None
    if start < len(command) or not result:
        result.append((start, len(command), operator))
    return result


def segment_words(source: str) -> list[str] | None:
    try:
        return shlex.split(source, comments=True, posix=True)
    except ValueError:
        return None


def command_word_index(words: list[str]) -> int:
    index = 0
    while index < len(words) and ASSIGNMENT.match(words[index]):
        index += 1
    return index


def word_end(source: str, index: int) -> int:
    """Index just past the shell word starting at `index` (quotes and escapes kept whole)."""
    quote: str | None = None
    while index < len(source):
        char = source[index]
        if quote:
            if char == "\\" and quote == '"':
                index += 1
            elif char == quote:
                quote = None
        elif char == "\\":
            index += 1
        elif char in "'\"":
            quote = char
        elif char.isspace():
            return index
        index += 1
    return index


def executable_end(source: str, command_index: int) -> int:
    """Source offset just past word `command_index`, the executable, of one segment."""
    index = 0
    for _ in range(command_index + 1):
        while index < len(source) and source[index].isspace():
            index += 1
        index = word_end(source, index)
    return index


def rewrite_command(command: str, session: str) -> str | None:
    """Insert `--session SESSION` after the executable of direct Trufflepig invocations,
    keeping the command prefix that `Bash(trufflepig-agent *)` permission rules match."""
    segments = shell_segments(command)
    if segments is None:
        return None
    edits: list[tuple[int, int, str]] = []
    shell_session_set = False
    for start, end, _operator in segments:
        words = segment_words(command[start:end])
        if not words:
            continue
        command_index = command_word_index(words)
        if command_index == len(words):
            shell_session_set |= any(word.startswith("TRUFFLEPIG_SESSION=") for word in words)
            continue
        executable = words[command_index].rsplit("/", 1)[-1]
        if executable == "export" and any(
            word == "TRUFFLEPIG_SESSION" or word.startswith("TRUFFLEPIG_SESSION=")
            for word in words[command_index + 1:]
        ):
            shell_session_set = True
        if shell_session_set or executable not in WRAPPERS:
            continue
        if any(word == "TRUFFLEPIG_SESSION" or word.startswith("TRUFFLEPIG_SESSION=")
               for word in words[:command_index]):
            continue
        if any(word == "--session" or word.startswith("--session=") for word in words[command_index + 1:]):
            continue
        insert_at = start + executable_end(command[start:end], command_index)
        edits.append((insert_at, insert_at, f" --session {shlex.quote(session)}"))
    if not edits:
        return None
    for begin, finish, insertion in reversed(edits):
        command = command[:begin] + insertion + command[finish:]
    return command


def hook_output(event: str, **fields: object) -> None:
    print(json.dumps({"hookSpecificOutput": {"hookEventName": event, **fields}}))


def payload_session(payload: dict[str, object]) -> str | None:
    explicit = os.environ.get("TRUFFLEPIG_SESSION")
    if explicit:
        return explicit
    child = identity.payload_subagent_session(payload)
    if child:
        return child
    agent_type = payload.get("agent_type")
    if isinstance(agent_type, str) and agent_type.strip():
        return None
    session_id = payload.get("session_id")
    if isinstance(session_id, str) and session_id.strip():
        return session_id
    inherited = os.environ.get("TRUFFLEPIG_CLAUDE_SESSION")
    return inherited if inherited else None


def state_directory() -> Path:
    state_home = Path(os.environ.get("XDG_STATE_HOME", "~/.local/state")).expanduser()
    return state_home / "trufflepig" / "claude-notices"


def wrapper_path() -> str | None:
    return shutil.which("trufflepig-agent") or str(Path.home() / ".local/bin/trufflepig-agent")


def pending_notification(payload: dict[str, object], event: str) -> None:
    session = payload_session(payload)
    if not session:
        return
    try:
        from trufflepig_board_notices import (
            pending_after,
            pending_reply_from_json,
            render_pending_context,
            should_poll_pending,
        )
    except ImportError:
        return
    state = state_directory()
    actor = f"claude/{session}"
    try:
        if not should_poll_pending(actor=actor, state_dir=state):
            return
    except Exception:
        return
    executable = wrapper_path()
    if not executable:
        return
    env = os.environ.copy()
    env["TRUFFLEPIG_SESSION"] = session
    env["TRUFFLEPIG_AGENT_HOOK"] = "1"
    command = [executable, "board", "pending"]
    try:
        after = pending_after(actor, state)
    except Exception:
        after = None
    if isinstance(after, int) and after > 0:
        command.append(str(after))
    command.append("--json")
    try:
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=PENDING_TIMEOUT,
            check=False,
            env=env,
        )
    except (OSError, subprocess.TimeoutExpired):
        return
    if result.returncode != 0:
        return
    try:
        reply = pending_reply_from_json(json.loads(result.stdout))
    except (ValueError, TypeError):
        reply = None
    if not reply:
        return
    try:
        context = render_pending_context(actor=actor, pending=reply, state_dir=state)
    except Exception:
        return
    if isinstance(context, str) and context.strip():
        hook_output(event, additionalContext=context[:1200])


def board_bye(payload: dict[str, object], event: str) -> None:
    if event == "SubagentStop":
        session = identity.payload_subagent_session(payload)
    else:
        session = payload_session(payload)
    if not session or not said_hello(f"claude/{session}"):
        return
    executable = wrapper_path()
    if not executable:
        return
    env = os.environ.copy()
    env["TRUFFLEPIG_SESSION"] = session
    env["TRUFFLEPIG_AGENT_HOOK"] = "1"
    try:
        subprocess.run([executable, "board", "bye"], capture_output=True,
                       text=True, timeout=BYE_TIMEOUT, check=False, env=env)
    except (OSError, subprocess.TimeoutExpired):
        pass
    try:
        from trufflepig_board_notices import forget_board_session
        forget_board_session(actor=f"claude/{session}", state_dir=state_directory())
    except ImportError:
        pass


def said_hello(actor: str) -> bool:
    """Whether the wrapper marked this session after a successful hello or claim.

    Most subagents never touch the Board, so their bye is skipped. Unreadable marker
    state fails open: the router answers a bye with nothing to end without writing."""
    try:
        from trufflepig_board_notices import board_session_known
        return board_session_known(actor=actor, state_dir=state_directory())
    except (ImportError, OSError):
        return True


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (ValueError, OSError):
        return 0
    if not isinstance(payload, dict):
        return 0
    event = payload.get("hook_event_name")
    if not isinstance(event, str):
        return 0
    if event == "PreToolUse" and payload.get("tool_name") == "Bash":
        tool_input = payload.get("tool_input")
        session = identity.payload_subagent_session(payload)
        if (not identity.identity_rewrite_enabled() or not session or os.environ.get("TRUFFLEPIG_SESSION")
                or not isinstance(tool_input, dict)):
            return 0
        command = tool_input.get("command")
        if not isinstance(command, str):
            return 0
        rewritten = rewrite_command(command, session)
        if rewritten is None:
            return 0
        updated = dict(tool_input)
        updated["command"] = rewritten
        hook_output(event, updatedInput=updated)
        return 0
    if event in {"PostToolUse", "PostToolUseFailure"}:
        pending_notification(payload, event)
    elif event == "SessionEnd":
        board_bye(payload, event)
    elif event == "SubagentStop" and identity.payload_subagent_session(payload):
        board_bye(payload, event)
    return 0


if __name__ == "__main__":
    sys.exit(main())
