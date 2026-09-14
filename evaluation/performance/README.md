# CLI performance smoke

`python3 evaluation/performance/run_smoke.py` records bounded local measurements
in [cli_smoke.json](cli_smoke.json). It reads the current `lunatic` and `tgstation`
working trees and removes its dedicated index caches afterwards. Set
`TRUFFLEPIG_SMOKE_BINARY` to use a frozen copy of the executable. Each command
has a 120-second deadline; an unsuccessful initial index skips dependent runs.

The binary is a debug build. A cold index means an empty Trufflepig cache, with
the operating-system page cache uncontrolled. The warm index uses the preceding
index cache in a fresh process. Ordinary search is lexical, runs in a fresh
process with `--no-daemon`, and includes repository reconciliation. These timings
therefore do not measure resident-daemon query latency. Avoid replacing the
binary or modifying either corpus during the script.

Elapsed time comes from Python's monotonic clock. Linux `wait4` supplies each
child's CPU times and peak resident memory in KiB; `/usr/bin/time` is unavailable
on this host. Cache measurements sum apparent file sizes and allocated blocks
after each child exits, excluding captured command output. They are persistent
disk measurements, not peak temporary disk usage.

Commit IDs and dirty flags describe provenance. Inputs are current working-tree
files, not frozen evaluation snapshots. Each operation has one sample, and other
development processes may compete for resources. No latency confidence interval,
retrieval recall, task success, or performance guarantee follows from this smoke.
Responses preserve coverage, errors, and timeout outcomes so incomplete work
remains visible. The corpora are development inputs from
[the evaluation manifest](../manifests/corpora.json).

The recorded debug run completes all six commands within their deadlines:

| Corpus | Cold index | Warm index | Search with reconciliation | Cold peak RSS | Index disk |
| --- | ---: | ---: | ---: | ---: | ---: |
| lunatic | 112.48 s | 21.92 s | 21.76 s | 88.43 MiB | 450.32 MiB |
| tgstation | 102.29 s | 52.83 s | 52.58 s | 98.04 MiB | 1261.34 MiB |

Lunatic reports 2,480 indexed files, 9 excluded files, and 14 parsing failures.
Tgstation reports 11,620 indexed files, 6,573 excluded files, and 849 parsing
failures. Both report zero walk failures and zero truncated files. Parsing
failures prevent treating structural coverage as complete. Persistent index
storage is approximately 21.5 and 17.1 times indexed source bytes respectively;
these measurements expose costs requiring optimization. The sampled exact-name
queries return a source definition first, which establishes neither recall nor
general ranking quality.

## Background history smoke

Run `python3 evaluation/performance/run_history_smoke.py --trufflepig target/debug/trufflepig`.
The fixed fixture creates 270 first-parent commits in one Rust file, then runs
16 exact-symbol queries across five client slots while history indexing proceeds.
It checks query results, captured-view completion, and daemon shutdown. Scratch
uses `TMPDIR` or a cache directory beneath the user's home, and is removed.

[Recorded observations](history_smoke_result.json) include complete client elapsed
times. This small synthetic run establishes concurrent progress only; it is not
a throughput target, large-repository latency result, or agent effectiveness trial.
