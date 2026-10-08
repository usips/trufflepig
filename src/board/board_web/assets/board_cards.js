import { actorMark, recipientMark } from "./marks/agent_marks.js";

export function createBoardCards({ state, dom, entryRecord, collection }) {
  const {
    el, add, link, refLink, badge, actorName, shortActor, timeNode, ageNode,
    empty, focusKey, planId,
  } = dom;
  function cardPreview(value) {
    const text = String(value || ""), characters = Array.from(text);
    return characters.length > 280 ? `${characters.slice(0, 279).join("")}…` : text;
  }

  function entryCard(record, options = {}) {
    const entry = entryRecord(record);
    if (!entry) return empty("Entry unavailable.");
    const item = el("article", "entry-card");
    focusKey(item, `entry-card:${entry.id}`);
    const meta = el("div", "entry-meta");
    add(meta, refLink(entry.id), badge(entry.kind), actorMark(entry),
      el("span", "", actorName(entry.actor)),
      entry.via === "outbox" ? el("span", "provenance", "(spooled, unverified)") : null,
      entry.model ? el("span", "", `${entry.model}${entry.effort ? ` · ${entry.effort}` : ""}`) : null,
      entry.state ? badge(entry.state.state || entry.state) : null, timeNode(entry.created_at));
    add(item, meta, el("p", "entry-body", entry.body));
    const refs = el("div", "entry-refs");
    if (entry.plan) refs.append(refLink(entry.plan));
    for (const ref of entry.refs || []) {
      refs.append(refLink(ref, ref, "mono", { plan: entry.plan, repo_key: entry.repo_key }));
    }
    if (entry.to) refs.append(el("span", "muted", `To ${entry.to}`));
    if (entry.supersedes) add(refs, el("span", "muted", "Supersedes"), refLink(entry.supersedes));
    if (refs.childElementCount) item.append(refs);
    if (options.actions) item.append(options.actions);
    return item;
  }
  function eventList(events) {
    const list = el("ol", "event-list");
    const fresh = event => state.seenAtOpen !== null && /^(0|[1-9]\d*)$/.test(String(event.seq))
      && BigInt(String(event.seq)) > BigInt(state.seenAtOpen);
    for (const event of events || []) {
      const item = el("li", "event-row");
      focusKey(item, `event:${event.seq}`);
      add(item, fresh(event) ? badge("new") : null, badge(event.kind), refLink(event.subject),
        el("span", "event-summary", cardPreview(event.summary)),
        actorMark(event), el("span", "muted small", shortActor(event.actor)),
        event.via === "outbox" ? el("span", "provenance", "(spooled, unverified)") : null,
        timeNode(event.created_at));
      list.append(item);
    }
    return list.childElementCount ? list : empty("No recent activity.");
  }
  function claimIdentity(claim, className = "") {
    const identity = add(el("span", `claim-identity ${className}`), actorMark(claim),
      el("span", "", shortActor(claim.actor)));
    identity.title = actorName(claim.actor);
    return identity;
  }
  function workingCard(claim, task) {
    const item = el("article", "working-card"); item.dataset.stale = claim.stale;
    focusKey(item, `working-card:${claim.task}`);
    add(item,
      add(el("div", "working-card-heading"), refLink(claim.task, task?.title || claim.task, "working-task"),
        claim.stale ? badge("stale") : badge("doing")),
      el("p", "working-scope", claim.scope),
      add(el("div", "working-meta"), claimIdentity(claim, "working-identity"),
        claim.model || claim.effort ? el("span", "", [claim.model, claim.effort].filter(Boolean).join(" · ")) : null,
        refLink(claim.task),
        add(el("span", "working-active"), el("span", "", "Active "), ageNode(claim.last_active))));
    return item;
  }
  function taskCard(task, claims, options = {}) {
    const claim = (claims || []).find(item => item.task === task.id && !item.ended_at);
    const item = el("article", "task-card");
    focusKey(item, `task-card:${task.id}`);
    if (claim) item.dataset.stale = claim.stale;
    add(item, refLink(task.id, task.title, ""), el("div", "small muted", task.id),
      claim
        ? add(el("div", "small task-claim"), claimIdentity(claim),
          el("span", "muted", " · "),
          ageNode(claim.last_active))
        : (task.assignee ? add(el("div", "small muted"), recipientMark(task.assignee),
          el("span", "", `Assigned: ${task.assignee}`)) : null),
      claim?.stale ? badge("stale") : null);
    if (claim?.model || claim?.effort) {
      item.append(el("div", "small muted", [claim.model, claim.effort].filter(Boolean).join(" · ")));
    }
    if (!claim && options.claimsOmitted) {
      item.append(link("Claim details omitted", "claims", { plan: options.plan || planId(task.id),
        after: options.claimAfter ? JSON.stringify(options.claimAfter) : null, through: options.through }, "small"));
    }
    return item;
  }
  function attentionItems(data) {
    return collection(data, "entries").map(record => ({ type: entryRecord(record).kind, record }));
  }
  function attentionCard(item) {
    const entry = entryRecord(item.record);
    const card = el("article", "attention-card");
    focusKey(card, `attention-card:${entry?.id || item.record.entry}`);
    const preview = cardPreview(entry?.body || item.record.summary || "Open entry");
    add(card,
      add(el("div", "row wrap"), badge(item.type), entry?.plan ? refLink(entry.plan) : null),
      el("p", "attention-preview", preview),
      refLink(entry?.id || item.record.entry, "Read entry →", "attention-entry-link"),
      add(el("p", "attention-author"), entry?.actor ? actorMark(entry) : null,
        el("span", "", entry?.actor ? `From ${shortActor(entry.actor)}` : ""),
        entry?.via === "outbox" ? el("span", "provenance", " (spooled, unverified)") : null));
    if (entry?.actor) card.querySelector(".attention-author").title = actorName(entry.actor);
    return card;
  }
  function doneFooter(plan, item) {
    const done = el("details", "lane-done"), count = item.done_count || 0;
    done.dataset.donePlan = plan.id;
    done.open = state.openDonePlans?.has(plan.id) || false;
    const summary = el("summary", "done-summary", `Done (${count})`);
    summary.dataset.focusKey = `done:${plan.id}`;
    const recent = (item.recent_done || []).slice(0, 5);
    const more = count - recent.length;
    add(done, summary, add(el("div", "lane-done-content"),
      recent.length ? add(el("div", "recent-done-cards"), recent.map(task => taskCard(task, [])))
        : el("p", "small muted", "No completed tasks yet."),
      link(more > 0 ? `${more} more →` : "View all completed tasks →", "plan",
        { ref: plan.id, tab: "done" }, "small done-history-link")));
    return done;
  }

  return { entryCard, eventList, workingCard, taskCard, attentionItems, attentionCard, doneFooter };
}
