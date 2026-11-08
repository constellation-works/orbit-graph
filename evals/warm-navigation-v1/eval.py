#!/usr/bin/env python3
"""Offline warm-navigation qualification export, freeze, replay and all-slot scoring."""
import argparse
import json
import os
import shutil
from pathlib import Path
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[1]
sys.path.insert(0, str(REPO / 'scripts/agent-eval'))
import eval_runner as runner
import plugin_profile as plugin
import warm_batch as batch


def fresh(path):
    path = Path(path).absolute()
    batch.check(not os.path.lexists(path) and path.parent.is_dir(), 'destination must be new with existing parent')
    return path


def export(destination):
    """Test-only source/Git fixture; lock is checked before producing any query."""
    lock = runner.load_json(ROOT / 'corpus.lock.json')
    for name, sha in lock['files'].items():
        batch.check(runner.sha256_file(ROOT / name) == sha, 'source-only fixture lock drift')
    destination = fresh(destination)
    usage = shutil.disk_usage(destination.parent)
    batch.check(usage.used / usage.total < .8, 'disk usage is at least 80%; no fixture creation')
    runner.make_private_dir(destination)
    runner.make_private_dir(destination / 'state')
    runner.make_private_dir(destination / 'state/home')
    for view in ('unchanged', 'edited'):
        (destination / view / 'src').mkdir(mode=0o700, parents=True)
        target = destination / view / 'src/lib.rs'
        target.write_bytes((ROOT / 'fixtures' / view / 'src/lib.rs').read_bytes())
        target.chmod(0o600)
    snapshot = runner.materialize_snapshot(destination / 'source', destination / 'state', '/usr/bin/git',
        runner.tree_entries(destination / 'unchanged', 'base'), runner.tree_entries(destination / 'edited', 'head'))
    record = batch.sealed({'schema_version': 1, 'study_kind': 'test-only', 'lock_sha256': plugin.digest(lock),
                           'snapshot': snapshot}, 'export_sha256')
    batch.write(destination / 'export.json', record)
    return record


def fixture_slots(export_dir, pin, arm):
    record = runner.load_json(Path(export_dir) / 'export.json')
    batch.verify_seal(record, 'export_sha256')
    batch.check(record['study_kind'] == 'test-only', 'qualification is test-only')
    snapshot = record['snapshot']
    protocol = runner.load_json(ROOT / 'protocol.json')
    slots = []
    for i, view in enumerate(protocol['schedule']):
        revision = snapshot['base' if view == 'unchanged' else 'head']['content_revision']
        q = dict(schema_version=5, profile=batch.PROFILE, case_id='qualification-%d' % i,
                 arm=arm, split='test-only', source_revision=revision,
                 base_revision=snapshot['base']['content_revision'], prompt='Find checkout and cite its declaration. Return only JSON with items, abstain, reason, evidence.',
                 limits=dict(wall_ms=60000, tool_calls=40, output_bytes=131072, answer_items=32, call_bytes=16384),
                 tools=list(runner.broker.ARM_TOOLS['baseline']) + (list(plugin.TOOLS) if arm == 'graph' else []),
                 order=i, plugin=pin, setup_limits=dict(wall_ms=60000, output_bytes=8388608),
                 source_commits=dict(head=snapshot['base_commit' if view == 'unchanged' else 'head_commit'], base=snapshot['base_commit']),
                 cache_policy=batch.CACHE_POLICY)
        slots.append(dict(request=batch.sealed(q, 'request_sha256'), head_view=view, base_view='unchanged'))
    return slots


def freeze(spec, destination):
    """Freeze all generic runtime, source, request and state controls before launch.

    A real cohort supplies independently reviewed slots; fixture-slots is always
    test-only. There is no provider call in export/freeze/replay/score.
    """
    batch.check(set(spec) == {"study_kind", "batch_id", "arm", "slots", "runtime"}, "freeze spec fields differ")
    plan = dict(schema_version=1, profile=batch.PROFILE, policy=batch.POLICY,
                study_files={n: runner.sha256_file(ROOT / n) for n in ("eval.py", "protocol.json", "corpus.lock.json")}, **spec)
    batch.validate_plan(batch.sealed(plan, 'freeze_sha256'))
    batch.write(fresh(destination), plan)
    return plan


