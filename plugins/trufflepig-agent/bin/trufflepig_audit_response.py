"""Extract audit metadata from compact CLI responses without changing delivery."""
import re

LANE_KEYS = {"semantic": "semantic_status", "rerank": "rerank_status"}
SINGLE_REPO_KEYS = {"indexed", "excluded", "parse_failures", "walk_failures", "truncated", "endpoint"}


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
            response["next"] = line[len("next: "):]
        elif line == "truncated: true":
            response["truncated"] = True
        if not seen_coverage:
            if line.startswith("coverage: "):
                seen_coverage = True
                for part in line[len("coverage: "):].split("; "):
                    words = part.split()
                    if not words:
                        continue
                    if words[0] == "scope":
                        response["scope"] = part[len("scope "):]
                    elif words[0] in LANE_KEYS and len(words) == 2:
                        lanes[LANE_KEYS[words[0]]] = words[1]
                    elif words[0] in SINGLE_REPO_KEYS:
                        single[words[0]] = words[1] if len(words) > 1 else True
                    else:
                        members[words[0]] = {
                            "state": "searched" if words[1:2] in (["complete"], ["partial"]) else (words[1] if len(words) > 1 else None),
                            "partial": words[1:2] == ["partial"],
                            "truncated": "truncated" in words[2:],
                        }
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
