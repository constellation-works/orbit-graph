#!/usr/bin/env python3
"""Offline prospective study preparation and provenance-gated scoring. No provider calls.

Reuse unchanged v1 pure source/oracle primitives; v2 owns its freeze, admission,
identity checks and commands. The runner and broker are never copied or patched.
"""
import argparse
from collections import defaultdict
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import re
import signal
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[1]
LOCK_SHA256 = "bcb44e4e071ceaeea4ec8ee71760e2f33273a3c4e696cb859f375b2ddb1f7e9e"


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


legacy = module('readonly_study_primitives', ROOT.parent / 'plugin-navigation-study-v1/eval.py')
source, profile = legacy.source, legacy.profile
runner, plugin, replies = profile.runner, profile.plugin, profile.replies
require, digest, seal, load = source.require, source.digest, source.seal, source.load
snapshot, check_evidence = legacy.snapshot, legacy.check_evidence
fresh, export, check_export = legacy.fresh, legacy.export, legacy.check_export
aliases, audited = legacy.aliases, legacy.audited
review_packets, observed_usage = legacy.review_packets, legacy.observed_usage
HARNESS = ('eval_runner.py', 'eval_broker.py', 'plugin_profile.py', 'reply_provenance.py')
VIEW_CONTRACT = ('\nTool text uses safe-tool-text-v1: original source/blob metadata remain '
                 'original while credential-shaped text is replaced by [REDACTED] before '
                 'delivery. Do not reconstruct masked literals. The requested behavior and '
                 'definition lines are answerable without them. Cite untransformed definition '
                 'lines exactly; report unavailable evidence honestly.')


def current_harness():
    require(runner.RUNNER_VERSION == '5' and runner.broker.BROKER_VERSION == '4'
            and replies.CONTRACT == 'safe-tool-text-v1', 'unsupported prospective harness contract')
    return {p: runner.sha256_file(REPO / 'scripts/agent-eval' / p) for p in HARNESS}


def frozen():
    data, protocol, lock = (load(ROOT / p) for p in ('corpus.json','protocol.json','corpus.lock.json'))
    require(digest(lock) == LOCK_SHA256, 'frozen lock differs; new source review/cohort required')
    require(lock['corpus_sha256'] == digest(data) and lock['protocol_sha256'] == digest(protocol),
            'corpus/protocol drift')
    for path, sha in lock['dependencies'].items():
        require(runner.sha256_file(REPO / path) == sha, 'review required: helper contract changed: ' + path)
    current_harness()
    require(data['schema_version'] == protocol['schema_version'] == 2, 'study schema differs')
    require(len(data['cases']) == 6 and len(protocol['order']) == 12, 'cohort shape differs')
    return data, protocol, lock


def rust_module(path, files, seen=()):
    require(path not in seen, 'cyclic Rust module inclusion')
    # Honor explicit file-backed modules, including query/trace.rs's #[path]
    # test module; filesystem spelling alone is not its Rust namespace.
    parents = []
    for parent, text in files.items():
        if not parent.endswith('.rs'):
            continue
        for match in re.finditer(r'#\[path\s*=\s*"([^"\n]+)"\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;',text):
            target = str(PurePosixPath(parent).parent / match[1])
            if target == path:
                parents.append((parent,match[2]))
    require(len(parents) <= 1, 'ambiguous Rust module inclusion')
    if parents:
        parent, name = parents[0]
        prefix = rust_module(parent, files, (*seen,path))
        return prefix+'::'+name if prefix else name
    parts = path.split('/src/',1)[-1].removeprefix('src/').rsplit('.',1)[0].split('/')
    if parts[-1] in ('mod','lib','main'): parts.pop()
    return '::'.join(parts)


