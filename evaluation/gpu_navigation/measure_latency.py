"""Measure prepared workspace search latency with serial and concurrent clients."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import math
from pathlib import Path
import subprocess
import threading
import time

from .replay import exact_counter


def search(helper, query, counter, barrier=None):
    if barrier is not None:
        barrier.wait(timeout=10)
    started = time.perf_counter()
    try:
        result = subprocess.run([str(helper), "search", query], capture_output=True,
                                text=True, timeout=30)
        elapsed = time.perf_counter() - started
        value = json.loads(result.stdout)
        coverage = value.get("coverage", [])
        ready = bool(coverage) and all(
            member.get("issues", {}).get("semantic_status") == "ready"
            for member in coverage)
        return {"query": query, "elapsed_seconds": elapsed,
                "exit_code": result.returncode, "semantic_ready": ready,
                "emitted_tokens": counter(result.stdout),
                "stdout": result.stdout, "stderr": result.stderr}
    except (subprocess.TimeoutExpired, ValueError, OSError) as error:
        return {"query": query, "elapsed_seconds": time.perf_counter() - started,
                "semantic_ready": False, "error": str(error)}


def summarize(records, threshold):
    times = sorted(record["elapsed_seconds"] for record in records)
    p95 = times[math.ceil(len(times) * 0.95) - 1]
    errors = sum(record.get("exit_code") != 0 for record in records)
    pending = sum(not record["semantic_ready"] for record in records)
    oversized = sum(record.get("emitted_tokens", 0) > 600 for record in records)
    return {"requests": len(records), "p50_seconds": times[len(times) // 2],
            "p95_seconds": p95, "maximum_seconds": times[-1],
            "errors": errors, "nonready_responses": pending,
            "over_budget_responses": oversized, "p95_limit_seconds": threshold,
            "passed": p95 <= threshold and not (errors or pending or oversized)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--helper", type=Path, required=True)
    parser.add_argument("--queries", type=Path,
                        default=Path(__file__).with_name("solver_prompts.json"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--serial", type=int, default=100)
    parser.add_argument("--clients", type=int, default=6)
    parser.add_argument("--waves", type=int, default=20)
    args = parser.parse_args()
    if min(args.serial, args.clients, args.waves) < 1 or args.clients > 32:
        parser.error("positive counts and at most 32 clients are required")
    counter = exact_counter()
    if counter is None:
        parser.error("tiktoken is required for exact output accounting")
    queries = [task["query"] for task in json.loads(args.queries.read_text())["tasks"]]
    if not queries:
        parser.error("at least one query is required")
    warmup = [search(args.helper, query, counter) for query in queries]
    serial = [search(args.helper, queries[index % len(queries)], counter)
              for index in range(args.serial)]
    concurrent = []
    with ThreadPoolExecutor(max_workers=args.clients) as pool:
        for wave in range(args.waves):
            barrier = threading.Barrier(args.clients)
            futures = [pool.submit(search, args.helper,
                                   queries[(wave * args.clients + client) % len(queries)],
                                   counter, barrier) for client in range(args.clients)]
            concurrent.extend(future.result() for future in futures)
    summary = {"serial": summarize(serial, 2.0),
               "concurrent": summarize(concurrent, 5.0),
               "clients": args.clients, "waves": args.waves,
               "warmup_requests_excluded": len(warmup), "tokenizer": "o200k_base"}
    summary["passed"] = summary["serial"]["passed"] and summary["concurrent"]["passed"]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    raw = args.output.with_suffix(".jsonl")
    with raw.open("w") as stream:
        for phase, records in (("warmup", warmup), ("serial", serial), ("concurrent", concurrent)):
            for record in records:
                stream.write(json.dumps({"phase": phase, **record}) + "\n")
    summary["raw_records"] = str(raw)
    args.output.write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
