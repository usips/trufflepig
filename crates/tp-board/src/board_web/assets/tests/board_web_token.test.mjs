import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { resolveBootstrapToken } from "../state/board_web_token.js";

const A = "a".repeat(64);
const B = "b".repeat(64);

describe("resolveBootstrapToken", () => {
  it("fresh tab accepts an offered token and strips it from the URL", () => {
    const decision = resolveBootstrapToken({
      locationHash: `#token=${A}`,
      locationHref: `https://board.test/#token=${A}`,
      storedToken: "",
    });
    assert.equal(decision.token, A);
    assert.equal(decision.persist, true);
    assert.equal(decision.mismatch, false);
    assert.equal(decision.replaceUrl, "/");
  });

  it("stored session wins on mismatch but the stray token is still stripped", () => {
    const decision = resolveBootstrapToken({
      locationHash: `#token=${B}`,
      locationHref: `https://board.test/#token=${B}`,
      storedToken: A,
    });
    assert.equal(decision.token, A);
    assert.equal(decision.persist, false);
    assert.equal(decision.mismatch, true);
    assert.equal(decision.replaceUrl, "/");
  });

  it("re-offered identical token rewrites the URL without persisting", () => {
    const decision = resolveBootstrapToken({
      locationHash: `#token=${A}`,
      locationHref: `https://board.test/#token=${A}`,
      storedToken: A,
    });
    assert.equal(decision.token, A);
    assert.equal(decision.persist, false);
    assert.equal(decision.mismatch, false);
    assert.equal(decision.replaceUrl, "/");
  });

  it("route hash leaves the session alone", () => {
    const decision = resolveBootstrapToken({
      locationHash: "#/P1",
      locationHref: "https://board.test/?ref=P2#/P1",
      storedToken: A,
    });
    assert.equal(decision.token, A);
    assert.equal(decision.persist, false);
    assert.equal(decision.mismatch, false);
    assert.equal(decision.replaceUrl, null);
  });

  it("launch ref with empty hash rewrites to the route", () => {
    const decision = resolveBootstrapToken({
      locationHash: "",
      locationHref: "https://board.test/?ref=P1",
      storedToken: "",
    });
    assert.equal(decision.token, "");
    assert.equal(decision.persist, false);
    assert.equal(decision.mismatch, false);
    assert.equal(decision.replaceUrl, "/#/P1");
  });

  it("offered token with launch ref accepts the token and routes the ref", () => {
    const decision = resolveBootstrapToken({
      locationHash: `#token=${A}`,
      locationHref: `https://board.test/?ref=E5#token=${A}`,
      storedToken: "",
    });
    assert.equal(decision.token, A);
    assert.equal(decision.persist, true);
    assert.equal(decision.mismatch, false);
    assert.equal(decision.replaceUrl, "/#/E5");
  });

  it("malformed tokens are ignored", () => {
    for (const hash of [
      `#token=${A.toUpperCase()}`,
      `#token=${A.slice(1)}`,
      `#token=${A}0`,
      `#/token=${A}`,
      "",
    ]) {
      const decision = resolveBootstrapToken({
        locationHash: hash,
        locationHref: `https://board.test/${hash}`,
        storedToken: "",
      });
      assert.equal(decision.token, "", hash);
      assert.equal(decision.persist, false, hash);
      assert.equal(decision.mismatch, false, hash);
      assert.equal(decision.replaceUrl, null, hash);
    }
  });
});
