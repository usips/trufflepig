# Board Projects

## Registry

The serving host's registered workspaces define board Projects, named after each workspace.
Registration uses the [workspace registry](workspace-contract.md#membership-and-discovery);
Projects are derived from the current host's files rather than stored in the board database.
`trufflepig ws list` lists registered configurations and their members without selecting a
workspace or opening an index. Its default output uses tab-separated workspace and member lines;
`--json` returns a `workspaces` array with `name`, `id`, `config_path`, `members`, and `error`.
Members carry `name`, `root`, and `available`. Output paths use the normal percent encoding.

Listing retains missing and invalid configuration entries with an error and no members.
Their names use the configuration filename stem, or the parent directory for
`trufflepig.workspace.toml`; their IDs hash the resolved absolute configuration path.
If path resolution fails, the registry-relative absolute path supplies the entry's path and ID.
Readable configurations retain their canonical path identity, and aliases of the same
configuration appear once, in registry order. Distinct configurations can share a name.
Missing member directories remain in the list with `available: false`. A missing registry
is empty; an unreadable or malformed registry is an error. Automatic workspace selection
retains its [strict registry validation](workspace-contract.md#membership-and-discovery).

## Read scopes

Overview, Attention, Inbox, Claims, and Feed require `ReadScope` on requests and replies.
Protocol versions follow the [board contract](board-contract.md).
Its JSON forms are `"all"`, `{"repo":"KEY"}`, `{"keys":["KEY1","KEY2"]}`, and `"unscoped"`.
`All` reads across repositories. `Repo` includes linked and unlinked plans and preserves
addressed messages and the reader's feedback. `Keys` includes a plan if any member key is linked
to it, excluding unlinked plans. `Unscoped` includes only real plans with no `plan_repos` row.
`Keys` and `Unscoped` constrain addressed messages, owned feedback, stale claims, and events;
planless events do not qualify. An explicit Feed plan must itself belong to the scope, even if
a shared event has evidence for other plans. Repository sets bind as one JSON array in SQL.

The serving host resolves a Project ID or unique raw workspace name into `Keys` before
`LocalBoard` dispatch. IDs take precedence over names; duplicate names report matching IDs.
Bare CLI Overview, Attention, and Inbox use the current root's `Repo` identity when resolved,
or `All` when no identity is available; `--all` selects `All`. Bare Feed retains its global
default. A Claims scope intersects the requested plan; regular plan reads using `All` remain
global.
The web request's optional top-level `project` requires a scoped read with `scope: "all"`.
An explicit scope and a Project selector cannot be combined. `LocalBoard` never reads workspace
files. Plan Overview and Show records expose sorted `repo_keys` from durable `plan_repos`,
including links whose checkout paths have been removed.

The host-only `Projects` operation returns Project records with `id`, display `name`, `repo_keys`,
`plan_count`, and `unavailable` members. Counts include each matching plan once per Project.
Unavailable members carry `name`, a percent-encoded `root`, and `available: false`.
Projects reads an existing database in one query-only snapshot; absent storage returns zero
counts and snapshot zero without creating directories or a database. Existing storage errors
remain errors; Projects never creates or migrates storage.
