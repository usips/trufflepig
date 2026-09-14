#!/usr/bin/env python3
"""Bounded workspace lifecycle observations; not an agent effectiveness trial."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import tempfile
import threading
import time


def fixture_processes(binary, scratch):
    processes = []
    for process in Path('/proc').glob('[0-9]*'):
        try:
            args = process.joinpath('cmdline').read_bytes().split(b'\0')
            if not args or os.fsdecode(args[0]) != str(binary):
                continue
            if not any(os.fsdecode(arg).startswith(str(scratch) + '/') for arg in args):
                continue
            status = process.joinpath('status').read_text()
            if '\nState:\tZ' in status:
                continue
            rss = next((int(line.split()[1]) * 1024 for line in status.splitlines()
                        if line.startswith('VmRSS:')), None)
            processes.append({'pid': int(process.name), 'rss_bytes': rss,
                              'command': [os.fsdecode(arg) for arg in args if arg]})
        except (OSError, ValueError):
            continue
    return processes


def index_identities(cache):
    identities = {}
    for database in cache.glob('members/*/index.sqlite3'):
        with sqlite3.connect(f'file:{database}?mode=ro', uri=True, timeout=2) as conn:
            meta = dict(conn.execute("SELECT key,value FROM meta WHERE key IN ('root','index_epoch','generation')"))
        identities[meta['root']] = {'cache': str(database.parent), **meta}
    return identities


def git_fixture(root, symbol):
    root.mkdir()
    root.joinpath('subdir').mkdir()
    subprocess.run(['git', 'init', '-q', '-b', 'main', str(root)], check=True)
    chunks = []
    timestamp = int(time.time()) - 10
    for number in range(2):
        body = f'pub fn {symbol}() -> usize {{ {number + 1} }}\n'.encode()
        chunks.append(f'blob\nmark :{number * 2 + 1}\ndata {len(body)}\n'.encode() + body + b'\n')
        chunks.append(f'commit refs/heads/main\nmark :{number * 2 + 2}\ncommitter Fixture <fixture@example.test> {timestamp + number} +0000\ndata 6\nchange\n'.encode())
        if number:
            chunks.append(b'from :2\n')
        chunks.append(f'M 100644 :{number * 2 + 1} a.rs\n\n'.encode())
    subprocess.run(['git', '-C', str(root), 'fast-import', '--quiet'], input=b''.join(chunks), check=True)
    subprocess.run(['git', '-C', str(root), 'reset', '--hard', '-q', 'HEAD'], check=True)


def run_smoke(binary, report):
    base = Path(os.environ.get('TMPDIR', Path.home() / '.cache/codex-tmp')).resolve()
    if base == Path('/tmp') or Path('/tmp') in base.parents:
        raise ValueError('TMPDIR must not use RAM-backed /tmp')
    base.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='workspace-smoke-', dir=base) as directory:
        scratch = Path(directory)
        engine, pack, cache = scratch / 'engine', scratch / 'pack', scratch / 'cache'
        git_fixture(engine, 'EngineAnchor')
        git_fixture(pack, 'ForeignAnchor')
        configs = [scratch / 'trufflepig.workspace.toml', scratch / 'second.toml']
        for number, config in enumerate(configs):
            config.write_text(f'[workspace]\nname="smoke{number}"\n[members.engine]\npath="engine"\n[members.pack]\npath="pack"\n')
        calls = report['calls']
        stop_editing = threading.Event()
        editor = None

        def invoke(label, words, config=configs[0], selected_cache=cache,
                   cwd=engine, allow_error=False):
            command = [str(binary), '--workspace', str(config), '--cache', str(selected_cache),
                       '--diagnostics', 'off', '--budget', '600', *words]
            started = time.monotonic()
            completed = subprocess.run(command, cwd=cwd, capture_output=True, text=True, timeout=35)
            try:
                response = json.loads(completed.stdout)
            except json.JSONDecodeError:
                response = {'unparsed_stdout': completed.stdout}
            calls.append({'label': label, 'arguments': words, 'cwd': str(cwd),
                          'elapsed_seconds': round(time.monotonic() - started, 6),
                          'exit_code': completed.returncode, 'response': response,
                          'stderr': completed.stderr,
                          'resident_process_samples': fixture_processes(binary, scratch)})
            if not allow_error:
                assert completed.returncode == 0, (label, response, completed.stderr)
            return response

        def poll(label, words, predicate, timeout=15):
            deadline = time.monotonic() + timeout
            while True:
                value = invoke(label, words, allow_error=True)
                if predicate(value):
                    return value
                assert time.monotonic() < deadline, (label, value)
                time.sleep(0.2)

        def cleanup():
            stop_editing.set()
            if editor:
                editor.join(timeout=3)
            for config in configs:
                invoke('cleanup_coordinator', ['stop'], config=config, allow_error=True)
            for identity in index_identities(cache).values():
                subprocess.run([str(binary), '--no-workspace', '--root', identity['root'],
                                '--cache', identity['cache'], '--diagnostics', 'off', 'stop'],
                               capture_output=True, timeout=10)
            deadline = time.monotonic() + 20
            while fixture_processes(binary, scratch) and time.monotonic() < deadline:
                time.sleep(0.2)
            remaining = fixture_processes(binary, scratch)
            report['cleanup'] = {'forced_fixture_pids': [p['pid'] for p in remaining]}
            for process in remaining:
                try:
                    os.kill(process['pid'], signal.SIGTERM)
                except ProcessLookupError:
                    pass
            deadline = time.monotonic() + 3
            while fixture_processes(binary, scratch) and time.monotonic() < deadline:
                time.sleep(0.1)
            report['cleanup']['remaining_processes'] = fixture_processes(binary, scratch)
            assert not report['cleanup']['remaining_processes'], 'fixture processes survived cleanup'

        try:
            before = fixture_processes(binary, scratch)
            local = invoke('no_daemon', ['--no-daemon', 'sym:ForeignAnchor'],
                           selected_cache=scratch / 'foreground-cache', cwd=engine / 'subdir')
            assert local['hits'][0]['member'] == 'pack'
            after = fixture_processes(binary, scratch)
            assert before == after == [], (before, after)
            report['checks']['no_daemon_launches_no_processes'] = True

            first = invoke('cold_start', ['sym:ForeignAnchor'], allow_error=True)
            report['cold_start_state'] = first.get('coverage', first.get('error'))
            ready = poll('warm_up', ['sym:ForeignAnchor'], lambda value:
                         any(hit.get('member') == 'pack' for hit in value.get('hits', [])) and
                         all(member.get('state') == 'searched' for member in value.get('coverage', [])))
            foreign_handle = next(hit['handle'] for hit in ready['hits'] if hit['member'] == 'pack')
            identities = index_identities(cache)
            report['initial_index_identities'] = identities
            shown = invoke('foreign_show_from_subdir', ['show', foreign_handle], cwd=engine / 'subdir')
            assert shown['member'] == 'pack' and 'ForeignAnchor' in str(shown['lines'])
            report['checks']['foreign_show_from_member_subdirectory'] = True

            edits = []

            def edit_source():
                number = 0
                while not stop_editing.wait(0.15):
                    engine.joinpath('edit.rs').write_text(f'pub fn Mutation{number}() {{}}\n')
                    edits.append(time.monotonic())
                    number += 1
                engine.joinpath('edit.rs').write_text('pub fn MutationFinal() {}\n')

            editor = threading.Thread(target=edit_source)
            editor.start()
            report['edit_observations'] = []
            for number in range(8):
                edits_before = len(edits)
                found = invoke(f'query_during_edits_{number}', ['sym:ForeignAnchor'])
                assert found['hits'][0]['member'] == 'pack'
                edits_after = len(edits)
                report['edit_observations'].append({'query': number, 'edits_before': edits_before,
                                                    'edits_after': edits_after})
                assert edits_after > edits_before, 'query did not overlap any observed fixture edit'
            stop_editing.set()
            editor.join(timeout=3)
            observed = poll('reconcile_final_edit', ['sym:MutationFinal'], lambda value:
                            any(hit.get('name') == 'MutationFinal' for hit in value.get('hits', [])))
            assert observed['hits'][0]['member'] == 'engine'
            report['checks']['queries_and_edit_reconciliation'] = True

            invoke('coordinator_stop', ['stop'])
            shown = invoke('coordinator_restart_show', ['show', foreign_handle], cwd=pack / 'subdir')
            assert shown['member'] == 'pack' and 'ForeignAnchor' in str(shown['lines'])
            report['checks']['handles_survive_coordinator_restart'] = True
            invoke('overlapping_workspace', ['sym:ForeignAnchor'], config=configs[1])
            later = index_identities(cache)
            assert set(later) == set(identities)
            assert all(later[root]['index_epoch'] == old['index_epoch']
                       for root, old in identities.items())
            report['checks']['overlapping_workspace_reuses_index_identity'] = True
            report['later_index_identities'] = later

            history = invoke('history_wait', ['--member', 'pack', 'hist-index', '--wait'])
            assert history['complete'] is True and history['visited'] == 2, history
            status = invoke('history_status', ['--member', 'pack', 'hist-status'])
            assert status['complete'] is True and status['tip'] == history['tip']
            diff = invoke('scoped_diff', ['--member', 'pack', 'diff', 'HEAD', '--target', 'path:a.rs'])
            assert diff['hits'] and all(hit.get('member') == 'pack' for hit in diff['hits']), diff
            before = invoke('historical_before_from_other_member',
                            ['show', diff['hits'][0]['handle'], '--side', 'before'], cwd=engine / 'subdir')
            assert before['member'] == 'pack' and '1' in str(before['lines']), before
            report['checks']['history_wait_and_immutable_owner_read'] = True
        finally:
            cleanup()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', default='target/debug/trufflepig')
    parser.add_argument('--output', default='evaluation/performance/workspace_smoke_result.json')
    options = parser.parse_args()
    binary = Path(options.binary).resolve()
    report = {'schema_version': 1, 'fixture': 'two-roots-two-commits-workspace-v1',
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'git_version': subprocess.check_output(['git', '--version'], text=True).strip(),
              'budget_tokens': 600, 'calls': [], 'checks': {},
              'timing_scope': 'Complete client invocation with captured stdout; debug binary',
              'rss_scope': 'Point-in-time /proc VmRSS of fixture daemons/workers; not peak RSS',
              'interpretation': 'Synthetic mechanism checks; no latency target or agent effectiveness conclusion',
              'unmeasured': ['real model unload/reload', 'large-repository throughput', 'peak process memory']}
    try:
        run_smoke(binary, report)
        report['passed'] = True
    except Exception as error:
        report['passed'] = False
        report['failure'] = repr(error)
    # Keep each complete invocation together; large raw JSON responses remain parseable.
    with Path(options.output).open('w') as output:
        output.write('{\n')
        for index, (key, value) in enumerate(report.items()):
            if index:
                output.write(',\n')
            output.write('  ' + json.dumps(key) + ': ')
            if key == 'calls':
                output.write('[\n' + ',\n'.join('    ' + json.dumps(call) for call in value) + '\n  ]')
            else:
                output.write(json.dumps(value, indent=2))
        output.write('\n}\n')
    print(json.dumps({'passed': report['passed'], 'checks': report['checks'],
                      'failure': report.get('failure'), 'output': options.output}))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
