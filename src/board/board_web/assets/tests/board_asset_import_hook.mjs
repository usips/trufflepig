// Resolves the board's root-absolute asset imports ("/board_views.js") to the
// assets directory, so node tests can exercise the real view builders.
export async function resolve(specifier, context, nextResolve) {
  if (specifier.startsWith("/")) {
    return nextResolve(new URL(`..${specifier}`, import.meta.url).href, context);
  }
  return nextResolve(specifier, context);
}
