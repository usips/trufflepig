"""Extract audit metadata from compact CLI responses without changing delivery."""
import re

LANE_KEYS = {"semantic": "semantic_status", "rerank": "rerank_status"}
SINGLE_REPO_KEYS = {"indexed", "excluded", "parse_failures", "walk_failures", "truncated", "endpoint"}
# `next: SET@N`, `next: more SET@N (-n N raises the page size)`, `next: show read:H@B`.
NEXT_FOOTER = re.compile(r"^next: (?:(?:more|show) )?(\S+)")


# `NAME[@WORKTREE] STATE[ (detail)][ → served from M index (…)][ truncated]`, where the
# fallback detail is free-form (`1 file differs`, `N files differ`, `no files differ`,
# `differences unknown`); any other coverage part (`file:X matched 0 …`, `lang:`,
# `filters`, `refs T sites …`) annotates.
MEMBER_PART = re.compile(
    r"^(?P<name>[A-Za-z0-9_.+-]+(?:@[^\s;]+)?) "
    r"(?P<state>complete|partial|warming|unavailable|timed[ _]out|unknown|parent_fallback)(?P<rest>(?: .*)?)$")
ANNOTATION_PREFIXES = ("file:", "-file:", "lang:", "filters", "refs ", "scope ")


def member_part(part: str) -> dict | None:
    """Audit fields of one member's coverage part, or None for an annotation."""
    match = MEMBER_PART.match(part)
    if not match or part.startswith(ANNOTATION_PREFIXES):
        return None
    state, rest = match.group("state"), match.group("rest")
    if state in ("complete", "partial"):
        state = "searched"
    elif state == "warming" and "served from" in rest:
        state = "parent_fallback"
    return {"name": match.group("name"), "state": state.replace(" ", "_"),
            "partial": match.group("state") == "partial", "truncated": rest.endswith(" truncated")}


def coverage_parts(summary: str) -> list[str]:
    """`; `-separated parts of a coverage line; separators inside parentheses, as in
    `scope home (warming; ws:all searches 3 members)`, stay within their part."""
    parts, depth, start = [], 0, 0
    for index, char in enumerate(summary):
        depth += (char == "(") - (char == ")")
        if depth == 0 and summary.startswith("; ", index):
            parts.append(summary[start:index])
            start = index + 2
    return [*parts, summary[start:]]


def next_cursor(line: str) -> str | None:
    """The bare cursor of a `next:` footer line, without its runnable verb or hint."""
    match = NEXT_FOOTER.match(line)
    return match.group(1) if match else None


def parse_lines_response(stdout: bytes) -> dict:
    """Rebuild the response fields the audit needs from a `--format lines` page."""
    text = stdout.decode("utf-8", "replace")
    response: dict = {"hits": []}
    lanes: dict = {}
    members: dict = {}
    single: dict = {}
    seen_coverage = False
    for line in text.splitlines():
        if line.startswith("next: "):
            response["next"] = next_cursor(line)
        elif line == "truncated: true":
            response["truncated"] = True
        if not seen_coverage:
            if line.startswith("coverage: "):
                seen_coverage = True
                for part in coverage_parts(line[len("coverage: "):]):
                    words = part.split()
                    member = member_part(part)
                    if not words:
                        continue
                    if words[0] == "scope":
                        response["scope"] = part[len("scope "):]
                    elif words[0] in LANE_KEYS and len(words) == 2:
                        lanes[LANE_KEYS[words[0]]] = words[1]
                    elif words[0] in SINGLE_REPO_KEYS:
                        single[words[0]] = words[1] if len(words) > 1 else True
                    elif member is not None:
                        members[member.pop("name")] = member
                    else:
                        response.setdefault("annotations", []).append(part)
            elif re.match(r"^[0-9a-f]{32}:\d+\t", line):
                response["hits"].append({"handle": line.split("\t", 1)[0]})
            continue
    if members:
        response["coverage"] = [
            {"member": name, "state": fields["state"], "partial": fields["partial"],
             "truncated": fields["truncated"], "issues": dict(lanes)}
            for name, fields in members.items()
        ]
    elif seen_coverage:
        response["coverage"] = {"parse_failures": single.get("parse_failures"), **lanes}
    return response


def coverage_summary(response: dict) -> dict:
    coverage = response.get("coverage")
    summary: dict = {}
    if isinstance(coverage, list):
        for member in coverage:
            if not isinstance(member, dict):
                continue
            issues = member.get("issues") or {}
            summary[str(member.get("member"))] = {
                "state": member.get("state"),
                "partial": member.get("partial"),
                "truncated": member.get("truncated"),
                "semantic_status": issues.get("semantic_status"),
                "rerank_status": issues.get("rerank_status"),
                "rerank_reason": issues.get("rerank_reason"),
            }
    elif isinstance(coverage, dict):
        summary["root"] = {
            "semantic_status": coverage.get("semantic_status"),
            "rerank_status": coverage.get("rerank_status"),
            "rerank_reason": coverage.get("rerank_reason"),
            "parse_failures": coverage.get("parse_failures"),
        }
    return summary
