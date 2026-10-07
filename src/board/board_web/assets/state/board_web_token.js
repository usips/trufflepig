// Only an exact token=<64 lowercase hex> hash offers a bootstrap token.
// Routes beginning #/ never offer tokens, including token query parameters.
export function offeredTokenFromHash(locationHash) {
  return locationHash.startsWith("#/")
    ? null
    : locationHash.slice(1).match(/^token=([0-9a-f]{64})$/)?.[1] || null;
}

// Fresh-token adoption: a tab whose stored token failed with 401/403, or a
// tab holding no token at all, adopts a URL token; a live tab keeps its
// session (never-overwrite).
export function resolveAdoptionToken({ locationHash, expired }) {
  return expired ? offeredTokenFromHash(locationHash) : null;
}

// Keep an existing session and retain differing offers until expiry.
// Strip valid token hashes; move launch refs for token launches or absent hashes.
// Without a stored session, persist and adopt the offered token.
export function resolveBootstrapToken({ locationHash, locationHref, storedToken }) {
  const offered = offeredTokenFromHash(locationHash);
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
  const pending = mismatch ? offered : null;
  return { token, persist, mismatch, pending,
    replaceUrl: offered || launch.href !== locationHref ? replacement : null };
}
