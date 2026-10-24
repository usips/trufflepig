// Overview's dense Working now rows: one native disclosure per open claim.
// The summary is a single line (two on narrow screens) whose full scope and
// identity live in titles and mark labels; the body lists live workers only.
// Claim pages and task details keep the full card and agent tree (board_cards.js).
import { actorMark, harnessVendor } from "./marks/agent_marks.js";

export function liveWorker(worker) { return !worker?.ended_at && !worker?.state; }

// Stale claims first, then the most recently active.
export function workingClaimOrder(left, right) {
  if (Boolean(left.stale) !== Boolean(right.stale)) return left.stale ? -1 : 1;
  return Number(right.last_active || 0) - Number(left.last_active || 0);
}

export function createBoardWorkingRows({ dom }) {
  const { el, add, refLink, badge, timeNode, disclosure, actorName } = dom;

  function slot(className, visible, spoken) {
    const item = el("span", className);
    if (!visible) return item;
    item.title = spoken;
    const shown = el("span", "", visible); shown.setAttribute("aria-hidden", "true");
    return add(item, shown, el("span", "sr-only", spoken));
  }
  function agentLabel(claim) {
    const harness = claim.actor?.harness;
    const showHarness = harness && (harness !== claim.vendor || !claim.model);
    return [showHarness ? harness : null, claim.model].filter(Boolean).join(" ") || "unclaimed";
  }
  function workerLine(worker, task) {
    const handle = worker.address?.handle || "worker";
    const actor = worker.actor || null;
    const scope = el("span", "working-worker-scope", worker.scope || "");
    scope.title = worker.scope || "";
    return add(el("li", "working-worker"),
      actorMark({ ...worker, actor, vendor: harnessVendor(actor?.harness) }),
      refLink(`${worker.address?.task || task}/${handle}`, `/${handle}`, "mono working-worker-handle"),
      el("span", "working-worker-role muted", worker.role || "worker"),
      scope, timeNode(worker.last_active));
  }
  function workingRow(claim, task) {
    const workers = Array.isArray(claim.workers) ? claim.workers : [];
    const live = workers.filter(liveWorker)
      .sort((left, right) => Number(right.last_active || 0) - Number(left.last_active || 0));
    const ended = workers.length - live.length, omitted = Number(claim.workers_omitted || 0);
    const row = disclosure(`working:${claim.task}`, "", "working-row");
    row.dataset.stale = String(Boolean(claim.stale));
    const summary = row.querySelector("summary");
    summary.className = "working-row-summary";

    const title = task?.title || "";
    const taskLink = refLink(claim.task, "", "working-row-task");
    add(taskLink, el("span", "mono working-row-id", claim.task),
      title ? el("span", "working-row-title", title) : null);
    taskLink.title = title ? `${claim.task} · ${title}` : claim.task;
    const scope = el("span", "working-row-scope", claim.scope || "");
    scope.title = claim.scope || "";
    const identity = [actorName(claim.actor), [claim.model, claim.effort].filter(Boolean).join(" · ")]
      .filter(Boolean).join(" · ");
    const agent = add(el("span", "working-row-agent"), actorMark(claim), el("span", "", agentLabel(claim)));
    agent.title = identity;
    const step = claim.progress?.step?.value;
    const hasStep = step && step.current !== undefined && step.total !== undefined;
    const questions = Number(claim.open_questions || 0);
    add(summary, taskLink, scope, agent,
      slot("working-row-step", hasStep ? `${step.current}/${step.total}` : "",
        hasStep ? `Step ${step.current} of ${step.total}` : ""),
      slot("working-row-questions", questions > 0 ? `⚑${questions}` : "",
        `${questions} open question${questions === 1 ? "" : "s"}`),
      slot("working-row-workers", live.length ? `👥${live.length}` : "",
        `${live.length} live worker${live.length === 1 ? "" : "s"}`),
      add(el("span", "working-row-age"), timeNode(claim.last_active)),
      add(el("span", "working-row-stale"), claim.stale ? badge("stale") : null));

    const body = el("div", "working-row-body");
    body.append(live.length
      ? add(el("ol", "working-workers"), live.map(worker => workerLine(worker, claim.task)))
      : el("p", "muted", "No live workers."));
    body.append(add(el("p", "working-row-footer"),
      ended ? el("span", "muted", `(${ended} ended)`) : null,
      omitted ? el("span", "muted", `${omitted} more omitted`) : null,
      refLink(claim.task, "task details →", "working-row-details")));
    row.append(body);
    return row;
  }
  return { workingRow };
}
