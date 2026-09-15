"""Replay the frozen twelve-task retrieval set across the four arms.

The search page is capped at 600 ``o200k_base`` tokens.  Every emitted top-hit
handle is followed with ``show`` so source evidence is checked against the
current frozen bytes rather than inferred from compact search metadata.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sqlite3
import time
import tomllib
from typing import Callable, Iterable
from urllib.parse import unquote_to_bytes

from .replay import event_limits_exceeded
from .schema import (
    ARMS,
    MAX_EMITTED_TOKENS,
    MAX_OUTPUT_TOKENS_PER_CALL,
    MAX_TOOL_CALLS,
    TASK_TIMEOUT_SECONDS,
    WORKFLOW_VERSION,
    grade_task,
    load_manifest,
    solver_task,
    validate_manifest,
    verify_snapshot,
    _member_path,
    _valid_line,
)


PAGE_BUDGET = 600
SHOW_BUDGET = MAX_OUTPUT_TOKENS_PER_CALL
PAGE_LIMIT = 10
DEFAULT_ARTIFACT_DIR = Path("/home/josh/.cache/codex-tmp/gpu-search-evaluation/retrieval")
DEFAULT_EVALUATION_DIR = Path("/home/josh/.cache/codex-tmp/gpu-search-evaluation")
DEFAULT_WORKSPACE = DEFAULT_EVALUATION_DIR / "space.toml"
DEFAULT_INFERENCE_CONFIG = DEFAULT_EVALUATION_DIR / "inference.toml"
DEFAULT_OLD_BINARY = DEFAULT_EVALUATION_DIR / "trufflepig-oldlexical"
DEFAULT_NEW_BINARY = Path("target/release/trufflepig")
DEFAULT_NEW_CACHE = DEFAULT_EVALUATION_DIR / "cache"
DEFAULT_OLD_CACHE = DEFAULT_EVALUATION_DIR / "old-cache"


class RetrievalError(ValueError):
    """Raised when a replay cannot satisfy its frozen-input contract."""


def exact_token_counter() -> Callable[[str], int]:
    """Load the declared tokenizer, failing rather than estimating output."""
    try:
        import tiktoken
    except ImportError as error:  # pragma: no cover - depends on the runner venv
        raise RetrievalError("tiktoken with o200k_base is required") from error
    encoder = tiktoken.get_encoding("o200k_base")
    return lambda text: len(encoder.encode_ordinary(text))


def workspace_roots(workspace: Path) -> dict[str, Path]:
    """Resolve configured workspace members without scanning or copying them."""
    try:
        document = tomllib.loads(workspace.read_text())
        members = document["members"]
    except (OSError, KeyError, TypeError, tomllib.TOMLDecodeError) as error:
        raise RetrievalError(f"cannot load workspace: {error}") from error
    if not isinstance(members, dict) or not members:
        raise RetrievalError("workspace has no members")
    roots = {}
    for name, item in members.items():
        if not isinstance(item, dict) or not isinstance(item.get("path"), str):
            raise RetrievalError(f"invalid workspace member: {name}")
        root = Path(item["path"]).expanduser()
        roots[name] = (root if root.is_absolute() else workspace.parent / root).resolve()
    return roots


def _path_is_nested(left: Path, right: Path) -> bool:
    try:
        left.relative_to(right)
        return True
    except ValueError:
        return False


def validate_cache_separation(old_cache: Path, new_cache: Path) -> None:
    """Reject overlapping old/new caches before either command can write."""
    old_cache = old_cache.expanduser().resolve()
    new_cache = new_cache.expanduser().resolve()
    if old_cache == new_cache or _path_is_nested(old_cache, new_cache) \
            or _path_is_nested(new_cache, old_cache):
        raise RetrievalError("old and new retrieval caches must be separate")


def _cache_looks_indexed(cache: Path, roots: dict[str, Path]) -> bool:
    """Recognize published indexes for every configured root.

    Staging databases are deliberately ignored; a file count alone can mistake
    an interrupted publication for a complete workspace.
    """
    members = cache / "members"
    if not members.is_dir():
        return False
    indexed_roots = set()
    for path in members.glob("*/index.sqlite3"):
        try:
            with sqlite3.connect(f"file:{path}?mode=ro", uri=True, timeout=1) as connection:
                row = connection.execute(
                    "SELECT value FROM meta WHERE key = 'root'"
                ).fetchone()
                generation = connection.execute(
                    "SELECT value FROM meta WHERE key = 'generation'"
                ).fetchone()
            if row and generation and int(generation[0]) > 0:
                indexed_roots.add(Path(row[0]).resolve())
        except (OSError, sqlite3.Error, ValueError):
            continue
    return indexed_roots >= {root.resolve() for root in roots.values()}


def _decode_path(value) -> str | None:
    if isinstance(value, dict):
        value = value.get("uri") or value.get("path")
    if not isinstance(value, str):
        return None
    if value.startswith("file:"):
        from urllib.parse import unquote_to_bytes, urlsplit

        parsed = urlsplit(value)
        if parsed.scheme == "file":
            value = parsed.path
            if parsed.netloc and parsed.netloc != "localhost":
                value = f"//{parsed.netloc}{value}"
        else:
            value = value[5:]
        return os.fsdecode(unquote_to_bytes(value))
    return value


def hit_member_path(hit: dict, roots: dict[str, Path]) -> tuple[str | None, str | None]:
    """Normalize a compact hit to its configured member and root-relative path."""
    path = _decode_path(hit.get("path") or hit.get("file") or hit.get("uri"))
    member = hit.get("member")
    if path is None:
        return member, None
    candidate = Path(path)
    if member in roots and candidate.is_absolute():
        try:
            return member, str(candidate.relative_to(roots[member]))
        except ValueError:
            return member, path
    if candidate.is_absolute():
        for name, root in roots.items():
            try:
                return name, str(candidate.relative_to(root))
            except ValueError:
                continue
    return member, path


def _label_files(task: dict) -> set[tuple[str, str]]:
    return {(task["corpus"], label["path"]) for label in task["labels"]}


def page_recall(task: dict, response: dict, roots: dict[str, Path]) -> dict:
    """Calculate file recall from only the hits emitted in the first page."""
    hits = response.get("hits", []) if isinstance(response, dict) else []
    expected = _label_files(task)
    metrics = {}
    for cutoff in (5, 10):
        found = {hit_member_path(hit, roots) for hit in hits[:cutoff]
                 if isinstance(hit, dict)}
        metrics[f"file_recall_at_{cutoff}"] = (
            len(expected & found) / len(expected) if expected else 0.0
        )
    metrics["hits_emitted"] = len(hits)
    metrics["page_complete"] = not bool(response.get("truncated"))
    return metrics


def show_byte_validation(task: dict, events: list[dict], roots: dict[str, Path]) -> dict:
    """Check delivered show lines against source bytes and label overlaps.

    A compact hit is metadata only.  This summary credits a hit for relevance
    only when a complete ``show`` response contains at least one line whose
    emitted text matches the current source bytes and overlaps a frozen label.
    Full label coverage remains available as ``label_coverage`` below.
    """
    valid_shows = 0
    relevant_shows = 0
    valid_lines = []
    for event in events:
        if event.get("operation") != "show" or event.get("status") not in ("ok", "success") \
                or not event.get("complete_delivery"):
            continue
        response = event.get("response", {})
        if not isinstance(response, dict) or not response.get("verified"):
            continue
        member, path = _member_path(response, roots)
        if member not in roots or not response.get("revision"):
            continue
        repository = response.get("repository")
        if (not isinstance(repository, str)
                or os.fsdecode(unquote_to_bytes(repository)) != str(roots[member])):
            continue
        if path is None:
            continue
        try:
            source = (roots[member] / path).resolve()
            source.relative_to(roots[member])
            data = source.read_bytes()
        except (AttributeError, OSError, TypeError, ValueError):
            continue
        current_lines = [_valid_line(data, line) for line in response.get("lines", [])
                         if isinstance(line, dict)]
        current_lines = [span for span in current_lines if span is not None]
        if not current_lines:
            continue
        valid_shows += 1
        valid_lines.extend((member, path, start, end) for start, end in current_lines)
        if any(member == task["corpus"] and path == label["path"]
               and start < label["end"] and label["start"] < end
               for member, path, start, end in valid_lines[-len(current_lines):]
               for label in task["labels"]):
            relevant_shows += 1
    overlapped = sum(any(member == task["corpus"] and path == label["path"]
                         and start < label["end"] and label["start"] < end
                         for member, path, start, end in valid_lines)
                     for label in task["labels"])
    return {
        "valid_show_count": valid_shows,
        "relevant_show_count": relevant_shows,
        "valid_line_count": len(valid_lines),
        "label_overlap_count": overlapped,
        "label_overlap_recall": (overlapped / len(task["labels"])
                                  if task["labels"] else 0.0),
    }


def _json_response(stdout: bytes) -> tuple[dict | None, str | None]:
    try:
        text = stdout.decode("utf-8")
    except UnicodeDecodeError as error:
        return None, f"stdout is not UTF-8: {error}"
    try:
        value = json.loads(text)
    except json.JSONDecodeError as error:
        return None, f"stdout is not JSON: {error}"
    if not isinstance(value, dict):
        return None, "stdout JSON is not an object"
    return value, None


def _raw_text(data: bytes) -> str | None:
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError:
        return None


class RetrievalRunner:
    """Capture complete CLI responses while measuring monotonic latency."""

    def __init__(self, binary: Path, workspace: Path, root: Path, cache: Path,
                 arm: str, inference_config: Path, counter: Callable[[str], int],
                 timeout: int = TASK_TIMEOUT_SECONDS):
        self.binary = binary.expanduser().resolve()
        self.workspace = workspace.expanduser().resolve()
        self.root = root.expanduser().resolve()
        self.cache = cache.expanduser().resolve()
        self.arm = arm
        self.inference_config = inference_config.expanduser().resolve()
        self.counter = counter
        self.timeout = timeout
        self._task_deadline: float | None = None

    def begin_task(self) -> None:
        """Start the per-task monotonic deadline used by every subprocess call."""
        self._task_deadline = time.monotonic() + TASK_TIMEOUT_SECONDS

    @property
    def semantic_flags(self) -> list[str]:
        if self.arm == "newfilefirstlexical":
            return ["--no-sem"]
        if self.arm == "cuda":
            return ["--sem"]
        if self.arm == "cuda_rerank":
            return ["--sem", "--rerank"]
        return []

    @property
    def daemon_flags(self) -> list[str]:
        # Searches use the published workspace/member indexes.  A daemon keeps
        # repeated task queries from rescanning every repository; cold indexes
        # are built explicitly by ``_index_events`` with --no-daemon.
        return []

    def command(self, operation: str, argument: str, budget: int, *, member: str | None = None,
                no_daemon: bool | None = None, semantic: bool | None = None) -> list[str]:
        semantic_flags = self.semantic_flags if semantic is None else (
            ["--sem"] if semantic else ["--no-sem"])
        daemon_flags = self.daemon_flags if no_daemon is None else (
            ["--no-daemon"] if no_daemon else [])
        command = [str(self.binary), "--workspace", str(self.workspace), "--root", str(self.root),
                   "--cache", str(self.cache), "--json", "--budget", str(budget),
                   "--limit", str(PAGE_LIMIT)]
        if member is not None:
            command += ["--member", member]
        command += [*semantic_flags, *daemon_flags, operation]
        if argument:
            command.append(argument)
        return command

    def __call__(self, operation: str, argument: str, *, budget: int,
                 member: str | None = None, no_daemon: bool | None = None,
                 semantic: bool | None = None) -> dict:
        command = self.command(operation, argument, budget, member=member,
                               no_daemon=no_daemon, semantic=semantic)
        environment = os.environ.copy()
        environment["TRUFFLEPIG_INFERENCE_CONFIG"] = str(self.inference_config)
        started = time.monotonic()
        timed_out = False
        try:
            remaining = (self.timeout if self._task_deadline is None else
                         max(0.001, self._task_deadline - time.monotonic()))
            completed = subprocess.run(command, capture_output=True, check=False,
                                       timeout=min(self.timeout, remaining), env=environment)
            stdout, stderr = completed.stdout, completed.stderr
            return_code = completed.returncode
        except subprocess.TimeoutExpired as error:
            stdout = error.stdout or b""
            stderr = error.stderr or b""
            return_code = None
            timed_out = True
        latency = max(0.0, time.monotonic() - started)
        response, parse_error = _json_response(stdout)
        status = response.get("status") if response else None
        if not status:
            status = "timeout" if timed_out else ("ok" if return_code == 0 else "error")
        stdout_text, stderr_text = _raw_text(stdout), _raw_text(stderr)
        stdout_tokens = self.counter(stdout_text) if stdout_text is not None else None
        stderr_tokens = self.counter(stderr_text) if stderr_text is not None else None
        return {
            "operation": operation,
            "argument": argument,
            "command": command,
            "status": status,
            "complete_delivery": not timed_out,
            "return_code": return_code,
            "response": response or {},
            "parse_error": parse_error,
            "raw_stdout": stdout_text,
            "raw_stderr": stderr_text,
            "raw_stdout_b64": base64.b64encode(stdout).decode("ascii"),
            "raw_stderr_b64": base64.b64encode(stderr).decode("ascii"),
            "stdout_bytes": len(stdout),
            "stderr_bytes": len(stderr),
            "stdout_emitted_tokens": stdout_tokens,
            "stderr_emitted_tokens": stderr_tokens,
            "emitted_tokens": (stdout_tokens + stderr_tokens
                                if stdout_tokens is not None and stderr_tokens is not None
                                else None),
            "tokenizer": "o200k_base" if stdout_tokens is not None
            and stderr_tokens is not None else None,
            "latency_seconds": latency,
            "elapsed_seconds": latency,
        }


def _usage(events: Iterable[dict]) -> dict:
    events = list(events)
    token_values = [event.get("emitted_tokens") for event in events]
    known = all(value is not None for value in token_values)
    return {
        "tool_calls": len(events),
        "stdout_bytes": sum(event.get("stdout_bytes", 0) for event in events),
        "stderr_bytes": sum(event.get("stderr_bytes", 0) for event in events),
        "emitted_tokens": sum(token_values) if known else None,
        "tokenizer": "o200k_base" if known else None,
        "latency_seconds": sum(event.get("latency_seconds", 0.0) for event in events),
        "token_cost_censored": not known,
    }


def _snapshot_error(task: dict, roots: dict[str, Path]) -> str | None:
    try:
        verify_snapshot(task, roots[task["corpus"]], require_revision=False)
    except (OSError, ValueError, KeyError) as error:
        return str(error)
    return None


def run_task(task: dict, roots: dict[str, Path], runner: RetrievalRunner) -> dict:
    """Run one search and its top-hit shows, fail-closed on source drift."""
    before_error = _snapshot_error(task, roots)
    events = []
    if before_error is None:
        if hasattr(runner, "begin_task"):
            runner.begin_task()
        events.append(runner("search", task["query"], budget=PAGE_BUDGET))
        search_event = events[0]
        response = search_event["response"]
        if search_event["status"] in ("ok", "success") and search_event["complete_delivery"]:
            seen_handles = set()
            for hit in response.get("hits", [])[:PAGE_LIMIT]:
                if not isinstance(hit, dict):
                    continue
                handle = hit.get("handle")
                if not handle or handle in seen_handles:
                    continue
                seen_handles.add(handle)
                if len(events) >= MAX_TOOL_CALLS:
                    break
                events.append(runner("show", handle, budget=SHOW_BUDGET))
    after_error = _snapshot_error(task, roots)
    snapshot_error = before_error or after_error
    search_response = events[0]["response"] if events and events[0]["operation"] == "search" else {}
    recall = page_recall(task, search_response, roots) if snapshot_error is None else {
        "file_recall_at_5": None,
        "file_recall_at_10": None,
        "hits_emitted": 0,
        "page_complete": False,
    }
    grade = grade_task(task, events, roots) if snapshot_error is None else {
        "metadata_discovered": 0,
        "source_evidenced": 0,
        "relevant_total": len(task["labels"]),
        "status": "invalid_snapshot",
        "missed_evidence": task["labels"],
    }
    byte_validation = show_byte_validation(task, events, roots) if snapshot_error is None else {
        "valid_show_count": 0,
        "relevant_show_count": 0,
        "valid_line_count": 0,
        "label_overlap_count": 0,
        "label_overlap_recall": None,
    }
    usage = _usage(events)
    exceeded = []
    if usage["tool_calls"] > MAX_TOOL_CALLS:
        exceeded.append("maximum_tool_calls")
    if usage["emitted_tokens"] is not None and usage["emitted_tokens"] > MAX_EMITTED_TOKENS:
        exceeded.append("maximum_emitted_tokens")
    if usage["latency_seconds"] > TASK_TIMEOUT_SECONDS:
        exceeded.append("task_timeout_seconds")
    for event in events:
        exceeded.extend(event_limits_exceeded({"usage": {
            "emitted_tokens": event.get("emitted_tokens")
        }}))
    if snapshot_error:
        exceeded.append("invalid_snapshot")
    exceeded = list(dict.fromkeys(exceeded))
    if snapshot_error:
        status = "invalid_snapshot"
    elif exceeded:
        status = "incomplete"
    elif not events or events[0]["status"] not in ("ok", "success"):
        status = "unavailable"
    elif any(event["status"] not in ("ok", "success") for event in events):
        status = "error"
    else:
        status = "ok"
    return {
        "record_type": "gpu_navigation_retrieval_task",
        "task_id": task["id"],
        "corpus": task["corpus"],
        "task": solver_task(task, arm=runner.arm),
        "snapshot_valid": snapshot_error is None,
        "snapshot_error": snapshot_error,
        "page": recall,
        "show_validation": {
            **byte_validation,
            "source_evidenced": grade["source_evidenced"],
            "relevant_total": grade["relevant_total"],
            "status": grade["status"],
            "missed_evidence": grade["missed_evidence"],
            "label_coverage": grade,
        },
        "usage": usage,
        "limits_exceeded": exceeded,
        "status": status,
        "events": events,
    }


def _index_events(runner: RetrievalRunner, roots: dict[str, Path]) -> list[dict]:
    members = list(roots)
    if _cache_looks_indexed(runner.cache, roots):
        return []
    runner.cache.mkdir(parents=True, exist_ok=True)
    # ``index`` is an owner-scoped workspace command.  Run it once per member
    # so a cold workspace cache cannot leave non-home members warming forever.
    semantic = False if runner.arm != "oldlexical" else None
    return [runner("index", "", budget=SHOW_BUDGET, member=member,
                   no_daemon=True, semantic=semantic) for member in members]


def run_arm(manifest: dict, arm: str, roots: dict[str, Path], *, binary: Path,
            workspace: Path, root: Path, cache: Path, inference_config: Path,
            counter: Callable[[str], int]) -> dict:
    """Run one arm in its own cache and retain index failures as artifacts."""
    if arm not in ARMS:
        raise RetrievalError(f"unknown retrieval arm: {arm}")
    runner = RetrievalRunner(binary, workspace, root, cache, arm, inference_config, counter)
    index = _index_events(runner, roots)
    records = []
    if not index or all(event.get("status") in ("ok", "success") for event in index):
        for task in manifest["tasks"]:
            records.append(run_task(task, roots, runner))
    else:
        for task in manifest["tasks"]:
            records.append({
                "record_type": "gpu_navigation_retrieval_task",
                "task_id": task["id"],
                "corpus": task["corpus"],
                "task": solver_task(task, arm=arm),
                "snapshot_valid": False,
                "snapshot_error": "index failed",
                "page": {"file_recall_at_5": None, "file_recall_at_10": None,
                         "hits_emitted": 0, "page_complete": False},
                "show_validation": {
                    "valid_show_count": 0,
                    "relevant_show_count": 0,
                    "valid_line_count": 0,
                    "label_overlap_count": 0,
                    "label_overlap_recall": None,
                    "source_evidenced": 0,
                    "relevant_total": len(task["labels"]),
                    "status": "unavailable",
                    "missed_evidence": task["labels"],
                    "label_coverage": {"status": "unavailable", "source_evidenced": 0,
                                        "relevant_total": len(task["labels"])},
                },
                "usage": {"tool_calls": 0, "emitted_tokens": 0,
                          "latency_seconds": 0.0, "tokenizer": "o200k_base",
                          "token_cost_censored": False},
                "limits_exceeded": ["index_error"],
                "status": "unavailable",
                "events": [],
            })
    usage = _usage([event for record in records for event in record["events"]])
    valid = [record for record in records if record["snapshot_valid"]]
    def mean(key: str):
        values = [record["page"][key] for record in valid if record["page"][key] is not None]
        return sum(values) / len(values) if values else None
    summary = {
        "tasks": len(records),
        "snapshot_invalid": sum(not record["snapshot_valid"] for record in records),
        "unavailable": sum(record["status"] == "unavailable" for record in records),
        "errors": sum(record["status"] in ("error", "incomplete") for record in records),
        "truncated_searches": sum(not record["page"]["page_complete"] for record in valid),
        "misses": sum(record["page"].get("file_recall_at_10") == 0 for record in valid),
        "mean_file_recall_at_5": mean("file_recall_at_5"),
        "mean_file_recall_at_10": mean("file_recall_at_10"),
        "mean_latency_seconds": (usage["latency_seconds"] / usage["tool_calls"]
                                  if usage["tool_calls"] else 0.0),
        "usage": usage,
    }
    return {
        "record_type": "gpu_navigation_retrieval_arm",
        "schema_version": 2,
        "workflow_version": WORKFLOW_VERSION,
        "arm": arm,
        "binary": str(binary),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()
        if binary.is_file() else None,
        "workspace": str(workspace),
        "cache": str(cache),
        "page_budget_tokens": PAGE_BUDGET,
        "show_budget_tokens": SHOW_BUDGET,
        "summary": summary,
        "index_events": index,
        "records": records,
    }


def _brief_summary(summary: dict) -> dict:
    """Keep command output useful while per-call raw data stays in arm files."""
    usage = summary.get("usage", {})
    return {
        key: summary.get(key) for key in (
            "tasks", "snapshot_invalid", "unavailable", "errors", "truncated_searches",
            "misses", "mean_file_recall_at_5", "mean_file_recall_at_10",
        )
    } | {"tool_calls": usage.get("tool_calls"),
         "emitted_tokens": usage.get("emitted_tokens"),
         "latency_seconds": usage.get("latency_seconds")}


def run_retrieval(manifest: dict, roots: dict[str, Path], *, arms: Iterable[str],
                  binaries: dict[str, Path], workspace: Path, root: Path,
                  caches: dict[str, Path], inference_config: Path,
                  output_dir: Path, counter: Callable[[str], int]) -> dict:
    """Run selected arms and write one raw, self-contained artifact per arm."""
    output_dir.mkdir(parents=True, exist_ok=True)
    arms = list(arms)
    if "oldlexical" in arms and "newfilefirstlexical" in arms:
        validate_cache_separation(caches["oldlexical"], caches["newfilefirstlexical"])
    artifacts = {}
    summaries = {}
    for arm in arms:
        report = run_arm(manifest, arm, roots, binary=binaries[arm], workspace=workspace,
                         root=root,
                         cache=caches[arm], inference_config=inference_config, counter=counter)
        path = output_dir / f"{arm}.json"
        path.write_text(json.dumps(report, indent=2) + "\n")
        artifacts[arm] = str(path)
        summaries[arm] = _brief_summary(report["summary"])
    # A later CUDA-only invocation completes the same replay directory.  Keep
    # already captured CPU artifacts discoverable in the combined index.
    for arm in ARMS:
        path = output_dir / f"{arm}.json"
        if path.is_file() and arm not in artifacts:
            artifacts[arm] = str(path)
            try:
                summaries[arm] = _brief_summary(json.loads(path.read_text())["summary"])
            except (OSError, KeyError, TypeError, json.JSONDecodeError):
                summaries[arm] = None
    combined = {
        "schema_version": 2,
        "workflow_version": WORKFLOW_VERSION,
        "label": "development first-600-token retrieval replay",
        "split": "development",
        "held_out_claim": False,
        "artifacts": artifacts,
        "summaries": summaries,
    }
    combined_path = output_dir / "summary.json"
    combined["summary_artifact"] = str(combined_path)
    combined_path.write_text(json.dumps(combined, indent=2) + "\n")
    return combined


def _parse_arms(value: str) -> list[str]:
    arms = [item.strip() for item in value.split(",") if item.strip()]
    if not arms or any(item not in ARMS for item in arms):
        raise argparse.ArgumentTypeError(f"arms must be drawn from: {', '.join(ARMS)}")
    return arms


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path,
                        default=Path(__file__).with_name("manifest.json"))
    parser.add_argument("--workspace", type=Path, default=DEFAULT_WORKSPACE)
    parser.add_argument("--root", type=Path,
                        help="workspace home; defaults to the first configured member")
    parser.add_argument("--old-binary", type=Path, default=DEFAULT_OLD_BINARY)
    parser.add_argument("--new-binary", type=Path, default=DEFAULT_NEW_BINARY)
    parser.add_argument("--new-cache", type=Path, default=DEFAULT_NEW_CACHE)
    parser.add_argument("--old-cache", type=Path, default=DEFAULT_OLD_CACHE)
    parser.add_argument("--inference-config", type=Path, default=DEFAULT_INFERENCE_CONFIG)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_ARTIFACT_DIR)
    parser.add_argument("--arms", type=_parse_arms,
                        default=["oldlexical", "newfilefirstlexical"],
                        help="comma-separated arms; add cuda/cuda_rerank only "
                             "after their lanes are ready")
    args = parser.parse_args()
    try:
        manifest = load_manifest(args.manifest)
        validate_manifest(manifest)
        workspace = args.workspace.expanduser().resolve()
        roots = workspace_roots(workspace)
        root = (args.root or next(iter(roots.values()))).expanduser().resolve()
        if root not in roots.values():
            raise RetrievalError("--root must be an exact configured workspace member")
        binaries = {"oldlexical": args.old_binary, "newfilefirstlexical": args.new_binary,
                    "cuda": args.new_binary, "cuda_rerank": args.new_binary}
        caches = {"oldlexical": args.old_cache, "newfilefirstlexical": args.new_cache,
                  "cuda": args.new_cache, "cuda_rerank": args.new_cache}
        for arm in args.arms:
            if not binaries[arm].is_file():
                raise RetrievalError(f"binary unavailable for {arm}: {binaries[arm]}")
        report = run_retrieval(manifest, roots, arms=args.arms, binaries=binaries,
                               workspace=workspace, root=root, caches=caches,
                               inference_config=args.inference_config,
                               output_dir=args.output_dir, counter=exact_token_counter())
    except (OSError, ValueError, KeyError, RetrievalError) as error:
        parser.exit(1, f"retrieval replay failed: {error}\n")
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
