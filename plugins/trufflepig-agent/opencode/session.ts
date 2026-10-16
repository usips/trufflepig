/** Attach the current OpenCode session to each shell call for Trufflepig attribution. */
import type { Plugin } from "@opencode-ai/plugin";

export const TrufflepigSession: Plugin = async () => ({
	"shell.env": async (input, output) => {
		// Terminal PTYs have no session; leave their environment unchanged.
		if (input.sessionID) output.env.TRUFFLEPIG_OPENCODE_SESSION = input.sessionID;
	},
});
