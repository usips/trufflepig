import { createBoardDetails } from "./pages/board_pages.js";
import { createBoardCards } from "./board_cards.js";
import { createDonePage } from "./pages/done_page.js";
import { actorMark, recipientMark } from "./marks/agent_marks.js";
import { INGEST_TIMEOUT_MS, INGEST_UNKNOWN_MESSAGE, completeIngest, resolveIngestEvent } from "./board_ingest.js";

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
    entryCard, eventList, workingCard, taskCard, attentionItems, attentionCard,
  } = createBoardCards({ state, dom, entryRecord, collection });
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
    const page = el("div");
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
    const plans = collection(data, "plans");
    const claims = (data.working || plans.flatMap(item => item.claims || [])).filter(claim => !claim.ended_at);
    if (claims.length) add(page, panel("Working now", add(el("div", "working-strip"), claims.map(workingCard))));
    const layout = el("div", "overview-layout");
    const lanes = el("div");
    for (const item of plans) {
      const plan = item.plan || item;
      const lane = el("section", "swimlane");
      focusKey(lane, `swimlane:${plan.id}`);
      const heading = add(el("div", "lane-heading"), refLink(plan.id),
        link(plan.title, "plan", { ref: plan.id }),
        el("span", "badge", `Revision ${plan.head_revision}`),
        projectChips(item.repo_keys, projects),
        el("span", "count", `${(item.tasks || []).length} tasks`));
      const grid = el("div", "lane-columns");
      for (const column of columns.filter(column => column !== "done")) {
        const tasks = (item.tasks || []).filter(task => task.column === column);
        const cell = add(el("div", "lane-column"), el("div", "column-name", `${column} · ${tasks.length}`));
        add(cell, tasks.map(task => taskCard(task, item.claims,
          { claimsOmitted: item.claims_omitted, plan: plan.id, through: data.through })));
        if (!tasks.length) cell.append(el("div", "small muted", "—"));
        grid.append(cell);
      }
      const recentDone = item.recent_done || [];
      const done = add(el("details", "lane-column lane-done"),
        el("summary", "column-name", `Done (${item.done_count || 0})`));
      add(done, recentDone.map(task => taskCard(task, [])));
      const moreDone = (item.done_count || 0) - recentDone.length;
      if (moreDone > 0) done.append(link(`${moreDone} more →`, "plan", { ref: plan.id, tab: "done" }, "small"));
      if (!recentDone.length && !moreDone) done.append(el("div", "small muted", "—"));
      grid.append(done);
      add(lane, heading, grid, omitted(item.tasks_omitted, "tasks"),
        item.tasks_omitted ? link("Browse all tasks", "plan", { ref: plan.id, tab: "tasks" }, "small") : null,
        omitted(item.claims_omitted, "claims"),
        item.claims_omitted ? link("Browse all claims", "claims", { plan: plan.id }, "small") : null);
      lanes.append(lane);
    }
    if (!plans.length) {
      const message = route.project ? "No plans in this scope." : "No plans yet.";
      lanes.append(panel("Plans", add(el("div"), empty(message),
        link("Create the first plan", "new", {}, "button primary"))));
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
    const page = add(el("div"), title("Needs you", "Proposals, questions, and feedback addressed to you."));
    const groups = [["proposals", "Proposals"], ["questions", "Questions"], ["feedback", "Feedback"]];
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
        add(el("div", "working-strip"), data.stale_claims.map(item => workingCard(item.claim)))));
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
    const body = field("Message", "body", defaults.body || "", { multiline: true, required: true, maxLength: 4096 });
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
    const tasks = data.tasks || [], claims = data.claims || [];
    const content = el("div");
    for (const task of tasks) {
      const claim = claims.find(item => item.task === task.id && !item.ended_at);
      const item = el("article", "task-detail");
      item.id = task.id;
      focusKey(item, `task-detail:${task.id}`);
      add(item, add(el("div", "row wrap"), refLink(task.id), badge(task.column), el("strong", "", task.title)));
      if (task.section) {
        const heading = headings.find(value => value.title === task.section);
        const section = el(heading ? "a" : "span", "small", task.section);
        if (heading) {
          section.href = routeUrl("plan", { ref: data.plan.id, heading: heading.anchor });
          section.dataset.nav = "true";
        }
        add(item, add(el("p"), el("span", "muted small", "Section: "), section));
      }
      if (claim) add(item,
        add(el("p", "small"), actorMark(claim), el("span", "", actorName(claim.actor))),
        claim.model || claim.effort ? el("p", "small", [claim.model, claim.effort].filter(Boolean).join(" · ")) : null,
        el("p", "small", claim.scope),
        add(el("p", "small muted"), el("span", "", "Last active "), ageNode(claim.last_active),
          claim.stale ? badge("stale") : null));
      else if (task.assignee) item.append(add(el("p", "small muted"), recipientMark(task.assignee),
        el("span", "", `Assigned to ${task.assignee}`)));
      if (!claim && (data.claims_omitted || data.claims_partial)) {
        item.append(link("Claim details omitted", "claims",
          { plan: data.plan.id, through: data.claims_through || data.through }, "small"));
      }
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
        if (heading) {
          section.href = routeUrl("plan", { ref: data.plan.id, heading: heading.anchor });
          section.dataset.nav = "true";
        }
        sections.append(section);
      }
      content.append(panel("Sections without tasks", sections));
    }
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
      add(el("div", "working-strip"), data.claims.map(item => workingCard(item.claim))),
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
