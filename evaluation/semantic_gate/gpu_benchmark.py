"""Run the bounded CPU/4090 semantic throughput and memory gate.

The runner is intentionally a separate process for each provider. This keeps
model initialization and CUDA allocator state out of the timing comparison;
the CUDA child is sampled with ``nvidia-smi`` every 50 ms.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

COUNT = 256
MAX_BYTES = 4096
MAX_MEMORY_BYTES = 16 * 1024**3
ARENA_BYTES = 10 * 1024**3
DEFAULT_GPU_UUID = "GPU-65ce3b3d-4e39-5c51-a52c-491c5b62175e"
DEFAULT_RUNTIME = "/usr/lib/libonnxruntime.so.1.29.0"
DEFAULT_PRELOAD = "/usr/lib/libcudnn.so.9"


def _read_json(path: Path):
    return json.loads(path.read_text())


def _entries(manifest):
    if isinstance(manifest, list):
        return manifest
    for key in ("selectedfiles", "selected_files", "files", "regions", "inputs"):
        if isinstance(manifest.get(key), list):
            return manifest[key]
    raise ValueError("selected-files manifest has no selectedfiles/files list")


def _flatten(entries):
    for item in entries:
        if not isinstance(item, dict):
            continue
        regions = item.get("regions")
        if isinstance(regions, list):
            for region in regions:
                if isinstance(region, dict):
                    yield {**item, **region}
        else:
            yield item


def freeze_inputs(manifest_path: Path, root: Path, output: Path) -> dict:
    """Materialize exactly 256 hashed source regions from a saved selection."""
    manifest = _read_json(manifest_path)
    frozen = []
    seen = set()
    for item in _flatten(_entries(manifest)):
        raw_path = item.get("path") or item.get("relative_path") or item.get("file")
        if not raw_path:
            continue
        source_path = (root / raw_path).resolve()
        try:
            source_path.relative_to(root.resolve())
        except ValueError:
            continue
        text = item.get("text")
        start = int(item.get("start", item.get("byte_start", 0)))
        end = item.get("end", item.get("byte_end"))
        if text is None:
            data = source_path.read_bytes()
            end = len(data) if end is None else int(end)
            text_bytes = data[start:end]
            try:
                text = text_bytes.decode("utf-8")
            except UnicodeDecodeError:
                continue
        else:
            text_bytes = text.encode("utf-8")
            end = start + len(text_bytes) if end is None else int(end)
        if not text or len(text_bytes) > MAX_BYTES or end - start != len(text_bytes):
            continue
        digest = hashlib.sha256(text_bytes).hexdigest()
        if digest in seen:
            continue
        seen.add(digest)
        frozen.append({
            "id": str(item.get("id", f"region-{len(frozen):04d}")),
            "path": str(raw_path),
            "start": start,
            "end": end,
            "bytes": len(text_bytes),
            "sha256": digest,
            "text": text,
        })
        if len(frozen) == COUNT:
            break
    if len(frozen) != COUNT:
        raise ValueError(f"selected-files manifest yielded {len(frozen)} valid distinct regions; expected {COUNT}")
    shape = {
        "count": len(frozen),
        "bytes_total": sum(item["bytes"] for item in frozen),
        "bytes_min": min(item["bytes"] for item in frozen),
        "bytes_max": max(item["bytes"] for item in frozen),
        "distinct_sha256": len({item["sha256"] for item in frozen}),
    }
    report = {"schema_version": 1, "source_manifest": str(manifest_path), "shape": shape, "inputs": frozen}
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n")
    return report


def _smi(args, fields, ordinal=None):
    command = ["nvidia-smi"]
    if ordinal is not None:
        command += ["-i", str(ordinal)]
    command += [f"--query-{args}={','.join(fields)}", "--format=csv,noheader,nounits"]
    result = subprocess.run(command, capture_output=True, text=True, timeout=5)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "nvidia-smi failed")
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


def gpu_inventory():
    rows = []
    for line in _smi("gpu", ["index", "uuid", "name", "memory.total", "memory.used"]):
        fields = [field.strip() for field in line.split(",")]
        if len(fields) >= 5:
            rows.append({"index": int(fields[0]), "uuid": fields[1], "name": fields[2],
                         "memory_total_mib": int(fields[3]), "memory_used_mib": int(fields[4])})
    return rows


def compute_workloads():
    rows = []
    for line in _smi("compute-apps", ["gpu_uuid", "pid", "process_name", "used_gpu_memory"]):
        fields = [field.strip() for field in line.split(",")]
        if len(fields) >= 4:
            try:
                memory = int(fields[3]) * 1024**2
                pid = int(fields[1])
            except ValueError:
                continue
            rows.append({"gpu_uuid": fields[0], "pid": pid, "process_name": fields[2],
                         "used_bytes": memory})
    return rows


def run_child(binary: Path, args, provider: str, ordinal: int, inputs: Path, artifact: Path):
    # Match the production worker: mask by UUID, then CUDA ordinal zero is the
    # configured physical device. The caller's ordinal remains in the report.
    child_ordinal = 0 if provider == "cuda" else ordinal
    command = [str(binary), "--model-dir", str(args.model_dir), "--runtime", str(args.runtime),
               "--inputs", str(inputs), "--provider", provider, "--device-ordinal", str(child_ordinal),
               "--iterations", str(args.iterations), "--arena-max-bytes", str(args.arena_bytes)]
    if args.include_long:
        command.append("--include-long")
    environment = os.environ.copy()
    if args.preload:
        environment["LD_PRELOAD"] = args.preload
    if provider == "cuda":
        environment["CUDA_VISIBLE_DEVICES"] = args.gpu_uuid
    started = time.monotonic()
    try:
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   text=True, env=environment)
    except OSError as error:
        return {"status": "unavailable", "provider": provider.upper(), "error": str(error)}, [], [], []
    samples = []
    other_workloads = []
    cross_gpu_workloads = []
    sample_gpu = provider == "cuda"
    while process.poll() is None:
        if time.monotonic() - started > args.child_timeout:
            process.kill()
            process.wait()
            report = {"status": "unavailable", "provider": provider.upper(), "error": "benchmark child timed out"}
            artifact.write_text(json.dumps(report, indent=2) + "\n")
            return report, samples, other_workloads, cross_gpu_workloads
        sample_started = time.monotonic()
        try:
            if not sample_gpu:
                time.sleep(0.05)
                continue
            workloads = compute_workloads()
            samples.append({"elapsed_ms": round((time.monotonic() - started) * 1000),
                            "process_gpu_memory_bytes": sum(row["used_bytes"] for row in workloads
                                                             if row["pid"] == process.pid and row["gpu_uuid"].lower() == args.gpu_uuid.lower()),
                            "workloads": workloads})
            other_workloads.extend(row for row in workloads if row["pid"] != process.pid)
            cross_gpu_workloads.extend(row for row in workloads
                                       if row["pid"] == process.pid and row["gpu_uuid"].lower() != args.gpu_uuid.lower())
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            samples.append({"elapsed_ms": round((time.monotonic() - started) * 1000), "error": str(error)})
        time.sleep(max(0.0, 0.05 - (time.monotonic() - sample_started)))
    stdout, stderr = process.communicate()
    report = None
    for line in reversed(stdout.splitlines()):
        try:
            report = json.loads(line)
            break
        except json.JSONDecodeError:
            continue
    if report is None:
        report = {"status": "unavailable", "provider": provider.upper(), "error": stderr[-4000:] or "benchmark emitted no JSON"}
    report["exit_code"] = process.returncode
    report["stderr_tail"] = stderr[-2000:]
    report["wall_ms"] = round((time.monotonic() - started) * 1000)
    report["memory_sampling"] = {"interval_ms": 50, "sample_count": len(samples), "samples": samples,
                                 "sampling_limit_bytes": MAX_MEMORY_BYTES}
    artifact.write_text(json.dumps(report, indent=2) + "\n")
    return report, samples, other_workloads, cross_gpu_workloads


def unique_workloads(rows):
    """Keep one report row for each observed process/device pair."""
    unique = {}
    for row in rows:
        unique[(row["gpu_uuid"], row["pid"])] = row
    return list(unique.values())


def benchmark(args) -> dict:
    artifact = args.artifact_dir
    artifact.mkdir(parents=True, exist_ok=True)
    frozen_path = artifact / "frozen_inputs.json"
    frozen = freeze_inputs(args.selected_files, args.source_root, frozen_path)
    binary = args.binary
    if not args.no_build:
        subprocess.run(["cargo", "build", "--release", "--features", "semantic-cuda", "--example", "semantic_benchmark"],
                       check=True, cwd=args.repo_root, timeout=args.build_timeout)
    if not binary.exists():
        raise RuntimeError(f"benchmark binary does not exist: {binary}")
    cpu, _, cpu_other, cpu_cross = run_child(binary, args, "cpu", 0, frozen_path, artifact / "cpu_result.json")
    try:
        inventory = gpu_inventory()
        initial_workloads = compute_workloads()
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        report = {"status": "unavailable", "reason": f"nvidia-smi unavailable: {error}", "cpu": cpu}
        (artifact / "gpu_benchmark.json").write_text(json.dumps(report, indent=2) + "\n")
        return report
    target = next((row for row in inventory if row["uuid"].lower() == args.gpu_uuid.lower()), None)
    if target is None:
        report = {"status": "unavailable", "reason": "configured 4090 UUID is not present", "gpu_inventory": inventory,
                  "other_gpu_workloads": unique_workloads(initial_workloads), "cpu": cpu}
        (artifact / "gpu_benchmark.json").write_text(json.dumps(report, indent=2) + "\n")
        return report
    args.ordinal = target["index"]
    try:
        existing = [row for row in initial_workloads if row["gpu_uuid"].lower() == args.gpu_uuid.lower()]
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        report = {"status": "unavailable", "reason": f"cannot inspect 4090 workloads: {error}", "gpu": target, "cpu": cpu}
        (artifact / "gpu_benchmark.json").write_text(json.dumps(report, indent=2) + "\n")
        return report
    if existing:
        report = {"status": "unavailable", "reason": "4090 is not exclusive", "other_gpu_workloads": unique_workloads(initial_workloads), "cpu": cpu}
        (artifact / "gpu_benchmark.json").write_text(json.dumps(report, indent=2) + "\n")
        return report
    cuda, samples, cuda_other, cuda_cross = run_child(binary, args, "cuda", target["index"], frozen_path, artifact / "cuda_result.json")
    memory = [sample.get("process_gpu_memory_bytes", 0) for sample in samples if "process_gpu_memory_bytes" in sample]
    peak = max(memory, default=None)
    cpu_rate = next((s.get("items_per_second") for s in cpu.get("scenarios", []) if s.get("name") == "frozen_256_regions"), None)
    cuda_rate = next((s.get("items_per_second") for s in cuda.get("scenarios", []) if s.get("name") == "frozen_256_regions"), None)
    speedup = cuda_rate / cpu_rate if cpu_rate and cuda_rate else None
    long_probe = next((s for s in cuda.get("scenarios", []) if s.get("name") == "8192_tokens_singleton"), None)
    long_status = long_probe.get("status") if long_probe else None
    reasons = []
    if cpu.get("status") != "passed" or cuda.get("status") != "passed":
        reasons.append("provider benchmark unavailable, failed, or OOM")
    if speedup is None or speedup < 2.0:
        reasons.append("CUDA throughput did not reach 2x CPU")
    if peak is None:
        reasons.append("GPU memory was not sampled")
    elif peak > MAX_MEMORY_BYTES:
        reasons.append("sampled process GPU memory exceeded 16 GiB")
    if long_probe is None:
        reasons.append("8192-token singleton scenario is missing")
    elif long_status in {"failed", "error"}:
        reasons.append("8192-token singleton was accepted or inference failed")
    elif long_probe and not long_probe.get("expected_rejection"):
        reasons.append("8192-token singleton did not meet the expected 4096-token rejection contract")
    if long_status == "skipped":
        reasons.append("8192-token singleton was not exercised")
    if cuda_cross:
        reasons.append("CUDA child allocated on another GPU")
    report = {"status": "passed" if not reasons else "failed", "target": "4090", "gpu": target,
              "gpu_inventory": inventory, "configured_device_ordinal": target["index"],
              "providers_order": ["CPU", "CUDA"], "arena_max_bytes": args.arena_bytes,
              "throughput": {"cpu_items_per_second": cpu_rate, "cuda_items_per_second": cuda_rate, "speedup": speedup, "required_speedup": 2.0},
              "gpu_memory": {"peak_process_bytes": peak, "limit_bytes": MAX_MEMORY_BYTES, "sample_interval_ms": 50, "sample_count": len(samples)},
              "other_gpu_workloads": unique_workloads(initial_workloads + cpu_other + cuda_other),
              "child_cross_gpu_allocations": unique_workloads(cpu_cross + cuda_cross), "reasons": reasons,
              "input_shape": frozen["shape"], "cpu": cpu, "cuda": cuda}
    (artifact / "gpu_benchmark.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--model-dir", type=Path, default=Path(os.environ.get("TRUFFLEPIG_MODEL_DIR", "/home/josh/.cache/codex-tmp/gpu-search-runtime/model")))
    parser.add_argument("--runtime", type=Path, default=Path(os.environ.get("ORT_DYLIB_PATH", DEFAULT_RUNTIME)))
    parser.add_argument("--preload", default=os.environ.get("LD_PRELOAD", DEFAULT_PRELOAD))
    parser.add_argument("--selected-files", type=Path, required=True)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--artifact-dir", type=Path, default=Path("/home/josh/.cache/codex-tmp/gpu-search-evaluation"))
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--gpu-uuid", default=DEFAULT_GPU_UUID)
    parser.add_argument("--iterations", type=int, default=1)
    parser.add_argument("--arena-bytes", type=int, default=ARENA_BYTES)
    parser.add_argument("--build-timeout", type=int, default=900)
    parser.add_argument("--child-timeout", type=int, default=600)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--include-long", action="store_true")
    args = parser.parse_args()
    args.repo_root = args.repo_root.resolve()
    args.binary = (args.binary or args.repo_root / "target/release/examples/semantic_benchmark").resolve()
    args.selected_files = args.selected_files.resolve()
    args.source_root = args.source_root.resolve()
    args.artifact_dir = args.artifact_dir.resolve()
    args.iterations = max(1, args.iterations)
    if args.arena_bytes <= 0:
        parser.error("--arena-bytes must be positive")
    return args


if __name__ == "__main__":
    try:
        result = benchmark(parse_args())
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        result = {"status": "unavailable", "reason": str(error)}
    print(json.dumps(result, indent=2))
    sys.exit(0 if result.get("status") == "passed" else 1)
