#!/usr/bin/env python3
"""Oracle-assisted workspace navigation with member-qualified evidence records."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import tomllib
from urllib.parse import unquote_to_bytes

from navigation_replay import (BUDGET, LABEL, MAX_CALLS, MAX_PAGES, CommandRunner,
                               combined_usage, exact_counter, fully_covered, verified_lines)
from records import EventRecord, OutcomeRecord, record_dict
from replay import source_path

WORKFLOW = "workspace-navigation-v1"


class WorkspaceRunner(CommandRunner):
    def __init__(self, binary, workspace, root, cache=None, no_daemon=False, counter=None):
        self.prefix = [str(binary), "--workspace", str(workspace), "--root", str(root),
                       "--json", "--budget", str(BUDGET)]
        if cache is not None:
            self.prefix += ["--cache", str(cache)]
        if no_daemon:
            self.prefix.append("--no-daemon")
        self.counter = counter


def member_roots(workspace):
    configuration = tomllib.loads(workspace.read_text())
    roots = {}
    for name, member in configuration["members"].items():
        path = Path(member["path"]).expanduser()
        roots[name] = (path if path.is_absolute() else workspace.parent / path).resolve()
    return roots


def validate_manifest(manifest, roots):
    if manifest.get("schema_version") != 1 or not manifest.get("tasks"):
        raise ValueError("workspace manifest requires schema_version 1 and tasks")
    seen = set()
    for task in manifest["tasks"]:
        if not task.get("id") or task["id"] in seen:
            raise ValueError("missing or duplicate workspace task id")
        seen.add(task["id"])
        queries = [task.get("query"), *task.get("followup_queries", [])]
        if len(queries) > MAX_PAGES or any(not isinstance(q, str) or not q.strip() for q in queries):
            raise ValueError("workspace task requires one to three nonempty frozen queries")
        if not task.get("expected"):
            raise ValueError("workspace task requires member-qualified expected evidence")
        labels = set()
        for label in task["expected"]:
            if label.get("member") not in roots or not isinstance(label.get("path"), str):
                raise ValueError("unknown evidence member or missing path")
            source_path(roots[label["member"]], label["path"])
            if ("start" in label) != ("end" in label):
                raise ValueError("evidence requires both byte bounds or neither")
            if "start" in label and not 0 <= label["start"] < label["end"]:
                raise ValueError("invalid evidence byte bounds")
            key = (label["member"], label["path"], label.get("start"), label.get("end"))
            if key in labels:
                raise ValueError("duplicate evidence label")
            labels.add(key)


def verify_expected(task, roots):
    """Freeze only labeled files; unrelated repository content can still change."""
    snapshot = []
    for label in task["expected"]:
        data = source_path(roots[label["member"]], label["path"]).read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        if label.get("sha256") not in (None, digest):
            raise ValueError(f"expected source changed: {label['member']}:{label['path']}")
        if label.get("end", 0) > len(data):
            raise ValueError("evidence span exceeds source length")
        snapshot.append(dict(member=label["member"], path=label["path"], sha256=digest))
    return snapshot


def event_identity(hit):
    return {key: hit[key] for key in ("member", "repository", "path", "revision", "start",
                                     "end", "handle", "member_rank") if key in hit}


def provenance_matches(value, roots):
    root = roots.get(value.get("member"))
    encoded = value.get("repository")
    return root is not None and isinstance(encoded, str) and os.fsdecode(unquote_to_bytes(encoded)) == str(root)


def matches(hit, label):
    if hit.get("member") != label["member"] or "path" not in hit:
        return False
    if os.fsdecode(unquote_to_bytes(hit["path"])) != label["path"]:
        return False
    return "start" not in label or (hit.get("start", 0) < label["end"]
                                     and label["start"] < hit.get("end", 0))


def covered(label, evidence):
    spans = [span for span in evidence if span["member"] == label["member"]]
    if "start" in label:
        return fully_covered(label, spans)
    return any(span["path"] == label["path"] for span in spans)


def source_evidence(response, selected, roots):
    if (not selected or response.get("member") != selected["member"]
            or not provenance_matches(response, roots)
            or response.get("repository") != selected.get("repository")):
        return []
    try:
        spans = verified_lines(roots[selected["member"]], response, selected)
    except (OSError, ValueError, KeyError, TypeError):
        return []
    return [dict(span, member=selected["member"]) for span in spans]


def navigate(task, roots, runner):
    labels = task["expected"]
    events, discovered, evidence, pending, attempted = [], set(), [], [], set()
    queries = list(task.get("followup_queries", []))
    pages, rank, context_ok = 0, 0, not task.get("requires_ctx", False)
    next_page, source_next, selected = None, None, None
    first_cost, complete_cost, reason = None, None, "exhausted"
    operation, argument = "search", task["query"]
    while len(events) < MAX_CALLS:
        response, status, delivered, usage = runner(operation, argument)
        accepted = delivered and status == "ok"
        hits = response.get("hits", []) if accepted and operation in ("search", "more") else []
        if operation == "search":
            rank = 0
        ranks = list(range(rank + 1, rank + len(hits) + 1))
        rank += len(hits)
        identified = []
        if operation in ("search", "more"):
            pages += 1
            next_page = response.get("next") if accepted else None
            for hit in hits:
                hit = {**hit, "repository": response.get("members", {}).get(hit.get("member"))}
                identified.append(event_identity(hit))
                if not provenance_matches(hit, roots):
                    continue
                relevant = {index for index, label in enumerate(labels) if matches(hit, label)}
                discovered.update(relevant)
                if relevant and hit.get("handle") and hit.get("revision"):
                    pending.append(hit)
        if selected and operation in ("show", "ctx"):
            identified.append(event_identity(selected))
        event = record_dict(EventRecord(task["id"], len(events) + 1, operation, status,
                                       delivered, usage, identified, ranks,
                                       response.get("coverage"), response.get("truncated", False)))
        event["argument"] = argument
        events.append(event)
        if operation == "ctx":
            subject = response.get("hit", {})
            context_ok = (accepted and provenance_matches(response, roots)
                          and response.get("member") == selected["member"]
                          and subject.get("path") == selected["path"]
                          and subject.get("revision") == selected["revision"]
                          and bool(response.get("relationships")) and not response.get("truncated"))
        if operation == "show":
            lines = source_evidence(response, selected, roots) if accepted else []
            evidence.extend(lines)
            source_next = response.get("next") if lines else None
        count = sum(covered(label, evidence) for label in labels)
        cost = usage_total(events)
        if count and first_cost is None and not cost.token_cost_censored:
            first_cost = cost.output_tokens
        if count == len(labels) and context_ok:
            reason = "complete_evidence"
            complete_cost = cost.output_tokens if not cost.token_cost_censored else None
            break
        needs_selected = selected and any(matches(selected, label) and not covered(label, evidence)
                                           for label in labels)
        if selected and not context_ok and ("ctx", selected["handle"]) not in attempted:
            operation, argument = "ctx", selected["handle"]
        elif source_next and needs_selected and ("show", source_next) not in attempted:
            operation, argument = "show", source_next
        else:
            source_next = None
            pending = [hit for hit in pending if ("show", hit["handle"]) not in attempted
                       and any(matches(hit, label) and not covered(label, evidence) for label in labels)]
            if pending:
                selected = pending.pop(0)
                operation, argument = "show", selected["handle"]
            elif next_page and pages + len(queries) < MAX_PAGES:
                operation, argument = "more", next_page
            elif queries and pages < MAX_PAGES:
                operation, argument = "search", queries.pop(0)
                selected = None
            else:
                reason = "page_limit" if next_page or queries else "exhausted"
                break
        attempted.add((operation, argument))
    else:
        reason = "call_limit"
    usage = usage_total(events)
    missed = [label for label in labels if not covered(label, evidence)]
    outcome = record_dict(OutcomeRecord(task["id"], len(discovered), len(labels) - len(missed),
        len(labels), context_ok, "complete" if not missed and context_ok else "incomplete",
        reason, usage, first_cost, complete_cost,
        reason != "complete_evidence" or usage.token_cost_censored))
    outcome["missed_evidence"] = missed
    return dict(task=dict(task, record_type="task", schema_version=1, workflow_version=WORKFLOW),
                events=events, outcome=outcome)


def usage_total(events):
    from types import SimpleNamespace
    from records import UsageRecord
    return combined_usage([SimpleNamespace(usage=UsageRecord(**event["usage"])) for event in events])


def replay(manifest, workspace, root, binary, cache=None, no_daemon=False):
    roots = member_roots(workspace)
    validate_manifest(manifest, roots)
    runner = WorkspaceRunner(binary, workspace, root, cache, no_daemon, exact_counter())
    records = []
    for task in manifest["tasks"]:
        snapshot = verify_expected(task, roots)
        record = navigate(task, roots, runner)
        record["task"]["snapshot"] = dict(kind="sha256-labeled-files", files=snapshot)
        try:
            changed = verify_expected(task, roots) != snapshot
        except (OSError, ValueError):
            changed = True
        if changed:
            record["outcome"].update(status="invalid_snapshot", evidence_cost_censored=True,
                                       stop_reason="labeled_source_changed")
        records.append(record)
    return dict(schema_version=1, workflow_version=WORKFLOW, label=LABEL,
                budget_tokens=BUDGET, maximum_pages=MAX_PAGES, maximum_calls=MAX_CALLS,
                label_scope="authored member/path evidence; not agent effectiveness or semantic completeness",
                snapshot_scope="labeled files only; other repository content is not frozen",
                records=records)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "workspace", "root", "manifest"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--cache", type=Path)
    parser.add_argument("--no-daemon", action="store_true")
    args = parser.parse_args()
    try:
        report = replay(json.loads(args.manifest.read_text()), args.workspace.resolve(),
                        args.root.resolve(), args.binary.resolve(), args.cache, args.no_daemon)
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f"workspace evaluation failed: {error}\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
