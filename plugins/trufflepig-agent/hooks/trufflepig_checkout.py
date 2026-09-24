"""Indexed checkouts: registered workspace members and their linked worktrees.

A directory belongs to a checkout by path or, for a linked worktree (a `.git`
file) anywhere on disk, through its Git common directory.
"""
from __future__ import annotations

import hashlib
import os
import subprocess
from dataclasses import dataclass
from pathlib import Path


try:
    import tomllib
except ModuleNotFoundError:  # Python < 3.11
    tomllib = None


def state_dir() -> Path:
    base = os.environ.get("XDG_STATE_HOME") or os.path.join(os.path.expanduser("~"), ".local", "state")
    return Path(base) / "trufflepig"


def config_dir() -> Path:
    return Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "trufflepig"


def cwd_key(cwd: str) -> str:
    return hashlib.blake2s(cwd.encode(), digest_size=6).hexdigest()


def load_toml(path: Path) -> dict:
    if tomllib is None:
        return {}
    try:
        document = tomllib.loads(path.read_text())
    except (OSError, ValueError):
        return {}
    return document if isinstance(document, dict) else {}


def member_roots_of(config_path: Path) -> list[tuple[Path, str]]:
    """(member root, member name) pairs declared by one workspace config."""
    roots: list[tuple[Path, str]] = []
    for name, member in (load_toml(config_path).get("members") or {}).items():
        raw = member.get("path") if isinstance(member, dict) else None
        if raw:
            roots.append(((config_path.parent / Path(os.path.expanduser(raw))).resolve(), str(name)))
    return roots


def workspace_members() -> list[tuple[Path, str, str]]:
    """(root, member, workspace) for every member of every registered workspace."""
    registry = config_dir() / "workspaces.toml"
    members: list[tuple[Path, str, str]] = []
    for entry in load_toml(registry).get("workspaces") or []:
        if not isinstance(entry, str):
            continue
        config = (config_dir() / Path(os.path.expanduser(entry))).resolve()
        workspace = str((load_toml(config).get("workspace") or {}).get("name") or config.stem)
        members.extend((root, name, workspace) for root, name in member_roots_of(config))
    return members


def git_common_dir(directory: Path) -> Path | None:
    """Canonical Git common directory of `directory`, or None outside a repository."""
    try:
        completed = subprocess.run(
            ["git", "-C", str(directory), "rev-parse", "--path-format=absolute", "--git-common-dir"],
            capture_output=True, text=True, timeout=2, check=False,
            env={**os.environ, "GIT_TERMINAL_PROMPT": "0"},
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if completed.returncode != 0 or not completed.stdout.strip():
        return None
    return Path(completed.stdout.strip()).resolve()


def member_common_dir(root: Path) -> Path | None:
    dot_git = root / ".git"
    if dot_git.is_dir():
        return dot_git.resolve()
    return git_common_dir(root) if dot_git.is_file() else None


@dataclass
class IndexedRoot:
    """The checkout that owns a directory: a member root or a linked worktree of one."""
    checkout: Path
    member: str
    workspace: str


def indexed_checkout(cwd: Path) -> IndexedRoot | None:
    """The indexed checkout that owns `cwd`. A linked worktree (a `.git` file), even one
    nested inside its member such as `.worktrees/NAME`, is its own checkout mapped to the
    member through the Git common directory; otherwise the member owns `cwd` by path."""
    members = workspace_members()
    nearest = next((a for a in (cwd, *cwd.parents) if (a / ".git").exists()), None)
    if nearest is not None and (nearest / ".git").is_file():
        common = git_common_dir(nearest)
        for root, member, workspace in members:
            if common is not None and member_common_dir(root) == common:
                return IndexedRoot(nearest, member, workspace)
        return None
    for root, member, workspace in members:
        if cwd == root or root in cwd.parents:
            return IndexedRoot(root, member, workspace)
    for ancestor in (cwd, *cwd.parents):
        if (ancestor / "trufflepig.workspace.toml").is_file():
            return IndexedRoot(ancestor, ancestor.name, ancestor.name)
    return None


def session_context(found: IndexedRoot) -> str:
    """SessionStart guidance for a checkout that trufflepig indexes."""
    return (
        f"This checkout ({found.member}, trufflepig workspace `{found.workspace}`) is indexed by "
        "trufflepig. For code search use `trufflepig-agent` via Bash instead of grep/rg/find:\n"
        "- definition body in one call: `trufflepig-agent show 'sym:Name'`; locations: `search 'sym:Name'`\n"
        "- references: `trufflepig-agent refs name`; regex: `trufflepig-agent search 're:a|b lang:rust'`\n"
        "- file outline with functions: `trufflepig-agent map path/to/file.rs`; files: `search 'name kind:file'`\n"
        "- concepts: `trufflepig-agent search 'few discriminating words'`\n"
        "Hits show a matching line; searches cover this checkout first and widen when it has no hits. "
        "Run it as its own Bash call (no `; echo`, no `| head`) so its exit code and footer are authoritative. "
        "grep is still right for logs, command output, and files outside this checkout. "
        "When briefing subagents, tell them to search with trufflepig-agent, not grep."
    )
