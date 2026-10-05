"""Public study CLI with original shared-harness fake-provider schema3 captures.

Private disposable installations only. These controls are never study evidence.
"""
import copy
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest

from test_eval import study, answer, review, run_cli, HERE


class OriginalCaptureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.corpus, cls.protocol, cls.lock = study.documents()
        cls.export = Path(os.environ['GRAPH_EFFECTIVENESS_EXPORT']).resolve(strict=True)
        cls.views = study.check_export(cls.corpus, cls.lock, cls.export)
        scratch = study.REPO / '.orbit/tmp/graph-effectiveness-bridge'
        scratch.mkdir(parents=True, exist_ok=True, mode=0o700)
        cls.work = Path(tempfile.mkdtemp(dir=scratch))
        cls.orbit = Path(os.environ['GRAPH_EFFECTIVENESS_ORBIT']).resolve(strict=True)
        cls.graph = Path(os.environ['GRAPH_EFFECTIVENESS_GRAPH']).resolve(strict=True)
        cls.codebases = Path(os.environ['GRAPH_EFFECTIVENESS_CODEBASES']).resolve(strict=True)
        cls.provider = study.REPO / 'scripts/agent-eval/tests/fake_codex.py'
        cls.rg = shutil.which('rg')
        if not cls.rg:
            raise RuntimeError('original capture controls require an explicit rg binary')
        cls.pin = cls.harness('plugin-inspect', '--plugin-repo', study.REPO, '--plugin-commit',
                              study.source.git(study.REPO, 'rev-parse', 'HEAD').decode().strip(),
                              '--orbit', cls.orbit, '--orbit-graph', cls.graph,
                              '--containment', 'none', '--out', cls.work/'inspect')['pin']
        cls.plan = cls.cli('study','plan','--study-id','TEST-ONLY-original-bridge')
        cls.plan_path = cls.save('plan.json', cls.plan)
        cls.req = cls.cli('study','requests','--plan',cls.plan_path,'--pin',cls.save('pin.json',cls.pin))['requests']
        cls.captures = {}
        # A matched pair with input/output available but no reported total.
        fields = dict(input_tokens=30529, output_tokens=990, cached_input_tokens=400, reasoning_output_tokens=300)
        cls.capture(0, usage=fields)
        cls.capture(1, usage=dict(fields, input_tokens=71040, output_tokens=1408))
        # Preserve failure, completely missing usage, multi-turn diagnostic sums,
        # and inconsistent usage without dropping any quality slots.
        cls.capture(2, omit_completed=True)
        cls.capture(3, usage=None)
        cls.capture(4, turns=True, usage=dict(input_tokens=20, output_tokens=6))
        cls.capture(5, usage=dict(input_tokens=10, cached_input_tokens=20, output_tokens=3, total_tokens=13))
        e=cls.captures[0][1]
        cls.rp=dict(schema_version=3,profile=study.PROSPECTIVE_PROFILE,model=e['model'],
                    provider_binary_sha256=e['provider']['binary_sha256'],harness=e['harness'],requests=cls.req)
        prepared=cls.cli('study','requests','--plan',cls.plan_path,'--pin',cls.work/'pin.json',
                         '--model-pin',cls.save('model-pin.json',{k:cls.rp[k] for k in ('model','provider_binary_sha256')}))
        if prepared['request_plan'] != cls.rp:
            raise AssertionError('public request preparation differs from actual capture pins')
        cls.rp_path=cls.save('request-plan.json',cls.rp)
        cls.truth=dict(schema_version=1,reviews=[review(p) for p in study.scoring.truth_packets(cls.corpus,cls.lock)['packets']],adjudications=[])
        cls.truth_path=cls.save('TEST-ONLY-truth.json',cls.truth)

    @classmethod
    def save(cls,name,value):
        p=cls.work/name;p.write_text(json.dumps(value)+'\n');return p

    @classmethod
    def cli(cls,*args,ok=True):
        r=run_cli(HERE/'eval.py',args,cls.work)
        if ok and (r.returncode != 0 or r.stderr):
            raise AssertionError((r.returncode,r.stdout,r.stderr))
        if not ok and (r.returncode == 0 or r.stdout):
            raise AssertionError((r.returncode,r.stdout,r.stderr))
        return json.loads(r.stdout if ok else r.stderr)

    @classmethod
    def harness(cls,*args,script=None):
        env=dict(PATH='/usr/bin:/bin',HOME=str(cls.work),LC_ALL='C.UTF-8',TZ='UTC',
                 XDG_CONFIG_HOME=str(cls.work),PYTHONDONTWRITEBYTECODE='1')
        if script is not None:
            env['FAKE_CODEX_SCRIPT_JSON']=json.dumps(script)
        result=study.runner.broker.run_child([sys.executable,'-B',str(study.REPO/'scripts/agent-eval/eval_runner.py'),*map(str,args)],
                                            str(cls.work),env,60,capture_limit=8*1024*1024)
        if not study.plugin.clean(result):
            raise AssertionError(study.plugin.evidence(result))
        return json.loads(result['stdout'])

    @classmethod
    def capture(cls,order,usage='default',omit_completed=False,turns=False):
        slot=cls.plan['slots'][order];c=next(c for c in cls.corpus['cases'] if c['id']==slot['case_id'])
        path=cls.work/f'capture-{order}'
        steps=[dict(call='read',arguments={'path':next(iter(cls.lock['snapshots'][c['head_view']]['files']))},omit_completed=omit_completed)]
        if slot['arm']=='graph':
            steps += [dict(call='graph_maintain',arguments={'operation':'graph_sync'}),dict(call='graph_search',arguments={'query':'pub'})]
        if turns:
            steps += [{'emit':dict(type='turn.completed',usage=dict(input_tokens=10,output_tokens=3))},
                      {'emit':dict(type='turn.started',turn_id='second')}]
        steps += [{'final':json.dumps(answer(c))}]
        cls.harness('run','--request',cls.save(f'request-{order}.json',cls.req[order]),
                    '--head',cls.export/'views'/c['head_view'],'--base',cls.export/'views'/c['base_view'],
                    '--source-repo',cls.codebases/c['repository'],'--plugin-repo',study.REPO,
                    '--orbit',cls.orbit,'--orbit-graph',cls.graph,'--codex',cls.provider,
                    '--model','TEST-ONLY-fake','--rg',cls.rg,'--containment','none','--auth-file','none',
                    '--provider-env','FAKE_CODEX_SCRIPT_JSON','--truth-path',HERE,'--out',path,
                    script=dict(steps=steps,usage=usage))
        cls.captures[order]=(path,study.load(path/'episode.json'))

    def adapt(self,orders=(0,1,2,3,4,5),extra=(),refusals=None,ok=True):
        selected=[self.captures[n][0] for n in orders]
        refusals=refusals if refusals is not None else [dict(order=s['order'],status='setup_refused',
            error=dict(code='TEST-ONLY-setup',message='TEST-ONLY control, no measured attempt'),
            operator='TEST-ONLY operator',evidence='TEST-ONLY external refusal record',elapsed_ms=10)
            for s in self.plan['slots'] if s['order'] not in orders]
        args=['study','adapt','--plan',self.plan_path,'--request-plan',self.rp_path,'--refusals',self.save('refusals.json',refusals)]
        for p in selected:args += ['--raw',p]
        return self.cli(*args,*extra,ok=ok)

    def test_mixed_original_pair_failure_null_usage_and_scoring(self):
        bundle=self.adapt()
        self.assertEqual(len(bundle['records']),48)
        for n in range(6):self.assertEqual(bundle['records'][n]['episode'],self.captures[n][1])
        self.assertEqual(bundle['records'][2]['episode']['status'],'failed')
        self.assertEqual(bundle['records'][2]['episode']['error']['code'],'telemetry_mismatch')
        b=self.save('bundle.json',bundle)
        packets=self.cli('study','review','--plan',self.plan_path,'--export',self.export,'--bundle',b)
        doc=dict(schema_version=1,reviews=[review(p) for p in packets['packets']],adjudications=[])
        args=['study','score','--plan',self.plan_path,'--export',self.export,'--bundle',b,
              '--review-packets',self.save('packets.json',packets),'--reviews',self.save('reviews.json',doc)]
        pending=self.cli(*args)
        self.assertIsNone(pending['attempts'][0]['correct'])
        scored=self.cli(*args,'--truth-reviews',self.truth_path)
        self.assertTrue(scored['attempts'][0]['correct']);self.assertTrue(scored['attempts'][1]['correct'])
        self.assertFalse(scored['attempts'][2]['correct'])
        self.assertFalse(scored['effectiveness_evidence'])
        self.assertEqual(scored['paired']['total_pairs'],24)
        self.assertIsNone(scored['paired']['paired_token_delta'])
        self.assertEqual(scored['paired']['missing_token_pairs'],24)
        for key in ('input_tokens','output_tokens','cached_input_tokens','reasoning_tokens'):
            comparison=scored['paired']['token_fields'][key]
            self.assertEqual(comparison['observed_pairs'],1)
            self.assertEqual(comparison['missing_pairs'],23)
        for n in (0,1):
            row=scored['attempts'][n];t=self.captures[n][1]['telemetry']
            self.assertIsNone(row['usage']['total_tokens'])
            self.assertIsNone(t['usage']['fields']['total_tokens']['observed_sum'])
            self.assertIsNone(t['usage']['derived']['uncached_input_tokens'])
            self.assertIsNone(row['usage']['cost_usd'])
            self.assertEqual(row['tool_attempts'],t['attempts'])
            self.assertEqual(row['timing']['wall_ms'],row['timing']['setup_ms']+row['timing']['provider_ms'])
            self.assertEqual(row['timing']['preflight_outside_wall_ms'],t['timing']['outside_wall']['preflight_ms'])
            self.assertEqual(row['timing']['preflight_within_setup_ms'],t['timing']['overlapping']['preflight_within_setup_ms'])
        for n in (3,4,5):
            self.assertTrue(scored['attempts'][n]['correct'])
            self.assertTrue(all(v is None for v in scored['attempts'][n]['usage'].values()))
        self.assertEqual(scored['attempts'][4]['telemetry']['usage']['fields']['input_tokens']['observed_sum'],30)
        self.assertEqual(scored['attempts'][5]['telemetry']['usage']['fields']['total_tokens']['coverage'],'inconsistent')
        rebuilt=study.scoring.review_packets(self.corpus,self.plan,bundle,packets)
        self.assertEqual(packets,rebuilt)
        self.assertNotEqual(packets['packets'][0]['attempt_blind_id'],study.digest(self.plan['slots'][0]['attempt_id']))
        contradiction=copy.deepcopy(self.truth);contradiction['reviews'][0]=review(study.scoring.truth_packets(self.corpus,self.lock)['packets'][0],'contradicted')
        refused=self.cli(*args,'--truth-reviews',self.save('contradiction.json',contradiction),ok=False)
        self.assertIn('contradict',refused['error']['message'])

    def test_mixed_cohort_tampering_relabels_and_runtime_pins_refuse(self):
        self.assertEqual(self.adapt((0,))['records'][0]['episode']['status'],'ok')
        out,original=self.captures[0];raw=(out/'episode.json').read_bytes()
        edits=[('ledger',lambda e:e['telemetry']['tools'].update(attempts=99)),
               ('lifecycle',lambda e:e['cleanup'].update(pipes_closed=False)),
               ('schema2',lambda e:e.update(schema_version=2,profile=study.plugin.PROFILE)),
               ('source',lambda e:e['source_provenance']['head']['selected_blobs'].update({'src/forged.rs':'0'*40}))]
        try:
            for name,edit in edits:
                with self.subTest(name=name):
                    bad=copy.deepcopy(original);edit(bad);study.runner.seal(bad,'artifact_sha256')
                    (out/'episode.json').write_text(json.dumps(bad))
                    self.adapt((0,),ok=False)
        finally:(out/'episode.json').write_bytes(raw)
        original_rp=self.rp_path.read_bytes()
        try:
            for field in ('harness','provider_binary_sha256','requests'):
                bad=copy.deepcopy(self.rp)
                if field=='harness':bad['harness']['telemetry.py']='0'*64
                elif field=='requests':bad['requests'][0]['schema_version']=2
                else:bad[field]='0'*64
                self.rp_path.write_text(json.dumps(bad))
                self.adapt((0,),ok=False)
        finally:self.rp_path.write_bytes(original_rp)
        self.adapt((0,0),ok=False)
        self.adapt((),refusals=[],ok=False)
        self.adapt((0,),extra=('--study-kind','agent'),ok=False)
        bundle=self.adapt((0,));bundle.update(study_kind='agent');study.seal(bundle,'bundle_sha256')
        self.cli('study','review','--plan',self.plan_path,'--export',self.export,'--bundle',self.save('promoted.json',bundle),ok=False)

    def test_freeze_requires_complete_truth_and_external_qualification(self):
        empty=self.save('empty.json',{})
        args=['study','freeze','--plan',self.plan_path,'--request-plan',self.rp_path,'--export',self.export,
              '--qualification',empty,'--custody',empty]
        result=self.cli(*args,'--truth-reviews',self.save('pending.json',dict(schema_version=1,reviews=[],adjudications=[])),ok=False)
        self.assertIn('pending',result['error']['message'])
        result=self.cli(*args,'--truth-reviews',self.truth_path,ok=False)
        self.assertIn('qualification',result['error']['message'])

    def attestations(self,rp):
        # Self-reported test documents exercise the external-attestation boundary;
        # they do not qualify this host, authenticate reviewers or authorize dispatch.
        evidence=self.save('TEST-ONLY-attestation-evidence.json',{'kind':'TEST-ONLY schema control, never host qualification'})
        files=[dict(path=str(evidence),sha256=study.sha(evidence.read_bytes()))]
        q=study.seal(dict(schema_version=1,operator='TEST-ONLY external attester',recorded_at='2026-10-05T00:00:00Z',
              method='strict-schema3-namespace-client',verdict='qualified',request_plan_sha256=study.digest(rp),
              lock_sha256=study.digest(self.lock),tool_versions={k:self.captures[0][1]['tool_versions'][k] for k in ('read','rg','git')},
              resource_limits={'memory.max':1024**3,'pids.max':128},bwrap={'version':'TEST-ONLY namespace pin','sha256':'b'*64},
              identity_frontend_sha256=study.sha(study.runtime.PARSER.read_bytes()),evidence=files),'qualification_sha256')
        custody=study.seal(dict(schema_version=1,study_kind='agent',operator='TEST-ONLY operator',recorded_at='2026-10-05T00:01:00Z',
              plan_sha256=self.plan['plan_sha256'],request_plan_sha256=study.digest(rp),truth_reviews_sha256=study.digest(self.truth),
              qualification_sha256=q['qualification_sha256'],attestations=dict(schedule_frozen_before_output=True,
              truth_review_identity_independently_attested=True,serial_dispatch=True,original_capture_custody=True,
              source_transfer_authorized=True,non_synthetic_provider=True),evidence=files),'custody_sha256')
        return q,custody

    def test_external_freeze_bindings_and_strict_live_capture_refusal(self):
        def freeze(rp,q,custody,ok=True):
            return self.cli('study','freeze','--plan',self.plan_path,'--request-plan',self.save('freeze-rp.json',rp),
                '--export',self.export,'--truth-reviews',self.truth_path,'--qualification',self.save('qualification.json',q),
                '--custody',self.save('custody.json',custody),ok=ok)
        q,custody=self.attestations(self.rp)
        result=freeze(self.rp,q,custody,ok=False)
        self.assertIn('synthetic',result['error']['message'])
        # A formal freeze can bind external assertions but cannot certify them.
        # The ensuing original uncontained fixture must still fail strict replay.
        rp=copy.deepcopy(self.rp);rp['provider_binary_sha256']='9'*64
        rp['model'].update(name='external-declared-model',version='external-declared-client')
        q,custody=self.attestations(rp)
        formal=freeze(rp,q,custody)
        self.assertIn('externally attested',formal['boundary'])
        original=self.rp_path.read_bytes()
        try:
            self.rp_path.write_text(json.dumps(rp))
            refused=self.adapt((0,),extra=('--study-kind','agent','--freeze',self.save('freeze.json',formal),'--export',self.export),ok=False)
            self.assertTrue(any(text in refused['error']['message'] for text in ('ceilings','uncontained')),
                            refused['error']['message'])
        finally:self.rp_path.write_bytes(original)
        for name in ('custody','qualification','evidence'):
            with self.subTest(name=name):
                badq,badc=copy.deepcopy(q),copy.deepcopy(custody)
                if name=='custody':
                    badc['attestations']['schedule_frozen_before_output']=False;study.seal(badc,'custody_sha256')
                elif name=='qualification':
                    badq['lock_sha256']='0'*64;study.seal(badq,'qualification_sha256')
                else:
                    badq['evidence'][0]['sha256']='0'*64;study.seal(badq,'qualification_sha256')
                freeze(rp,badq,badc,ok=False)


if __name__ == '__main__':unittest.main()