def declaration_names(identity, files):
    """Conservative file/module/owner names; refuse uncertain or duplicate declarations.

    Source-review records remain the semantic authority. This check catches wrong
    namespaces/owners rather than treating a same-file alias as proof of identity.
    The selected Rust modules use top-level impl blocks; unsupported shapes fail.
    """
    path, line = identity['file'], identity['line']
    require(path in files, 'declaration file not in source view')
    lines = files[path].splitlines()
    require(type(line) is int and 1 <= line <= len(lines), 'declaration line outside source')
    quote = lines[line-1].strip()
    require(quote == identity['quote'], 'declaration quote differs')
    match = re.search(r'\b(fn|def|class|const|static|struct|enum)\s+(\w+)', quote)
    require(match, 'identity is not a declaration')
    leaf = match[2]
    owner = None
    if path.endswith('.py'):
        indent = len(lines[line-1]) - len(lines[line-1].lstrip())
        if indent:
            for prior in reversed(lines[:line-1]):
                if not prior.strip() or prior.lstrip().startswith(('#','@')):
                    continue
                level = len(prior)-len(prior.lstrip())
                if level < indent:
                    m = re.match(r'\s*class (\w+)\b', prior)
                    require(m, 'unsupported nested declaration owner')
                    owner = m[1]
                    break
            require(owner, 'method owner missing')
        separator = '.'
    else:
        for prior in reversed(lines[:line-1]):
            if prior == '}':
                break
            if prior.startswith('impl'):
                m = re.match(r'impl(?:<[^>]+>)?\s+(.+?)\s*\{', prior)
                require(m, 'unsupported impl owner')
                owner = m[1].split(' for ')[-1].split('<')[0].strip()
                require(re.fullmatch(r'\w+', owner), 'unsupported impl qualification')
                break
        separator = '::'
    module_parts = path.split('/src/',1)[-1].removeprefix('src/').rsplit('.',1)[0].split('/')
    if module_parts[-1] in ('mod','__init__'): module_parts.pop()
    module_name = rust_module(path, files) if separator == '::' else separator.join(module_parts)
    name = separator.join([owner,leaf]) if owner else leaf
    kind = identity['selector'].rsplit(':',1)[1]
    allowed_kinds = {'class'} if match[1] in ('class','struct','enum') else ({'constant'} if match[1] in ('const','static') else ({'method'} if owner else {'function','test'}))
    require(kind in allowed_kinds, 'declaration kind/owner differs')
    names = {name, module_name+separator+name}
    if separator == '::': names.add('crate::'+module_name+'::'+name)
    # A bare function cannot silently collapse duplicate declarations in one file.
    declarations = [x for x in lines if re.match(r'\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:fn|def|class|const|static|struct|enum)\s+'+re.escape(leaf)+r'\b',x)]
    if not owner: require(len(declarations) == 1, 'ambiguous declaration')
    return {f'symbol:{path}#{n}:{kind}' for n in names}


def verify(data, lock, repos):
    trees = legacy.verify(data, lock, repos)
    for case in data['cases']:
        files = trees[case['repository']]
        for identity in case['truth']['identities']:
            require(set([identity['selector'], *identity['aliases']]) <= declaration_names(identity, files),
                    'frozen alias not source-supported')
            require(replies.Redactor().text(identity['quote'])[1] == 0, 'required citation is masked')
        for claim in case['truth']['rubric']:
            for e in claim['evidence']:
                excerpt = '\n'.join(files[e['file']].splitlines()[e['line']-1:e['end_line']])
                require(replies.Redactor().text(excerpt)[1] == 0, 'rubric evidence unavailable in safe text')
    return trees


def answer_metrics(answer, case, files, support):
    for entry in support:
        if entry.get('accepted'):
            identity = dict(entry['declaration'], selector=entry['item'])
            require(entry['item'] in declaration_names(identity, files), 'support owner/module differs')
    return legacy.answer_metrics(answer, case, files, support)


def summarize(rows, kind, protocol, replay):
    result = legacy.summarize(rows, kind, protocol, replay)
    result['follow_up_consideration'] &= all(row['status']=='ok' for row in rows)
    result['schema_version'] = 2
    result['reply_contract'] = replies.CONTRACT
    result['effectiveness_evidence'] = kind == 'agent'
    return result



def requests(data, protocol, lock, pin):
    plugin.validate_pin(pin)
    cases = {c['id']: c for c in data['cases']}
    result = []
    for slot in protocol['order']:
        case = cases[slot['case_id']]
        commit = data['repositories'][case['repository']]
        revision = lock['snapshots'][case['repository']]['content_revision']
        request = dict(schema_version=2, profile=plugin.PROFILE, **slot,
                       split=case['split'], fixture=case['repository'], source_revision=revision,
                       base_revision=revision, corpus_sha256=digest(data),
                       prompt=case['prompt'] + source.CONTRACT + VIEW_CONTRACT,
                       limits=protocol['limits'], setup_limits=protocol['setup_limits'],
                       source_commits=dict(head=commit, base=commit), plugin=pin,
                       tools=source.COMMON + (list(plugin.TOOLS) if slot['arm']=='graph' else []),
                       cache_policy=runner.CACHE_POLICY)
        result.append(runner.validate_request(seal(request, 'request_sha256')))
    return result


