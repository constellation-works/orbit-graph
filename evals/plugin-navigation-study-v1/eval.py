#!/usr/bin/env python3
"""Offline installed-plugin study: verify, export, freeze, adapt, review packets, score.

Never starts a provider, installs a plugin, or changes historical evaluation files.
"""
import argparse
from collections import Counter, defaultdict
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[1]
LOCK_SHA256 = "9b8ac285facf88fd15a88fbcf7cf049836338784e358fd91c92814496a025508"


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


source = module("historical_source_helper", REPO / "evals/real-agent-navigation/eval.py")
profile = module("installed_profile_evaluator", REPO / "evals/plugin-agent-navigation/eval.py")
runner, plugin = profile.runner, profile.plugin
require, digest, seal, load = source.require, source.digest, source.seal, source.load


def snapshot(repo, commit):
    require(repo.is_dir() and (repo / ".git").exists(), "explicit Git checkout root required")
    require(Path(source.git(repo, "rev-parse", "--show-toplevel").decode().strip()).resolve()
            == repo.resolve(), "parent Git discovery refused")
    manifest, files = source.snapshot(repo, commit)
    require(all(plugin.source_path_allowed(p) for p in files), "profile source allowlist differs")
    blobs = {}
    for entry in source.git(repo, "ls-tree", "-rz", "--full-tree", commit).split(b"\0"):
        if entry:
            meta, name = entry.split(b"\t", 1)
            if name.decode() in files:
                mode, kind, oid = meta.decode().split()
                blobs[name.decode()] = {"mode": mode, "oid": oid}
    manifest["blobs"] = blobs
    return manifest, files


def frozen():
    data, protocol, lock = (load(ROOT / name) for name in
                            ("corpus.json", "protocol.json", "corpus.lock.json"))
    require(digest(lock) == LOCK_SHA256, "frozen lock differs; new review/cohort required")
    require(lock["corpus_sha256"] == digest(data) and lock["protocol_sha256"] == digest(protocol),
            "corpus/protocol drift")
    for path, sha in lock["dependencies"].items():
        require(runner.sha256_file(REPO / path) == sha, "review required: helper contract changed: " + path)
    require(len(data["cases"]) == 6 and len(protocol["order"]) == 12, "cohort shape differs")
    return data, protocol, lock


def check_evidence(e, files):
    lines = files[e["file"]].splitlines()
    require(1 <= e["line"] <= e["end_line"] <= len(lines), "source range outside file")
    require(lines[e["line"] - 1].strip() == e["quote"], "source quote differs")
    excerpt = "\n".join(lines[e["line"]-1:e["end_line"]]) + "\n"
    require(hashlib.sha256(excerpt.encode()).hexdigest() == e["excerpt_sha256"], "source range differs")


def verify(data, lock, repos):
    require(set(repos) == set(data["repositories"]), "need exactly orbit-graph, Orbit, pulsar roots")
    trees = {}
    for name, commit in data["repositories"].items():
        manifest, files = snapshot(repos[name], commit)
        require(manifest == lock["snapshots"][name], "source manifest differs: " + name)
        trees[name] = files
    for case in data["cases"]:
        files = trees[case["repository"]]
        aliases = defaultdict(set)
        for identity in case["truth"]["identities"]:
            require(files[identity["file"]].splitlines()[identity["line"]-1].strip() == identity["quote"],
                    "identity source differs")
            for alias in [identity["selector"], *identity["aliases"]]:
                require(alias.split('#')[0] == 'symbol:' + identity['file'], "alias crosses files")
                aliases[alias].add(identity['selector'])
            if "owner_evidence" in identity:
                check_evidence(identity["owner_evidence"], files)
        require(all(len(ids) == 1 for ids in aliases.values()), "ambiguous frozen alias")
        for claim in case["truth"]["rubric"]:
            for e in claim["evidence"]:
                check_evidence(e, files)
    return trees


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
                       prompt=case['prompt'] + source.CONTRACT,
                       limits=protocol['limits'], setup_limits=protocol['setup_limits'],
                       source_commits=dict(head=commit, base=commit), plugin=pin,
                       tools=source.COMMON + (list(plugin.TOOLS) if slot['arm']=='graph' else []),
                       cache_policy=runner.CACHE_POLICY)
        result.append(runner.validate_request(seal(request, 'request_sha256')))
    return result


