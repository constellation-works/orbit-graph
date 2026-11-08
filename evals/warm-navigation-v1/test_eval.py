"""Test-only qualification with original real privately installed plugin captures."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('warm_study', HERE / 'eval.py')
study = importlib.util.module_from_spec(spec); spec.loader.exec_module(study)
r, batch, plugin = study.runner, study.batch, study.plugin
sys.path.insert(0, str(study.REPO / 'scripts/agent-eval/tests'))
import support

ANSWER = {'items': ['symbol:src/lib.rs#checkout:function'], 'abstain': False, 'reason': 'source declaration',
          'evidence': [{'item': 'symbol:src/lib.rs#checkout:function', 'file': 'src/lib.rs', 'line': 2,
                        'quote': 'pub fn checkout(value: i32) -> i32 { subtotal(value) }'}]}
FINAL = {'final': json.dumps(ANSWER)}
if os.environ.get('WARM_REQUIRE_STRICT') == '1' and not (os.environ.get('AGENT_EVAL_ORBIT') and os.environ.get('AGENT_EVAL_ORBIT_GRAPH')):
    raise RuntimeError('strict warm qualification requires explicit Orbit and graph binaries')

READ = {'call': 'read', 'arguments': {'path': 'src/lib.rs'}}
SEARCH = {'call': 'graph_search', 'arguments': {'query': 'discount'}}
SYNC = {'call': 'graph_maintain', 'arguments': {'operation': 'graph_sync'}}
VERSION = {'call': 'graph_version', 'arguments': {}}


@unittest.skipUnless(os.environ.get('AGENT_EVAL_ORBIT') and os.environ.get('AGENT_EVAL_ORBIT_GRAPH'),
                     'set AGENT_EVAL_ORBIT and AGENT_EVAL_ORBIT_GRAPH for real private warm plugin qualification')
class WarmCaptureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.strict = os.environ.get('WARM_REQUIRE_STRICT') == '1'
        cls.containment = 'bwrap' if cls.strict else 'none'
        if cls.strict:
            from test_runner import bwrap_unavailable
            reason = bwrap_unavailable()
            if reason or not all(plugin.resource_bounds().values()):
                raise RuntimeError(reason or 'strict qualification requires finite cgroup memory.max and pids.max')
        cls.work = support.scratch()
        cls.export_dir = cls.work / 'export'
        cls.study_cli('export','--destination',cls.export_dir)
        cls.orbit = str(Path(os.environ['AGENT_EVAL_ORBIT']).resolve())
        cls.graph = str(Path(os.environ['AGENT_EVAL_ORBIT_GRAPH']).resolve())
        cls.provider = str(support.FAKE_CODEX)
        cls.rg = shutil.which('rg')
        cls.commit = r.git_run('/usr/bin/git', study.REPO, cls.work, 'rev-parse', 'HEAD')
        cls.inspection = cls.harness('plugin-inspect', '--plugin-repo', study.REPO, '--plugin-commit', cls.commit,
                                    '--orbit', cls.orbit, '--orbit-graph', cls.graph,
                                    '--containment', cls.containment, '--out', cls.work/'inspect')
        cls.serial = 0

    @classmethod
    def tearDownClass(cls):
        support.remove(cls.work)

    @classmethod
    def study_cli(cls, *args):
        result=r.broker.run_child([sys.executable,'-B',str(HERE/'eval.py'),*map(str,args)],str(cls.work),
                                  {'PATH':'/usr/bin:/bin','HOME':str(cls.work)},60,capture_limit=16*1024**2)
        if not plugin.clean(result):
            raise AssertionError(plugin.evidence(result))
        return json.loads(result['stdout'])

    @classmethod
    def harness(cls, *args, script=None):
        env = dict(PATH='/usr/bin:/bin', HOME=str(cls.work), XDG_CONFIG_HOME=str(cls.work), LC_ALL='C.UTF-8')
        if script is not None:
            env['FAKE_CODEX_SCRIPT_JSON'] = json.dumps(script)
        result = r.broker.run_child([sys.executable, '-B', str(r.HERE/'eval_runner.py'), *map(str,args)],
                                     str(cls.work), env, 180, capture_limit=8*1024**2)
        if not plugin.clean(result):
            raise AssertionError(plugin.evidence(result))
        return json.loads(result['stdout'])

    @classmethod
    def plan(cls, arm, pin=None):
        cls.serial += 1
        binaries = dict(codex=cls.provider, orbit=cls.orbit, orbit_graph=cls.graph, python='/usr/bin/python3',
                        git='/usr/bin/git', rg=cls.rg, bwrap=shutil.which('bwrap') if cls.strict else None)
        runtime = dict(model=dict(provider='codex-cli',name='fake-model',version='codex-cli 0.0.0-fake',settings={}),
                       provider_binary_sha256=r.sha256_file(cls.provider),
                       harness={n:r.sha256_file(r.HERE/n) for n in batch.HARNESS},
                       binaries={n:r.sha256_file(p) if p else None for n,p in binaries.items()},
                       settings={}, containment=cls.containment, allow_code_mode=False,resource_limits=plugin.resource_bounds(),
                       provider_env=['FAKE_CODEX_SCRIPT_JSON'], approval_reference='test-only-no-network')
        spec = dict(study_kind='test-only',batch_id='TEST-ONLY-%d'%cls.serial,arm=arm,runtime=runtime,
                    slots=study.fixture_slots(cls.export_dir,pin or cls.inspection['pin'],arm))
        path = cls.work/('plan-%d.json'%cls.serial)
        spec_path=cls.work/('spec-%d.json'%cls.serial);batch.write(spec_path,spec)
        return path, cls.study_cli('freeze','--spec',spec_path,'--destination',path)

    @classmethod
    def capture(cls, arm='graph', script=None, pin=None):
        path, plan = cls.plan(arm,pin)
        out = cls.work/('batch-%d'%cls.serial)
        report = cls.harness('run-batch','--plan',path,'--export',cls.export_dir,'--out',out,
                            '--codex',cls.provider,'--model','fake-model','--rg',cls.rg,'--python','/usr/bin/python3',
                            '--orbit',cls.orbit,'--orbit-graph',cls.graph,'--plugin-repo',study.REPO,
                            '--source-repo',cls.export_dir/'source','--containment',cls.containment,'--auth-file','none','--truth-path',HERE,
                            '--provider-env','FAKE_CODEX_SCRIPT_JSON',
                            script=script or dict(steps=[READ,SEARCH,SYNC,SEARCH,VERSION,FINAL]))
        return out, plan, report

    def reviews(self, plan, episodes):
        return dict(schema_version=1,freeze_sha256=plan['freeze_sha256'],reviews=[
            dict(slot=i,artifact_sha256=a['artifact_sha256'],correct=True,reviewer='TEST-ONLY-source-fixture',
                 source_evidence=['src/lib.rs:2']) for i,a in enumerate(episodes) if a and a['status']=='ok'])

    def test_real_state_reuse_invalidation_parity_and_all_slot_accounting(self):
        out, plan, report = self.capture()
        self.assertTrue(report['complete'], r.load_json(out/'batch.json',64*1024**2))
        self.assertEqual(report['outcomes'], ['ok']*3, report)
        _, record, episodes = batch.replay(out,diagnostic=True)
        self.assertEqual([a['batch']['reused_installation'] for a in episodes],[False,True,True])
        self.assertEqual([a['timing']['plugin_install_ms'] > 0 for a in episodes],[True,False,False])
        self.assertEqual([a['calls'][1]['status'] for a in episodes],['failed','ok','ok'])
        stale=json.loads(episodes[-1]['calls'][1]['output'])['product_reply']['structuredContent']['index']
        self.assertFalse(stale['fresh'])
        self.assertNotEqual(stale['revision'],stale['checkout_revision'])
        rebuilt=json.loads(episodes[-1]['calls'][2]['output'])['product_reply']['structuredContent']
        self.assertTrue(rebuilt['code_index']['fresh'])
        self.assertTrue(rebuilt['result']['seeded_from_published'])
        self.assertEqual(rebuilt['result']['files_changed'],1)
        self.assertTrue(all(a['calls'][3]['status']=='ok' for a in episodes))
        self.assertTrue(all(o['cache']=='unknown' for a in episodes for o in a['batch']['cache_observations']))
        self.assertEqual(episodes[0]['treatment']['skill_files'],episodes[-1]['treatment']['skill_files'])
        self.assertEqual(episodes[0]['treatment']['inventory'],episodes[-1]['treatment']['inventory'])
        self.assertNotEqual(record['slots'][1]['after'],record['slots'][2]['after'])
        # Independent empty-state rebuild of the edited pin, through the shared cold runner.
        request = copy.deepcopy(plan['slots'][-1]['request'])
        request.update(schema_version=3,profile=plugin.PROSPECTIVE_PROFILE,cache_policy=r.CACHE_POLICY)
        r.seal(request,'request_sha256')
        request_path=self.work/'fresh-request.json';batch.write(request_path,request)
        fresh=self.work/'fresh-parity'
        self.harness('run','--request',request_path,'--head',self.export_dir/'edited','--base',self.export_dir/'unchanged',
                     '--out',fresh,'--codex',self.provider,'--model','fake-model','--rg',self.rg,
                     '--orbit',self.orbit,'--orbit-graph',self.graph,'--plugin-repo',study.REPO,
                     '--source-repo',self.export_dir/'source','--containment','none','--auth-file','none',
                     '--provider-env','FAKE_CODEX_SCRIPT_JSON',script=dict(steps=[SYNC,SEARCH,FINAL]))
        original=r.load_json(fresh/'episode.json',64*1024**2)
        def semantics(call):
            product=json.loads(call['output'])['product_reply']['structuredContent']
            # Location/coverage/results are semantic evidence; index timing/root are not.
            return product['result']
        self.assertEqual(semantics(episodes[-1]['calls'][3]),semantics(original['calls'][1]))
        audits=self.work/'audits.json';batch.write(audits,self.reviews(plan,episodes))
        scored=self.study_cli('score','--batch',out,'--audits',audits,'--diagnostic')
        replayed=self.study_cli('replay','--batch',out,'--diagnostic')
        self.assertTrue(replayed['complete'])
        self.assertEqual(scored['all_attempt_correct'],3)
        self.assertAlmostEqual(scored['amortized_batch_wall_ms']*3,scored['measured_batch_wall_ms'])
        self.assertIsNone(scored['batch_usage_total'])
        self.assertTrue(all(row['telemetry']['usage']['fields']['total_tokens']['total'] is None for row in scored['rows']))
        self.assertEqual(scored['setup_bytes'],episodes[0]['setup_output_bytes'])
        if self.strict:
            batch.replay(out)
        else:
            with self.assertRaises(ValueError): batch.replay(out)  # diagnostic cannot become study evidence
        # Unmodified cold adapter still refuses the warm profile even when sealed.
        spec=importlib.util.spec_from_file_location('cold_adapter',study.REPO/'evals/plugin-agent-navigation/eval.py')
        adapter=importlib.util.module_from_spec(spec);spec.loader.exec_module(adapter)
        with self.assertRaises(ValueError): adapter.load_episode(out/'slot-000',diagnostic=True)
        for version in (True, 2):
            batch_path=out/'batch.json';original_batch=batch_path.read_bytes()
            altered=copy.deepcopy(record);altered['schema_version']=version;r.seal(altered,'batch_sha256')
            batch_path.write_text(json.dumps(altered))
            try:
                with self.assertRaisesRegex(ValueError,'schema'): batch.replay(out,True)
            finally:
                batch_path.write_bytes(original_batch)
        # A resealed smaller wall cannot contradict the original phase clocks.
        batch_path=out/'batch.json';original_batch=batch_path.read_bytes()
        altered=copy.deepcopy(record);altered['slots'][0]['end_ns']=altered['slots'][0]['start_ns']+1
        r.seal(altered,'batch_sha256');batch_path.write_text(json.dumps(altered))
        try:
            with self.assertRaisesRegex(ValueError,'phase'): batch.replay(out,True)
        finally:
            batch_path.write_bytes(original_batch)
        # State and provenance tampering both refuse.
        state=out/'product/held/.orbit-graph/tampered';state.write_text('wrong cache');state.chmod(0o600)
        with self.assertRaisesRegex(ValueError,'tampered'): batch.replay(out,True)
        state.unlink()
        home=out/'product/orbit-home';home.chmod(0o777)
        try:
            with self.assertRaisesRegex(ValueError,'unsafe state root'): batch.replay(out,True)
        finally:
            home.chmod(0o700)
        evidence=out/'slot-001/broker-calls.jsonl';saved=evidence.read_bytes();evidence.write_bytes(saved+b'{}\n')
        with self.assertRaises(ValueError): batch.replay(out,True)
        evidence.write_bytes(saved)

    def test_first_task_cannot_redefine_installed_state(self):
        out,_,report=self.capture('graph',dict(steps=[READ,{'tamper_installation':True},FINAL]))
        self.assertEqual(report['outcomes'],['ok','not_run','not_run'])
        record=r.load_json(out/'batch.json',64*1024**2)
        self.assertIn('installed bytes changed',record['slots'][0]['state_error'])
        with self.assertRaisesRegex(ValueError,'state handoff'): batch.replay(out,True)

    def test_counterbalanced_cohort_and_undeclared_state_refusal(self):
        outputs=[]
        for arm in ('baseline','graph','graph','baseline'):
            out,_,_=self.capture(arm,dict(steps=[READ,FINAL]))
            outputs.append(out)
        self.assertEqual(study.cohort_check(outputs,True)['scheduled'],12)
        with self.assertRaises(ValueError): study.cohort_check(outputs[:2],True)
        with self.assertRaises(ValueError): study.cohort_check(list(reversed(outputs)),True)
        stray=outputs[-1]/'product/undeclared';stray.write_text('not declared');stray.chmod(0o600)
        with self.assertRaisesRegex(ValueError,'undeclared'): batch.replay(outputs[-1],True)
        stray.unlink()

    def test_source_export_drift_refuses_without_replacing_prior_outcomes(self):
        path=self.export_dir/'edited/src/lib.rs';original=path.read_bytes()
        try:
            path.write_bytes(original+b'// unpinned edit\n')
            out,plan,report=self.capture('baseline',dict(steps=[READ,FINAL]))
            self.assertEqual(report['outcomes'],['ok','ok','refused'])
            _,_,episodes=batch.replay(out,True)
            result=study.score(out,self.reviews(plan,episodes),True)
            self.assertEqual((result['scheduled'],result['all_attempt_correct']),(3,2))
        finally:
            path.write_bytes(original)

    def test_cancellation_preserves_partial_schedule(self):
        out,plan,report=self.capture('baseline',dict(steps=[READ,{'cancel_runner': True}]))
        _,record,episodes=batch.replay(out,True)
        self.assertTrue(record['cancelled'])
        self.assertEqual(report['outcomes'],['failed','not_run','not_run'])
        result=study.score(out,self.reviews(plan,episodes),True)
        self.assertEqual(result['scheduled'],3)
        self.assertEqual(result['all_attempt_correct'],0)

    def test_equal_controls_baseline_isolation_and_missing_usage(self):
        out, plan, report=self.capture('baseline',dict(steps=[READ,FINAL],usage=None))
        _,_,episodes=batch.replay(out,True)
        _,graph_plan=self.plan('graph')
        batch.paired(plan,graph_plan)
        for version in (True, 2):
            changed=copy.deepcopy(plan);changed['schema_version']=version;r.seal(changed,'freeze_sha256')
            with self.assertRaisesRegex(ValueError,'profile/policy'): batch.validate_plan(changed)
        changed=copy.deepcopy(graph_plan);changed['slots'][1]['request']['prompt']+=' drift'
        r.seal(changed['slots'][1]['request'],'request_sha256');r.seal(changed,'freeze_sha256')
        with self.assertRaisesRegex(ValueError,'schedule'): batch.paired(plan,changed)
        self.assertEqual(report['outcomes'],['ok']*3)
        self.assertTrue(all(a['treatment'] is None for a in episodes))
        self.assertEqual(list((out/'product/orbit-home').iterdir()),[])
        spec={k:plan[k] for k in ('study_kind','batch_id','arm','slots','runtime')}
        spec['undeclared']=True
        with self.assertRaisesRegex(ValueError,'spec fields'): study.freeze(spec,self.work/'bad-freeze.json')
        # Empty baseline state must not hide a dangling root link.
        home=out/'product/orbit-home';parked=self.work/'parked-home'
        home.rename(parked);home.symlink_to(self.work/'missing-home')
        try:
            with self.assertRaisesRegex(ValueError,'unsafe state root'): batch.replay(out,True)
        finally:
            home.unlink();parked.rename(home)
        result=study.score(out,self.reviews(plan,episodes),True)
        self.assertTrue(all(row['telemetry']['usage']['fields']['input_tokens']['total'] is None for row in result['rows']))

    def test_failed_and_refused_slots_never_shrink_denominator(self):
        out,plan,report=self.capture('baseline',dict(steps=[dict(READ,omit_completed=True),FINAL]))
        _,_,episodes=batch.replay(out,True)
        self.assertEqual(report['outcomes'],['failed']*3)
        result=study.score(out,self.reviews(plan,episodes),True)
        self.assertEqual((result['scheduled'],result['all_attempt_correct']),(3,0))
        pin=dict(self.inspection['pin'],backend_sha256='0'*64)
        out,plan,report=self.capture(pin=pin)
        _,_,episodes=batch.replay(out,True)
        self.assertEqual(report['outcomes'],['refused','not_run','not_run'])
        result=study.score(out,self.reviews(plan,episodes),True)
        self.assertEqual(result['scheduled'],3)
        self.assertFalse(result['complete'])
        self.assertIsNone(result['measured_tool_bytes'])
        path=out/'batch.json';saved=path.read_bytes();forged=r.load_json(path,64*1024**2)
        forged['slots'][0]['directory']='../outside';r.seal(forged,'batch_sha256');path.write_text(json.dumps(forged))
        try:
            with self.assertRaisesRegex(ValueError,'slot directory'): batch.replay(out,True)
        finally:
            path.write_bytes(saved)
        self.assertAlmostEqual(result['amortized_batch_wall_ms']*3,result['measured_batch_wall_ms'])


if __name__ == '__main__': unittest.main()
