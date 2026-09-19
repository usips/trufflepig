/** Attach the current omp session to each bash invocation, regardless of cwd. */
import type { BashToolCallEvent, ExtensionAPI } from "@oh-my-pi/pi-coding-agent";

export default function (pi: ExtensionAPI) {
	pi.on("tool_call", (event, ctx) => {
		if (event.toolName !== "bash") return;
		const input = (event as BashToolCallEvent).input;
		const session = ctx.sessionManager.getSessionId();
		// A throwing handler blocks every bash call, so never spread a missing input.
		if (!input || typeof input !== "object" || !session) return;
		return {
			input: {
				...input,
				env: { ...input.env, TRUFFLEPIG_OMP_SESSION: session },
			},
		};
	});
}
