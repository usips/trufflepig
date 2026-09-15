"""Frozen development navigation evaluation helpers.

The package keeps task labels and snapshot fingerprints on the grader side.
Use :func:`solver_task` to construct the task view given to an agent.
"""

from .schema import (
    ARMS,
    CORPORA,
    MAX_OUTPUT_TOKENS_PER_CALL,
    MAX_EMITTED_TOKENS,
    MAX_TOOL_CALLS,
    SCHEMA_VERSION,
    SPLIT,
    TASK_TIMEOUT_SECONDS,
    WORKFLOW_VERSION,
    corpus_roots,
    grade_task,
    load_manifest,
    solver_task,
    solver_tasks,
    validate_manifest,
    verify_snapshot,
)

__all__ = [
    "ARMS",
    "CORPORA",
    "MAX_OUTPUT_TOKENS_PER_CALL",
    "MAX_EMITTED_TOKENS",
    "MAX_TOOL_CALLS",
    "SCHEMA_VERSION",
    "SPLIT",
    "TASK_TIMEOUT_SECONDS",
    "WORKFLOW_VERSION",
    "corpus_roots",
    "grade_task",
    "load_manifest",
    "solver_task",
    "solver_tasks",
    "validate_manifest",
    "verify_snapshot",
]
