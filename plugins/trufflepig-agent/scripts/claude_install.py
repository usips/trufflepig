"""Merge or remove Claude attribution, steering, and Board lifecycle hooks in user settings."""
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
PLUGIN_PREFIX = "trufflepig-agent@"
SKILL_NAMES = ("trufflepig-code-search", "trufflepig-plan-board")
CLAUDE_HOOK_EVENTS = (
    ("PreToolUse", "Bash", 5),
    ("PostToolUse", None, 5),
    ("PostToolUseFailure", None, 5),
    ("SessionEnd", None, 1),
    ("SubagentStop", None, 1),
)


def managed_hooks(bin_dir: Path) -> list[tuple[str, str | None, str, int]]:
    """(event, matcher, command, timeout) for every hook a settings install registers."""
    session = shlex.quote(str((bin_dir / "trufflepig-claude-session").absolute()))
    steer = f"{shlex.quote(str((bin_dir / 'trufflepig-agent-steer').absolute()))} claude"
    board = shlex.quote(str((bin_dir / "trufflepig-claude-hooks").absolute()))
    return [("SessionStart", None, session, 5), ("SubagentStart", None, session, 5),
            ("PreToolUse", "Bash", steer, 5), ("PreToolUse", "Agent", steer, 5),
            ("PostToolUse", "Bash", steer, 5),
            *((event, matcher, board, timeout) for event, matcher, timeout in CLAUDE_HOOK_EVENTS)]


def plugin_enabled(path: Path) -> bool:
    """Whether Claude settings enable the trufflepig-agent plugin, which then owns
    the skill and hooks; the installer must not register them a second time."""
    try:
        settings = json.loads(path.read_text()) if path.exists() else {}
    except ValueError:
        return False
    enabled = settings.get("enabledPlugins") if isinstance(settings, dict) else None
    return isinstance(enabled, dict) and any(
        name.startswith(PLUGIN_PREFIX) and value for name, value in enabled.items())


def merge_command(hooks: dict, event: str, matcher: str | None, command: str, timeout: int) -> None:
    """Add one command hook to the matching event group without replacing other hooks."""
    groups = hooks.setdefault(event, [])
    if not isinstance(groups, list):
        raise ValueError(f"Claude {event} hooks must be an array")
    matching = [candidate for candidate in groups
                if isinstance(candidate, dict) and candidate.get("matcher") == matcher]
    for candidate in matching:
        commands = candidate.get("hooks", [])
        if not isinstance(commands, list):
            raise ValueError(f"Claude {event} hook group hooks must be an array")
        if any(isinstance(h, dict) and h.get("type") == "command" and h.get("command") == command
               for h in commands):
            return
    group = matching[0] if matching else None
    if group is None:
        group = {"matcher": matcher} if matcher else {}
        group["hooks"] = []
        groups.append(group)
    commands = group.setdefault("hooks", [])
    if not isinstance(commands, list):
        raise ValueError(f"Claude {event} hook group hooks must be an array")
    commands.append({"type": "command", "command": command, "timeout": timeout})


def remove_command(hooks: dict, event: str, matcher: str | None, command: str) -> None:
    """Remove one command hook, dropping a group or event that it leaves empty."""
    groups = hooks.get(event)
    if not isinstance(groups, list):
        return
    for group in list(groups):
        if not isinstance(group, dict) or group.get("matcher") != matcher \
                or not isinstance(group.get("hooks"), list):
            continue
        kept = [h for h in group["hooks"]
                if not (isinstance(h, dict) and h.get("type") == "command" and h.get("command") == command)]
        if len(kept) == len(group["hooks"]):
            continue
        if kept:
            group["hooks"] = kept
        else:
            groups.remove(group)
    if not groups:
        del hooks[event]


def load_settings(path: Path) -> dict:
    settings = json.loads(path.read_text()) if path.exists() else {}
    if not isinstance(settings, dict):
        raise ValueError(f"expected a settings object in {path}")
    return copy.deepcopy(settings)


def remove_settings(path: Path, bin_dir: Path) -> dict:
    """Settings without the hooks and permission a settings install added."""
    settings = load_settings(path)
    hooks = settings.get("hooks")
    if isinstance(hooks, dict):
        for event, matcher, command, _timeout in managed_hooks(bin_dir):
            remove_command(hooks, event, matcher, command)
    allow = settings.get("permissions", {}).get("allow") if isinstance(settings.get("permissions"), dict) else None
    if isinstance(allow, list) and PERMISSION in allow:
        allow.remove(PERMISSION)
    return settings


