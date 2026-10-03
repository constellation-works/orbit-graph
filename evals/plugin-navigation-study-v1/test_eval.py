"""Deterministic TEST-ONLY oracle/export/contract fixtures; no provider results.

Actual Git validation is an explicit operator/CI command. These tests read only
frozen evidence files via the explicit source roots when source tests are enabled.
"""
import copy
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location('study', Path(__file__).with_name('eval.py'))
study = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(study)


def answer(case):
    identities = [i for i in case['truth']['identities'] if i['required']]
    return dict(items=[i['selector'] for i in identities],abstain=False,
                reason=' '.join(c['required_behavior'] for c in case['truth']['rubric']),
                evidence=[dict(item=i['selector'],**{k:i[k] for k in ('file','line','quote')}) for i in identities])


def review(episode, case, passed=True):
    # Never written as a raw capture; score fixtures remain explicitly test-only.
    return study.seal(dict(run_id=episode['run_id'],artifact_sha256=episode['artifact_sha256'],
        reviewer='TEST-ONLY deterministic fixture',signed_at='2026-10-03T00:00:00Z',blinding='arm-blinded',
        attestation='TEST-ONLY: scripted oracle fixture, not an agent or human review',supporting_symbols=[],
        judgments=[]),'audit_sha256')


def audit_for(episode, case, passed=True):
    audit = review(episode,case)
    audit['judgments'] = [{'claim_id':c['id'],'pass':passed,'rationale':'TEST-ONLY known fixture claim',
        'answer_quote':c['required_behavior'],'source_evidence':c['evidence']} for c in case['truth']['rubric']]
    return study.seal(audit,'audit_sha256')


class StudyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.data, cls.protocol, cls.lock = study.frozen()
        # Full selected files read from pinned objects, never working trees or graph queries.
        roots = Path(os.environ.get('STUDY_SOURCE_ROOT','/home/daniel/workspace/constellation/codebases'))
        cls.files = {}
        for case in cls.data['cases']:
            name=case['repository']; repo=roots/('orbit' if name=='Orbit' else name)
            cls.files.setdefault(name,{})
            for path in {i['file'] for i in case['truth']['identities']} | {
                    e['file'] for c in case['truth']['rubric'] for e in c['evidence']}:
                cls.files[name][path]=study.source.git(repo,'show',cls.data['repositories'][name]+':'+path).decode()
        scratch=study.REPO/'.orbit/tmp/study-tests'
        scratch.mkdir(parents=True,exist_ok=True)
        cls.scratch=scratch

    def test_all_source_reviewed_ranges_and_aliases(self):
        for c in self.data['cases']:
            for claim in c['truth']['rubric']:
                for e in claim['evidence']:study.check_evidence(e,self.files[c['repository']])
            for i in c['truth']['identities']:
                self.assertEqual(self.files[c['repository']][i['file']].splitlines()[i['line']-1].strip(),i['quote'])
                if 'owner_evidence' in i:study.check_evidence(i['owner_evidence'],self.files[c['repository']])

    def test_complete_partial_wrong_and_module_owner_aliases(self):
        for c in self.data['cases']:
            files=self.files[c['repository']]; a=answer(c)
            self.assertTrue(study.answer_metrics(a,c,files,[])['objective_pass'])
            aliased=copy.deepcopy(a)
            for idx,i in enumerate(i for i in c['truth']['identities'] if i['required']):
                aliased['items'][idx]=i['aliases'][0];aliased['evidence'][idx]['item']=i['aliases'][0]
            self.assertTrue(study.answer_metrics(aliased,c,files,[])['objective_pass'])
            partial=copy.deepcopy(a);partial['items'].pop();partial['evidence'].pop()
            result=study.answer_metrics(partial,c,files,[])
            self.assertFalse(result['objective_pass']);self.assertGreater(result['identity_recall'],0)
            wrong=copy.deepcopy(a);wrong['items'][0]+='_invented';wrong['evidence'][0]['item']=wrong['items'][0]
            self.assertEqual(study.answer_metrics(wrong,c,files,[])['unsupported'],[wrong['items'][0]])

    def test_ambiguous_alias_cannot_be_repaired_by_citation_or_review(self):
        c=copy.deepcopy(self.data['cases'][0]); a=answer(c)
        # Two declarations sharing a short alias: exact citation alone must not pick one.
        c['truth']['identities'][0]['aliases'].append('symbol:'+c['truth']['identities'][0]['file']+'#ambiguous:method')
        c['truth']['identities'][1]['aliases'].append(c['truth']['identities'][0]['aliases'][-1])
        ambiguous=c['truth']['identities'][0]['aliases'][-1]
        a['items'][0]=ambiguous;a['evidence'][0]['item']=ambiguous
        result=study.answer_metrics(a,c,self.files[c['repository']],[])
        self.assertIn(ambiguous,result['unsupported']);self.assertFalse(result['objective_pass'])
        with self.assertRaises(ValueError):
            study.answer_metrics(a,c,self.files[c['repository']],[{'item':ambiguous}])

    def test_wrong_file_quote_and_line_never_get_citation_credit(self):
        c=self.data['cases'][0]
        for key,value in [('file','src/wrong.rs'),('quote','fn invented() {}'),('line',1)]:
            a=answer(c);a['evidence'][0][key]=value
            result=study.answer_metrics(a,c,self.files[c['repository']],[])
            self.assertFalse(result['objective_pass']);self.assertEqual(len(result['citation_invalid']),1)

    def test_accurate_extra_support_accepted_only_with_source_review(self):
        c=self.data['cases'][0];a=answer(c);files=self.files[c['repository']]
        path=c['truth']['identities'][0]['file'];quote=files[path].splitlines()[250].strip()
        item=f'symbol:{path}#query::source::special_kind:function'
        declaration=dict(file=path,line=251,quote=quote)
        a['items'].append(item);a['evidence'].append(dict(item=item,**declaration))
        self.assertFalse(study.answer_metrics(a,c,files,[])['objective_pass'])
        lines=files[path].splitlines(); excerpt='\n'.join(lines[250:275])+'\n'
        evidence=dict(revision='head',**declaration,end_line=275,
                      excerpt_sha256=study.hashlib.sha256(excerpt.encode()).hexdigest())
        support=[dict(item=item,accepted=True,rationale='TEST-ONLY source review of file-local helper declaration and relevance',
                      declaration=declaration,identity_evidence=[evidence])]
        self.assertTrue(study.answer_metrics(a,c,files,support)['objective_pass'])
        support[0]['declaration']['file']='src/wrong.rs'
        with self.assertRaises(ValueError):study.answer_metrics(a,c,files,support)

    def fixtures(self):
        episodes=[];audits=[]
        for slot in self.protocol['order']:
            c=next(c for c in self.data['cases'] if c['id']==slot['case_id'])
            e=dict(run_id='TEST-ONLY-'+str(slot['order']),artifact_sha256=study.digest(slot),
                   request=dict(slot),status='ok',error=None,answer=answer(c),calls=[],
                   timing=dict(wall_ms=100+slot['order'],setup_ms=30,provider_ms=70+slot['order'],
                               plugin_install_ms=10 if slot['arm']=='graph' else 0,graph_sync_ms=0,preflight_ms=5),
                   setup_output_bytes=100 if slot['arm']=='graph' else 0,output_bytes=10,usage=None,usage_raw=None)
            episodes.append(e);audits.append(audit_for(e,c))
        return dict(study_kind='test-only',episodes=episodes),dict(schema_version=1,audits=audits)

    def test_full_semantics_and_all_denominators(self):
        bundle,audits=self.fixtures();report=study.score(self.data,self.protocol,self.files,bundle,audits)
        self.assertFalse(report['effectiveness_evidence']);self.assertFalse(report['follow_up_consideration'])
        for arm in ('baseline','graph'):
            self.assertEqual(report['groups']['all/'+arm]['total'],6)
            self.assertEqual(report['groups']['held-out/'+arm]['correct'],4)
            self.assertEqual(report['groups']['development/'+arm]['correct'],2)
            self.assertIsNone(report['groups']['all/'+arm]['usage']['cost_usd']['sum'])
        self.assertTrue(all(p['both_correct'] for p in report['pairs']))
        for status in ('failed','invalid','timeout'):
            b,a=self.fixtures();b['episodes'][0].update(status=status,error=dict(code=status,message='TEST-ONLY'),answer=None)
            a['audits'].pop(0)
            report=study.score(self.data,self.protocol,self.files,b,a)
            self.assertEqual(report['groups']['all/baseline']['total'],6)
            self.assertEqual(report['groups']['all/baseline']['correct'],5)
            self.assertEqual(report['groups']['all/baseline']['statuses'][status],1)
            self.assertIsNone(report['pairs'][0]['wall_delta_ms'])

    def test_semantic_review_is_not_inferred_from_identities(self):
        b,a=self.fixtures();a['audits'][0]['judgments'][0]['pass']=False
        study.seal(a['audits'][0],'audit_sha256')
        report=study.score(self.data,self.protocol,self.files,b,a)
        self.assertTrue(report['episodes'][0]['quality']['objective_pass'])
        self.assertFalse(report['episodes'][0]['correct'])
        for mutate in (lambda a:a['audits'].pop(),lambda a:a['audits'][0].update(reviewer=''),
                       lambda a:a['audits'][0].update(artifact_sha256='0'*64),
                       lambda a:a['audits'][0]['judgments'][0].update(answer_quote='ABSENT'),
                       lambda a:a['audits'][0]['judgments'][0].update(source_evidence=[])):
            b,a=self.fixtures();mutate(a)
            for audit in a['audits']:study.seal(audit,'audit_sha256')
            with self.assertRaises(ValueError):study.score(self.data,self.protocol,self.files,b,a)

    def test_observed_usage_and_adoption_are_not_imputed(self):
        b,a=self.fixtures();b['episodes'][0]['usage']=dict(input_tokens=10,output_tokens=5,cost_usd=None,source='fixture')
        b['episodes'][0]['usage_raw']=dict(cached_input_tokens=2)
        b['episodes'][1]['calls']=[dict(tool='graph_search',status='failed'),dict(tool='graph_status',status='ok')]
        report=study.score(self.data,self.protocol,self.files,b,a)
        usage=report['groups']['all/baseline']['usage']['input_tokens']
        self.assertEqual(usage,dict(observed=1,total=6,observed_sum=10,sum=None))
        adoption=report['groups']['all/graph']['adoption']
        self.assertEqual(adoption['attempted'],1);self.assertEqual(adoption['calls'],2)
        self.assertEqual(adoption['successful_calls'],1);self.assertEqual(adoption['denominator'],6)

    def test_profile_requests_counterbalance_and_no_hidden_truth(self):
        pin=dict(commit='1'*40,backend_sha256='2'*64,orbit_sha256='3'*64,inventory_sha256='4'*64,skill_sha256='5'*64)
        requests=study.requests(self.data,self.protocol,self.lock,pin)
        self.assertEqual(len(requests),12)
        self.assertEqual(sum(requests[n]['arm']=='baseline' for n in range(0,12,2)),3)
        for n in range(0,12,2):
            first,second=requests[n:n+2]
            for key in set(first)-{'order','arm','tools','request_sha256'}:self.assertEqual(first[key],second[key])
            self.assertNotIn('truth',first);self.assertNotIn('rubric',first)
            self.assertEqual(first['limits'],study.source.LIMITS)
            self.assertEqual(first['setup_limits'],dict(wall_ms=60000,output_bytes=8388608))
            for r in (first,second):self.assertEqual(study.runner.validate_request(r),r)
        changed=copy.deepcopy(requests[0]);changed['limits']['tool_calls']=61
        with self.assertRaises(ValueError):study.runner.validate_request(changed)

    def test_allowlist_and_export_no_overwrite_or_partial_admission(self):
        for name in ('docs/a.rs','src/.env','src/skills/a.py','src/credentials/a.py','evals/truth.py','src/answers/a.py'):
            self.assertFalse(study.source.included(name) and study.plugin.source_path_allowed(name))
        files={'orbit-graph':{'src/lib.rs':'fn source_only() {}\n'}}
        manifest=dict(files={'src/lib.rs':study.hashlib.sha256(files['orbit-graph']['src/lib.rs'].encode()).hexdigest()},
                      content_revision=study.digest(files['orbit-graph']))
        lock=dict(snapshots={'orbit-graph':manifest})
        with tempfile.TemporaryDirectory(dir=self.scratch) as tmp:
            dest=Path(tmp)/'export'
            study.export(self.data,lock,files,dest);study.check_export(self.data,lock,dest)
            before=(dest/'complete.json').read_bytes()
            with self.assertRaises(ValueError):study.export(self.data,lock,files,dest)
            self.assertEqual(before,(dest/'complete.json').read_bytes())
            (dest/'orbit-graph/src/lib.rs').write_text('tampered')
            with self.assertRaises(ValueError):study.check_export(self.data,lock,dest)
            partial=Path(tmp)/'partial';original=study.source.write_new
            def fail_marker(path,value):
                if path.name=='complete.json':raise OSError('TEST-ONLY injected disk failure')
                return original(path,value)
            with mock.patch.object(study.source,'write_new',fail_marker),self.assertRaises(OSError):
                study.export(self.data,lock,files,partial)
            self.assertTrue((partial/'orbit-graph/src/lib.rs').exists())
            self.assertFalse((partial/'complete.json').exists())
            with self.assertRaises(OSError):study.check_export(self.data,lock,partial)
            with self.assertRaises(ValueError):study.export(self.data,lock,files,partial)

    def test_hand_constructed_raw_cannot_pass_profile_replay(self):
        b,_=self.fixtures()
        with tempfile.TemporaryDirectory(dir=self.scratch) as tmp:
            Path(tmp,'episode.json').write_text(json.dumps(b['episodes'][0]))
            with self.assertRaises(ValueError):study.profile.load_episode(tmp,diagnostic=True)
        packets=study.review_packets(b,self.data)
        for p in packets['packets']:
            self.assertNotIn('arm',p);self.assertNotIn('judgments',p)



