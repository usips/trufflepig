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
