import { spawn } from "node:child_process";
import { once } from "node:events";
import { writeFileSync } from "node:fs";

const escaped = spawn(
  process.execPath,
  ["-e", "setInterval(() => {}, 60_000);"],
  { detached: true, stdio: "inherit" },
);
await once(escaped, "spawn");
escaped.unref();
writeFileSync(new URL("./escaped-pipe.pid", import.meta.url), String(escaped.pid));
process.stdout.write(`ESCAPED_PIPE_PID=${escaped.pid}\n`);
