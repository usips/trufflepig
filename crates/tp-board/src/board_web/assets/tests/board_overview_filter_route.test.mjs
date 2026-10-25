import { afterEach, it } from "node:test";
import assert from "node:assert/strict";
import { createBoardRouting, routeFromLocation } from "../board_routing.js";
import { createBoardDom } from "../board_dom.js";
import { installDomShim, resetDomShim } from "./support/dom_shim.mjs";

installDomShim();
afterEach(() => resetDomShim());

it("overview filter choice survives navigation and can be reset", () => {
  const previous = globalThis.location;
  try {
    globalThis.location = { hash: "#/overview?showInactive=1", search: "" };
    assert.equal(routeFromLocation().showInactive, "1");
    globalThis.location = { hash: "#/P8", search: "" };
    assert.equal(routeFromLocation().showInactive, "1");
    globalThis.location = { hash: "#/overview?showInactive=0", search: "" };
    assert.equal(routeFromLocation().showInactive, "0");
    globalThis.location = { hash: "#/P8", search: "" };
    assert.equal(routeFromLocation().showInactive, "0");
  } finally {
    if (previous === undefined) delete globalThis.location;
    else globalThis.location = previous;
  }
});

it("allows filter navigation to reload without moving focus to main", () => {
  const originals = ["location", "history"].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)]);
  try {
    globalThis.location = { hash: "#/overview", search: "" };
    const history = {
      state: null,
      pushState(_state, _title, url) { this.state = _state; location.hash = url.slice(url.indexOf("#")); },
      replaceState(_state, _title, url) { this.state = _state; location.hash = url.slice(url.indexOf("#")); },
    };
    globalThis.history = history;
    const state = { route: routeFromLocation(), projects: [], navigation: 0 };
    const dom = createBoardDom(state), focusRequests = [];
    const routing = createBoardRouting({ state, routeUrl: dom.routeUrl, notice: () => {},
      load: focus => focusRequests.push(focus) });

    routing.navigate("overview", { ...state.route, showInactive: "1" }, false, false);
    assert.deepEqual(focusRequests, [false]);
    assert.equal(state.route.showInactive, "1");
    routing.navigate("overview", { ...state.route, showInactive: "0" });
    assert.deepEqual(focusRequests, [false, true], "ordinary navigation still focuses the new page");
  } finally {
    for (const [key, descriptor] of originals) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete globalThis[key];
    }
  }
});
