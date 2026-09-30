"""Shell command parsing for search steering: simple commands, pipes, grep options,
and line-range reads (`sed -n A,Bp`, `cat`).

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


def parse_grep(program: str, args: list[str], directory: Path) -> GrepCall:
    """Options of one grep-like call; `git grep` trees are checked under `directory`."""
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
        revisions = any(not (directory / tree).exists() for tree in trees) if double_dash_at is None \
            else bool(trees)
    return GrepCall(program, pattern, positional, flags, after, includes, types, revisions)


def changed_directory(directory: Path, target: str) -> Path:
    return Path(os.path.normpath(directory / os.path.expanduser(target)))


def git_subcommand(args: list[str], directory: Path) -> tuple[list[str], Path] | None:
    """`git` global options before the subcommand: (subcommand words, directory after
    `-C DIR`), or None when `--git-dir`/`--work-tree` make the tree unknowable."""
    index = 0
    while index < len(args):
        arg = args[index]
        if arg == "-C" and index + 1 < len(args):
            directory = changed_directory(directory, args[index + 1])
            index += 2
        elif arg == "-c" and index + 1 < len(args):
            index += 2
        elif arg.startswith(("--git-dir", "--work-tree")):
            return None
        elif arg.startswith("-"):
            index += 1  # --no-pager, -P, --paginate, --literal-pathspecs, ...
        else:
            return args[index:], directory
    return None


def search_directory(command: str, cwd: Path) -> Path:
    """Directory of the command's first program other than `cd`, after preceding
    `cd DIR` and its own `git -C DIR`; decides which indexed checkout owns the command."""
    directory = cwd
    for words, _ in segments(command) or []:
        words = strip_prefix(words)
        if not words:
            continue
        program = os.path.basename(words[0])
        if program == "cd" and len(words) > 1:
            directory = changed_directory(directory, words[1])
            continue
        if program == "git":
            found = git_subcommand(words[1:], directory)
            return found[1] if found else directory
        return directory
    return directory


SED_LINES = re.compile(r"^(\d+)(?:,(\d+))?p$")


def sed_line_range(args: list[str]) -> tuple[int, int, list[str]] | None:
    """(first, last, files) of `sed -n 'A,Bp[;C,Dp]' FILE...` (the first range); None for
    any other sed program, including in-place edits."""
    quiet = False
    scripts: list[str] = []
    operands: list[str] = []
    index = 0
    while index < len(args):
        arg = args[index]
        if arg in ("--quiet", "--silent"):
            quiet = True
        elif arg.startswith("--") or arg.startswith("-") and len(arg) > 1 and "i" in arg:
            return None  # --in-place, --expression=..., -i, and other programs
        elif arg.startswith("-") and len(arg) > 1:
            quiet |= "n" in arg
            if arg.endswith("e") and index + 1 < len(args):
                index += 1
                scripts.append(args[index])
            elif not set(arg[1:]) <= set("nEr"):
                return None
        else:
            operands.append(arg)
        index += 1
    if not scripts and operands:
        scripts.append(operands.pop(0))
    ranges = [SED_LINES.match(part.strip()) for script in scripts for part in script.split(";") if part.strip()]
    if not quiet or not ranges or not all(ranges):
        return None
    first = int(ranges[0].group(1))
    return first, int(ranges[0].group(2) or first), operands


def cat_files(args: list[str]) -> list[str] | None:
    """Files `cat [-n|-b|-A|...] FILE...` prints; None when it reads stdin."""
    if any(arg.startswith("-") and not re.fullmatch(r"-[nbAesTtuv]+", arg) for arg in args):
        return None  # `-` (stdin) or long options
    files = [arg for arg in args if not arg.startswith("-")]
    return files or None
