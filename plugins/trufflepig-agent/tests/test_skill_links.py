"""Skill guides link to repository URLs and stay within the guide length limit."""

from pathlib import Path
import re
import unittest

SKILLS = Path(__file__).resolve().parents[1] / "skills"
MAX_GUIDE_LINES = 200
CHECKOUT_PHRASE = "in the Trufflepig checkout"
# Installed skills (Muse keeps copies) cannot follow checkout-relative links, so a
# Markdown link target naming docs/ must be a repository URL on the default branch.
LINK_TARGET = re.compile(r"\]\(([^)\s]+)\)")
REPOSITORY_DOCS = "https://github.com/usips/trufflepig/blob/master/docs/"


def checkout_docs_links(text):
    return [target for target in LINK_TARGET.findall(text)
            if "docs/" in target and not target.startswith(REPOSITORY_DOCS)]


def skill_markdown():
    return sorted(SKILLS.rglob("*.md"))


class SkillLinkTests(unittest.TestCase):
    def test_skill_docs_avoid_checkout_relative_references(self):
        for path in skill_markdown():
            text = path.read_text(encoding="utf-8")
            with self.subTest(path=path.relative_to(SKILLS).as_posix()):
                self.assertNotIn(CHECKOUT_PHRASE, text)
                self.assertEqual(checkout_docs_links(text), [])

    def test_link_check_ignores_command_examples(self):
        text = ("Run `board worker add x --files docs/design.md`; see [CLI](../../docs/cli.md), "
                f"[branch](https://github.com/usips/trufflepig/blob/feat/x/docs/a.md), [ok]({REPOSITORY_DOCS}cli.md).")
        self.assertEqual(checkout_docs_links(text), [
            "../../docs/cli.md", "https://github.com/usips/trufflepig/blob/feat/x/docs/a.md"])

    def test_skill_guides_stay_within_length_limit(self):
        for path in skill_markdown():
            lines = len(path.read_text(encoding="utf-8").splitlines())
            with self.subTest(path=path.relative_to(SKILLS).as_posix()):
                self.assertLessEqual(lines, MAX_GUIDE_LINES)


if __name__ == "__main__":
    unittest.main()
