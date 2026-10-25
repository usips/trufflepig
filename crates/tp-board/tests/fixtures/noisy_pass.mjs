import { writeNoisyNodeOutput } from "./noisy_node_output.mjs";

await writeNoisyNodeOutput("NOISY_PASS");
process.stdout.write("NOISY_PASS_EXIT_OK\n");
