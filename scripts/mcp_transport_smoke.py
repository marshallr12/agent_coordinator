"""Real adapter crash/replay checks against disposable service data; no CLI needed.

The proxy forwards first, then cuts the claim response. Assertions read actual
journal bytes and operator service state independently of adapter/model output.
"""
import json
import os
from pathlib import Path
import secrets
import subprocess
import threading
import urllib.request
import urllib.error
import uuid
import time
from http.server import BaseHTTPRequestHandler, HTTPServer, ThreadingHTTPServer


def verified_git_clean(directory):
    root = subprocess.run(['git', '-C', str(directory), 'rev-parse', '--show-toplevel'], capture_output=True, text=True)
    if root.returncode != 0 or Path(root.stdout.strip()).resolve() != Path(directory).resolve():
        raise AssertionError('Workspace is not a verified repository root; cleanliness is unverified.')
    result = subprocess.run(['git', '-C', str(directory), 'status', '--porcelain=v1',
                             '--untracked-files=all'], capture_output=True, text=True)
    if result.returncode != 0:
        raise AssertionError('Git status failed; cleanliness is unverified.')
    return result.stdout == ''


def exercise_mcp_transport(temporary, api, project, origin, binary, evaluation_directory=None):
    credential = api('/api/v1/admin/agents', {'name': 'standalone-adapter-eval' if evaluation_directory else 'standalone-adapter-smoke'})
    token, session, proof = credential['token'], str(uuid.uuid4()), secrets.token_hex(32)
    state = temporary / 'standalone-journal'
    dropped = threading.Event()
    sent = []
    first_claim = []
    replay_verified = []

    class Proxy(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = self.rfile.read(int(self.headers['Content-Length']))
            call = json.loads(body)
            params = call.get('params', {})
            # The durable request MUST exist before the first upstream write.
            if call.get('method') == 'tools/call' and 'idempotency_key' in params.get('arguments', {}):
                saved = json.loads((state / 'journal.json').read_text())
                key = params['arguments']['idempotency_key']
                assert saved['pending'] == key and saved['requests'][key] == params
                assert token not in json.dumps(saved) and proof not in json.dumps(saved)
                sent.append(params)
            headers = {k: v for k, v in self.headers.items() if k.lower() not in ('host', 'content-length')}
            request = urllib.request.Request(origin + '/mcp', data=body, headers=headers)
            try:
                with urllib.request.urlopen(request, timeout=10) as response:
                    payload, status = response.read(), response.status
            except urllib.error.HTTPError as response:
                payload, status = response.read(), response.code
            if params.get('name') == 'coordinator_claim' and not dropped.is_set():
                assert status == 200 and json.loads(payload)['result'].get('isError') is not True
                receipt = json.loads(payload)['result']['structuredContent']['data']
                first_claim.append(receipt['claim']['attempt'])
                dropped.set()
                self.close_connection = True
                return
            if params.get('name') == 'coordinator_claim' and first_claim:
                current = api(f"/api/v1/projects/{project}/attempts/{first_claim[0]['id']}")['attempt']
                assert current['session_id'] == session and current['expires_at'] == first_claim[0]['expires_at']
                replay_verified.append(True)
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    proxy = ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
    thread = threading.Thread(target=proxy.serve_forever, daemon=True)
    thread.start()
    env = {k: v for k, v in os.environ.items() if not k.startswith('AGENT_COORDINATOR_')}
    env.update(AGENT_COORDINATOR_MCP_URL=f'http://127.0.0.1:{proxy.server_port}/mcp',
               AGENT_COORDINATOR_MCP_TOKEN=token, AGENT_COORDINATOR_MCP_SESSION_ID=session,
               AGENT_COORDINATOR_MCP_SESSION_PROOF=proof, AGENT_COORDINATOR_MCP_PROJECT_ID=project)
    command = [str(binary), '--state-dir', str(state), '--allow-insecure-loopback']
    process = None

    def start():
        return subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True, env=env)

    def rpc(method, params):
        identity = str(uuid.uuid4())
        process.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': identity,
                                       'method': method, 'params': params}) + '\n')
        process.stdin.flush()
        line = process.stdout.readline()
        assert line and token not in line and proof not in line, 'Adapter exited or exposed secrets.'
        value = json.loads(line)
        assert value['id'] == identity
        return value

    def tool(name, arguments=None, failure=False):
        result = rpc('tools/call', {'name': name, 'arguments': arguments or {}})
        if failure:
            assert 'error' in result or result['result'].get('isError') is True
            return result
        assert 'error' not in result, result.get('error')
        assert result['result'].get('isError') is not True, 'Tool failed.'
        return result['result']['structuredContent']

    def mutation(name, body, **arguments):
        arguments.update(body=body, idempotency_key=str(uuid.uuid4()))
        return tool(name, arguments)

    def initialize():
        value = rpc('initialize', {'protocolVersion': '2025-11-25', 'capabilities': {},
                                 'clientInfo': {'name': 'standalone-smoke', 'version': '1'}})
        assert 'error' not in value
        status = tool('coordinator_transport_status')
        assert status['durable_mutation_journal']
        assert status['configured_identity']['session_id'] == session
        assert status['configured_identity']['project_id'] == project

    try:
        process = start()
        initialize()
        duplicate = subprocess.run(command, input='', text=True, capture_output=True, env=env, timeout=10)
        assert duplicate.returncode != 0, 'Concurrent adapter acquired the same journal.'
        if evaluation_directory is not None:
            directory = Path(evaluation_directory)
            directory.mkdir(mode=0o700, parents=True, exist_ok=True)
            task = api(f'/api/v1/projects/{project}/tasks', {'title': 'Bounded startup evaluation',
                       'description': 'Claim, inspect workspace without editing source, checkpoint, and release. Do not submit or implement code.',
                       'kind': 'code', 'acceptance_criteria': ['Truthful ownership and workspace observations'], 'priority': 1})
            transcript = []
            class Gateway(BaseHTTPRequestHandler):
                def log_message(self, *_):
                    pass
                def do_POST(self):
                    nonlocal process
                    value = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    response = rpc(value['method'], value.get('params', {}))
                    transcript.append({'request': value, 'response': response})
                    if dropped.is_set() and value.get('params', {}).get('name') == 'coordinator_claim':
                        # Force actual adapter process death after the upstream
                        # effect but before the evaluator can retry.
                        process.kill()
                        process.communicate(timeout=10)
                        process = start()
                        initialize()
                    payload = json.dumps(response).encode()
                    self.send_response(200)
                    self.send_header('Content-Length', str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
            gateway = HTTPServer(('127.0.0.1', 0), Gateway)
            gateway_thread = threading.Thread(target=gateway.serve_forever, daemon=True)
            gateway_thread.start()
            helper = directory / 'mcp_call.py'
            helper.write_text('import json,sys,urllib.request\n'
                + 'value={"method":sys.argv[1],"params":json.loads(sys.argv[2]) if len(sys.argv)>2 else {}}\n'
                + 'if value["method"].startswith("coordinator_"): value={"method":"tools/call","params":{"name":value["method"],"arguments":value["params"]}}\n'
                + f'request=urllib.request.Request("http://127.0.0.1:{gateway.server_port}/", data=json.dumps(value).encode(), headers={{"Content-Type":"application/json"}})\n'
                + 'with urllib.request.urlopen(request, timeout=40) as response: print(response.read().decode())\n')
            (directory / 'ready.json').write_text(json.dumps({'project': project, 'task': task['id'],
                'service_origin': origin, 'helper': str(helper), 'session': session}))
            try:
                deadline = time.monotonic() + 900
                while not (directory / 'finished.json').exists() and time.monotonic() < deadline:
                    time.sleep(0.25)
                assert (directory / 'finished.json').exists(), 'Evaluator did not finish before deadline.'
                observed = api(f"/api/v1/projects/{project}/tasks/{task['id']}")
                saved = json.loads((state / 'journal.json').read_text())
                assert token not in json.dumps(transcript) and proof not in json.dumps(transcript)
                (directory / 'transcript.json').write_text(json.dumps(transcript, indent=2))
                claims = [call for call in sent if call['name'] == 'coordinator_claim']
                assert dropped.is_set() and len(claims) == 2 and claims[0] == claims[1]
                assert len(observed['attempts']) == 1 and observed['current_attempt_id'] is None
                assert observed['attempts'][0]['session_id'] == session
                assert observed['checkpoints'] and saved['pending'] is None
                assert all(call['arguments']['idempotency_key'] in saved['requests'] for call in sent)
                assert token not in json.dumps(transcript) and proof not in json.dumps(transcript)
                model_report = json.loads((directory / 'finished.json').read_text())
                git_check = subprocess.run(['git', '-C', str(directory), 'status', '--porcelain=v1'], capture_output=True, text=True)
                assert git_check.returncode != 0, 'Evaluation fixture unexpectedly became a Git repository.'
                assert model_report.get('workspace_clean') is None and model_report.get('git_exit_code') == git_check.returncode, 'Unsupported workspace-cleanliness claim.'
                assert replay_verified, 'Receipt replay authority was not independently inspected.'
                report = {'git_exit_code': git_check.returncode, 'cleanliness_unverified': True, 'receipt_did_not_renew': True, 'forced_interruption': True, 'exact_replay': True, 'attempt_count': 1,
                          'released': True, 'journal_verified': True, 'transcript': transcript,
                          'task': observed, 'model_report': model_report}
                (directory / 'verified.json').write_text(json.dumps(report, indent=2))
                print('PASS: isolated evaluator actual journal, exact interrupted replay, single attempt, checkpoint and release independently verified.')
            finally:
                gateway.shutdown()
                gateway.server_close()
                gateway_thread.join(timeout=10)
            return
        tool('coordinator_session_register', {}, failure=True)
        assert not tool('coordinator_transport_status')['pending']
        mutation('coordinator_session_register', {'session_id': session,
                 'workstation_id': 'disposable-adapter', 'harness': 'standalone-smoke',
                 'capabilities': ['code']})
        orientation = tool('coordinator_orientation', {'project': project})['data']
        mutation('coordinator_instructions_ack', {'instruction_version': orientation['instruction_version'],
                 'sections': orientation['required_sections'], 'policy_revision': orientation['policy_revision'], 'project_id': project})
        task = api(f'/api/v1/projects/{project}/tasks', {'title': 'Adapter interrupted claim',
                   'description': 'Disposable transport test', 'kind': 'code',
                   'acceptance_criteria': ['One claim only'], 'priority': 1})
        arguments = {'project': project, 'idempotency_key': str(uuid.uuid4()),
                     'body': {'task_id': task['id'], 'expected_task_revision': task['revision'], 'policy_revision': orientation['policy_revision'], 'instruction_version': orientation['instruction_version']}}
        tool('coordinator_claim', arguments, failure=True)
        assert dropped.wait(5)
        process.kill()
        process.communicate(timeout=10)
        saved = json.loads((state / 'journal.json').read_text())
        assert saved['pending'] == arguments['idempotency_key']
        observed = api(f"/api/v1/projects/{project}/tasks/{task['id']}")
        assert len(observed['attempts']) == 1
        before = observed['attempts'][0]
        assert before['session_id'] == session and before['state'] == 'active'
        process = start()
        initialize()
        assert tool('coordinator_transport_status')['pending']
        # Reads work; a new mutation is refused before dispatch.
        tool('coordinator_session_get')
        count = len(sent)
        tool('coordinator_claim', dict(arguments, idempotency_key=str(uuid.uuid4())), failure=True)
        assert len(sent) == count
        tool('coordinator_transport_retry')
        assert not tool('coordinator_transport_status')['pending']
        after = api(f"/api/v1/projects/{project}/tasks/{task['id']}")['attempts']
        assert len(after) == 1 and after[0]['id'] == before['id']
        assert after[0]['expires_at'] == before['expires_at'] and after[0]['session_id'] == session
        assert sent[-1] == sent[-2], 'Recovery did not replay exact arguments.'
        # Simulate the host losing an already-completed stdio reply.
        process.kill()
        process.communicate(timeout=10)
        process = start()
        initialize()
        tool('coordinator_transport_retry')
        latest = api(f"/api/v1/projects/{project}/tasks/{task['id']}")['attempts']
        assert len(latest) == 1 and latest[0]['expires_at'] == before['expires_at']
        mutation('coordinator_checkpoint', {'generation': before['generation'], 'summary': 'Forced interruption independently verified.'}, project=project, attempt=before['id'])
        mutation('coordinator_attempt_release', {'generation': before['generation'], 'summary': 'Adapter smoke complete.', 'blocked': False}, project=project, attempt=before['id'])
        assert api(f"/api/v1/projects/{project}/tasks/{task['id']}")['current_attempt_id'] is None
        invalid = temporary / 'not-a-repository'
        invalid.mkdir()
        try:
            verified_git_clean(invalid)
        except AssertionError:
            pass
        else:
            raise AssertionError('Invalid Git check accepted as clean.')
        print('PASS: standalone MCP durable pre-dispatch journal, forced post-commit interruption, exact replay, one owner/no renewal, concurrent exclusion, invalid Git rejection.')
    finally:
        if process is not None and process.poll() is None:
            process.kill()
            process.communicate(timeout=10)
        proxy.shutdown()
        proxy.server_close()
        thread.join(timeout=10)
