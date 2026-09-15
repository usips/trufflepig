"""Version-two records for paired development navigation trials.

Task labels, source coordinates, parent revisions, and hashes are grader data.
The solver view deliberately contains only the natural-language task contract.
"""

from __future__ import annotations

import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
from urllib.parse import unquote_to_bytes, urlsplit

SCHEMA_VERSION = 2
WORKFLOW_VERSION = "gpu-navigation-v1"
SPLIT = "development"
MAX_TOOL_CALLS = 12
MAX_EMITTED_TOKENS = 12_000
MAX_OUTPUT_TOKENS_PER_CALL = 900
TASK_TIMEOUT_SECONDS = 600
ARMS = ("oldlexical", "newfilefirstlexical", "cuda")
CORPORA = ("lunatic", "tales-from-space", "tgstation")


class ManifestError(ValueError):
    """Raised when a frozen manifest cannot be safely replayed."""


def load_manifest(path: Path | str | None = None) -> dict:
    if path is None:
        path = Path(__file__).with_name("manifest.json")
    path = Path(path)
    try:
        manifest = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ManifestError(f"cannot load manifest: {error}") from error
    validate_manifest(manifest)
    return manifest


def validate_manifest(manifest: dict) -> None:
    if not isinstance(manifest, dict):
        raise ManifestError("manifest must be an object")
    if manifest.get("schema_version") != SCHEMA_VERSION:
        raise ManifestError("unsupported gpu navigation schema")
    if manifest.get("workflow_version") != WORKFLOW_VERSION:
        raise ManifestError("unexpected gpu navigation workflow")
    if manifest.get("split") != SPLIT or manifest.get("held_out_claim") is not False:
        raise ManifestError("gpu manifest must be development-only")
    limits = manifest.get("limits")
    if limits != {
        "maximum_tool_calls": MAX_TOOL_CALLS,
        "maximum_emitted_tokens": MAX_EMITTED_TOKENS,
        "maximum_output_tokens_per_call": MAX_OUTPUT_TOKENS_PER_CALL,
        "task_timeout_seconds": TASK_TIMEOUT_SECONDS,
    }:
        raise ManifestError("gpu manifest limits do not match the trial contract")
    arms = manifest.get("arms")
    if (not isinstance(arms, list) or len(arms) != len(ARMS)
            or any(not isinstance(item, dict) for item in arms)
            or {item.get("id") for item in arms} != set(ARMS)):
        raise ManifestError("gpu manifest must declare all three retrieval arms")
    corpora = manifest.get("corpora")
    if not isinstance(corpora, dict) or set(corpora) != set(CORPORA):
        raise ManifestError("gpu manifest must declare the three development corpora")
    for name, corpus in corpora.items():
        if not isinstance(corpus, dict) or not corpus.get("root") \
                or not corpus.get("parent_revision") or not corpus.get("language"):
            raise ManifestError(f"invalid corpus declaration: {name}")
    tasks = manifest.get("tasks")
    if not isinstance(tasks, list) or len(tasks) != 12:
        raise ManifestError("gpu manifest requires exactly twelve frozen tasks")
    task_ids = set()
    for task in tasks:
        _validate_task(task, corpora, task_ids)


def _validate_task(task: dict, corpora: dict, task_ids: set[str]) -> None:
    if not isinstance(task, dict) or not task.get("id") or task["id"] in task_ids:
        raise ManifestError("missing or duplicate gpu task id")
    task_ids.add(task["id"])
    corpus = corpora.get(task.get("corpus"))
    if corpus is None or task.get("language") != corpus.get("language"):
        raise ManifestError(f"task has unknown corpus or language: {task.get('id')}")
    if not isinstance(task.get("prompt"), str) or not task["prompt"].strip():
        raise ManifestError(f"task has no solver prompt: {task['id']}")
    if not isinstance(task.get("query"), str) or not task["query"].strip():
        raise ManifestError(f"task has no retrieval query: {task['id']}")
    labels = task.get("labels")
    snapshot = task.get("snapshot")
    if not isinstance(labels, list) or not labels or not isinstance(snapshot, dict):
        raise ManifestError(f"task has no frozen grading labels: {task['id']}")
    files = snapshot.get("files")
    if not snapshot.get("parent_revision") or not isinstance(files, list) or not files:
        raise ManifestError(f"task has no frozen source snapshot: {task['id']}")
    if snapshot["parent_revision"] != corpus.get("parent_revision"):
        raise ManifestError(f"task snapshot revision disagrees with corpus: {task['id']}")
    frozen = {}
    for item in files:
        if not isinstance(item, dict) or not _safe_relative_path(item.get("path")) \
                or not _sha256(item.get("sha256")):
            raise ManifestError(f"invalid frozen file in {task['id']}")
        if item["path"] in frozen:
            raise ManifestError(f"duplicate frozen file in {task['id']}")
        frozen[item["path"]] = item["sha256"]
    for label in labels:
        if not isinstance(label, dict) or not _safe_relative_path(label.get("path")) \
                or label.get("path") not in frozen:
            raise ManifestError(f"label is outside frozen files in {task['id']}")
        start, end = label.get("start"), label.get("end")
        if not isinstance(start, int) or not isinstance(end, int) or not 0 <= start < end:
            raise ManifestError(f"invalid label span in {task['id']}")
        if label.get("sha256") != frozen[label["path"]]:
            raise ManifestError(f"label hash disagrees with snapshot in {task['id']}")
        if label.get("provenance") != "read-evidence-parent-snapshot":
            raise ManifestError(f"label provenance is not read evidence in {task['id']}")


