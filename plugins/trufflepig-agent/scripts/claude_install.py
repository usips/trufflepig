"""Merge the Claude session hook and narrow runtime access into user settings."""
from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import shlex
import stat
import tempfile


def claude_home() -> Path:
    return Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude").expanduser().absolute()


PERMISSION = "Bash(trufflepig-agent *)"


def merge_command(hooks: dict, event: str, matcher: str | None, command: str, timeout: int) -> None:
    """Add one command hook to `event` unless an identical group already runs it."""
    groups = hooks.setdefault(event, [])
    if not isinstance(groups, list):
        raise ValueError(f"Claude {event} hooks must be an array")
    installed = any(isinstance(group, dict) and group.get("matcher") == matcher and
                    any(h.get("type") == "command" and h.get("command") == command
                        for h in group.get("hooks", []) if isinstance(h, dict)) for group in groups)
    if not installed:
        group = {"matcher": matcher} if matcher else {}
        group["hooks"] = [{"type": "command", "command": command, "timeout": timeout}]
        groups.append(group)


def prepare_settings(path: Path, hook: Path, runtime: Path, steer: Path | None = None) -> dict:
    """Merge session attribution, optional Bash search steering, the wrapper's Bash
    permission, and runtime write access into Claude user settings."""
    settings = json.loads(path.read_text()) if path.exists() else {}
    if not isinstance(settings, dict):
        raise ValueError(f"expected a settings object in {path}")
    settings = copy.deepcopy(settings)
    if settings.get("disableAllHooks"):
        raise ValueError("Claude disables hooks; enable them before installing session attribution")
    hooks = settings.setdefault("hooks", {})
    if not isinstance(hooks, dict):
        raise ValueError("Claude hooks must be an object")
    merge_command(hooks, "SessionStart", None, shlex.quote(str(hook.absolute())), 5)
    if steer is not None:
        command = f"{shlex.quote(str(steer.absolute()))} claude"
        merge_command(hooks, "PreToolUse", "Bash", command, 5)
        merge_command(hooks, "PostToolUse", "Bash", command, 5)
    permissions = settings.setdefault("permissions", {})
    if not isinstance(permissions, dict):
        raise ValueError("Claude permissions must be an object")
    allow = permissions.setdefault("allow", [])
    if not isinstance(allow, list):
        raise ValueError("Claude permissions.allow must be an array")
    if PERMISSION not in allow:
        allow.append(PERMISSION)
    sandbox = settings.setdefault("sandbox", {})
    if not isinstance(sandbox, dict):
        raise ValueError("Claude sandbox must be an object")
    filesystem = sandbox.setdefault("filesystem", {})
    if not isinstance(filesystem, dict):
        raise ValueError("Claude sandbox.filesystem must be an object")
    writable = filesystem.setdefault("allowWrite", [])
    if not isinstance(writable, list) or not all(isinstance(p, str) for p in writable):
        raise ValueError("Claude sandbox.filesystem.allowWrite must be a string array")
    if str(runtime) not in writable:
        writable.append(str(runtime))
    return settings


def write_settings(path: Path, settings: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    mode = stat.S_IMODE(path.stat().st_mode) if path.exists() else 0o600
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                         prefix=".trufflepig-settings-", delete=False) as output:
            temporary = Path(output.name)
            output.write(json.dumps(settings, indent=2, ensure_ascii=False) + "\n")
        temporary.chmod(mode)
        temporary.replace(path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
