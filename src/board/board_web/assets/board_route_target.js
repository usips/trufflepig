// A navigation's target: `focus=` names a `data-anchor` (or element id), then
// `heading`, `oid` (commit-<oid>), and `task` fall back in that order. Revealing
// opens enclosing disclosures, scrolls, and marks the target `data-targeted`
// (a fading outline); live refreshes never call this, so they keep the reader's place.
import { commitAnchor } from "./board_refs.js";

export function routeTargetId(route) {
  return route.focus || route.heading || (route.oid ? commitAnchor(route.oid) : "") || route.task || "";
}

export function findRouteTarget(root, id) {
  if (!id) return null;
  return root.querySelector(`[data-anchor="${CSS.escape(id)}"]`) || document.getElementById(id);
}

export function revealRouteTarget(root, route) {
  for (const marked of root.querySelectorAll("[data-targeted]")) delete marked.dataset.targeted;
  const target = findRouteTarget(root, routeTargetId(route));
  if (!target) return null;
  for (let at = target.parentElement; at && at !== root; at = at.parentElement) {
    if (at.tagName === "DETAILS" && !at.open) { at.open = true; at.boardLoad?.(); }
  }
  void target.offsetWidth; // Restart the fade when the same element is targeted again.
  target.dataset.targeted = "true";
  target.scrollIntoView?.({ block: "start" });
  return target;
}