def prepare_settings(path: Path, runtime: Path, bin_dir: Path, plugin: bool = False) -> dict:
    """Merge attribution, steering, and Board hooks plus runtime access into Claude settings.
    With the plugin enabled it owns the hooks, so earlier settings entries are removed."""
    settings = load_settings(path)
    if settings.get("disableAllHooks"):
        raise ValueError("Claude disables hooks; enable them before installing session attribution")
    hooks = settings.setdefault("hooks", {})
    if not isinstance(hooks, dict):
        raise ValueError("Claude hooks must be an object")
    for event, matcher, command, timeout in managed_hooks(bin_dir):
        if plugin:
            remove_command(hooks, event, matcher, command)
        else:
            merge_command(hooks, event, matcher, command, timeout)
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


def check_claude_hooks(settings_path: Path, bin_dir: Path, *, notices_helper: Path | None = None,
                       plugin_hooks: bool = False) -> None:
    """Check the installed Claude hook links and their settings registrations."""
    plugin = Path(__file__).resolve().parents[1]
    expected_links = (
        (bin_dir / "trufflepig-claude-hooks", plugin / "hooks/claude-hooks.py", True),
        (bin_dir / "trufflepig_identity.py", plugin / "hooks/trufflepig_identity.py", False),
    )
    if notices_helper is not None:
        expected_links += ((bin_dir / "trufflepig_board_notices.py", notices_helper, False),)
    for installed, source, executable in expected_links:
        try:
            resolved = installed.resolve(strict=True)
        except OSError as error:
            raise ValueError(f"Claude hook dependency is missing: {installed}") from error
        if resolved != source.resolve() or not resolved.is_file():
            raise ValueError(f"Claude hook dependency does not match {source}: {installed}")
        if executable and not os.access(resolved, os.X_OK):
            raise ValueError(f"Claude hook is not executable: {installed}")

    if plugin_hooks:
        hook_path = plugin / "hooks/hooks.json"
        command = '"${CLAUDE_PLUGIN_ROOT}/hooks/claude-hooks.py"'
    else:
        hook_path = settings_path
        command = shlex.quote(str((bin_dir / "trufflepig-claude-hooks").absolute()))
    try:
        configuration = json.loads(hook_path.read_text())
    except (OSError, ValueError) as error:
        raise ValueError(f"cannot read Claude hook configuration at {hook_path}: {error}") from error
    hooks = configuration.get("hooks") if isinstance(configuration, dict) else None
    if not isinstance(hooks, dict):
        raise ValueError(f"Claude hook configuration is missing from {hook_path}")
    for event, matcher, _timeout in CLAUDE_HOOK_EVENTS:
        matches = sum(
            1
            for group in hooks.get(event, [])
            if isinstance(group, dict) and group.get("matcher") == matcher
            for hook in group.get("hooks", [])
            if isinstance(hook, dict) and hook.get("type") == "command" and hook.get("command") == command
        )
        if matches != 1:
            raise ValueError(f"Claude {event} hook registration count is {matches}, expected one")


def write_settings(path: Path, settings: dict) -> None:
    """Atomically replace the settings file, following a symlinked `settings.json`
    to the file it names so the link itself survives."""
    target = path.resolve()
    target.parent.mkdir(parents=True, exist_ok=True)
    mode = stat.S_IMODE(target.stat().st_mode) if target.exists() else 0o600
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=target.parent,
                                         prefix=".trufflepig-settings-", delete=False) as output:
            temporary = Path(output.name)
            output.write(json.dumps(settings, indent=2, ensure_ascii=False) + "\n")
        temporary.chmod(mode)
        temporary.replace(target)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def remove_skill_links(plugin: Path) -> list[Path]:
    """Remove personal skill symlinks that point into this checkout."""
    removed = []
    for name in SKILL_NAMES:
        link = claude_home() / "skills" / name
        if link.is_symlink() and link.resolve() == (plugin / "skills" / name).resolve():
            link.unlink()
            removed.append(link)
    return removed
