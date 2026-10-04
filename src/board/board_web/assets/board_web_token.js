// Pure launch-URL decision for the board tab bootstrap: resolves the session
// token, whether to persist or flag it, and the history rewrite, if any.
// A bootstrap token is accepted only from a non-route hash that is exactly
// `token=<64 lowercase hex>` — never from a query parameter inside a #/
// route. A differing stored session always wins; the stray token is still
// stripped from the URL and flagged instead of silently replacing it.
export function resolveBootstrapToken({ locationHash, locationHref, storedToken }) {
  const offered = locationHash.startsWith("#/")
    ? null
    : locationHash.slice(1).match(/^token=([0-9a-f]{64})$/)?.[1] || null;
  const launch = new URL(locationHref);
  const launchRef = launch.searchParams.get("ref") || "";
  if (/^(?:P[1-9]\d*(?:@[1-9]\d*)?|E[1-9]\d*)$/.test(launchRef) && (offered || !locationHash)) {
    launch.searchParams.delete("ref"); launch.hash = `/${launchRef}`;
  } else if (offered) launch.hash = "";
  const replacement = launch.pathname + launch.search + launch.hash;
  let token = storedToken, persist = false, mismatch = false;
  if (offered && offered !== storedToken) {
    if (storedToken) mismatch = true;
    else { token = offered; persist = true; }
  }
  return { token, persist, mismatch, replaceUrl: offered || launch.href !== locationHref ? replacement : null };
}