def freeze(data, protocol, lock, runtime, inspection, export_dir, destination):
    current_harness()
    source.shape(runtime, ['schema_version', 'study_kind', 'operator', 'frozen_at', 'model',
                           'provider_binary_sha256', 'harness', 'baseline_tool_versions',
                           'resource_limits', 'binary_paths', 'approval_reference'], 'runtime')
    require(runtime['schema_version'] == 1 and runtime['study_kind'] in ('agent', 'test-only'), 'runtime kind')
    # Offline preparation accepts an explicitly pending approval reference.
    # Recording/fixing runtime pins never authorizes a provider or source transfer.
    for key in ('operator', 'frozen_at', 'approval_reference'):
        source.text(runtime[key], key)
    require(re.fullmatch(r'\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ', runtime['frozen_at']), 'freeze timestamp')
    source.shape(runtime['model'], ['provider', 'name', 'version', 'settings'], 'model')
    for key in ('provider', 'name', 'version'):
        source.text(runtime['model'][key], key)
    require(isinstance(runtime['model']['settings'], dict), 'model settings must be runner-recorded object')
    require(set(runtime['baseline_tool_versions']) == {'read', 'rg', 'git'}, 'baseline versions')
    require(set(runtime['resource_limits']) == {'memory.max', 'pids.max'} and
            all(type(v) is int and v > 0 for v in runtime['resource_limits'].values()), 'finite resource pins')
    treatment = load(Path(inspection['out']) / 'plugin-treatment.json')
    source.check_seal(treatment, 'treatment_sha256')
    require(treatment['pin'] == inspection['pin'] and treatment['inventory'] == inspection['inventory'], 'inspection evidence differs')
    require(inspection['profile'] == plugin.PROFILE and inspection['provider_started'] is False,
            'inspection contract')
    if runtime['study_kind'] == 'agent':
        require(inspection['contained'] is True, 'strict inspection required for agent freeze')
        require('fake' not in runtime['model']['version'].lower() and
                'fake' not in runtime['model']['name'].lower(), 'fake provider is test-only')
    inventory = plugin.plugin_inventory({'tools': inspection['inventory']})
    require([v['name'] for v in inventory] == list(plugin.TOOLS), 'inventory order differs')
    require(plugin.digest(inventory) == inspection['pin']['inventory_sha256'], 'inventory digest differs')
    require(runtime['harness'] == {p: runner.sha256_file(REPO / 'scripts/agent-eval' / p)
                                  for p in HARNESS}, 'harness pins differ')
    require(set(runtime['binary_paths']) == {'provider', 'orbit', 'backend', 'git', 'rg', 'python', 'bwrap'}, 'binary paths')
    pins = {k: runner.sha256_file(Path(v).resolve(strict=True)) for k,v in runtime['binary_paths'].items()}
    require(pins['provider'] == runtime['provider_binary_sha256'] and
            pins['orbit'] == inspection['pin']['orbit_sha256'] and
            pins['backend'] == inspection['pin']['backend_sha256'], 'executable pins differ')
    for tool in ('git','rg'):
        require(runtime['baseline_tool_versions'][tool].endswith('sha256:' + pins[tool]), 'tool version pin differs')
    require(runtime['baseline_tool_versions']['read'].endswith('sha256:' + runtime['harness']['eval_broker.py']), 'read pin differs')
    export_dir = check_export(data, lock, export_dir)
    plan = dict(schema_version=2, profile=plugin.PROFILE, model=runtime['model'],
                provider_binary_sha256=runtime['provider_binary_sha256'], harness=runtime['harness'],
                requests=requests(data, protocol, lock, inspection['pin']))
    destination = fresh(destination)
    destination.mkdir(mode=0o700)
    source.write_new(destination / 'plan.json', plan)
    for request in plan['requests']:
        source.write_new(destination / f"request-{request['order']:02}.json", request)
    record = seal(dict(schema_version=1, runtime=runtime, binary_sha256=pins, inspection=inspection,
                       study_files={p.name: runner.sha256_file(p) for p in ROOT.iterdir() if p.is_file()},
                       inspection_treatment=treatment, export=str(export_dir), corpus_sha256=digest(data), protocol_sha256=digest(protocol),
                       lock_sha256=digest(lock), plan_sha256=digest(plan)), 'freeze_sha256')
    source.write_new(destination / 'freeze.json', record)  # completion marker, last
    return dict(destination=str(destination), freeze_sha256=record['freeze_sha256'], episodes=12,
                study_kind=runtime['study_kind'], runtime_preregistered=True)


