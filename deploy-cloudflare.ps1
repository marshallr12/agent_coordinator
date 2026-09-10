#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Builds the Agent Coordinator mdBook and deploys it to Cloudflare Pages (free plan).

.DESCRIPTION
    End-to-end deploy script using Cloudflare's Wrangler CLI (run via `npx`).
    On each run it will:

      1. Ensure you are authenticated to Cloudflare (see AUTHENTICATION below).
      2. Create the Pages project if it does not already exist.
      3. Build the book with the pinned `mdbook` into target/book.
      4. Upload the built site as a new production deployment.
      5. Fetch the live site and confirm the deployed index carries the book title.

    Cloudflare Pages' free plan has unlimited bandwidth and free SSL. A Pages
    project serves at the ROOT of its own *.pages.dev domain, so mdBook's relative
    links need no base path.

    This publishes documentation only. It does not deploy the coordination
    service, and it never touches the service's data, credentials, or backups.

.AUTHENTICATION
    Two options; pick one:

    (a) Interactive (easiest for a workstation): run `npx wrangler login` once.
        Wrangler opens a browser, you authorize, and the token is cached locally.

    (b) API token (better for CI / headless): create a token with the
        "Cloudflare Pages:Edit" permission at
        https://dash.cloudflare.com/profile/api-tokens and set these env vars:
            $env:CLOUDFLARE_API_TOKEN  = '<your-token>'
            $env:CLOUDFLARE_ACCOUNT_ID = '<your-account-id>'
        Wrangler reads both automatically. Never commit or print the token.

.PREREQUISITES
    - Node.js + npx (provides Wrangler on demand): https://nodejs.org
    - The pinned mdBook on PATH: cargo install mdbook --version 0.5.4 --locked
    - A free Cloudflare account: https://dash.cloudflare.com/sign-up

.PARAMETER ProjectName
    Cloudflare Pages project name (also the *.pages.dev subdomain).
    Default: agent-coordinator-docs

.PARAMETER ProductionBranch
    Branch name Cloudflare treats as "production" for this project. This is a
    label for direct (non-Git) uploads and need not match a real Git branch.
    Default: main

.EXAMPLE
    ./deploy-cloudflare.ps1
    Build and deploy using defaults (creating the project on first run).

.EXAMPLE
    ./deploy-cloudflare.ps1 -ProjectName my-docs
#>

[CmdletBinding()]
param(
    [string]$ProjectName      = 'agent-coordinator-docs',
    [string]$ProductionBranch = 'main'
)

# Stop on the first error so a failed step never silently deploys stale content.
# `$ErrorActionPreference` alone governs PowerShell errors only; it does NOT react
# to a native command (npx, mdbook) exiting non-zero. The second line makes pwsh
# 7.3+ treat native failures as errors too. The explicit exit-code guards below
# remain for older versions, where that variable is inert.
$ErrorActionPreference                   = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Resolve paths relative to THIS script. book.toml sits at the repository root
# and sets build-dir = "target/book"; keep the two in step.
$BookRoot   = $PSScriptRoot
$BookOutput = Join-Path $BookRoot 'target' 'book'
$BookTitle  = 'Agent Coordinator'

# Fail fast with a clear message if a required CLI is missing.
function Assert-Command($name, $hint) {
    if (-not (Get-Command $name -ErrorAction SilentlyContinue)) {
        throw "Required command '$name' not found on PATH. $hint"
    }
}

# Pull the project names out of `pages project list --json`. Wrangler may print
# an update banner around the payload, so slice from the first '[' to the last
# ']' rather than feeding the whole capture to ConvertFrom-Json. 'Project Name'
# is wrangler's display key; fall back to a plain 'name' should it rename it.
function Get-PagesProjectNames($outputLines) {
    $text  = ($outputLines | Out-String)
    $start = $text.IndexOf('[')
    $end   = $text.LastIndexOf(']')
    if ($start -lt 0 -or $end -le $start) {
        throw "Could not parse the project list returned by wrangler:`n$text"
    }
    $parsed = $text.Substring($start, $end - $start + 1) | ConvertFrom-Json
    return @($parsed | ForEach-Object { $_.'Project Name' ?? $_.name })
}

