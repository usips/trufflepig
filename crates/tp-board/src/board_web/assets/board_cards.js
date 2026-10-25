import { actorAttribution, harnessVendor, recipientMark } from "./marks/agent_marks.js";
import { entryAnchor, workerAnchor } from "./board_refs.js";
import { boardRefLinkedInline, boardTextRefs } from "./board_linkify.js";

export function createBoardCards({ state, dom, entryRecord, collection }) {
  const {
    el, add, link, refLink, linkified, badge, actorName, timeNode, ageNode,
    disclosure, empty, focusKey, planId,
  } = dom;
  // Needs you and Latest activity previews: 280 codepoints, references whole.
  const PREVIEW = { maxCodepoints: 280, copy: false };

  function entryCard(record, options = {}) {
    const entry = entryRecord(record);
    if (!entry) return empty("Entry unavailable.");
    const item = el("article", "entry-card");
    focusKey(item, `entry-card:${entry.id}`);
    item.dataset.anchor = entryAnchor(entry.id);
    const meta = el("div", "entry-meta");
    add(meta, refLink(entry.id), badge(entry.kind), actorAttribution(entry),
      entry.via === "outbox" ? el("span", "provenance", "(spooled, unverified)") : null,
      entry.model ? el("span", "", `${entry.model}${entry.effort ? ` · ${entry.effort}` : ""}`) : null,
      entry.state ? badge(entry.state.state || entry.state) : null, timeNode(entry.created_at));
    const context = { plan: entry.plan, repo_key: entry.repo_key };
    add(item, meta, linkified("p", "entry-body", entry.body, context));
    // Chips keep only server-extracted references the body does not link inline.
    const refs = el("div", "entry-refs"), inline = boardTextRefs(entry.body);
    if (entry.plan) refs.append(refLink(entry.plan));
    for (const ref of entry.refs || []) {
      if (!boardRefLinkedInline(inline, ref)) refs.append(refLink(ref, ref, "mono", context));
    }
    if (entry.to) refs.append(el("span", "muted", `To ${entry.to}`));
    if (entry.supersedes) add(refs, el("span", "muted", "Supersedes"), refLink(entry.supersedes));
    if (refs.childElementCount) item.append(refs);
    if (options.actions) item.append(options.actions);
    return item;
  }
  // Claim and worker events link where the work lives: a claim opens its task
  // focused on the claim entry (still linked), a worker event opens that worker.
  function eventSubject(event) {
    const summary = String(event.summary || "");
    const claimed = event.kind === "claim" && /^took (P[1-9]\d*\.\d+):/.exec(summary);
    if (claimed && /^E[1-9]\d*$/.test(String(event.subject))) {
      return add(el("span", "event-subject"),
        refLink(claimed[1], claimed[1], "mono", { focus: entryAnchor(event.subject) }),
        el("span", "muted small", " · "), refLink(event.subject, event.subject, "mono muted small"));
    }
    const worker = ["worker_add", "worker_end"].includes(event.kind)
      && /\bworker (P[1-9]\d*\.\d+)\/([a-z0-9_-]{1,32})\b/.exec(summary);
    if (worker && worker[1] === String(event.subject)) return refLink(`${worker[1]}/${worker[2]}`);
    return refLink(event.subject, event.subject, "mono", { plan: event.plan });
  }
  function eventList(events) {
    const list = el("ol", "event-list");
    const activity = (events || []).map(event => {
      const sequence = String(event.seq);
      return { event, sequence: /^(0|[1-9]\d*)$/.test(sequence) ? BigInt(sequence) : null };
    });
    const ordered = activity.every(item => item.sequence !== null)
      ? activity.sort((left, right) => left.sequence > right.sequence ? -1
        : left.sequence < right.sequence ? 1 : 0).map(item => item.event)
      : (events || []);
    const fresh = event => state.seenAtOpen !== null && /^(0|[1-9]\d*)$/.test(String(event.seq))
      && BigInt(String(event.seq)) > BigInt(state.seenAtOpen);
    for (const event of ordered) {
      const item = el("li", "event-row");
      focusKey(item, `event:${event.seq}`);
      add(item, fresh(event) ? badge("new") : null, badge(event.kind), eventSubject(event),
        linkified("span", "event-summary", event.summary, { plan: event.plan }, PREVIEW),
        actorAttribution(event),
        event.via === "outbox" ? el("span", "provenance", "(spooled, unverified)") : null,
        timeNode(event.created_at));
      list.append(item);
    }
    return list.childElementCount ? list : empty("No recent activity.");
  }
  function claimIdentity(claim, options = {}) {
    const source = (options.entries || []).find(entry => entry?.id === claim.entry);
    return add(el("span", "claim-identity"), actorAttribution({ ...claim,
      origin: source?.origin, authority: source?.authority,
    }));
  }
  function displayNow(options = {}) {
    const supplied = Number(options.now);
    return Number.isFinite(supplied) && supplied > 0
      ? supplied : (Date.now() + (Number(state.clockOffsetMs) || 0)) / 1000;
  }
  function agedState(timestamp, now, ttlSecs) {
    const at = Number(timestamp), ttl = Number(ttlSecs);
    if (!Number.isFinite(at) || !Number.isFinite(now)) return "fresh";
    const elapsed = Math.max(0, now - at);
    if (Number.isFinite(ttl) && ttl > 0 && elapsed > ttl) return "expired";
    return elapsed > 3600 ? "old" : "fresh";
  }
  function progressFact(label, fact, value, options = {}) {
    if (!fact) return null;
    const now = displayNow(options), ttlSecs = options.ttlSecs;
    const row = el("div", "progress-fact");
    row.dataset.stale = String(agedState(fact.updated_at, now, ttlSecs) === "expired");
    add(row, el("strong", "", label), value,
      add(el("span", "progress-updated muted small"), el("span", "", "Updated "),
        timeNode(fact.updated_at)));
    return row;
  }
  function progressPanel(progress, options = {}) {
    if (!progress) return null;
    const rows = [];
    if (progress.head) {
      const head = progress.head.value || {};
      rows.push(progressFact("Head", progress.head,
        add(el("span", "progress-value mono"), el("span", "", head.oid || "Unknown head"),
          head.verified ? badge("verified") : badge("reported")), options));
    }
    if (progress.step) {
      const step = progress.step.value || {};
      rows.push(progressFact("Step", progress.step,
        el("span", "progress-value", `${step.current} / ${step.total}`), options));
    }
    if (progress.tests) {
      const tests = progress.tests.value || {};
      rows.push(progressFact("Reported tests", progress.tests,
        el("span", "progress-value", `${tests.passed} passed · ${tests.failed} failed`), options));
    }
    for (const gate of progress.gates || []) {
      const status = String(gate.status || "unknown").toLowerCase();
      rows.push(progressFact(`Gate · ${gate.name}`, gate,
        add(el("span", "progress-value"), badge(status)), options));
    }
    if (progress.eta_at) {
      const eta = Number(progress.eta_at.value);
      const value = add(el("span", "progress-value progress-eta"),
        el("span", "", "ETA "), timeNode(progress.eta_at.value));
      value.dataset.overdue = String(Number.isFinite(eta) && eta < displayNow(options));
      rows.push(progressFact("Estimate", progress.eta_at, value, options));
    }
    if (!rows.length) return null;
    const list = add(el("div", "progress-facts"), rows);
    return add(el("section", "progress-panel"), el("h3", "", "Progress"), list);
  }
  function verdictLabel(verdict) {
    if (!verdict) return "";
    if (typeof verdict === "string") {
      const match = /^(findings|blocking)=(\d+)$/.exec(verdict);
      return match ? `${match[1]} (${match[2]})` : verdict;
    }
    const [kind, count] = Object.entries(verdict)[0] || [];
    return kind ? count === undefined || count === null ? kind : `${kind} (${count})` : "";
  }
  function workerRoleLabel(role) {
    return ({ code: "Code worker", review: "Reviewer", test: "Tester", research: "Researcher" })[role]
      || "Worker";
  }
  function workerRow(worker, options = {}) {
    const now = displayNow(options), ended = Boolean(worker.ended_at || worker.state);
    const source = (options.entries || []).find(entry => entry?.id === worker.entry);
    const item = el("li", "agent-tree-row worker-row");
    item.dataset.state = ended ? String(worker.state || "ended") : "live";
    if (worker.address?.task && worker.address?.handle) {
      item.dataset.anchor = workerAnchor(worker.address.task, worker.address.handle);
    }
    const details = add(el("div", "agent-row-heading"),
      el("strong", "", workerRoleLabel(worker.role)),
      worker.state ? badge(worker.state) : badge("live"),
      el("span", "agent-handle mono", `/${worker.address?.handle || "worker"}`));
    add(item, details);
    if (worker.actor || source?.origin || source?.authority) {
      const actor = worker.actor || source?.actor || null;
      item.append(actorAttribution({ ...worker,
        actor, vendor: harnessVendor(actor?.harness),
        origin: source?.origin, authority: source?.authority,
      }));
    }
    add(item,
      worker.model || worker.effort
        ? el("p", "agent-model muted small", [worker.model, worker.effort].filter(Boolean).join(" · ")) : null,
      linkified("p", "agent-scope", worker.scope, { plan: planId(worker.address?.task) }),
      worker.files?.length ? el("p", "agent-files muted small", `Files: ${worker.files.join(", ")}`) : null,
      add(el("p", "agent-active muted small"), el("span", "", "Last active "), ageNode(worker.last_active)),
      worker.ended_at ? add(el("p", "agent-ended muted small"), el("span", "", "Ended "), timeNode(worker.ended_at)) : null,
      worker.note ? linkified("p", "agent-note", worker.note, { plan: planId(worker.address?.task) }) : null,
      worker.verdict ? el("p", "agent-verdict small", `Review: ${verdictLabel(worker.verdict)}`) : null,
      progressPanel(worker.progress, { ...options, now }));
    return item;
  }
  function workerTree(claim, options = {}) {
    const workers = Array.isArray(claim?.workers) ? claim.workers : [];
    const now = displayNow(options), ttlSecs = options.ttlSecs;
    const entries = options.entries || [];
    const claimEntry = entries.find(entry => entry?.id === claim.entry);
    const lead = add(el("li", "agent-tree-row lead-row"),
      add(el("div", "agent-row-heading"), el("strong", "", "Task lead"),
        claim.ended_at ? badge(claim.end_reason || "ended") : badge(claim.stale ? "stale" : "live")),
      actorAttribution({ ...claim, origin: claimEntry?.origin, authority: claimEntry?.authority }),
      claim.model || claim.effort
        ? el("p", "agent-model muted small", [claim.model, claim.effort].filter(Boolean).join(" · ")) : null,
      linkified("p", "agent-scope", claim.scope, { plan: planId(claim.task) }),
      add(el("p", "agent-active muted small"), el("span", "", "Last active "), ageNode(claim.last_active)));
    if (claim.entry) lead.dataset.anchor = entryAnchor(claim.entry);
    if (claim.delegated_by) {
      lead.append(add(el("p", "agent-delegator muted small"), el("span", "", "Assigned by "),
        actorAttribution({ actor: claim.delegated_by,
          vendor: harnessVendor(claim.delegated_by.harness) })));
    }
    const ordered = workers.slice().sort((left, right) => {
      const leftLive = !left.ended_at && !left.state, rightLive = !right.ended_at && !right.state;
      if (leftLive !== rightLive) return leftLive ? -1 : 1;
      return Number(right.last_active || 0) - Number(left.last_active || 0);
    });
    const bounded = ordered.slice(0, 20);
    const overflow = Math.max(0, ordered.length - bounded.length) + Number(claim.workers_omitted || 0);
    const recent = [], oldEnded = [];
    for (const worker of bounded) {
      const endedAt = Number(worker.ended_at);
      if ((worker.state || worker.ended_at) && Number.isFinite(endedAt) && now - endedAt > 1800) oldEnded.push(worker);
      else recent.push(worker);
    }
    const list = add(el("ol", "agent-tree-list"), lead, recent.map(worker => workerRow(worker, { ...options, entries, ttlSecs })));
    if (oldEnded.length) {
      const ended = disclosure(`ended-workers:${claim.entry || claim.task}`,
        `Ended workers (${oldEnded.length})`, "agent-ended-group");
      ended.append(add(el("ol", "agent-tree-list"),
        oldEnded.map(worker => workerRow(worker, { ...options, entries, ttlSecs }))));
      list.append(add(el("li", "agent-ended-group-row"), ended));
    }
    const tree = el("section", "agent-tree");
    tree.setAttribute("aria-label", "Task agents");
    add(tree, el("h3", "", "Task agents"), list,
      overflow ? el("p", "omitted worker-omitted", `${overflow} workers omitted from this snapshot.`) : null,
      progressPanel(claim.progress, { ...options, ttlSecs }));
    return tree;
  }
  function claimSignals(claim, options = {}) {
    if (!claim) return null;
    const items = [];
    const count = Number(claim.open_questions || 0);
    if (count > 0 || options.showZeroQuestions) {
      const question = el("span", "claim-signal question-signal");
      const age = agedState(claim.oldest_open_question_at, displayNow(options), options.ttlSecs);
      question.dataset.age = age;
      add(question, el("span", "", `${count} open questions`));
      if (claim.oldest_open_question) {
        add(question, el("span", "muted small", " · oldest "), refLink(claim.oldest_open_question));
      }
      if (claim.oldest_open_question_at !== null && claim.oldest_open_question_at !== undefined) {
        add(question, el("span", "muted small", " · "), timeNode(claim.oldest_open_question_at));
      }
      items.push(question);
    }
    const commits = Number(claim.commit_count || 0);
    if (commits > 0) {
      const latest = el("span", "claim-signal commit-signal");
      add(latest, el("span", "", `${commits} linked commits`));
      if (claim.latest_commit) add(latest, el("span", "muted small", " · latest "),
        refLink(claim.latest_commit, claim.latest_commit, "mono", { plan: planId(claim.task) }));
      if (claim.latest_commit_at !== null && claim.latest_commit_at !== undefined) {
        add(latest, el("span", "muted small", " · "), timeNode(claim.latest_commit_at));
      }
      items.push(latest);
    }
    return items.length ? add(el("div", "claim-signals"), items) : null;
  }
  function workingCard(claim, task, options = {}) {
    const item = el("article", "working-card"); item.dataset.stale = claim.stale;
    focusKey(item, `working-card:${claim.task}`);
    const now = displayNow(options), ttlSecs = options.ttlSecs;
    add(item,
      add(el("div", "working-card-heading"), refLink(claim.task, task?.title || claim.task, "working-task"),
        claim.stale ? badge("stale") : badge("doing")),
      linkified("p", "working-scope", claim.scope, { plan: planId(claim.task) }),
      add(el("div", "working-meta"), actorAttribution(claim),
        claim.model || claim.effort ? el("span", "", [claim.model, claim.effort].filter(Boolean).join(" · ")) : null,
        refLink(claim.task),
        add(el("span", "working-active"), el("span", "", "Active "), ageNode(claim.last_active))));
    const sourceEntries = options.entries || [];
    const signals = claimSignals(claim, { ...options, now, ttlSecs });
    if (signals) item.append(signals);
    const agents = workerTree(claim, { ...options, entries: sourceEntries, now, ttlSecs });
    item.append(agents);
    return item;
  }
  function taskCard(task, claims, options = {}) {
    const claim = (claims || []).find(item => item.task === task.id && !item.ended_at);
    const item = el("article", "task-card");
    focusKey(item, `task-card:${task.id}`);
    if (claim) item.dataset.stale = claim.stale;
    add(item, refLink(task.id, task.title, ""), el("div", "small muted", task.id),
      claim
        ? add(el("div", "small task-claim"), claimIdentity(claim, options),
          el("span", "muted", " · "),
          ageNode(claim.last_active))
        : (task.assignee ? add(el("div", "small muted"), recipientMark(task.assignee),
          el("span", "", `Assigned: ${task.assignee}`)) : null),
      claim?.stale ? badge("stale") : null);
    if (claim?.model || claim?.effort) {
      item.append(el("div", "small muted", [claim.model, claim.effort].filter(Boolean).join(" · ")));
    }
    if (claim) {
      const signals = claimSignals(claim, { now: options.now, ttlSecs: options.ttlSecs });
      if (signals) item.append(signals);
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
    const preview = entry?.body || item.record.summary || "Open entry";
    add(card,
      add(el("div", "row wrap"), badge(item.type), entry?.plan ? refLink(entry.plan) : null),
      linkified("p", "attention-preview", preview, { plan: entry?.plan }, PREVIEW),
      refLink(entry?.id || item.record.entry, "Read entry →", "attention-entry-link"),
      add(el("p", "attention-author"), entry?.actor ? actorAttribution(entry) : null,
        entry?.via === "outbox" ? el("span", "provenance", " (spooled, unverified)") : null));
    if (entry?.actor) card.querySelector(".attention-author").title = actorName(entry.actor);
    return card;
  }
  function doneFooter(plan, item) {
    const count = item.done_count || 0;
    const done = disclosure(`done:${plan.id}`, `Done (${count})`, "lane-done", "done-summary");
    const recent = (item.recent_done || []).slice(0, 5);
    const more = count - recent.length;
    add(done, add(el("div", "lane-done-content"),
      recent.length ? add(el("div", "recent-done-cards"), recent.map(task => taskCard(task, [])))
        : el("p", "small muted", "No completed tasks yet."),
      link(more > 0 ? `${more} more →` : "View all completed tasks →", "plan",
        { ref: plan.id, tab: "done" }, "small done-history-link")));
    return done;
  }

  return { entryCard, eventList, workingCard, taskCard, workerTree, progressPanel, claimSignals,
    attentionItems, attentionCard, doneFooter };
}