def checked_freeze(data, protocol, lock, folder):
    folder = Path(folder)
    f, plan = load(folder/'freeze.json'), load(folder/'plan.json')
    source.check_seal(f, 'freeze_sha256')
    require(f['study_files'] == {p.name: runner.sha256_file(p) for p in ROOT.iterdir() if p.is_file()}, 'frozen study implementation changed')
    require((f['corpus_sha256'], f['protocol_sha256'], f['lock_sha256'], f['plan_sha256']) ==
            (digest(data), digest(protocol), digest(lock), digest(plan)), 'freeze drift')
    require(plan['requests'] == requests(data, protocol, lock, f['inspection']['pin']), 'requests drift')
    require(plan == dict(schema_version=2, profile=plugin.PROFILE, model=f['runtime']['model'],
                         provider_binary_sha256=f['runtime']['provider_binary_sha256'],
                         harness=f['runtime']['harness'], requests=plan['requests']), 'plan/runtime differs')
    for request in plan['requests']:
        require(load(folder/f"request-{request['order']:02}.json") == request, 'request file differs')
    return f, plan


def adapt(data, protocol, lock, frozen_dir, directories, custody):
    f, plan = checked_freeze(data, protocol, lock, frozen_dir)
    kind = f['runtime']['study_kind']
    require(len(directories) == 12, 'all twelve raw episodes required; never omit failures')
    replay = profile.replay(plan, directories, diagnostic=kind == 'test-only')
    episodes = sorted((profile.load_episode(p, diagnostic=kind=='test-only') for p in directories),
                      key=lambda a:a['request']['order'])
    source.shape(custody, ['operator','attested_at','freeze_sha256','captures','attestation'], 'capture custody')
    for key in ('operator','attested_at','attestation'):
        source.text(custody[key], key)
    require(custody['freeze_sha256']==f['freeze_sha256'], 'custody freeze differs')
    require(custody['captures']=={a['run_id']:a['artifact_sha256'] for a in episodes}, 'capture custody differs')
    for a in episodes:
        require(a['runner_version'] == '5' and a['lifecycle_contract'] == runner.LIFECYCLE_CONTRACTS['5'], 'historical lifecycle capture refused')
        require(a['harness'] == current_harness(), 'prospective harness differs')
        require(a['reply_contract']['version'] == replies.CONTRACT, 'prospective safe text required')
        require(a['reply_contract']['policy'] == replies.Redactor().policy, 'host-value masking not admitted; use auth-file, no secret provider-env')
        require({k:a['tool_versions'][k] for k in ('read','rg','git')} == f['runtime']['baseline_tool_versions'], 'tool pins differ')
        require(a['resource_limits'] == f['runtime']['resource_limits'] or kind=='test-only', 'resource pins differ')
        for role in ('head','base'):
            manifest = lock['snapshots'][a['request']['fixture']]
            require(a['source_provenance'][role]['selected_blobs'] ==
                    {p:v['oid'] for p,v in manifest['blobs'].items()}, 'raw selected source blobs differ')
        if kind=='agent':
            require('fake' not in a['model']['version'].lower(), 'fake captures cannot be agent evidence')
    refs = []
    for directory in directories:
        a = load(Path(directory)/'episode.json')
        refs.append(dict(directory=str(Path(directory).resolve()),run_id=a['run_id'],artifact_sha256=a['artifact_sha256']))
    # Complete raw objects are retained, not narrowed to successful/answer records.
    return seal(dict(schema_version=1, study_kind=kind, freeze_sha256=f['freeze_sha256'],
                     custody=custody, replay=replay, raw=refs, episodes=episodes), 'bundle_sha256')


