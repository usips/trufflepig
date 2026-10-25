import { once } from "node:events";

const NOISE_BYTES_PER_STREAM = 1024 * 1024;
const CHUNK_BYTES = 16 * 1024;
const stdoutChunk = Buffer.alloc(CHUNK_BYTES, 0x6f);
const stderrChunk = Buffer.alloc(CHUNK_BYTES, 0x65);

async function writeChunk(stream, chunk) {
  if (!stream.write(chunk)) {
    await once(stream, "drain");
  }
}

async function writeNoisyStream(stream, channel, chunk) {
  await writeChunk(stream, `${channel}_BEGIN\n`);

  for (let bytes = 0; bytes < NOISE_BYTES_PER_STREAM; bytes += chunk.length) {
    await writeChunk(stream, chunk);
  }

  await writeChunk(stream, `${channel}_END\n`);
}

export async function writeNoisyNodeOutput(prefix) {
  await Promise.all([
    writeNoisyStream(process.stdout, `${prefix}_STDOUT`, stdoutChunk),
    writeNoisyStream(process.stderr, `${prefix}_STDERR`, stderrChunk),
  ]);
}
