/**
 * Trufflepig session attribution for omp.
 *
 * Writes the omp session id to $XDG_STATE_HOME/trufflepig/agent-sessions/omp/<key>
 * so trufflepig-agent can tag every call. The key is the cwd_key() contract in
 * bin/trufflepig-agent: SHA-256 of the cwd, first twelve hex digits. Written on
 * session_start and again on session_switch (/new, /resume, fork, handoff).
 * Attribution must never block the session, so every failure is swallowed.
 */
import { createHash } from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";

function recordSession(ctx: ExtensionContext): void {
	try {
		const session = ctx.sessionManager.getSessionId();
		if (!session) return;
		const state = process.env.XDG_STATE_HOME || path.join(os.homedir(), ".local", "state");
		const dir = path.join(state, "trufflepig", "agent-sessions", "omp");
		const key = createHash("sha256").update(ctx.cwd).digest("hex").slice(0, 12);
		fs.mkdirSync(dir, { recursive: true });
		fs.writeFileSync(path.join(dir, key), `${session}\n`);
	} catch {
		// An unwritable state directory must not stop the session.
	}
}

export default function (pi: ExtensionAPI) {
	pi.on("session_start", (_event, ctx) => recordSession(ctx));
	pi.on("session_switch", (_event, ctx) => recordSession(ctx));
}
