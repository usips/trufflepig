import { writeNoisyNodeOutput } from "./noisy_node_output.mjs";

await writeNoisyNodeOutput("NOISY_FAILURE");
process.stderr.write("NOISY_FAILURE_USEFUL_DIAGNOSTIC: fixture failed intentionally\n");
process.exitCode = 23;
