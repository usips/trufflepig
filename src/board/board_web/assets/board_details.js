import { createBoardEntries } from "/board_entries.js";
import { createBoardFeedback } from "/board_feedback.js";
import { decodeBoardFragment } from "/board_dom.js";

export function createBoardDetails(context) {
  const { state, dom, board, jsonFetch, navigate, scheduleRefresh, submitMutation, notice, apiVersion, entryRecord, collection, permitted, nextAfter, pageParams, queryFilters, postKinds, entryKinds, entryCard, taskCard, workingCard, postForm, taskDetails } = context;
  const { el, add, button, routeUrl, link, refLink, badge, actorName, shortActor, stamp, timeNode, age, ageNode, panel, empty, omitted, title, field, formStatus, planId, retainForm } = dom;
  const { feedbackControls, feedbackMetadata, feedbackPage } = createBoardFeedback(context);
  const { filtersForm, entriesPage } = createBoardEntries(context);
  function planTabs(ref, selected) {
    const tabs = el("nav", "tabs"); tabs.setAttribute("aria-label", "Plan sections");
    for (const [key, label] of [["ssot", "Plan"], ["tasks", "Tasks"], ["entries", "Entries"], ["history", "History"], ["commits", "Commits"], ["review", "Stored review"]]) tabs.append(link(label, "plan", { ref: planId(ref), tab: key }, selected === key ? "active" : ""));
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
          marker.textContent = [actorName(claim.actor), claim.model, claim.effort, claim.stale ? "stale" : null].filter(Boolean).join(" · ");
          markers.append(marker);
        } else {
          markers.append(badge(task.column));
          if (view.claims_omitted) markers.append(link("Claim details omitted", "claims", { plan: view.plan.id, after: view.claims_next_after ? JSON.stringify(view.claims_next_after) : null, through: view.through }, "badge"));
        }
      }
      if (!tasks.length && view.sections_without_tasks.includes(heading.title)) markers.append(badge("No task"));
      else if (!tasks.length && view.tasks_omitted) markers.append(link("Task details omitted", "plan", { ref: view.plan.id, tab: "tasks",
        taskAfter: view.tasks_next_after, taskThrough: view.through, taskCeiling: JSON.stringify(view.task_ceiling) }, "badge"));
      if (markers.childElementCount) element.append(markers);
    }
  }
  function renderPlan(view, rendered, extra, route) {
    const plan = view.plan;
    const page = el("div");
    const actions = el("div", "actions");
    if (permitted(view, "can_edit")) actions.append(link("Edit plan", "edit", { ref: view.revision.id }, "button compact"));
    add(page, title(`${plan.id} · ${plan.title}`, `Revision ${plan.head_revision} · Owner ${plan.owner_user}${plan.steward ? ` · Steward ${plan.steward}` : ""}`, actions), planTabs(plan.id, route.tab || "ssot"));
    const tab = route.tab || "ssot";
    if (tab === "tasks") {
      const tasks = extra.tasks, claims = extra.claims;
      const details = taskDetails({ ...view, tasks: tasks.tasks, claims: claims.claims.map(item => item.claim), tasks_omitted: tasks.omitted, claims_omitted: claims.omitted,
        claims_partial: Boolean(claims.after), claims_through: claims.through }, rendered.headings);
      if (tasks.next_after) details.append(link("Next tasks", "plan", { ...route, taskAfter: tasks.next_after, taskThrough: tasks.through, taskCeiling: JSON.stringify(tasks.ceiling) }, "button"));
      page.append(panel("Tasks", details));
      const claimList = add(el("div", "working-strip"), claims.claims.map(item => workingCard(item.claim)));
      if (claims.next_after) claimList.append(link("Next claims", "plan", { ...route, claimAfter: JSON.stringify(claims.next_after), claimThrough: claims.through }, "button"));
      return add(page, panel("Claims", claimList));
    }
    if (tab === "entries") return add(page, panel("Task status", add(el("div", "task-status-strip"), view.tasks.slice(0, 8).map(task => taskCard(task, view.claims, { claimsOmitted: view.claims_omitted, claimAfter: view.claims_next_after, through: view.through })),
      link("Manage tasks", "plan", { ref: plan.id, tab: "tasks" }, "small"))), entriesPage(extra, { ...route, plan: plan.id }, false), panel("Post an entry", postForm(plan.id)));
    if (tab === "history") return add(page, historyPage(extra, plan));
    if (tab === "commits") return add(page, commitsPage(view.commits, view.commits_omitted, plan.id), link("Browse all commit entries", "plan", { ref: plan.id, tab: "entries", kind: "commit" }, "button"),
      route.oid && !view.commits.some(commit => commit.oid === route.oid) ? link("Find this commit entry", "search", { q: route.oid, plan: plan.id }, "button") : null);
    if (tab === "review") return add(page, reviewPage(extra));
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
    aside.append(panel("Tasks", add(el("div"), (view.tasks || []).slice(0, 8).map(task => taskCard(task, view.claims, { claimsOmitted: view.claims_omitted, claimAfter: view.claims_next_after, through: view.through })), link("Manage tasks", "plan", { ref: plan.id, tab: "tasks", claimThrough: view.through }, "small"),
      view.tasks_next_after ? link("More tasks", "plan", { ref: plan.id, tab: "tasks", taskAfter: view.tasks_next_after, taskThrough: view.through, taskCeiling: JSON.stringify(view.task_ceiling), claimThrough: view.through }, "small") : null,
      view.claims_next_after ? link("More claims", "claims", { plan: plan.id, after: JSON.stringify(view.claims_next_after), through: view.through }, "small") : null)));
    add(layout, body, aside); page.append(layout);
    const entries = view.entries || [];
    if (entries.length) page.append(panel("Recent entries", add(el("div", "entry-list"), entries.map(record => entryCard(record)), omitted(view.entries_omitted, "entries"), link("All entries", "plan", { ref: plan.id, tab: "entries" }))));
    return page;
  }
  function historyPage(data, plan) {
    const selector = el("form", "filters");
    const from = field("From revision", "from", "1", { type: "number", required: true });
    const to = field("To revision", "to", String(plan.head_revision), { type: "number", required: true });
    for (const input of [from.input, to.input]) { input.min = "1"; input.max = String(plan.head_revision); input.step = "1"; }
    const compare = el("button", "primary", "Compare revisions"); compare.type = "submit";
    add(selector, from.wrapper, to.wrapper, compare);
    selector.addEventListener("submit", event => {
      event.preventDefault();
      const start = from.input.value, end = to.input.value;
      if (!/^[1-9]\d*$/.test(start) || !/^[1-9]\d*$/.test(end) || BigInt(start) > BigInt(end) || BigInt(end) > BigInt(plan.head_revision)) { formStatus(selector, "Choose revisions from 1 through the current head, with the earlier revision first."); return; }
      navigate("plan", { ref: `${plan.id}@${start}..${end}` });
    });
    const list = el("ol", "history-list");
    const revisions = collection(data, "revisions");
    for (const revision of revisions) {
      const item = el("li");
      const details = add(el("div"), refLink(revision.id), add(el("p"), el("span", "", actorName(revision.actor)), el("span", "", ` · ${revision.source}`)), el("p", "", revision.summary));
      const number = Number(String(revision.id).split("@")[1]);
      add(item, details, number > 1 ? refLink(`${plan.id}@${number - 1}..${number}`, "View diff", "small") : null, timeNode(revision.created_at));
      list.append(item);
    }
    const page = panel("Revision history", add(el("div"), retainForm(selector, `diff-selector:${plan.id}`), list.childElementCount ? list : empty("No revisions.")));
    page.append(omitted(data?.omitted, "revisions") || el("span"));
    const cursor = nextAfter(data);
    if (cursor !== null) page.append(link("Next revisions", "plan", pageParams({ ref: plan.id, tab: "history" }, data), "button compact"));
    return page;
  }
  function commitsPage(commits, count, plan) {
    const list = el("ol", "commit-list");
    for (const commit of commits) {
      const item = el("li", "commit-row");
      item.id = `commit-${commit.oid}`;
      add(item, refLink(commit.oid, commit.oid, "mono", { plan: plan || commit.plans[0]?.plan_id, repo_key: commit.repo_key }), el("p", "", commit.subject),
        el("p", "muted small", `${commit.author} · ${stamp(commit.committed_at)} · ${commit.files} files · +${commit.insertions} −${commit.deletions}`));
      if (commit.coauthors?.length) item.append(el("p", "small", commit.coauthors.map(author => `${author.harness} (${author.model})`).join(", ")));
      if (commit.plans?.length) item.append(add(el("div", "entry-refs"), commit.plans.map(value => refLink(value.task_ordinal ? `${value.plan_id}.${value.task_ordinal}` : value.plan_id))));
      list.append(item);
    }
    return panel("Linked commits", add(el("div"), list.childElementCount ? list : empty("No linked commits."), omitted(count, "commits")));
  }
  function diffPage(diff) {
    const page = el("div", "server-diff");
    add(page, el("p", "mono small", `${diff.before?.id || diff.before} → ${diff.after?.id || diff.after || diff.entry}`));
    if (!diff.hunks.length) return add(page, empty("No changes."));
    for (const hunk of diff.hunks) {
      const block = el("section", "diff-hunk");
      const content = el("div", "diff-body");
      add(block, el("h3", "mono", `@@ −${hunk.before_start},${hunk.removed.length} +${hunk.after_start},${hunk.added.length} @@`), content);
      for (const [lines, kind, prefix] of [[hunk.context_before, "context", " "], [hunk.removed, "removed", "−"], [hunk.added, "added", "+"], [hunk.context_after, "context", " "]]) {
        for (const text of lines) {
          const line = el("div", "diff-line"); line.dataset.change = kind;
          add(line, el("span", "diff-sign", prefix), el("span", "diff-text", text));
          if (text && !text.endsWith("\n")) line.append(el("span", "diff-eof", " · no final newline"));
          content.append(line);
        }
      }
      page.append(block);
    }
    return page;
  }
  function reviewPage(data) {
    if (!data) return empty("No stored review evidence.");
    const page = el("div");
    page.append(el("p", "muted small", "Stored evidence only. Repository ingestion runs through the router."));
    if (data.diff) page.append(diffPage(data.diff));
    page.append(panel("Entries", add(el("div", "entry-list"), (data.entries || []).map(record => entryCard(record)))));
    page.append(commitsPage(data.commits || [], data.commits_omitted));
    for (const [key, label] of [["open_proposals", "Open proposals"], ["open_questions", "Open questions"], ["open_feedback", "Open feedback"]]) {
      if (data[key]?.length) page.append(panel(label, add(el("div", "entry-list"), data[key].map(record => key === "open_proposals" ?
        add(el("article", "entry-card"), add(el("div", "entry-meta"), refLink(record.entry), badge(record.state), refLink(`${record.plan}@${record.base_revision}`)), el("p", "entry-body", record.body)) : entryCard(record)))));
    }
    return page;
  }
  function entryPage(view, diff, route, extra = {}) {
    const entry = view.entry;
    const page = add(el("div"), title(`${entry.id} · ${entry.kind}`, entry.plan ? `Plan ${entry.plan}` : "Board entry"), entryCard(entry));
    const proposal = view.proposal;
    if (proposal) {
      const head = view.plan_head_revision;
      if (proposal.state === "open" && head !== undefined && head !== null && proposal.base_revision !== head) page.append(el("p", "warning", `This proposal is stale: based on revision ${proposal.base_revision}; the plan is now at revision ${head}. Acceptance keeps its original base and will fail until a fresh proposal is made.`));
      page.append(panel("Proposed SSOT change", diffPage(diff)));
      const full = el("details", "section");
      add(full, el("summary", "", "Full proposed plan"), el("pre", "entry-body", proposal.body)); page.append(full);
      if (proposal.decision_entry) page.append(add(el("p", "small"), el("span", "muted", "Decision: "), refLink(proposal.decision_entry)));
      if (proposal.state === "open" && permitted(view, "can_decide")) {
        const form = el("form");
        const note = field("Decision note", "decision-note", "", { multiline: true, maxLength: 4096 });
        const actions = el("div", "actions");
        const accept = button("Accept proposal", () => void submitMutation(form, { op: "accept", proposal: entry.id, note: note.input.value || null }), "primary");
        accept.disabled = head !== undefined && head !== null && proposal.base_revision !== head;
        if (accept.disabled) accept.title = "The proposal needs to be rebased before acceptance.";
        actions.append(accept);
        actions.append(button("Reject proposal", () => {
          if (!note.input.value.trim()) { formStatus(form, "Give a reason for rejecting this proposal."); note.input.focus(); return; }
          void submitMutation(form, { op: "reject", proposal: entry.id, reason: note.input.value });
        }, "danger"));
        add(form, note.wrapper, actions); page.append(panel("Decision", retainForm(form, `decision:${entry.id}`)));
      }
    }
    const replies = extra.replies?.entries || view.replies;
    if (replies.length) page.append(panel("Replies and references", add(el("div", "entry-list"), replies.map(record => entryCard(record)))));
    if (!extra.replies) page.append(omitted(view.replies_omitted, "replies") || el("span"));
    const repliesNext = extra.replies ? extra.replies.next_after : view.replies_next_after;
    if (repliesNext) page.append(link("Next replies", "entry", { ...route, repliesAfter: JSON.stringify(repliesNext), repliesThrough: extra.replies?.through || view.through }, "button"));
    const backrefs = extra.backrefs?.entries || view.backrefs;
    if (backrefs.length) page.append(panel("Referenced by", add(el("div", "entry-list"), backrefs.map(record => entryCard(record)))));
    if (!extra.backrefs) page.append(omitted(view.backrefs_omitted, "references") || el("span"));
    const backrefsNext = extra.backrefs ? extra.backrefs.next_after : view.backrefs_next_after;
    if (backrefsNext) page.append(link("Next references", "entry", { ...route, backrefsAfter: JSON.stringify(backrefsNext), backrefsThrough: extra.backrefs?.through || view.through }, "button"));
    if (entry.kind === "question" && entry.plan && permitted(view, "can_answer")) page.append(panel("Answer this question", postForm(entry.plan, { kind: "answer", question: entry.id, to: actorName(entry.actor) })));
    if (entry.kind === "feedback") {
      if (view.feedback) page.append(panel("Report metadata", feedbackMetadata(view.feedback)));
      const controls = feedbackControls({ ...view, state: entry.state?.state });
      if (controls) page.append(panel("Triage feedback", controls));
    }
    if (view.linked_commit) page.append(commitsPage([view.linked_commit], 0, entry.plan));
    return page;
  }
  function searchPage(data, route) {
    const page = add(el("div"), title("Search", "Find text in plan revisions, proposals, and entries."));
    const form = el("form", "search-form");
    const input = el("input"); input.type = "search"; input.name = "q"; input.required = true;
    input.value = route.q; input.placeholder = "Search board text"; input.setAttribute("aria-label", "Search board text");
    const plan = el("input"); plan.name = "plan"; plan.value = route.plan; plan.placeholder = "Plan (optional P#)"; plan.setAttribute("aria-label", "Limit search to plan");
    const submit = el("button", "primary", "Search"); submit.type = "submit";
    add(form, input, plan, submit);
    form.addEventListener("submit", event => { event.preventDefault(); navigate("search", { q: input.value, plan: plan.value }); });
    page.append(retainForm(form, `search:${route.q}:${route.plan}`));
    if (!route.q) return add(page, empty("Enter a phrase to search the board."));
    const hits = collection(data, "hits");
    for (const hit of hits) {
      const item = refLink(hit.target, "", "search-result");
      add(item, add(el("div", "row"), el("h2", "mono", hit.target), badge(hit.source || hit.kind), hit.plan ? el("span", "muted small", hit.plan) : null), el("p", "", hit.snippet));
      page.append(item);
    }
    if (!hits.length) page.append(empty("No matches."));
    if (data?.truncated) page.append(el("p", "omitted", "More matches exist. Refine your query or limit it to a plan."));
    return page;
  }
  function editorPage(mode, view, repositories = []) {
    const revision = view?.revision || view;
    const base = revision?.id || "";
    const key = mode === "new" ? "new" : `${mode}:${base}`;
    let draft = state.drafts.get(key);
    if (!draft) { draft = { title: "", body: revision?.body || "", summary: "", steward: "", repo_key: "", base }; state.drafts.set(key, draft); }
    const page = add(el("div"), title(mode === "new" ? "New plan" : "Edit plan", base ? `Based on ${draft.base}. This base stays fixed while you edit.` : "A shared source of truth for a piece of work."));
    const form = el("form", "panel form-panel");
    const fields = [];
    if (mode === "new") fields.push(field("Title", "title", draft.title, { required: true, maxLength: 256 }));
    const body = field("Plan text", "body", draft.body, { multiline: true, className: "ssot-editor", maxLength: 32768, hint: "Markdown, up to 32 KiB" });
    fields.push(body);
    if (mode === "new") fields.push(field("Steward harness", "steward", draft.steward, { hint: "optional" }));
    else fields.push(field("Change summary", "summary", draft.summary, { required: true, maxLength: 4096 }));
    if (mode === "new" && repositories.length) {
      const known = new Map();
      for (const target of repositories) {
        const registration = target?.registration;
        if (registration?.repo_key && !known.has(registration.repo_key)) known.set(registration.repo_key, registration.origin_label || registration.repo_key);
      }
      if (known.size) fields.push(field("Repository", "repo_key", draft.repo_key, {
        choices: [["", "No repository link (visible in every scope)"], ...[...known].map(([repoKey, label]) => [repoKey, label])],
        hint: "optional; scopes the plan to a registered repository",
      }));
    }
    for (const item of fields) {
      item.input.addEventListener("input", () => { draft[item.input.name] = item.input.value; });
      form.append(item.wrapper);
    }
    const actions = el("div", "form-footer");
    const submit = el("button", "primary", mode === "new" ? "Create plan" : "Save revision"); submit.type = "submit";
    const cancel = link("Cancel", base ? "plan" : "overview", base ? { ref: planId(base) } : {}, "button");
    const bytes = el("span", "byte-count");
    const updateBytes = () => { const size = new TextEncoder().encode(body.input.value).length; bytes.textContent = `${size.toLocaleString()} / 32,768 bytes`; bytes.classList.toggle("error", size > 32768); };
    body.input.addEventListener("input", updateBytes); updateBytes();
    add(actions, submit, cancel, bytes); form.append(actions);
    if (mode === "edit" && view?.plan && !permitted(view, "can_edit")) { submit.disabled = true; formStatus(form, "The backend has not granted permission to edit this plan."); }
    form.addEventListener("submit", event => {
      event.preventDefault();
      const values = Object.fromEntries(fields.map(item => [item.input.name, item.input.value]));
      const op = mode === "new" ? { op: "new", title: values.title, body: values.body, steward: values.steward || null, repo_key: values.repo_key || null } : { op: mode, base: draft.base, body: values.body, summary: values.summary };
      void submitMutation(form, op, (reply, current) => {
        state.drafts.delete(key);
        Object.assign(draft, { title: "", body: revision?.body || "", summary: "", steward: "", repo_key: "" });
        for (const item of fields) item.input.value = draft[item.input.name] || "";
        if (!current) return;
        notice("Plan saved.", "success");
        const change = reply.data;
        navigate("plan", { ref: change.plan || planId(change.revision) });
      });
    });
    add(page, retainForm(form, `editor:${key}`));
    return page;
  }

  return { planTabs, sanitizedMarkup, renderPlan, filtersForm, entriesPage, historyPage, commitsPage, diffPage, reviewPage, feedbackControls, feedbackPage, entryPage, searchPage, editorPage };
}
