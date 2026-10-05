import { createBoardEntries } from "./board_entries.js";
import { createBoardFeedback } from "./feedback_triage.js";
import { createPlanPage } from "./plan_page.js";
import { createProposalPage } from "./proposal_page.js";

export function createBoardDetails(context) {
  const {
    state, dom, board, jsonFetch, navigate, scheduleRefresh, submitMutation, notice, apiVersion,
    entryRecord, collection, permitted, nextAfter, pageParams, queryFilters, postKinds, entryKinds,
    entryCard, taskCard, workingCard, postForm, taskDetails,
  } = context;
  const {
    el, add, button, routeUrl, link, refLink, badge, actorName, shortActor, stamp, timeNode, age,
    ageNode, panel, empty, omitted, title, field, formStatus, planId, focusKey, retainForm,
  } = dom;
  const { feedbackControls, feedbackPage } = createBoardFeedback(context);
  const { filtersForm, entriesPage } = createBoardEntries(context);
  const { planTabs, sanitizedMarkup, renderPlan, historyPage, commitsPage } = createPlanPage(context);
  const { entryPage, diffPage } = createProposalPage(context);
  function searchPage(data, route) {
    const page = add(el("div"), title("Search", "Find text in plan titles, revisions, proposals, and entries."));
    const form = el("form", "search-form");
    const query = field("Search board text", "q", route.q, { placeholder: "Search board text" });
    query.input.type = "search"; query.input.required = true;
    const plan = field("Limit to plan", "plan", route.plan, { placeholder: "Plan (optional P#)" });
    const submit = focusKey(el("button", "primary", "Search"), `search-submit:${route.q}:${route.plan}`);
    submit.type = "submit";
    add(form, query.wrapper, plan.wrapper, submit);
    form.addEventListener("submit", event => {
      event.preventDefault(); navigate("search", { q: query.input.value, plan: plan.input.value });
    });
    page.append(retainForm(form, `search:${route.q}:${route.plan}`));
    if (!route.q) return add(page, empty("Enter a phrase to search the board."));
    const hits = collection(data, "hits");
    for (const hit of hits) {
      const item = refLink(hit.target, "", "search-result");
      add(item,
        add(el("div", "row"), el("h2", "mono", hit.target), badge(hit.source || hit.kind),
          hit.plan ? el("span", "muted small", hit.plan) : null),
        el("p", "", hit.snippet));
      page.append(item);
    }
    if (!hits.length) page.append(empty("No matches."));
    if (data?.truncated) {
      page.append(el("p", "omitted", "More matches exist. Refine your query or limit it to a plan."));
    }
    return page;
  }
  function editorPage(mode, view, repositories = []) {
    const revision = view?.revision || view;
    const base = revision?.id || "";
    const key = mode === "new" ? "new" : `${mode}:${base}`;
    let draft = state.drafts.get(key);
    if (!draft) {
      draft = { title: "", body: revision?.body || "", summary: "", steward: "", repo_key: "", base };
      state.drafts.set(key, draft);
    }
    const page = add(el("div"), title(
      mode === "new" ? "New plan" : "Edit plan",
      base
        ? `Based on ${draft.base}. This base stays fixed while you edit.`
        : "A shared source of truth for a piece of work."
    ));
    const form = el("form", "panel form-panel");
    const fields = [];
    if (mode === "new") fields.push(field("Title", "title", draft.title, { required: true, maxLength: 256 }));
    const body = field("Plan text", "body", draft.body, {
      multiline: true, className: "ssot-editor", maxLength: 32768, hint: "Markdown, up to 32 KiB",
    });
    fields.push(body);
    if (mode === "new") fields.push(field("Steward harness", "steward", draft.steward, { hint: "optional" }));
    else fields.push(field("Change summary", "summary", draft.summary, { required: true, maxLength: 4096 }));
    if (mode === "new" && repositories.length) {
      const known = new Map();
      for (const target of repositories) {
        const registration = target?.registration;
        if (registration?.repo_key && !known.has(registration.repo_key)) {
          known.set(registration.repo_key, registration.origin_label || registration.repo_key);
        }
      }
      if (known.size) fields.push(field("Repository", "repo_key", draft.repo_key, {
        choices: [["", "No repository link (visible in every scope)"],
          ...[...known].map(([repoKey, label]) => [repoKey, label])],
        hint: "optional; scopes the plan to a registered repository",
      }));
    }
    for (const item of fields) {
      item.input.addEventListener("input", () => { draft[item.input.name] = item.input.value; });
      form.append(item.wrapper);
    }
    const actions = el("div", "form-footer");
    const submit = focusKey(el("button", "primary", mode === "new" ? "Create plan" : "Save revision"),
      `editor:${key}:submit`);
    submit.type = "submit";
    const cancel = link("Cancel", base ? "plan" : "overview", base ? { ref: planId(base) } : {}, "button");
    const bytes = el("span", "byte-count");
    const updateBytes = () => {
      const size = new TextEncoder().encode(body.input.value).length;
      bytes.textContent = `${size.toLocaleString()} / 32,768 bytes`;
      bytes.classList.toggle("error", size > 32768);
    };
    body.input.addEventListener("input", updateBytes); updateBytes();
    add(actions, submit, cancel, bytes); form.append(actions);
    if (mode === "edit" && view?.plan && !permitted(view, "can_edit")) {
      submit.disabled = true;
      formStatus(form, "The backend has not granted permission to edit this plan.");
    }
    form.addEventListener("submit", event => {
      event.preventDefault();
      const values = Object.fromEntries(fields.map(item => [item.input.name, item.input.value]));
      const op = mode === "new"
        ? {
          op: "new", title: values.title, body: values.body,
          steward: values.steward || null, repo_key: values.repo_key || null,
        }
        : { op: mode, base: draft.base, body: values.body, summary: values.summary };
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

  return {
    planTabs, sanitizedMarkup, renderPlan, filtersForm, entriesPage, historyPage, commitsPage,
    diffPage, feedbackControls, feedbackPage, entryPage, searchPage, editorPage,
  };
}