def score(data, protocol, trees, bundle, audits):
    source.shape(audits,['schema_version','audits'], 'reviews')
    require(audits['schema_version']==1, 'review schema')
    by_run = {a['run_id']:a for a in audits['audits']}
    require(len(by_run)==len(audits['audits']), 'duplicate review')
    successful = {a['run_id'] for a in bundle['episodes'] if a['status']=='ok'}
    require(set(by_run)==successful, 'reviews must cover exactly successful episodes')
    cases = {c['id']:c for c in data['cases']}
    rows = []
    for a in bundle['episodes']:
        request = a['request']; case = cases[request['case_id']]
        quality, semantic, blind = None, False, None
        if a['status']=='ok':
            audit = by_run[a['run_id']]
            semantic = audited(audit,a,case)
            quality = answer_metrics(a['answer'],case,trees[case['repository']],audit['supporting_symbols'])
            blind = audit['blinding']
        graph_calls = [c for c in a['calls'] if c['tool'].startswith('graph_')]
        rows.append(dict(case_id=case['id'],split=case['split'],arm=request['arm'],run_id=a['run_id'],
                         artifact_sha256=a['artifact_sha256'],status=a['status'],error=a['error'],
                         quality=quality,semantic_pass=semantic,review_blinding=blind,
                         correct=a['status']=='ok' and quality['objective_pass'] and semantic,
                         calls=len(a['calls']),graph_calls=len(graph_calls),
                         successful_graph_calls=sum(c['status']=='ok' for c in graph_calls),
                         timing=a['timing'],setup_output_bytes=a['setup_output_bytes'],
                         output_bytes=a['output_bytes'],usage=observed_usage(a)))
    return summarize(rows,bundle['study_kind'],protocol,bundle['replay'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', help='offline source validation, export, runtime freeze, raw adaptation, review packets or scoring', choices=['validate','export','freeze','adapt','review-packets','score'])
    parser.add_argument('--repo',action='append',default=[],metavar='NAME=GIT_ROOT', help='explicit source checkout root; repeat for all three repositories')
    for name in ('destination','runtime','inspection','export','frozen','custody','bundle','audits'):
        parser.add_argument('--'+name, help='path to the '+name+' JSON or directory (see README)')
    parser.add_argument('--raw',action='append',default=[],help='original runner capture directory; supply all twelve')
    args = parser.parse_args()
    signal.signal(signal.SIGALRM, lambda *_: (_ for _ in ()).throw(ValueError('600-second study deadline')))
    signal.alarm(600)
    try:
        data, protocol, lock = frozen()
        if args.command in ('validate','export','score'):
            repos = dict(v.split('=',1) for v in args.repo)
            require(len(repos)==len(args.repo), 'duplicate repository')
            trees = verify(data,lock,{k:Path(v) for k,v in repos.items()})
        if args.command=='validate':
            result = dict(valid=True,cases=6,source_files={k:len(v) for k,v in trees.items()},lock_sha256=digest(lock))
        elif args.command=='export':
            result = export(data,lock,trees,args.destination)
        elif args.command=='freeze':
            result = freeze(data,protocol,lock,load(args.runtime),load(args.inspection),args.export,args.destination)
        elif args.command=='adapt':
            result = adapt(data,protocol,lock,args.frozen,args.raw,load(args.custody))
            source.write_new(fresh(args.destination),result)
            result = dict(bundle_sha256=result['bundle_sha256'],episodes=12,study_kind=result['study_kind'])
        else:
            bundle = load(args.bundle)
            source.check_seal(bundle,'bundle_sha256')
            # Re-read all original evidence and replay again; edited neutral objects cannot bypass it.
            actual = adapt(data,protocol,lock,args.frozen,[r['directory'] for r in bundle['raw']],bundle['custody'])
            require(actual==bundle, 'bundle no longer matches raw provenance')
            if args.command=='review-packets':
                result = review_packets(bundle,data)
                source.write_new(fresh(args.destination),result)
                result = dict(packets=len(result['packets']),destination=args.destination)
            else:
                result = score(data,protocol,trees,bundle,load(args.audits))
        print(json.dumps(result,indent=2,sort_keys=True))
        return 0
    except BrokenPipeError:
        return 0
    except (ValueError,OSError,KeyError,TypeError,IndexError) as error:
        print(json.dumps(dict(code='study_invalid',error=str(error),hint='Use the v2 README contracts and a fresh output destination; never rewrite frozen evidence.')),file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
