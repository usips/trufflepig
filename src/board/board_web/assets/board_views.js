import { createBoardDetails } from "./pages/board_pages.js";
import { createBoardTaskView } from "./board_task_view.js";
import { createBoardCards } from "./board_cards.js";
import { createDonePage } from "./pages/done_page.js";
import { actorAttribution, recipientMark } from "./marks/agent_marks.js";
import { INGEST_TIMEOUT_MS, INGEST_UNKNOWN_MESSAGE, completeIngest, resolveIngestEvent } from "./board_ingest.js";

function taskColumnLabel(column) {
  return column === "transferred" ? "Transferred" : column;
}

export function createBoardViews(context) {
  const {
    state, dom, board, jsonFetch, navigate, scheduleRefresh, submitMutation, notice, apiVersion,
    entryRecord, collection, permitted, nextAfter, pageParams, queryFilters, postKinds, entryKinds,
    columns, errorMessage,
  } = context;
  const {
    el, add, button, routeUrl, link, refLink, badge, actorName, shortActor, stamp, timeNode, age,
    ageNode, panel, empty, omitted, title, field, formStatus, planId, focusKey, retainForm,
  } = dom;
  const {
    entryCard, eventList, workingCard, taskCard, workerTree, claimSignals,
    attentionItems, attentionCard, doneFooter,
  } = createBoardCards({ state, dom, entryRecord, collection });
  const { taskViewDetails } = createBoardTaskView({ dom, entryCard, workerTree, claimSignals });
  function projectChips(repoKeys, projects = state.projects || []) {
    if (!Array.isArray(repoKeys)) return null;
    const chips = el("div", "row wrap small");
    if (!repoKeys.length) {
      chips.append(link("Unscoped", "overview", { project: "unscoped" }, "badge"));
    }
    for (const project of projects) {
      if (!project.repo_keys.some(key => repoKeys.includes(key))) continue;
      const unavailable = project.unavailable || [];
      const chip = link(project.name, "overview", { project: project.id }, "badge project-chip");
      chip.dataset.project = project.id;
      if (unavailable.length) {
        chip.dataset.kind = "stale"; chip.textContent += " · unavailable";
        chip.title = `Unavailable members: ${unavailable.map(member => member.name).join(", ")}`;
      }
      chips.append(chip);
    }
    return chips.childElementCount ? chips : null;
  }
  function renderOverview(data, attention, route, projects) {
    const page = el("div", "overview-page");
    const showInactive = route.showInactive === "1";
    // The 202 is only a queue receipt; the terminal result arrives as an
    // `ingest` stream frame carrying the 202's ticket (see board_web_main.js
    // onIngest), so the control stays busy until its own frame or a timeout.
    // A frame that published before the 202 completes from the receipt cache.
    const ingest = focusKey(button("Ingest commits", async () => {
      ingest.disabled = true; ingest.setAttribute("aria-busy", "true");
      try {
        const reply = await jsonFetch("/api/v1/ingest", {
          method: "POST", headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ api: apiVersion }),
        });
        if (reply?.ingest !== "queued" || typeof reply?.ticket !== "string" || !reply.ticket) {
          throw new Error("Board returned an incompatible ingest reply.");
        }
        if (state.ingestTimer !== null) { clearTimeout(state.ingestTimer); state.ingestTimer = null; }
        state.ingestPending = true;
        state.ingestTicket = reply.ticket;
        // The receipt may have published before this 202 arrived; an
        // early receipt completes the control without arming the timer.
        const early = state.ingestReceipts.take(reply.ticket);
        if (early !== undefined) {
          completeIngest(state, early, { notice, scheduleRefresh });
        } else {
          state.ingestTimer = setTimeout(() => {
            state.ingestTimer = null;
            if (resolveIngestEvent(state.ingestTicket, { type: "timeout" }).outcome !== "unknown") return;
            state.ingestTicket = null;
            state.ingestPending = false;
            notice(INGEST_UNKNOWN_MESSAGE, "error");
            scheduleRefresh();
          }, INGEST_TIMEOUT_MS);
          notice("Repository ingestion queued; the result arrives over the live stream.");
        }
      } catch (error) {
        ingest.disabled = false; ingest.removeAttribute("aria-busy"); notice(errorMessage(error), "error");
      }
    }, "compact"), "overview:ingest");
    if (state.ingestPending) { ingest.disabled = true; ingest.setAttribute("aria-busy", "true"); }
    add(page, title("Board", "Shared plans and the work happening now.", ingest));
    const filter = el("details", "overview-filter");
    filter.open = state.overviewFiltersOpen === true;
    filter.addEventListener("toggle", () => { state.overviewFiltersOpen = filter.open; });
    const filterSummary = el("summary", "", "Filters");
    filterSummary.dataset.focusKey = "overview:filters";
    filter.append(filterSummary);
    const filterChoice = el("label", "overview-filter-choice");
    const filterCheckbox = el("input");
    filterCheckbox.type = "checkbox";
    filterCheckbox.checked = showInactive;
    focusKey(filterCheckbox, "overview:show-inactive");
    filterCheckbox.addEventListener("change", event => {
      navigate("overview", {
        ...route, after: "", through: "", showInactive: event.target.checked ? "1" : "0",
      }, false, false);
    });
    filterChoice.append(filterCheckbox, el("span", "", "Show plans without active tasks"));
    filter.append(filterChoice);
    const filterStatus = el("p", "overview-filter-status",
      showInactive ? "Showing all plans, sorted by most recently updated."
        : "Showing plans with active tasks, sorted by most recently updated.");
    filterStatus.setAttribute("role", "status");
    filterStatus.setAttribute("aria-live", "polite");
    add(page, add(el("div", "overview-filter-tools"), filter, filterStatus));
    const plans = collection(data, "plans");
    const claims = (data.working || plans.flatMap(item => item.claims || [])).filter(claim => !claim.ended_at);
    const tasksById = new Map(plans.flatMap(item => (item.tasks || []).map(task => [task.id, task])));
    if (claims.length) add(page, panel("Working now", add(el("div", "working-strip"),
      claims.map(claim => workingCard(claim, tasksById.get(claim.task), {
        now: data.server_now, ttlSecs: data.claim_ttl_secs, entries: data.entries,
      }))), "working-panel"));
    const layout = el("div", "overview-layout");
    const lanes = el("div");
    for (const item of plans) {
      const plan = item.plan || item;
      const activeTasks = (item.tasks || []).filter(task => task.column !== "done");
      const activeCount = activeTasks.length + (item.tasks_omitted || 0);
      const lane = el("section", "swimlane");
      focusKey(lane, `swimlane:${plan.id}`);
      const heading = add(el("div", "lane-heading"), refLink(plan.id),
        link(plan.title, "plan", { ref: plan.id }),
        el("span", "badge", `Revision ${plan.head_revision}`),
        projectChips(item.repo_keys, projects),
        el("span", "count", `${activeCount} active · ${item.done_count || 0} done`));
      const grid = el("div", "lane-columns");
      for (const column of columns.filter(column => column !== "done")) {
        const tasks = activeTasks.filter(task => task.column === column);
        const label = taskColumnLabel(column);
        const cell = add(el("div", "lane-column"), el("div", "column-name", `${label} · ${tasks.length}`));
        add(cell, tasks.map(task => taskCard(task, item.claims,
          { claimsOmitted: item.claims_omitted, plan: plan.id, through: data.through,
            now: data.server_now, ttlSecs: data.claim_ttl_secs, entries: data.entries })));
        if (!tasks.length) cell.append(el("div", "small muted", "—"));
        grid.append(cell);
      }
      add(lane, heading, activeCount ? grid : el("p", "lane-empty small muted", "No active tasks."),
        doneFooter(plan, item), omitted(item.tasks_omitted, "tasks"),
        item.tasks_omitted ? link("Browse all tasks", "plan", { ref: plan.id, tab: "tasks" }, "small") : null,
        omitted(item.claims_omitted, "claims"),
        item.claims_omitted ? link("Browse all claims", "claims", { plan: plan.id }, "small") : null);
      lanes.append(lane);
    }
    if (!plans.length) {
      const message = showInactive
        ? (route.project ? "No plans in this scope." : "No plans yet.")
        : (route.project ? "No plans in this scope have active tasks." : "No plans with active tasks.");
      lanes.append(panel(showInactive ? "Plans" : "Active plans", add(el("div"), empty(message),
        showInactive ? link("Create the first plan", "new", {}, "button primary")
          : el("p", "small muted", "Use Filters to show plans without active tasks."))));
    }
    lanes.append(omitted(data.omitted, "plans") || el("span"));
    if (data.next_after) lanes.append(link("Next plans", "overview", pageParams(route, data), "button"));
    if (route.after) lanes.append(link("Latest board", "overview", {}, "button"));
    const rail = el("aside", "needs-rail");
    const items = attentionItems(attention);
    const needs = panel("Needs you", add(el("div"), items.slice(0, 8).map(attentionCard),
      items.length ? link("View all", "attention", {}, "small") : empty("Nothing waiting for you."),
      omitted(attention?.entries_omitted, "entries")));
    rail.append(needs);
    add(layout, lanes, rail); page.append(layout);
    page.append(panel("Latest activity", eventList(data.events || []), "ticker"));
    return page;
  }
  function renderAttention(data, route) {
    const page = add(el("div"), title("Needs you", "Addressed questions, proposals needing decisions or new bases, and stale claims."));
    const groups = [["proposals", "Proposals"], ["questions", "Questions"]];
    for (const [key, label] of groups) {
      const records = collection(data, "entries").filter(record => entryRecord(record).kind === key.replace(/s$/, ""));
      page.append(panel(label, records.length
        ? add(el("div", "entry-list"), records.map(record => entryCard(record)))
        : empty(`No ${label.toLowerCase()} waiting.`)));
    }
    if (data.rebase_needed?.length) {
      page.append(panel("Your proposals need a new base",
        add(el("div", "entry-refs"), data.rebase_needed.map(ref => refLink(ref)))));
    }
    if (data.stale_claims?.length) {
      page.append(panel("Stale claims",
        add(el("div", "working-strip"), data.stale_claims.map(item => workingCard(item.claim, null, {
          now: data.server_now, ttlSecs: data.claim_ttl_secs, entries: data.entries,
        })))));
    }
    add(page, omitted(data.entries_omitted, "entries"), omitted(data.claims_omitted, "stale claims"));
    if (data.next_after) page.append(link("Next attention entries", "attention", pageParams(route, data), "button"));
    if (data.claims_next_after) {
      page.append(link("More stale claims", "claims",
        { own_stale: "1", after: JSON.stringify(data.claims_next_after), through: data.through }, "button"));
    }
    return page;
  }
  function postForm(target, defaults = {}) {
    const form = el("form");
    const kind = field("Kind", "kind", defaults.kind || "note", { choices: postKinds });
    const body = field("Message", "body", defaults.body || "", { multiline: true, required: true, maxLength: 32768 });
    const to = field("To", "to", defaults.to || "", { hint: "optional user, harness, or full actor" });
    const supersedes = field("Supersedes", "supersedes", "", { hint: "optional E#" });
    if (defaults.question) {
      kind.input.disabled = true; to.input.readOnly = true;
      form.append(add(el("p", "small muted"), el("span", "", "Reply references "),
        refLink(defaults.question), el("span", "", " and is addressed to the asker.")));
    }
    const submit = focusKey(
      el("button", "primary", defaults.kind === "answer" ? "Send answer" : "Post entry"),
      `post:${target}:${defaults.question || ""}`);
    submit.type = "submit";
    add(form, kind.wrapper, body.wrapper, add(el("div", "filters"), to.wrapper, supersedes.wrapper), submit);
    form.addEventListener("submit", event => {
      event.preventDefault();
      const text = defaults.question ? `${defaults.question}\n\n${body.input.value}` : body.input.value;
      const op = {
        op: "post", target, kind: kind.input.value, body: text,
        to: to.input.value || null, supersedes: supersedes.input.value || null,
      };
      void submitMutation(form, op, (_, current) => {
        body.input.value = defaults.body || ""; if (current) { notice("Entry posted.", "success"); scheduleRefresh(); }
      });
    });
    return retainForm(form, `post:${target}:${defaults.question || ""}`);
  }
  function taskDetails(data, headings = []) {
    const tasks = [...(data.tasks || [])], claims = data.claims || [];
    const tasksWritable = data.plan?.lifecycle_state !== "closed";
    const selectedView = data.task_view;
    if (selectedView?.task && !tasks.some(task => task.id === selectedView.task.id)) {
      tasks.unshift(selectedView.task);
    }
    const content = el("div");
    for (const task of tasks) {
      const taskView = selectedView?.task?.id === task.id ? selectedView : null;
      const claim = taskView ? taskView.current_claim
        : claims.find(item => item.task === task.id && !item.ended_at);
      const item = el("article", "task-detail");
      item.id = task.id;
      focusKey(item, `task-detail:${task.id}`);
      add(item, add(el("div", "row wrap"), refLink(task.id), badge(taskColumnLabel(task.column)), el("strong", "", task.title)));
      if (task.section) {
        const heading = headings.find(value => value.title === task.section);
        const section = el(heading ? "a" : "span", "small", task.section);
        if (heading) {
          section.href = routeUrl("plan", { ref: data.plan.id, heading: heading.anchor });
          section.dataset.nav = "true";
        }
        add(item, add(el("p"), el("span", "muted small", "Section: "), section));
      }
      if (claim && !taskView) add(item,
        add(el("p", "small"), actorAttribution({ ...claim,
          origin: data.entries?.find(entry => entry?.id === claim.entry)?.origin,
          authority: data.entries?.find(entry => entry?.id === claim.entry)?.authority,
        })),
        claim.model || claim.effort ? el("p", "small", [claim.model, claim.effort].filter(Boolean).join(" · ")) : null,
        el("p", "small", claim.scope),
        add(el("p", "small muted"), el("span", "", "Last active "), ageNode(claim.last_active),
          claim.stale ? badge("stale") : null));
      if (claim && !taskView) {
        const signals = claimSignals(claim, { now: data.server_now, ttlSecs: data.claim_ttl_secs });
        if (signals) item.append(signals);
      }
      else if (!taskView && task.assignee) item.append(add(el("p", "small muted"), recipientMark(task.assignee),
        el("span", "", `Assigned to ${task.assignee}`)));
      if (!taskView && !claim && (data.claims_omitted || data.claims_partial)) {
        item.append(link("Claim details omitted", "claims",
          { plan: data.plan.id, through: data.claims_through || data.through }, "small"));
      }
      if (taskView) item.append(taskViewDetails(taskView));
      const controls = el("form", "actions");
      if (tasksWritable && task.column !== "transferred" && !task.transferred_to) {
        if (claim?.stale) {
          const release = focusKey(button("Release stale claim", () => {
            void submitMutation(controls, { op: "task_move", task: task.id, column: "todo", to: null });
          }, "compact"), `move:${task.id}:release-stale`);
          release.append(el("span", "sr-only", ` ${task.id}`));
          controls.append(release);
        }
        for (const column of columns.filter(column => column !== task.column && column !== "transferred")) {
          if (claim?.stale && column === "todo") continue;
          const move = focusKey(button(`Move to ${taskColumnLabel(column)}`, () => {
            void submitMutation(controls, { op: "task_move", task: task.id, column, to: null });
          }, "compact"), `move:${task.id}:${column}`);
          move.append(el("span", "sr-only", ` ${task.id}`));
          controls.append(move);
        }
        item.append(retainForm(controls, `move:${task.id}`));
      }
      content.append(item);
    }
    if (!tasks.length) content.append(empty("No tasks in this plan."));
    content.append(omitted(data.tasks_omitted, "tasks") || el("span"));
    content.append(omitted(data.claims_omitted, "claims") || el("span"));
    if (data.sections_without_tasks?.length) {
      const sections = el("div", "entry-refs");
      for (const text of data.sections_without_tasks) {
        const heading = headings.find(item => item.title === text);
        const section = el(heading ? "a" : "span", "small", text);
        if (heading) {
          section.href = routeUrl("plan", { ref: data.plan.id, heading: heading.anchor });
          section.dataset.nav = "true";
        }
        sections.append(section);
      }
      content.append(panel("Sections without tasks", sections));
    }
    if (!tasksWritable) return content;
    const create = el("form");
    const name = field("New task", "task-title", "", { required: true, maxLength: 256 });
    const section = field("SSOT section", "section", "",
      { choices: [["", "No section"], ...headings.map(heading => [heading.title, heading.title])] });
    const submit = focusKey(el("button", "primary", "Create task"), `create-task:${data.plan.id}`);
    submit.type = "submit";
    add(create, name.wrapper, section.wrapper, submit);
    create.addEventListener("submit", event => {
      event.preventDefault();
      const op = {
        op: "task_create", plan: data.plan.id, title: name.input.value, to: null,
        section: section.input.value || null,
      };
      void submitMutation(create, op, (_, current) => {
        name.input.value = ""; if (current) scheduleRefresh();
      });
    });
    add(content, el("hr"), retainForm(create, `new-task:${data.plan.id}`));
    return content;
  }

  function renderClaims(data, route) {
    const page = add(el("div"),
      title(route.own_stale === "1" ? "Your stale claims" : "Claims",
        route.plan ? `Plan ${route.plan}` : "Working agents across the board."),
      add(el("div", "working-strip"), data.claims.map(item => workingCard(item.claim, null, {
        now: data.server_now, ttlSecs: data.claim_ttl_secs, entries: data.entries,
      }))),
      omitted(data.omitted, "claims"));
    if (data.next_after) page.append(link("Next claims", "claims", pageParams(route, data), "button"));
    return page;
  }
  const done = createDonePage({ state, dom, taskCard });
  const details = createBoardDetails({
    ...context, entryCard, taskCard, workingCard, postForm, taskDetails, projectChips, ...done,
  });
  return {
    entryCard, eventList, workingCard, taskCard, attentionItems, attentionCard, renderOverview,
    renderAttention, renderClaims, postForm, taskDetails, projectChips, ...done, ...details,
  };
}
