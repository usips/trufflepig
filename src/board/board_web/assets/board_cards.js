export function createBoardCards({ state, dom, entryRecord, collection }) {
  const {
    el, add, link, refLink, badge, actorName, shortActor, timeNode, ageNode,
    empty, focusKey, planId,
  } = dom;

  function entryCard(record, options = {}) {
    const entry = entryRecord(record);
    if (!entry) return empty("Entry unavailable.");
    const item = el("article", "entry-card");
    focusKey(item, `entry-card:${entry.id}`);
    const meta = el("div", "entry-meta");
    add(meta, refLink(entry.id), badge(entry.kind), el("span", "", actorName(entry.actor)),
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
        el("span", "event-summary", event.summary),
        el("span", "muted small", shortActor(event.actor)),
        event.via === "outbox" ? el("span", "provenance", "(spooled, unverified)") : null,
        timeNode(event.created_at));
      list.append(item);
    }
    return list.childElementCount ? list : empty("No recent activity.");
  }
  function workingCard(claim) {
    const item = el("div", "working-card"); item.dataset.stale = claim.stale;
    focusKey(item, `working-card:${claim.task}`);
    add(item,
      add(el("div", "row spread"), el("strong", "", shortActor(claim.actor)),
        claim.stale ? badge("stale") : badge("doing")),
      refLink(claim.task), el("div", "muted small", actorName(claim.actor)),
      claim.model || claim.effort ? el("div", "small", [claim.model, claim.effort].filter(Boolean).join(" · ")) : null,
      add(el("div", "small"), el("span", "muted", "Active "), ageNode(claim.last_active)),
      el("div", "small", claim.scope));
    return item;
  }
  function taskCard(task, claims, options = {}) {
    const claim = (claims || []).find(item => item.task === task.id && !item.ended_at);
    const item = el("article", "task-card");
    focusKey(item, `task-card:${task.id}`);
    if (claim) item.dataset.stale = claim.stale;
    add(item, refLink(task.id, task.title, ""), el("div", "small muted", task.id),
      claim
        ? add(el("div", "small"), el("span", "", actorName(claim.actor)), el("span", "muted", " · "),
          ageNode(claim.last_active))
        : (task.assignee ? el("div", "small muted", `Assigned: ${task.assignee}`) : null),
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
    add(card,
      add(el("div", "row"), badge(item.type), entry?.plan ? refLink(entry.plan) : null),
      refLink(entry?.id || item.record.entry, entry?.body || item.record.summary || "Open entry", ""),
      add(el("p"), el("span", "", entry?.actor ? `From ${actorName(entry.actor)}` : ""),
        entry?.via === "outbox" ? el("span", "provenance", " (spooled, unverified)") : null));
    return card;
  }

  return { entryCard, eventList, workingCard, taskCard, attentionItems, attentionCard };
}
