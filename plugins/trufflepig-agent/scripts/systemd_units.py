"""User systemd unit rendering, activation, and failure rollback for the router/board services."""
from __future__ import annotations

import json
import os
from pathlib import Path
import pwd
import subprocess

PLUGIN = Path(__file__).resolve().parents[1]
SUPPORTED_UNIT_STATES = frozenset(("enabled", "enabled-runtime", "disabled", "static", "not-found"))
MASKED_UNIT_STATES = frozenset(("masked", "masked-runtime"))
ENABLED_UNIT_STATES = frozenset(("enabled", "enabled-runtime"))


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
                            capture_output=True, text=True, check=False)
    return result.stdout.strip() in ("active", "activating", "reloading")


def unit_enablement(name: str) -> str:
    """Return a supported systemd enablement state or fail before mutation."""
    result = subprocess.run(["systemctl", "--user", "is-enabled", name],
                            capture_output=True, text=True, check=False)
    state = result.stdout.strip()
    if state in MASKED_UNIT_STATES:
        raise ValueError(f"cannot safely update {name}: systemd reports {state}")
    if state not in SUPPORTED_UNIT_STATES:
        detail = state or result.stderr.strip() or f"exit status {result.returncode}"
        raise ValueError(f"cannot safely update {name}: unsupported systemd state {detail}")
    return state


class UnitRollback:
    """Attempt each recovery operation independently and retain its diagnostic."""

    def __init__(self):
        self.errors = []

    def attempt(self, description, operation, *args, **kwargs):
        try:
            return operation(*args, **kwargs)
        except Exception as error:
            self.errors.append(f"{description}: {error}")
            return None

    def systemctl(self, *args) -> bool:
        command = ["systemctl", "--user", *args]
        description = " ".join(command)
        result = self.attempt(description, subprocess.run, command, check=False,
                              capture_output=True, text=True)
        if result is None:
            return False
        if result.returncode:
            detail = result.stderr.strip() or result.stdout.strip() or f"exit status {result.returncode}"
            self.errors.append(f"{description}: {detail}")
            return False
        return True

    def restore_enablement(self, name: str, previous: str, was_active: bool) -> bool:
        """Restore links; report whether --now also stopped an initially inactive unit."""
        if previous == "not-found":
            options = () if was_active else ("--now",)
            stopped = self.systemctl("disable", *options, name)
            return stopped and not was_active
        current = self.attempt(f"read enablement for {name}", unit_enablement, name)
        if current == previous:
            return False
        stopped = False
        if current in ENABLED_UNIT_STATES or current is None:
            options = ("--runtime",) if current == "enabled-runtime" else ()
            stop = not was_active and previous not in ENABLED_UNIT_STATES
            stopped = self.systemctl("disable", *options, *(("--now",) if stop else ()), name) and stop
        if previous in ENABLED_UNIT_STATES:
            options = ("--runtime",) if previous == "enabled-runtime" else ()
            self.systemctl("enable", *options, name)
        restored = self.attempt(f"verify enablement for {name}", unit_enablement, name)
        if restored is not None and restored != previous:
            self.errors.append(f"cannot restore {name} to systemd state {previous}: reports {restored}")
        return stopped

    def restore(self, unit_directory, previous_units, previous_active,
                previous_enablement, activation_attempts):
        for name, previous in previous_units.items():
            path = unit_directory / name
            if previous is None:
                self.attempt(f"remove {path}", path.unlink, missing_ok=True)
            else:
                self.attempt(f"restore {path}", path.write_text, previous)
        self.systemctl("daemon-reload")
        for name, was_active in previous_active.items():
            if was_active:
                self.systemctl("restart", name)
        stopped_units = set()
        for name in activation_attempts:
            if self.restore_enablement(name, previous_enablement[name], previous_active[name]):
                stopped_units.add(name)
        for name, was_active in previous_active.items():
            if not was_active and name not in stopped_units:
                exists = self.attempt(f"inspect {unit_directory / name}", (unit_directory / name).exists)
                if name in activation_attempts or exists:
                    self.systemctl("stop", name)


def apply_units(binary: str, unit_directory: Path, router_runtime: Path, managed_router: bool,
                install_router: bool, install_board: bool,
                unit_text: str | None, board_unit_text: str | None, check_router) -> None:
    """Write and enable the requested user units; on failure restore the prior unit state."""
    previous_active = {name: unit_active(name)
                       for name in ("trufflepig-system.service", "trufflepig-board.service")}
    board_was_active = previous_active["trufflepig-board.service"]
    previous_units = {}
    for name, text in (("trufflepig-system.service", unit_text), ("trufflepig-board.service", board_unit_text)):
        if text is not None:
            try:
                previous_units[name] = (unit_directory / name).read_text()
            except FileNotFoundError:
                previous_units[name] = None
    previous_enablement = {}
    for name, requested in (("trufflepig-system.service", install_router),
                            ("trufflepig-board.service", install_board)):
        if requested:
            previous_enablement[name] = (unit_enablement(name) if (unit_directory / name).exists()
                                         else "not-found")
    activation_attempts = []
    try:
        unit_directory.mkdir(parents=True, exist_ok=True)
        for name, text in (("trufflepig-system.service", unit_text),
                           ("trufflepig-board.service", board_unit_text)):
            if text is not None:
                (unit_directory / name).write_text(text)
        subprocess.run(["systemctl", "--user", "daemon-reload"], check=True)
        if install_router:
            # Stop uses the newly loaded control-group policy, including old children.
            subprocess.run(["systemctl", "--user", "stop", "trufflepig-system.service"], check=True)
            subprocess.run([binary, "system", "stop"], capture_output=True)
            activation_attempts.append("trufflepig-system.service")
            subprocess.run(["systemctl", "--user", "enable", "--now", "trufflepig-system.service"], check=True)
            # A successful systemctl command does not prove the router finished
            # starting on the migrated database.
            check_router(router_runtime, managed_restart=True)
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
                check_router(router_runtime, managed_restart=True)
            activation_attempts.append("trufflepig-board.service")
            subprocess.run(["systemctl", "--user", "enable", "--now", "trufflepig-board.service"], check=True)
            print(f"systemd board service: {unit_directory / 'trufflepig-board.service'}")
            print("board bootstrap URL: run `trufflepig board web`")
    except Exception as error:
        rollback = UnitRollback()
        rollback.restore(unit_directory, previous_units, previous_active,
                         previous_enablement, activation_attempts)
        error.rollback_errors = tuple(rollback.errors)
        raise
