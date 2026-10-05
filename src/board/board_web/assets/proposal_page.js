import { createBoardFeedback } from "./feedback_triage.js";
import { createPlanPage } from "./plan_page.js";

export function createProposalPage(context) {
  const {
    state, dom, board, jsonFetch, navigate, scheduleRefresh, submitMutation, notice, apiVersion,
    entryRecord, collection, permitted, nextAfter, pageParams, queryFilters, postKinds, entryKinds,
    entryCard, taskCard, workingCard, postForm, taskDetails,
  } = context;
  const {
    el, add, button, routeUrl, link, refLink, badge, actorName, shortActor, stamp, timeNode, age,
    ageNode, panel, empty, omitted, title, field, formStatus, planId, focusKey, retainForm,
  } = dom;
  const { feedbackControls, feedbackMetadata } = createBoardFeedback(context);
  const { commitsPage } = createPlanPage(context);
  function diffPage(diff) {
    const page = el("div", "server-diff");
    add(page, el("p", "mono small",
      `${diff.before?.id || diff.before} → ${diff.after?.id || diff.after || diff.entry}`));
    if (!diff.hunks.length) return add(page, empty("No changes."));
    for (const hunk of diff.hunks) {
      const block = el("section", "diff-hunk");
      const content = el("div", "diff-body");
      const heading = el("h3", "mono",
        `@@ −${hunk.before_start},${hunk.removed.length} +${hunk.after_start},${hunk.added.length} @@`);
      add(block, heading, content);
      for (const [lines, kind, prefix] of [
        [hunk.context_before, "context", " "], [hunk.removed, "removed", "−"],
        [hunk.added, "added", "+"], [hunk.context_after, "context", " "],
      ]) {
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
  function entryPage(view, diff, route, extra = {}) {
    const entry = view.entry;
    const page = add(el("div"),
      title(`${entry.id} · ${entry.kind}`, entry.plan ? `Plan ${entry.plan}` : "Board entry"),
      entryCard(entry));
    const proposal = view.proposal;
    if (proposal) {
      const head = view.plan_head_revision;
      if (proposal.state === "open" && head !== undefined && head !== null && proposal.base_revision !== head) {
        page.append(el("p", "warning",
          `This proposal is stale: based on revision ${proposal.base_revision}; the plan is now at revision ${head}.`
            + " Acceptance keeps its original base and will fail until a fresh proposal is made."));
      }
      page.append(panel("Proposed SSOT change", diffPage(diff)));
      const full = el("details", "section");
      add(full, el("summary", "", "Full proposed plan"), el("pre", "entry-body", proposal.body)); page.append(full);
      if (proposal.decision_entry) {
        page.append(add(el("p", "small"), el("span", "muted", "Decision: "), refLink(proposal.decision_entry)));
      }
      if (proposal.state === "open" && permitted(view, "can_decide")) {
        const form = el("form");
        const note = field("Decision note", "decision-note", "", { multiline: true, maxLength: 4096 });
        const actions = el("div", "actions");
        const accept = focusKey(button("Accept proposal",
          () => void submitMutation(form, { op: "accept", proposal: entry.id, note: note.input.value || null }),
          "primary"), `decision:${entry.id}:accept`);
        accept.disabled = head !== undefined && head !== null && proposal.base_revision !== head;
        if (accept.disabled) accept.title = "The proposal needs to be rebased before acceptance.";
        actions.append(accept);
        actions.append(focusKey(button("Reject proposal", () => {
          if (!note.input.value.trim()) {
            formStatus(form, "Give a reason for rejecting this proposal."); note.input.focus(); return;
          }
          void submitMutation(form, { op: "reject", proposal: entry.id, reason: note.input.value });
        }, "danger"), `decision:${entry.id}:reject`));
        add(form, note.wrapper, actions); page.append(panel("Decision", retainForm(form, `decision:${entry.id}`)));
      }
    }
    const replies = extra.replies?.entries || view.replies;
    if (replies.length) {
      page.append(panel("Replies and references",
        add(el("div", "entry-list"), replies.map(record => entryCard(record)))));
    }
    if (!extra.replies) page.append(omitted(view.replies_omitted, "replies") || el("span"));
    const repliesNext = extra.replies ? extra.replies.next_after : view.replies_next_after;
    if (repliesNext) {
      page.append(link("Next replies", "entry", {
        ...route, repliesAfter: JSON.stringify(repliesNext),
        repliesThrough: extra.replies?.through || view.through,
      }, "button"));
    }
    const backrefs = extra.backrefs?.entries || view.backrefs;
    if (backrefs.length) {
      page.append(panel("Referenced by", add(el("div", "entry-list"), backrefs.map(record => entryCard(record)))));
    }
    if (!extra.backrefs) page.append(omitted(view.backrefs_omitted, "references") || el("span"));
    const backrefsNext = extra.backrefs ? extra.backrefs.next_after : view.backrefs_next_after;
    if (backrefsNext) {
      page.append(link("Next references", "entry", {
        ...route, backrefsAfter: JSON.stringify(backrefsNext),
        backrefsThrough: extra.backrefs?.through || view.through,
      }, "button"));
    }
    if (entry.kind === "question" && entry.plan && permitted(view, "can_answer")) {
      page.append(panel("Answer this question",
        postForm(entry.plan, { kind: "answer", question: entry.id, to: actorName(entry.actor) })));
    }
    if (entry.kind === "feedback") {
      if (view.feedback) page.append(panel("Report metadata", feedbackMetadata(view.feedback)));
      const controls = feedbackControls({ ...view, state: entry.state?.state });
      if (controls) page.append(panel("Triage feedback", controls));
    }
    if (view.linked_commit) page.append(commitsPage([view.linked_commit], 0, entry.plan));
    return page;
  }

  return { entryPage, diffPage };
}
