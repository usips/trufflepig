import { createBoardDetails } from "/board_details.js";

export function createBoardViews(context) {
  const { state, dom, board, jsonFetch, navigate, scheduleRefresh, submitMutation, notice, apiVersion, entryRecord, collection, permitted, nextAfter, pageParams, queryFilters, postKinds, entryKinds, columns, errorMessage } = context;
  const { el, add, button, routeUrl, link, refLink, badge, actorName, shortActor, stamp, timeNode, age, ageNode, panel, empty, omitted, title, field, formStatus, planId, focusKey, retainForm } = dom;
  function entryCard(record, options = {}) {
    const entry = entryRecord(record);
    if (!entry) return empty("Entry unavailable.");
    const item = el("article", "entry-card");
    const meta = el("div", "entry-meta");
    add(meta, refLink(entry.id), badge(entry.kind), el("span", "", actorName(entry.actor)), entry.via === "outbox" ? el("span", "provenance", "(spooled, unverified)") : null,
      entry.model ? el("span", "", `${entry.model}${entry.effort ? ` · ${entry.effort}` : ""}`) : null,
      entry.state ? badge(entry.state.state || entry.state) : null, timeNode(entry.created_at));
    add(item, meta, el("p", "entry-body", entry.body));
    const refs = el("div", "entry-refs");
    if (entry.plan) refs.append(refLink(entry.plan));
    for (const ref of entry.refs || []) refs.append(refLink(ref, ref, "mono", { plan: entry.plan, repo_key: entry.repo_key }));
    if (entry.to) refs.append(el("span", "muted", `To ${entry.to}`));
    if (entry.supersedes) add(refs, el("span", "muted", "Supersedes"), refLink(entry.supersedes));
    if (refs.childElementCount) item.append(refs);
    if (options.actions) item.append(options.actions);
    return item;
  }
  function eventList(events) {
    const list = el("ol", "event-list");
    for (const event of events || []) {
      const item = el("li", "event-row");
      add(item, badge(event.kind), refLink(event.subject), el("span", "event-summary", event.summary),
        el("span", "muted small", shortActor(event.actor)), event.via === "outbox" ? el("span", "provenance", "(spooled, unverified)") : null, timeNode(event.created_at));
      list.append(item);
    }
    return list.childElementCount ? list : empty("No recent activity.");
  }
  function workingCard(claim) {
    const item = el("div", "working-card"); item.dataset.stale = claim.stale;
    add(item, add(el("div", "row spread"), el("strong", "", shortActor(claim.actor)), claim.stale ? badge("stale") : badge("doing")),
      refLink(claim.task), el("div", "muted small", actorName(claim.actor)), claim.model || claim.effort ? el("div", "small", [claim.model, claim.effort].filter(Boolean).join(" · ")) : null,
      add(el("div", "small"), el("span", "muted", "Active "), ageNode(claim.last_active)), el("div", "small", claim.scope));
    return item;
  }
  function taskCard(task, claims, options = {}) {
    const claim = (claims || []).find(item => item.task === task.id && !item.ended_at);
    const item = el("article", "task-card");
    if (claim) item.dataset.stale = claim.stale;
    add(item, refLink(task.id, task.title, ""), el("div", "small muted", task.id),
      claim ? add(el("div", "small"), el("span", "", actorName(claim.actor)), el("span", "muted", " · "), ageNode(claim.last_active)) :
        (task.assignee ? el("div", "small muted", `Assigned: ${task.assignee}`) : null), claim?.stale ? badge("stale") : null);
    if (claim?.model || claim?.effort) item.append(el("div", "small muted", [claim.model, claim.effort].filter(Boolean).join(" · ")));
    if (!claim && options.claimsOmitted) item.append(link("Claim details omitted", "claims", { plan: options.plan || planId(task.id),
      after: options.claimAfter ? JSON.stringify(options.claimAfter) : null, through: options.through }, "small"));
    return item;
  }
  function attentionItems(data) {
    return collection(data, "entries").map(record => ({ type: entryRecord(record).kind, record }));
  }
  function attentionCard(item) {
    const entry = entryRecord(item.record);
    const card = el("article", "attention-card");
    add(card, add(el("div", "row"), badge(item.type), entry?.plan ? refLink(entry.plan) : null),
      refLink(entry?.id || item.record.entry, entry?.body || item.record.summary || "Open entry", ""),
      add(el("p"), el("span", "", entry?.actor ? `From ${actorName(entry.actor)}` : ""), entry?.via === "outbox" ? el("span", "provenance", " (spooled, unverified)") : null));
    return card;
  }
  function renderOverview(data, attention, route) {
    const page = el("div");
    const ingest = focusKey(button("Ingest commits", async () => {
      ingest.disabled = true;
      try {
        await jsonFetch("/api/v1/ingest", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ api: apiVersion }) });
        notice("Repository ingestion completed.", "success"); scheduleRefresh();
      } catch (error) { notice(errorMessage(error), "error"); }
      finally { ingest.disabled = false; }
    }, "compact"), "overview:ingest");
    add(page, title("Board", "Shared plans and the work happening now.", ingest));
    const plans = collection(data, "plans");
    const claims = (data.working || plans.flatMap(item => item.claims || [])).filter(claim => !claim.ended_at);
    if (claims.length) add(page, panel("Working now", add(el("div", "working-strip"), claims.map(workingCard))));
    const layout = el("div", "overview-layout");
    const lanes = el("div");
    for (const item of plans) {
      const plan = item.plan || item;
      const lane = el("section", "swimlane");
      const heading = add(el("div", "lane-heading"), refLink(plan.id), link(plan.title, "plan", { ref: plan.id }),
        el("span", "badge", `Revision ${plan.head_revision}`), el("span", "count", `${(item.tasks || []).length} tasks`));
      const grid = el("div", "lane-columns");
      for (const column of columns) {
        const tasks = (item.tasks || []).filter(task => task.column === column);
        const cell = add(el("div", "lane-column"), el("div", "column-name", `${column} · ${tasks.length}`));
        add(cell, tasks.map(task => taskCard(task, item.claims, { claimsOmitted: item.claims_omitted, plan: plan.id, through: data.through })));
        if (!tasks.length) cell.append(el("div", "small muted", "—"));
        grid.append(cell);
      }
      add(lane, heading, grid, omitted(item.tasks_omitted, "tasks"), item.tasks_omitted ? link("Browse all tasks", "plan", { ref: plan.id, tab: "tasks" }, "small") : null,
        omitted(item.claims_omitted, "claims"), item.claims_omitted ? link("Browse all claims", "claims", { plan: plan.id }, "small") : null); lanes.append(lane);
    }
    if (!plans.length) lanes.append(panel("Plans", add(el("div"), empty("No plans yet."), link("Create the first plan", "new", {}, "button primary"))));
    lanes.append(omitted(data.omitted, "plans") || el("span"));
    if (data.next_after) lanes.append(link("Next plans", "overview", pageParams(route, data), "button"));
    if (route.after) lanes.append(link("Latest board", "overview", {}, "button"));
    const rail = el("aside", "needs-rail");
    const items = attentionItems(attention);
    const needs = panel("Needs you", add(el("div"), items.slice(0, 8).map(attentionCard), items.length ? link("View all", "attention", {}, "small") : empty("Nothing waiting for you."),
      omitted(attention?.entries_omitted, "entries")));
    rail.append(needs);
    add(layout, lanes, rail); page.append(layout);
    page.append(panel("Latest activity", eventList(data.events || []), "ticker"));
    return page;
  }
  function renderAttention(data, route) {
    const page = add(el("div"), title("Needs you", "Proposals, questions, and feedback addressed to you."));
    const groups = [["proposals", "Proposals"], ["questions", "Questions"], ["feedback", "Feedback"]];
    for (const [key, label] of groups) {
      const records = collection(data, "entries").filter(record => entryRecord(record).kind === key.replace(/s$/, ""));
      page.append(panel(label, records.length ? add(el("div", "entry-list"), records.map(record => entryCard(record))) : empty(`No ${label.toLowerCase()} waiting.`)));
    }
    if (data.rebase_needed?.length) page.append(panel("Your proposals need a new base", add(el("div", "entry-refs"), data.rebase_needed.map(ref => refLink(ref)))));
    if (data.stale_claims?.length) page.append(panel("Stale claims", add(el("div", "working-strip"), data.stale_claims.map(item => workingCard(item.claim)))));
    add(page, omitted(data.entries_omitted, "entries"), omitted(data.claims_omitted, "stale claims"));
    if (data.next_after) page.append(link("Next attention entries", "attention", pageParams(route, data), "button"));
    if (data.claims_next_after) page.append(link("More stale claims", "claims", { own_stale: "1", after: JSON.stringify(data.claims_next_after), through: data.through }, "button"));
    return page;
  }
  function postForm(target, defaults = {}) {
    const form = el("form");
    const kind = field("Kind", "kind", defaults.kind || "note", { choices: postKinds });
    const body = field("Message", "body", defaults.body || "", { multiline: true, required: true, maxLength: 4096 });
    const to = field("To", "to", defaults.to || "", { hint: "optional user, harness, or full actor" });
    const supersedes = field("Supersedes", "supersedes", "", { hint: "optional E#" });
    if (defaults.question) {
      kind.input.disabled = true; to.input.readOnly = true;
      form.append(add(el("p", "small muted"), el("span", "", "Reply references "), refLink(defaults.question), el("span", "", " and is addressed to the asker.")));
    }
    const submit = focusKey(el("button", "primary", defaults.kind === "answer" ? "Send answer" : "Post entry"), `post:${target}:${defaults.question || ""}`); submit.type = "submit";
    add(form, kind.wrapper, body.wrapper, add(el("div", "filters"), to.wrapper, supersedes.wrapper), submit);
    form.addEventListener("submit", event => {
      event.preventDefault();
      const text = defaults.question ? `${defaults.question}\n\n${body.input.value}` : body.input.value;
      void submitMutation(form, { op: "post", target, kind: kind.input.value, body: text, to: to.input.value || null, supersedes: supersedes.input.value || null }, (_, current) => {
        body.input.value = defaults.body || ""; if (current) { notice("Entry posted.", "success"); scheduleRefresh(); }
      });
    });
    return retainForm(form, `post:${target}:${defaults.question || ""}`);
  }
  function taskDetails(data, headings = []) {
    const tasks = data.tasks || [], claims = data.claims || [];
    const content = el("div");
    for (const task of tasks) {
      const claim = claims.find(item => item.task === task.id && !item.ended_at);
      const item = el("article", "task-detail");
      item.id = task.id;
      add(item, add(el("div", "row wrap"), refLink(task.id), badge(task.column), el("strong", "", task.title)));
      if (task.section) {
        const heading = headings.find(value => value.title === task.section);
        const section = el(heading ? "a" : "span", "small", task.section);
        if (heading) { section.href = routeUrl("plan", { ref: data.plan.id, heading: heading.anchor }); section.dataset.nav = "true"; }
        add(item, add(el("p"), el("span", "muted small", "Section: "), section));
      }
      if (claim) add(item, el("p", "small", actorName(claim.actor)), claim.model || claim.effort ? el("p", "small", [claim.model, claim.effort].filter(Boolean).join(" · ")) : null, el("p", "small", claim.scope),
        add(el("p", "small muted"), el("span", "", "Last active "), ageNode(claim.last_active), claim.stale ? badge("stale") : null));
      else if (task.assignee) item.append(el("p", "small muted", `Assigned to ${task.assignee}`));
      if (!claim && (data.claims_omitted || data.claims_partial)) item.append(link("Claim details omitted", "claims", { plan: data.plan.id, through: data.claims_through || data.through }, "small"));
      const controls = el("form", "actions");
      for (const column of columns.filter(column => column !== task.column)) {
        const move = focusKey(button(`Move to ${column}`, () => {
          void submitMutation(controls, { op: "task_move", task: task.id, column, to: null });
        }, "compact"), `move:${task.id}:${column}`);
        move.append(el("span", "sr-only", ` ${task.id}`));
        controls.append(move);
      }
      item.append(retainForm(controls, `move:${task.id}`)); content.append(item);
    }
    if (!tasks.length) content.append(empty("No tasks in this plan."));
    content.append(omitted(data.tasks_omitted, "tasks") || el("span"));
    content.append(omitted(data.claims_omitted, "claims") || el("span"));
    if (data.sections_without_tasks?.length) {
      const sections = el("div", "entry-refs");
      for (const text of data.sections_without_tasks) {
        const heading = headings.find(item => item.title === text);
        const section = el(heading ? "a" : "span", "small", text);
        if (heading) { section.href = routeUrl("plan", { ref: data.plan.id, heading: heading.anchor }); section.dataset.nav = "true"; }
        sections.append(section);
      }
      content.append(panel("Sections without tasks", sections));
    }
    const create = el("form");
    const name = field("New task", "task-title", "", { required: true, maxLength: 256 });
    const section = field("SSOT section", "section", "", { choices: [["", "No section"], ...headings.map(heading => [heading.title, heading.title])] });
    const submit = focusKey(el("button", "primary", "Create task"), `create-task:${data.plan.id}`); submit.type = "submit";
    add(create, name.wrapper, section.wrapper, submit);
    create.addEventListener("submit", event => { event.preventDefault(); void submitMutation(create, { op: "task_create", plan: data.plan.id, title: name.input.value, to: null, section: section.input.value || null }, (_, current) => { name.input.value = ""; if (current) scheduleRefresh(); }); });
    add(content, el("hr"), retainForm(create, `new-task:${data.plan.id}`));
    return content;
  }

  function renderClaims(data, route) {
    const page = add(el("div"), title(route.own_stale === "1" ? "Your stale claims" : "Claims", route.plan ? `Plan ${route.plan}` : "Working agents across the board."),
      add(el("div", "working-strip"), data.claims.map(item => workingCard(item.claim))), omitted(data.omitted, "claims"));
    if (data.next_after) page.append(link("Next claims", "claims", pageParams(route, data), "button"));
    return page;
  }
  const details = createBoardDetails({ ...context, entryCard, taskCard, workingCard, postForm, taskDetails });
  return { entryCard, eventList, workingCard, taskCard, attentionItems, attentionCard, renderOverview, renderAttention, renderClaims, postForm, taskDetails, ...details };
}
