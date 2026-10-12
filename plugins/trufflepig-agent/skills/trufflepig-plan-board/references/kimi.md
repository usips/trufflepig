# Kimi K3 Board setup

Kimi Code discovers this skill under `$KIMI_CODE_HOME/skills` (default
`~/.kimi-code/skills`) and can select it for Board work. Invoke it explicitly
with `/skill:trufflepig-plan-board P7`, replacing `P7` with the user's plan.
Start a fresh Kimi session after installation. A project skill with the same
name takes precedence; `--skills-dir` replaces normal discovery.

Run from the checkout associated with the work. Use Kimi's shell tool and the
absolute wrapper path when it is absent from `PATH`. Prefix each call with
`TRUFFLEPIG_AGENT_HARNESS=kimi`, including search calls: a Kimi session launched
by another agent can inherit that agent's harness environment.

For a session whose configured provider model is `k3` and thinking effort is
`high`, register and read coordination state with separate shell calls:

```sh
TRUFFLEPIG_AGENT_HARNESS=kimi "$HOME/.local/bin/trufflepig-agent" board hello k3 high
TRUFFLEPIG_AGENT_HARNESS=kimi "$HOME/.local/bin/trufflepig-agent" board inbox
TRUFFLEPIG_AGENT_HARNESS=kimi "$HOME/.local/bin/trufflepig-agent" board show
TRUFFLEPIG_AGENT_HARNESS=kimi "$HOME/.local/bin/trufflepig-agent" board attention
```

Use the actual provider model ID and effort for the running session, rather
than assuming every Kimi session uses K3. `kimi-code/k3` is a CLI configuration
alias; `k3` is its provider model ID. For that model, the commit trailer is
`Co-authored-by: k3 <noreply@moonshot.ai>`. Include every contributing model as
required by the shared skill and repository instructions.

The wrapper takes session identity from `TRUFFLEPIG_SESSION`, then
`KIMI_SESSION_ID`, then `AGENT_SESSION_ID`, then the installed session hook's
checkout marker. Check the harness and session returned by `board hello`.
Replace an inherited parent agent's `TRUFFLEPIG_SESSION` with this Kimi
session's ID. If no native ID or session hook is available, choose a unique
explicit `TRUFFLEPIG_SESSION` for this session and include it on every call;
do not claim work using the shared checkout/day fallback. Separate concurrent
Kimi sessions need distinct identities, including delegated workers.

Read the requested plan and task before claiming or changing coordination
state. Board access supports the user's authorized work; it does not authorize
additional implementation, commits, deployment, or messages. Plan approval is
reserved for the human owner or assigned steward. Follow the shared skill for
claims, proposals, stale revisions, review evidence, and feedback.

Use `board web` when the user requests the web board. Its bootstrap URL contains
a secret token; do not store it in files, commits, feedback, or audit notes.
On `board_unavailable` or an API mismatch, report the error once. Do not loop,
silently switch identity, or restart services during ordinary Board work.
