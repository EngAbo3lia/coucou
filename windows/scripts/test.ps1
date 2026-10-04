<#
.SYNOPSIS
  Runs Coucou's tests, regenerates the SAP catalogue, and builds/installs the app.

.DESCRIPTION
  One entry point for everything that has to be checked before a change ships,
  so the commands in README.md cannot drift from the ones that actually work.

  Modes:
    offline     cargo test --lib plus the TypeScript check. No network, no server.
    coverage    Just sapb1::coverage, with --nocapture so the case count prints.
    live        One ignored live test. Credentials come from the Credential
                Manager, the same entries the app reads. WRITES DOCUMENTS.
    catalogue   Dump $metadata from the live server and regenerate catalogue.rs.
    build       Release build, front end bundled.
    install     build, then replace the installed coucou.exe and relaunch.

.PARAMETER Mode
  offline | coverage | live | catalogue | build | install. Default: offline.

.PARAMETER Test
  Substring of the live test name. Required for -Mode live. Examples:
    live_sales_cycle, live_partner, live_dump_metadata, live_ask, live_write_path

.PARAMETER Yes
  Skip the confirmation prompt. Only meaningful for live and install.

.EXAMPLE
  .\scripts\test.ps1 -Mode offline
  .\scripts\test.ps1 -Mode live -Test live_partner
  .\scripts\test.ps1 -Mode install

.NOTES
  The build target directory is redirected to %TEMP% by default, because the
  repo lives on a drive that runs out of space. Override with -TargetDir.
