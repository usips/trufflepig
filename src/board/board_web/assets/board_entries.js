export function createBoardEntries(context) {
  const { dom, navigate, entryRecord, collection, nextAfter, pageParams, entryKinds } = context;
  const { el, add, refLink, badge, actorName, timeNode, empty, omitted, title, field, retainForm, link } = dom;
  function filtersForm(route, includePlan = true) {
    const form = el("form", "filters");
    const fields = [];
    if (includePlan) fields.push(field("Plan", "plan", route.plan, { placeholder: "P#" }));
    fields.push(field("Kind", "kind", route.kind, { choices: [["", "All kinds"], ...entryKinds] }));
    fields.push(field("Harness", "harness", route.harness, { placeholder: "codex, human…" }));
    fields.push(field("User", "user", route.user, { placeholder: "user" }));
    fields.push(field("Host", "host", route.host, { placeholder: "host" }));
    fields.push(field("Task", "task", route.task, { placeholder: "P#.N" }));
    const submit = el("button", "compact", "Filter"); submit.type = "submit";
    add(form, fields.map(item => item.wrapper), submit);
    form.addEventListener("submit", event => {
      event.preventDefault();
      navigate(route.view, { ref: route.ref, tab: route.tab, plan: route.plan, ...Object.fromEntries(new FormData(form)) });
    });
    return retainForm(form, `filters:${route.view}:${route.ref || "all"}`);
  }
  function entriesTable(records) {
    const table = el("table", "entry-table");
    const caption = el("caption", "sr-only", "Board entries");
    const head = el("thead"), heading = el("tr"), body = el("tbody");
    for (const label of ["Time", "Actor", "Model / effort", "Kind", "Task", "Refs", "Text"]) {
      const column = el("th", "", label); column.scope = "col"; heading.append(column);
    }
    head.append(heading); add(table, caption, head, body);
    for (const record of records) {
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
      add(row, cells); body.append(row);
    }
    return add(el("div", "entry-table-scroll"), table);
  }
  function entriesPage(data, route, heading = true) {
    const page = el("div");
    if (heading) page.append(title("Entries", "Append-only evidence and conversation."));
    page.append(filtersForm(route, !route.ref));
    const entries = collection(data, "entries");
    add(page, entriesTable(entries), entries.length ? null : empty("No entries match these filters."), omitted(data.omitted, "entries"));
    if (nextAfter(data) !== null) page.append(add(el("div", "pager"), link("Next entries", route.view, pageParams(route, data), "button")));
    return page;
  }
  return { filtersForm, entriesPage, entriesTable };
}