# Create the Pages project unless it already exists. Comparing names explicitly
# matters: `-notmatch` on an ARRAY is a filter, not a boolean, and would report
# "missing" whenever any other row failed to match.
function Confirm-PagesProject($name, $branch) {
    Write-Host "==> Ensuring Cloudflare Pages project '$name'..." -ForegroundColor Cyan
    try {
        $projects = npx @Wrangler pages project list --json
    } catch {
        throw ("Could not list Cloudflare Pages projects. If the message above is an auth " +
               "error, run 'npx wrangler login', or set CLOUDFLARE_API_TOKEN and " +
               "CLOUDFLARE_ACCOUNT_ID (see .AUTHENTICATION in this script's header).")
    }
    if ((Get-PagesProjectNames $projects) -contains $name) {
        Write-Host '    already exists.'
        return
    }
    # Capture rather than throw so an "already exists" answer (renamed JSON key,
    # concurrent create) does not abort a deploy that would have worked.
    $PSNativeCommandUseErrorActionPreference = $false
    $create     = npx @Wrangler pages project create $name --production-branch $branch --force 2>&1
    $createExit = $LASTEXITCODE
    $PSNativeCommandUseErrorActionPreference = $true
    if ($createExit -eq 0) {
        Write-Host '    created.'
    } elseif (($create -join "`n") -match '8000002|already exists') {
        Write-Host '    already exists.'
    } else {
        $create | ForEach-Object { Write-Host $_ }
        throw "Could not create Pages project '$name' (wrangler exited $createExit)."
    }
}

# Build the book from the repository root. target/book may hold a previous
# build, so gate on the exit code rather than on the directory existing.
function Build-Book($root, $output) {
    Write-Host '==> Building mdBook...' -ForegroundColor Cyan
    mdbook build $root
    if ($LASTEXITCODE -ne 0) {
        throw "mdbook build failed (exited $LASTEXITCODE). Nothing was published."
    }
    if (-not (Test-Path (Join-Path $output 'index.html'))) {
        throw "Build did not produce '$output/index.html'."
    }
}

# Upload the built site. `--branch` tags the upload as production so it lands
# on the main *.pages.dev URL rather than a preview URL.
function Publish-Book($output, $name, $branch) {
    Write-Host '==> Deploying to Cloudflare Pages...' -ForegroundColor Cyan
    npx @Wrangler pages deploy $output --project-name $name --branch $branch --commit-dirty=true --force
    if ($LASTEXITCODE -ne 0) {
        throw "Deployment failed (wrangler exited $LASTEXITCODE). Nothing was published."
    }
}

# Fetch the live index and require the book title, retrying while the edge
# propagates. A successful upload is not proof the site serves.
function Test-LiveSite($url, $title) {
    Write-Host "==> Verifying $url ..." -ForegroundColor Cyan
    foreach ($attempt in 1..12) {
        try {
            $response = Invoke-WebRequest -Uri $url -UseBasicParsing -TimeoutSec 20
            if ($response.StatusCode -eq 200 -and $response.Content -match [regex]::Escape($title)) {
                Write-Host "    served HTTP 200 with title '$title' (attempt $attempt)."
                return
            }
        } catch {
            Write-Host "    attempt $attempt not ready: $($_.Exception.Message)"
        }
        Start-Sleep -Seconds 5
    }
    throw "Deployed site at $url did not serve the expected index within the retry window."
}

Assert-Command 'npx'    'Install Node.js (provides npx): https://nodejs.org'
Assert-Command 'mdbook' 'Install mdBook: cargo install mdbook --version 0.5.4 --locked'

# `-y` lets npx fetch Wrangler on first use without prompting. Wrangler 4.131+
# delegates Pages commands to Workers static assets unless `--force` is given;
# without it, project creation and deploy fail with "Missing entry-point".
$Wrangler = @('-y', 'wrangler')

if ([string]::IsNullOrWhiteSpace($env:CLOUDFLARE_API_TOKEN)) {
    Write-Host 'No CLOUDFLARE_API_TOKEN set; assuming an interactive `wrangler login` session.' -ForegroundColor Yellow
    Write-Host 'If deploy fails with an auth error, run:  npx wrangler login' -ForegroundColor Yellow
}

Confirm-PagesProject $ProjectName $ProductionBranch
Build-Book $BookRoot $BookOutput
Publish-Book $BookOutput $ProjectName $ProductionBranch

$ProductionUrl = "https://$ProjectName.pages.dev/"
Test-LiveSite $ProductionUrl $BookTitle

Write-Host ''
Write-Host "==> Done. Production URL: $ProductionUrl" -ForegroundColor Green
Write-Host '    (The exact deployment URL is also printed by Wrangler just above.)'
