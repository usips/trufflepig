# Kimi Board setup

Kimi Code discovers this skill under `$KIMI_CODE_HOME/skills` (default
`~/.kimi-code/skills`). Invoke `/skill:trufflepig-plan-board P7` with the plan
from the work brief. Read the shared plan-board skill for coordinator, lead,
worker, reviewer, progress, and feedback steps.

Use Kimi's shell and the checkout's `trufflepig-agent` wrapper. A native
environment marker key containing `KIMI` or a Kimi process ancestor detects the
harness and takes priority over the fallback hint. Check the returned actor with
`trufflepig-agent board hello`; never use `--client human` or `--client cli`.
If neither native signal is available, set `TRUFFLEPIG_AGENT_HARNESS=kimi` for
that call. If Kimi does not expose a unique native session ID, set a unique
`TRUFFLEPIG_SESSION` for this session and each separately spawned worker.
Replace an inherited parent value with the worker's own ID. This override sets
the actor session; it does not change harness detection. Do not share session
IDs between the lead and its workers.

For a configured K3 session whose provider model is `k3` and thinking effort
is `high`, use separate shell calls:

```sh
trufflepig-agent board hello k3 high
trufflepig-agent board show P7
trufflepig-agent board pending
```

Use the provider's actual model ID and effort for other configurations.
`kimi-code/k3` is a CLI configuration alias; `k3` is the provider model ID.
For that model the commit trailer is `Co-authored-by: k3 <noreply@moonshot.ai>`.
Include every contributing model with its actual co-author identity.

`board pending` is read-only: it neither acknowledges activity nor renews the
claim. Do not poll it on every turn. File Trufflepig feedback only when a
failure forces a workaround; use the canonical body and limits in the
[feedback contract](https://github.com/usips/trufflepig/blob/master/docs/board-feedback-contract.md).

Use `board web` when the user requests the web board. Its bootstrap URL contains
a secret token; do not store it in files, commits, feedback, or audit notes.
On `board_unavailable` or an API mismatch, report the error once. Do not loop,
silently switch identity, or restart services during ordinary Board work.
