#!/usr/bin/env python3
"""Install shared agent commands, skills, and optional user router/board services."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import pwd  # tests patch install_agent.pwd.getpwuid for the passwd-home fallback
import re
import shutil
import socket
import stat
import subprocess
import sys
import time
import uuid

PLUGIN = Path(__file__).resolve().parents[1]
SKILLS = ("trufflepig-code-search", "trufflepig-plan-board")
BOARD_API = 6
BOARD_SCHEMA = 8
ROUTER_READY_TIMEOUT_SECONDS = 10
sys.path.insert(0, str(PLUGIN / "bin"))
from trufflepig_runtime import runtime_config_path
from omp_install import omp_agent_dir
from claude_install import claude_home, plugin_enabled, prepare_settings, write_settings
from board_service_pin import validate_router_database
from systemd_units import (apply_units, board_database_path, render_service, service_text,
                           system_runtime_path)


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


def require_board_api(binary: str) -> None:
    advice = f"{binary} must support board API {BOARD_API}; reinstall the current trufflepig binary before --systemd/--board"
    try:
        result = subprocess.run([binary, "--board-api-version"], stdin=subprocess.DEVNULL,
                                capture_output=True, timeout=5)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ValueError(advice) from error
    if result.returncode or result.stdout != f"{BOARD_API}\n".encode() or result.stderr:
        raise ValueError(advice)


def read_exact(connection: socket.socket, count: int, deadline: float) -> bytes:
    chunks = []
    while count > 0:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("router status deadline expired")
        connection.settimeout(remaining)
        chunk = connection.recv(count)
        if not chunk:
            raise OSError("router closed the status reply")
        chunks.append(chunk)
        count -= len(chunk)
    return b"".join(chunks)


def router_status(runtime: Path, deadline: float) -> dict | None:
    """Read one router status reply without exceeding its shared deadline."""
    if time.monotonic() >= deadline:
        return None
    if not (runtime / "daemon.sock").is_socket():
        return None
    request = json.dumps({
        "command": "Arguments",
        "arguments": {
            "context": {"request_id": str(uuid.uuid4()),
                        "created_unix_micros": int(time.time() * 1_000_000),
                        "session": None, "client": None},
            "args": ["system", "status"],
        },
    }).encode()
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            connection.settimeout(remaining)
            connection.connect(str(runtime / "daemon.sock"))
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            connection.settimeout(remaining)
            connection.sendall(len(request).to_bytes(4, "big") + request)
            length = int.from_bytes(read_exact(connection, 4, deadline), "big")
            if length > 4 * 1024 * 1024:
                return None
            reply = json.loads(read_exact(connection, length, deadline))
        if not isinstance(reply, dict) or reply.get("status") != "success":
            return None
        output = reply.get("output")
        if not isinstance(output, str):
            return None
        status = json.loads(output)
    except (OSError, ValueError):
        return None
    if time.monotonic() >= deadline:
        return None
    return status if isinstance(status, dict) else None


def router_endpoint_present(runtime: Path) -> bool:
    """Distinguish a missing endpoint from a present but unusable socket path."""
    socket_path = runtime / "daemon.sock"
    try:
        socket_path.lstat()
    except FileNotFoundError:
        return False
    except OSError as error:
        raise ValueError(f"cannot inspect router endpoint {socket_path}: {error}") from error
    return True


def absent_database_path(database: Path) -> bool:
    """Return true only when the configured database path is genuinely absent."""
    try:
        database.lstat()
    except FileNotFoundError:
        # A missing parent is valid on a fresh profile, but a dangling symlink
        # anywhere in that parent chain is a broken configuration.
        for parent in database.parents:
            try:
                metadata = parent.lstat()
            except FileNotFoundError:
                continue
            except OSError as error:
                raise ValueError(f"cannot inspect board database parent {parent}: {error}") from error
            if stat.S_ISLNK(metadata.st_mode):
                try:
                    parent.resolve(strict=True)
                except OSError as error:
                    raise ValueError(f"cannot resolve board database parent {parent}: {error}") from error
        return True
    except OSError as error:
        raise ValueError(f"cannot inspect board database {database}: {error}") from error
    return False


def require_current_router(runtime: Path, *, database: Path | None = None,
                           allow_absent: bool = False,
                           timeout: float = ROUTER_READY_TIMEOUT_SECONDS) -> None:
    """Require API 6/schema 8, or a reported fresh database, from the router."""
    advice = "restart trufflepig-system.service with the current trufflepig binary"
    if allow_absent and not router_endpoint_present(runtime):
        return
    deadline = time.monotonic() + timeout
    last_status = None
    endpoint_seen = False
    while True:
        if time.monotonic() >= deadline:
            break
        endpoint_seen |= router_endpoint_present(runtime)
        status = router_status(runtime, deadline)
        endpoint_seen |= router_endpoint_present(runtime)
        if status is None:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            time.sleep(min(0.25, remaining))
            continue
        if time.monotonic() >= deadline:
            break
        last_status = status
        if status.get("status") != "ok":
            raise ValueError(f"router returned an incomplete status; {advice}")
        if "board_error" in status:
            raise ValueError(f"router reports board_error {status['board_error']}; {advice}")
        api = status.get("board_api")
        if api != BOARD_API:
            raise ValueError(f"router reports board_api {api}, expected {BOARD_API}; {advice}")
        supported = status.get("schema_supported")
        schema = status.get("schema_file")
        if supported != BOARD_SCHEMA:
            raise ValueError(f"router reports schema_supported {supported}, expected {BOARD_SCHEMA}; {advice}")
        if schema == BOARD_SCHEMA:
            return
        if schema is None:
            reported_database = status.get("board_db")
            if (database is None or not isinstance(reported_database, str)
                    or not Path(reported_database).is_absolute()):
                raise ValueError(f"router omitted schema_file without an absolute board_db path; {advice}")
            if reported_database != str(database):
                raise ValueError(f"router reports board_db {reported_database}, expected {database}; {advice}")
            if absent_database_path(database):
                return
            raise ValueError(f"router omitted schema_file for existing board database {database}; {advice}")
        if not isinstance(schema, int) or isinstance(schema, bool):
            raise ValueError(f"router reports invalid schema_file {schema!r}; {advice}")
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        time.sleep(min(0.25, remaining))
    if allow_absent and last_status is None and not endpoint_seen:
        return
    if last_status is None:
        detail = f"did not become ready with board API {BOARD_API} and schema {BOARD_SCHEMA}"
    else:
        detail = (f"reports schema_file {last_status.get('schema_file')}, "
                  f"expected {BOARD_SCHEMA}")
    raise ValueError(f"router {detail} within {timeout:g}s; {advice}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=Path.home() / ".local/bin")
    for flag in ("codex", "claude", "grok", "kimi", "muse", "kimi-hooks", "omp", "systemd", "board", "commands"):
        parser.add_argument(f"--{flag}", action="store_true")
    parser.add_argument("--omp-agent-dir", type=Path, help="explicit omp agent directory")
    parser.add_argument("--project", type=Path, action="append", default=[])
    parser.add_argument("--runtime-dir", type=Path, help="disk-backed, sandbox-writable agent runtime")
    parser.add_argument("--steer", choices=("off", "nudge", "block", "strict"),
                        help="search steering mode recorded for the selected harnesses")
    parser.add_argument("--check", type=Path, metavar="ROOT", help="search and read a file through the installed wrapper")
    args = parser.parse_args()
    if not any((args.codex, args.claude, args.grok, args.kimi, args.muse, args.kimi_hooks, args.omp, args.systemd, args.board,
                args.commands, args.project, args.check)):
        args.kimi = args.muse = True
    if args.omp_agent_dir and not args.omp:
        parser.error("--omp-agent-dir requires --omp")
    if args.runtime_dir and not (args.codex or args.claude or args.grok):
        parser.error("--runtime-dir requires --codex, --claude, or --grok")
    steer_harnesses = [name for name in ("claude", "kimi", "muse") if getattr(args, name)]
    if args.steer and not steer_harnesses:
        parser.error("--steer requires --claude, --kimi, or --muse")

    skills = tuple(PLUGIN / "skills" / name for name in SKILLS)
    links = [(PLUGIN / "bin" / name, args.bin / name)
             for name in ("trufflepig-agent", "trufflepig-audit")]
    links.extend((PLUGIN / "hooks" / source, args.bin / name) for source, name in (
        ("session-start.sh", "trufflepig-agent-session"), ("steer-search.py", "trufflepig-agent-steer")))
    kimi_home = Path(os.environ.get("KIMI_CODE_HOME") or Path.home() / ".kimi-code")
    if args.kimi:
        links.extend((skill, kimi_home / "skills" / skill.name) for skill in skills)
    if args.codex:
        links.extend((skill, Path.home() / ".agents/skills" / skill.name) for skill in skills)
    if args.grok:
        grok_home = Path(os.environ.get("GROK_HOME") or Path.home() / ".grok").expanduser().absolute()
        links.extend((skill, grok_home / "skills" / skill.name) for skill in skills)
    # An enabled Claude plugin already provides the skill and hooks.
    claude_plugin = args.claude and plugin_enabled(claude_home() / "settings.json")
    if args.claude and not claude_plugin:
        links.extend((skill, claude_home() / "skills" / skill.name) for skill in skills)
        links.append((PLUGIN / "hooks/claude-session.py", args.bin / "trufflepig-claude-session"))
    if args.omp:
        agent_dir = omp_agent_dir(args.omp_agent_dir)
        links.extend((skill, agent_dir / "skills" / skill.name) for skill in skills)
        links.append((PLUGIN / "omp/session.ts", agent_dir / "extensions/trufflepig-session.ts"))
    links.extend((skill, root.absolute() / ".agents/skills" / skill.name)
                 for root in args.project for skill in skills)
    for source, destination in links:
        check_link(source, destination)

    config_path = runtime_config_path()
    settings = {}
    if config_path.exists():
        settings = json.loads(config_path.read_text())
        if not isinstance(settings, dict):
            raise ValueError(f"expected an object in {config_path}")
    if args.codex or args.claude or args.grok:
        runtime = (args.runtime_dir or Path(settings.get("runtime_dir") or
                   Path.home() / ".cache/codex-tmp/trufflepig-agent")).resolve()
        if runtime == Path("/tmp") or Path("/tmp") in runtime.parents:
            raise ValueError("--runtime-dir must use disk-backed storage outside /tmp")
        settings.update(runtime_dir=str(runtime), spool_dir=str(runtime / "spool"))
    if args.steer:
        steer = settings.get("steer") if isinstance(settings.get("steer"), dict) else {}
        settings["steer"] = {**steer, **{name: args.steer for name in steer_harnesses}}
    spool = Path(settings["spool_dir"]) if settings.get("spool_dir") else None
    binary = shutil.which("trufflepig")
    if (args.systemd or args.board) and (not binary or not shutil.which("systemctl")):
        raise ValueError("--systemd/--board requires trufflepig and systemctl on PATH")
    if binary:
        binary = str(Path(binary).absolute())
    if args.systemd or args.board:
        config_home = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
        unit_directory = config_home / "systemd/user"
        database = board_database_path()
        require_board_api(binary)
        router_runtime = system_runtime_path(binary)
        validate_router_database(database, router_runtime, config_home)
        # A managed router is restarted below, so check it after the restart;
        # only a router the install will not restart gates writing anything.
        managed_router = args.systemd or (unit_directory / "trufflepig-system.service").exists()
        if args.board and not managed_router:
            # The board shares the router's database: refuse before touching
            # anything when a listening router is stale. An unmanaged absent
            # router remains valid because this install does not restart it.
            require_current_router(router_runtime, database=database, allow_absent=True)
    unit_text = service_text(binary, spool) if args.systemd else None
    board_unit_text = render_service("trufflepig-board.service", binary) if args.board else None

    if args.claude:
        claude_settings_path = claude_home() / "settings.json"
        claude_settings = prepare_settings(
            claude_settings_path,
            None if claude_plugin else args.bin / "trufflepig-claude-session",
            runtime, None if claude_plugin else args.bin / "trufflepig-agent-steer")
    for source, destination in links:
        link(source, destination)
    if args.codex or args.claude or args.grok:
        runtime.mkdir(parents=True, exist_ok=True, mode=0o700)
        print(f"agent runtime: {runtime}")
        if not args.systemd:
            print(f"router must use spool: {spool} (--systemd configures it)")
    if args.codex or args.claude or args.grok or args.steer:
        config_path.parent.mkdir(parents=True, exist_ok=True)
        config_path.write_text(json.dumps(settings, indent=2) + "\n")
    if args.steer:
        print(f"search steering {args.steer}: {', '.join(steer_harnesses)}")
    if args.claude:
        write_settings(claude_settings_path, claude_settings)
        owner = "plugin trufflepig-agent (enabled)" if claude_plugin else "settings"
        print(f"Claude skill and hooks via {owner}; wrapper permission and runtime access: "
              f"{claude_settings_path}")
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
            for skill in skills:
                subprocess.run(["muse", "skills", "install", str(skill), "--scope", "user", "--force", "--json"], check=True)
        else:
            print("muse not found; skipped", file=sys.stderr)
    if args.systemd or args.board:
        def check_router(runtime: Path) -> None:
            require_current_router(runtime, database=database)

        apply_units(binary, unit_directory, router_runtime, managed_router,
                    args.systemd, args.board, unit_text, board_unit_text, check_router)
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
