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

PERMISSION_BASE_PHP = "src/XF/Permission/Builder.php"
PERMISSION_SEARCH_PHP = "src/addons/USIPS/EphyraSearch/XF/Permission/Builder.php"
PERMISSION_CHAT_PHP = "src/addons/USIPS/EphyraChat/XF/Permission/Builder.php"
FORUM_BASE_PHP = "src/XF/Pub/Controller/ForumController.php"
FORUM_XFES_PHP = "src/addons/XFES/XF/Pub/Controller/Forum.php"
FORUM_MULTISITE_PHP = "src/addons/XenCentral/MultiSite/Pub/Controller/XF/Forum.php"
USER_BASE_PHP = "src/XF/Entity/User.php"
USER_SIROPU_PHP = "src/addons/Siropu/ReferralContests/XF/Entity/User.php"
USER_UIX_PHP = "src/addons/ThemeHouse/UIX/XF/Entity/User.php"
USER_NCMEC_PHP = "src/addons/USIPS/NCMEC/XF/Entity/User.php"
USER_XFMG_PHP = "src/addons/XFMG/XF/Entity/User.php"
USER_XFRM_PHP = "src/addons/XFRM/XF/Entity/User.php"

XFCP_REFERENCE_FIXTURES = {
    "XFCP_MediaItem": {EXTENSION_PHP: 1},
    "XFCP_Builder": {PERMISSION_SEARCH_PHP: 1, PERMISSION_CHAT_PHP: 1},
    "XFCP_Forum": {FORUM_XFES_PHP: 1, FORUM_MULTISITE_PHP: 1},
    "XFCP_User": {
        USER_SIROPU_PHP: 1,
        USER_UIX_PHP: 1,
        USER_NCMEC_PHP: 1,
        USER_XFMG_PHP: 1,
        USER_XFRM_PHP: 1,
    },
}

PARENT_CALL_FIXTURES = {
    "rebuildCombination": {PERMISSION_SEARCH_PHP: 1, PERMISSION_CHAT_PHP: 1},
    "rebuildCombinationContent": {PERMISSION_SEARCH_PHP: 1, PERMISSION_CHAT_PHP: 1},
    "actionIndex": {FORUM_MULTISITE_PHP: 2},
    "getStructure": {USER_XFMG_PHP: 1, USER_XFRM_PHP: 1},
}

XFCP_EXTENSION_METADATA = (
    ("permission_search", "src/addons/USIPS/EphyraSearch/_data/class_extensions.xml",
     r"XF\Permission\Builder", r"USIPS\EphyraSearch\XF\Permission\Builder", "10"),
    ("permission_chat", "src/addons/USIPS/EphyraChat/_data/class_extensions.xml",
     r"XF\Permission\Builder", r"USIPS\EphyraChat\XF\Permission\Builder", "20"),
    ("forum_xfes", "src/addons/XFES/_data/class_extensions.xml",
     r"XF\Pub\Controller\Forum", r"XFES\XF\Pub\Controller\Forum", "10"),
    ("forum_multisite", "src/addons/XenCentral/MultiSite/_data/class_extensions.xml",
     r"XF\Pub\Controller\Forum", r"XenCentral\MultiSite\Pub\Controller\XF\Forum", "10"),
    ("user_siropu", "src/addons/Siropu/ReferralContests/_data/class_extensions.xml",
     r"XF\Entity\User", r"Siropu\ReferralContests\XF\Entity\User", "10"),
    ("user_uix", "src/addons/ThemeHouse/UIX/_data/class_extensions.xml",
     r"XF\Entity\User", r"ThemeHouse\UIX\XF\Entity\User", "10"),
    ("user_ncmec", "src/addons/USIPS/NCMEC/_data/class_extensions.xml",
     r"XF\Entity\User", r"USIPS\NCMEC\XF\Entity\User", "10"),
    ("user_xfmg", "src/addons/XFMG/_data/class_extensions.xml",
     r"XF\Entity\User", r"XFMG\XF\Entity\User", "10"),
    ("user_xfrm", "src/addons/XFRM/_data/class_extensions.xml",
     r"XF\Entity\User", r"XFRM\XF\Entity\User", "10"),
)

