#!/usr/bin/env python3
"""Bounded, read-only corpus CLI smoke; outputs JSON and removes index caches."""

import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import subprocess
import tempfile
import time


WORKSPACE = Path(__file__).resolve().parents[2]
BINARY = Path(os.environ.get("TRUFFLEPIG_SMOKE_BINARY", WORKSPACE / "target/debug/trufflepig"))
SCRATCH = Path("/home/josh/.cache/codex-tmp")
CORPORA = {"lunatic": "isAllowedOrdinaryContactAction", "tgstation": "Recreate_MC"}


def git(root, *args):
    return subprocess.check_output(
        ["git", "--no-optional-locks", "-C", str(root), *args], text=True
    ).strip()


def disk_bytes(cache):
    files = [p for p in cache.rglob("*") if p.is_file()]
    return {
        "apparent_bytes": sum(p.stat().st_size for p in files),
        "allocated_bytes": sum(p.stat().st_blocks * 512 for p in files),
    }


def measure(root, cache, words, timeout=120):
    argv = [str(BINARY), "--root", str(root), "--cache", str(cache),
            "--no-daemon", "--json", *words]
    before = disk_bytes(cache)
    output = cache / "command-output.json"
    errors = cache / "command-errors.txt"
    started = time.monotonic()
    with output.open("wb") as stdout, errors.open("wb") as stderr:
        pid = os.fork()
        if pid == 0:
            os.setsid()
            os.dup2(stdout.fileno(), 1)
            os.dup2(stderr.fileno(), 2)
            os.execv(argv[0], argv)
        timed_out = False
        while True:
            child, status, usage = os.wait4(pid, os.WNOHANG)
            if child:
                break
            if time.monotonic() - started >= timeout:
                timed_out = True
                os.killpg(pid, signal.SIGKILL)
                _, status, usage = os.wait4(pid, 0)
                break
            time.sleep(0.01)
    elapsed = time.monotonic() - started
    raw_output = output.read_text(errors="replace")
    stderr = errors.read_text(errors="replace")
    output.unlink()
    errors.unlink()
    try:
        response = json.loads(raw_output)
    except json.JSONDecodeError:
        response = {"unparsed_output": raw_output[:2000]}
    after = disk_bytes(cache)
    return {
        "words": words,
        "elapsed_seconds": round(elapsed, 4),
        "user_seconds": round(usage.ru_utime, 4),
        "system_seconds": round(usage.ru_stime, 4),
        "peak_rss_kib": usage.ru_maxrss,
        "exit_code": os.waitstatus_to_exitcode(status),
        "timed_out": timed_out,
        "timeout_seconds": timeout,
        "cache_before": before,
        "cache_after": after,
        "cache_growth_bytes": after["apparent_bytes"] - before["apparent_bytes"],
        "response": response,
        "stderr": stderr[:2000],
    }


def main():
    report = {
        "schema_version": 1,
        "measurement": "single-run process/index-cache smoke, not a quality benchmark",
        "platform": platform.platform(),
        "binary_sha256": hashlib.sha256(BINARY.read_bytes()).hexdigest(),
        "binary_profile": "debug",
        "implementation_head": git(WORKSPACE, "rev-parse", "HEAD"),
        "implementation_dirty": bool(git(WORKSPACE, "status", "--porcelain")),
        "timer": "Python monotonic + Linux wait4 rusage; /usr/bin/time unavailable",
        "os_page_cache": "not flushed; cold means empty Trufflepig index cache",
        "search_mode": "ordinary lexical, fresh process, --no-daemon; includes reconciliation",
        "corpora": [],
    }
    for name, query in CORPORA.items():
        root = Path("/home/josh/Source") / name
        cache = Path(tempfile.mkdtemp(prefix=f"trufflepig-perf-{name}-", dir=SCRATCH))
        entry = {
            "name": name,
            "head": git(root, "rev-parse", "HEAD"),
            "dirty": bool(git(root, "status", "--porcelain")),
            "snapshot": "current working tree; HEAD records provenance, not frozen inputs",
            "runs": {},
        }
        report["corpora"].append(entry)
        try:
            entry["runs"]["cold_index"] = measure(root, cache, ["index"])
            print(f"{name}: cold index complete", flush=True)
            if entry["runs"]["cold_index"]["exit_code"] == 0:
                entry["runs"]["warm_index"] = measure(root, cache, ["index"])
                entry["runs"]["ordinary_search"] = measure(root, cache, ["search", query])
            else:
                entry["skipped"] = "warm index and search require successful cold index"
        finally:
            shutil.rmtree(cache)
        (WORKSPACE / "evaluation/performance/cli_smoke.json").write_text(
            json.dumps(report, indent=2) + "\n"
        )
        print(f"{name}: measurements written and scratch cache removed", flush=True)


if __name__ == "__main__":
    main()
