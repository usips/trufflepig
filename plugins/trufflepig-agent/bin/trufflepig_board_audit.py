"""Keep board audit records private and attach bounded session call metadata."""
from __future__ import annotations

import json
from datetime import datetime
from pathlib import Path
import re

BOARD_VERBS = {"board", "feedback"}
BOARD_SUBVERBS = {"hello", "new", "show", "post", "claim", "task", "propose", "edit", "accept", "reject", "inbox", "review", "ingest"}
FEEDBACK_REPORT_KINDS = {"blocked", "confused", "wrong", "missing"}
FEEDBACK_SUBVERBS = FEEDBACK_REPORT_KINDS | {"ls", "close"}
RECENT_CALL_LIMIT = 5
RECENT_CALL_BYTES = 2048
AUDIT_TAIL_BYTES = 64 * 1024
BOARD_REFERENCE = re.compile(r"(?:P[1-9][0-9]*(?:\.[1-9][0-9]*|@[1-9][0-9]*(?:\.\.[1-9][0-9]*)?|@[1-9][0-9]*\.\.)?|E[1-9][0-9]*)\Z")
AUDIT_OPTIONS = {"--format", "--diagnostics", "--budget", "-b", "--limit", "-n"}
PRIVATE_OPTIONS = {"--body", "--scope", "--section", "--board-text", "--recent-calls", "--agent-model", "--agent-effort"}
COVERAGE_FIELDS = {"state", "partial", "truncated", "semantic_status", "rerank_status", "parse_failures"}


def abbreviated(value: object, cap: int = 128) -> str:
    """Bound UTF-8 bytes, including an ellipsis when shortening text."""
    text = str(value)
    encoded = text.encode("utf-8")
    if len(encoded) <= cap:
        return text
    return encoded[:cap - 3].decode("utf-8", "ignore") + "…"


def error_prefix(value: object) -> str:
    match = re.match(r"\s*([a-z][a-z0-9_]{0,63}):", value if isinstance(value, str) else "")
    return match.group(1) + ":" if match else ""


def audit_arguments(verb: str, args: list[str]) -> list[str]:
    if verb not in BOARD_VERBS:
        return args
    if not args:
        return []
    # Retain the subverb/kind and an ID, never titles, summaries, or body text.
    accepted = BOARD_SUBVERBS if verb == "board" else FEEDBACK_SUBVERBS
    structural = [args[0]] if args[0] in accepted else []
    if len(args) > 1 and BOARD_REFERENCE.fullmatch(args[1]):
        structural.append(args[1])
    return structural


def audit_options(verb: str, values: dict[str, str]) -> dict[str, str]:
    if verb in BOARD_VERBS:
        return {key: value for key, value in values.items() if key in AUDIT_OPTIONS}
    return {key: value for key, value in values.items()
            if key not in PRIVATE_OPTIONS | {"--session", "--client"}}


def tail_records(path: Path) -> list[dict]:
    """Read only a bounded tail; a partial first row is discarded."""
    try:
        with path.open("rb") as handle:
            handle.seek(0, 2)
            start = max(0, handle.tell() - AUDIT_TAIL_BYTES)
            handle.seek(start)
            data = handle.read(AUDIT_TAIL_BYTES)
        if start:
            data = data.split(b"\n", 1)[-1]
    except OSError:
        return []
    records = []
    for line in data.splitlines():
        try:
            record = json.loads(line)
        except (ValueError, UnicodeDecodeError):
            continue
        if isinstance(record, dict):
            records.append(record)
    return records


def recent_call(record: dict) -> dict:
    verb = abbreviated(record.get("verb", ""), 32)
    args = record.get("args")
    args = [arg for arg in args if isinstance(arg, str)] if isinstance(args, list) else []
    args = audit_arguments(verb, args)
    coverage = record.get("coverage")
    summary = {}
    if isinstance(coverage, dict):
        for member, fields in list(coverage.items())[:3]:
            if isinstance(fields, dict):
                summary[abbreviated(member, 40)] = {
                    key: abbreviated(value, 32) if isinstance(value, str) else value
                    for key, value in fields.items()
                    if key in COVERAGE_FIELDS and isinstance(value, (str, bool, int))
                }
    code = record.get("exit_code")
    return {"verb": verb, "args": [abbreviated(arg) for arg in args[:3]],
            "exit_code": code if type(code) is int and -(2**31) <= code < 2**31 else None,
            "error_prefix": error_prefix(record.get("error")) or error_prefix(record.get("stderr")) or None,
            "truncated": record.get("truncated") if isinstance(record.get("truncated"), bool) else None,
            "coverage": json.dumps(summary, separators=(",", ":"), ensure_ascii=False) if summary else None}


def recent_calls(log_roots: list[Path], harness: str, session: str) -> str:
    """The last five same-session calls, without output bodies, within 2 KiB."""
    records, seen = [], set()
    for root in log_roots:
        path = root / f"{harness}.jsonl"
        resolved = path.resolve()
        if resolved in seen:
            continue
        seen.add(resolved)
        records.extend(record for record in tail_records(path)
                       if record.get("harness") == harness and record.get("session") == session
                       and not str(record.get("verb", "")).startswith("hook:"))
    def order(record: dict) -> int:
        if type(record.get("ts_ns")) is int:
            return record["ts_ns"]
        try:
            return int(datetime.fromisoformat(str(record.get("ts", ""))).timestamp() * 1_000_000_000)
        except (ValueError, OverflowError, OSError):
            return 0
    records.sort(key=order)
    calls = [recent_call(record) for record in records[-RECENT_CALL_LIMIT:]]
    encode = lambda: json.dumps(calls, separators=(",", ":"), ensure_ascii=False)
    # Trim metadata first so the five most recent calls usually all survive.
    while len(encode().encode("utf-8")) > RECENT_CALL_BYTES:
        expanded = next((call for call in calls if call["coverage"] is not None), None)
        if expanded is not None:
            expanded["coverage"] = None
        else:
            next(call for call in calls if call["args"])["args"].pop()
    return encode()