def fresh(path):
    path = Path(os.path.abspath(path))
    require(not path.exists() and not path.is_symlink(), 'destination exists')
    require(path.parent.is_dir() and path.parent.resolve() == path.parent,
            'destination parent must exist and be physical')
    return path


def export(data, lock, trees, destination):
    destination = fresh(destination)
    destination.mkdir(mode=0o700)
    for name, files in trees.items():
        for path, content in files.items():
            source.write_new(destination / name / path, content)
    # Manifests/answers are siblings of source roots, never mounted source files.
    source.write_new(destination / 'source-manifest.json', lock['snapshots'])
    source.write_new(destination / 'complete.json', seal(dict(schema_version=1,
                     corpus_sha256=digest(data), manifests_sha256=digest(lock['snapshots'])), 'export_sha256'))
    return dict(destination=str(destination), source_views=len(trees), complete=True)


def check_export(data, lock, directory):
    directory = Path(directory).resolve()
    marker = load(directory / 'complete.json')
    source.check_seal(marker, 'export_sha256')
    require(marker['corpus_sha256'] == digest(data) and marker['manifests_sha256'] == digest(lock['snapshots']),
            'export marker differs')
    require(load(directory / 'source-manifest.json') == lock['snapshots'], 'export manifest differs')
    for name, manifest in lock['snapshots'].items():
        entries = runner.tree_entries(directory / name, name)
        require({p: runner.sha256_file(path) for p, path in entries} == manifest['files'], 'export bytes differ')
        require(runner.content_revision(entries, name)['content_revision'] == manifest['content_revision'],
                'export revision differs')
    return directory


def freeze(data, protocol, lock, runtime, inspection, export_dir, destination):
    source.shape(runtime, ['schema_version', 'study_kind', 'operator', 'frozen_at', 'model',
                           'provider_binary_sha256', 'harness', 'baseline_tool_versions',
                           'resource_limits', 'binary_paths', 'approval_reference'], 'runtime')
    require(runtime['schema_version'] == 1 and runtime['study_kind'] in ('agent', 'test-only'), 'runtime kind')
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
                                  for p in ('eval_runner.py','eval_broker.py','plugin_profile.py')}, 'harness pins differ')
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


def aliases(case):
    result = defaultdict(list)
    for identity in case['truth']['identities']:
        for name in [identity['selector'], *identity['aliases']]:
            result[name].append(identity)
    return result


