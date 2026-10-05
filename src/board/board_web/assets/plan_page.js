import { createBoardEntries } from "./board_entries.js";
import { decodeBoardFragment } from "./board_dom.js";

export function createPlanPage(context) {
  const {
    state, dom, board, jsonFetch, navigate, scheduleRefresh, submitMutation, notice, apiVersion,
    entryRecord, collection, permitted, nextAfter, pageParams, queryFilters, postKinds, entryKinds,
    entryCard, taskCard, workingCard, postForm, taskDetails,
  } = context;
  const {
    el, add, button, routeUrl, link, refLink, badge, actorName, shortActor, stamp, timeNode, age,
    ageNode, panel, empty, omitted, title, field, formStatus, planId, focusKey, retainForm,
  } = dom;
  const { entriesPage } = createBoardEntries(context);
  function planTabs(ref, selected) {
    const tabs = el("nav", "tabs"); tabs.setAttribute("aria-label", "Plan sections");
    for (const [key, label] of [
      ["ssot", "Plan"], ["tasks", "Tasks"], ["entries", "Entries"],
      ["history", "History"], ["commits", "Commits"], ["review", "Review"],
    ]) tabs.append(link(label, "plan", { ref: planId(ref), tab: key }, selected === key ? "active" : ""));
    return tabs;
  }
  function sanitizedMarkup(rendered) {
    const item = el("div", "markup");
    // Only the server's sanitized render endpoint supplies this HTML.
    item.innerHTML = rendered.html;
    for (const anchor of item.querySelectorAll('a[href^="#"]')) {
      const heading = decodeBoardFragment(anchor.getAttribute("href").slice(1));
      anchor.href = routeUrl(state.route.view, { ...state.route, heading }); anchor.dataset.nav = "true";
    }
    return item;
  }
  function annotateHeadings(markup, rendered, view) {
    const anchors = [...markup.querySelectorAll("[id]")];
    for (const heading of rendered.headings) {
      const element = anchors.find(item => item.id === heading.anchor);
      if (!element) continue;
      const tasks = view.tasks.filter(task => task.section === heading.title);
      const markers = el("span", "heading-markers");
      for (const task of tasks) {
        markers.append(refLink(task.id, task.id, "badge heading-task"));
        const claim = view.claims.find(item => item.task === task.id && !item.ended_at);
        if (claim) {
          const marker = badge(claim.stale ? "stale" : "claimed");
          marker.textContent = [actorName(claim.actor), claim.model, claim.effort, claim.stale ? "stale" : null]
            .filter(Boolean).join(" · ");
          markers.append(marker);
        } else {
          markers.append(badge(task.column));
          if (view.claims_omitted) {
            markers.append(link("Claim details omitted", "claims", {
              plan: view.plan.id,
              after: view.claims_next_after ? JSON.stringify(view.claims_next_after) : null,
              through: view.through,
            }, "badge"));
          }
        }
      }
      if (!tasks.length && view.sections_without_tasks.includes(heading.title)) markers.append(badge("No task"));
      else if (!tasks.length && view.tasks_omitted) {
        markers.append(link("Task details omitted", "plan", {
          ref: view.plan.id, tab: "tasks", taskAfter: view.tasks_next_after,
          taskThrough: view.through, taskCeiling: JSON.stringify(view.task_ceiling),
        }, "badge"));
      }
      if (markers.childElementCount) element.append(markers);
    }
  }
  function renderPlan(view, rendered, extra, route) {
    const plan = view.plan;
    const page = el("div");
    const actions = el("div", "actions");
    if (permitted(view, "can_edit")) {
      actions.append(link("Edit plan", "edit", { ref: view.revision.id }, "button compact"));
    }
    add(
      page,
      title(
        `${plan.id} · ${plan.title}`,
        `Revision ${plan.head_revision} · Owner ${plan.owner_user}${plan.steward ? ` · Steward ${plan.steward}` : ""}`,
        actions
      ),
      planTabs(plan.id, route.tab || "ssot")
    );
    const tab = route.tab || "ssot";
    if (tab === "tasks") {
      const tasks = extra.tasks, claims = extra.claims;
      const details = taskDetails({
        ...view, tasks: tasks.tasks, claims: claims.claims.map(item => item.claim),
        tasks_omitted: tasks.omitted, claims_omitted: claims.omitted,
        claims_partial: Boolean(claims.after), claims_through: claims.through,
      }, rendered.headings);
      if (tasks.next_after) {
        details.append(link("Next tasks", "plan", {
          ...route, taskAfter: tasks.next_after, taskThrough: tasks.through,
          taskCeiling: JSON.stringify(tasks.ceiling),
        }, "button"));
      }
      page.append(panel("Tasks", details));
      const claimList = add(el("div", "working-strip"), claims.claims.map(item => workingCard(item.claim)));
      if (claims.next_after) {
        claimList.append(link("Next claims", "plan",
          { ...route, claimAfter: JSON.stringify(claims.next_after), claimThrough: claims.through }, "button"));
      }
      return add(page, panel("Claims", claimList));
    }
    if (tab === "entries") {
      const statusCards = view.tasks.slice(0, 8).map(task => taskCard(task, view.claims, {
        claimsOmitted: view.claims_omitted, claimAfter: view.claims_next_after, through: view.through,
      }));
      const statusPanel = panel("Task status", add(el("div", "task-status-strip"), statusCards,
        link("Manage tasks", "plan", { ref: plan.id, tab: "tasks" }, "small")));
      return add(page, statusPanel, entriesPage(extra, { ...route, plan: plan.id }, false),
        panel("Post an entry", postForm(plan.id)));
    }
    if (tab === "history") return add(page, historyPage(extra, plan));
    if (tab === "commits") {
      const findCommit = route.oid && !view.commits.some(commit => commit.oid === route.oid)
        ? link("Find this commit entry", "search", { q: route.oid, plan: plan.id }, "button") : null;
      return add(page, commitsPage(view.commits, view.commits_omitted, plan.id),
        link("Browse all commit entries", "plan", { ref: plan.id, tab: "entries", kind: "commit" }, "button"),
        findCommit);
    }
    if (tab === "review") {
      const hint = add(el("p", "muted small"), el("span", "", "Assemble the stored evidence for a revision with "),
        el("code", "", `trufflepig board review ${plan.id}@N`), ".");
      return add(page, panel("Review", add(el("div"),
        el("p", "muted", "Review packets stay a CLI surface; the web board carries plans, entries, and history."),
        hint)));
    }
    const layout = el("div", "plan-layout");
    const markup = sanitizedMarkup(rendered); annotateHeadings(markup, rendered, view);
    const body = panel("", markup);
    const aside = el("aside");
    const toc = el("ol", "toc");
    for (const heading of rendered?.headings || []) {
      const item = el("li"); item.dataset.level = heading.level;
      const anchor = link(heading.title, "plan", { ref: plan.id, heading: heading.anchor });
      item.append(anchor); toc.append(item);
    }
    if (toc.childElementCount) aside.append(panel("On this page", toc, "toc-panel"));
    const asideCards = (view.tasks || []).slice(0, 8).map(task => taskCard(task, view.claims, {
      claimsOmitted: view.claims_omitted, claimAfter: view.claims_next_after, through: view.through,
    }));
    const moreTasks = view.tasks_next_after ? link("More tasks", "plan", {
      ref: plan.id, tab: "tasks", taskAfter: view.tasks_next_after, taskThrough: view.through,
      taskCeiling: JSON.stringify(view.task_ceiling), claimThrough: view.through,
    }, "small") : null;
    const moreClaims = view.claims_next_after
      ? link("More claims", "claims",
        { plan: plan.id, after: JSON.stringify(view.claims_next_after), through: view.through }, "small")
      : null;
    aside.append(panel("Tasks", add(el("div"), asideCards,
      link("Manage tasks", "plan", { ref: plan.id, tab: "tasks", claimThrough: view.through }, "small"),
      moreTasks, moreClaims)));
    add(layout, body, aside); page.append(layout);
    const entries = view.entries || [];
    if (entries.length) {
      page.append(panel("Recent entries", add(el("div", "entry-list"), entries.map(record => entryCard(record)),
        omitted(view.entries_omitted, "entries"), link("All entries", "plan", { ref: plan.id, tab: "entries" }))));
    }
    return page;
  }
  function historyPage(data, plan) {
    const selector = el("form", "filters");
    const from = field("From revision", "from", "1", { type: "number", required: true });
    const to = field("To revision", "to", String(plan.head_revision), { type: "number", required: true });
    for (const input of [from.input, to.input]) {
      input.min = "1"; input.max = String(plan.head_revision); input.step = "1";
    }
    const compare = focusKey(el("button", "primary", "Compare revisions"), `compare:${plan.id}`);
    compare.type = "submit";
    add(selector, from.wrapper, to.wrapper, compare);
    selector.addEventListener("submit", event => {
      event.preventDefault();
      const start = from.input.value, end = to.input.value;
      if (!/^[1-9]\d*$/.test(start) || !/^[1-9]\d*$/.test(end) || BigInt(start) > BigInt(end)
        || BigInt(end) > BigInt(plan.head_revision)) {
        formStatus(selector, "Choose revisions from 1 through the current head, with the earlier revision first.");
        return;
      }
      navigate("plan", { ref: `${plan.id}@${start}..${end}` });
    });
    const list = el("ol", "history-list");
    const revisions = collection(data, "revisions");
    for (const revision of revisions) {
      const item = el("li");
      const details = add(el("div"), refLink(revision.id),
        add(el("p"), el("span", "", actorName(revision.actor)), el("span", "", ` · ${revision.source}`)),
        el("p", "", revision.summary));
      const number = Number(String(revision.id).split("@")[1]);
      add(item, details,
        number > 1 ? refLink(`${plan.id}@${number - 1}..${number}`, "View diff", "small") : null,
        timeNode(revision.created_at));
      list.append(item);
    }
    const page = panel("Revision history", add(el("div"), retainForm(selector, `diff-selector:${plan.id}`),
      list.childElementCount ? list : empty("No revisions.")));
    page.append(omitted(data?.omitted, "revisions") || el("span"));
    const cursor = nextAfter(data);
    if (cursor !== null) {
      page.append(link("Next revisions", "plan", pageParams({ ref: plan.id, tab: "history" }, data), "button compact"));
    }
    return page;
  }
  function commitsPage(commits, count, plan) {
    const list = el("ol", "commit-list");
    for (const commit of commits) {
      const item = el("li", "commit-row");
      item.id = `commit-${commit.oid}`;
      add(item,
        refLink(commit.oid, commit.oid, "mono", { plan: plan || commit.plans[0]?.plan_id, repo_key: commit.repo_key }),
        el("p", "", commit.subject),
        el("p", "muted small",
          `${commit.author} · ${stamp(commit.committed_at)} · ${commit.files} files · `
            + `+${commit.insertions} −${commit.deletions}`));
      if (commit.coauthors?.length) {
        item.append(el("p", "small", commit.coauthors.map(author => `${author.harness} (${author.model})`).join(", ")));
      }
      if (commit.plans?.length) {
        const links = commit.plans.map(value => refLink(
          value.task_ordinal ? `${value.plan_id}.${value.task_ordinal}` : value.plan_id));
        item.append(add(el("div", "entry-refs"), links));
      }
      list.append(item);
    }
    return panel("Linked commits", add(el("div"),
      list.childElementCount ? list : empty("No linked commits."), omitted(count, "commits")));
  }

  return { planTabs, sanitizedMarkup, renderPlan, historyPage, commitsPage };
}
