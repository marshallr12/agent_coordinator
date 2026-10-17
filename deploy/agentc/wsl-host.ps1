#requires -Version 5.1
<#
.SYNOPSIS
Keep an accepted WSL2 supervisor online under its owning Windows account.
.DESCRIPTION
Install registers a logon task, without starting it. Stop drains Linux first.
Shutdown and Restart drain before requesting Windows shutdown. No credentials
are accepted, copied, or stored. Linux bring-up is in wsl-host.py and the book.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Install', 'Run', 'Stop', 'Shutdown', 'Restart', 'Uninstall')]
    [string]$Action,
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$')]
    [string]$Distribution = 'AgentcUbuntu',
    [ValidatePattern('^Agentc-WSL-[A-Za-z0-9_.-]+$')]
    [string]$TaskName = 'Agentc-WSL-Supervisor'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-VMConfiguration {
    $config = Join-Path $env:USERPROFILE '.wslconfig'
    $values = @{}
    $section = ''
    foreach ($line in (Get-Content -LiteralPath $config)) {
        $text = $line.Trim()
        if ($text -match '^\[([^\]]+)\]$') { $section = $Matches[1].ToLowerInvariant() }
        elseif ($section -eq 'wsl2' -and $text -match '^([A-Za-z]+)\s*=\s*([^#;]+)') {
            $key = $Matches[1].ToLowerInvariant()
            if ($values.ContainsKey($key)) { throw "Duplicate WSL setting: $key" }
            $values[$key] = $Matches[2].Trim().ToLowerInvariant()
        }
    }
    $expected = @{ networkingmode = 'nat'; dnstunneling = 'false'; autoproxy = 'false'; localhostforwarding = 'false'; guiapplications = 'false' }
    foreach ($key in $expected.Keys) {
        if ($values[$key] -ne $expected[$key]) { throw "WSL VM configuration requires $key=$($expected[$key]). Review wslconfig.example and restart at a quiet point." }
    }
    foreach ($key in @('memory', 'processors', 'swap')) {
        if (-not $values.ContainsKey($key)) { throw "Set a WSL VM resource limit: $key" }
    }
}

function Invoke-Linux([string]$Operation) {
    & "$env:SystemRoot\System32\wsl.exe" --distribution $Distribution --user root --exec /opt/agentc/bin/agentc-wsl-host $Operation
    if ($LASTEXITCODE -ne 0) { throw "WSL $Operation failed ($LASTEXITCODE); Windows shutdown was not requested." }
}

function Stop-HostLoop {
    # Do not terminate WSL, kill a task, or request Windows shutdown before drain.
    Invoke-Linux 'stop'
    $task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    if ($null -ne $task) { Stop-ScheduledTask -InputObject $task }
}

switch ($Action) {
    'Install' {
        Assert-VMConfiguration
        Invoke-Linux 'ready'
        if ($null -ne (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue)) {
            throw "Task $TaskName already exists; inspect it and use Uninstall before replacing it."
        }
        $directory = Join-Path $env:LOCALAPPDATA 'Agentc\WSL'
        New-Item -ItemType Directory -Force -Path $directory | Out-Null
        $installed = Join-Path $directory 'wsl-host.ps1'
        # The task persists after the source checkout is removed.
        if ([IO.Path]::GetFullPath($PSCommandPath) -ne [IO.Path]::GetFullPath($installed)) {
            Copy-Item -LiteralPath $PSCommandPath -Destination $installed
        }
        if ($installed.Contains('"')) { throw 'Unsupported quote in installation path.' }
        $identity = [Security.Principal.WindowsIdentity]::GetCurrent().Name
        $principal = New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive -RunLevel Limited
        $trigger = New-ScheduledTaskTrigger -AtLogOn -User $identity
        $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) `
            -MultipleInstances IgnoreNew -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) `
            -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable
        $arguments = '-NoProfile -NonInteractive -WindowStyle Hidden -File "{0}" -Action Run -Distribution {1} -TaskName {2}' -f $installed, $Distribution, $TaskName
        $taskAction = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -Argument $arguments
        Register-ScheduledTask -TaskName $TaskName -Action $taskAction -Trigger $trigger `
            -Principal $principal -Settings $settings -Description "Accepted agentc supervisor in $Distribution; drain with wsl-host.ps1 -Action Stop." | Out-Null
        Write-Output "Registered $TaskName for $identity at logon. Start with Start-ScheduledTask -TaskName $TaskName."
    }
    'Run' { Assert-VMConfiguration; Invoke-Linux 'run' }
    'Stop' { Stop-HostLoop }
    'Shutdown' {
        Stop-HostLoop
        & "$env:SystemRoot\System32\shutdown.exe" /s /t 0
        if ($LASTEXITCODE -ne 0) { throw 'Windows shutdown request failed.' }
    }
    'Restart' {
        Stop-HostLoop
        & "$env:SystemRoot\System32\shutdown.exe" /r /t 0
        if ($LASTEXITCODE -ne 0) { throw 'Windows restart request failed.' }
    }
    'Uninstall' {
        Stop-HostLoop
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
        Write-Output 'Removed the Windows task. Linux installation and evidence were preserved.'
    }
}