def answer_metrics(answer, case, files, support):
    # Reuse the public response shape validator, never its historical closed-set oracle.
    runner.check_answer(answer, source.LIMITS)
    known = aliases(case)
    items = answer['items']
    expected = {i['selector'] for i in case['truth']['identities'] if i['required']}
    resolved, cited, unsupported, invalid = set(), set(), [], []
    cited_items, supported_items = set(), set()
    support_by_item = {s['item']:s for s in support}
    require(len(support_by_item)==len(support), 'duplicate supporting-symbol review')
    require(set(support_by_item)<=set(items)-set(known), 'support cannot override frozen identity/ambiguous alias')
    for item in items:
        candidates = known.get(item, [])
        identity = candidates[0] if len(candidates)==1 else None
        if identity is None and item not in known:
            review = support_by_item.get(item)
            if review:
                source.shape(review, ['item','accepted','rationale','declaration','identity_evidence'], 'support review')
                require(type(review['accepted']) is bool, 'support verdict')
                source.text(review['rationale'], 'support rationale')
                for e in review['identity_evidence']:
                    check_evidence(e, files)
                declaration = review['declaration']
                source.shape(declaration,['file','line','quote'], 'support declaration')
                lines = files.get(declaration['file'], '').splitlines()
                require(1 <= declaration['line'] <= len(lines) and
                        lines[declaration['line']-1].strip()==declaration['quote'], 'support declaration differs')
                match = re.fullmatch(r'symbol:([^#]+)#([^#]+):(function|method|test|class|constant)',item)
                require(match and match[1]==declaration['file'], 'support item/file differs')
                parts = re.split(r'::|\.',match[2])
                leaf = parts[-1]
                require(re.search(r'\b(?:fn|def|class|const|static|struct|enum)\s+'+re.escape(leaf)+r'\b',declaration['quote']), 'support is not named declaration')
                declarations = [line for line in lines if re.match(r'\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:fn|def|class|const|static|struct|enum)\s+' + re.escape(leaf) + r'\b', line)]
                require(len(declarations) == 1, 'additional support has ambiguous declarations; needs a new reviewed corpus, not alias guessing')
                module_path = declaration['file'].split('/src/', 1)[-1].removeprefix('src/').rsplit('.', 1)[0]
                module_parts = module_path.split('/')
                if module_parts[-1] in ('mod','__init__'):
                    module_parts.pop()
                prefix = parts[:-1]
                if match[3] == 'method':
                    require(prefix, 'support method needs owner qualification')
                    owner = prefix.pop()
                    context = '\n'.join('\n'.join(files[e['file']].splitlines()[e['line']-1:e['end_line']]) for e in review['identity_evidence'])
                    require(re.search(r'\b(?:class|impl)\s+(?:<[^>]+>\s*)?' + re.escape(owner) + r'\b', context), 'support owner lacks source context')
                require(not prefix or prefix == module_parts or prefix == ['crate'] + module_parts, 'support module qualification differs from source file')
                require(review['identity_evidence'] and any(e['file']==declaration['file'] and
                        e['line']<=declaration['line']<=e['end_line'] for e in review['identity_evidence']), 'support identity lacks source context')
                # Owner/module identity and relevance are explicitly human-reviewed, not inferred by regex.
                if review['accepted']:
                    identity = dict(selector=item, **declaration)
        if identity:
            resolved.add(identity['selector'])
            supported_items.add(item)
        else:
            unsupported.append(item)
        citations = [e for e in answer['evidence'] if e['item']==item]
        for e in citations:
            good = identity is not None and all(e[k]==identity[k] for k in ('file','line','quote'))
            if good:
                cited.add(identity['selector'])
                cited_items.add(item)
            else:
                invalid.append(e)
    for e in answer['evidence']:
        if e['item'] not in items:
            invalid.append(e)
    return dict(required_found=len(resolved & expected), required_total=len(expected),
                identity_recall=len(resolved & expected)/len(expected), unsupported=unsupported,
                cited_required=len(cited & expected), citation_invalid=invalid,
                returned_items=len(items), supported_items=len(supported_items),
                identity_precision=len(supported_items)/len(items) if items else 0,
                citation_total=len(answer['evidence']), citation_valid=len(answer['evidence'])-len(invalid),
                citation_coverage=len(cited_items)/len(items) if items else 0,
                objective_pass=expected<=resolved and set(items)==cited_items and not unsupported and not invalid,
                supporting_accepted=len(resolved-expected))


def audited(audit, episode, case):
    source.shape(audit,['run_id','artifact_sha256','reviewer','signed_at','blinding','attestation',
                        'judgments','supporting_symbols','audit_sha256'], 'semantic review')
    source.check_seal(audit,'audit_sha256')
    require(audit['run_id']==episode['run_id'] and audit['artifact_sha256']==episode['artifact_sha256'], 'review attribution differs')
    require(audit['blinding'] in ('arm-blinded','unblinded'), 'blinding state required')
    # Share strict attributed claim/quote/source checking with the existing evaluator.
    old = {k:audit[k] for k in ('run_id','reviewer','signed_at','attestation','judgments')}
    old['record_sha256'] = episode['artifact_sha256']
    source.seal(old,'attestation_sha256')
    record = dict(episode,record_sha256=episode['artifact_sha256'])
    return source.audit_check(old,record,case,{})


def review_packets(bundle, data):
    cases = {c['id']:c for c in data['cases']}
    # No arm, tools, timings, status label, or ready-made pass judgments in review packets.
    return dict(schema_version=1, instructions='Review answer and source; author judgments. Packet IDs are hashes, not arm labels. Tool references in prose can still unblind.',
                packets=[dict(packet_id=a['artifact_sha256'],case_id=a['request']['case_id'],
                              answer=a['answer'],truth=cases[a['request']['case_id']]['truth'])
                         for a in sorted(bundle['episodes'], key=lambda a:a['artifact_sha256']) if a['status']=='ok'])


