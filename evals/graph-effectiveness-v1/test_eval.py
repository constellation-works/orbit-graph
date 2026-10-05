"""Real offline CLI and source-bound scoring tests. No provider or plugin installs."""
import copy
import importlib.util
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location('graph_effectiveness', HERE / 'eval.py')
study = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = study
SPEC.loader.exec_module(study)


def answer(case):
    items, evidence = [], []
    for identity in case['truth']['identities']:
        s = identity['selector']
        kind = 'class' if s['kind'] in ('struct','class') else 'method' if identity['owner'] else 'function'
        item = f"symbol:{s['file']}#{s['name']}:{kind}"
        items.append(item)
        evidence.append(dict(item=item,file=s['file'],line=s['line'],quote=s['citation']['quote'].strip()))
    return dict(items=items,evidence=evidence,abstain=not items,
                reason='TEST-ONLY '+ ' '.join(x['expected'] for x in case['truth']['claims']))


def fixture(corpus, protocol, plan):
    cases = {c['id']:c for c in corpus['cases']}
    records = []
    for s in plan['slots']:
        e = dict(status='ok',error=None,answer=answer(cases[s['case_id']]),calls=[{'tool':'read','status':'ok'}],
                 timing=dict(wall_ms=1000,setup_ms=200,provider_ms=800,graph_sync_ms=None,plugin_install_ms=0),
                 usage={'input_tokens':1000,'output_tokens':100},usage_raw={'cached_input_tokens':200},
                 output_bytes=100,setup_output_bytes=0)
        records.append(dict(order=s['order'],episode=e))
    return dict(schema_version=1,kind='offline-scoring-fixture-v1',study_kind='test-only',
                plan_sha256=plan['plan_sha256'],records=records)


def review(p, verdict='verified', reviewer='TEST-ONLY independent source reviewer'):
    source = study.source
    identity = p.get('identity')
    result = dict(packet_sha256=p['packet_sha256'],reviewer=reviewer,reviewed_at='2026-10-04T23:00:00Z',
                  method='source-wide-independent' if p['kind'].endswith('identity') else 'diff-source-review' if p['kind']=='truth-diff' else 'semantic-source-review',
                  blinding='source-only' if p['kind'].startswith('truth') else 'arm-blinded',verdict=verdict,
                  rationale='TEST-ONLY fixture exercises attributed review validation, not independent review of real agent results.',
                  answer_quote=None if p['kind'].startswith('truth') else 'TEST-ONLY',source_evidence=p['source_evidence'],
                  identity={k:identity[k] for k in ('owner','module','trait')} | {'declaration':identity['selector']} if identity else None,
                  initial_review_sha256=[])
    return source.seal(result,'review_sha256')


def run_cli(path, args, scratch):
    # The existing bounded, group-owning capture supervisor sweeps descendants
    # before reaping. The wrapper preserves both streams and the CLI's exit code.
    wrapper = '''import contextlib,io,json,runpy,sys
path=sys.argv[1];sys.argv=sys.argv[1:];out=io.StringIO();err=io.StringIO();code=0
with contextlib.redirect_stdout(out),contextlib.redirect_stderr(err):
 try: runpy.run_path(path,run_name="__main__")
 except SystemExit as result: code=result.code or 0
print(json.dumps(dict(exit_code=code,stdout=out.getvalue(),stderr=err.getvalue())))
'''
    env=dict(PATH='/usr/bin:/bin',HOME=str(scratch),LC_ALL='C.UTF-8',TZ='UTC',PYTHONDONTWRITEBYTECODE='1')
    value=json.loads(study.source.capture([sys.executable,'-B','-c',wrapper,str(path),*map(str,args)],env,bound=64*1024*1024))
    return subprocess.CompletedProcess(args,value['exit_code'],value['stdout'],value['stderr'])


class StudyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        value = os.environ.get('GRAPH_EFFECTIVENESS_EXPORT')
        if not value:
            raise RuntimeError('GRAPH_EFFECTIVENESS_EXPORT must explicitly name a verified full source export; see README')
        cls.export = Path(value).resolve(strict=True)
        cls.corpus, cls.protocol, cls.lock = study.documents()
        cls.views = study.check_export(cls.corpus,cls.lock,cls.export)
        cls.scratch = study.REPO / '.orbit/tmp/graph-effectiveness-tests'
        cls.scratch.mkdir(parents=True,exist_ok=True)
        cls.goldens = study.load(HERE/'cli-goldens.json')

    def setUp(self):
        self.work = Path(tempfile.mkdtemp(dir=self.scratch))
        # Scratch remains available as diagnostic evidence, entirely under .orbit/tmp.
        self.plan = study.schedule(self.corpus,self.protocol,'TEST-ONLY-study',1,['baseline','product'],'cold')

    def save(self,name,value):
        p=self.work/name;p.write_text(json.dumps(value)+'\n');return p

    def cli(self,*args,ok=True):
        r=run_cli(HERE/'eval.py',args,self.work)
        if ok:
            self.assertEqual(r.returncode,0,r.stderr)
            self.assertEqual(r.stderr,'')
            return json.loads(r.stdout)
        self.assertNotEqual(r.returncode,0,r.stdout)
        self.assertEqual(r.stdout,'')
        return json.loads(r.stderr)

    def prepare(self):
        plan=self.save('plan.json',self.plan)
        f=fixture(self.corpus,self.protocol,self.plan)
        return plan,self.save('fixture.json',f),f

    def test_all_cli_help_and_error_goldens(self):
        for args,expected in self.goldens['help'].items():
            r=run_cli(HERE/'eval.py',args.split(),self.work)
            self.assertEqual({'exit_code':r.returncode,'stdout':r.stdout,'stderr':r.stderr},expected)
        self.assertEqual(self.cli('--json','study','plan'),self.cli('study','plan','--json'))
        r=self.cli('study','plan','--cache','warm',ok=False)
        self.assertEqual(r,self.goldens['warm_error'])
        self.assertEqual(self.cli('study','plan','--treatments','baseline,skill_ablation',ok=False),self.goldens['ablation_error'])
        r=self.cli('study','plan','--export',self.export,ok=False)
        self.assertEqual(r['error']['code'],'usage_error')

    def test_real_validate_and_full_universe(self):
        r=self.cli('study','validate','--export',self.export)
        self.assertEqual(r['cases'],24)
        self.assertEqual(set(r['families'].values()),{4})
        self.assertFalse(r['runtime_admitted'])
        self.assertEqual(r['truth_admission']['admitted_cases'],0)
        self.assertEqual(r['truth_admission']['coverage']['unsupported'],48)
        self.assertEqual(r['truth_admission']['coverage']['adjudicated_pending'],48)
        self.assertEqual(sum(c['language']=='python' for c in self.corpus['cases']),8)
        self.assertEqual(sum(c['language']=='rust' for c in self.corpus['cases']),16)
        self.assertEqual(len(self.corpus['exposed_questions_excluded']),20)
        for view,manifest in self.lock['snapshots'].items():
            self.assertEqual(set(self.views[view]),set(manifest['files']))
            self.assertEqual(set(manifest['files']),set(manifest['blobs']))
            self.assertFalse(any(p.startswith('.orbit-plugin/') or p.startswith('evals/') for p in manifest['files']))

    def test_export_refuses_changes_symlinks_and_overwrite(self):
        one=self.work/'export';study.export(self.corpus,self.lock,self.views,one)
        study.check_export(self.corpus,self.lock,one)
        with self.assertRaises(ValueError):study.export(self.corpus,self.lock,self.views,one)
        root=one/'views'/self.corpus['cases'][0]['head_view']
        path=root/self.corpus['cases'][0]['truth']['identities'][0]['selector']['file']
        path.write_text(path.read_text()+'\n# changed\n')
        with self.assertRaisesRegex(ValueError,'export file set/bytes'):study.check_export(self.corpus,self.lock,one)
        two=self.work/'symlink-export';study.export(self.corpus,self.lock,self.views,two)
        path=two/'views'/self.corpus['cases'][0]['head_view']/self.corpus['cases'][0]['truth']['identities'][0]['selector']['file']
        path.unlink();path.symlink_to(HERE/'README.md')
        with self.assertRaisesRegex(ValueError,'symlink'):study.check_export(self.corpus,self.lock,two)

    def test_plan_repetitions_counterbalance_and_request_contract(self):
        plan=self.cli('study','plan','--study-id','TEST-ONLY-three','--repetitions',3)
        self.assertEqual(len(plan['slots']),144)
        for rep in range(1,4):
            first=[s for s in plan['slots'] if s['repetition']==rep and s['order']%2==0]
            for field in ('repository','family'):
                cases={c['id']:c for c in self.corpus['cases']}
                for group in {c[field] for c in self.corpus['cases']}:
                    arms=[s['arm'] for s in first if cases[s['case_id']][field]==group]
                    self.assertEqual(arms.count('baseline'),arms.count('graph'))
        pin=dict(commit='1'*40,backend_sha256='2'*64,orbit_sha256='3'*64,inventory_sha256='4'*64,skill_sha256='5'*64)
        r=self.cli('study','requests','--plan',self.save('p.json',plan),'--pin',self.save('pin.json',pin))
        self.assertEqual(len(r['requests']),144)
        for req in r['requests']:
            self.assertEqual(req['schema_version'],3)
            self.assertEqual(req['profile'],'installed-plugin-skill-v3')
        self.assertEqual(len({r['case_id'] for r in r['requests']}),72)
        self.assertFalse(r['runtime_admitted'])
        with self.assertRaises(ValueError):study.requests(self.corpus,self.protocol,self.lock,plan,{})

    def test_qualified_python_identity_citation_and_ambiguous_truth(self):
        root=self.work/'qualified';root.mkdir();(root/'a.py').write_text('def locate():\n    return 1\n')
        sel=dict(language='python',name='locate',file='a.py',line=1,kind='function',citation={'start_line':1,'end_line':1,'quote':'def locate():'})
        files={'a.py':(root/'a.py').read_text()}
        check=study.scoring.automated(root,files,[sel])['items'][0]
        self.assertEqual(check['outcome'],'verified');self.assertTrue(check['citation_ok'])
        wrong=copy.deepcopy(sel);wrong['citation']['quote']='def wrong():'
        check=study.scoring.automated(root,files,[wrong])['items'][0]
        self.assertEqual(check['outcome'],'verified');self.assertFalse(check['citation_ok'])
        wrong=copy.deepcopy(sel);wrong['line']=2
        self.assertEqual(study.scoring.automated(root,files,[wrong])['items'][0]['outcome'],'contradicted')
        (root/'b.py').write_text('def locate():\n    return 2\n');files['b.py']=(root/'b.py').read_text()
        self.assertEqual(study.scoring.automated(root,files,[sel])['items'][0]['outcome'],'ambiguous')
        project=study.identity.Project(root,python_files=['a.py','b.py'])
        with self.assertRaisesRegex(ValueError,'invalid required'):study.identity.score(project,[sel],[])
        sel['name']='a.locate'
        self.assertEqual(study.scoring.automated(root,files,[sel])['items'][0]['outcome'],'verified')
        (root/'c.py').write_text('if True:\n    value=1\n');files['c.py']=(root/'c.py').read_text()
        self.assertEqual(study.scoring.automated(root,files,[sel])['items'][0]['outcome'],'unsupported')
        wrong=copy.deepcopy(sel);wrong['name']='a..locate'
        self.assertEqual(study.scoring.automated(root,files,[wrong])['items'][0]['outcome'],'contradicted')

    def test_qualified_rust_and_full_multicrate_refusal(self):
        root=self.work/'rust';(root/'src').mkdir(parents=True)
        (root/'src/lib.rs').write_text('pub fn locate() -> u8 { 1 }\n')
        sel=dict(language='rust',name='crate::locate',file='src/lib.rs',line=1,kind='fn',citation={'start_line':1,'end_line':1,'quote':'pub fn locate() -> u8 { 1 }'})
        files={'src/lib.rs':(root/'src/lib.rs').read_text()}
        result=study.scoring.automated(root,files,[sel])['items'][0]
        self.assertEqual(result['outcome'],'verified',result)
        (root/'other/src').mkdir(parents=True);(root/'other/src/lib.rs').write_text('pub fn locate() {}\n')
        files['other/src/lib.rs']=(root/'other/src/lib.rs').read_text()
        self.assertEqual(study.scoring.automated(root,files,[sel])['items'][0]['outcome'],'unsupported')

    def test_invalid_truth_quotes_and_real_history(self):
        c=copy.deepcopy(self.corpus);c['cases'][0]['truth']['identities'][0]['selector']['citation']['quote']='wrong'
        with self.assertRaisesRegex(ValueError,'citation'):study.validate_truth(c,self.views)
        c=copy.deepcopy(self.corpus);c['cases'][0]['truth']['resolution']='ambiguous'
        with self.assertRaisesRegex(ValueError,'ambiguous required'):study.validate_truth(c,self.views)
        c=copy.deepcopy(self.corpus);c['cases'][-1]['base_view']=c['cases'][-1]['head_view']
        with self.assertRaisesRegex(ValueError,'actual change'):study.validate_truth(c,self.views)
        c=copy.deepcopy(self.corpus);a=next(x for x in c['cases'] if 'absence_search' in x['truth']);a['truth']['absence_search']['needle']='pub'
        with self.assertRaisesRegex(ValueError,'absence claim'):study.validate_truth(c,self.views)
        self.assertEqual(len([x for x in self.corpus['cases'] if x.get('diff_review')]),4)
        for case in self.corpus['cases']:
            if 'diff_review' in case:self.assertNotEqual(self.corpus['views'][case['base_view']]['commit'],self.corpus['views'][case['head_view']]['commit'])

    def test_independent_truth_review_and_disagreement_preservation(self):
        packets=study.scoring.truth_packets(self.corpus,self.lock)['packets']
        document=dict(schema_version=1,reviews=[review(p) for p in packets],adjudications=[])
        r=study.scoring.truth_admission(self.corpus,self.export,document,self.views)
        self.assertEqual(r['admitted_cases'],24);self.assertFalse(r['live_admitted'])
        self.assertEqual(r['independent_diff_review'],'verified')
        bad=copy.deepcopy(document);bad['reviews'][0]['reviewer']=packets[0]['author'];study.seal(bad['reviews'][0],'review_sha256')
        with self.assertRaisesRegex(ValueError,'independent reviewer'):study.scoring.checked_reviews(bad,packets,self.views,truth=True)
        p=packets[0];r1=review(p,'ambiguous','TEST-ONLY reviewer one');r2=review(p,'verified','TEST-ONLY reviewer two')
        d=dict(schema_version=1,reviews=[r1,r2],adjudications=[])
        self.assertEqual(study.scoring.checked_reviews(d,[p],self.views,truth=True)[p['packet_sha256']],'pending')
        adjudication=review(p,'verified','TEST-ONLY adjudicator');adjudication['initial_review_sha256']=[r1['review_sha256'],r2['review_sha256']];study.seal(adjudication,'review_sha256');d['adjudications']=[adjudication]
        self.assertEqual(study.scoring.checked_reviews(d,[p],self.views,truth=True)[p['packet_sha256']],'verified')
        self.assertEqual(d['reviews'],[r1,r2])
        d['adjudications'][0]['initial_review_sha256']=[];study.seal(d['adjudications'][0],'review_sha256')
        with self.assertRaisesRegex(ValueError,'preserved'):study.scoring.checked_reviews(d,[p],self.views,truth=True)

    def test_real_cli_score_reviews_missing_telemetry_and_failed_attempts(self):
        plan,path,f=self.prepare()
        # A genuine scorer-fixture path labels every output synthetic, never replayed.
        packets=self.cli('study','review','--plan',plan,'--export',self.export,'--fixture',path)
        reviews=dict(schema_version=1,reviews=[review(p) for p in packets['packets']],adjudications=[])
        truth=dict(schema_version=1,reviews=[review(p) for p in study.scoring.truth_packets(self.corpus,self.lock)['packets']],adjudications=[])
        report=self.cli('study','score','--plan',plan,'--export',self.export,'--fixture',path,'--review-packets',self.save('review-packets.json',packets),'--reviews',self.save('reviews.json',reviews),'--truth-reviews',self.save('truth.json',truth))
        self.assertEqual(report['arms']['baseline']['correct'],24)
        self.assertEqual(report['arms']['graph']['correct'],24)
        self.assertFalse(report['effectiveness_evidence']);self.assertEqual(report['decision'],'offline-development-only')
        self.assertEqual(report['arms']['graph']['usage']['cached_input_tokens']['sum'],4800)
        self.assertEqual(report['paired']['both_correct_time']['pairs'],24)
        # Change fixture prospectively, with new packets/reviews. No historical rescore.
        f['records'][0]['episode']['usage']={'input_tokens':None,'output_tokens':100}
        f['records'][1]['episode'].update(status='failed',error={'code':'TEST-ONLY failure','message':'TEST-ONLY setup/provider failure retained'},answer=None)
        f['records'][2]['episode'].update(status='timeout',error={'code':'TEST-ONLY timeout','message':'TEST-ONLY timeout retained'},answer=None)
        path=self.save('partial.json',f)
        packets=self.cli('study','review','--plan',plan,'--export',self.export,'--fixture',path)
        reviews=dict(schema_version=1,reviews=[review(p) for p in packets['packets']],adjudications=[])
        r=self.cli('study','score','--plan',plan,'--export',self.export,'--fixture',path,'--review-packets',self.save('partial-packets.json',packets),'--reviews',self.save('partial-reviews.json',reviews),'--truth-reviews',self.work/'truth.json')
        self.assertEqual(sum(v['attempts'] for v in r['arms'].values()),48)
        self.assertEqual(r['paired']['missing_token_pairs'],1)
        self.assertEqual(sum(v['outcomes'].get('failed',0)+v['outcomes'].get('timeout',0) for v in r['arms'].values()),2)
        self.assertEqual(r['paired']['both_correct_time']['pairs'],22)
        self.assertTrue(r['paired']['both_correct_time']['conditional'])
        self.assertEqual(r['arms']['baseline']['usage']['cost_usd']['observed'],0)
        no_truth=self.cli('study','score','--plan',plan,'--export',self.export,'--fixture',path,'--review-packets',self.work/'partial-packets.json','--reviews',self.work/'partial-reviews.json')
        self.assertGreater(sum(v['pending'] for v in no_truth['arms'].values()),0)

    def test_pending_reviews_wrong_quotes_and_written_identities(self):
        plan,path,f=self.prepare()
        r=self.cli('study','score','--plan',plan,'--export',self.export,'--fixture',path)
        self.assertEqual(r['arms']['baseline']['correct'],0);self.assertEqual(r['arms']['baseline']['pending'],24)
        target=f['records'][0]['episode']['answer'];target['evidence'][0]['quote']='wrong source quote'
        path=self.save('wrong.json',f)
        r=self.cli('study','score','--plan',plan,'--export',self.export,'--fixture',path)
        self.assertIs(r['attempts'][0]['correct'],False);self.assertFalse(r['attempts'][0]['identities'][0]['exact_source_quote'])
        f=fixture(self.corpus,self.protocol,self.plan);target=f['records'][0]['episode']['answer'];old=target['items'][0];target['items'][0]=old.replace('detect_facets','absent_wrong_identity');target['evidence'][0]['item']=target['items'][0]
        bundle=study.diagnostic_fixture(f,self.plan,self.protocol);packets=study.scoring.review_packets(self.corpus,self.plan,bundle)
        doc=dict(schema_version=1,reviews=[review(p,'contradicted' if p['kind']=='answer-identity' and p['target_id']=='submitted-0' and 'absent_wrong_identity' in p['answer_text'] else 'verified') for p in packets['packets']],adjudications=[])
        r=study.scoring.score(self.corpus,self.protocol,self.plan,bundle,self.export,self.views,doc,None,packets)
        self.assertIs(r['attempts'][0]['correct'],False);self.assertEqual(r['attempts'][0]['identities'][0]['adjudicated'],'contradicted')

    def test_missing_duplicate_fake_live_and_malformed_captures_refuse(self):
        plan,path,f=self.prepare()
        for records in [f['records'][:-1],[*f['records'][:-1],f['records'][0]]]:
            bad=copy.deepcopy(f);bad['records']=records
            r=self.cli('study','score','--plan',plan,'--export',self.export,'--fixture',self.save('bad-'+str(len(records))+str(records[-1]['order'])+'.json',bad),ok=False)
            self.assertIn('attempt',r['error']['message'])
        bad=copy.deepcopy(f);bad['study_kind']='agent'
        r=self.cli('study','score','--plan',plan,'--export',self.export,'--fixture',self.save('fake-live.json',bad),ok=False)
        self.assertIn('fabricated',r['error']['message'])
        raw=self.work/'malformed-raw';raw.mkdir();(raw/'episode.json').write_text('{}\n')
        with self.assertRaisesRegex(ValueError,'installed-plugin'):study.profile.load_episode(raw,diagnostic=True)
        bad=self.save('fake-bundle.json',study.seal(dict(schema_version=1,study_kind='agent',plan_sha256=self.plan['plan_sha256'],records=[]),'bundle_sha256'))
        r=self.cli('study','score','--plan',plan,'--export',self.export,'--bundle',bad,ok=False)
        self.assertIn('fabricated/live',r['error']['message'])

    def test_attributed_setup_refusals_and_fake_adaptation_boundary(self):
        pin=dict(commit='1'*40,backend_sha256='2'*64,orbit_sha256='3'*64,inventory_sha256='4'*64,skill_sha256='5'*64)
        req=study.requests(self.corpus,self.protocol,self.lock,self.plan,pin)
        rp=dict(schema_version=3,profile=study.PROSPECTIVE_PROFILE,model={'provider':'TEST-ONLY','name':'fake','version':'fake','settings':{}},
                provider_binary_sha256='6'*64,harness={p:study.sha((study.REPO/'scripts/agent-eval'/p).read_bytes()) for p in ('eval_runner.py','eval_broker.py','plugin_profile.py','reply_provenance.py')},requests=req)
        refusals=[dict(order=s['order'],status='setup_refused',error={'code':'TEST-ONLY capability unavailable','message':'TEST-ONLY refused before provider'},operator='TEST-ONLY operator',evidence='TEST-ONLY preflight refusal record',elapsed_ms=10) for s in self.plan['slots']]
        plan=self.save('plan.json',self.plan);rp_path=self.save('rp.json',rp);ref_path=self.save('refusals.json',refusals)
        adapted=self.cli('study','adapt','--plan',plan,'--request-plan',rp_path,'--refusals',ref_path)
        self.assertEqual(len(adapted['records']),48)
        b=self.save('adapted.json',adapted)
        r=self.cli('study','score','--plan',plan,'--export',self.export,'--bundle',b)
        self.assertEqual(r['arms']['baseline']['outcomes'],{'setup_refused':24})
        self.assertEqual(r['arms']['graph']['correct'],0)
        self.assertEqual(r['arms']['graph']['usage']['input_tokens']['observed'],0)
        self.assertIsNone(r['arms']['graph']['bytes']['output_bytes']['sum'])
        self.assertIsNone(r['arms']['graph']['timing']['provider_ms']['sum_ms'])
        self.cli('study','adapt','--plan',plan,'--request-plan',rp_path,'--refusals',self.save('missing-refusal.json',refusals[:-1]),ok=False)
        self.cli('study','adapt','--plan',plan,'--request-plan',rp_path,'--refusals',ref_path,'--study-kind','agent',ok=False)

    def test_precision_requires_complete_cases_and_no_power_claim(self):
        rows=[]
        for n in range(24):
            for arm in ('baseline','graph'):
                rows.append(dict(case_id=str(n),repetition=1,arm=arm,correct=bool((n%4)!=0 or arm=='graph'),usage={'input_tokens':100,'output_tokens':50},timing={'wall_ms':100}))
        report={'schema_version':1,'paired':study.analysis.paired(rows)}
        r=self.cli('study','precision','--report',self.save('report.json',report))
        self.assertIsNone(r['sufficient_48_cases']);self.assertGreater(r['approximate_80_percent_power_cases'],0)
        rows[0]['correct']=None;report['paired']=study.analysis.paired(rows)
        self.cli('study','precision','--report',self.save('pending-report.json',report),ok=False)

    def test_identical_repeated_answers_have_opaque_complete_cli_bindings(self):
        self.plan = study.schedule(self.corpus,self.protocol,'TEST-ONLY-repeated',3,['baseline','product'],'cold')
        plan,path,f = self.prepare()
        packets = self.cli('study','review','--plan',plan,'--export',self.export,'--fixture',path)
        candidate_ids = {study.digest(s['attempt_id']) for s in self.plan['slots']}
        self.assertTrue(all(p['attempt_blind_id'] not in candidate_ids for p in packets['packets']))
        self.assertEqual(len(packets['packets']),len(packets['operator_bindings']))
        self.assertEqual(len({p['attempt_blind_id'] for p in packets['packets']}),144)
        tokens=[]
        for p in packets['packets']:
            if p['attempt_blind_id'] not in tokens:tokens.append(p['attempt_blind_id'])
        public_orders=[next(b['order'] for b in packets['operator_bindings'].values() if b['attempt_blind_id']==token) for token in tokens]
        self.assertNotEqual(public_orders,list(range(144)))
        self.assertEqual(tokens,sorted(tokens))
        bundle=study.diagnostic_fixture(f,self.plan,self.protocol)
        self.assertEqual(packets,study.scoring.review_packets(self.corpus,self.plan,bundle,packets))
        doc = dict(schema_version=1,reviews=[review(p) for p in packets['packets']],adjudications=[])
        truth = dict(schema_version=1,reviews=[review(p) for p in study.scoring.truth_packets(self.corpus,self.lock)['packets']],adjudications=[])
        report = self.cli('study','score','--plan',plan,'--export',self.export,'--fixture',path,
                          '--review-packets',self.save('packets.json',packets),'--reviews',self.save('reviews.json',doc),
                          '--truth-reviews',self.save('truth.json',truth))
        self.assertEqual(len(report['attempts']),144)
        self.assertTrue(all(r['correct'] is True for r in report['attempts']))
        f['records'][0]['episode']['answer']['reason']+=' changed'
        refused=self.cli('study','score','--plan',plan,'--export',self.export,'--fixture',self.save('changed.json',f),
                         '--review-packets',self.work/'packets.json','--reviews',self.work/'reviews.json',ok=False)
        self.assertIn('registry inputs changed',refused['error']['message'])

    def test_manual_unresolved_review_never_overridden_by_automation(self):
        plan,path,f = self.prepare()
        bundle = study.diagnostic_fixture(f,self.plan,self.protocol)
        packets = study.scoring.review_packets(self.corpus,self.plan,bundle)
        truth = dict(schema_version=1,reviews=[review(p) for p in study.scoring.truth_packets(self.corpus,self.lock)['packets']],adjudications=[])
        for verdict in ('ambiguous','unsupported','pending','contradicted'):
            doc = dict(schema_version=1,reviews=[review(p,verdict if p['kind']=='answer-identity' else 'verified')
                       for p in packets['packets'] if verdict!='pending' or p['kind']!='answer-identity'],adjudications=[])
            def verified(root,files,selectors):
                return dict(items=[dict(outcome='verified',identity_ok=True,citation_ok=True,reason=None,identity=None)
                                   for s in selectors],failures={},pins=None)
            with patch.object(study.scoring,'automated',verified):
                report = study.scoring.score(self.corpus,self.protocol,self.plan,bundle,self.export,self.views,doc,truth,packets)
            expected = False if verdict=='contradicted' else None
            self.assertTrue(all(r['correct'] is expected for r in report['attempts'] if r['identities']),verdict)

    def test_qualified_automation_manual_dispute_matrix_through_real_cli(self):
        # Isolated synthetic study with complete, expressible source universes.
        # This tests the production CLI without narrowing the real corpus.
        repo = self.work/'qualified-cli'
        here = repo/'evals/graph-effectiveness-v1';here.mkdir(parents=True)
        for name in ('eval.py','scoring.py','analysis.py'):
            shutil.copyfile(HERE/name,here/name)
        for relative in self.lock['dependencies']:
            target=repo/relative
            target.parent.mkdir(parents=True,exist_ok=True)
            if not target.exists():target.symlink_to(study.REPO/relative)
        c=copy.deepcopy(self.corpus);p=copy.deepcopy(self.protocol);lock=copy.deepcopy(self.lock)
        c['source_ordering']='TEST-ONLY synthetic source, not repository measurements'
        files={'fixture.py':'','src/lib.rs':''}
        for n,case in enumerate(c['cases']):
            path='fixture.py' if case['language']=='python' else 'src/lib.rs'
            leaf='case_'+str(n);line=len(files[path].splitlines())+1
            quote=f'def {leaf}():' if case['language']=='python' else f'pub fn {leaf}() {{}}'
            files[path]+=quote+'\n'+('    return 1\n' if case['language']=='python' else '')
            sel=dict(language=case['language'],name=('fixture.' if path.endswith('.py') else 'crate::')+leaf,
                     file=path,line=line,kind='function' if path.endswith('.py') else 'fn',
                     citation=dict(start_line=line,end_line=line,quote=quote))
            ev=dict(view=case['head_view'],file=path,start_line=line,end_line=line,quote=quote,
                    sha256=study.sha((quote+'\n').encode()))
            case['truth']['identities']=[dict(view=case['head_view'],selector=sel,owner=None,
                                             module='fixture' if path.endswith('.py') else 'crate',trait=None,context=[ev])]
            case['truth'].pop('absence_search',None)
            case['prompt']='TEST-ONLY locate declaration and explain '+', '.join('['+cl['id']+']' for cl in case['truth']['claims'])
            for claim in case['truth']['claims']:claim.update(expected='TEST-ONLY synthetic claim',evidence=[ev])
        views={v:files for v in c['views']}
        for manifest in lock['snapshots'].values():
            manifest.update(files={path:study.sha(t.encode()) for path,t in files.items()},content_revision=study.digest(files))
        lock.update(corpus_sha256=study.digest(c),protocol_sha256=study.digest(p))
        for name,value in [('corpus.json',c),('protocol.json',p),('corpus.lock.json',lock)]:
            (here/name).write_text(json.dumps(value)+'\n')
        export=self.work/'qualified-export';study.export(c,lock,views,export)
        def cli(*args,ok=True):
            r=run_cli(here/'eval.py',args,self.work)
            if ok:
                self.assertEqual(r.returncode,0,r.stderr);return json.loads(r.stdout)
            self.assertNotEqual(r.returncode,0);self.assertEqual(r.stdout,'');return json.loads(r.stderr)
        plan=cli('study','plan','--study-id','TEST-ONLY-qualified')
        plan_path=self.save('qualified-plan.json',plan)
        path=self.save('qualified-fixture.json',fixture(c,p,plan))
        packets=cli('study','review','--plan',plan_path,'--export',export,'--fixture',path)
        registry=self.save('qualified-packets.json',packets)
        truth_packets=cli('study','truth-packets','--export',export)['packets']
        truth=self.save('qualified-truth.json',dict(schema_version=1,reviews=[review(p) for p in truth_packets],adjudications=[]))
        for verdict in ('ambiguous','unsupported','pending','contradicted','verified'):
            doc=dict(schema_version=1,reviews=[review(p,verdict if p['kind']=='answer-identity' else 'verified')
                     for p in packets['packets'] if verdict!='pending' or p['kind']!='answer-identity'],adjudications=[])
            result=cli('study','score','--plan',plan_path,'--export',export,'--fixture',path,
                       '--review-packets',registry,'--reviews',self.save('qualified-'+verdict+'.json',doc),'--truth-reviews',truth)
            self.assertTrue(all(i['automated']['outcome']=='verified' for r in result['attempts'] for i in r['identities']))
            expected=True if verdict=='verified' else False if verdict=='contradicted' else None
            self.assertTrue(all(r['correct'] is expected for r in result['attempts']),verdict)
        # Malformed and ambiguous required truth are refused by the same CLI.
        original=copy.deepcopy(c)
        for flaw in ('quote','ambiguity'):
            c=copy.deepcopy(original)
            if flaw=='quote':c['cases'][0]['truth']['identities'][0]['selector']['citation']['quote']='wrong'
            else:c['cases'][0]['truth']['resolution']='ambiguous'
            lock['corpus_sha256']=study.digest(c)
            (here/'corpus.json').write_text(json.dumps(c)+'\n')
            (here/'corpus.lock.json').write_text(json.dumps(lock)+'\n')
            marker=study.seal(dict(schema_version=1,corpus_sha256=study.digest(c),snapshots_sha256=study.digest(lock['snapshots'])),'export_sha256')
            (export/'complete.json').write_text(json.dumps(marker)+'\n')
            refused=cli('study','validate','--export',export,ok=False)
            self.assertIn('citation' if flaw=='quote' else 'ambiguous required',refused['error']['message'])

    def test_constant_development_is_unqualified_through_cli(self):
        for delta in (0,1,-1):
            rows = [dict(case_id=str(n),repetition=1,arm=arm,
                         correct=(arm=='graph' if delta==1 else arm=='baseline' if delta==-1 else True),
                         usage=None,timing={'wall_ms':100}) for n in range(24) for arm in ('baseline','graph')]
            paired = study.analysis.paired(rows)
            self.assertIsNone(paired['descriptive_case_bootstrap_ci'])
            r = self.cli('study','precision','--report',self.save('constant-'+str(delta)+'.json',dict(schema_version=1,paired=paired)))
            self.assertEqual(r['qualification'],'insufficient-development-variation')
            self.assertIsNone(r['approximate_80_percent_power_cases'])
            self.assertTrue(all(v is None for v in r['planned_accuracy_half_width'].values()))
        rows=[dict(case_id=str(n),repetition=1,arm=arm,correct=arm==('graph' if n else 'baseline'),
                   usage=None,timing={'wall_ms':100}) for n in range(2) for arm in ('baseline','graph')]
        result=self.cli('study','precision','--report',self.save('few-clusters.json',dict(schema_version=1,paired=study.analysis.paired(rows))))
        self.assertEqual(result['qualification'],'insufficient-development-variation')
        self.assertIsNone(result['approximate_precision_cases'])

    def test_prospective_partial_usage_and_refused_tool_attempts_cli(self):
        plan,path,f=self.prepare()
        slot=next(s for s in self.plan['slots'] if s['arm']=='graph')
        e=f['records'][slot['order']]['episode']
        fields={key:dict(expected_turns=2,reported_turns=1,valid_observations=1,
                        observed_sum=200,coverage='partial',total=None,total_state='partial')
                for key in ('input_tokens','cached_input_tokens','output_tokens','reasoning_tokens','total_tokens')}
        fields['cached_input_tokens'].update(expected_turns=1,observed_sum=0,total=0,coverage='complete',total_state='observed_single_turn')
        e['telemetry']=dict(contract='prospective-accounting-v1',usage=dict(fields=fields,cost_usd=None),
                            attempts=[dict(tool='read',outcome='ok'),dict(tool='graph_sync',outcome='refused')],
                            tools=dict(attempts=2,graph_attempts=1,graph_successful=0,all_attempt_denominator=None,
                                       coverage=dict(state='partial',reported_attempts=2,expected_attempts=None)),
                            timing=dict(wall_ms=1000,setup_ms=200,provider_ms=800,
                                        overlapping=dict(graph_sync_ms=0,plugin_install_ms=0),outside_wall=dict(preflight_ms=5),end_to_end_ms=None))
        r=self.cli('study','score','--plan',plan,'--export',self.export,'--fixture',self.save('prospective-partial.json',f))
        row=r['attempts'][slot['order']]
        self.assertEqual(row['calls'],2);self.assertEqual(row['graph_calls'],1);self.assertEqual(row['successful_graph_calls'],0)
        self.assertIsNone(row['call_denominator']);self.assertIsNone(row['usage']['input_tokens'])
        self.assertEqual(row['usage']['cached_input_tokens'],0)
        self.assertEqual(r['paired']['missing_token_pairs'],1)
        self.assertEqual(r['arms']['graph']['usage']['input_tokens']['field_coverage'][0]['observed_sum'],200)
        self.assertEqual(row['phase_timing']['outside_wall']['preflight_ms'],5)


if __name__=='__main__':unittest.main()
