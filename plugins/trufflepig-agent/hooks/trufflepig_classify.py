"""Classify shell searches and translate each into its trufflepig-agent equivalent.

`classify(command, cwd, checkout)` is a pure function of the command text and the
file system under `checkout`; it returns nothing for searches the hook must not steer.
"""
from __future__ import annotations

import os
import re
import shlex
from dataclasses import dataclass, field
from pathlib import Path

from trufflepig_shell import GrepCall, parse_grep, segments, strip_prefix

# Cheap prefilter: skip parsing commands that cannot contain a search program.
MAYBE_SEARCH = re.compile(r"\b(rg|grep|egrep|fgrep|ugrep|ag|ack|find|fd|bfs|git\s+grep)\b")
PROGRAMS = {"grep", "egrep", "fgrep", "ugrep", "rg", "ag", "ack", "find", "bfs", "fd", "git"}
DEFINITION_KEYWORDS = ("fn", "struct", "enum", "trait", "impl", "type", "mod", "class", "def",
                       "const", "static", "interface", "function", "proc", "union", "macro_rules!")
KEYWORD_ALTERNATIVES = "|".join(re.escape(k) for k in DEFINITION_KEYWORDS)
# Applied to `normalized()` patterns: a definition keyword followed by a concrete name.
DEFINITION = re.compile(rf"(?:^|[^A-Za-z0-9_!])(?:{KEYWORD_ALTERNATIVES})\s+([A-Za-z_][A-Za-z0-9_]*)")
IDENTIFIER = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*$")
NON_CODE_PATH = re.compile(r"(^|/)(target|node_modules|\.git|logs?)(/|$)|\.(log|jsonl|out|txt|csv)$|^/(tmp|proc|sys|dev)(/|$)")
EXTENSION_LANGUAGES = {"rs": "rust", "ts": "ts", "tsx": "ts", "mts": "ts", "js": "js", "jsx": "js",
                       "mjs": "js", "cjs": "js", "luau": "luau", "lua": "luau", "dm": "dm", "dme": "dm",
                       "md": "text", "toml": "text", "txt": "text", "json": "text", "yaml": "text",
                       "yml": "text"}
TYPE_LANGUAGES = {"rust": "rust", "ts": "ts", "typescript": "ts", "js": "js", "javascript": "js",
                  "lua": "luau", "luau": "luau", "md": "text", "markdown": "text", "toml": "text",
                  "json": "text", "yaml": "text"}
STRUCTURED = {"rs", "rust", "ts", "tsx", "mts", "cts", "typescript", "js", "jsx", "mjs", "cjs", "javascript",
              "luau", "lua", "dm", "dme", "dmf"}
MODIFIERS = {"pub", "crate", "super", "self", "async", "export", "local", "unsafe", "extern", "default",
             "test", "derive", "abstract", "public", "private", "protected", "final"}
SHELL_VARIABLE = re.compile(r"\$[A-Za-z_{(]")


@dataclass
class Search:
    """One shell search the hook may steer."""
    kind: str
    program: str
    pattern: str
    hint: str
    paths: list[str] = field(default_factory=list)


def language_filter(includes: list[str], types: list[str], paths: list[str]) -> str:
    for glob in [*includes, *(p for p in paths if "*" in p)]:
        match = re.search(r"\.\{?([A-Za-z0-9]+)", glob)
        if match and match.group(1).lower() in EXTENSION_LANGUAGES:
            return f"lang:{EXTENSION_LANGUAGES[match.group(1).lower()]}"
    for kind in types:
        if kind.lower() in TYPE_LANGUAGES:
            return f"lang:{TYPE_LANGUAGES[kind.lower()]}"
    return ""


