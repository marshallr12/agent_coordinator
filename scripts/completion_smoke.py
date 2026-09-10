"""Real Git, producer, reviewer, and integration acceptance using disposable data."""
import json
import subprocess
import sys
import time


def exercise_completion(temporary, api, cli, owner):
    root = temporary / 'completion fixture'
    root.mkdir()
    source, remote = root / 'source repository', root / 'remote repository.git'
    source.mkdir()

    def git(*args, cwd=source):
        result = subprocess.run(['git', '-C', str(cwd), *args], capture_output=True,
                                text=True, timeout=20)
        assert result.returncode == 0, f'Disposable completion Git operation failed: {args[0]}'
        return result.stdout.strip()

    git('init', '--initial-branch=main')
    git('init', '--bare', str(remote))
    git('config', 'user.name', 'Completion fixture')
    git('config', 'user.email', 'completion@example.invalid')
    (source / 'README.md').write_text('Initial source.\n')
    git('add', 'README.md')
    git('commit', '-m', 'Initial source')
    base = git('rev-parse', 'HEAD')
    git('remote', 'add', 'origin', str(remote))
    git('push', 'origin', 'main')
    project = api('/api/v1/projects', {'name':'Completion smoke',
                  'repository_url':str(remote), 'target_branch':'main'})
    p = project['id']
    policy = api(f'/api/v1/projects/{p}/policy', {
        'expected_revision':project['policy_revision'], 'review_mode':'both',
        'recovery_mode':'agent', 'lease_seconds':600, 'rules':'',
        'agent_rule_editing':False, 'automatic_integration':False}, method='PATCH')
    roster = api(f'/api/v1/projects/{p}/workflow-policy', {
        'expected_revision':0, 'canonical_repository_key':'smoke/completion',
        'required_checks':[{'identity':'acceptance', 'version':'1', 'environment':'smoke-native'}]}, method='PUT')

    def client(index, *args, **kwargs):
        return cli(index, *args, project_id=p, **kwargs)['data']

    reviewer = 1 - owner
    for index in [owner, reviewer]:
        client(index, 'connect')
    task = client(owner, 'tasks', 'create', body={
        'title':'Integrate reviewed source', 'description':'Complete against exact integrated evidence.',
        'acceptance_criteria':['Feature is present in the integrated result']})
    dependent = client(owner, 'tasks', 'create', body={
        'title':'Work unblocked by completion', 'acceptance_criteria':['Prerequisite is complete'],
        'depends_on':[task['id']]})
    claim = client(owner, 'claim', '--task', task['id'], '--revision', str(task['revision']))
    attempt = claim['claim']['attempt']
    own = ['--attempt',attempt['id'],'--generation',str(attempt['generation'])]
    implementation = root / 'implementation worktree'
    client(owner, 'worktree', 'prepare', *own, '--source',str(source), '--path',str(implementation),
           '--branch','task/feature', '--base',base)
    (implementation / 'feature.txt').write_text('Verified integration feature.\n')
    git('add','feature.txt',cwd=implementation)
    git('commit','-m','Implement feature',cwd=implementation)
    candidate = git('rev-parse','HEAD',cwd=implementation)
    submission_data = client(owner, 'submissions','code',*own,
        '--task-revision',str(task['revision']), '--project-policy-revision',str(policy['policy_revision']),
        '--workflow-policy-revision',str(roster['revision']), '--checkout',str(implementation),
        body={'summary':'Feature implemented.', 'acceptance_evidence':[
            {'criterion':'Feature is present in the integrated result','evidence':'feature.txt in the committed candidate.'}],
              'handoff':'Review and integrate this exact candidate.'})
    submission = submission_data['submission']
    activities = submission_data['activities']

    def activity(kind):
        return next(item for item in activities if item['kind'] == kind)

    def claim_args(item):
        return ['--activity',item['id'],'--submission',submission['id'],
                '--project-policy-revision',str(policy['policy_revision']),
                '--workflow-policy-revision',str(roster['revision'])]

    agent_review = activity('agent_review')
    review_claim = client(reviewer,'reviews','claim',*claim_args(agent_review))
    review_attempt = review_claim['attempt']
    client(reviewer,'reviews','decide','--activity',agent_review['id'],
           '--attempt',review_attempt['id'],'--generation',str(review_attempt['generation']),
           '--submission',submission['id'], body={'decision':'approved',
           'summary':'Independent reviewer inspected the exact candidate.', 'findings':[]})
    human_review = activity('human_review')
    human_claim = api(f'/api/v1/projects/{p}/workflow-activities/{human_review["id"]}/claim',{
        'expected_submission_id':submission['id'],
        'expected_project_policy_revision':policy['policy_revision'],
        'expected_workflow_policy_revision':roster['revision']})
    api(f'/api/v1/projects/{p}/workflow-activities/{human_review["id"]}/review',{
        'generation':human_claim['attempt']['generation'], 'submission_id':submission['id'],
        'decision':'approved','summary':'Operator inspected the candidate acceptance evidence.', 'findings':[]})
    integration = activity('integration')
    api(f'/api/v1/projects/{p}/workflow-activities/{integration["id"]}/authorization',{
        'submission_id':submission['id'],'expected_project_policy_revision':policy['policy_revision'],
        'expected_workflow_policy_revision':roster['revision'],'summary':'Authorize this exact integration.'})
    integration_claim = client(owner,'integrations','claim',*claim_args(integration))
    integrate_attempt = integration_claim['attempt']
    integration_own = ['--attempt',integrate_attempt['id'],'--generation',str(integrate_attempt['generation'])]
    integration_args = ['--activity',integration['id'],*integration_own]
    checkout = root / 'integration worktree'
    client(owner,'worktree','prepare',*integration_own,'--source',str(source),'--path',str(checkout),
           '--branch','task/integration','--base',base)
    preparation = ['integrations','prepare',*integration_args,'--submission',submission['id'],
                   '--checkout',str(checkout),'--expected-target',base,'--candidate',candidate]
    client(owner,*preparation)
    client(owner,*preparation)  # Reconcile the retained local and server intents.
    result_revision = git('rev-parse','HEAD',cwd=checkout)
    assert (checkout / 'feature.txt').is_file(), 'Integrated result was not made available for checks.'
    cli(owner,'integrations','publish',*integration_args,project_id=p,expected=7)
    assert git('ls-remote','origin','refs/heads/main').split()[0] == base, 'Publication bypassed required checks.'
    resource = api('/api/v1/resources', {'key':'smoke/completion-check','capacity':1,'description':'Exact result check'})
    reservation = client(owner,'resources','reserve',*integration_own,
        body={'items':[{'resource_id':resource['id'],'units':1}]})
    reservation_id = reservation.get('reservation',reservation)['id']
    job_input = root / 'check.json'
    job_input.write_text(json.dumps({'label':'Integrated acceptance', 'program':sys.executable,
        'argv':['-c','from pathlib import Path; assert Path("feature.txt").read_text() == "Verified integration feature.\\n"'],
        'environment':{},'log_limit_bytes':4096,
        'check_identity':'acceptance','check_version':'1','check_environment':'smoke-native'}))
    client(owner,'jobs','run',*integration_own,'--activity',integration['id'],'--reservation',reservation_id,
           '--checkout',str(checkout),'--input',str(job_input))
    jobs = api(f'/api/v1/projects/{p}/jobs')['items']
    job_id = next(job['id'] for job in jobs if job['attempt_id'] == integrate_attempt['id'])
    deadline = time.monotonic()+20
    while time.monotonic() < deadline:
        job = api(f'/api/v1/projects/{p}/jobs/{job_id}')
        if job['state'] == 'succeeded':
            break
        assert job['state'] not in ['failed','not_started'], 'Exact integrated check failed.'
        time.sleep(.1)
    else:
        raise AssertionError('Exact integrated check was not reported.')
    assert job['source_revision'] == result_revision and job['inputs_unchanged']
    client(owner,'reservations','release','--reservation',reservation_id,
           '--generation',str(integrate_attempt['generation']),'--reason','Producer completed on exact integrated source.')
    client(owner,'integrations','publish',*integration_args)
    client(owner,'integrations','publish',*integration_args)  # Historical success never republishes.
    assert api(f'/api/v1/projects/{p}/tasks/{task["id"]}')['lifecycle'] != 'done'
    assert api(f'/api/v1/projects/{p}/tasks/{dependent["id"]}')['work_status'] == 'blocked'
    client(owner,'integrations','finish',*integration_args,
           body={'submission_id':submission['id'], 'summary':'Published exact integrated source with required checks.', 'check_job_ids':[job_id]})
    assert api(f'/api/v1/projects/{p}/tasks/{task["id"]}')['lifecycle'] == 'done'
    assert api(f'/api/v1/projects/{p}/tasks/{dependent["id"]}')['work_status'] == 'ready'
    assert git('ls-remote','origin','refs/heads/main').split()[0] == result_revision
    print('PASS: immutable submission, independent agent and human review, authorization,')
    print('      real Git integration, exact producer checks, publication, and dependency release.')