#>
[CmdletBinding()]
param(
    [ValidateSet('offline', 'coverage', 'live', 'catalogue', 'build', 'install')]
    [string] $Mode = 'offline',
    [string] $Test,
    [switch] $Yes,
    [switch] $Force,
    [string] $TargetDir = "$env:TEMP\opencode\coucou-target"
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$script:Root = Split-Path -Parent $PSScriptRoot
$script:Cargo = Join-Path $script:Root 'src-tauri\Cargo.toml'
$script:Installed = Join-Path $env:LOCALAPPDATA 'Coucou\coucou.exe'
$script:Catalogue = Join-Path $script:Root 'src-tauri\src\sapb1\catalogue.rs'

$env:CARGO_TARGET_DIR = $TargetDir

function Write-Step([string] $Message) {
    Write-Host ''
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Write-Fail([string] $Message) {
    Write-Host "    $Message" -ForegroundColor Red
}

function Write-Note([string] $Message) {
    Write-Host "    $Message" -ForegroundColor DarkGray
}

# Fails the script when a native command returns a non-zero exit code, which
# PowerShell does not do on its own.
function Invoke-Checked([string] $Exe, [string[]] $Arguments, [string] $What) {
    Write-Note "$Exe $($Arguments -join ' ')"
    # Cargo writes its warnings to stderr, and under `$ErrorActionPreference =
    # 'Stop'` PowerShell turns a native command's stderr into a terminating
    # error — so a passing run would be reported as a failure. Only the exit
    # code decides whether a command passed.
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        # Through the host, not the pipeline: otherwise the command's stdout
        # becomes part of the calling function's return value, and a caller that
        # expects one path gets npm's whole log instead.
        & $Exe @Arguments | Write-Host
    }
    finally {
        $ErrorActionPreference = $previous
    }
    if ($LASTEXITCODE -ne 0) {
        throw "$What failed with exit code $LASTEXITCODE."
    }
}

function Confirm-Destructive([string] $What) {
    if ($Yes) {
        return
    }
    Write-Host ''
    Write-Host "    $What" -ForegroundColor Yellow
    $answer = Read-Host '    Type yes to continue'
    if ($answer -cne 'yes') {
        throw 'Cancelled.'
    }
}

function Get-LiveTestNames() {
    $names = Select-String -Path (Join-Path $script:Root 'src-tauri\src\sapb1\*.rs') `
        -Pattern 'fn (live_\w+)' -AllMatches |
        ForEach-Object { $_.Matches } |
        ForEach-Object { $_.Groups[1].Value } |
        Sort-Object -Unique
    return @($names)
}

# ---------------------------------------------------------------------------

function Invoke-Offline {
    Write-Step 'Rust offline suite'
    Invoke-Checked 'cargo' @('test', '--manifest-path', $script:Cargo, '--lib') 'The Rust tests'

    Write-Step 'TypeScript'
    Push-Location $script:Root
    try {
        Invoke-Checked 'npx' @('tsc', '--noEmit', '-p', 'tsconfig.json') 'The TypeScript check'
    }
    finally {
        Pop-Location
    }

    Write-Host ''
    Write-Host 'Offline checks passed.' -ForegroundColor Green
}

function Invoke-Coverage {
    Write-Step 'Endpoint coverage sweep'
    Invoke-Checked 'cargo' @(
        'test', '--manifest-path', $script:Cargo, '--lib',
        'sapb1::coverage', '--', '--nocapture'
    ) 'The coverage sweep'
}

function Invoke-Live {
    if (-not $Test) {
        Write-Host 'Live tests available:' -ForegroundColor Yellow
        Get-LiveTestNames | ForEach-Object { Write-Host "    $_" }
        throw 'Pass -Test <name> to pick one.'
    }

    $known = @(Get-LiveTestNames)
    $match = @($known | Where-Object { $_ -like "*$Test*" })
    if ($match.Count -eq 0) {
        Write-Host "No live test matches '$Test'." -ForegroundColor Red
        Write-Host 'Known:' -ForegroundColor Yellow
        $known | ForEach-Object { Write-Host "    $_" }
        throw 'Unknown test.'
    }
    if ($match.Count -gt 1) {
        Write-Host "'$Test' matches more than one test:" -ForegroundColor Yellow
        $match | ForEach-Object { Write-Host "    $_" }
        throw 'Be more specific.'
    }

    $name = $match[0]

    # Only the document-creating tests need a confirmation. The rest are reads,
    # and warning on those trains the reader to skip the prompt.
    $WRITES_DOCUMENTS = @(
        'live_sales_cycle_create_order_then_invoice'
    )
    if ($WRITES_DOCUMENTS -contains $name) {
        Confirm-Destructive "$name creates real documents on the configured company."
    }
    elseif ($name -like 'live_ask*' -or $name -like 'live_write_path*' -or $name -like 'live_conversation*') {
        Write-Note "$name calls the configured chat backend, so it costs tokens and returns model-dependent output."
    }

    Write-Step "Live test $name"
    Invoke-Checked 'cargo' @(
        'test', '--manifest-path', $script:Cargo, '--lib',
        $name, '--', '--ignored', '--nocapture'
    ) "The live test $name"
}

function Invoke-Catalogue {
    # Delegates to the version-aware refresh: it only regenerates when the
    # server version differs from the one the catalogue carries.
    $refresh = Join-Path $PSScriptRoot 'refresh-sap-schema.ps1'
    Write-Step 'Refreshing the SAP schema catalogue'
    $arguments = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $refresh)
    if ($Force) { $arguments += '-Force' }
    Invoke-Checked 'powershell' $arguments 'The schema refresh'
}

function Invoke-Build {
    Write-Step 'Release build'
    Push-Location $script:Root
    try {
        Invoke-Checked 'npm' @('run', 'tauri', 'build', '--', '--no-bundle') 'The build'
    }
    finally {
        Pop-Location
    }

    $binary = Join-Path $TargetDir 'release\coucou.exe'
    if (-not (Test-Path -LiteralPath $binary)) {
        throw "The build finished but $binary is missing."
    }
    Write-Host ''
    Write-Host "Built $binary." -ForegroundColor Green
    return $binary
}

function Invoke-Install {
    Confirm-Destructive 'This replaces the installed Coucou and restarts it.'
    $binary = Invoke-Build

    Write-Step 'Installing'
    Get-Process coucou -ErrorAction SilentlyContinue | Stop-Process
    Start-Sleep -Seconds 2
    Copy-Item -LiteralPath $binary -Destination $script:Installed -Force
    Write-Note "copied to $script:Installed"

    $log = Join-Path $env:LOCALAPPDATA 'Coucou\coucou.log'
    $before = if (Test-Path -LiteralPath $log) { (Get-Item -LiteralPath $log).Length } else { 0 }

    Start-Process $script:Installed | Out-Null
    Start-Sleep -Seconds 12

    $process = Get-Process coucou -ErrorAction SilentlyContinue
    if (-not $process) {
        throw 'The app did not stay running after launch.'
    }
    Write-Host ''
    Write-Host "Coucou is running (pid $($process[0].Id))." -ForegroundColor Green

    if (Test-Path -LiteralPath $log) {
        $logItem = Get-Item -LiteralPath $log
        if ($logItem.Length -gt $before) {
            $line = Get-Content -LiteralPath $log -Tail 1
            Write-Note "log: $line"
        }
    }
}

# ---------------------------------------------------------------------------

try {
    switch ($Mode) {
        'offline' { Invoke-Offline }
        'coverage' { Invoke-Coverage }
        'live' { Invoke-Live }
        'catalogue' { Invoke-Catalogue }
        'build' { Invoke-Build | Out-Null }
        'install' { Invoke-Install }
    }
}
catch {
    # Written to stderr, not the host: a host-only message is invisible the
    # moment the script's output is redirected, which is exactly when it matters.
    [Console]::Error.WriteLine('')
    [Console]::Error.WriteLine("FAILED: $($_.Exception.Message)")
    if ($_.ScriptStackTrace) {
        [Console]::Error.WriteLine($_.ScriptStackTrace)
    }
    exit 1
}

exit 0