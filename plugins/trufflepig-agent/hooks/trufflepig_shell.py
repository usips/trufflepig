"""Shell command parsing for search steering: simple commands, pipes, and grep options.

Parsing is conservative: heredocs and command substitution yield no commands.
"""
from __future__ import annotations

import os
import re
import shlex
from dataclasses import dataclass
from pathlib import Path

SEPARATORS = {";", "&&", "||", "|", "&", "|&", "\n", "(", ")", "{", "}"}
WRAPPERS = {"command", "builtin", "exec", "nice", "time", "timeout", "env", "noglob"}
# Short options that consume the next argument (or the rest of the cluster).
VALUE_SHORT = {
    "grep": set("efABCmdD"), "egrep": set("efABCmdD"), "fgrep": set("efABCmdD"),
    "ugrep": set("efABCmdDgtN"), "git": set("efABCm"),
    "rg": set("efABCmgtTMjEr"), "ag": set("ABCmG"), "ack": set("ABCm"),
}
VALUE_LONG = {"--regexp", "--file", "--after-context", "--before-context", "--context",
              "--max-count", "--include", "--exclude", "--exclude-dir", "--glob", "--iglob",
              "--type", "--type-not", "--max-depth", "--color", "--colour", "--binary-files",
              "--devices", "--directories", "--threads", "--encoding", "--sort", "--sortr"}


def tokenize(command: str) -> list[str] | None:
    """Shell words and operators, or None when the text cannot be parsed safely.
    Heredocs and command substitution are left alone rather than guessed at."""
    if "<<" in command or "$(" in command or "`" in command:
        return None
    text = command.replace("\\\n", " ").replace("\n", " ; ")
    lexer = shlex.shlex(text, posix=True, punctuation_chars=";&|(){}<>")
    lexer.whitespace_split = True
    try:
        return list(lexer)
    except ValueError:
        return None


def segments(command: str) -> list[tuple[list[str], bool]] | None:
    """Simple commands as (words, piped) where `piped` means stdin comes from a pipe.
    Redirections (`2>&1`, `> out`, `< in`) and their targets are dropped."""
    tokens = tokenize(command)
    if tokens is None:
        return None
    result: list[tuple[list[str], bool]] = []
    words: list[str] = []
    piped = False
    index = 0
    while index < len(tokens):
        token = tokens[index]
        index += 1
        if token and set(token) <= set("<>&"):
            if "<" in token or ">" in token:
                if words and words[-1].isdigit():
                    words.pop()  # file descriptor of `2>`
                index += 1  # the redirection target
                continue
        if token in SEPARATORS or token and set(token) <= set(";&|(){}"):
            if words:
                result.append((words, piped))
            words = []
            piped = token in ("|", "|&")
            continue
        words.append(token)
    if words:
        result.append((words, piped))
    return result


def strip_prefix(words: list[str]) -> list[str]:
    """Drop environment assignments and transparent wrappers before the program name."""
    index = 0
    while index < len(words):
        word = words[index]
        if re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", word):
            index += 1
        elif word in WRAPPERS:
            index += 1
            if word == "timeout" and index < len(words) and re.match(r"^\d", words[index]):
                index += 1
            while index < len(words) and words[index].startswith("-") and word in ("env", "nice", "timeout"):
                index += 1
        else:
            break
    return words[index:]


@dataclass
class GrepCall:
    program: str
    pattern: str | None
    paths: list[str]
    flags: set[str]
    after: int
    includes: list[str]
    types: list[str]
    revisions: bool = False


def parse_grep(program: str, args: list[str]) -> GrepCall:
    value_short = VALUE_SHORT.get(program, set())
    flags: set[str] = set()
    patterns: list[str] = []
    positional: list[str] = []
    includes: list[str] = []
    types: list[str] = []
    after = 0
    index = 0
    only_positional = False
    double_dash_at = None
    while index < len(args):
        arg = args[index]
        if only_positional or not arg.startswith("-") or arg == "-":
            positional.append(arg)
            index += 1
            continue
        if arg == "--":
            only_positional = True
            double_dash_at = len(positional)
            index += 1
            continue
        if arg.startswith("--"):
            name, _, value = arg.partition("=")
            if not value and name in VALUE_LONG and index + 1 < len(args):
                index += 1
                value = args[index]
            flags.add(name)
            if name in ("--include", "--glob", "--iglob"):
                includes.append(value)
            elif name == "--type":
                types.append(value)
            elif name == "--regexp":
                patterns.append(value)
            elif name in ("--after-context", "--context") and value.isdigit():
                after = max(after, int(value))
            index += 1
            continue
        cluster = arg[1:]
        position = 0
        while position < len(cluster):
            letter = cluster[position]
            if letter in value_short:
                value = cluster[position + 1:]
                if not value and index + 1 < len(args):
                    index += 1
                    value = args[index]
                if letter == "e":
                    patterns.append(value)
                elif letter in "AC" and value.isdigit():
                    after = max(after, int(value))
                elif letter in "g":
                    includes.append(value)
                elif letter == "t":
                    types.append(value)
                flags.add(letter)
                break
            if letter.isdigit() and program in ("grep", "egrep", "fgrep", "ugrep", "git"):
                digits = re.match(r"\d+", cluster[position:]).group()
                after = max(after, int(digits))
                position += len(digits)
                continue
            flags.add(letter)
            position += 1
        index += 1
    basic = program in ("grep", "ugrep", "git") and not {"E", "P", "F"} & flags
    if len(patterns) > 1 and ("F" in flags or "--fixed-strings" in flags or program == "fgrep"):
        # Several fixed strings become one extended alternation of escaped literals.
        patterns = [re.escape(p) for p in patterns]
        flags -= {"F", "--fixed-strings"}
        flags.add("E")
        program = "grep" if program == "fgrep" else program
        basic = False
    if patterns:
        pattern = ("\\|" if basic else "|").join(patterns)
    else:
        pattern = positional.pop(0) if positional else None
    if double_dash_at is not None and not patterns:
        double_dash_at -= 1
    revisions = False
    if program == "git":
        trees = positional if double_dash_at is None else positional[:max(double_dash_at, 0)]
        revisions = any(not os.path.exists(tree) for tree in trees) if double_dash_at is None else bool(trees)
    return GrepCall(program, pattern, positional, flags, after, includes, types, revisions)


def final_directory(command: str, cwd: Path) -> Path:
    """The directory a leading `cd DIR &&` moves the command into, for root detection."""
    match = re.match(r"^\s*cd\s+(\"[^\"]+\"|'[^']+'|\S+)\s*(&&|;)", command)
    if not match:
        return cwd
    target = match.group(1).strip("'\"")
    return Path(os.path.normpath(cwd / os.path.expanduser(target)))
