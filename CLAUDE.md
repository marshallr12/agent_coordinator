Read `.agent-coordinator.toml` for the service URL and project ID. Fetch
`{service_url}/api/v1/info` anonymously over HTTPS with redirects disabled.
Read `data.agent_startup.guide` and the advertised same-origin authentication help.
Follow that workflow: automatically select, claim, and begin eligible work at
session start unless the user explicitly changes the scope. Follow CONTRIBUTING.md
for repository engineering requirements. Keep credentials outside the repository
and never print them. If discovery or authentication fails, report the blocker.
