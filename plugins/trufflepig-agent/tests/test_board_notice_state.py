"""Notice state is rewritten only when a poll or claim changes it."""
import sys
import tempfile
from pathlib import Path
import unittest
from unittest import mock

PLUGIN = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PLUGIN / "bin"))
import trufflepig_board_notices as notices  # noqa: E402


class NoticeStateWriteTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix="notice-state-")
        self.addCleanup(scratch.cleanup)
        self.state = Path(scratch.name) / "claude-notices"

    def test_rate_limited_poll_and_watermark_read_do_not_rewrite_state(self):
        with mock.patch.object(notices, "_write_state", wraps=notices._write_state) as write:
            self.assertTrue(notices.should_poll_pending(actor="claude/s", state_dir=self.state, now=1000.0))
            self.assertEqual(write.call_count, 1)
            self.assertFalse(notices.should_poll_pending(actor="claude/s", state_dir=self.state, now=1010.0))
            self.assertIsNone(notices.pending_after("claude/s", self.state))
            self.assertEqual(write.call_count, 1)
            self.assertTrue(notices.should_poll_pending(actor="claude/s", state_dir=self.state, now=1061.0))
            self.assertEqual(write.call_count, 2)


if __name__ == "__main__":
    unittest.main()
