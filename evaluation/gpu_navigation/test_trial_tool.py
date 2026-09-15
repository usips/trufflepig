import base64
from concurrent.futures import ThreadPoolExecutor
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

from . import trial_tool


class _OneByteEncoder:
    """Small deterministic stand-in so wrapper accounting is testable offline."""

    def encode_ordinary(self, text):
        return list(text.encode("utf-8"))

    def decode(self, tokens):
        return bytes(tokens).decode("utf-8")


class _CapturedStderr:
    def __init__(self):
        self.buffer = io.BytesIO()

    def write(self, value):
        return len(value)

    def flush(self):
        pass


class TrialToolTests(unittest.TestCase):
    def test_concurrent_calls_share_the_final_budget_slot(self):
        with tempfile.TemporaryDirectory(dir="/home/josh/.cache/codex-tmp") as directory:
            base = Path(directory)
            state, log = base / "state.json", base / "calls.jsonl"
            state.write_text(json.dumps({"calls": 11, "emitted_tokens": 0}))
            command = [sys.executable, "-c", "import time; time.sleep(0.05)"]
            with patch.object(trial_tool, "tokenizer", return_value=_OneByteEncoder()), \
                    ThreadPoolExecutor(max_workers=2) as pool:
                results = list(pool.map(lambda _: trial_tool.run_call(state, log, command), range(2)))
            self.assertEqual(sorted(results), [0, 124])
            self.assertEqual(json.loads(state.read_text())["calls"], 12)
            records = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(sum("command" in item for item in records), 1)

    def test_clip_preserves_exact_token_prefix(self):
        encoder = _OneByteEncoder()
        self.assertEqual(trial_tool._clip("abcdef", encoder, 3), ("abc", 3, True))
        self.assertEqual(trial_tool._clip("abc", encoder, 3), ("abc", 3, False))

    def test_call_logs_raw_output_and_delivers_per_call_cap(self):
        with tempfile.TemporaryDirectory(dir="/home/josh/.cache/codex-tmp") as directory:
            base = Path(directory)
            state, log = base / "state.json", base / "calls.jsonl"
            command = [sys.executable, "-c", "import sys; print('x' * 901, end=''); print('y' * 901, file=sys.stderr, end='')"]
            stdout, stderr = io.StringIO(), _CapturedStderr()
            with patch.object(trial_tool, "tokenizer", return_value=_OneByteEncoder()), \
                    patch.object(trial_tool.sys, "stdout", stdout), \
                    patch.object(trial_tool.sys, "stderr", stderr):
                status = trial_tool.run_call(state, log, command)
            self.assertEqual(status, 0)
            self.assertEqual(len(stdout.getvalue()), 900)
            self.assertEqual(len(stderr.buffer.getvalue()), 0)
            record = json.loads(log.read_text().splitlines()[0])
            self.assertEqual(base64.b64decode(record["stdout_b64"]), b"x" * 901)
            self.assertEqual(base64.b64decode(record["stderr_b64"]), b"y" * 901)
            self.assertEqual(record["observed_emitted_tokens"], 1802)
            self.assertEqual(record["delivered_emitted_tokens"], 900)
            self.assertTrue(record["output_truncated"])
            self.assertEqual(json.loads(state.read_text())["emitted_tokens"], 900)
            with patch.object(trial_tool, "tokenizer", return_value=_OneByteEncoder()), \
                    patch.object(trial_tool.sys, "stdout", io.StringIO()), \
                    patch.object(trial_tool.sys, "stderr", _CapturedStderr()):
                self.assertEqual(trial_tool.run_call(
                    state, log, [sys.executable, "-c", "print('z', end='')"]), 0)
            self.assertEqual(json.loads(state.read_text())["calls"], 2)
            self.assertEqual(json.loads(state.read_text())["emitted_tokens"], 901)

    def test_exhausted_state_rejects_without_running_a_command(self):
        with tempfile.TemporaryDirectory(dir="/home/josh/.cache/codex-tmp") as directory:
            base = Path(directory)
            state, log = base / "state.json", base / "calls.jsonl"
            state.write_text(json.dumps({"calls": 12, "emitted_tokens": 0,
                                         "started_at": None, "elapsed_seconds": 0}))
            with patch.object(trial_tool, "tokenizer", return_value=_OneByteEncoder()):
                self.assertEqual(trial_tool.run_call(state, log, ["does-not-exist"]), 124)
            self.assertIn("maximum_tool_calls", log.read_text())


if __name__ == "__main__":
    unittest.main()