XFCP_EXTENSION_SOURCES = {
    PERMISSION_SEARCH_PHP: (
        "class Builder extends XFCP_Builder",
        "parent::rebuildCombination(",
        "parent::rebuildCombinationContent(",
    ),
    PERMISSION_CHAT_PHP: (
        "class Builder extends XFCP_Builder",
        "parent::rebuildCombination(",
        "parent::rebuildCombinationContent(",
    ),
    FORUM_XFES_PHP: ("class Forum extends XFCP_Forum",),
    FORUM_MULTISITE_PHP: (
        "class Forum extends XFCP_Forum",
        "parent::actionIndex(",
    ),
    USER_SIROPU_PHP: ("class User extends XFCP_User",),
    USER_UIX_PHP: ("class User extends XFCP_User",),
    USER_NCMEC_PHP: ("class User extends XFCP_User",),
    USER_XFMG_PHP: ("class User extends XFCP_User", "parent::getStructure("),
    USER_XFRM_PHP: ("class User extends XFCP_User", "parent::getStructure("),
}

XENFORO_FIXTURE_PHP_FILES = (
    PERMISSION_BASE_PHP,
    PERMISSION_SEARCH_PHP,
    PERMISSION_CHAT_PHP,
    FORUM_BASE_PHP,
    FORUM_XFES_PHP,
    FORUM_MULTISITE_PHP,
    USER_BASE_PHP,
    USER_SIROPU_PHP,
    USER_UIX_PHP,
    USER_NCMEC_PHP,
    USER_XFMG_PHP,
    USER_XFRM_PHP,
)

XFCP_RELATIONSHIP_KINDS = (
    "framework_parent_candidate",
    "php_extends_candidate",
    "php_parent_call_candidate",
    "inheritance_issue",
)

XFCP_OCCURRENCE_PROVENANCE_PREFIX = "xenforo_generated_placeholder"
PARENT_CALL_OCCURRENCE_PROVENANCE_PREFIX = "php_parent_call_candidate"


