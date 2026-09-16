---
name: trufflepig-code-search
description: Use trufflepig-agent instead of Grep, Glob, rg, or multi-file reads to locate code - definitions, callers, feature traces, symbol or regex lookups, outlines, recent changes. Ranked hits with handles for show and ctx.
whenToUse: Any task that would otherwise start with Grep, Glob, rg, find, or reading several files to locate code in an indexed repository. Not for editing files or running builds.
---

# Trufflepig code search

Always invoke the tool as `trufflepig-agent` (never bare `trufflepig`). The
wrapper adds the audit flags, resolves the harness session, and appends one
JSONL record per call so tool struggles are reviewable later. Every response
is one JSON object on stdout.

## Workflow

1. `trufflepig-agent search 'natural language or identifiers'`
2. Pick a hit and read it: `trufflepig-agent show HANDLE` (handles look like
   `SETID:3` or `member/SETID:3`; copy them verbatim, never construct them).
3. Need the neighbourhood? `trufflepig-agent ctx HANDLE` lists definitions,
   references, imports and containers around that hit.
4. Need more of the same result set? `trufflepig-agent more CURSOR` where
   `CURSOR` is the `next` field of the previous page.

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
- History: `hist path:FILE`, `since`, `diff --target HANDLE`, `blame`.

## Budget and paging

- The response budget defaults to 1200 `o200k_base` tokens in configured
  workspaces (600 elsewhere). Pass `-b 3000` when a `show` is cut off or a
  hit will not fit; pass `-n 5` to shorten pages you only skim.
- A `truncated: true` page means more hits exist; use `more` before widening
  the query.

## Reading responses

- `hits[]` carry `file` (URI), `start_line`, `end_line`, `name`, `handle`.
- `coverage` reports per-member `partial` and `issues` such as
  `semantic_status`, `rerank_status`, `parse_failures`. `unavailable` there is
  informational: lexical results are still complete.
- `status: stale_result` or `stale_source`: the file changed; run the search
  again and use the new handle. Never retry the same handle.
- Exit code 2 with an `insufficient_budget` error: retry with a larger `-b`.
- An empty complete search is a real answer. Vary terms once (synonyms,
  a filename, `sym:`), then fall back to `re:` or ordinary file tools.

## Do not

- Do not start with Grep, Glob, `rg`, or `find` in an indexed repository: a
  hook blocks them until you have made one `trufflepig-agent` call. Use
  `re:pattern` for regex needs.
- Do not run the identical query more than twice in a session.
- Do not paste large `show` output into your reply; cite `file:line` instead.
- Do not add `--no-daemon`, `--cache`, or `--root` unless the user asks.
