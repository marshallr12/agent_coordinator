# Invoke the installed native CLI with this repository's non-secret binding.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $Session,

    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $CoordinatorArgs
)

$ErrorActionPreference = 'Stop'
$coordinatorExecutable = Join-Path $env:LOCALAPPDATA 'AgentCoordinator/bin/agent-coordinator.exe'
$coordinatorBinding = Join-Path (Split-Path -Parent $PSScriptRoot) '.agent-coordinator.toml'
if (-not (Test-Path -LiteralPath $coordinatorExecutable -PathType Leaf)) {
    throw 'Install the verified Windows Agent Coordinator CLI in %LOCALAPPDATA%/AgentCoordinator/bin first.'
}
if (-not $CoordinatorArgs) { $CoordinatorArgs = @('connect') }
& $coordinatorExecutable --repo-config $coordinatorBinding --session $Session @CoordinatorArgs
exit $LASTEXITCODE
