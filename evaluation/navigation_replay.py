#!/usr/bin/env python3
"""Oracle-assisted navigation replay over frozen source, with bounded followups."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
from urllib.parse import unquote_to_bytes

from records import EventRecord, OutcomeRecord, TaskRecord, UsageRecord, record_dict
from replay import source_path, verify_snapshot

BUDGET = 600
MAX_PAGES = 3
MAX_CALLS = 8
LABEL = "oracle-assisted navigation replay"


def overlap(left, right):
    return (left["path"] == right["path"] and left["start"] < right["end"]
            and right["start"] < left["end"])


def identity(hit):
    return {key: hit[key] for key in ("path", "revision", "start", "end", "handle")
            if key in hit}


def decoded_hit(hit):
    return {**hit, "path": os.fsdecode(unquote_to_bytes(hit["path"]))}


def exact_counter():
    try:
        import tiktoken
    except ImportError:
        return None
    tokenizer = tiktoken.get_encoding("o200k_base")
    return lambda text: len(tokenizer.encode_ordinary(text))


class CommandRunner:
    """Run ordinary CLI requests; captured pipe delivery is local to this runner."""

    def __init__(self, binary, root, cache, counter=None):
        self.prefix = [str(binary), "--root", str(root), "--cache", str(cache),
                       "--no-daemon", "--json", "--budget", str(BUDGET)]
        self.counter = counter

    def __call__(self, operation, argument):
        started = time.perf_counter()
        try:
            result = subprocess.run(self.prefix + [operation, argument],
                                    capture_output=True, check=False, timeout=30)
            stdout, stderr, code, complete = result.stdout, result.stderr, result.returncode, True
        except subprocess.TimeoutExpired as error:
            stdout, stderr, code, complete = error.stdout or b"", error.stderr or b"", None, False
        try:
            text = stdout.decode("utf-8")
        except UnicodeDecodeError:
            text = None
        try:
            response = json.loads(text) if text is not None else None
            if not isinstance(response, dict):
                raise ValueError("response is not an object")
        except ValueError:
            response = {"status": "invalid_response"}
        tokens = self.counter(text) if self.counter and text is not None else None
        status = response.get("status", "ok" if code == 0 else "error")
        if not complete:
            status = "timeout"
        if tokens is not None and tokens > BUDGET:
            status = "budget_exceeded"
        usage = UsageRecord(1, len(stdout), len(stderr), time.perf_counter() - started,
                            tokens, "o200k_base" if tokens is not None else None,
                            not complete or tokens is None)
        return response, status, complete, usage


def combined_usage(events):
    usages = [event.usage for event in events]
    known = all(item.output_tokens is not None for item in usages)
    return UsageRecord(len(events), sum(u.stdout_bytes for u in usages),
                       sum(u.stderr_bytes for u in usages), sum(u.elapsed_seconds for u in usages),
                       sum(u.output_tokens for u in usages) if known else None,
                       "o200k_base" if known else None,
                       any(u.token_cost_censored for u in usages))


def verified_lines(root, response, expected):
    """Credit only emitted original bytes at the selected immutable revision."""
    if (not response.get("verified") or response.get("revision") != expected.get("revision")
            or response.get("path") != expected.get("path")):
        return []
    path = decoded_hit(response)["path"]
    source = source_path(root, path).read_bytes()
    spans = []
    for line in response.get("lines", []):
        start, end = line["start"], line["end"]
        if not 0 <= start < end <= len(source):
            continue
        data = source[start:end]
        if line.get("encoding") == "utf8":
            try:
                valid = data.decode("utf-8") == line.get("text")
            except UnicodeDecodeError:
                valid = False
        elif line.get("encoding") == "byte-escaped":
            valid = "".join(chr(b) if 0x20 <= b <= 0x7e and b != 92
                            else f"\\x{b:02x}" for b in data) == line.get("text")
        else:
            valid = False
        if valid:
            spans.append(dict(path=path, start=start, end=end))
    return spans


def fully_covered(label, spans):
    end = label["start"]
    for span in sorted((s for s in spans if s["path"] == label["path"]),
                       key=lambda s: s["start"]):
        if span["start"] <= end:
            end = max(end, span["end"])
    return end >= label["end"]


def navigate(task, root, runner):
    relevant = task["relevant"]
    events, discovered, evidence, pending, attempted = [], set(), [], [], set()
    pages, rank, context_ok = 0, 0, not task.get("requires_ctx", False)
    next_page, source_next, selected = None, None, None
    first_cost, complete_cost, reason = None, None, "exhausted"
    operation, argument = "search", task["query"]
    while len(events) < MAX_CALLS:
        response, status, delivered, usage = runner(operation, argument)
        accepted = delivered and status == "ok"
        hits = response.get("hits", []) if accepted and operation in ("search", "more") else []
        ranks = list(range(rank + 1, rank + len(hits) + 1))
        rank += len(hits)
        identities = [identity(hit) for hit in hits]
        if operation in ("search", "more"):
            pages += 1
            next_page = response.get("next") if accepted else None
            for hit in hits:
                matching = {i for i, span in enumerate(relevant) if overlap(decoded_hit(hit), span)}
                discovered.update(matching)
                if matching and hit.get("handle") and hit.get("revision"):
                    pending.append(hit)
        if selected and operation in ("show", "ctx"):
            identities.append(identity(selected))
        events.append(EventRecord(task["id"], len(events) + 1, operation, status,
                                  delivered, usage, identities, ranks,
                                  response.get("coverage"), response.get("truncated", False)))
        if operation == "ctx":
            context_ok = accepted and bool(response.get("relationships")) and not response.get("truncated")
        if operation == "show":
            lines = verified_lines(root, response, selected) if accepted else []
            evidence.extend(lines)
            source_next = response.get("next") if lines else None
        count = sum(fully_covered(span, evidence) for span in relevant)
        cost = combined_usage(events)
        if count and first_cost is None and not cost.token_cost_censored:
            first_cost = cost.output_tokens
        if count == len(relevant) and context_ok:
            reason = "complete_evidence"
            complete_cost = cost.output_tokens if not cost.token_cost_censored else None
            break
        if selected and not context_ok and ("ctx", selected["handle"]) not in attempted:
            operation, argument = "ctx", selected["handle"]
        elif source_next and ("show", source_next) not in attempted:
            operation, argument = "show", source_next
        else:
            source_next = None
            pending = [hit for hit in pending if ("show", hit["handle"]) not in attempted
                       and any(overlap(decoded_hit(hit), span) and not fully_covered(span, evidence)
                               for span in relevant)]
            if pending:
                selected = pending.pop(0)
                operation, argument = "show", selected["handle"]
            elif next_page and pages < MAX_PAGES:
                operation, argument = "more", next_page
            else:
                reason = "page_limit" if next_page else "exhausted"
                break
        attempted.add((operation, argument))
    else:
        reason = "call_limit"
    count = sum(fully_covered(span, evidence) for span in relevant)
    usage = combined_usage(events)
    outcome = OutcomeRecord(task["id"], len(discovered), count, len(relevant), context_ok,
                            "complete" if reason == "complete_evidence" else "incomplete", reason,
                            usage, first_cost, complete_cost,
                            reason != "complete_evidence" or usage.token_cost_censored)
    return dict(task=record_dict(TaskRecord(task["id"], task["query"], relevant,
                                          task.get("snapshot", {}), task.get("requires_ctx", False))),
                events=[record_dict(event) for event in events], outcome=record_dict(outcome))


def replay_navigation(manifest_path, binary):
    manifest = json.loads(manifest_path.read_text())
    root = (manifest_path.parent / manifest["root"]).resolve()
    verify_snapshot(root, manifest)
    scratch = Path(os.environ.get("TMPDIR", "/home/josh/.cache/codex-tmp"))
    if scratch.resolve() == Path("/tmp") or Path("/tmp") in scratch.resolve().parents:
        scratch = Path("/home/josh/.cache/codex-tmp")
    scratch.mkdir(parents=True, exist_ok=True)
    records = []
    with tempfile.TemporaryDirectory(prefix="navigation-eval-", dir=scratch) as cache:
        indexed = subprocess.run([str(binary), "--root", str(root), "--cache", cache,
                                  "--no-daemon", "index"], capture_output=True, timeout=120)
        if indexed.returncode:
            raise RuntimeError("Trufflepig index failed")
        runner = CommandRunner(binary, root, cache, exact_counter())
        for task in manifest["tasks"]:
            verify_snapshot(root, manifest)
            records.append(navigate({**task, "snapshot": manifest["snapshot"]}, root, runner))
    verify_snapshot(root, manifest)
    return dict(schema_version=1, workflow_version="navigation-v1", label=LABEL,
                budget_tokens=BUDGET, maximum_pages=MAX_PAGES, maximum_calls=MAX_CALLS,
                label_scope="authored fixtures; no agent effectiveness claims", records=records)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path,
                        default=Path(__file__).parent / "manifests/fixtures.json")
    parser.add_argument("--trufflepig", type=Path, required=True)
    args = parser.parse_args()
    try:
        report = replay_navigation(args.manifest.resolve(), args.trufflepig.resolve())
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.TimeoutExpired) as error:
        parser.exit(1, f"navigation evaluation failed: {error}\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
