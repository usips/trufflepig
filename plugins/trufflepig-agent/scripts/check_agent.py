#!/usr/bin/env python3
"""Exercise search and verified source delivery through the real agent wrapper."""
import argparse
from pathlib import Path
import re
import subprocess
import sys


def check(wrapper: str, root: Path) -> None:
    def call(*args: str) -> str:
        result = subprocess.run([wrapper, "--no-workspace", "--root", str(root), "--budget", "1200", *args], cwd=root,
                                capture_output=True, text=True, timeout=60)
        if result.returncode:
            raise RuntimeError(result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}")
        return result.stdout

    output = call("search", "file:")
    match = re.search(r"^([0-9a-f]{32}:\d+)\t", output, re.M)
    if match is None:
        raise RuntimeError("search returned no readable handle; check indexing and root coverage: " + output[:600])
    source = call("show", match[1])
    if "verified: true" not in source:
        raise RuntimeError("show did not return verified source: " + source[:600])
    print(f"search → show verified in {root}; wrapper={wrapper}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--wrapper", default="trufflepig-agent")
    args = parser.parse_args()
    try:
        check(args.wrapper, args.root.resolve())
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f"Trufflepig smoke check failed: {error}\n"
              "Check PATH and the index; upgrade/restart router AND member daemons for flag errors.\n"
              "For sandbox failures, verify the shared spool heartbeat and directory permissions.\n"
              "See plugins/trufflepig-agent/README.md for recovery.", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
