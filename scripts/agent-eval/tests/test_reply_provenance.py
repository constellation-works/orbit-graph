"""Synthetic source/secret fixtures through the real runner, broker and private plugin."""
import copy
import importlib.util
import json
import os
import unittest

import support
from support import eval_runner as runner
from test_runner import EpisodeCase
import test_plugin_profile as plugin_tests
SPEC = plugin_tests.SPEC
import reply_provenance as replies


class RedactionProofTests(unittest.TestCase):
    def test_shapes_values_escaping_and_idempotence(self):
        secret = 'private-value-"café"-13795'
        redactor = replies.Redactor([secret])
        for value in (secret, 'sk-' + 'synthetic13795' * 3, 'Bearer ' + 'example13795' * 3):
            for _ in range(4):
                safe, proof = redactor.transform(value)
                self.assertGreater(len(proof['spans']), 0)
                self.assertNotIn(value, safe)
                replies.verify_text(safe, proof, redactor.policy)
                self.assertEqual(redactor.text(safe), (safe, 0))
                value = json.dumps(value)[1:-1]
        self.assertNotIn(secret, json.dumps(redactor.policy))
        for ordinary in ('ordinary words', 'prefix-sk-abcdefghijklmnop', '[REDACTED]'):
            self.assertEqual(redactor.text(ordinary), (ordinary, 0))

    def test_proof_rejects_wrong_spans_hashes_costs_and_rules(self):
        redactor = replies.Redactor()
        safe, proof = redactor.transform('source sk-' + 'synthetic13795' * 2)
        changes = [dict(proof, original_bytes=proof['original_bytes'] + 1),
                   dict(proof, original_sha256=proof['safe_sha256']),
                   dict(proof, safe_sha256='0' * 64), dict(proof, safe_bytes=True),
                   dict(proof, spans=[])]
        for key, value in (('start', 0), ('rules', ['unknown']), ('original_bytes', 0)):
            altered = copy.deepcopy(proof)
            altered['spans'][0][key] = value
            changes.append(altered)
        for altered in changes:
            with self.subTest(altered=altered), self.assertRaises(ValueError):
                replies.verify_text(safe, altered, redactor.policy)


@unittest.skipUnless(support.RG and os.environ.get('AGENT_EVAL_ORBIT') and
                     os.environ.get('AGENT_EVAL_ORBIT_GRAPH'), 'requires private installed-plugin binaries')
