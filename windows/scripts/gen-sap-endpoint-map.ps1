<#
.SYNOPSIS
  Regenerates the SAP Business One endpoint capability map.

.DESCRIPTION
  For every entity set the server exposes, this records two things:

    * read   - can a plain GET return rows;
    * write  - what a POST with an empty body answers, classified by the SAP
               error code (the HTTP status is misleading: business errors are
               returned as 404 too, so the code in the body decides).

  The result is docs/sap-endpoint-map.csv and a one-line summary per category.

  WARNING: this POSTs an empty body to every endpoint. On a company that is
  disposable this is fine; on a real one it can create records on the few paths
  that accept an empty body. Run it against a learning company only.

.NOTES
  Credentials come from the Windows Credential Manager, the same entries the app
  reads (sapb1-url, sapb1-company, sapb1-user, sapb1-password).
#>
[CmdletBinding()]
param(
    [string] $Out,
    [switch] $SkipProbe
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$script:Root = Split-Path -Parent $PSScriptRoot
if (-not $Out) { $Out = Join-Path $script:Root 'docs\sap-endpoint-map.csv' }

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public class CoucouMapCred {
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
    if (-not [CoucouMapCred]::CredRead($target, 1, 0, [ref]$ptr)) {
        throw "No credential '$target'. Save it in Coucou > Settings > Integrations first."
    }
    try {
        $c = [System.Runtime.InteropServices.Marshal]::PtrToStructure($ptr, [type][CoucouMapCred+CREDENTIAL])
        return [System.Runtime.InteropServices.Marshal]::PtrToStringUni($c.CredentialBlob, [int]($c.CredentialBlobSize / 2))
    }
    finally {
        [CoucouMapCred]::CredFree($ptr)
    }
}

[System.Net.ServicePointManager]::ServerCertificateValidationCallback = { $true }
[System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12

$base = "$(Get-CredentialValue 'sapb1-url')/b1s/v1"
$loginBody = Join-Path $env:TEMP 'coucou-map-login.json'
[System.IO.File]::WriteAllText($loginBody, (@{
    CompanyDB = Get-CredentialValue 'sapb1-company'
    UserName  = Get-CredentialValue 'sapb1-user'
    Password  = Get-CredentialValue 'sapb1-password'
} | ConvertTo-Json -Compress))
$login = & curl.exe -sk -X POST "$base/Login" -H 'Content-Type: application/json' --data-binary "@$loginBody" | ConvertFrom-Json
Remove-Item -LiteralPath $loginBody -Force -ErrorAction SilentlyContinue
if (-not $login.SessionId) { throw 'Login was refused.' }
$sid = $login.SessionId
Write-Host "logged in to $(Get-CredentialValue 'sapb1-company') (B1 $($login.Version))"

$svc = & curl.exe -sk -H "Cookie: B1SESSION=$sid" "$base/" | ConvertFrom-Json
$sets = $svc.value | Where-Object { $_.kind -eq 'EntitySet' } | ForEach-Object { $_.name }
Write-Host "entity sets: $($sets.Count)"

$tmp = Join-Path $env:TEMP 'coucou-map-body.tmp'
$rows = New-Object System.Collections.Generic.List[object]
$i = 0
foreach ($set in $sets) {
    $i++
    if ($i % 50 -eq 0) { Write-Host "  ... $i / $($sets.Count)" }

    $read = & curl.exe -sk -o $tmp -w '%{http_code}' -H "Cookie: B1SESSION=$sid" "${base}/${set}?`$top=1&`$count=true"
    $readRows = ''
    if ($read -eq '200') {
        try { $readRows = (Get-Content $tmp -Raw | ConvertFrom-Json).'odata.count' } catch { $readRows = '' }
    }

    $write = ''
    $code = ''
    $detail = ''
    if (-not $SkipProbe) {
        $write = & curl.exe -sk -o $tmp -w '%{http_code}' -X POST -H "Cookie: B1SESSION=$sid" -H 'Content-Type: application/json' --data-binary '{}' "$base/$set"
        try {
            $j = Get-Content $tmp -Raw | ConvertFrom-Json
            if ($j.error) {
                # The code, not the HTTP status, says what happened: business
                # errors also come back as 404, so `writeHttp` alone would lie.
                $code = [string]$j.error.code
                $detail = [string]$j.error.message.value
            }
        } catch { }
    }

    $rows.Add([pscustomobject]@{
        set       = $set
        readHttp  = [int]$read
        readRows  = $readRows
        writeHttp = if ($write) { [int]$write } else { '' }
        writeCode = $code
        writeNote = $detail
    })
}

$dir = Split-Path -Parent $Out
if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
$rows | Export-Csv -Path $Out -NoTypeInformation -Encoding UTF8
Write-Host "wrote $Out"
$readable = ($rows | Where-Object { $_.readHttp -eq 200 }).Count
Write-Host "readable: $readable / $($sets.Count)"
if (-not $SkipProbe) {
    $rows | Group-Object writeCode | Sort-Object Count -Descending |
        ForEach-Object { "  {0,-8} {1}" -f $_.Name, $_.Count }
}
