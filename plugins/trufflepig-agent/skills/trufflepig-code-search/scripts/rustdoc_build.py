"""Build one Cargo target's rustdoc JSON and record its compilation context."""

import fcntl
import hashlib
import json
from pathlib import Path
import subprocess
import time


def capture(command, cwd):
    return subprocess.check_output(command, cwd=cwd, text=True).strip()


def select_package(metadata, manifest, name):
    members = [p for p in metadata["packages"] if p["id"] in metadata["workspace_members"]]
    if name:
        selected = [p for p in members if p["name"] == name]
    else:
        selected = [p for p in members if Path(p["manifest_path"]).resolve() == manifest]
        if not selected and len(members) == 1:
            selected = members
    if len(selected) != 1:
        raise ValueError("Select one workspace package with --package: "
                         + ", ".join(p["name"] for p in members))
    return selected[0]


def run_rustdoc(command, cwd, package_id, crate):
    features = None
    with subprocess.Popen(command, cwd=cwd, stdout=subprocess.PIPE, text=True) as process:
        for line in process.stdout:
            message = json.loads(line)
            if (message.get("reason") == "compiler-artifact"
                    and message.get("package_id") == package_id
                    and message["target"]["name"].replace("-", "_") == crate):
                features = message["features"]
        if process.wait():
            raise subprocess.CalledProcessError(process.returncode, command)
    if features is None:
        raise ValueError("Cargo did not report the selected target's resolved features")
    return features


def build_document(args):
    manifest = Path(args.manifest_path).resolve(strict=True)
    cwd = manifest.parent
    # rustup run fails for an absent toolchain instead of installing it implicitly.
    runner = ["rustup", "run", args.toolchain]
    compiler = capture([*runner, "rustc", "-vV"], cwd)
    host = next(line.removeprefix("host: ") for line in compiler.splitlines()
                if line.startswith("host: "))
    target = args.target or host
    if "/" in target or "\\" in target or target in (".", ".."):
        raise ValueError("--target must be a built-in target triple, not a JSON target path")
    metadata = json.loads(capture([
        *runner, "cargo", "metadata", "--format-version", "1", "--no-deps",
        "--locked", "--manifest-path", str(manifest),
    ], cwd))
    package = select_package(metadata, manifest, args.package)
    targets = [t for t in package["targets"]
               if (args.bin and "bin" in t["kind"] and t["name"] == args.bin)
               or (not args.bin and any(k in t["kind"] for k in
                                       ("lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro")))]
    if len(targets) != 1:
        raise ValueError("Select one library (default) or an existing --bin NAME")
    crate = targets[0]["name"].replace("-", "_")
    target_dir = Path(metadata["target_directory"])
    target_dir.mkdir(parents=True, exist_ok=True)
    artifact = target_dir / target / "doc" / f"{crate}.json"
    command = [*runner, "cargo", "rustdoc", "--locked", "--message-format=json-render-diagnostics",
               "--manifest-path", str(manifest),
               "--package", package["name"], "--target-dir", str(target_dir),
               "--target", target]
    command += ["--bin", args.bin] if args.bin else ["--lib"]
    if args.features:
        command += ["--features", args.features]
    if args.no_default_features:
        command.append("--no-default-features")
    command += ["--", "-Z", "unstable-options", "--output-format", "json",
                "--document-private-items", "--document-hidden-items"]
    # Serialize helper invocations and never consume a previous build after failure.
    with (target_dir / ".trufflepig-rustdoc.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        artifact.unlink(missing_ok=True)
        started = time.time()
        features = run_rustdoc(command, cwd, package["id"], crate)
        raw = artifact.read_bytes()
    document = json.loads(raw)
    provenance = {
        "package": package["name"], "manifest": package["manifest_path"],
        "workspace": metadata["workspace_root"], "crate": crate,
        "source_root": metadata["workspace_root"],
        "target": target, "toolchain": args.toolchain, "compiler": compiler,
        "requested_features": args.features, "default_features": not args.no_default_features,
        "resolved_features": features,
        "command": command, "artifact": str(artifact),
        "artifact_sha256": hashlib.sha256(raw).hexdigest(),
        "build_started_unix": started, "format_version": document.get("format_version"),
        "scope": "selected target; includes private and hidden items; dependencies are not indexed",
        "verification": "build evidence only; verify current source with trufflepig-agent show",
    }
    return document, provenance