def relative_prefix(path: str, cwd: Path, checkout: Path) -> str | None:
    """`file:` prefix for `path` relative to the indexed checkout, "" for the whole
    checkout, or None when the path lies outside it."""
    target = Path(os.path.expanduser(path))
    target = (target if target.is_absolute() else cwd / target)
    target = Path(os.path.normpath(target))
    if target == checkout:
        return ""
    if checkout not in target.parents:
        return None
    relative = target.relative_to(checkout).as_posix()
    # A missing path without an extension is most likely a directory named from
    # another checkout or a later cwd; a file-looking name stays a file.
    directory = target.is_dir() or not target.exists() and "." not in target.name
    return relative + ("/" if directory else "")


def bre_to_regex(pattern: str, call: GrepCall) -> str:
    """Translate basic grep syntax to the Rust regex syntax `re:` accepts."""
    extended = call.program in ("egrep", "rg", "ag", "ack") or {"E", "P"} & call.flags or \
        "--extended-regexp" in call.flags or "--perl-regexp" in call.flags
    if not extended and call.program != "fgrep" and "F" not in call.flags:
        pattern = pattern.replace(r"\|", "|").replace(r"\(", "(").replace(r"\)", ")") \
            .replace(r"\+", "+").replace(r"\?", "?").replace(r"\{", "{").replace(r"\}", "}")
    pattern = pattern.replace("[[:space:]]", r"\s").replace("[[:alnum:]_]", r"\w")
    if "i" in call.flags or "--ignore-case" in call.flags:
        pattern = "(?i)" + pattern
    return pattern


def quote(text: str) -> str:
    return shlex.quote(text)


def normalized(pattern: str) -> str:
    """Pattern with regex syntax reduced to spaces; `\\w+`-style wildcards become `@` so
    a keyword followed by a wildcard is not mistaken for a named definition."""
    text = re.sub(r"\\w[+*?]?|\[[^\]]*\][+*?]?|\.[+*?]", " @ ", pattern)
    text = re.sub(r"\\[sbB<>][+*?]?|\\\(|\\\)|[()^$?+*{}]", " ", text)
    return text.replace("\\|", "|").replace("\\", "")


def definition_names(pattern: str) -> list[str]:
    names = []
    for name in DEFINITION.findall(normalized(pattern)):
        if name not in DEFINITION_KEYWORDS and name not in ("pub", "async", "unsafe", "crate", "self", "super") \
                and name not in names:
            names.append(name)
    return names


def split_alternatives(pattern: str) -> list[str]:
    parts = re.split(r"\\\||\|", pattern)
    return [re.sub(r"^\\b|\\b$|^\\<|\\>$", "", part.strip()) for part in parts]


def is_structural(alternative: str) -> bool:
    """An outline-style alternative: definition keywords and modifiers, no concrete name."""
    words = re.findall(r"[A-Za-z_][A-Za-z0-9_!]*", normalized(alternative))
    if not words:
        return "#" in alternative
    return all(w in DEFINITION_KEYWORDS or w in MODIFIERS for w in words) and \
        any(w in DEFINITION_KEYWORDS or w in ("pub", "export") for w in words)


def extensions(paths: list[str], includes: list[str], types: list[str]) -> set[str]:
    """File extensions a search is known to target, from paths, globs, and types."""
    found = {m.group(1).lower() for text in [*paths, *includes]
             for m in [re.search(r"\.\{?([A-Za-z0-9]+)\}?$", text)] if m}
    found |= {kind.lower() for kind in types}
    return found


