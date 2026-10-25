"""CPU layout of the router units: the daemon slice, legacy drop-ins, and runtime weights."""
from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys

PLUGIN = Path(__file__).resolve().parents[1]
DAEMON_SLICE = "app-trufflepig.slice"
PRIORITY_UNITS = ("trufflepig-system.service", "trufflepig-board.service")
# Earlier setups lowered whole units with this drop-in; it would also lower the router.
LEGACY_DROP_IN = "10-nice.conf"
PRIORITY_KEYS = frozenset(("Nice", "CPUWeight", "StartupCPUWeight", "CPUQuota", "IOWeight",
                           "StartupIOWeight", "IOSchedulingClass", "IOSchedulingPriority",
                           "CPUSchedulingPolicy", "CPUSchedulingPriority"))


def slice_text() -> str:
    return (PLUGIN / "systemd" / DAEMON_SLICE).read_text()


def priority_only(text: str) -> bool:
    """Whether a drop-in only sets [Service] scheduling and weight keys."""
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line[0] in "#;" or line == "[Service]":
            continue
        key, separator, _ = line.partition("=")
        if not separator or key.strip() not in PRIORITY_KEYS:
            return False
    return True


def legacy_drop_ins(unit_directory: Path) -> dict[str, str]:
    """Owned legacy priority drop-ins by path relative to the unit directory."""
    owned = {}
    for unit in PRIORITY_UNITS:
        name = f"{unit}.d/{LEGACY_DROP_IN}"
        path = unit_directory / name
        if not path.is_file():
            continue
        text = path.read_text()
        if priority_only(text):
            owned[name] = text
        else:
            print(f"warning: {path} sets more than priority; left in place, but it overrides "
                  f"the router/daemon CPU split", file=sys.stderr)
    return owned


def runtime_weights() -> dict[str, str]:
    """`systemctl set-property --runtime` CPU weights that override the units until reboot."""
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime or not Path(runtime).is_absolute():
        return {}
    weights = {}
    for unit in PRIORITY_UNITS:
        path = Path(runtime) / "systemd/user.control" / f"{unit}.d" / "50-CPUWeight.conf"
        try:
            text = path.read_text()
        except OSError:
            continue
        values = [line.partition("=")[2].strip() for line in text.splitlines()
                  if line.strip().startswith("CPUWeight=")]
        if values and values[-1]:
            weights[unit] = values[-1]
    return weights


def reset_runtime_weights(weights: dict[str, str]) -> None:
    """Return each overridden unit to the weight its unit file declares."""
    for unit in weights:
        subprocess.run(["systemctl", "--user", "set-property", "--runtime", unit, "CPUWeight="],
                       check=True)
