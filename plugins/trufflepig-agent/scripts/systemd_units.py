"""User systemd unit rendering, activation, and failure rollback for the router/board services."""
from __future__ import annotations

import json
import os
from pathlib import Path
import pwd
import subprocess

PLUGIN = Path(__file__).resolve().parents[1]


def board_database_path() -> Path:
    override = os.environ.get("TRUFFLEPIG_BOARD_DB")
    if override is not None:
        if not override:
            raise ValueError("TRUFFLEPIG_BOARD_DB must not be empty")
        if not Path(override).is_absolute():
            raise ValueError("TRUFFLEPIG_BOARD_DB must be absolute")
        return Path(override)
    data_home = os.environ.get("XDG_DATA_HOME")
    base = Path(data_home) if data_home and Path(data_home).is_absolute() \
        else Path(pwd.getpwuid(os.getuid()).pw_dir) / ".local/share"
    return (base / "trufflepig/board.sqlite3").absolute()


def system_runtime_path(binary: str) -> Path:
    """Resolve the router runtime dir through the binary, its single owner."""
    try:
        result = subprocess.run([binary, "system", "dir"], stdin=subprocess.DEVNULL,
                                capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ValueError(f"{binary} cannot resolve the system runtime dir: {error}") from error
    lines = result.stdout.splitlines()
    if result.returncode or len(lines) != 1 or not Path(lines[0]).is_absolute():
        raise ValueError(f"{binary} system dir failed: {result.stderr.strip() or result.stdout.strip()}")
    return Path(lines[0])


def render_service(unit_name: str, binary: str, spool: Path | None = None) -> str:
    # systemd expands percent specifiers even inside quoted strings.
    quote = lambda value: json.dumps(str(value).replace("%", "%%"), ensure_ascii=False)
    text = (PLUGIN / "systemd" / unit_name).read_text()
    text = text.replace("@TRUFFLEPIG@", quote(binary))
    environment = [("TRUFFLEPIG_BOARD_DB", board_database_path()),
                   ("TRUFFLEPIG_SYSTEM_DIR", system_runtime_path(binary))]
    if spool is not None:
        environment.append(("TRUFFLEPIG_SPOOL_DIR", spool))
    lines = "".join("Environment=" + quote(f"{name}={value}") + "\n" for name, value in environment)
    return text.replace("[Service]\n", "[Service]\n" + lines)


def service_text(binary: str, spool: Path | None) -> str:
    return render_service("trufflepig-system.service", binary, spool)


def unit_active(name: str) -> bool:
    """True while a user unit is up: active, activating, or reloading."""
    result = subprocess.run(["systemctl", "--user", "is-active", name],
                            capture_output=True, text=True)
    return result.stdout.strip() in ("active", "activating", "reloading")


def apply_units(binary: str, unit_directory: Path, router_runtime: Path, managed_router: bool,
                install_router: bool, install_board: bool,
                unit_text: str | None, board_unit_text: str | None, check_router) -> None:
    """Write and enable the requested user units; on failure restore the prior unit state."""
    router_was_active = unit_active("trufflepig-system.service")
    board_was_active = unit_active("trufflepig-board.service")
    unit_directory.mkdir(parents=True, exist_ok=True)
    previous_units = {}
    for name, text in (("trufflepig-system.service", unit_text), ("trufflepig-board.service", board_unit_text)):
        if text is not None:
            try:
                previous_units[name] = (unit_directory / name).read_text()
            except FileNotFoundError:
                previous_units[name] = None
            (unit_directory / name).write_text(text)
    try:
        subprocess.run(["systemctl", "--user", "daemon-reload"], check=True)
        if install_router:
            # Stop uses the newly loaded control-group policy, including old children.
            subprocess.run(["systemctl", "--user", "stop", "trufflepig-system.service"], check=True)
            subprocess.run([binary, "system", "stop"], capture_output=True)
            subprocess.run(["systemctl", "--user", "enable", "--now", "trufflepig-system.service"], check=True)
            if board_was_active:
                # The router stop propagates to the board through PartOf, so the
                # board is stopped by now and try-restart would leave it down;
                # start revives it. An inactive board stays untouched.
                subprocess.run(["systemctl", "--user", "start", "trufflepig-board.service"], check=True)
            print(f"systemd user service: {unit_directory / 'trufflepig-system.service'}")
        if install_board:
            subprocess.run(["systemctl", "--user", "stop", "trufflepig-board.service"], check=True)
            if not install_router and managed_router:
                # The board shares the router's database; both must run the same binary.
                subprocess.run(["systemctl", "--user", "restart", "trufflepig-system.service"], check=True)
            subprocess.run(["systemctl", "--user", "enable", "--now", "trufflepig-board.service"], check=True)
            if managed_router:
                # The restarted router must run the current binary on the migrated database.
                check_router(router_runtime)
            print(f"systemd board service: {unit_directory / 'trufflepig-board.service'}")
            print("board bootstrap URL: run `trufflepig board web`")
    except (OSError, ValueError, subprocess.CalledProcessError):
        # Undo this run's unit state: disable and remove units it created,
        # stop units that were inactive before, and start the units that
        # were active before on their restored unit files.
        for name, previous in previous_units.items():
            if previous is None:
                subprocess.run(["systemctl", "--user", "disable", "--now", name], check=True)
                (unit_directory / name).unlink(missing_ok=True)
        for name, was_active in (("trufflepig-system.service", router_was_active),
                                ("trufflepig-board.service", board_was_active)):
            if not was_active and (unit_directory / name).exists():
                subprocess.run(["systemctl", "--user", "stop", name], check=True)
        for name, previous in previous_units.items():
            if previous is not None:
                (unit_directory / name).write_text(previous)
        subprocess.run(["systemctl", "--user", "daemon-reload"], check=True)
        for name, was_active in (("trufflepig-system.service", router_was_active),
                                ("trufflepig-board.service", board_was_active)):
            if was_active:
                subprocess.run(["systemctl", "--user", "start", name], check=True)
        raise
