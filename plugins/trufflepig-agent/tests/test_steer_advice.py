"""Steering advice: subagent brief checks, trufflepig call shapes, and tip markers."""
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest import mock

PLUGIN = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PLUGIN / "hooks"))
import trufflepig_advice as advice  # noqa: E402
import trufflepig_shell as shell  # noqa: E402
import trufflepig_steer as policy  # noqa: E402

# (brief, phrase the lead is told about or None); drawn from lead briefs in agent transcripts.
BRIEFS = [
    ("Find where the web root is set. Just grep for sccache/RUSTC_WRAPPER in crates/.", "Just grep for"),
    ("grep counts are fine", "grep counts"),
    ("grep for 'custody-invariant', 'fov-threads' across docs/ crates/ tools/", "grep for"),
    ("Do not guess; grep for fn tick across crates.", "grep for"),
    ("Use rg -n to list the callers.", "Use rg"),
    ('When briefing subagents, never write "use grep" or "grep for X" in a subagent prompt.', None),
    ("Grep for 'panic' in the daemon log.", None),
    ("Run the tests, then grep for FAILED in the output.", None),
    ("The audit found `grep -rn foo src` calls in most lanes.", None),
    ("Use trufflepig-agent instead of grep for definitions.", None),
    ("Never guess the file, and never grep for callers.", None),
    ("don't grep; use trufflepig-agent", None),
    ("Summarize docs/plan.md.", None),
]


class BriefTests(unittest.TestCase):
    def test_grep_instructions_in_briefs(self):
        for brief, phrase in BRIEFS:
            with self.subTest(brief=brief):
                self.assertEqual(advice.grep_brief(brief), phrase)


class CallShapeTests(unittest.TestCase):
    def test_pipes_and_chains_hide_the_footer(self):
        cases = [("trufflepig-agent refs X 2>&1 | head -80", "piped"),
                 ("sed -n 1,2p f; echo ====; trufflepig-agent refs x 2>&1 | head -80", "piped"),
                 ("git log -3 && trufflepig-agent search x", "chained"),
                 ("trufflepig-agent search a; trufflepig-agent search b", "chained"),
                 ("cd /x && trufflepig-agent map y", None),
                 ("TRUFFLEPIG_X=1 trufflepig-agent search y 2>/dev/null", None)]
        for command, shape in cases:
            with self.subTest(command=command):
                self.assertEqual(advice.call_shape(command), shape)

    def test_segments_report_stdout_redirection(self):
        redirected = {"cat f > out": True, "cat f >> out": True, "cat f &> out": True, "cat f 1> out": True,
                      "cat f 2> err": False, "cat f 2>&1": False, "cat f >&2": False, "cat f": False}
        for command, expected in redirected.items():
            with self.subTest(command=command):
                self.assertEqual(shell.segments(command)[0].redirected, expected)
        first, second = shell.segments("cat f > out | sed -n 1p")
        self.assertEqual((first.redirected, second.redirected, second.piped), (True, False, True))


class TipMarkerTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory(prefix="trufflepig-tips-")
        self.addCleanup(scratch.cleanup)
        patch = mock.patch.dict(os.environ, {"XDG_STATE_HOME": scratch.name})
        patch.start()
        self.addCleanup(patch.stop)
        self.markers = Path(scratch.name) / "trufflepig/steer-nudge"

    def test_first_tip_is_claimed_once_per_agent_and_class(self):
        self.assertTrue(policy.first_tip("s:a:/repo", "definition"))
        self.assertFalse(policy.first_tip("s:a:/repo", "definition"))
        self.assertTrue(policy.first_tip("s:a:/repo", "read"))
        self.assertTrue(policy.first_tip("s:b:/repo", "definition"))

    def test_markers_older_than_a_day_are_pruned(self):
        self.markers.mkdir(parents=True)
        stale, fresh = self.markers / "old-definition", self.markers / "new-definition"
        stale.touch()
        fresh.touch()
        two_days_ago = time.time() - 2 * policy.TIP_MARKER_SECONDS
        os.utime(stale, (two_days_ago, two_days_ago))
        self.assertTrue(policy.first_tip("s:a:/repo", "definition"))
        self.assertFalse(stale.exists())
        self.assertTrue(fresh.exists())
        # Pruning runs at most once a day.
        stale.touch()
        os.utime(stale, (two_days_ago, two_days_ago))
        self.assertTrue(policy.first_tip("s:a:/repo", "read"))
        self.assertTrue(stale.exists())


if __name__ == "__main__":
    unittest.main()
