"""Destination planning and checked symlinks for agent commands and skills."""
from __future__ import annotations

from argparse import Namespace
from dataclasses import dataclass
import os
from pathlib import Path

from claude_install import claude_home, plugin_enabled
from omp_install import omp_agent_dir


@dataclass(frozen=True)
class AgentLinkPlan:
    skills: tuple[Path, ...]
    links: list[tuple[Path, Path]]
    kimi_home: Path
    claude_plugin: bool

    @classmethod
    def prepare(cls, plugin: Path, args: Namespace) -> AgentLinkPlan:
        skills = tuple(plugin / "skills" / name
                       for name in ("trufflepig-code-search", "trufflepig-plan-board"))
        links = [(plugin / "bin" / name, args.bin / name)
                 for name in ("trufflepig-agent", "trufflepig-audit")]
        links.extend((plugin / "hooks" / source, args.bin / name) for source, name in (
            ("session-start.sh", "trufflepig-agent-session"),
            ("steer-search.py", "trufflepig-agent-steer")))
        kimi_home = Path(os.environ.get("KIMI_CODE_HOME") or Path.home() / ".kimi-code")
        if args.kimi:
            links.extend((skill, kimi_home / "skills" / skill.name) for skill in skills)
        if args.codex:
            links.extend((skill, Path.home() / ".agents/skills" / skill.name) for skill in skills)
        if args.gemini:
            gemini_home = Path(os.environ.get("GEMINI_CLI_HOME") or Path.home()).expanduser().absolute()
            links.extend((skill, gemini_home / ".gemini/skills" / skill.name) for skill in skills)
        if args.opencode:
            config_home = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
            opencode_dir = config_home.expanduser().absolute() / "opencode"
            links.extend((skill, opencode_dir / "skills" / skill.name) for skill in skills)
            links.append((plugin / "opencode/session.ts", opencode_dir / "plugins/trufflepig-session.ts"))
        if args.grok:
            grok_home = Path(os.environ.get("GROK_HOME") or Path.home() / ".grok").expanduser().absolute()
            links.extend((skill, grok_home / "skills" / skill.name) for skill in skills)
        claude_plugin = args.claude and plugin_enabled(claude_home() / "settings.json")
        if args.claude and not claude_plugin:
            links.extend((skill, claude_home() / "skills" / skill.name) for skill in skills)
            links.append((plugin / "hooks/claude-session.py", args.bin / "trufflepig-claude-session"))
        if args.claude:
            links.extend((plugin / "hooks" / source, args.bin / destination) for source, destination in (
                ("claude-hooks.py", "trufflepig-claude-hooks"),
                ("trufflepig_identity.py", "trufflepig_identity.py")))
            links.append((plugin / "bin/trufflepig_board_notices.py",
                          args.bin / "trufflepig_board_notices.py"))
        if args.omp:
            agent_dir = omp_agent_dir(args.omp_agent_dir)
            links.extend((skill, agent_dir / "skills" / skill.name) for skill in skills)
            links.append((plugin / "omp/session.ts", agent_dir / "extensions/trufflepig-session.ts"))
        links.extend((skill, root.absolute() / ".agents/skills" / skill.name)
                     for root in args.project for skill in skills)
        for source, destination in links:
            check_link(source, destination)
        return cls(skills, links, kimi_home, claude_plugin)


def check_link(source: Path, destination: Path) -> None:
    if destination.is_symlink() and destination.resolve() == source.resolve():
        return
    if destination.exists() or destination.is_symlink():
        raise ValueError(f"unmanaged destination exists: {destination}; move it before installing")


def link(source: Path, destination: Path) -> None:
    check_link(source, destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    if not destination.is_symlink():
        destination.symlink_to(source, target_is_directory=source.is_dir())
    print(f"linked {destination}")
