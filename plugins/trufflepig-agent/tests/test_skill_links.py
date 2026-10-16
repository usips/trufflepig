"""Skill guides link to repository URLs and stay within the guide length limit."""

from pathlib import Path
import re
import unittest

SKILLS = Path(__file__).resolve().parents[1] / "skills"
MAX_GUIDE_LINES = 200
CHECKOUT_PHRASE = "in the Trufflepig checkout"
# Every docs/ reference must be a repository URL: `blob/master/docs/...`.
CHECKOUT_DOCS_PATH = re.compile(r"(?<!blob/master/)docs/")


def skill_markdown():
    return sorted(SKILLS.rglob("*.md"))


class SkillLinkTests(unittest.TestCase):
    def test_skill_docs_avoid_checkout_relative_references(self):
        for path in skill_markdown():
            text = path.read_text(encoding="utf-8")
            with self.subTest(path=path.relative_to(SKILLS).as_posix()):
                self.assertNotIn(CHECKOUT_PHRASE, text)
                self.assertIsNone(CHECKOUT_DOCS_PATH.search(text))

    def test_skill_guides_stay_within_length_limit(self):
        for path in skill_markdown():
            lines = len(path.read_text(encoding="utf-8").splitlines())
            with self.subTest(path=path.relative_to(SKILLS).as_posix()):
                self.assertLessEqual(lines, MAX_GUIDE_LINES)


if __name__ == "__main__":
    unittest.main()
