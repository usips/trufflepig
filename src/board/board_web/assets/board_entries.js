export function createBoardEntries(context) {
  const { state, dom, navigate, entryRecord, collection, entryKinds } = context;
  const { el, add, refLink, badge, actorName, timeNode, empty, omitted, title, field, focusKey, retainForm, link } = dom;
  function filtersForm(route, includePlan = true) {
    const form = el("form", "filters");
    const fields = [];
    if (includePlan) fields.push(field("Plan", "plan", route.plan, { placeholder: "P#" }));
    fields.push(field("Kind", "kind", route.kind, { choices: [["", "All kinds"], ...entryKinds] }));
    fields.push(field("Harness", "harness", route.harness, { placeholder: "codex, human…" }));
    fields.push(field("User", "user", route.user, { placeholder: "user" }));
    fields.push(field("Host", "host", route.host, { placeholder: "host" }));
    fields.push(field("Task", "task", route.task, { placeholder: "P#.N" }));
    const submit = focusKey(el("button", "compact", "Filter"), `filters-submit:${route.view}:${route.ref || "all"}`); submit.type = "submit";
    add(form, fields.map(item => item.wrapper), submit);
    form.addEventListener("submit", event => {
      event.preventDefault();
      navigate(route.view, { ref: route.ref, tab: route.tab, plan: route.plan, ...Object.fromEntries(new FormData(form)) });
    });
    return retainForm(form, `filters:${route.view}:${route.ref || "all"}`);
  }
  function entryRow(record) {
    const entry = entryRecord(record), row = el("tr", "entry-row"); row.dataset.entryId = entry.id;
    const cells = Array.from({ length: 7 }, () => el("td"));
    add(cells[0], refLink(entry.id), el("br"), timeNode(entry.created_at));
    add(cells[1], el("span", "", actorName(entry.actor)), entry.via === "outbox" ? el("span", "provenance", " (spooled, unverified)") : null);
    cells[2].textContent = [entry.model, entry.effort].filter(Boolean).join(" · ") || "—";
    add(cells[3], badge(entry.kind), entry.state ? badge(entry.state.state) : null);
    const refs = entry.refs || [];
    const tasks = refs.filter(ref => /^P[1-9]\d*\.\d+$/.test(String(ref)));
    add(cells[4], tasks.length ? add(el("div", "entry-refs"), tasks.map(ref => refLink(ref))) : el("span", "muted", "—"));
    const linked = el("div", "entry-refs");
    if (entry.plan) linked.append(refLink(entry.plan));
    for (const ref of refs.filter(ref => !tasks.includes(ref))) linked.append(refLink(ref, ref, "mono", { plan: entry.plan, repo_key: entry.repo_key }));
    if (entry.supersedes) add(linked, el("span", "muted", "Supersedes"), refLink(entry.supersedes));
    cells[5].append(linked.childElementCount ? linked : el("span", "muted", "—"));
    cells[6].className = "entry-text-cell"; cells[6].append(el("p", "entry-body", entry.body));
    if (entry.to) cells[6].append(el("p", "small muted", `To ${entry.to}`));
    return add(row, cells);
  }
  function liveMatch(entry, route) {
    const actor = entry.actor || {}, refs = entry.refs || [];
    return (!route.plan || entry.plan === route.plan) && (!route.kind || entry.kind === route.kind) &&
      (!route.harness || actor.harness === route.harness) && (!route.user || actor.user === route.user) && (!route.host || actor.host === route.host) &&
      (!route.task || refs.includes(route.task)) && (!route.references || refs.includes(route.references));
  }
  function entriesTable(records, route = {}, through = null) {
    const table = el("table", "entry-table");
    const caption = el("caption", "sr-only", "Board entries");
    const head = el("thead"), heading = el("tr"), body = el("tbody");
    for (const label of ["Time", "Actor", "Model / effort", "Kind", "Task", "Refs", "Text"]) {
      const column = el("th", "", label); column.scope = "col"; heading.append(column);
    }
    head.append(heading); add(table, caption, head, body);
    // Live rows are newer than the page's snapshot watermark, so they sort
    // above the paged history; a divider marks the seam between the two.
    const live = [];
    if (!route.after && !route.through && through !== null && state.liveEntries.length) {
      const shown = new Set(records.map(record => entryRecord(record)?.id));
      for (const item of state.liveEntries) {
        if (shown.has(item.entry.id) || !liveMatch(item.entry, route)) continue;
        if (BigInt(item.seq) <= BigInt(String(through))) continue;
        live.push(item);
      }
      live.sort((a, b) => BigInt(b.seq) < BigInt(a.seq) ? -1 : BigInt(b.seq) > BigInt(a.seq) ? 1 : 0);
    }
    for (const item of live) body.append(entryRow(item.entry));
    if (live.length && records.length) {
      const divider = el("tr", "live-divider");
      const cell = el("td", "", state.liveGap ? "Live delivery skipped some entries — refresh to catch up." : "New entries since this page loaded");
      cell.colSpan = 7; divider.append(cell); body.append(divider);
    }
    for (const record of records) body.append(entryRow(record));
    return add(el("div", "entry-table-scroll"), table);
  }
  function entriesPage(data, route, heading = true) {
    const page = el("div");
    if (heading) page.append(title("Entries", "Append-only evidence and conversation."));
    page.append(filtersForm(route, !route.ref));
    const entries = collection(data, "entries");
    add(page, entriesTable(entries, route, data.through ?? null), entries.length ? null : empty("No entries match these filters."), omitted(data.omitted, "entries"));
    const pager = el("div", "pager");
    if (route.after) pager.append(link("Newest entries", route.view, { ...route, after: null, through: null }, "button"));
    if (entries.length && data.more_older) {
      const oldest = entryRecord(entries[entries.length - 1]);
      pager.append(link("Older entries", route.view, { ...route, after: JSON.stringify({ seq: String(oldest.seq), entry: oldest.id }), through: null }, "button"));
    }
    if (pager.childElementCount) page.append(pager);
    return page;
  }
  return { filtersForm, entriesPage, entriesTable };
}