def score(directory, audits, diagnostic=False):
    plan, record, episodes = batch.replay(directory, diagnostic)
    batch.check(set(audits) == {'schema_version', 'freeze_sha256', 'reviews'} and audits['schema_version'] == 1
                and audits['freeze_sha256'] == plan['freeze_sha256'], 'review freeze differs')
    reviews = {row['slot']: row for row in audits['reviews']}
    batch.check(len(reviews) == len(audits['reviews']), 'duplicate review')
    batch.check(set(reviews) == {i for i, a in enumerate(episodes) if a and a['status'] == 'ok'}, 'review exactly successful slots')
    rows = []
    for i, (row, a) in enumerate(zip(record['slots'], episodes)):
        review = reviews.get(i)
        if review:
            batch.check(set(review) == {'slot', 'artifact_sha256', 'correct', 'reviewer', 'source_evidence'}
                        and type(review['correct']) is bool and review['reviewer'] and review['source_evidence']
                        and review['artifact_sha256'] == a['artifact_sha256'], 'independent review evidence differs')
        rows.append(dict(slot=i, status=row['status'], correct=bool(review and review['correct']),
                         artifact_sha256=a['artifact_sha256'] if a else None,
                         telemetry=a['telemetry'] if a else None, cache=a['batch']['cache_observations'] if a else [],
                         output_bytes=a['output_bytes'] if a else None,
                         setup_output_bytes=a['setup_output_bytes'] if a else None))
    wall = (record['end_ns'] - record['start_ns']) / 1_000_000
    complete_bytes = all(a is not None and a['telemetry']['tools']['coverage']['state'] == 'complete' for a in episodes)
    return dict(schema_version=1, profile=batch.PROFILE, study_kind=plan['study_kind'], effectiveness_evidence=False,
                freeze_sha256=plan['freeze_sha256'], batch_sha256=record['batch_sha256'],
                complete=record['complete'], scheduled=len(rows), all_attempt_correct=sum(r['correct'] for r in rows),
                all_attempt_accuracy=sum(r['correct'] for r in rows) / len(rows), measured_batch_wall_ms=wall,
                amortized_batch_wall_ms=wall / len(rows),
                measured_tool_bytes=sum(a['telemetry']['tools']['output_bytes'] for a in episodes if a) if complete_bytes else None,
                tool_bytes_coverage='complete' if complete_bytes else 'partial',
                observed_tool_bytes=sum(a['telemetry']['tools']['output_bytes'] for a in episodes if a),
                setup_bytes=sum(a['setup_output_bytes'] for a in episodes if a) if complete_bytes else None,
                observed_setup_bytes=sum(a['setup_output_bytes'] for a in episodes if a),
                batch_usage_total=None, cost_usd=None,
                outside_batch_wall={'source_export_ms': None, 'runtime_qualification_ms': None, 'independent_review_ms': None},
                timing_note='Per-slot setup, preflight, provider, installation and sync overlap measured batch wall; never add them.',
                rows=rows)


def cohort_check(directories, diagnostic=False):
    """Root's later cohort must retain both arm orders on the same host boot."""
    protocol = runner.load_json(ROOT / 'protocol.json')
    expected = [arm for pair in protocol['batch_orders'] for arm in pair]
    batch.check(len(directories) == len(expected), 'all four counterbalanced batches required')
    captures = [batch.replay(p, diagnostic) for p in directories]
    batch.check([p['arm'] for p, _, _ in captures] == expected, 'counterbalanced batch order differs')
    batch.check(len({p['batch_id'] for p, _, _ in captures}) == len(expected), 'reused batch identity')
    batch.check(len({r['clock_id'] for _, r, _ in captures}) == 1, 'batch clocks from different hosts/boots')
    for i in range(1, len(captures)):
        batch.check(captures[i-1][1]['end_ns'] <= captures[i][1]['start_ns'], 'batch boundaries overlap or order differs')
    for i in (0, 2):
        batch.paired(captures[i][0], captures[i+1][0])
    batch.paired(captures[0][0], captures[2][0])
    return dict(counterbalanced=True, batches=len(captures), scheduled=sum(len(p['slots']) for p, _, _ in captures),
                all_batches_complete=all(r['complete'] for _, r, _ in captures),
                os_cache='uncontrolled; no claim of a cache flush or identical page-cache contents')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['export', 'fixture-slots', 'freeze', 'pair', 'replay', 'score', 'cohort-check'])
    for name in ('destination', 'export', 'pin', 'spec', 'left', 'right', 'batch', 'audits'):
        parser.add_argument('--' + name)
    parser.add_argument('--capture', action='append', default=[])
    parser.add_argument('--arm', choices=['baseline', 'graph'])
    parser.add_argument('--diagnostic', action='store_true')
    args = parser.parse_args()
    try:
        if args.command == 'export': result = export(args.destination)
        elif args.command == 'fixture-slots': result = fixture_slots(args.export, runner.load_json(args.pin), args.arm)
        elif args.command == 'freeze': result = freeze(runner.load_json(args.spec), args.destination)
        elif args.command == 'pair':
            batch.paired(runner.load_json(args.left), runner.load_json(args.right)); result = {'paired': True}
        elif args.command == 'cohort-check': result = cohort_check(args.capture, args.diagnostic)
        elif args.command == 'replay':
            plan, record, _ = batch.replay(args.batch, args.diagnostic)
            result = dict(profile=plan['profile'], complete=record['complete'], slots=record['slots'])
        else: result = score(args.batch, runner.load_json(args.audits), args.diagnostic)
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0
    except (ValueError, OSError, KeyError, TypeError) as error:
        print(json.dumps({'error': {'code': 'warm_study_invalid', 'message': str(error)}}), file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
