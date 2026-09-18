"""Resolve omp native discovery directories, including profiles and overrides."""
import os
from pathlib import Path
import re


def omp_agent_dir(explicit: Path | None = None) -> Path:
    if explicit is not None:
        return explicit.expanduser().absolute()
    profile = os.environ.get("OMP_PROFILE", os.environ.get("PI_PROFILE", "")).strip()
    root = Path.home() / (os.environ.get("PI_CONFIG_DIR") or ".omp")
    if profile and profile != "default":
        if not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,63}", profile) or profile.endswith("."):
            raise ValueError("invalid omp profile name")
        return root / "profiles" / profile / "agent"
    override = os.environ.get("PI_CODING_AGENT_DIR")
    return Path(override).expanduser().absolute() if override else root / "agent"
