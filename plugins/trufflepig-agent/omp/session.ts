/**
 * Trufflepig session attribution for omp.
 *
 * On session_start, records the session id for the session's working directory
 * so trufflepig-agent can tag every call with it. The marker key must match
 * `omp_marker_key()` in trufflepig_runtime.py; the marker lands at
 * $XDG_STATE_HOME/trufflepig/agent-sessions/omp/<key>. Attribution must never
 * block session startup, so all failures are swallowed.
 *
 * Installed to ~/.omp/agent/extensions/, where omp auto-discovers extensions.
 */
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { createHash } from "node:crypto";

interface SessionContext {
	cwd: string;
	sessionManager: { getSessionId?: () => string | undefined };
}

export default function (pi: {
	on(event: "session_start", handler: (event: unknown, ctx: SessionContext) => unknown): void;
}) {
	pi.on("session_start", async (_event, ctx) => {
		try {
			const session = ctx.sessionManager.getSessionId?.();
			if (typeof session !== "string" || session.length === 0) return;
			const state = process.env.XDG_STATE_HOME || path.join(os.homedir(), ".local", "state");
			const dir = path.join(state, "trufflepig", "agent-sessions", "omp");
			// SHA-256(cwd) truncated to 12 hex: Bun/Node expose only full-length
			// blake2s (first six bytes differ from Python's digest_size=6), so
			// blake2s is not portable here; SHA-256 truncation is identical in
			// Python, Bun, and Node.
			const key = createHash("sha256").update(ctx.cwd).digest("hex").slice(0, 12);
			fs.mkdirSync(dir, { recursive: true });
			fs.writeFileSync(path.join(dir, key), session + "\n");
		} catch {
			// A missing session id or unwritable state must not stop the session.
		}
	});
}