@unittest.skipUnless(os.environ.get('STUDY_FAKE_PROFILE') == '1',
                     'set STUDY_FAKE_PROFILE=1 and STUDY_EXPORT plus AGENT_EVAL_ORBIT/ORBIT_GRAPH for full fake profile smoke')
class ProfileSmoke(unittest.TestCase):
    """Real CLI/capture path, local scripted provider, diagnostic only. Retain evidence."""
    def test_twelve_requests_freeze_capture_adapt_review_score(self):
        import shutil
        import sys
        data, protocol, lock = study.frozen()
        source_export = Path(os.environ['STUDY_EXPORT']).resolve()
        study.check_export(data,lock,source_export)
        scratch=study.REPO/'.orbit/tmp/study-profile-smoke'
        scratch.mkdir(parents=True,exist_ok=True)
        work=Path(tempfile.mkdtemp(dir=scratch))
        tool=study.REPO/'scripts/agent-eval/eval_runner.py'
        fake=study.REPO/'scripts/agent-eval/tests/fake_codex.py'
        orbit=Path(os.environ['AGENT_EVAL_ORBIT']).resolve()
        backend=Path(os.environ['AGENT_EVAL_ORBIT_GRAPH']).resolve()
        rg=Path(shutil.which('rg')).resolve()
        roots=Path(os.environ.get('STUDY_SOURCE_ROOT','/home/daniel/workspace/constellation/codebases'))
        graph_repo=roots/'orbit-graph'
        commit=study.source.git(study.REPO,'rev-parse','HEAD').decode().strip()
        def cli(script,*args,extra=None,expected=0):
            env=dict(os.environ, HOME=str(work))
            env.update(extra or {})
            command=[sys.executable,'-B',str(script),*map(str,args)]
            result=study.runner.broker.run_child(command,str(study.REPO),env,180,capture_limit=32*1024*1024)
            self.assertEqual(result['exit_code'],expected,(command,result))
            self.assertIsNone(result['stopped'],result)
            return json.loads(result['stdout']) if result['stdout'].strip() else None
        inspection=cli(tool,'plugin-inspect','--plugin-repo',graph_repo,'--plugin-commit',commit,
                       '--orbit',orbit,'--orbit-graph',backend,'--out',work/'inspection','--containment','none')
        study.source.write_new(work/'inspection.json',inspection)
        version=lambda binary:study.source.capture([str(binary),'--version'],{'PATH':'/usr/bin:/bin','HOME':str(work)}).decode().strip()
        hashes={p:study.runner.sha256_file(tool.parent/p) for p in ('eval_runner.py','eval_broker.py','plugin_profile.py')}
        runtime=dict(schema_version=1,study_kind='test-only',operator='TEST-ONLY fixture',
                     frozen_at='2026-10-03T00:00:00Z',approval_reference='TEST-ONLY offline smoke; no live authority',
                     model=dict(provider='codex-cli',name='fake-model',version=version(fake),settings={}),
                     provider_binary_sha256=study.runner.sha256_file(fake),harness=hashes,
                     baseline_tool_versions=dict(read=study.runner.broker.BROKER_NAME+' '+study.runner.broker.BROKER_VERSION+' sha256:'+hashes['eval_broker.py'],
                       rg=version(rg).split(' (')[0]+' sha256:'+study.runner.sha256_file(rg),
                       git=version('/usr/bin/git')+' sha256:'+study.runner.sha256_file('/usr/bin/git')),
                     resource_limits={'memory.max':4294967296,'pids.max':256},
                     binary_paths=dict(provider=str(fake),orbit=str(orbit),backend=str(backend),git='/usr/bin/git',rg=str(rg),python='/usr/bin/python3',bwrap='/usr/bin/bwrap'))
        study.source.write_new(work/'runtime.json',runtime)
        cli(study.ROOT/'eval.py','freeze','--runtime',work/'runtime.json','--inspection',work/'inspection.json',
            '--export',source_export,'--destination',work/'frozen')
        before=(work/'frozen/freeze.json').read_bytes()
        cli(study.ROOT/'eval.py','freeze','--runtime',work/'runtime.json','--inspection',work/'inspection.json',
            '--export',source_export,'--destination',work/'frozen',expected=1)
        self.assertEqual(before,(work/'frozen/freeze.json').read_bytes())
        plan=study.load(work/'frozen/plan.json');episodes=[];directories=[];audits=[]
        for request in plan['requests']:
            n=request['order'];case=next(c for c in data['cases'] if c['id']==request['case_id']);a=answer(case)
            if n==3: # Partial identity coverage, semantic prose still correct.
                a['items'].pop();a['evidence'].pop()
            if n==4: # Wrong extra identity.
                a['items'][0]+='_wrong';a['evidence'][0]['item']=a['items'][0]
            if n==5: # Valid module-qualified aliases.
                for j,i in enumerate(i for i in case['truth']['identities'] if i['required']):
                    a['items'][j]=i['aliases'][0];a['evidence'][j]['item']=i['aliases'][0]
            if n==6:a['evidence'][0]['quote']='wrong citation'
            steps=[dict(call='graph_version',arguments={})] if request['arm']=='graph' else [dict(call='read',arguments={'path':case['truth']['identities'][0]['file']})]
            steps.append(dict(final='not JSON' if n==1 else json.dumps(a)))
            script=dict(steps=steps,exit_code=7 if n==2 else 0)
            repository=roots/('orbit' if request['fixture']=='Orbit' else request['fixture'])
            out=work/f'run-{n:02}';directories.append(out)
            args=['run','--request',work/f'frozen/request-{n:02}.json','--head',source_export/request['fixture'],
                  '--base',source_export/request['fixture'],'--source-repo',repository,'--out',out,
                  '--codex',fake,'--model','fake-model','--rg',rg,'--containment','none','--auth-file','none',
                  '--provider-env','FAKE_CODEX_SCRIPT_JSON']
            if request['arm']=='graph':args+=['--plugin-repo',graph_repo,'--orbit',orbit,'--orbit-graph',backend]
            cli(tool,*args,extra={'FAKE_CODEX_SCRIPT_JSON':json.dumps(script)},expected=0)
            episode=study.load(out/'episode.json');episodes.append(episode)
            self.assertEqual(episode['status'], 'invalid' if n==1 else 'failed' if n==2 else 'ok')
            if episode['status']=='ok':audits.append(audit_for(episode,case))
        frozen=study.load(work/'frozen/freeze.json')
        custody=dict(operator='TEST-ONLY fixture',attested_at='2026-10-03T00:00:00Z',freeze_sha256=frozen['freeze_sha256'],
                     captures={a['run_id']:a['artifact_sha256'] for a in episodes},attestation='TEST-ONLY original local fake runner captures; uncontained; never agent evidence')
        study.source.write_new(work/'custody.json',custody)
        raw_args=[v for d in directories for v in ('--raw',d)]
        cli(study.ROOT/'eval.py','adapt','--frozen',work/'frozen',*raw_args,'--custody',work/'custody.json','--destination',work/'bundle.json')
        cli(study.ROOT/'eval.py','review-packets','--frozen',work/'frozen','--bundle',work/'bundle.json','--destination',work/'packets.json')
        study.source.write_new(work/'audits.json',dict(schema_version=1,audits=audits))
        repo_args=[v for name in data['repositories'] for v in ('--repo',name+'='+str(roots/('orbit' if name=='Orbit' else name)))]
        report=cli(study.ROOT/'eval.py','score',*repo_args,'--frozen',work/'frozen','--bundle',work/'bundle.json','--audits',work/'audits.json')
        study.source.write_new(work/'report.json',report)
        self.assertFalse(report['effectiveness_evidence']);self.assertFalse(report['follow_up_consideration'])
        self.assertEqual(len(report['episodes']),12)
        self.assertEqual(sum(r['correct'] for r in report['episodes']),7)
        self.assertEqual(report['groups']['all/baseline']['total'],6)
        self.assertEqual(report['groups']['all/graph']['total'],6)
        # Cannot relabel this captured diagnostic fixture as live evidence.
        for d in directories:
            with self.assertRaises(ValueError):study.profile.load_episode(d,diagnostic=False)
        print('TEST-ONLY full profile evidence: '+str(work),flush=True)


if __name__=='__main__':
    unittest.main()
