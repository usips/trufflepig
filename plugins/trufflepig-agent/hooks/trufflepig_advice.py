"""Advice beyond search translation: `trufflepig-agent` calls whose footer a pipe or
chain hides, and subagent briefs that tell the subagent to grep code.

Both are pure functions of the text; `steer-search.py` decides delivery and cadence.
"""
from __future__ import annotations

import os
import re

from trufflepig_shell import segments, strip_prefix

# Directory changes before the call keep its output and exit status intact.
TRANSPARENT = {"cd", "pushd"}
GREP_BRIEF = re.compile(
    r"\b(?:use|using|just|run|via|with|try)\s+`?(?:git\s+grep|grep|rg|ripgrep)\b(?:`?\s+for\b)?"
    r"|\b(?:git\s+grep|grep|rg)\s+(?:for|across|through|counts?|is\s+fine|-[A-Za-z]*[rRnlw])\b"
    r"|\bgrep(?:ping)?\s+(?:the\s+)?(?:code|codebase|repo|source|crates?)\b", re.I)
NEGATION = re.compile(r"\b(?:not|never|don'?t|no|avoid|instead\s+of|rather\s+than|than|without)\W*\w*\W*$", re.I)


def call_shape(command: str) -> str | None:
    """`piped` when a trufflepig-agent call's output feeds a pipe, `chained` when the
    call shares its shell command with other programs, else None."""
    if "trufflepig-agent" not in command:
        return None
    parsed = segments(command)
    if not parsed:
        return None
    programs = []
    for words, piped in parsed:
        words = strip_prefix(words)
        programs.append((os.path.basename(words[0]) if words else "", piped))
    calls = [index for index, (program, _) in enumerate(programs) if program == "trufflepig-agent"]
    if not calls:
        return None
    if any(index + 1 < len(programs) and programs[index + 1][1] for index in calls):
        return "piped"
    others = [program for index, (program, _) in enumerate(programs)
              if index not in calls and program not in TRANSPARENT and program]
    return "chained" if others or len(calls) > 1 else None


def call_shape_tip(shape: str, full: bool) -> str:
    if not full:
        return "trufflepig: run `trufflepig-agent` as its own Bash call; pipes and chains hide its footer."
    if shape == "piped":
        return ("trufflepig: piping `trufflepig-agent` output (`| head`, `| grep`, `2>&1 | ...`) drops its "
                "coverage and `next:` footer and hides its exit status. Run it as its own Bash call; "
                "`-n N` sets how many hits a page holds, and each `next:` line is the runnable follow-up.")
    return ("trufflepig: run `trufflepig-agent` as its own Bash call, without `;`, `&&`, or `echo` "
            "separators; its exit status and footer are the result. `cd DIR && trufflepig-agent ...` is fine.")


def grep_brief(prompt: str) -> str | None:
    """The phrase in a subagent brief that tells it to grep, if any; negated mentions
    ("never use grep", "instead of grep for") do not count."""
    for match in GREP_BRIEF.finditer(prompt):
        if not NEGATION.search(prompt[max(0, match.start() - 24):match.start()]):
            return match.group(0).strip("` ")
    return None


def grep_brief_tip(phrase: str, member: str, full: bool) -> str:
    if not full:
        return f"trufflepig: this brief says `{phrase}`; ask subagents for `trufflepig-agent` searches instead."
    return (f"trufflepig: this subagent brief says `{phrase}`. {member} is indexed by trufflepig; the subagent "
            "starts with trufflepig search guidance, but an explicit grep instruction in its brief overrides "
            "it. In future briefs ask for `trufflepig-agent` (`search 'sym:X'`, `show 'sym:X'`, `refs X`, "
            "`search 'few words'`) for code, and keep grep for logs and command output.")
