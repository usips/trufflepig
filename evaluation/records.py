"""Version-one runner-neutral task, event, outcome, and usage records."""

from dataclasses import asdict, dataclass, field
from typing import Optional

SCHEMA_VERSION = 1
WORKFLOW_VERSION = "navigation-v1"


@dataclass(frozen=True)
class TaskRecord:
    task_id: str
    query: str
    relevant: list[dict]
    snapshot: dict
    requires_ctx: bool = False
    schema_version: int = SCHEMA_VERSION
    workflow_version: str = WORKFLOW_VERSION
    record_type: str = "task"


@dataclass(frozen=True)
class UsageRecord:
    tool_calls: int
    stdout_bytes: int
    stderr_bytes: int
    elapsed_seconds: float
    output_tokens: Optional[int] = None
    tokenizer: Optional[str] = None
    token_cost_censored: bool = True
    input_tokens: Optional[int] = None
    record_type: str = "usage"
    schema_version: int = SCHEMA_VERSION

    def __post_init__(self):
        if min(self.tool_calls, self.stdout_bytes, self.stderr_bytes) < 0:
            raise ValueError("usage counts must be nonnegative")
        if self.output_tokens is not None and not self.tokenizer:
            raise ValueError("token counts require a named tokenizer")
        if not self.token_cost_censored and self.output_tokens is None:
            raise ValueError("complete token cost requires an observed count")


@dataclass(frozen=True)
class EventRecord:
    task_id: str
    sequence: int
    operation: str
    status: str
    complete_delivery: bool
    usage: UsageRecord
    identities: list[dict] = field(default_factory=list)
    original_ranks: list[int] = field(default_factory=list)
    coverage: Optional[dict] = None
    truncated: bool = False
    record_type: str = "event"
    schema_version: int = SCHEMA_VERSION


@dataclass(frozen=True)
class OutcomeRecord:
    task_id: str
    metadata_discovered: int
    source_evidenced: int
    relevant_total: int
    context_satisfied: bool
    status: str
    stop_reason: str
    usage: UsageRecord
    tokens_to_first_evidence: Optional[int]
    tokens_to_complete_evidence: Optional[int]
    evidence_cost_censored: bool
    record_type: str = "outcome"
    schema_version: int = SCHEMA_VERSION


def record_dict(record):
    return asdict(record)
