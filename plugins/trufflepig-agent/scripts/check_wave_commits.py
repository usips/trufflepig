#!/usr/bin/env python3
"""Check wave commit evidence, or install a hook checking actual push ranges."""
import argparse
from pathlib import Path
import re
import shlex
import subprocess
import sys


TRAILER = re.compile(r"^([A-Za-z0-9-]+):[ \t]*(.*)$")
SUBJECT = re.compile(r"^[a-z][a-z0-9-]*(?:\([^()\s]+\))?!?: \S.*$")
OID = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
HARNESSES = {"codex", "codex cli", "cli", "human", "claude", "claude code",
             "kimi", "muse", "muse code", "omp", "chatgpt", "gemini", "grok", "qwen"}
REASON = r"n/a \((?=[^();]*[A-Za-z])[^();]+\)"
COUNT = rf"(?:\d+/\d+|{REASON})"
SUITES = re.compile(
    rf"Suites: lib (?:\d+/\d+/\d+ \(full\)|{REASON}); "
    rf"board (?:\d+/\d+/\d+|{REASON}); Python (?:\d+ OK|{REASON}); "
    rf"assets (?:node20 {COUNT}, node21 {COUNT}, node22 {COUNT}|{REASON})\Z"
)
TEST_NAME = r"[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*"
RED_FIRST = re.compile(rf"^Red-first: (?:`({TEST_NAME})`|({TEST_NAME}))\s+\S")


def git(*args, input=None):
    result = subprocess.run(["git", *args], input=input, capture_output=True,
                            text=True, timeout=30)
    if result.returncode:
        raise RuntimeError(" ".join((result.stderr or result.stdout).splitlines()))
    return result.stdout


def commit_violations(oid):
    message = git("show", "--no-patch", "--format=%B", oid, "--")
    lines = message.splitlines()
    while lines and not lines[-1].strip():
        lines.pop()
    violations = []
    subject = lines[0] if lines else ""
    if len(subject) > 50 or subject.endswith(".") or not SUBJECT.fullmatch(subject):
        violations.append(("subject", "use a Conventional subject <=50 characters, without a period"))
    for number, line in enumerate(lines[1:], 2):
        if len(line) > 72:
            violations.append(("body", f"line {number} is {len(line)} characters (maximum 72)"))

    start = len(lines)
    while start and lines[start - 1].strip():
        start -= 1
    block = lines[start:]
    if not block or any(not TRAILER.match(line) and not line[:1].isspace()
                        for line in block) or not TRAILER.match(block[0]):
        violations.append(("trailers", "end with one contiguous trailer paragraph"))
    if any(re.match(r"^(Plan|Plan-Task|Co-authored-by):", line, re.I)
           for line in lines[:start]):
        violations.append(("trailers", "Plan, Plan-Task and co-authors must share the final block"))

    parsed = {}
    for line in git("interpret-trailers", "--parse", input=message).splitlines():
        match = TRAILER.match(line)
        if match:
            parsed.setdefault(match[1].lower(), []).append(match[2])
    plans = parsed.get("plan", [])
    tasks = parsed.get("plan-task", [])
    if not plans or any(not re.fullmatch(r"P\d+", plan) for plan in plans):
        violations.append(("Plan", "require Git-parsed Plan: P<number> trailers"))
    if not tasks or any(not re.fullmatch(r"P\d+\.\d+", task) for task in tasks):
        violations.append(("Plan-Task", "require Git-parsed Plan-Task: P<number>.<number>"))
    for task in tasks:
        if task.partition(".")[0] not in plans:
            violations.append(("Plan-Task", f"{task} has no matching Plan trailer"))
    authors = parsed.get("co-authored-by", [])
    if not authors:
        violations.append(("Co-authored-by", "require at least one model co-author"))
    for author in authors:
        match = re.fullmatch(r"([^<>]+) <([^<>\s@]+@[^<>\s@]+)>", author)
        if not match or not match[1].strip() or match[1].strip().lower() in HARNESSES:
            violations.append(("Co-authored-by", f"use an actual model ID and email, not {author!r}"))

    body = lines[1:start]
    red = [RED_FIRST.match(line) for line in body]
    if not any(match and not re.fullmatch(r"E\d{4}", match[1] or match[2]) for match in red):
        violations.append(("Red-first", "name a bare or backticked test and record its parent failure"))
    suites = []
    for index, line in enumerate(body):
        if line.startswith("Suites:"):
            stop = index + 1
            while stop < len(body) and body[stop].strip():
                stop += 1
            suites.append(" ".join("\n".join(body[index:stop]).split()))
    if len(suites) != 1 or not SUITES.fullmatch(suites[0]):
        violations.append(("Suites", "require lib full P/F/I; board P/F/I; Python N OK; "
                           "assets node20, node21, node22 N/F (or n/a with a reason)"))
    return violations


def revision_commits(revision):
    return git("rev-list", "--reverse", "--end-of-options", revision, "--").splitlines()


def push_commits(destination, stream):
    updates = []
    for line in stream:
        fields = line.split()
        if len(fields) != 4 or not OID.fullmatch(fields[1]) or not OID.fullmatch(fields[3]):
            raise RuntimeError("invalid pre-push update; expected two refs and two full object IDs")
        local, remote = fields[1], fields[3]
        if set(local) != {"0"}:
            updates.append((local, remote))
    known = []
    if any(set(remote) == {"0"} for _, remote in updates):
        for line in git("ls-remote", "--refs", "--", destination).splitlines():
            fields = line.split()
            if len(fields) != 2 or not OID.fullmatch(fields[0]):
                raise RuntimeError("invalid remote ref data; cannot determine new-branch range")
            known.append(fields[0])
    commits = {}
    for local, remote in updates:
        if set(remote) == {"0"}:
            args = [local, "--not", *known]
            found = git("rev-list", "--reverse", *args, "--").splitlines()
        else:
            found = revision_commits(f"{remote}..{local}")
        commits.update(dict.fromkeys(found))
    return commits


def install_hook():
    hook = Path(git("rev-parse", "--path-format=absolute", "--git-path", "hooks/pre-push").strip())
    script = Path(__file__).resolve()
    content = ("#!/bin/sh\n# trufflepig-wave-commit-hook v1\nexec "
               f"{shlex.quote(sys.executable)} {shlex.quote(str(script))} --pre-push \"$@\"\n")
    if hook.is_symlink() or (hook.exists() and hook.read_text() != content):
        raise RuntimeError(f"refusing to overwrite unrelated hook: {hook}")
    if not hook.exists():
        hook.parent.mkdir(parents=True, exist_ok=True)
        with hook.open("x") as output:
            output.write(content)
    hook.chmod(hook.stat().st_mode | 0o111)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("revision", nargs="?", help="Git revision range, such as BASE..HEAD")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--install-hook", action="store_true", help="install a safe pre-push hook")
    mode.add_argument("--pre-push", nargs=2, metavar=("REMOTE", "URL"), help=argparse.SUPPRESS)
    args = parser.parse_args()
    if bool(args.revision) == bool(args.install_hook or args.pre_push):
        parser.error("provide a revision range or --install-hook")
    try:
        if args.install_hook:
            install_hook()
            commits = []
        elif args.pre_push:
            commits = push_commits(args.pre_push[1], sys.stdin)
        else:
            commits = revision_commits(args.revision)
        failed = False
        for oid in commits:
            for rule, detail in commit_violations(oid):
                print(f"{oid} {rule}: {detail}")
                failed = True
        if failed:
            return 1
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f"{args.revision or 'hook'} git: {error}")
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