def _sha256(value) -> bool:
    if not isinstance(value, str) or len(value) != 64:
        return False
    try:
        int(value, 16)
    except ValueError:
        return False
    return True


def _safe_relative_path(value) -> bool:
    if not isinstance(value, str) or not value or os.path.isabs(value):
        return False
    path = Path(value)
    return ".." not in path.parts and not value.startswith("./")


def corpus_roots(manifest: dict, base: Path | str | None = None) -> dict[str, Path]:
    base = Path(base or Path.cwd()).resolve()
    roots = {}
    for name, item in manifest["corpora"].items():
        root = Path(item["root"]).expanduser()
        roots[name] = (root if root.is_absolute() else base / root).resolve()
    return roots


def verify_snapshot(task: dict, root: Path | str, *, require_revision: bool = True) -> dict:
    """Verify selected files without copying a repository or corpus."""
    root = Path(root).resolve()
    snapshot = task["snapshot"]
    if require_revision:
        try:
            revision = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"],
                                      check=True, capture_output=True, text=True,
                                      timeout=10).stdout.strip()
        except (OSError, subprocess.SubprocessError) as error:
            raise ManifestError(f"cannot inspect corpus revision: {error}") from error
        if revision != snapshot["parent_revision"]:
            raise ManifestError(f"corpus revision changed: {root}")
    files = []
    frozen_sizes = {}
    for item in snapshot["files"]:
        path = _source_path(root, item["path"])
        data = path.read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        if digest != item["sha256"]:
            raise ManifestError(f"frozen source changed: {item['path']}")
        frozen_sizes[item["path"]] = len(data)
        files.append(dict(item))
    for label in task["labels"]:
        if label["end"] > frozen_sizes[label["path"]]:
            raise ManifestError(f"frozen label exceeds source: {label['path']}")
    return {"parent_revision": snapshot["parent_revision"], "files": files}


def _source_path(root: Path, relative: str) -> Path:
    path = (root / relative).resolve()
    try:
        path.relative_to(root)
    except ValueError as error:
        raise ManifestError(f"source escapes corpus root: {relative}") from error
    if not path.is_file():
        raise ManifestError(f"frozen source is unavailable: {relative}")
    return path


def solver_task(task: dict, *, arm: str | None = None) -> dict:
    """Return the only task fields that may be shown to a solver."""
    if arm is not None and arm not in ARMS:
        raise ManifestError(f"unknown retrieval arm: {arm}")
    view = {
        "schema_version": SCHEMA_VERSION,
        "workflow_version": WORKFLOW_VERSION,
        "id": task["id"],
        "corpus": task["corpus"],
        "language": task["language"],
        "intent": task["intent"],
        "prompt": task["prompt"],
        "query": task["query"],
        "limits": {
            "maximum_tool_calls": MAX_TOOL_CALLS,
            "maximum_emitted_tokens": MAX_EMITTED_TOKENS,
            "maximum_output_tokens_per_call": MAX_OUTPUT_TOKENS_PER_CALL,
            "task_timeout_seconds": TASK_TIMEOUT_SECONDS,
        },
    }
    if arm is not None:
        view["retrieval_arm"] = arm
    return view


