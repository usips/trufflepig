"""Reject service paths that disagree with the existing router environment or pin."""
from __future__ import annotations

import json
from pathlib import Path
import shlex
from typing import NamedTuple


class RouterUnitPins(NamedTuple):
    database: Path | None
    runtime: Path | None


def absolute_service_path(value: object, source: Path) -> Path:
    if not isinstance(value, str) or not value or not Path(value).is_absolute():
        raise ValueError(f"invalid board service pin in {source}; expected an absolute path")
    return Path(value)


def environment_values(raw_value: str, unit: Path) -> list[str]:
    try:
        values = [json.loads(raw_value)] if raw_value.startswith('"') and raw_value.endswith('"') else shlex.split(raw_value)
    except ValueError as error:
        raise ValueError(f"cannot parse service environment in {unit}") from error
    if not all(isinstance(value, str) for value in values):
        raise ValueError(f"cannot parse service environment in {unit}")
    return values


def router_unit_pins(config_home: Path) -> RouterUnitPins:
    unit = config_home / "systemd/user/trufflepig-system.service"
    if not unit.exists():
        return RouterUnitPins(None, None)
    environment: dict[str, str] = {}
    unsets: list[str] = []
    files = [unit, *sorted(unit.with_name(unit.name + ".d").glob("*.conf"))]
    for source in files:
        section = ""
        for raw in source.read_text().splitlines():
            line = raw.strip()
            if line.startswith("[") and line.endswith("]"):
                section = line[1:-1]
            if section != "Service":
                continue
            directive, separator, raw_value = line.partition("=")
            if not separator:
                continue
            directive, raw_value = directive.strip(), raw_value.strip()
            if directive == "EnvironmentFile" and raw_value:
                raise ValueError(f"cannot validate board service EnvironmentFile in {source}; resolve the router pin first")
            if directive == "Environment":
                if not raw_value:
                    environment.clear()
                    continue
                for value in environment_values(raw_value, source):
                    name, separator, value = value.partition("=")
                    if separator:
                        environment[name] = value
            if directive == "UnsetEnvironment":
                if not raw_value:
                    unsets.clear()
                else:
                    unsets.extend(environment_values(raw_value, source))
    # systemd applies all removals after all Environment and drop-in assignments.
    for unset in unsets:
        name, separator, value = unset.partition("=")
        if not separator or environment.get(name) == value:
            environment.pop(name, None)
    pins = []
    for name in ("TRUFFLEPIG_BOARD_DB", "TRUFFLEPIG_SYSTEM_DIR"):
        value = environment.get(name)
        if value is not None:
            if "%" in value.replace("%%", ""):
                raise ValueError(f"unresolved board service specifier in {unit}; resolve the router pin first")
            pins.append(absolute_service_path(value.replace("%%", "%"), unit))
        else:
            pins.append(None)
    return RouterUnitPins(*pins)


def router_database_pin(runtime: Path) -> Path | None:
    marker = runtime / "board-backend.json"
    try:
        text = marker.read_text()
    except FileNotFoundError:
        return None
    try:
        value = json.loads(text)
    except ValueError as error:
        raise ValueError(f"invalid router board pin in {marker}") from error
    if not isinstance(value, dict) or set(value) != {"database"}:
        raise ValueError(f"invalid router board pin in {marker}")
    return absolute_service_path(value["database"], marker)


def same_path(candidate: Path, pinned: Path) -> bool:
    if candidate == pinned:
        return True
    try:
        return candidate.resolve(strict=True) == pinned.resolve(strict=True)
    except OSError:
        return False


def validate_router_database(database: Path, runtime: Path, config_home: Path) -> None:
    pins = router_unit_pins(config_home)
    if pins.runtime is not None and not same_path(runtime, pins.runtime):
        raise ValueError(f"board runtime mismatch: router uses {pins.runtime}; set TRUFFLEPIG_SYSTEM_DIR={pins.runtime} before installing the service")
    pinned = router_database_pin(runtime) or pins.database
    if pinned is not None and not same_path(database, pinned):
        raise ValueError(f"board database mismatch: router uses {pinned}; set TRUFFLEPIG_BOARD_DB={pinned} before installing the service")
