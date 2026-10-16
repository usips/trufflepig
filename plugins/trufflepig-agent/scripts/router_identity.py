"""Best-effort checks that a ready router still owns its installed executable."""
from __future__ import annotations

import os
from pathlib import Path
import socket

_LISTENING_FLAG = 0x10000


class RouterIdentityConflict(ValueError):
    """More than one matching process owns the router listener."""


def _listener_inodes(runtime: Path, proc_root: Path) -> set[str] | None:
    """Return visible listening stream inodes for this exact runtime socket."""
    try:
        lines = os.fsdecode((proc_root / "net/unix").read_bytes()).splitlines()
    except OSError:
        return None
    endpoint = str(runtime / "daemon.sock")
    inodes: set[str] = set()
    for line in lines[1:]:
        fields = line.split(maxsplit=7)
        if len(fields) != 8 or fields[7] != endpoint:
            continue
        try:
            flags = int(fields[3], 16)
            kind = int(fields[4], 16)
        except ValueError:
            continue
        if kind == socket.SOCK_STREAM and flags & _LISTENING_FLAG:
            inodes.add(fields[6])
    return inodes


def _process_uid(process: Path) -> int | None:
    try:
        lines = (process / "status").read_text(encoding="ascii", errors="replace").splitlines()
    except OSError:
        return None
    for line in lines:
        if line.startswith("Uid:"):
            try:
                return int(line.split()[1])
            except (IndexError, ValueError):
                return None
    return None


def _process_argv(process: Path) -> list[bytes] | None:
    try:
        return (process / "cmdline").read_bytes().split(b"\0")
    except OSError:
        return None


def _owns_socket(process: Path, inodes: set[str]) -> bool:
    try:
        descriptors = list((process / "fd").iterdir())
    except OSError:
        return False
    for descriptor in descriptors:
        try:
            target = os.readlink(descriptor)
        except OSError:
            continue
        if any(target == f"socket:[{inode}]" for inode in inodes):
            return True
    return False


def _visible_owners(proc_root: Path, inodes: set[str], uid: int) -> dict[int, str]:
    owners: dict[int, str] = {}
    try:
        processes = list(proc_root.iterdir())
    except OSError:
        return owners
    for process in processes:
        if not process.name.isdecimal():
            continue
        pid = int(process.name)
        if _process_uid(process) != uid:
            continue
        argv = _process_argv(process)
        if argv is None or b"system-serve" not in argv:
            continue
        if not _owns_socket(process, inodes):
            continue
        try:
            executable = os.readlink(process / "exe")
        except OSError:
            executable = "<unavailable>"
        owners[pid] = executable
    return owners


def _replaced_error(runtime: Path, process_id: object, executable: object, advice: str) -> ValueError:
    return ValueError(
        f"router process {process_id} uses replaced executable {executable} for runtime {runtime}; {advice}"
    )


def require_router_exclusive(runtime: Path, advice: str, *,
                             proc_root: Path = Path("/proc")) -> dict[int, str]:
    """Return visible exact listener owners, rejecting multiple owners."""
    getuid = getattr(os, "getuid", None)
    inodes = _listener_inodes(runtime, proc_root)
    if getuid is None or not inodes:
        return {}
    owners = _visible_owners(proc_root, inodes, getuid())
    if len(owners) > 1:
        processes = ", ".join(f"PID {pid} ({path})" for pid, path in sorted(owners.items()))
        pids = " ".join(str(pid) for pid in sorted(owners))
        raise RouterIdentityConflict(
            f"multiple router processes own runtime {runtime}: {processes}; "
            "stop the owning user systemd unit first (for a managed router, run "
            "`systemctl --user stop trufflepig-system.service`), then terminate "
            f"the listed remaining processes with `kill -- {pids}`; {advice}"
        )
    return owners


def require_router_identity(runtime: Path, status: dict, advice: str, *,
                            proc_root: Path = Path("/proc")) -> None:
    """Reject a replaced router or multiple `system-serve` owners of its listener."""
    owners = require_router_exclusive(runtime, advice, proc_root=proc_root)
    executable = status.get("executable", "<unreported>")
    process_id = status.get("process_id", "<unreported>")
    if status.get("executable_replaced") is True or (
            isinstance(executable, str) and executable.endswith(" (deleted)")):
        raise _replaced_error(runtime, process_id, executable, advice)
    if owners:
        pid, path = next(iter(owners.items()))
        if path.endswith(" (deleted)"):
            raise _replaced_error(runtime, pid, path, advice)
