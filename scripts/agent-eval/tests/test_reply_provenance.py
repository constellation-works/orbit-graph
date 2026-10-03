"""Synthetic source/secret fixtures through the real runner, broker and private plugin."""
import copy
import importlib.util
import json
import os
import sys
import unittest

import support
from support import eval_runner as runner
from test_runner import EpisodeCase
import test_plugin_profile as plugin_tests
SPEC = plugin_tests.SPEC
import reply_provenance as replies
import plugin_profile as plugin
from support import eval_broker as broker


@unittest.skipUnless(support.RG, 'requires ripgrep')
class SafeBudgetTests(unittest.TestCase):
    """Final delivered-byte bounds through the real stdio MCP process."""

    def setUp(self):
        self.work = support.scratch()
        self.counter = 0
        self.addCleanup(support.remove, self.work)

    def client(self, text, arm, values=(), orbit=None):
        self.counter += 1
        work = self.work / f'{arm}-{self.counter}'
        repo, snapshot = support.snapshot_repo(work, files_head={'src/lib.rs': text})
        path, state = support.broker_config(work, repo, snapshot, arm,
            dict(support.LIMITS, call_bytes=1024, tool_calls=200), str(support.FAKE_GRAPH))
        config = json.loads(path.read_text())
        redactor = replies.Redactor(values)
        config.update(schema_version=2, reply_contract={
            'version': replies.CONTRACT, 'policy': redactor.policy, 'source_binding': '0' * 64},
            plugin=None)
        if arm == 'graph':
            config['tools'] = list(broker.COMMON_TOOLS + plugin.TOOLS)
            config['plugin'] = {'orbit': str(orbit or '/unused'), 'home': str(state / 'home'),
                'policy': plugin.POLICY, 'inventory': [
                    {'name': tool, 'description': 'private fixture', 'inputSchema': {}}
                    for tool in plugin.TOOLS]}
        path.write_text(json.dumps(config))
        client = support.McpClient(path)
        self.addCleanup(client.terminate)
        client.request('initialize')
        return client, state, redactor

    def call(self, client, tool, args):
        result = client.request('tools/call', {'name': tool, 'arguments': args})['result']
        text = result['content'][0]['text']
        self.assertFalse(result['isError'], text)
        self.assertLessEqual(len(text.encode()), 1024)
        body = json.loads(text)
        self.assertEqual(body['status'], 'ok')
        self.assertEqual(body['text_view']['contract'], replies.CONTRACT)
        return body, text

    def check_log(self, client, state, redactor, delivered):
        self.assertEqual(client.close(), 0)
        self.assertFalse((state / 'budget.json').exists())
        calls = [entry for entry in support.log_entries(state) if entry['type'] == 'call']
        self.assertEqual([call['output'] for call in calls], delivered)
        for call in calls:
            proof = call['reply_provenance']
            self.assertEqual(proof['safe_output'], call['output'])
            self.assertFalse(proof['truncated'])
            self.assertEqual(proof['delivered_bytes'], len(call['output'].encode()))
            self.assertEqual(proof['delivered_sha256'], replies.sha(call['output']))
            self.assertEqual(proof['call_sha256'], replies.digest({k: call[k] for k in
                ('tool', 'input', 'output', 'status', 'elapsed_ms')}))
            replies.verify_text(call['output'], proof['text'], redactor.policy)
            for view in proof['views']:
                replies.verify_text(view['safe_text'], view['text'], redactor.policy)
        return calls

    def pages(self, client, tool, arguments, expected, delivered):
        offset, pieces = 0, []
        for _ in range(100):
            body, text = self.call(client, tool, dict(arguments, offset=offset))
            delivered.append(text)
            self.assertEqual(body['offset'], offset)
            self.assertEqual(body['total_chars'], len(expected))
            pieces.append(body['output'])
            following = body['next_offset']
            if following is None:
                break
            self.assertEqual(following, offset + len(body['output']))
            self.assertGreater(following, offset)
            offset = following
        else:
            self.fail('continuation did not finish')
        self.assertGreater(len(pieces), 1)
        self.assertEqual(''.join(pieces), expected)

    def test_harmless_rg_and_git_continuations_at_minimum_budget(self):
        text = ''.join('fixture ' + 'plain text ' * 6 + '\n' for _ in range(35))
        for arm in ('baseline', 'graph'):
            for tool in ('rg', 'git'):
                with self.subTest(arm=arm, tool=tool):
                    client, state, redactor = self.client(text, arm)
                    delivered = []
                    expected = (''.join(f'src/lib.rs:{i}:{line}\n'
                        for i, line in enumerate(text.splitlines(), 1)) if tool == 'rg' else text)
                    args = ({'pattern': 'fixture'} if tool == 'rg' else
                            {'op': 'file_at', 'rev': 'HEAD', 'path': 'src/lib.rs'})
                    self.pages(client, tool, args, expected, delivered)
                    self.check_log(client, state, redactor, delivered)

    def test_read_continuations_include_disclosure_and_encoding(self):
        lines = ['fixture café 漢字 "quoted" \\ ' * 3 for _ in range(35)]
        for arm in ('baseline', 'graph'):
            with self.subTest(arm=arm):
                client, state, redactor = self.client('\n'.join(lines) + '\n', arm)
                delivered, seen, start = [], [], 1
                for _ in range(40):
                    body, text = self.call(client, 'read', {'path': 'src/lib.rs', 'start_line': start})
                    delivered.append(text)
                    self.assertEqual(body['start_line'], start)
                    seen += body['content'].splitlines()
                    if body['next_start_line'] is None:
                        break
                    self.assertGreater(body['next_start_line'], start)
                    start = body['next_start_line']
                else:
                    self.fail('read continuation did not finish')
                self.assertGreater(len(delivered), 1)
                self.assertEqual(seen, [f'{i}\t{line}' for i, line in enumerate(lines, 1)])
                self.check_log(client, state, redactor, delivered)

    def test_redaction_expansion_pages(self):
        value = 'host1379'  # 120 eight-byte values become ten-byte markers.
        first = 'fixture text ' + value * 120
        self.assertEqual(len(first.encode()), 973)
        text = first + '\nfixture café 漢字 "quoted" \\\n'
        for arm in ('baseline', 'graph'):
            for tool in ('rg', 'git'):
                with self.subTest(arm=arm, tool=tool):
                    client, state, redactor = self.client(text, arm, [value])
                    self.assertEqual(len(redactor.text(first)[0].encode()), 1213)
                    delivered = []
                    source = (''.join(f'src/lib.rs:{i}:{line}\n'
                        for i, line in enumerate(text.splitlines(), 1)) if tool == 'rg' else text)
                    expected = redactor.text(source)[0]
                    args = ({'pattern': 'fixture'} if tool == 'rg' else
                            {'op': 'file_at', 'rev': 'HEAD', 'path': 'src/lib.rs'})
                    self.pages(client, tool, args, expected, delivered)
                    calls = self.check_log(client, state, redactor, delivered)
                    self.assertNotIn(value, json.dumps(calls))
                    view = calls[0]['reply_provenance']['views'][0]
                    self.assertEqual(len(view['text']['spans']), 120)
                    self.assertEqual(view['safe_text'], expected)

    def test_long_read_line_is_cut_and_advances(self):
        value = 'host1379'
        text = 'fixture text ' + value * 120 + '\nfixture café 漢字 "quoted" \\\n'
        for arm in ('baseline', 'graph'):
            with self.subTest(arm=arm):
                client, state, redactor = self.client(text, arm, [value])
                delivered = []
                body, output = self.call(client, 'read', {'path': 'src/lib.rs'})
                delivered.append(output)
                self.assertIn('[line cut at ', body['content'])
                self.assertEqual(body['next_start_line'], 2)
                body, output = self.call(client, 'read', {'path': 'src/lib.rs', 'start_line': 2})
                delivered.append(output)
                self.assertEqual(body['content'], '2\t' + text.splitlines()[1])
                self.assertIsNone(body['next_start_line'])
                calls = self.check_log(client, state, redactor, delivered)
                self.assertNotIn(value, json.dumps(calls))

    def test_unpageable_product_includes_final_disclosure_in_bound(self):
        self.product_bound('x' * 640, fits_without_disclosure=True)

    def test_unpageable_product_redaction_expansion_preserves_raw_transport_identity(self):
        self.product_bound('host1379' * 80, values=['host1379'])

    def product_bound(self, product_text, values=(), fits_without_disclosure=False):
        orbit = self.work / 'private-orbit'
        # A stdio fixture only: no installation, service or host state.
        orbit.write_text(f'#!{sys.executable}\n' + '''import json, sys
assert json.loads(sys.stdin.readline())['method'] == 'initialize'
print(json.dumps({'jsonrpc': '2.0', 'id': 1, 'result': {
    'protocolVersion': '2024-11-05', 'serverInfo': {'name': 'orbit-mcp'}, 'capabilities': {}}}), flush=True)
assert json.loads(sys.stdin.readline())['method'] == 'notifications/initialized'
assert json.loads(sys.stdin.readline())['id'] == 2
print(json.dumps({'jsonrpc': '2.0', 'id': 2, 'result': {
    'isError': False, 'content': [{'type': 'text', 'text': PAYLOAD}]}}), flush=True)
assert sys.stdin.read() == ''
'''.replace('PAYLOAD', repr(product_text)))
        orbit.chmod(0o700)
        client, state, redactor = self.client('fixture\n', 'graph', values, orbit)
        result = client.request('tools/call', {'name': 'graph_search',
            'arguments': {'query': 'fixture'}})['result']
        self.assertTrue(result['isError'])
        output = result['content'][0]['text']
        self.assertLessEqual(len(output.encode()), 1024)
        body = json.loads(output)
        self.assertEqual(body['status'], 'truncated')
        self.assertEqual(body['error']['code'], 'call_output_truncated')
        self.assertEqual(client.close(), 0)
        self.assertEqual(json.loads((state / 'budget.json').read_text())['code'],
                         'call_output_truncated')
        entries = support.log_entries(state)
        full = next(e['reply'] for e in entries if e['type'] == 'plugin_output')
        # A product reply cannot be paged without changing its lossless contract.
        if fits_without_disclosure:
            self.assertLessEqual(len(broker.canonical(full).encode()), 1024)
        self.assertGreater(len(broker.canonical(dict(full, text_view=body['text_view'])).encode()), 1024)
        rows = [{'jsonrpc': '2.0', 'id': 1, 'result': {
                    'protocolVersion': '2024-11-05', 'serverInfo': {'name': 'orbit-mcp'}, 'capabilities': {}}},
                {'jsonrpc': '2.0', 'id': 2, 'result': {'isError': False,
                    'content': [{'type': 'text', 'text': product_text}]}}]
        self.assertEqual(full['transport']['stdout_sha256'],
                         replies.sha(''.join(json.dumps(row) + '\n' for row in rows)))
        call = next(e for e in entries if e['type'] == 'call')
        self.assertEqual(call['output'], output)
        proof = call['reply_provenance']
        self.assertFalse(proof['truncated'])  # bounded error, not cut JSON
        self.assertEqual(proof['delivered_bytes'], len(output.encode()))
        replies.verify_text(output, proof['text'], redactor.policy)
        for view in proof['views']:
            replies.verify_text(view['safe_text'], view['text'], redactor.policy)
        for value in values:
            self.assertNotIn(value, json.dumps(entries))


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

    def test_minimum_budget_reconciles_provider_log_proofs_and_episode_cost(self):
        self.fixture()
        self.host_value = 'host1379'
        text = support.HEAD['src/lib.rs'] + '// fixture text ' + self.host_value * 120 + '\n'
        text += ('// fixture café 漢字 "quoted" \\ ' * 3 + '\n') * 35
        self.head.joinpath('src/lib.rs').write_text(text)
        self.source, self.snapshot = support.snapshot_repo(self.work / 'bounded-source',
            files_head={'src/lib.rs': text}, files_base={'src/lib.rs': support.BASE['src/lib.rs']})
        steps = [{'call': tool, 'arguments': args} for tool, args in (
            ('read', {'path': 'src/lib.rs'}),
            ('read', {'path': 'src/lib.rs', 'start_line': 9}),
            ('rg', {'pattern': 'fixture'}), ('rg', {'pattern': 'fixture', 'offset': 512}),
            ('git', {'op': 'file_at', 'rev': 'HEAD', 'path': 'src/lib.rs'}),
            ('git', {'op': 'file_at', 'rev': 'HEAD', 'path': 'src/lib.rs', 'offset': 512}))]
        steps += [{'final': json.dumps(support.ANSWER)}]
        for arm in ('baseline', 'graph'):
            with self.subTest(arm=arm):
                code, report, stderr, out = self.plugin_episode(steps, arm=arm,
                    limits=dict(support.LIMITS, call_bytes=1024),
                    env={'FAKE_API_KEY': self.host_value}, shutdown='eof_then_sigterm')
                self.assertEqual(code, 0, (report, stderr))
                artifact = self.assert_outcome(out, 'ok', None)
                self.assertEqual(self.evaluator.load_episode(out, diagnostic=True)['status'], 'ok')
                self.assertEqual(artifact['redactions'], 0)
                self.assertEqual(artifact['output_bytes'], sum(len(c['output'].encode())
                    for c in artifact['calls']) + len(artifact['final_output'].encode()))
                for call, proof in zip(artifact['calls'], artifact['reply_provenance']):
                    self.assertLessEqual(len(call['output'].encode()), 1024)
                    self.assertEqual(json.loads(call['output'])['status'], 'ok')
                    self.assertFalse(proof['truncated'])
                    self.assertEqual(proof['delivered_bytes'], len(call['output'].encode()))
                transcript = runner.analyse_transcript((out / 'provider.jsonl').read_bytes(),
                                                       False, artifact['request']['tools'])
                self.assertIsNone(runner.reconcile_calls(transcript, artifact['calls']))
                self.assertNotIn(self.host_value, (out / 'provider.jsonl').read_text())
                self.assertEqual(artifact['source_provenance']['head']['commit'],
                                 self.snapshot['head_commit'])
                self.assertEqual(artifact['cleanup']['survivors'], [])

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
