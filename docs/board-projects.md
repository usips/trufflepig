# Board Projects

## Registry

The serving host's registered workspaces define board Projects, named after each workspace.
Registration uses the [workspace registry](workspace-contract.md#membership-and-discovery);
Projects are derived from the current host's files rather than stored in the board database.
`trufflepig ws list` lists registered configurations and their members without selecting a
workspace or opening an index. Its default output uses tab-separated workspace and member lines;
`--json` returns a `workspaces` array with `name`, `id`, `config_path`, `members`, and `error`.
Members carry `name`, `root`, and `available`. Registry listing and Projects output paths follow
the [response path encoding](cli.md).

Listing retains missing and invalid configuration entries with an error and no members.
Their names use the configuration filename stem, or the parent directory for
`trufflepig.workspace.toml`; their IDs hash the resolved absolute configuration path.
If path resolution fails, the registry-relative absolute path supplies the entry's path and ID.
Readable configurations retain their canonical path identity, and aliases of the same
configuration appear once, in registry order. Distinct configurations can share a name.
Missing member directories remain in the list with `available: false`. A missing registry
is empty; an unreadable or malformed registry is an error. Automatic workspace selection
retains its [strict registry validation](workspace-contract.md#membership-and-discovery).

Project enumeration uses every entry in the global registry, including its XDG/home path fallback.
Selecting an invocation workspace does not narrow the Project list.

## Live host resolution

`ProjectResolver` runs at the serving edge (`BoardHost` or `board-serve`), before dispatch to
`LocalBoard`. It resolves the registry and member roots on that host; `LocalBoard` never reads
workspace files or stores Project membership. Host resolution preserves the workspace ID and raw
name separately from the display name in Project records.
Duplicate names display as `name·id[:6]`, while selectors use the full ID or raw name.

Each available member resolves through its Git common directory, so repository subdirectories and
linked worktrees map to the same repository key. Resolution first checks the stored `repo_paths`
binding for the actor host and common directory, using the stored lossy path conversion. Without a
binding, it derives identity through read-only `RepoIdentityCache::register`, without a DB write.
Member keys form a set; duplicate keys do not duplicate plans.

Missing or invalid configurations retain an unavailable Project whose unavailable member represents
the configuration path. Missing directories, non-Git roots, and failed member probes are unavailable
members. A partially available Project uses its resolved keys; a Project with no keys matches no
plans. The host-only Projects read returns `id`, display `name`, `repo_keys`, `unavailable`, and
`plan_count`. Counts include each matching plan once per Project in one query-only snapshot.
A missing board DB produces zero counts and snapshot zero without creating or migrating storage;
unreadable existing storage remains an error.

Resolution caches results for at most 60 seconds and invalidates on registry or configuration
mtime/size changes, host, DB path/presence, or repository override changes. Member-root changes
without a configuration change become visible on the next resolution after the cache expires.

## Read scopes

Overview, Attention, Inbox, Claims, Feed, and board-wide Done reads carry `ReadScope`:

| Scope | JSON | Plan selection |
|---|---|---|
| `All` | `"all"` | No repository filter |
| `Repo` | `{"repo":"K"}` | Repository K, unlinked plans, and read-specific exceptions |
| `Keys` | `{"keys":["K1","K2"]}` | Plans linked to any selected key |
| `Unscoped` | `"unscoped"` | Plans with no `plan_repos` row |

`Repo` preserves addressed-event and own-feedback exceptions to the caller's repository filter.
`Keys` is strict: it excludes unlinked plans and standalone planless events; addressed and feedback
exceptions do not widen it. Empty `Keys` selects nothing. A plan linked to repositories in two
Projects appears in both. Read-specific actor and recipient rules still apply under `All`.
`Unscoped` selects actual plans without repository associations; it excludes standalone planless
events and feedback. `Repo` includes those plans alongside its repository plans and read-specific
exceptions.

Mixed-plan events qualify through same-sequence entry evidence matching the selected scope.
An explicit Feed plan must itself satisfy the scope, even when a shared event has other plan
evidence. Repository key sets bind as one JSON array through SQLite `json_each`.

Bare CLI Overview, Attention, and Inbox use the current root's `Repo` identity when resolved,
or `All` when no identity is available; `--all` selects `All`. Bare Feed retains its global default.
A Claims scope intersects the requested plan; `All` leaves that plan's repository membership
unrestricted.

The serving host resolves a Project ID or raw name to `Keys` before calling `LocalBoard`. Full IDs
take precedence over names. Ambiguity fails `invalid_options: project NAME matches <id1>, <id2>`,
listing IDs in ascending order; the displayed name suffix is a label, not extra selector syntax.
The wire `project` selector requires a scoped read with `All` as its operation scope. Combining it
with another explicit scope fails `invalid_options`; the host replaces `All` with the resolved keys.

## Browser selector

The header selector offers All, each host Project, and Unscoped. Selection scopes Overview,
Needs you, Working now, claims, and the feed/ticker. Stream events trigger reads in that selection;
they do not inject unrelated global events into a selected Project's ticker.

Routes preserve `?project=<id>` or `?project=unscoped`, and tab `sessionStorage` stores the choice
under `trufflepig-board-project:<board-id>`. A route selection takes precedence over stored state.
Navigation and reload retain the choice; All clears the query and stored selection. Switching
selection clears page cursors before reading the new scope.

Plan headers derive chips from current durable `plan_repos` membership in `PlanOverview.repo_keys`
and `PlanView.repo_keys`; stored-revision headers also use the current plan membership. They show
Projects whose keys overlap, or Unscoped only for a plan with no durable keys. Unavailable local
checkouts do not make a linked plan Unscoped. Matching Projects with unavailable members have
warning chips; the selected Project header reports unavailable members. A removed saved Project
remains selected with a Project unavailable warning and a read error; selecting an available
Project recovers the scoped reads. Other route behavior is in the
[web UI contract](board-web-ui.md).

## CLI

`board projects` lists the host Project records described above. `--project NAME|ID` follows
[read-scope resolution](#read-scopes). The reserved `--project unscoped` selects Unscoped; select a
workspace named `unscoped` by ID. `--project` and `--all` are mutually exclusive. The
[CLI grammar](board-cli-contract.md#commands) defines accepted commands; continuation hints retain
the supplied selector.

## Multiple hosts

Projects are live host-local views of registered workspaces and Git bindings. Different hosts can
resolve different membership or availability; a shared board does not make one host's registry
authoritative for another. Cross-host Project membership requires a stored snapshot; live local
resolution supplies no such shared snapshot.