def summarize(rows, kind, protocol):
    groups = {}
    for split in ('all','development','held-out'):
        for arm in ('baseline','graph'):
            selected = [r for r in rows if r['arm']==arm and (split=='all' or r['split']==split)]
            costs = {}
            for key in sorted({k for r in selected for k in r['usage']}):
                values = [r['usage'].get(key) for r in selected]
                observed = [v for v in values if v is not None]
                costs[key] = dict(observed=len(observed),total=len(values),
                                  observed_sum=sum(observed) if observed else None,
                                  sum=sum(observed) if len(observed)==len(values) else None)
            ok = [r for r in selected if r['status']=='ok']
            groups[split+'/'+arm] = dict(total=len(selected), correct=sum(r['correct'] for r in selected),
                accuracy=sum(r['correct'] for r in selected)/len(selected) if selected else None,
                statuses=dict(Counter(r['status'] for r in selected)),
                error_codes=dict(Counter(r['error']['code'] for r in selected if r['error'])),
                adoption=dict(attempted=sum(r['graph_calls']>0 for r in selected), denominator=len(selected),
                              among_ok=sum(r['graph_calls']>0 for r in ok),ok_denominator=len(ok),
                              successful=sum(r['successful_graph_calls']>0 for r in selected),
                              calls=sum(r['graph_calls'] for r in selected),
                              successful_calls=sum(r['successful_graph_calls'] for r in selected)),
                tool_calls=sum(r['calls'] for r in selected), output_bytes=sum(r['output_bytes'] for r in selected),
                setup_output_bytes=sum(r['setup_output_bytes'] for r in selected),
                timing_totals={k:sum(r['timing'][k] for r in selected) for k in selected[0]['timing']} if selected else {}, usage=costs)
    pairs = []
    for case_id in dict.fromkeys(r['case_id'] for r in rows):
        b,g = ([r for r in rows if r['case_id']==case_id and r['arm']==a][0] for a in ('baseline','graph'))
        both = b['correct'] and g['correct']
        pairs.append(dict(case_id=case_id,correct_delta=int(g['correct'])-int(b['correct']),both_correct=both,
                          wall_delta_ms=g['timing']['wall_ms']-b['timing']['wall_ms'] if both else None,
                          baseline_over_graph=b['timing']['wall_ms']/g['timing']['wall_ms'] if both and g['timing']['wall_ms'] else None))
    held_graph = [r for r in rows if r['arm']=='graph' and r['split']=='held-out']
    eligible = (kind=='agent' and groups['held-out/graph']['correct']>=groups['held-out/baseline']['correct']
                and any(p['correct_delta']>0 and p['case_id'] in {r['case_id'] for r in held_graph} for p in pairs)
                and all(r['status']=='ok' and not r['quality']['unsupported'] for r in held_graph))
    return dict(schema_version=1,study_kind=kind,effectiveness_evidence=kind=='agent',episodes=rows,
                groups=groups,pairs=pairs,follow_up_consideration=eligible,limitations=protocol['limitations'])


def observed_usage(episode):
    usage, raw = episode.get('usage') or {}, episode.get('usage_raw') or {}
    return {key: (usage.get(key) if key in ('input_tokens','output_tokens','cost_usd') else raw.get(key))
            for key in ('input_tokens','output_tokens','cached_input_tokens','total_tokens','cost_usd')}


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
    return summarize(rows,bundle['study_kind'],protocol)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['validate','export','freeze','adapt','review-packets','score'])
    parser.add_argument('--repo',action='append',default=[],metavar='NAME=GIT_ROOT')
    for name in ('destination','runtime','inspection','export','frozen','custody','bundle','audits'):
        parser.add_argument('--'+name)
    parser.add_argument('--raw',action='append',default=[])
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
    except (ValueError,OSError,KeyError,TypeError,IndexError) as error:
        print(json.dumps(dict(code='study_invalid',error=str(error))),file=sys.stderr)
        return 1


if __name__=='__main__':
    sys.exit(main())