def classify_grep(call: GrepCall, cwd: Path, checkout: Path, piped: bool) -> Search | None:
    if call.pattern is None or piped and not call.paths:
        return None
    if any(SHELL_VARIABLE.search(text) for text in (call.pattern, *call.paths)):
        return None  # shell variables are not expanded here
    if call.revisions or {"q", "c", "--quiet", "--count", "L", "--files-without-match"} & call.flags:
        return None
    literal = call.program == "fgrep" or bool({"F", "--fixed-strings"} & call.flags)
    recursive = call.program in ("rg", "ag", "ack", "git") or bool({"r", "R", "--recursive"} & call.flags)
    if not call.paths and not recursive:
        return None  # reads stdin
    paths = call.paths or ["."]
    prefixes = []
    for path in paths:
        if any(ch in path for ch in "*?[") and not os.path.exists(path):
            prefix = relative_prefix(path.split("*")[0].rsplit("/", 1)[0] or ".", cwd, checkout)
        else:
            prefix = relative_prefix(path, cwd, checkout)
        if prefix is None or NON_CODE_PATH.search(path) or NON_CODE_PATH.search(prefix or ""):
            return None
        prefixes.append(prefix)
    single_files = [p for p in prefixes if p and not p.endswith("/")]
    scope = " ".join(f"file:{p}" for p in dict.fromkeys(prefixes) if p and len(prefixes) == 1)
    language = language_filter(call.includes, call.types, paths)
    filters = " ".join(part for part in (scope, language) if part)
    pattern = call.pattern
    alternatives = split_alternatives(pattern)
    known = extensions(single_files, call.includes, call.types)
    # Symbols and outlines exist only for structurally parsed languages; other files
    # are lexical text, where an equivalent regex search still works.
    structured = not known or bool(known & STRUCTURED)
    regex_query = "re:" + bre_to_regex(re.escape(pattern) if literal else pattern, call) + \
        (" " + filters if filters else "")
    names = [] if literal else definition_names(pattern)
    if names and all(definition_names(a) for a in alternatives):
        if structured:
            query = f"sym:{names[0]}" + (f" {filters}" if filters else "")
            hint = f"trufflepig-agent search {quote(query)}"
            if len(names) > 1:
                hint += f" (one call per name: {', '.join('sym:' + n for n in names[1:3])})"
        else:
            hint = f"trufflepig-agent search {quote(regex_query)}"
        if call.after >= 5:
            return Search("body", call.program, pattern,
                          f"{hint}, then `trufflepig-agent show HANDLE` for the whole definition body", paths)
        return Search("definition", call.program, pattern, hint, paths)
    only_files = bool(single_files) and len(single_files) == len(prefixes)
    if not literal and only_files and structured and all(is_structural(a) for a in alternatives):
        return Search("outline", call.program, pattern,
                      " and ".join(f"trufflepig-agent map {quote(p)}" for p in single_files[:2]), paths)
    if only_files:
        return None  # a targeted read of known files gains little from an index
    if alternatives and all(IDENTIFIER.match(a) for a in alternatives) and \
            all("_" in a or "::" in a or re.search(r"[a-z][A-Z]|^[A-Z][a-z]+[A-Z]", a) or a.isupper() and len(a) > 3
                for a in alternatives):
        if len(alternatives) == 1:
            hint = f"trufflepig-agent refs {quote(alternatives[0])}"
            if filters:
                hint += f"  (or search {quote('re:' + alternatives[0] + ' ' + filters)})"
        else:
            hint = f"trufflepig-agent search {quote('re:' + '|'.join(alternatives) + (' ' + filters if filters else ''))}"
        return Search("references", call.program, pattern, hint, paths)
    words = pattern.split()
    if ("i" in call.flags or "--ignore-case" in call.flags) and len(words) <= 1 and \
            re.fullmatch(r"[a-z]+(\\\|[a-z]+)*", pattern):
        concept = " ".join(split_alternatives(pattern))
        return Search("concept", call.program, pattern,
                      f"trufflepig-agent search {quote(concept + (' ' + filters if filters else ''))}", paths)
    regex = bre_to_regex(re.escape(pattern) if literal else pattern, call)
    return Search("regex", call.program, pattern,
                  f"trufflepig-agent search {quote('re:' + regex + (' ' + filters if filters else ''))}", paths)


