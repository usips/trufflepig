#!/usr/bin/env python3
"""Replay frozen retrieval cases using ordinary rg/read and optional Trufflepig."""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
from urllib.parse import unquote_to_bytes


def source_path(root, relative):
    path = root / relative
    if Path(relative).is_absolute() or not path.resolve().is_relative_to(root):
        raise ValueError(f"source escapes corpus root: {relative!r}")
    return path


def verify_snapshot(root, manifest):
    if manifest.get("schema_version") != 1:
        raise ValueError("unsupported manifest version")
    files = manifest["snapshot"]["files"]
    actual = {str(p.relative_to(root)) for p in root.rglob("*") if p.is_file()}
    if actual != set(files):
        raise ValueError("snapshot file set changed")
    for relative, expected in files.items():
        data = source_path(root, relative).read_bytes()
        if hashlib.sha256(data).hexdigest() != expected:
            raise ValueError(f"snapshot hash mismatch: {relative}")
    task_ids = set()
    for task in manifest["tasks"]:
        if task["id"] in task_ids:
            raise ValueError("duplicate task id")
        task_ids.add(task["id"])
        if not task["relevant"]:
            raise ValueError("retrieval task requires relevance labels")
        for span in task["relevant"]:
            if span["path"] not in files:
                raise ValueError("relevance path is outside frozen snapshot")
            size = source_path(root, span["path"]).stat().st_size
            if not 0 <= span["start"] < span["end"] <= size:
                raise ValueError("relevance span is outside source bytes")


def rg_bytes(field):
    if "bytes" in field:
        return base64.b64decode(field["bytes"])
    return field["text"].encode("utf-8")


def baseline_search(root, task, files):
    spec = task["baseline"]
    command = ["rg", "--json", "--sort", "path", "--no-ignore", "--hidden"]
    if spec.get("literal", True):
        command.append("--fixed-strings")
    command += ["--", spec["pattern"], *sorted(files)]
    start = time.perf_counter()
    completed = subprocess.run(command, cwd=root, capture_output=True, check=False)
    elapsed = time.perf_counter() - start
    if completed.returncode not in (0, 1):
        raise RuntimeError(completed.stderr.decode("utf-8", errors="replace"))
    hits = []
    for line in completed.stdout.splitlines():
        event = json.loads(line)
        if event["type"] != "match":
            continue
        data = event["data"]
        path = os.fsdecode(rg_bytes(data["path"]))
        for match in data["submatches"]:
            hits.append(dict(path=path, start=data["absolute_offset"] + match["start"],
                             end=data["absolute_offset"] + match["end"]))
    read_bytes = 0
    if hits:
        first = hits[0]
        source = source_path(root, first["path"]).read_bytes()
        line_number = source[:first["start"]].count(b"\n")
        lines = source.splitlines(keepends=True)
        excerpt = b"".join(lines[max(0, line_number - 5):line_number + 6])
        read_bytes = len(excerpt)
    return dict(hits=hits, search_seconds=elapsed,
                search_protocol_bytes=len(completed.stdout), read_source_bytes=read_bytes,
                tool_calls=1 + bool(hits), truncated=False, status="ok")


def trufflepig_search(binary, root, cache, task):
    command = [str(binary), "--root", str(root), "--cache", str(cache),
               "--no-daemon", "--json", "--budget", "10000", "--limit", "10",
               "search", task["query"]]
    start = time.perf_counter()
    completed = subprocess.run(command, capture_output=True, check=False)
    elapsed = time.perf_counter() - start
    try:
        response = json.loads(completed.stdout)
    except (json.JSONDecodeError, UnicodeDecodeError):
        response = {"status": "invalid_response"}
    if not isinstance(response, dict):
        response = {"status": "invalid_response"}
    hits = []
    for hit in response.get("hits", []):
        hits.append(dict(path=os.fsdecode(unquote_to_bytes(hit["path"])),
                         start=hit["start"], end=hit["end"]))
    status = response.get("status", "ok" if completed.returncode == 0 else "error")
    return dict(hits=hits, search_seconds=elapsed,
                search_protocol_bytes=len(completed.stdout), tool_calls=1,
                truncated=response.get("truncated", False), status=status,
                coverage=response.get("coverage"), exit_code=completed.returncode)


def recall_metrics(task, result):
    relevant = task["relevant"]
    paths = {span["path"] for span in relevant}
    hits = result["hits"]
    metrics = {}
    for k in (5, 10):
        found = {hit["path"] for hit in hits[:k]}
        metrics[f"file_recall_at_{k}"] = len(paths & found) / len(paths)
    covered = sum(any(hit["path"] == span["path"] and
                      hit["start"] < span["end"] and span["start"] < hit["end"]
                      for hit in hits[:10]) for span in relevant)
    metrics["span_recall_at_10"] = covered / len(relevant)
    metrics["miss"] = covered == 0
    return metrics


def replay(manifest_path, binary=None):
    manifest = json.loads(manifest_path.read_text())
    root = (manifest_path.parent / manifest["root"]).resolve()
    verify_snapshot(root, manifest)
    scratch = Path("/home/josh/.cache/codex-tmp")
    scratch.mkdir(parents=True, exist_ok=True)
    records = []
    with tempfile.TemporaryDirectory(prefix="trufflepig-eval-", dir=scratch) as cache:
        if binary:
            command = [str(binary), "--root", str(root), "--cache", cache,
                       "--no-daemon", "index"]
            indexed = subprocess.run(command, capture_output=True, check=False)
            if indexed.returncode:
                raise RuntimeError(f"Trufflepig index failed: {indexed.stderr!r}")
        for task in manifest["tasks"]:
            runs = {"rg_read": baseline_search(root, task, manifest["snapshot"]["files"])}
            if binary:
                runs["trufflepig"] = trufflepig_search(binary, root, Path(cache), task)
            for engine, result in runs.items():
                records.append(dict(task_id=task["id"], language=task["language"],
                                    intent=task["intent"],
                                    engine=engine, **result,
                                    metrics=recall_metrics(task, result)))
    verify_snapshot(root, manifest)
    aggregates = {}
    for engine in sorted({record["engine"] for record in records}):
        samples = [record for record in records if record["engine"] == engine]
        aggregates[engine] = dict(
            tasks=len(samples), misses=sum(r["metrics"]["miss"] for r in samples),
            mean_span_recall_at_10=sum(r["metrics"]["span_recall_at_10"]
                                       for r in samples) / len(samples),
            non_ok=sum(r["status"] != "ok" for r in samples))
    by_language = {}
    for record in records:
        key = f"{record['engine']}:{record['language']}"
        group = by_language.setdefault(key, {"tasks": 0, "misses": 0, "recall_sum": 0})
        group["tasks"] += 1
        group["misses"] += record["metrics"]["miss"]
        group["recall_sum"] += record["metrics"]["span_recall_at_10"]
    for group in by_language.values():
        group["mean_span_recall_at_10"] = group.pop("recall_sum") / group["tasks"]
    return dict(schema_version=1, manifest=str(manifest_path),
                label_scope="authored fixtures; no held-out or agent-success claims",
                aggregates=aggregates, by_language=by_language, records=records)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path,
                        default=Path(__file__).parent / "manifests/fixtures.json")
    parser.add_argument("--trufflepig", type=Path)
    args = parser.parse_args()
    try:
        report = replay(args.manifest.resolve(),
                        args.trufflepig.resolve() if args.trufflepig else None)
    except (OSError, ValueError, KeyError, RuntimeError) as error:
        parser.exit(1, f"evaluation failed: {error}\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
