"""Runner-neutral replay and optional CLI capture for GPU navigation trials."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import time

from .schema import (
    ARMS,
    MAX_EMITTED_TOKENS,
    MAX_OUTPUT_TOKENS_PER_CALL,
    MAX_TOOL_CALLS,
    TASK_TIMEOUT_SECONDS,
    WORKFLOW_VERSION,
    corpus_roots,
    grade_task,
    load_manifest,
    solver_task,
    validate_manifest,
    verify_snapshot,
)


def exact_counter():
    """Return the declared tokenizer, or ``None`` when it is unavailable."""
    try:
        import tiktoken
    except ImportError:
        return None
    tokenizer = tiktoken.get_encoding("o200k_base")
    return lambda text: len(tokenizer.encode_ordinary(text))


class ArmRunner:
    """Capture one Trufflepig arm while preserving complete pipe delivery."""

    def __init__(self, binary: Path | str, workspace: Path | str, root: Path | str,
                 cache: Path | str, *, budget: int = MAX_OUTPUT_TOKENS_PER_CALL,
                 timeout: int = TASK_TIMEOUT_SECONDS, counter=None):
        budget = min(budget, MAX_OUTPUT_TOKENS_PER_CALL)
        self.prefix = [str(Path(binary).expanduser()), "--workspace", str(workspace),
                       "--root", str(root), "--cache", str(cache), "--no-daemon",
                       "--json", "--budget", str(budget)]
        self.timeout = timeout
        self.budget = budget
        self.counter = counter

    def __call__(self, operation: str, argument: str) -> tuple[dict, str, bool, dict]:
        started = time.perf_counter()
        try:
            result = subprocess.run(self.prefix + [operation, argument], capture_output=True,
                                    check=False, timeout=self.timeout)
            stdout, stderr, complete = result.stdout, result.stderr, True
            code = result.returncode
        except subprocess.TimeoutExpired as error:
            stdout = error.stdout or b""
            stderr = error.stderr or b""
            complete, code = False, None
        try:
            text = stdout.decode("utf-8")
            value = json.loads(text)
            response = value if isinstance(value, dict) else {"status": "invalid_response"}
        except (UnicodeDecodeError, json.JSONDecodeError):
            text, response = None, {"status": "invalid_response"}
        status = response.get("status", "ok" if code == 0 else "error")
        if not complete:
            status = "timeout"
        tokens = self.counter(text) if self.counter and text is not None else None
        if tokens is not None and tokens > self.budget:
            status = "budget_exceeded"
        usage = {
            "tool_calls": 1,
            "stdout_bytes": len(stdout),
            "stderr_bytes": len(stderr),
            "elapsed_seconds": time.perf_counter() - started,
            "emitted_tokens": tokens,
            "tokenizer": "o200k_base" if tokens is not None else None,
            "token_cost_censored": not complete or tokens is None,
        }
        return response, status, complete, usage


def arm_spec(manifest: dict, arm: str) -> dict:
    if arm not in ARMS:
        raise ValueError(f"unknown retrieval arm: {arm}")
    return next(item for item in manifest["arms"] if item["id"] == arm)


def aggregate_usage(events: list[dict]) -> dict:
    usage = [event.get("usage", {}) for event in events]
    token_values = [item.get("delivered_emitted_tokens",
                              item.get("emitted_tokens", item.get("output_tokens")))
                    for item in usage]
    known = all(value is not None for value in token_values)
    input_values = [item.get("input_tokens") for item in usage]
    input_known = all(value is not None for value in input_values)
    return {
        "tool_calls": sum(item.get("tool_calls", 0) for item in usage),
        "stdout_bytes": sum(item.get("stdout_bytes", 0) for item in usage),
        "stderr_bytes": sum(item.get("stderr_bytes", 0) for item in usage),
        "elapsed_seconds": sum(item.get("elapsed_seconds", 0) for item in usage),
        "emitted_tokens": sum(token_values) if known else None,
        "tokenizer": "o200k_base" if known else None,
        "token_cost_censored": not known or any(item.get("token_cost_censored", True)
                                                for item in usage),
        "input_tokens": sum(input_values) if input_known else None,
    }


def limits_exceeded(usage: dict) -> list[str]:
    exceeded = []
    if usage.get("tool_calls", 0) > MAX_TOOL_CALLS:
        exceeded.append("maximum_tool_calls")
    tokens = usage.get("emitted_tokens")
    if tokens is not None and tokens > MAX_EMITTED_TOKENS:
        exceeded.append("maximum_emitted_tokens")
    if usage.get("elapsed_seconds", 0) > TASK_TIMEOUT_SECONDS:
        exceeded.append("task_timeout_seconds")
    return exceeded


def event_limits_exceeded(event: dict) -> list[str]:
    """Return per-call violations recorded by a budgeted adapter."""
    usage = event.get("usage", {}) if isinstance(event, dict) else {}
    tokens = usage.get("delivered_emitted_tokens",
                      usage.get("emitted_tokens", usage.get("output_tokens")))
    if tokens is not None and tokens > MAX_OUTPUT_TOKENS_PER_CALL:
        return ["maximum_output_tokens_per_call"]
    return []


def replay_trace(manifest_or_path: dict | Path | str, trace: dict | Path | str,
                 *, roots: dict[str, Path] | None = None) -> dict:
    """Grade captured paired trials without exposing labels to the solver.

    A trace contains responses already observed by an agent adapter.  This
    function never performs oracle reads and never counts a hidden read: only
    ``show`` responses present in the trace can provide source evidence.
    """
    manifest = (load_manifest(manifest_or_path) if not isinstance(manifest_or_path, dict)
                else manifest_or_path)
    validate_manifest(manifest)
    value = _load_json(trace)
    if value.get("schema_version") != 2 or value.get("workflow_version") != WORKFLOW_VERSION:
        raise ValueError("trace does not use gpu navigation schema version 2")
    roots = roots or corpus_roots(manifest)
    tasks = {task["id"]: task for task in manifest["tasks"]}
    records = []
    for trial in value.get("trials", []):
        task = tasks.get(trial.get("task_id"))
        if task is None:
            raise ValueError(f"trace references unknown task: {trial.get('task_id')}")
        solver_arm = trial.get("solver_arm")
        retrieval_arm = trial.get("retrieval_arm")
        if solver_arm not in manifest["solver_arms"] or retrieval_arm not in ARMS:
            raise ValueError("trace has an unknown paired arm")
        events = trial.get("events", [])
        if not isinstance(events, list):
            raise ValueError("trial events must be a list")
        usage = aggregate_usage(events)
        # The parent revision remains frozen metadata, while active ranking
        # work may advance unrelated files in a shared corpus checkout. The
        # selected-file hashes below are the grading boundary for a replay.
        verify_snapshot(task, roots[task["corpus"]], require_revision=False)
        grade = grade_task(task, events, roots)
        exceeded = limits_exceeded(usage)
        for event in events:
            exceeded.extend(event_limits_exceeded(event))
        exceeded = list(dict.fromkeys(exceeded))
        records.append({
            "record_type": "gpu_navigation_trial",
            "schema_version": 2,
            "workflow_version": WORKFLOW_VERSION,
            "task_id": task["id"],
            "corpus": task["corpus"],
            "solver_arm": solver_arm,
            "retrieval_arm": retrieval_arm,
            "task": solver_task(task, arm=retrieval_arm),
            "usage": usage,
            "limits_exceeded": exceeded,
            "grade": grade,
            "status": "incomplete" if exceeded else grade["status"],
        })
    return {
        "schema_version": 2,
        "workflow_version": WORKFLOW_VERSION,
        "label": "development paired navigation replay",
        "split": "development",
        "held_out_claim": False,
        "limits": {
            "maximum_tool_calls": MAX_TOOL_CALLS,
            "maximum_emitted_tokens": MAX_EMITTED_TOKENS,
            "maximum_output_tokens_per_call": MAX_OUTPUT_TOKENS_PER_CALL,
            "task_timeout_seconds": TASK_TIMEOUT_SECONDS,
        },
        "records": records,
    }


def _load_json(value: dict | Path | str) -> dict:
    if isinstance(value, dict):
        return value
    try:
        parsed = json.loads(Path(value).read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot load trial trace: {error}") from error
    if not isinstance(parsed, dict):
        raise ValueError("trial trace must be an object")
    return parsed


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", type=Path)
    parser.add_argument("--manifest", type=Path,
                        default=Path(__file__).with_name("manifest.json"))
    args = parser.parse_args()
    try:
        report = replay_trace(args.manifest, args.trace)
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f"gpu navigation replay failed: {error}\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
