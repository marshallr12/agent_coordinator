"""Exercise operator milestones through the built HTTP service and native CLI."""
import secrets
import subprocess


def exercise_operator(api, cli, owner, server_options):
    project = api('/api/v1/projects', {
        'name': 'Operator workflow', 'repository_url': 'https://example.com/operator.git',
        'target_branch': 'main'})['id']
    cli(owner, 'connect', project_id=project)
    task = cli(owner, 'tasks', 'create', project_id=project, body={
        'title': 'Draft child', 'description': 'Needs admission', 'kind': 'general',
        'acceptance_criteria': ['Preserve the intended scope'], 'planned': True})['data']
    updated = cli(owner, 'tasks', 'edit', '--id', task['id'], project_id=project, body={
        'expected_revision': task['revision'], 'title': 'Admitted child',
        'description': task['description'], 'acceptance_criteria': task['acceptance_criteria'],
        'priority': 2, 'depends_on': [], 'planned': False})['data']
    assert updated['title'] == 'Admitted child'
    assert cli(owner, 'tasks', 'show', '--id', task['id'], project_id=project)['data']['revision'] == 2
    history = cli(owner, 'tasks', 'history', '--id', task['id'], '--kind', 'task_revisions',
                  '--limit', '1', project_id=project)['data']
    assert len(history['items']) == 1 and history['next_cursor']
    second = cli(owner, 'tasks', 'history', '--id', task['id'], '--kind', 'task_revisions',
                 '--limit', '1', '--cursor', history['next_cursor'], project_id=project)['data']
    assert len(second['items']) == 1 and second['items'] != history['items']
    objective = cli(owner, 'objectives', 'create', project_id=project, body={
        'title': 'Complete the objective', 'acceptance_criteria': ['Review the complete outcome'],
        'children': [{'task_id': task['id'], 'required': True}]})['data']
    objective = objective.get('objective', objective)
    assert not objective['required_children_ready']
    assert len(cli(owner, 'objectives', 'list', project_id=project)['data']['items']) == 1
    detail = cli(owner, 'objectives', 'show', '--id', objective['id'], project_id=project)['data']
    assert detail['children'][0]['task_id'] == task['id']
    cli(owner, 'objectives', 'children', '--id', objective['id'], project_id=project,
        body={'expected_revision': detail['objective_revision'],
              'children': [{'task_id': task['id'], 'required': True}]})
    policy = cli(owner, 'policy', 'show', project_id=project)['data']
    assert policy['policy_revision'] == 1
    assert cli(owner, 'policy', 'history', project_id=project)['data']['items']
    operator = api('/api/v1/admin/operators', {'name': 'additional-operator', 'role': 'operator',
        'password': secrets.token_hex(24)})['operator']
    disabled = api(f"/api/v1/admin/operators/{operator['id']}/access", {
        'expected_revision': operator['revision'], 'role': 'operator', 'enabled': False})
    assert not disabled['operator']['enabled']
    recovered = subprocess.run(server_options + ['recover-operator-password', '--username',
        'additional-operator', '--reason', 'Disposable host recovery exercise', '--password-stdin'],
        input=secrets.token_hex(24) + '\n', text=True, capture_output=True, timeout=30)
    assert recovered.returncode == 0, 'Built host account recovery failed.'
    assert api(f"/api/v1/admin/operators/{operator['id']}")['operator']['enabled']
    account = api('/api/v1/auth/account')['operator']
    assert account['enabled'] and account['role'] == 'admin'
    assert any(item['current'] for item in api('/api/v1/browser-sessions')['items'])
    old = api('/api/v1/admin/agents', {'name': 'rotation-smoke'})
    rotated = api(f"/api/v1/admin/credentials/{old['credential_id']}/rotate", {'name': 'rotated-smoke'})
    assert rotated['principal_id'] == old['principal_id']
    assert rotated['replaced_credential_revoked'] and rotated['token'] != old['token']
    print('PASS: native task admission/history/objectives/policy and operator access/rotation/host recovery.')