class Acceptance:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.failures: list[str] = []
        self.calls = 0
        self.counts: dict[str, int] = {}
        self.errors: list[str] = []
        self.root = args.xf_root.resolve()
        self.cache = args.cache_root.resolve()
        self.source_lines: dict[str, list[str]] = {}
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
        return len(mappings) == 1 and self.source_xfcp_fixtures()

    def source_xfcp_fixtures(self) -> bool:
        metadata_paths = {entry[1] for entry in XFCP_EXTENSION_METADATA}
        required = set(XENFORO_FIXTURE_PHP_FILES) | metadata_paths
        present = all((self.root / relative).is_file() for relative in required)
        if not self.check("xfcp_chain_fixtures_present", present):
            return False

        metadata: dict[str, ET.Element] = {}
        try:
            for relative in metadata_paths:
                metadata[relative] = ET.parse(self.root / relative).getroot()
            sources = {
                relative: (self.root / relative).read_text()
                for relative in XFCP_EXTENSION_SOURCES
            }
            permission_base = (self.root / PERMISSION_BASE_PHP).read_text()
            forum_base = (self.root / FORUM_BASE_PHP).read_text()
            user_base = (self.root / USER_BASE_PHP).read_text()
        except (OSError, ET.ParseError, UnicodeDecodeError):
            return self.check("xfcp_chain_fixtures_readable", False)
        self.check("xfcp_chain_fixtures_readable", True)

        metadata_checks = []
        for label, xml_path, from_class, to_class, execute_order in XFCP_EXTENSION_METADATA:
            tree = metadata[xml_path]
            matches = [node for node in tree.iter("extension")
                       if node.get("from_class") == from_class
                       and node.get("to_class") == to_class
                       and node.get("active") == "1"
                       and node.get("execute_order") == execute_order]
            metadata_checks.append(len(matches) == 1)
            self.check(f"xfcp_metadata_{label}", len(matches) == 1)
        forum_orders = [order for label, _, _, _, order in XFCP_EXTENSION_METADATA
                        if label.startswith("forum_")]
        user_orders = [order for label, _, _, _, order in XFCP_EXTENSION_METADATA
                       if label.startswith("user_")]
        self.check("xfcp_forum_order_tie", forum_orders == ["10", "10"])
        self.check("xfcp_user_order_tie", user_orders == ["10"] * 5)

        source_checks = []
        for relative, markers in XFCP_EXTENSION_SOURCES.items():
            source = sources[relative]
            matches = all(marker in source for marker in markers)
            source_checks.append(matches)
            label = relative.removeprefix("src/addons/").replace("/", "_") \
                .removesuffix(".php").lower()
            self.check(f"xfcp_source_{label}", matches)

        forum_base_ok = "class ForumController extends" in forum_base
        self.check("forum_legacy_alias_base_fixture", forum_base_ok)
        self.check("permission_builder_base_fixture", "class Builder" in permission_base)
        self.check("user_entity_base_fixture", "class User" in user_base)
        return (all(metadata_checks) and all(source_checks) and forum_base_ok
                and "class Builder" in permission_base and "class User" in user_base)

    def reference_hits_for_fixture(
        self,
        label: str,
        symbol: str,
        expected: dict[str, int],
        snippet_marker: str,
    ) -> list[dict[str, Any]]:
        expected_paths = tuple(expected)
        matches: list[dict[str, Any]] = []
        observed: dict[str, int] = {}
        cursors: set[str] = set()
        page = self.invoke(f"refs_{label}", "refs", symbol)
        page_number = 0
        while page and page_number < 100:
            for hit in self.hits(page):
                relative = next(
                    (path for path in expected_paths if self.hit_path(hit).endswith(path)),
                    None,
                )
                if relative is not None and self.hit_contains_source_marker(
                    hit, relative, snippet_marker,
                ):
                    matches.append(hit)
                    observed[relative] = observed.get(relative, 0) + 1
            if all(observed.get(path, 0) >= count for path, count in expected.items()):
                break
            cursor = page.get("next")
            if not isinstance(cursor, str) or cursor in cursors:
                break
            cursors.add(cursor)
            page_number += 1
            page = self.invoke(f"refs_{label}_page_{page_number}", "more", cursor)
        if page_number == 100 and page and isinstance(page.get("next"), str):
            self.failures.append(f"refs_{label}_pagination_cap")
        self.counts[f"{label}_reference_hits"] = len(matches)
        self.check(f"refs_{label}_source_occurrences", observed == expected)

        candidate_hits = [hit for hit in matches
                          if hit.get("target") is None
                          and isinstance(hit.get("candidates"), list)
                          and bool(hit["candidates"])]
        self.counts[f"{label}_candidate_hits"] = len(candidate_hits)
        self.counts[f"{label}_candidate_ids"] = sum(
            len(hit["candidates"]) for hit in candidate_hits
        )
        self.check(f"refs_{label}_candidate_shape",
                   len(candidate_hits) == len(matches) and bool(matches)
                   and all(hit.get("resolution") == "candidate" for hit in matches))
        return matches

    def hit_contains_source_marker(
        self,
        hit: dict[str, Any],
        relative_path: str,
        marker: str,
    ) -> bool:
        try:
            line = int(hit.get("start_line", 0))
            if relative_path not in self.source_lines:
                self.source_lines[relative_path] = (self.root / relative_path).read_text(
                    encoding="utf-8", errors="replace",
                ).splitlines()
            source_lines = self.source_lines[relative_path]
            return 1 <= line <= len(source_lines) and marker in source_lines[line - 1]
        except (OSError, TypeError, ValueError):
            return False

    def symbol_context_for_fixture(
        self,
        label: str,
        symbol: str,
        relative_path: str,
        contexts: list[tuple[str, list[dict[str, Any]]]],
    ) -> None:
        search = self.invoke(
            f"search_{label}",
            "search",
            f"sym:{symbol}",
            f"file:{relative_path}",
            "lang:php",
        )
        hit = next((candidate for candidate in self.hits(search)
                    if self.hit_path(candidate).endswith(relative_path)
                    and candidate.get("name") == symbol), None) if search else None
        if not hit or not isinstance(hit.get("handle"), str):
            self.failures.append(f"ctx_{label}")
            return
        context = self.invoke(f"ctx_{label}", "ctx", hit["handle"])
        relationships = context.get("relationships", []) if context else []
        if not isinstance(relationships, list):
            self.failures.append(f"ctx_{label}_relationships")
            return
        contexts.append((self.hit_path(hit), [relationship for relationship in relationships
                                             if isinstance(relationship, dict)]))

    @staticmethod
    def relationship_target_path(relationship: dict[str, Any]) -> str:
        target = relationship.get("target")
        return Acceptance.hit_path(target) if isinstance(target, dict) else ""

    @staticmethod
    def relationship_source_path(relationship: dict[str, Any]) -> str:
        source = relationship.get("source")
        return Acceptance.hit_path(source) if isinstance(source, dict) else ""

    def check_xfcp_context_relationships(
        self,
        contexts: list[tuple[str, list[dict[str, Any]]]],
    ) -> None:
        unique_relationships = {
            json.dumps(relationship, sort_keys=True): relationship
            for _, relationships in contexts
            for relationship in relationships
        }
        relationships = list(unique_relationships.values())
        for kind in XFCP_RELATIONSHIP_KINDS:
            matches = [relationship for relationship in relationships
                       if relationship.get("kind") == kind]
            self.counts[f"{kind}_relationships"] = len(matches)
            self.check(f"ctx_{kind}", bool(matches))

        candidate_kinds = XFCP_RELATIONSHIP_KINDS[:-1]
        for kind in candidate_kinds:
            self.check(f"ctx_{kind}_resolution",
                       any(relationship.get("kind") == kind
                           and relationship.get("resolution") == "candidate"
                           for relationship in relationships))

        framework_edges = [relationship for relationship in relationships
                           if relationship.get("kind") == "framework_parent_candidate"]
        self.check("ctx_forum_framework_parent_candidate", any(
            "ForumController" in json.dumps(relationship)
            and any(path in json.dumps(relationship) for path in
                    (FORUM_XFES_PHP, FORUM_MULTISITE_PHP))
            for relationship in framework_edges))

        permission_parent_edges = [relationship for relationship in relationships
                                   if relationship.get("kind") == "php_parent_call_candidate"
                                   and self.relationship_source_path(relationship).endswith(
                                       PERMISSION_SEARCH_PHP)]
        self.check("ctx_search_parent_calls_reach_base", all(
            any(self.relationship_target_path(relationship).endswith(PERMISSION_BASE_PHP)
                and method in json.dumps(relationship)
                for relationship in permission_parent_edges)
            for method in ("rebuildCombination", "rebuildCombinationContent")))

        chat_parent_edges = [relationship for relationship in relationships
                             if relationship.get("kind") == "php_parent_call_candidate"
                             and self.relationship_source_path(relationship).endswith(
                                 PERMISSION_CHAT_PHP)]
        self.check("ctx_chat_parent_calls_reach_search", all(
            any(self.relationship_target_path(relationship).endswith(PERMISSION_SEARCH_PHP)
                and method in json.dumps(relationship)
                for relationship in chat_parent_edges)
            for method in ("rebuildCombination", "rebuildCombinationContent")))

        forum_tie_contexts = [relationships for path, relationships in contexts
                              if path.endswith(FORUM_XFES_PHP)
                              or path.endswith(FORUM_MULTISITE_PHP)]
        user_tie_contexts = [relationships for path, relationships in contexts
                             if any(path.endswith(expected)
                                    for expected in XFCP_REFERENCE_FIXTURES["XFCP_User"])]
        forum_tie_edges = [relationship for context in forum_tie_contexts
                           for relationship in context
                           if relationship.get("kind") == "framework_parent_candidate"
                           and relationship.get("resolution") == "candidate"
                           and "priority_tie=10" in str(relationship.get("provenance", ""))]
        user_tie_edges = [relationship for context in user_tie_contexts
                          for relationship in context
                          if relationship.get("kind") == "framework_parent_candidate"
                          and relationship.get("resolution") == "candidate"
                          and "priority_tie=10" in str(relationship.get("provenance", ""))]
        forum_paths = (FORUM_XFES_PHP, FORUM_MULTISITE_PHP)
        forum_targets = (FORUM_BASE_PHP, *forum_paths)
        expected_forum_pairs = {
            (source, target)
            for source in forum_paths
            for target in forum_targets
            if target != source
        } | {(source, FORUM_BASE_PHP) for source in forum_paths}
        user_paths = tuple(XFCP_REFERENCE_FIXTURES["XFCP_User"])
        expected_user_pairs = {
            (source, target)
            for source in user_paths
            for target in (USER_BASE_PHP, *user_paths)
            if target != source
        } | {(source, USER_BASE_PHP) for source in user_paths}

        def tie_pairs(edges: list[dict[str, Any]], source_paths: tuple[str, ...],
                      target_paths: tuple[str, ...]) -> set[tuple[str, str]]:
            pairs: set[tuple[str, str]] = set()
            for edge in edges:
                source = self.relationship_source_path(edge)
                target = self.relationship_target_path(edge)
                source_fixture = next((path for path in source_paths
                                       if source.endswith(path)), None)
                target_fixture = next((path for path in target_paths
                                       if target.endswith(path)), None)
                if source_fixture and target_fixture:
                    pairs.add((source_fixture, target_fixture))
            return pairs

        observed_forum_pairs = tie_pairs(
            forum_tie_edges, forum_paths, (FORUM_BASE_PHP, *forum_paths),
        )
        observed_user_pairs = tie_pairs(
            user_tie_edges, user_paths, (USER_BASE_PHP, *user_paths),
        )
        self.counts["ctx_forum_tie_candidate_edges"] = len(observed_forum_pairs)
        self.counts["ctx_user_tie_candidate_edges"] = len(observed_user_pairs)
        self.check("ctx_forum_tied_candidate_group",
                   expected_forum_pairs <= observed_forum_pairs)
        self.check("ctx_user_tied_candidate_group",
                   expected_user_pairs <= observed_user_pairs)
        self.check("ctx_inheritance_issue_explanation", any(
            relationship.get("kind") == "inheritance_issue"
            and relationship.get("resolution") == "unresolved"
            and bool(relationship.get("provenance"))
            for relationship in relationships))

    def run_navigation(self) -> None:
        context_records: list[tuple[str, list[dict[str, Any]]]] = []
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

        placeholder_refs = self.reference_hits_for_fixture(
            "xfcp_trampoline",
            XFCP_NAME,
            XFCP_REFERENCE_FIXTURES[XFCP_NAME],
            f"extends {XFCP_NAME}",
        )
        self.counts["xfcp_reference_hits"] = len(placeholder_refs)
        self.check("refs_xfcp_trampoline", bool(placeholder_refs)
                   and all(hit.get("target") is None
                           and isinstance(hit.get("candidates"), list)
                           and bool(hit["candidates"])
                           and hit.get("resolution") == "candidate"
                           for hit in placeholder_refs))

        for name, expected in XFCP_REFERENCE_FIXTURES.items():
            if name == XFCP_NAME:
                continue
            self.reference_hits_for_fixture(
                f"{name.lower()}_trampoline",
                name,
                expected,
                f"extends {name}",
            )

        for method, expected in PARENT_CALL_FIXTURES.items():
            self.reference_hits_for_fixture(
                f"parent_{method.lower()}",
                method,
                expected,
                f"parent::{method}(",
            )

        class_context_fixtures = (
            ("permission_search_class", "Builder", PERMISSION_SEARCH_PHP),
            ("permission_chat_class", "Builder", PERMISSION_CHAT_PHP),
            ("forum_xfes_class", "Forum", FORUM_XFES_PHP),
            ("forum_multisite_class", "Forum", FORUM_MULTISITE_PHP),
            ("user_siropu_class", "User", USER_SIROPU_PHP),
            ("user_uix_class", "User", USER_UIX_PHP),
            ("user_ncmec_class", "User", USER_NCMEC_PHP),
            ("user_xfmg_class", "User", USER_XFMG_PHP),
            ("user_xfrm_class", "User", USER_XFRM_PHP),
        )
        for label, symbol, path in class_context_fixtures:
            self.symbol_context_for_fixture(label, symbol, path, context_records)
        for method, paths in PARENT_CALL_FIXTURES.items():
            for index, path in enumerate(paths, start=1):
                self.symbol_context_for_fixture(
                    f"parent_{method.lower()}_{index}", method, path, context_records
                )

        self.symbol_context_for_fixture(
            "forum_controller_base",
            "ForumController",
            FORUM_BASE_PHP,
            context_records,
        )
        self.symbol_context_for_fixture(
            "permission_builder_base",
            "Builder",
            PERMISSION_BASE_PHP,
            context_records,
        )
        self.symbol_context_for_fixture(
            "user_entity_base",
            "User",
            USER_BASE_PHP,
            context_records,
        )

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
        elif extension_hit:
            context_records.append((self.hit_path(extension_hit), [
                relation for relation in extension_relations if isinstance(relation, dict)
            ]))
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

        self.check_xfcp_context_relationships(context_records)

    def index_metrics(self) -> dict[str, Any]:
        database = self.cache / "index.sqlite3"
        fixture_paths = tuple(dict.fromkeys((
            CORE_POST,
            EXTENSION_PHP,
            BASE_PHP,
            *XENFORO_FIXTURE_PHP_FILES,
        )))
        occurrence_names = tuple(dict.fromkeys((
            *XFCP_REFERENCE_FIXTURES.keys(),
            *PARENT_CALL_FIXTURES.keys(),
        )))
        try:
            connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
            rows = connection.execute(
                "SELECT status, COUNT(*) FROM files WHERE language='php' GROUP BY status"
            ).fetchall()
            fixture_placeholders = ",".join("?" for _ in fixture_paths)
            fixture_rows = connection.execute(
                f"SELECT path, status FROM files WHERE path IN ({fixture_placeholders})",
                fixture_paths,
            ).fetchall()
            occurrence_placeholders = ",".join("?" for _ in fixture_paths)
            name_placeholders = ",".join("?" for _ in occurrence_names)
            occurrence_rows = connection.execute(
                "SELECT f.path, o.name, o.role, o.provenance, o.target, o.candidates "
                "FROM occurrences o JOIN files f ON f.id=o.file_id "
                f"WHERE f.path IN ({occurrence_placeholders}) "
                f"AND o.name IN ({name_placeholders})",
                (*fixture_paths, *occurrence_names),
            ).fetchall()
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
            fixture_statuses.get(path) == "complete"
            for path in (CORE_POST, EXTENSION_PHP, BASE_PHP)))
        self.check("xfcp_chain_php_complete", all(
            fixture_statuses.get(path) == "complete" for path in XENFORO_FIXTURE_PHP_FILES))

        expected_occurrences: dict[tuple[str, str], tuple[int, str, str | None]] = {}
        for name, files in XFCP_REFERENCE_FIXTURES.items():
            for path, count in files.items():
                expected_occurrences[(path, name)] = (
                    count, XFCP_OCCURRENCE_PROVENANCE_PREFIX, None,
                )
        for name, files in PARENT_CALL_FIXTURES.items():
            for path, count in files.items():
                expected_occurrences[(path, name)] = (
                    count, PARENT_CALL_OCCURRENCE_PROVENANCE_PREFIX, "call",
                )

        observed_occurrences: dict[tuple[str, str], list[tuple[Any, ...]]] = {}
        for row in occurrence_rows:
            path, name, role, provenance = row[:4]
            key = (str(path), str(name))
            expected = expected_occurrences.get(key)
            if (expected is not None
                    and (expected[2] is None or role == expected[2])
                    and role != "declaration"
                    and str(provenance).startswith(expected[1])):
                observed_occurrences.setdefault(key, []).append(row)

        occurrence_counts_match = all(
            len(observed_occurrences.get(key, [])) == expected[0]
            for key, expected in expected_occurrences.items()
        )
        self.check("xfcp_occurrence_facts_present", occurrence_counts_match)
        selected_occurrences = [row for facts in observed_occurrences.values() for row in facts]
        candidate_occurrences = []
        candidate_ids: set[int] = set()
        for _, _, _, _, target, encoded_candidates in selected_occurrences:
            try:
                candidates = json.loads(encoded_candidates)
            except (json.JSONDecodeError, TypeError):
                candidates = None
            if target is None and isinstance(candidates, list) and candidates:
                candidate_occurrences.append((target, candidates))
                candidate_ids.update(candidate for candidate in candidates
                                     if isinstance(candidate, int) and not isinstance(candidate, bool))

        self.counts["xfcp_source_occurrences"] = sum(
            len(observed_occurrences.get((path, name), []))
            for name, files in XFCP_REFERENCE_FIXTURES.items()
            for path in files
        )
        self.counts["php_parent_call_occurrences"] = sum(
            len(observed_occurrences.get((path, name), []))
            for name, files in PARENT_CALL_FIXTURES.items()
            for path in files
        )
        self.counts["candidate_occurrences"] = len(candidate_occurrences)
        self.counts["candidate_ids"] = sum(len(candidates)
                                            for _, candidates in candidate_occurrences)
        self.counts["unique_candidate_ids"] = len(candidate_ids)
        self.check("xfcp_occurrence_targets_unresolved",
                   len(candidate_occurrences) == len(selected_occurrences)
                   and bool(selected_occurrences))
        self.check("xfcp_occurrence_candidate_ids",
                   len(candidate_ids) > 0
                   and all(isinstance(candidate, int) and not isinstance(candidate, bool)
                           for _, candidates in candidate_occurrences for candidate in candidates))

        if candidate_ids:
            candidate_placeholders = ",".join("?" for _ in candidate_ids)
            indexed_candidate_ids = {int(row[0]) for row in connection.execute(
                f"SELECT id FROM definitions WHERE id IN ({candidate_placeholders})",
                tuple(candidate_ids),
            ).fetchall()}
        else:
            indexed_candidate_ids = set()
        self.check("xfcp_candidate_ids_indexed", candidate_ids == indexed_candidate_ids)
        connection.close()

        placeholder_uses = [row for row in selected_occurrences
                            if row[0] == EXTENSION_PHP and row[1] == XFCP_NAME
                            and row[2] != "declaration"
                            and str(row[3]).startswith(XFCP_OCCURRENCE_PROVENANCE_PREFIX)]
        self.counts["xfcp_placeholder_use_facts"] = len(placeholder_uses)
        placeholder_candidate_use = False
        for row in placeholder_uses:
            try:
                placeholder_candidate_use |= row[4] is None and bool(json.loads(row[5]))
            except (json.JSONDecodeError, TypeError):
                continue
        self.check("xfcp_placeholder_index_provenance", placeholder_candidate_use)
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
