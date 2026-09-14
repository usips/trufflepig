#!/usr/bin/env python3
"""Observe one fixed synthetic workload; this is not an agent effectiveness trial."""
import argparse, concurrent.futures, json, os, pathlib, subprocess, tempfile, time
parser = argparse.ArgumentParser(description='Synthetic background history and foreground-query smoke check')
parser.add_argument('--trufflepig', default='target/debug/trufflepig')
options = parser.parse_args()
exe = str(pathlib.Path(options.trufflepig).resolve())
scratch_base = pathlib.Path(os.environ.get('TMPDIR', pathlib.Path.home() / '.cache' / 'trufflepig-eval'))
scratch_base.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix='tpl-', dir=scratch_base) as scratch:
    p = pathlib.Path(scratch)
    root = p / 'r'
    cache = p / 'c'
    root.mkdir()
    subprocess.run(['git', 'init', '-q', '-b', 'main', str(root)], check=True)
    chunks = []
    now = int(time.time())
    for n in range(270):
        body = f'pub fn observed() -> usize {{ {n} }}\n'.encode()
        chunks.append(f'blob\nmark :{n * 2 + 1}\ndata {len(body)}\n'.encode() + body + b'\n')
        chunks.append(f'commit refs/heads/main\nmark :{n * 2 + 2}\ncommitter Fixture <fixture@example.test> {now + n} +0000\ndata 6\nchange\n'.encode())
        if n:
            chunks.append(f'from :{n * 2}\n'.encode())
        chunks.append(f'M 100644 :{n * 2 + 1} a.rs\n\n'.encode())
    subprocess.run(['git', '-C', str(root), 'fast-import', '--quiet'], input=b''.join(chunks), check=True)
    subprocess.run(['git', '-C', str(root), 'reset', '--hard', '-q', 'HEAD'], check=True)
    base = [exe, '--root', str(root), '--cache', str(cache), '--diagnostics', 'off']

    def run(*args):
        began = time.monotonic()
        r = subprocess.run(base + list(args), capture_output=True, text=True, timeout=30)
        assert r.returncode == 0, (args, r.stdout, r.stderr)
        return (json.loads(r.stdout), time.monotonic() - began)
    try:
        run('search', 'sym:observed')
        statuses = [run('hist-status')[0]]
        with concurrent.futures.ThreadPoolExecutor(max_workers=5) as pool:
            calls = [pool.submit(run, 'search', 'sym:observed') for _ in range(16)]
            waiter = pool.submit(run, 'hist-index', '--wait')
            results = [f.result() for f in calls]
            waited = waiter.result()
        statuses.append(run('hist-status')[0])
        assert statuses[-1]['visited'] == 270 and statuses[-1]['complete']
        assert all((result['hits'][0]['name'] == 'observed' for result, _ in results))
        print(json.dumps({'schema_version': 1, 'fixture': 'single-file-first-parent-v1', 'budget_tokens': 600, 'timing_scope': 'complete client invocation including startup and stdout capture', 'commits': 270, 'queries': 16, 'parallel_clients': 5, 'all_queries_correct': True, 'initial_history': statuses[0], 'final_history': statuses[-1], 'query_elapsed_seconds': [round(elapsed, 4) for _, elapsed in results], 'wait_elapsed_seconds': round(waited[1], 4)}))
    finally:
        run('stop')
    time.sleep(2)
