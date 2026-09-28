#!/usr/bin/env python3
"""Exercise PHP navigation and XenForo add-on relationships on a real checkout."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import time
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Any


CACHE_BASE = Path.home() / ".cache" / "codex-tmp"
CORE_POST = "src/XF/Entity/Post.php"
BASE_PHP = "src/addons/XFMG/Finder/MediaItem.php"
EXTENSION_PHP = "src/addons/USIPS/NCMEC/XFMG/Finder/MediaItem.php"
EXTENSION_XML = "src/addons/USIPS/NCMEC/_data/class_extensions.xml"
EXTENSION_MANIFEST = "src/addons/USIPS/NCMEC/addon.json"
BASE_MANIFEST = "src/addons/XFMG/addon.json"
EXTENSION_ID = "USIPS/NCMEC"
XFCP_NAME = "XFCP_MediaItem"
BASE_CLASS = r"XFMG\Finder\MediaItem"
EXTENSION_CLASS = r"USIPS\NCMEC\XFMG\Finder\MediaItem"


class Acceptance:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.failures: list[str] = []
        self.calls = 0
        self.counts: dict[str, int] = {}
        self.errors: list[str] = []
        self.root = args.xf_root.resolve()
        self.cache = args.cache_root.resolve()
        self.system_dir = self.cache / "system"
        self.spool_dir = self.cache / "spool"
        self.processes: dict[str, subprocess.Popen[bytes]] = {}

    def check(self, name: str, condition: bool) -> bool:
        if not condition:
            self.failures.append(name)
        return condition

    def invoke(self, name: str, *words: str) -> dict[str, Any] | None:
        self.calls += 1
        command = [str(self.args.binary), "--root", str(self.root), "--no-workspace",
                   "--cache", str(self.cache), "--limit", "500", "--budget", "12000",
                   "--diagnostics", "off", "--json"]
        command.extend(words)
        environment = self.environment()
        try:
            result = subprocess.run(command, cwd=self.root, env=environment, capture_output=True,
                                    text=True, timeout=self.args.timeout, check=False)
        except subprocess.TimeoutExpired:
            self.failures.append(name)
            self.errors.append(f"{name}:timeout")
            return None
        except OSError as error:
            self.failures.append(name)
            self.errors.append(f"{name}:{type(error).__name__}")
            return None
        if result.returncode != 0:
            self.failures.append(name)
            self.errors.append(f"{name}:exit_{result.returncode}")
            return None
        try:
            value = json.loads(result.stdout)
        except (json.JSONDecodeError, TypeError):
            self.failures.append(name)
            self.errors.append(f"{name}:invalid_json")
            return None
        if not isinstance(value, dict) or "error" in value:
            self.failures.append(name)
            self.errors.append(f"{name}:cli_error")
            return None
        return value

    def environment(self) -> dict[str, str]:
        environment = os.environ.copy()
        for name in ("TMPDIR", "TMP", "TEMP"):
            environment[name] = str(self.cache)
        if self.args.new_build:
            environment["XDG_CACHE_HOME"] = str(self.cache / "xdg-cache")
            environment["TRUFFLEPIG_SYSTEM_DIR"] = str(self.system_dir)
            environment["TRUFFLEPIG_SPOOL_DIR"] = str(self.spool_dir)
        return environment

    @staticmethod
    def hits(value: dict[str, Any]) -> list[dict[str, Any]]:
        hits = value.get("hits")
        return [hit for hit in hits if isinstance(hit, dict)] if isinstance(hits, list) else []

    @staticmethod
    def hit_path(hit: dict[str, Any]) -> str:
        return str(hit.get("path") or hit.get("file") or "")

    def daemon_command(self, verb: str) -> list[str]:
        command = [str(self.args.binary), "--diagnostics", "off"]
        if verb == "serve":
            command.extend([
                "--root", str(self.root), "--no-workspace", "--cache", str(self.cache),
                "--history-cache", str(self.cache / "history"),
            ])
        command.append(verb)
        return command

    def start_owned(self, name: str, command: list[str]) -> bool:
        try:
            self.processes[name] = subprocess.Popen(
                command, cwd=self.root, env=self.environment(), stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
        except OSError as error:
            self.failures.append(f"start_{name}")
            self.errors.append(f"start_{name}:{type(error).__name__}")
            return False
        return True

    def published_php_files(self) -> tuple[int, int] | None:
        try:
            connection = sqlite3.connect(f"file:{self.cache / 'index.sqlite3'}?mode=ro", uri=True)
            count = connection.execute(
                "SELECT COUNT(*) FROM files WHERE language='php'"
            ).fetchone()[0]
            generation = int(connection.execute(
                "SELECT value FROM meta WHERE key='generation'"
            ).fetchone()[0])
            connection.close()
            return int(count), generation
        except (sqlite3.Error, TypeError, ValueError):
            return None

    def wait_for_index(self) -> bool:
        root_process = self.processes["root"]
        deadline = time.monotonic() + self.args.timeout
        saw_stage = False
        while time.monotonic() < deadline:
            if root_process.poll() is not None:
                self.failures.append("root_daemon_exited_during_index")
                self.errors.append(f"root_daemon:exit_{root_process.returncode}")
                return False
            staging = any(path.is_dir() and path.name.startswith("stage-")
                          for path in self.cache.iterdir())
            saw_stage |= staging
            published = self.published_php_files()
            if saw_stage and not staging and published and published[0] >= 5_000 and published[1] > 0:
                self.counts["php_indexed_files"] = published[0]
                return True
            time.sleep(0.5)
        self.failures.append("php_index_publication_timeout")
        self.errors.append("root_daemon:timeout")
        return False

    def start_new_build_daemons(self) -> bool:
        if (self.cache / "daemon.sock").exists() or (self.system_dir / "daemon.sock").exists():
            self.failures.append("isolated_daemon_socket_present")
            self.errors.append("cache:already_in_use")
            return False
        print("Indexing the full XenForo checkout in an isolated daemon...", file=sys.stderr, flush=True)
        if not self.start_owned("root", self.daemon_command("serve")) or not self.wait_for_index():
            return False
        print("XenForo index published; starting its private router...", file=sys.stderr, flush=True)
        if not self.start_owned("system", self.daemon_command("system-serve")):
            return False
        socket = self.system_dir / "daemon.sock"
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            system_process = self.processes["system"]
            if system_process.poll() is not None:
                self.failures.append("system_daemon_exited_at_startup")
                self.errors.append(f"system_daemon:exit_{system_process.returncode}")
                return False
            if socket.exists() and self.processes["root"].poll() is None:
                return True
            time.sleep(0.05)
        self.failures.append("system_daemon_startup_timeout")
        self.errors.append("system_daemon:timeout")
        return False

    def stop_owned_daemons(self) -> None:
        stop_commands = {
            "root": [str(self.args.binary), "--root", str(self.root), "--no-workspace",
                     "--cache", str(self.cache), "--history-cache", str(self.cache / "history"), "stop"],
            "system": [str(self.args.binary), "system", "stop"],
        }
        for name in ("root", "system"):
            process = self.processes.get(name)
            if process is None or process.poll() is not None:
                continue
            try:
                subprocess.run(stop_commands[name], cwd=self.root, env=self.environment(),
                               stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, timeout=8, check=False)
            except subprocess.TimeoutExpired:
                self.errors.append(f"stop_{name}:timeout")
            except OSError as error:
                self.errors.append(f"stop_{name}:{type(error).__name__}")
        for name in ("root", "system"):
            process = self.processes.get(name)
            if process is None:
                continue
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                self.errors.append(f"stop_{name}:terminated")

    def source_fixture(self) -> bool:
        required = (CORE_POST, EXTENSION_PHP, BASE_PHP, EXTENSION_XML, EXTENSION_MANIFEST, BASE_MANIFEST)
        exists = all((self.root / relative).is_file() for relative in required)
        if not self.check("xenforo_fixture_present", exists):
            return False
        try:
            manifest = json.loads((self.root / EXTENSION_MANIFEST).read_text())
            tree = ET.parse(self.root / EXTENSION_XML)
            source = (self.root / EXTENSION_PHP).read_text()
        except (OSError, ValueError, ET.ParseError):
            return self.check("xenforo_fixture_readable", False)

        mappings = [node for node in tree.getroot().iter("extension")
                    if node.get("from_class") == BASE_CLASS
                    and node.get("to_class") == EXTENSION_CLASS and node.get("active") == "1"]
        self.check("active_class_extension_metadata", len(mappings) == 1)
        self.check("implicit_addon_requirement_fixture", manifest.get("require") in ([], {}))
        self.check("xfcp_trampoline_fixture", f"extends {XFCP_NAME}" in source)
        return len(mappings) == 1

    def run_navigation(self) -> None:
        post_search = self.invoke(
            "search_core_post",
            "search",
            "sym:Post",
            f"file:{CORE_POST}",
            "lang:php",
        )
        post_hits = self.hits(post_search) if post_search else []
        post_hit = next((hit for hit in post_hits if self.hit_path(hit).endswith(CORE_POST)), None)
        self.counts["core_symbol_hits"] = len(post_hits)
        self.check("search_core_post_definition", post_hit is not None)

        if post_hit and isinstance(post_hit.get("handle"), str):
            shown = self.invoke("show_core_post", "show", post_hit["handle"])
            shown_text = json.dumps(shown) if shown else ""
            self.check("show_core_post_body", "class Post" in shown_text and "canView" in shown_text)
        else:
            self.failures.append("show_core_post_body")

        outline = self.invoke("map_core_post", "map", CORE_POST)
        outline_text = json.dumps(outline) if outline else ""
        outline_hits = self.hits(outline) if outline else []
        self.counts["outline_entries"] = len(outline_hits)
        self.check("map_core_post_methods", "canView" in outline_text and "getContentUrl" in outline_text)

        references = self.invoke("refs_xfcp_trampoline", "refs", XFCP_NAME)
        reference_hits = self.hits(references) if references else []
        self.counts["xfcp_reference_hits"] = len(reference_hits)
        placeholder_refs = [hit for hit in reference_hits
                            if self.hit_path(hit).endswith(EXTENSION_PHP)
                            and f"extends {XFCP_NAME}" in json.dumps(hit.get("snippet", {}))]
        self.check("refs_xfcp_trampoline", bool(placeholder_refs)
                   and XFCP_NAME in json.dumps(references)
                   and all(hit.get("target") is None and not hit.get("candidates")
                           and hit.get("resolution") == "unresolved"
                           for hit in placeholder_refs))

        extension_search = self.invoke("search_extension_class", "search", "sym:MediaItem",
                                       f"file:{Path(EXTENSION_PHP).parent}", "lang:php")
        extension_hits = self.hits(extension_search) if extension_search else []
        extension_hit = next(
            (hit for hit in extension_hits if self.hit_path(hit).endswith(EXTENSION_PHP)),
            None,
        )
        self.counts["extension_symbol_hits"] = len(extension_hits)
        self.check("search_extension_class", extension_hit is not None)

        extension_context = None
        if extension_hit and isinstance(extension_hit.get("handle"), str):
            extension_context = self.invoke(
                "ctx_class_extension",
                "ctx",
                extension_hit["handle"],
            )
        else:
            self.failures.append("ctx_class_extension")
        extension_relations = (
            extension_context.get("relationships", []) if extension_context else []
        )
        if not isinstance(extension_relations, list):
            extension_relations = []
        class_edges = [rel for rel in extension_relations if isinstance(rel, dict)
                       and rel.get("kind") == "xenforo_class_extension_candidate"
                       and rel.get("resolution") == "candidate"]
        matching_class_edges = [
            relation
            for relation in class_edges
            if BASE_PHP in json.dumps(relation)
            and EXTENSION_PHP in json.dumps(relation)
            and "xenforo_class_extensions_xml" in str(relation.get("provenance", ""))
            and "active=1" in str(relation.get("provenance", ""))
            and "execute_order=10" in str(relation.get("provenance", ""))
            and "runtime_xfcp_chain_unresolved" in str(relation.get("provenance", ""))
        ]
        self.counts["class_extension_candidates"] = len(class_edges)
        self.check("ctx_class_extension_candidate", bool(matching_class_edges))

        addon_search = self.invoke("search_addon_manifest", "search", f"sym:{EXTENSION_ID}",
                                   f"file:{EXTENSION_MANIFEST}")
        addon_hits = self.hits(addon_search) if addon_search else []
        addon_hit = next(
            (hit for hit in addon_hits if self.hit_path(hit).endswith(EXTENSION_MANIFEST)),
            None,
        )
        self.counts["addon_manifest_hits"] = len(addon_hits)
        self.check("search_addon_manifest_definition", addon_hit is not None)

        addon_context = None
        if addon_hit and isinstance(addon_hit.get("handle"), str):
            addon_context = self.invoke("ctx_addon_dependency", "ctx", addon_hit["handle"])
        else:
            self.failures.append("ctx_addon_dependency")
        addon_relations = addon_context.get("relationships", []) if addon_context else []
        if not isinstance(addon_relations, list):
            addon_relations = []
        addon_edges = [rel for rel in addon_relations if isinstance(rel, dict)
                       and rel.get("kind") == "xenforo_addon_extension_dependency_candidate"
                       and rel.get("resolution") == "candidate"
                       and EXTENSION_MANIFEST in json.dumps(rel)
                       and BASE_MANIFEST in json.dumps(rel)]
        self.counts["addon_extension_dependency_candidates"] = len(addon_edges)
        self.check("ctx_addon_extension_dependency_candidate", bool(addon_edges))

    def index_metrics(self) -> dict[str, Any]:
        database = self.cache / "index.sqlite3"
        try:
            connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
            rows = connection.execute(
                "SELECT status, COUNT(*) FROM files WHERE language='php' GROUP BY status"
            ).fetchall()
            fixture_rows = connection.execute(
                "SELECT path, status FROM files WHERE path IN (?, ?, ?)",
                (CORE_POST, EXTENSION_PHP, BASE_PHP),
            ).fetchall()
            placeholder_rows = connection.execute(
                "SELECT o.role, o.provenance, o.target, o.candidates FROM occurrences o "
                "JOIN files f ON f.id=o.file_id WHERE f.path=? AND o.name=?",
                (EXTENSION_PHP, XFCP_NAME),
            ).fetchall()
            connection.close()
        except sqlite3.Error:
            self.failures.append("php_index_metrics")
            return {"files": 0, "statuses": {}, "complete_percent": 0.0}

        statuses = {str(status): int(count) for status, count in rows}
        total = sum(statuses.values())
        complete = statuses.get("complete", 0)
        complete_percent = 100.0 * complete / total if total else 0.0
        self.counts["php_indexed_files"] = total
        self.check("whole_xenforo_php_file_count", total >= 5_000)
        self.check("php_complete_index_floor", total > 0 and complete_percent >= 95.0)
        fixture_statuses = {path: status for path, status in fixture_rows}
        self.check("core_and_addon_php_complete", all(
            fixture_statuses.get(path) == "complete" for path in (CORE_POST, EXTENSION_PHP, BASE_PHP)))
        placeholder_uses = [row for row in placeholder_rows
                            if row[0] != "declaration" and row[1] == "xenforo_generated_placeholder"]
        self.counts["xfcp_placeholder_use_facts"] = len(placeholder_uses)
        self.check("xfcp_placeholder_index_provenance", any(
            target is None and json.loads(candidates) == []
            for _, _, target, candidates in placeholder_uses))
        return {
            "files": total,
            "statuses": statuses,
            "complete_percent": round(complete_percent, 2),
            "minimum_files": 5_000,
            "minimum_complete_percent": 95.0,
        }

    def execute(self) -> int:
        self.cache.mkdir(parents=True, exist_ok=True)
        if not self.source_fixture():
            print(json.dumps({"status": "fail", "failed_checks": self.failures,
                              "errors": self.errors}))
            return 1
        try:
            if self.args.new_build:
                if self.start_new_build_daemons():
                    print("XenForo index published; running navigation checks...",
                          file=sys.stderr, flush=True)
                    self.run_navigation()
            else:
                print("Indexing the full XenForo checkout...", file=sys.stderr, flush=True)
                if self.invoke("index_xenforo", "index") is None:
                    self.failures.append("index_xenforo")
                else:
                    self.run_navigation()
        finally:
            if self.args.new_build:
                self.stop_owned_daemons()
        php_index = self.index_metrics()
        report = {
            "status": "fail" if self.failures else "pass",
            "cli_calls": self.calls,
            "counts": self.counts,
            "php_index": php_index,
            "failed_checks": self.failures,
            "errors": self.errors,
        }
        print(json.dumps(report, sort_keys=True))
        return 1 if self.failures else 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="trufflepig executable")
    parser.add_argument("--xf-root", type=Path, required=True, help="XenForo checkout root")
    parser.add_argument("--cache-root", type=Path, required=True,
                        help="scratch/cache directory under ~/.cache/codex-tmp")
    parser.add_argument("--new-build", action="store_true",
                        help="start isolated daemons using this binary for a fresh full index")
    parser.add_argument("--timeout", type=int, default=1800, help="per-command timeout in seconds")
    args = parser.parse_args()
    binary = str(args.binary)
    resolved_binary = args.binary.expanduser()
    if not resolved_binary.is_file():
        located = shutil.which(binary)
        if located is None:
            parser.error("--binary must name an executable file or PATH command")
        resolved_binary = Path(located)
    if not os.access(resolved_binary, os.X_OK):
        parser.error("--binary is not executable")
    args.binary = resolved_binary.resolve()
    args.xf_root = args.xf_root.expanduser()
    if not args.xf_root.is_dir(): parser.error("--xf-root must be an existing checkout directory")
    cache = args.cache_root.expanduser().resolve()
    if cache != CACHE_BASE and CACHE_BASE not in cache.parents:
        parser.error("--cache-root must be inside ~/.cache/codex-tmp")
    args.cache_root = cache
    if args.timeout < 1: parser.error("--timeout must be positive")
    return args


def main() -> int:
    return Acceptance(parse_args()).execute()


if __name__ == "__main__":
    sys.exit(main())
