"""Budgeted subprocess wrapper for one agent tool call.

The wrapper persists a small trial ledger so separate tool invocations share
the twelve-call, token, and wall-clock limits. It requires ``tiktoken`` for
exact ``o200k_base`` accounting and never estimates tokens from bytes.
"""

from __future__ import annotations

import argparse
import base64
import fcntl
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

from .schema import (MAX_EMITTED_TOKENS, MAX_OUTPUT_TOKENS_PER_CALL,
                     MAX_TOOL_CALLS, TASK_TIMEOUT_SECONDS)

MAX_CAPTURE_BYTES = 1_048_576
POLL_SECONDS = 0.02


def tokenizer():
    try:
        import tiktoken
    except ImportError as error:
        raise RuntimeError("tiktoken with o200k_base is required for exact accounting") from error
    return tiktoken.get_encoding("o200k_base")


def _read_state(path: Path) -> dict:
    if not path.exists():
        return {"calls": 0, "emitted_tokens": 0, "started_at": None, "elapsed_seconds": 0.0}
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"invalid trial state: {error}") from error
    if not isinstance(value, dict):
        raise RuntimeError("trial state must be an object")
    return value


def _write_state(path: Path, state: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    handle = tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                         prefix=f".{path.name}.", delete=False)
    temporary = Path(handle.name)
    try:
        with handle:
            json.dump(state, handle, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        if temporary.exists():
            temporary.unlink()


def _append_log(path: Path, record: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as stream:
        json.dump(record, stream, sort_keys=True)
        stream.write("\n")


def _tokenize(output: bytes, encoder) -> tuple[str, int]:
    try:
        text = output.decode("utf-8")
    except UnicodeDecodeError as error:
        raise RuntimeError("tool output is not UTF-8; exact o200k_base count unavailable") from error
    tokens = encoder.encode_ordinary(text)
    return text, len(tokens)


def _clip(text: str, encoder, limit: int) -> tuple[str, int, bool]:
    encoded = encoder.encode_ordinary(text)
    if len(encoded) <= limit:
        return text, len(encoded), False
    return encoder.decode(encoded[:limit]), limit, True


def _terminate(process: subprocess.Popen) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=0.25)
    except ProcessLookupError:
        return
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            return
        process.wait(timeout=1)


def _spool_size(stream) -> int:
    return os.fstat(stream.fileno()).st_size


def _read_spool(stream) -> tuple[bytes, bool, int]:
    size = _spool_size(stream)
    stream.seek(0)
    return stream.read(MAX_CAPTURE_BYTES), size > MAX_CAPTURE_BYTES, size


def _run_bounded(command: list[str], timeout: float, directory: Path) -> tuple[
        bytes, bytes, bool, int | None, str, bool, bool, int, int, float]:
    """Run with disk-backed bounded capture and terminate runaway output."""
    directory.mkdir(parents=True, exist_ok=True)
    stdout_spool = tempfile.TemporaryFile(mode="w+b", dir=directory)
    stderr_spool = tempfile.TemporaryFile(mode="w+b", dir=directory)
    started = time.perf_counter()
    process = subprocess.Popen(command, stdout=stdout_spool, stderr=stderr_spool,
                               start_new_session=True)
    complete, output_limit, timed_out = False, False, False
    return_code = None
    deadline = started + timeout
    try:
        while True:
            return_code = process.poll()
            stdout_too_large = _spool_size(stdout_spool) > MAX_CAPTURE_BYTES
            stderr_too_large = _spool_size(stderr_spool) > MAX_CAPTURE_BYTES
            if stdout_too_large or stderr_too_large:
                output_limit = True
                _terminate(process)
                return_code = 124
                break
            if return_code is not None:
                complete = True
                break
            if time.perf_counter() >= deadline:
                timed_out = True
                _terminate(process)
                return_code = 124
                break
            time.sleep(POLL_SECONDS)
        stdout, stdout_truncated, stdout_size = _read_spool(stdout_spool)
        stderr, stderr_truncated, stderr_size = _read_spool(stderr_spool)
        output_limit = output_limit or stdout_truncated or stderr_truncated
        if output_limit:
            complete = False
    finally:
        if process.poll() is None:
            _terminate(process)
        stdout_spool.close()
        stderr_spool.close()
    status = "output_limit" if output_limit else "timeout" if timed_out else (
        "ok" if return_code == 0 else "error")
    return (stdout, stderr, complete, return_code, status, stdout_truncated,
            stderr_truncated, stdout_size, stderr_size, time.perf_counter() - started)


def run_call(state_path: Path, log_path: Path, command: list[str]) -> int:
    state_path.parent.mkdir(parents=True, exist_ok=True)
    with state_path.with_suffix(state_path.suffix + ".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        return _run_locked_call(state_path, log_path, command)


def _run_locked_call(state_path: Path, log_path: Path, command: list[str]) -> int:
    encoder = tokenizer()
    state = _read_state(state_path)
    now = time.time()
    started_at = state.get("started_at") or now
    wall_elapsed = max(0.0, now - started_at)
    used_calls = int(state.get("calls", 0))
    used_tokens = int(state.get("emitted_tokens", 0))
    if used_calls >= MAX_TOOL_CALLS:
        return _reject(log_path, "maximum_tool_calls", state)
    if used_tokens >= MAX_EMITTED_TOKENS:
        return _reject(log_path, "maximum_emitted_tokens", state)
    if wall_elapsed >= TASK_TIMEOUT_SECONDS:
        return _reject(log_path, "task_timeout_seconds", state)

    remaining = min(MAX_OUTPUT_TOKENS_PER_CALL, MAX_EMITTED_TOKENS - used_tokens)
    timeout = max(0.001, min(TASK_TIMEOUT_SECONDS - wall_elapsed, TASK_TIMEOUT_SECONDS))
    state_path.parent.mkdir(parents=True, exist_ok=True)
    try:
        (stdout, stderr, complete, return_code, status, stdout_raw_truncated,
         stderr_raw_truncated, stdout_bytes, stderr_bytes, elapsed) = _run_bounded(
             command, timeout, state_path.parent)
    except OSError as error:
        record = {"record_type": "trial_tool_call", "sequence": used_calls + 1,
                  "command": command, "status": "command_error", "error": str(error),
                  "complete_delivery": False, "output_truncated": False,
                  "observed_emitted_tokens": None, "delivered_emitted_tokens": 0,
                  "token_cost_censored": True,
                  "elapsed_seconds": 0.0}
        _append_log(log_path, record)
        state.update({"calls": used_calls + 1, "started_at": started_at,
                      "elapsed_seconds": max(0.0, time.time() - started_at),
                      "emitted_tokens": used_tokens})
        _write_state(state_path, state)
        return 124

    try:
        stdout_text, stdout_tokens = _tokenize(stdout, encoder)
        stderr_text, stderr_tokens = _tokenize(stderr, encoder)
        observed_tokens = stdout_tokens + stderr_tokens
    except RuntimeError as error:
        stdout_text, stderr_text = "", ""
        stdout_tokens = stderr_tokens = observed_tokens = None
        status = "tokenizer_error"
        stderr += f"\n{error}\n".encode()

    delivered_stdout, delivered_stdout_tokens = "", 0
    delivered_stderr, delivered_stderr_tokens = "", 0
    truncated = False
    if observed_tokens is not None:
        delivered_stdout, delivered_stdout_tokens, stdout_truncated = _clip(
            stdout_text, encoder, remaining)
        stderr_remaining = remaining - delivered_stdout_tokens
        delivered_stderr, delivered_stderr_tokens, stderr_truncated = _clip(
            stderr_text, encoder, stderr_remaining)
        truncated = stdout_truncated or stderr_truncated
        if truncated and status == "ok":
            status = "truncated"
    delivered_tokens = (delivered_stdout_tokens + delivered_stderr_tokens
                        if observed_tokens is not None else 0)
    record = {
        "record_type": "trial_tool_call",
        "sequence": used_calls + 1,
        "command": command,
        "status": status,
        "complete_delivery": complete and not truncated,
        # These are bounded raw prefixes. The size and truncation flags make
        # the capture limit explicit when a process is terminated for output.
        "stdout_b64": base64.b64encode(stdout).decode("ascii"),
        "stderr_b64": base64.b64encode(stderr).decode("ascii"),
        "stdout_bytes": stdout_bytes,
        "stderr_bytes": stderr_bytes,
        "stdout_capture_truncated": stdout_raw_truncated,
        "stderr_capture_truncated": stderr_raw_truncated,
        "delivered_stdout_bytes": len(delivered_stdout.encode("utf-8")),
        "delivered_stderr_bytes": len(delivered_stderr.encode("utf-8")),
        "observed_stdout_tokens": stdout_tokens,
        "observed_stderr_tokens": stderr_tokens,
        "observed_emitted_tokens": observed_tokens,
        "delivered_emitted_tokens": delivered_tokens,
        "tokenizer": "o200k_base" if observed_tokens is not None else None,
        "token_cost_censored": (observed_tokens is None or not complete
                                or stdout_raw_truncated or stderr_raw_truncated),
        "output_truncated": truncated,
        "elapsed_seconds": elapsed,
        "wall_elapsed_seconds": max(0.0, time.time() - started_at),
        "return_code": return_code,
    }
    _append_log(log_path, record)
    state.update({"calls": used_calls + 1, "started_at": started_at,
                  "elapsed_seconds": max(0.0, time.time() - started_at),
                  # Only bytes delivered to the solver consume the emitted
                  # output budget. Raw observations remain diagnostics above.
                  "emitted_tokens": used_tokens + delivered_tokens})
    _write_state(state_path, state)
    sys.stdout.write(delivered_stdout)
    sys.stdout.flush()
    sys.stderr.write(delivered_stderr)
    sys.stderr.flush()
    if status in ("output_limit", "tokenizer_error", "command_error"):
        return 124
    if return_code is not None:
        return return_code
    return 124


def _reject(log_path: Path, reason: str, state: dict) -> int:
    record = {"record_type": "trial_tool_call", "sequence": int(state.get("calls", 0)) + 1,
              "status": reason, "complete_delivery": False, "output_truncated": False,
              "observed_emitted_tokens": 0, "delivered_emitted_tokens": 0,
              "token_cost_censored": True, "elapsed_seconds": 0.0}
    _append_log(log_path, record)
    return 124


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("a command is required after --")
    try:
        return run_call(args.state, args.log, command)
    except (OSError, RuntimeError, ValueError) as error:
        parser.exit(2, f"trial tool failed: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
