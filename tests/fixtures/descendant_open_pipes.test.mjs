import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { it } from "node:test";

it("leaves a descendant holding the inherited output pipes", async () => {
  const descendant = spawn(
    process.execPath,
    ["-e", "setInterval(() => {}, 60_000);"],
    { stdio: "inherit" },
  );
  await once(descendant, "spawn");
  descendant.unref();

  assert.ok(descendant.pid);
  process.stdout.write(
    `DESCENDANT_PID=${descendant.pid}\nDESCENDANT_TEST_PID=${process.pid}\n`,
  );
});