def solver_tasks(manifest: dict, *, arm: str | None = None) -> list[dict]:
    """Return all public task prompts in manifest order."""
    validate_manifest(manifest)
    return [solver_task(task, arm=arm) for task in manifest["tasks"]]


def _decoded_path(value) -> str | None:
    if isinstance(value, dict):
        value = value.get("uri") or value.get("path")
    if not isinstance(value, str):
        return None
    if value.startswith("file:"):
        parsed = urlsplit(value)
        if parsed.scheme == "file":
            value = parsed.path
            if parsed.netloc and parsed.netloc != "localhost":
                value = f"//{parsed.netloc}{value}"
        else:
            value = value[5:]
    return os.fsdecode(unquote_to_bytes(value))


def _member_path(hit: dict, roots: dict[str, Path]) -> tuple[str | None, str | None]:
    path = _decoded_path(hit.get("path") or hit.get("file") or hit.get("uri"))
    member = hit.get("member")
    if member in roots and path is not None and os.path.isabs(path):
        path = os.path.relpath(path, roots[member])
    elif path is not None and os.path.isabs(path):
        for name, root in roots.items():
            candidate = Path(path)
            try:
                path = str(candidate.relative_to(root))
            except ValueError:
                continue
            member = name
            break
    return member, path


def _event_response(event: dict) -> tuple[dict, bool]:
    response = event.get("response", event)
    if not isinstance(response, dict):
        return {}, False
    complete = event.get("complete_delivery", event.get("complete", True))
    return response, bool(complete)


def _valid_line(source: bytes, line: dict) -> tuple[int, int] | None:
    start, end = line.get("start"), line.get("end")
    if not isinstance(start, int) or not isinstance(end, int) or not 0 <= start < end <= len(source):
        return None
    data = source[start:end]
    text = line.get("text")
    if line.get("encoding") == "utf8":
        try:
            valid = data.decode("utf-8") == text
        except UnicodeDecodeError:
            valid = False
    elif line.get("encoding") == "byte-escaped":
        valid = "".join(chr(byte) if 0x20 <= byte <= 0x7e and byte != 92
                         else f"\\x{byte:02x}" for byte in data) == text
    else:
        valid = False
    return (start, end) if valid else None


def grade_task(task: dict, events: list[dict], roots: dict[str, Path]) -> dict:
    """Grade emitted metadata and complete original-byte source evidence.

    Labels are used here only after the solver events have been captured.  The
    returned summary intentionally contains no inferred success beyond bytes
    verified against the frozen files.
    """
    labels = task["labels"]
    discovered: set[int] = set()
    evidence: list[dict] = []
    for raw_event in events:
        event = raw_event if isinstance(raw_event, dict) else {}
        response, complete = _event_response(event)
        operation = event.get("operation", response.get("operation", "search"))
        if event.get("status") not in (None, "ok", "success"):
            continue
        if operation in ("search", "more"):
            for hit in response.get("hits", []):
                member, path = _member_path(hit, roots)
                for index, label in enumerate(labels):
                    if member == task["corpus"] and path == label["path"]:
                        discovered.add(index)
        if operation != "show" or not complete or not response.get("verified"):
            continue
        member, path = _member_path(response, roots)
        if member != task["corpus"] or not response.get("revision"):
            continue
        repository = response.get("repository")
        if repository is not None and os.fsdecode(unquote_to_bytes(repository)) != str(roots[member]):
            continue
        try:
            source = _source_path(roots[member], path)
            data = source.read_bytes()
        except (KeyError, ManifestError, OSError):
            continue
        for line in response.get("lines", []):
            span = _valid_line(data, line)
            if span:
                evidence.append(dict(member=member, path=path, start=span[0], end=span[1]))
    evidenced = []
    for label in labels:
        covered = _fully_covered(label, evidence)
        evidenced.append(covered)
    missed = [copy.deepcopy(label) for label, covered in zip(labels, evidenced) if not covered]
    return {
        "metadata_discovered": len(discovered),
        "source_evidenced": sum(evidenced),
        "relevant_total": len(labels),
        "status": "complete" if all(evidenced) else "incomplete",
        "missed_evidence": missed,
    }


def _fully_covered(label: dict, evidence: list[dict]) -> bool:
    cursor = label["start"]
    for span in sorted((item for item in evidence
                        if item["path"] == label["path"]), key=lambda item: item["start"]):
        if span["start"] <= cursor:
            cursor = max(cursor, span["end"])
    return cursor >= label["end"]
