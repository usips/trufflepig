process.stdout.write("STDOUT_TOO_OLD\nnot ok 1 - early stdout failure\n");
process.stderr.write("STDERR_TOO_OLD\nnot ok 2 - early stderr failure\n");
for (let number = 0; number < 250; number += 1) {
  process.stdout.write(`stdout tail ${number}\n`);
  process.stderr.write(`stderr tail ${number}\n`);
}
process.exitCode = 23;