FIND_FILESYSTEM_ACTIONS = {"-exec", "-execdir", "-delete", "-ok", "-okdir", "-mtime", "-mmin", "-newer",
                           "-atime", "-amin", "-ctime", "-cmin", "-size", "-empty", "-perm", "-user",
                           "-group", "-fprint", "-printf", "-ls", "-inum", "-links", "-samefile"}


def classify_find(program: str, args: list[str], cwd: Path, checkout: Path) -> Search | None:
    if FIND_FILESYSTEM_ACTIONS & set(args):
        return None
    if "-type" in args and args[args.index("-type") + 1:args.index("-type") + 2] == ["d"]:
        return None
    if any(SHELL_VARIABLE.search(arg) for arg in args):
        return None
    names = []
    for index, arg in enumerate(args[:-1]):
        excluded = index > 0 and args[index - 1] in ("-not", "!") or args[index + 2:index + 3] == ["-prune"]
        if arg in ("-name", "-iname", "-path", "-ipath") and not excluded:
            names.append(args[index + 1])
    names.sort(key=lambda name: "/" in name)  # prefer base-name patterns over path patterns
    if program == "fd":
        positional = [a for a in args if not a.startswith("-")]
        names, roots = positional[:1], positional[1:] or ["."]
    else:
        roots = []
        for arg in args:
            if arg.startswith("-") or arg in ("(", ")", "!"):
                break
            roots.append(arg)
        roots = roots or ["."]
    if not names:
        return None
    return files_search(program, names, roots, cwd, checkout)


def files_search(program: str, names: list[str], roots: list[str], cwd: Path, checkout: Path) -> Search | None:
    """Translate a file-name search. `file:` is a path prefix, so a base-name pattern
    becomes a filename-weighted plain search restricted to files."""
    prefixes = []
    for root in roots:
        prefix = relative_prefix(root, cwd, checkout)
        if prefix is None or NON_CODE_PATH.search(root):
            return None
        prefixes.append(prefix)
    language = language_filter([n for n in names if "*" in n], [], [])
    base = f"file:{prefixes[0]}" if len(prefixes) == 1 and prefixes[0] else ""
    name = names[0] if names else ""
    stem = re.sub(r"[*?\[\]]", " ", name.rsplit("/", 1)[-1])
    stem = re.sub(r"\.[A-Za-z0-9]+\s*$", "", stem).strip() if "." in stem else stem.strip()
    query = " ".join(part for part in (stem, base, language, "kind:file") if part)
    return Search("files", program, name, f"trufflepig-agent search {quote(query)}", roots)


def classify(command: str, cwd: Path, checkout: Path) -> list[Search]:
    """Steerable searches inside `command`, run from `cwd` within indexed `checkout`."""
    if not MAYBE_SEARCH.search(command) or "trufflepig" in command:
        return []
    parsed = segments(command)
    if parsed is None:
        return []
    found: list[Search] = []
    directory = cwd
    for words, piped in parsed:
        words = strip_prefix(words)
        if not words:
            continue
        program = os.path.basename(words[0])
        if program == "cd" and len(words) > 1:
            directory = Path(os.path.normpath(directory / os.path.expanduser(words[1])))
            continue
        if program not in PROGRAMS or program == "xargs":
            continue
        if piped and program in ("find", "bfs", "fd"):
            continue
        search = None
        if program == "git":
            if len(words) < 2 or words[1] != "grep":
                continue
            call = parse_grep("git", words[2:])
            search = classify_grep(call, directory, checkout, piped)
        elif program in ("find", "bfs", "fd"):
            search = classify_find(program, words[1:], directory, checkout)
        elif program == "rg" and "--files" in words:
            call = parse_grep("rg", [w for w in words[1:] if w != "--files"])
            roots = ([call.pattern] if call.pattern else []) + call.paths
            search = None if piped else files_search("rg", call.includes, roots or ["."], directory, checkout)
        else:
            call = parse_grep(program, words[1:])
            search = classify_grep(call, directory, checkout, piped)
        if search is not None:
            found.append(search)
    return found
