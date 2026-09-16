---
name: trufflepig-code-search
description: Use trufflepig-agent instead of Grep, Glob, rg, or multi-file reads to locate code - definitions, callers, feature traces, symbol or regex lookups, outlines, recent changes. Ranked hits with handles for show and ctx.
whenToUse: Any task that would otherwise start with Grep, Glob, rg, find, or reading several files to locate code in an indexed repository. Not for editing files or running builds.
---

# Trufflepig code search

Always invoke the tool as `trufflepig-agent` (never bare `trufflepig`). The
wrapper adds the audit flags, resolves the harness session, and appends one
JSONL record per call so tool struggles are reviewable later. Responses are
plain tab-separated lines meant to be read directly. Do not pipe them into
python, jq, awk, or head; pass `--json` only when you need a field the lines
omit.

## Workflow

1. `trufflepig-agent search 'natural language or identifiers'`
2. Pick a hit and read it: `trufflepig-agent show HANDLE`. The handle is the
   first column of a hit line (`SETID:3`); copy it verbatim, never construct it.
3. Need the neighbourhood? `trufflepig-agent ctx HANDLE` lists definitions,
   references, imports and containers around that hit (JSON).
4. Need more of the same result set? `trufflepig-agent more CURSOR` where
   `CURSOR` is the value on the `next:` line of the previous page.

Read whole files only after search has told you which file matters.

## Query syntax

- Plain words: fused exact-identifier, lexical, filename and semantic ranking.
  Prefer 2 to 5 specific terms (`airlock pump pressure`), not sentences.
- `sym:Name` exact, case-sensitive symbol definitions.
- `re:pattern` live regex over current bytes (Rust regex syntax).
- Filters, combinable: `file:src/path/` prefix, `lang:rust|ts|js|luau|dm|text`,
  `kind:function|struct|file|...`.
- Workspace scope: `in:MEMBER` for one member, `ws:home` for the current
  checkout, default is every member.
- `refs NAME` symbol occurrences with resolved targets and candidates.
- `map PATH` module and container outline for a path prefix.
- History: `hist path:FILE`, `since`, `diff --target HANDLE`, `blame` (JSON).

## Reading responses

A search, refs, map, or more page is one line per hit, then a footer:

```text
HANDLE<TAB>member/path/to/file.rs:START-END<TAB>NAME
coverage: lunatic partial; tgstation complete truncated; rerank unavailable
next: SETID@20
truncated: true
```

- `member/` appears only in a workspace. `NAME` appears only for symbol hits
  (`sym:`, `refs`, `map`); plain searches return file regions without a name.
- `coverage:` names every member with `complete` or `partial` and adds
  `truncated` when that member's candidates were cut. `semantic` or `rerank`
  `unavailable` is informational: lexical results are still complete.
- `next:` is present only when more hits exist. `truncated: true` means the
  result set itself was capped (per-member candidate ceilings), not the page.
- `show` prints a `path (member) revision START-END` header, then
  `LINE<TAB>text` rows, then `next:`/`truncated:` when cut, `verified:`, and
  `encoding: byte-escaped` once if any row holds non-UTF-8 bytes.
- Errors are always one JSON object with an `error` field and exit code 2.
- `stale_result` or `stale_source` errors: the file changed; run the search
  again and use the new handle. Never retry the same handle.
- An empty complete search is a real answer. Vary terms once (synonyms,
  a filename, `sym:`), then fall back to `re:` or ordinary file tools.

## Budget and paging

- Every response fits a token budget (1200 `o200k_base` tokens in configured
  workspaces, 600 elsewhere). `-n` caps hits per page, but the budget wins:
  fewer lines than you asked for plus a `next:` line means the page was cut,
  not that the hits do not exist.
- To see the rest, run `more CURSOR`, or repeat the search with `-b 3000`.
  `-b` applies to search as well as `show`; use it before widening a query.
- Exit code 2 with an `insufficient_budget` error: retry with a larger `-b`.

## Do not

- Do not start with Grep, Glob, `rg`, or `find` in an indexed repository: a
  hook blocks them until you have made one `trufflepig-agent` call. Use
  `re:pattern` for regex needs.
- Do not run the identical query more than twice in a session.
- Do not paste large `show` output into your reply; cite `file:line` instead.
- Do not add `--no-daemon`, `--cache`, or `--root` unless the user asks.