class SafeReplyTests(EpisodeCase):
    def setUp(self):
        helper = plugin_tests.InstalledPluginTests()
        helper.setUp()
        for name in ('work', 'head', 'base', 'truth', 'counter', 'source', 'snapshot',
                     'commit', 'install_args', 'pin', 'inventory'):
            setattr(self, name, getattr(helper, name))

    plugin_episode = plugin_tests.InstalledPluginTests.plugin_episode

    def fixture(self):
        self.key = 'sk-' + 'SyntheticCredential13795' * 2
        self.host_value = 'private-host-fixture-13795'
        text = support.HEAD['src/lib.rs'] + '\n// fixture ' + self.key + '\n'
        text += '// fixture ' + self.host_value + '\n'
        # Mask before line cutting, including a credential across the cut.
        text += '// ' + 'x' * 979 + ' ' + self.key + '\n'
        self.head.joinpath('src/lib.rs').write_text(text)
        self.source, self.snapshot = support.snapshot_repo(self.work / 'credential-source',
            files_head={'src/lib.rs': text}, files_base={'src/lib.rs': support.BASE['src/lib.rs']})
        self.install_args += ['--provider-env', 'FAKE_API_KEY']
        self.evaluator = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(self.evaluator)

    def run_safe(self, arm='baseline', **script):
        steps = [{'call': 'read', 'arguments': {'path': 'src/lib.rs'}},
                 {'call': 'rg', 'arguments': {'pattern': 'fixture'}},
                 {'call': 'git', 'arguments': {'op': 'file_at', 'rev': 'HEAD', 'path': 'src/lib.rs'}}]
        if arm == 'graph':
            steps += [{'call': 'graph_maintain', 'arguments': {'operation': 'graph_sync'}},
                      {'call': 'graph_show', 'arguments': {'selector': 'file:src/lib.rs'}}]
        steps += [{'final': json.dumps(support.ANSWER)}]
        code, report, stderr, out = self.plugin_episode(steps, arm=arm,
            env={'FAKE_API_KEY': self.host_value}, **script)
        self.assertEqual(code, 0, (report, stderr))
        return out, self.assert_outcome(out, 'ok', None)

    def test_source_credentials_replay_exactly_in_both_arms_and_plugin(self):
        self.fixture()
        for arm in ('baseline', 'graph'):
            with self.subTest(arm=arm):
                out, artifact = self.run_safe(arm)
                self.assertEqual(artifact['runner_version'], '5')
                self.assertEqual(artifact['redactions'], 0)
                self.assertTrue(all(f['redactions'] == 0 for f in artifact['files'].values()))
                replayed = self.evaluator.load_episode(out, diagnostic=True)
                self.assertEqual(replayed['artifact_sha256'], artifact['artifact_sha256'])
                self.assertEqual(artifact['output_bytes'], sum(len(c['output'].encode())
                    for c in artifact['calls']) + len(artifact['final_output'].encode()))
                proofs = artifact['reply_provenance']
                self.assertTrue(all(p['views'] for p in proofs[:3]))
                if arm == 'graph':
                    self.assertTrue(proofs[-1]['views'])  # installed product source response
                for name in (*artifact['files'], 'episode.json', 'state/broker-calls.jsonl'):
                    data = (out / name).read_text()
                    self.assertNotIn(self.key, data, name)
                    self.assertNotIn(self.host_value, data, name)
                    self.assertNotIn('sk-Synthetic', data, name)  # cut-line prefix
                transcript = runner.analyse_transcript((out / 'provider.jsonl').read_bytes(),
                                                       False, artifact['request']['tools'])
                self.assertIsNone(runner.reconcile_calls(transcript, artifact['calls']))
                # Capture and preregistration preserve the original, untransformed source view.
                self.assertEqual(artifact['source_provenance']['head']['commit'],
                                 self.snapshot['head_commit'])
                self.assertEqual(artifact['inputs']['head']['content_revision'],
                                 support.revision(self.head))

    def test_replay_rejects_forged_proof_source_version_log_cost_and_provider(self):
        self.fixture()
        out, artifact = self.run_safe()
        original = (out / 'episode.json').read_bytes()
        mutations = []
        for key, value in (('delivered_bytes', 0), ('delivered_sha256', '0' * 64),
                           ('contract_sha256', '0' * 64), ('seq', 5)):
            altered = copy.deepcopy(artifact)
            altered['reply_provenance'][0][key] = value
            mutations.append(altered)
        altered = copy.deepcopy(artifact)
        altered['source_provenance']['head']['selected_blobs']['src/lib.rs'] = '0' * 40
        mutations += [altered, dict(artifact, runner_version='4'), dict(artifact, redactions=1),
                      dict(artifact, output_bytes=artifact['output_bytes'] + 1)]
        for altered in mutations:
            runner.seal(altered, 'artifact_sha256')
            (out / 'episode.json').write_text(json.dumps(altered))
            with self.assertRaises(ValueError):
                self.evaluator.load_episode(out, diagnostic=True)
        (out / 'episode.json').write_bytes(original)
        # Even a jointly resealed artifact/log must satisfy transformation arithmetic.
        log_path = out / 'broker-calls.jsonl'
        original_log = log_path.read_bytes()
        entries = [json.loads(line) for line in original_log.splitlines()]
        call = next(e for e in entries if e['type'] == 'call')
        call['reply_provenance']['views'][0]['text']['original_bytes'] += 1
        altered = copy.deepcopy(artifact)
        altered['reply_provenance'][0] = call['reply_provenance']
        log_path.write_text(''.join(json.dumps(e) + '\n' for e in entries))
        altered['files']['broker-calls.jsonl'].update(sha256=runner.sha256_file(log_path),
                                                   bytes=log_path.stat().st_size)
        runner.seal(altered, 'artifact_sha256')
        (out / 'episode.json').write_text(json.dumps(altered))
        with self.assertRaisesRegex(ValueError, 'transformation cost'):
            self.evaluator.load_episode(out, diagnostic=True)
        log_path.write_bytes(original_log)
        (out / 'episode.json').write_bytes(original)
        # Provider-only credentials remain a late, non-replayable redaction.
        code, _, stderr, leaked = self.plugin_episode([
            {'call': 'read', 'arguments': {'path': 'src/lib.rs'}},
            {'echo_env': 'FAKE_API_KEY'}, {'final': json.dumps(support.ANSWER)}], arm='baseline',
            env={'FAKE_API_KEY': self.host_value})
        self.assertEqual(code, 0, stderr)
        with self.assertRaisesRegex(ValueError, 'redacted capture'):
            self.evaluator.load_episode(leaked, diagnostic=True)
        self.assertNotIn(self.host_value, (leaked / 'provider.jsonl').read_text())
        # High-confidence provider output also refuses without any host-value match.
        code, _, stderr, shaped = self.plugin_episode([
            {'call': 'read', 'arguments': {'path': 'src/lib.rs'}},
            {'emit': {'type': 'item.completed', 'item': {'id': 'secret-output',
                'type': 'agent_message', 'text': self.key}}},
            {'final': json.dumps(support.ANSWER)}], arm='baseline',
            env={'FAKE_API_KEY': self.host_value})
        self.assertEqual(code, 0, stderr)
        with self.assertRaisesRegex(ValueError, 'redacted capture'):
            self.evaluator.load_episode(shaped, diagnostic=True)
        self.assertNotIn(self.key, (shaped / 'provider.jsonl').read_text())

    def test_failed_truncated_malformed_and_cancelled_captures_stay_failed(self):
        self.fixture()
        read = {'call': 'read', 'arguments': {'path': 'src/lib.rs'}}
        final = {'final': json.dumps(support.ANSWER)}
        cases = [([read, final], dict(support.LIMITS, output_bytes=1024),
                  'failed', 'output_budget_exceeded'),
                 ([read, {'emit_raw': 'not json'}, final], support.LIMITS,
                  'failed', 'provider_output_malformed'),
                 ([read, {'sleep': 60}], dict(support.LIMITS, wall_ms=3000),
                  'timeout', 'wall_time_exceeded'),
                 ([dict(read, omit_completed=True), final], support.LIMITS,
                  'failed', 'telemetry_mismatch')]
        for steps, limits, status, error in cases:
            with self.subTest(error=error):
                code, _, stderr, out = self.plugin_episode(steps, arm='baseline', limits=limits,
                    env={'FAKE_API_KEY': self.host_value})
                self.assertEqual(code, 0, stderr)
                artifact = self.assert_outcome(out, status, error)
                replay = self.evaluator.load_episode(out, diagnostic=True)
                self.assertEqual(replay['status'], status)
                self.assertEqual(replay['error']['code'], error)
                self.assertEqual(replay['output_bytes'], artifact['output_bytes'])
                self.assertEqual(artifact['cleanup']['survivors'], [])
