<#
.SYNOPSIS
  Refreshes the SAP Business One schema snapshot only when the server version changed.

.DESCRIPTION
  The generated catalogue (src-tauri/src/sapb1/catalogue.rs) carries the version
  the login response reported when it was built. This script logs in, reads that
  version back, and:

    * same version          -> does nothing, and says so;
    * different version     -> dumps $metadata and regenerates the catalogue;
    * -Force                -> regenerates even when the version matches.

  The point is that a schema refresh is tied to a real Business One upgrade, not
  to every run, so the catalogue cannot drift silently and is never regenerated
  for no reason.

.PARAMETER Metadata
  Where to write the raw $metadata dump. Default: %TEMP%\sap-metadata.xml.

.PARAMETER Out
  The catalogue file to regenerate. Default: src-tauri/src/sapb1/catalogue.rs.

.PARAMETER Force
  Regenerate even when the server reports the same version.

.NOTES
  Credentials come from the Windows Credential Manager, the same entries the app
  reads (sapb1-url, sapb1-company, sapb1-user, sapb1-password).
#>
[CmdletBinding()]
param(
    [string] $Metadata = "$env:TEMP\sap-metadata.xml",
    [string] $Out,
    [switch] $Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$script:Root = Split-Path -Parent $PSScriptRoot
if (-not $Out) { $Out = Join-Path $script:Root 'src-tauri\src\sapb1\catalogue.rs' }

# Read the Windows Credential Manager the way the app does. Generic credentials
# store the secret as UTF-16 in CredentialBlob.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public class CoucouCred {
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
  public struct CREDENTIAL {
    public uint Flags; public uint Type; public string TargetName; public string Comment;
    public long LastWritten; public uint CredentialBlobSize; public IntPtr CredentialBlob;
    public uint Persist; public uint AttributeCount; public IntPtr Attributes;
    public string TargetAlias; public string UserName;
  }
  [DllImport("advapi32.dll", EntryPoint="CredReadW", CharSet=CharSet.Unicode, SetLastError=true)]
  public static extern bool CredRead(string target, uint type, int reserved, out IntPtr credential);
  [DllImport("advapi32.dll")]
  public static extern void CredFree(IntPtr cred);
}
'@

function Get-CredentialValue([string] $Key) {
    $target = "$Key.fr.louisraille.coucou"
    $ptr = [IntPtr]::Zero
    if (-not [CoucouCred]::CredRead($target, 1, 0, [ref]$ptr)) {
        throw "No credential '$target' in the Windows Credential Manager. Save it in Coucou > Settings > Integrations first."
    }
    try {
        $c = [System.Runtime.InteropServices.Marshal]::PtrToStructure($ptr, [type][CoucouCred+CREDENTIAL])
        return [System.Runtime.InteropServices.Marshal]::PtrToStringUni($c.CredentialBlob, [int]($c.CredentialBlobSize / 2))
    }
    finally {
        [CoucouCred]::CredFree($ptr)
    }
}

[System.Net.ServicePointManager]::ServerCertificateValidationCallback = { $true }
[System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12

$base = "$(Get-CredentialValue 'sapb1-url')/b1s/v1"
$company = Get-CredentialValue 'sapb1-company'
$user = Get-CredentialValue 'sapb1-user'
$password = Get-CredentialValue 'sapb1-password'

function Invoke-Curl([string[]] $CurlArgs) {
    $output = & curl.exe @CurlArgs
    if ($LASTEXITCODE -ne 0) { throw "curl.exe failed with exit code $LASTEXITCODE." }
    return $output
}

Write-Host "Logging in to $company ..."
# curl and PowerShell both mangle the JSON, so it is written to a temp file.
$loginBody = Join-Path $env:TEMP 'coucou-login.json'
[System.IO.File]::WriteAllText($loginBody, (@{
    CompanyDB = $company
    UserName  = $user
    Password  = $password
} | ConvertTo-Json -Compress))

$login = Invoke-Curl @('-sk', '-X', 'POST', "$base/Login", '-H', 'Content-Type: application/json', '--data-binary', "@$loginBody") | ConvertFrom-Json
Remove-Item -LiteralPath $loginBody -Force -ErrorAction SilentlyContinue
if (-not $login.SessionId) { throw 'Login was refused. Check the saved credentials.' }

$serverVersion = [string]$login.Version
if (-not $serverVersion) { throw 'The login response carried no Version field.' }
$sid = $login.SessionId
Write-Host "Server version: $serverVersion"

# The version the committed catalogue was built from.
$currentVersion = $null
if (Test-Path -LiteralPath $Out) {
    $match = Select-String -Path $Out -Pattern 'pub const SAP_VERSION: &str = "([^"]*)"' |
        Select-Object -First 1
    if ($match) { $currentVersion = $match.Matches[0].Groups[1].Value }
}
Write-Host "Catalogue version: $(if ($currentVersion) { $currentVersion } else { '<none>' })"

if ($currentVersion -eq $serverVersion -and -not $Force) {
    Write-Host ''
    Write-Host "Schema is up to date for version $serverVersion. Nothing to do." -ForegroundColor Green
    exit 0
}

Write-Host ''
Write-Host 'Fetching $metadata ...'
Invoke-Curl @('-sk', '-H', "Cookie: B1SESSION=$sid", "$base/`$metadata", '-o', $Metadata) | Out-Null
$bytes = (Get-Item -LiteralPath $Metadata).Length
Write-Host "Dumped $bytes bytes to $Metadata"

Write-Host 'Regenerating the catalogue ...'
Push-Location $script:Root
try {
    & node scripts/gen-sap-catalogue.mjs --in $Metadata --out $Out --version $serverVersion
    if ($LASTEXITCODE -ne 0) { throw "The generator failed with exit code $LASTEXITCODE." }
}
finally {
    Pop-Location
}

Write-Host ''
Write-Host "Catalogue refreshed to version $serverVersion." -ForegroundColor Green
Write-Host 'Review the diff before committing: it is generated from your server.'
