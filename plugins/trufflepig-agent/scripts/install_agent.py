#!/usr/bin/env python3
"""Install shared agent commands, skills, and an optional user router service."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

PLUGIN = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PLUGIN / "bin"))
from trufflepig_runtime import runtime_config_path
from claude_install import claude_home, prepare_settings, write_settings


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


def service_text(binary: str, spool: Path | None) -> str:
    # systemd expands percent specifiers even inside quoted strings.
    quote = lambda value: json.dumps(str(value).replace("%", "%%"), ensure_ascii=False)
    text = (PLUGIN / "systemd/trufflepig-system.service").read_text()
    text = text.replace("@TRUFFLEPIG@", quote(binary))
    if spool is not None:
        text = text.replace("[Service]\n", "[Service]\nEnvironment=" +
                            quote(f"TRUFFLEPIG_SPOOL_DIR={spool}") + "\n")
    return text


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=Path.home() / ".local/bin")
    for flag in ("codex", "claude", "kimi", "muse", "kimi-hooks", "omp", "systemd"):
        parser.add_argument(f"--{flag}", action="store_true")
    parser.add_argument("--project", type=Path, action="append", default=[])
    parser.add_argument("--runtime-dir", type=Path, help="disk-backed, sandbox-writable agent runtime")
    parser.add_argument("--check", type=Path, metavar="ROOT", help="search and read a file through the installed wrapper")
    args = parser.parse_args()
    if not any((args.codex, args.claude, args.kimi, args.muse, args.kimi_hooks, args.omp, args.systemd, args.project, args.check)):
        args.kimi = args.muse = True
    if args.runtime_dir and not (args.codex or args.claude):
        parser.error("--runtime-dir requires --codex or --claude")

    skill = PLUGIN / "skills/trufflepig-code-search"
    links = [(PLUGIN / "bin" / name, args.bin / name)
             for name in ("trufflepig-agent", "trufflepig-audit")]
    links.extend((PLUGIN / "hooks" / source, args.bin / name) for source, name in (
        ("session-start.sh", "trufflepig-agent-session"), ("steer-search.py", "trufflepig-agent-steer")))
    kimi_home = Path(os.environ.get("KIMI_CODE_HOME") or Path.home() / ".kimi-code")
    if args.kimi:
        links.append((skill, kimi_home / "skills/trufflepig-code-search"))
    if args.codex:
        links.append((skill, Path.home() / ".agents/skills/trufflepig-code-search"))
    if args.claude:
        links.append((skill, claude_home() / "skills/trufflepig-code-search"))
    if args.omp:
        # omp's config root is ~/.omp (not governed by XDG_CONFIG_HOME).
        omp_home = Path.home() / ".omp"
        links.append((skill, omp_home / "agent/skills/trufflepig-code-search"))
        links.append((PLUGIN / "omp/session.ts", omp_home / "agent/extensions/trufflepig-session.ts"))

    links.extend((skill, root.absolute() / ".agents/skills/trufflepig-code-search") for root in args.project)
    for source, destination in links:
        check_link(source, destination)

    config_path = runtime_config_path()
    settings = {}
    if config_path.exists():
        settings = json.loads(config_path.read_text())
        if not isinstance(settings, dict):
            raise ValueError(f"expected an object in {config_path}")
    if args.codex or args.claude:
        runtime = (args.runtime_dir or Path(settings.get("runtime_dir") or
                   Path.home() / ".cache/codex-tmp/trufflepig-agent")).resolve()
        if runtime == Path("/tmp") or Path("/tmp") in runtime.parents:
            raise ValueError("--runtime-dir must use disk-backed storage outside /tmp")
        settings.update(runtime_dir=str(runtime), spool_dir=str(runtime / "spool"))
    spool = Path(settings["spool_dir"]) if settings.get("spool_dir") else None
    binary = shutil.which("trufflepig")
    if args.systemd and (not binary or not shutil.which("systemctl")):
        raise ValueError("--systemd requires trufflepig and systemctl on PATH")

    if args.claude:
        claude_settings_path = claude_home() / "settings.json"
        claude_settings = prepare_settings(claude_settings_path,
                                           args.bin / "trufflepig-claude-session", runtime)
    for source, destination in links:
        link(source, destination)
    if args.codex or args.claude:
        runtime.mkdir(parents=True, exist_ok=True, mode=0o700)
        config_path.parent.mkdir(parents=True, exist_ok=True)
        config_path.write_text(json.dumps(settings, indent=2) + "\n")
        print(f"agent runtime: {runtime}")
        if not args.systemd:
            print(f"router must use spool: {spool} (--systemd configures it)")
    if args.claude:
        write_settings(claude_settings_path, claude_settings)
        print(f"Claude SessionStart hook and runtime access: {claude_settings_path}")
    if not binary:
        print("warning: trufflepig missing from PATH; cargo install --path . --locked", file=sys.stderr)

    if args.kimi_hooks:
        config = kimi_home / "config.toml"
        text = config.read_text() if config.exists() else ""
        block = ("# trufflepig-agent hooks begin\n" + (PLUGIN / "kimi/hooks.toml").read_text() +
                 "# trufflepig-agent hooks end\n")
        pattern = r"# trufflepig-agent hooks begin\n.*?# trufflepig-agent hooks end\n"
        text = re.sub(pattern, lambda _: block, text, count=1, flags=re.S) if re.search(pattern, text, re.S) else text + "\n" + block
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text(text)
    if args.muse:
        if shutil.which("muse"):
            subprocess.run(["muse", "skills", "install", str(skill), "--scope", "user", "--force", "--json"], check=True)
        else:
            print("muse not found; skipped", file=sys.stderr)
    if args.systemd:
        config_home = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
        unit = config_home / "systemd/user/trufflepig-system.service"
        unit.parent.mkdir(parents=True, exist_ok=True)
        unit.write_text(service_text(binary, spool))
        subprocess.run(["systemctl", "--user", "daemon-reload"], check=True)
        # Stop uses the newly loaded control-group policy, including old children.
        subprocess.run(["systemctl", "--user", "stop", "trufflepig-system.service"], check=True)
        subprocess.run([binary, "system", "stop"], capture_output=True)
        subprocess.run(["systemctl", "--user", "enable", "--now", "trufflepig-system.service"], check=True)
        print(f"systemd user service: {unit}")
    if args.check:
        subprocess.run([sys.executable, str(PLUGIN / "scripts/check_agent.py"),
                        "--wrapper", str(args.bin / "trufflepig-agent"), str(args.check.absolute())], check=True)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"trufflepig-agent install: {error}", file=sys.stderr)
        sys.exit(2)
