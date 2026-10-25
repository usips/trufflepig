// Board reference grammar and its hash routes, shared by refLink and any text
// linkifier. `boardRefRoute` returns `{ view, params }` for routeUrl, or null for
// text that is not a complete reference. Anchor helpers name the `data-anchor`
// targets that a route's `focus=` parameter scrolls to and highlights.
export const BOARD_REF_PATTERNS = Object.freeze({
  entry: /^E[1-9]\d*$/,
  task: /^P[1-9]\d*\.\d+$/,
  worker: /^(P[1-9]\d*\.\d+)\/([a-z0-9_-]{1,32})$/,
  plan: /^P[1-9]\d*(?:@\d+(?:\.\.(?:\d+)?)?)?$/,
  commit: /^(?:[a-fA-F0-9]{40}|[a-fA-F0-9]{64})$/,
});

export function entryAnchor(entry) { return `entry-${entry}`; }
export function workerAnchor(task, handle) { return `worker-${task}-${handle}`; }
export function commitAnchor(oid) { return `commit-${String(oid).toLowerCase()}`; }
function planOf(ref) { return String(ref).match(/^P[1-9]\d*/)?.[0] || ""; }

// context: { plan, repo_key } give a commit its plan's Commits tab; `focus`
// names an anchor on the task route (entry-E#, worker-P#.N-handle).
export function boardRefRoute(ref, context = {}) {
  const text = String(ref ?? "");
  const focus = context.focus || undefined;
  if (BOARD_REF_PATTERNS.entry.test(text)) return { view: "entry", params: { ref: text } };
  if (BOARD_REF_PATTERNS.task.test(text)) {
    return { view: "plan", params: { ref: planOf(text), tab: "tasks", task: text, focus } };
  }
  const worker = BOARD_REF_PATTERNS.worker.exec(text);
  if (worker) {
    return { view: "plan", params: {
      ref: planOf(text), tab: "tasks", task: worker[1], focus: workerAnchor(worker[1], worker[2]),
    } };
  }
  if (BOARD_REF_PATTERNS.plan.test(text)) return { view: "plan", params: { ref: text } };
  if (BOARD_REF_PATTERNS.commit.test(text)) {
    const oid = text.toLowerCase();
    return context.plan
      ? { view: "plan", params: { ref: context.plan, tab: "commits", oid, repo: context.repo_key } }
      : { view: "search", params: { q: text } };
  }
  return null;
}
