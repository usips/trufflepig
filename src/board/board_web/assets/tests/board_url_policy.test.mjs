import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { boardAuthorityBlocked, boardUrlAllowed } from "../board_url_policy.js";

describe("shared URL policy vectors", () => {
  it("blocks loopback and IP-literal authorities after decoding", () => {
    for (const authority of ["localhost:7341", "user@127.0.0.1", "%6c%6fcalhost", "[::1]:80", "１２７.0.0.1"]) {
      assert.equal(boardAuthorityBlocked(authority), true, authority);
    }
    for (const authority of ["example.test:7341", "localhost.example.com", "0x.example.test"]) {
      assert.equal(boardAuthorityBlocked(authority), false, authority);
    }
  });

  it("match the Rust plan_markup allowed_url vectors", async () => {
    const vectors = JSON.parse(await readFile(new URL("./support/board_url_policy_vectors.json",
      import.meta.url), "utf8"));
    for (const [key, expected] of [["denied", false], ["allowed", true]]) {
      assert.ok(vectors[key].length > 10);
      for (const url of vectors[key]) assert.equal(boardUrlAllowed(url), expected, `${key}: ${JSON.stringify(url)}`);
    }
  });
});
