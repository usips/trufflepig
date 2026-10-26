# Plan lifecycle and task succession

The [board contract](board-contract.md) defines shared identity, transport, and storage rules. The
[CLI contract](board-cli-contract.md#commands) gives command grammar. This contract defines explicit
plan closeout and cross-plan task succession.

## Plan lifecycle

A new plan is open. `board close P7 NOTE…` and `board reopen P7 NOTE…` change only the plan's
lifecycle state and append an audited entry/event; each note is nonblank `EntryText`.

Both operations require `decide` authority for the plan and reject a plan with any unended claim or
worker. Closing also requires every task to be `done` or `transferred`. Close does not change task
statuses, accept or reject proposals, or answer questions. Open proposals and questions remain in the
history and can still be decided or answered.

A closed plan rejects new tasks, task moves, claims, workers, proposals, direct edits, and task
transfers. Existing-plan entries remain writable, including notes, questions, answers, and reviews;
existing proposals can still be accepted or rejected. Reopen requires `decide` authority and writes
an audited state change without changing task or proposal state.

## Session claim release

A session bye releases only the caller's active lead claims and moves their `doing` tasks to `todo`.
For each ended claim, the board writes a plan-scoped `claim` entry at the bye's event sequence. The
entry records `session_ended`, references the exact claim-start entry and the planless bye entry, and
includes the task reference so it remains available in task entries. The bye entry and the
claim-end entries share the operation sequence; the references distinguish each claim when one bye
ends several claims.

A plan feed includes the single planless bye event for every affected plan through those
same-sequence claim-end entries. The event remains one global session event; task-specific causes
stay in the linked plan and task entries. A repeated bye adds no entries or event, and a child
worker's bye does not end its lead's claim.

## Task succession

`board transfer P7.3 P8 NOTE…` transfers a task to a different plan. Both plans must be open; the
source task must be current, not `done`, and have no unended claim or worker. The actor needs `decide`
authority on both plans. If the source task has an assignee, the actor also needs `assign` authority
on the destination plan.

A transfer creates a new task ordinal in the destination with the source title, assignee, section,
and current column. The source task remains in its plan. Its displayed `transferred` column is
derived from the transfer link and is read-only; the transfer operation is the only way to create it.
Normal task history and references remain attached to the source. The transfer entry is written to
the source plan and references both task IDs. The immutable transfer record keeps source and
destination authority bases and grant entries for audit.

Task views follow the full succession chain. They retain history and open questions from earlier
cards; questions reach the current successor's holder and workers, and the human owner of the
destination plan. Task state changes and further transfers require the current card. Task-addressed
entries can reference an earlier card and remain visible in the successor's lineage history.

## Text and storage bounds

Transfer and lifecycle notes use nonblank `EntryText`, limited to 32 KiB of UTF-8 bytes. Feedback
keeps its separate 4 KiB aggregate limit. API 13 and schema migration 13 carry plan lifecycle state
and immutable task-transfer records; the [board contract](board-contract.md#storage-configuration-and-search)
defines migration compatibility.
