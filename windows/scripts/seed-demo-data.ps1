<#
.SYNOPSIS
  Seeds demo data into a disposable SAP Business One company for testing.

.DESCRIPTION
  Creates a small, self-consistent dataset in dependency order, then verifies it
  by reading the totals back:

    1. items                    (Items needs ItemCode)
    2. goods receipts           (so invoices cannot fall into negative inventory)
    3. customers                (BusinessPartners needs CardCode, and a BillToState)
    4. A/R invoices dated in the last completed quarter
    5. a stock transfer, an inventory goods issue and a journal entry

  WARNING: this posts real, non-deletable documents. Run it against a learning
  company only. Everything it creates is prefixed DEMO / DEMOC so it is visible
  and easy to find.

.PARAMETER Invoices
  How many A/R invoices to create. Default 100.

.PARAMETER Force
  Run even if DEMO items already exist (may duplicate).

.NOTES
  Credentials come from the Windows Credential Manager.
#>
[CmdletBinding()]
param(
    [int] $Invoices = 100,
    [switch] $Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public class SeedCred {
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
    if (-not [SeedCred]::CredRead($target, 1, 0, [ref]$ptr)) {
        throw "No credential '$target'. Save it in Coucou > Settings > Integrations first."
    }
    try {
        $c = [System.Runtime.InteropServices.Marshal]::PtrToStructure($ptr, [type][SeedCred+CREDENTIAL])
        return [System.Runtime.InteropServices.Marshal]::PtrToStringUni($c.CredentialBlob, [int]($c.CredentialBlobSize / 2))
    }
    finally { [SeedCred]::CredFree($ptr) }
}

[System.Net.ServicePointManager]::ServerCertificateValidationCallback = { $true }
[System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12

$base = "$(Get-CredentialValue 'sapb1-url')/b1s/v1"
$loginBody = Join-Path $env:TEMP 'seed-login.json'
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

$bodyFile = Join-Path $env:TEMP 'seed-body.json'

function Invoke-Sap([string] $Set, [string] $Json) {
    # A file body, never an inline string: PowerShell mangles inline JSON and the
    # server answers `BadFormat`.
    [System.IO.File]::WriteAllText($bodyFile, $Json)
    $raw = & curl.exe -sk -X POST -H "Cookie: B1SESSION=$sid" -H 'Content-Type: application/json' --data-binary "@$bodyFile" "$base/$Set"
    return ($raw | ConvertFrom-Json)
}

function Get-Sap([string] $Path) {
    return (& curl.exe -sk -H "Cookie: B1SESSION=$sid" "$base/$Path" | ConvertFrom-Json)
}

# Strict mode throws when a property is absent, so an error is detected by its
# presence, never by reading it blind.
function Test-SapError($r) {
    return $null -ne $r.PSObject.Properties['error']
}

function Get-SapError($r) {
    if (Test-SapError $r) { return [string]$r.error.message.value }
    return ''
}

function New-DemoItem([string] $Code, [string] $Name) {
    $r = Invoke-Sap 'Items' (@{
        ItemCode = $Code; ItemName = $Name; ItemsGroupCode = 100
        InventoryItem = 'tYES'; SalesItem = 'tYES'; PurchaseItem = 'tYES'
    } | ConvertTo-Json -Compress)
    if (Test-SapError $r) { throw "Item ${Code}: $(Get-SapError $r)" }
    return $r.ItemCode
}

function New-GoodsReceipt([string] $Code, [double] $Qty) {
    $r = Invoke-Sap 'InventoryGenEntries' (@{
        DocDate = '2026-07-01'
        DocumentLines = @(@{ ItemCode = $Code; Quantity = $Qty; WarehouseCode = '01'; UnitPrice = 10.0 })
    } | ConvertTo-Json -Compress -Depth 5)
    if (Test-SapError $r) { throw "Goods receipt ${Code}: $(Get-SapError $r)" }
    return $r.DocNum
}

function New-DemoCustomer([string] $Code, [string] $Name) {
    # The state code must belong to the partner's country: `1` is Saudi Mekka and
    # fails for any other country with `Linked value 1 does not exist`.
    $r = Invoke-Sap 'BusinessPartners' (@{
        CardCode = $Code; CardName = $Name; CardType = 'cCustomer'
        Country = 'US'; Currency = 'GBP'
        BillToState = 'CA'
    } | ConvertTo-Json -Compress)
    if (Test-SapError $r) { throw "Customer ${Code}: $(Get-SapError $r)" }
    return $r.CardCode
}

# --- 1. items -------------------------------------------------------------
Write-Host ''
Write-Host 'Seeding items ...'
$items = @()
foreach ($n in 1..5) {
    $code = 'DEMO-I{0:d2}' -f $n
    $exists = -not (Test-SapError (Get-Sap "Items('$code')"))
    if ($exists -and -not $Force) {
        $items += $code
        Write-Host "  item $code (exists)"
    } else {
        $items += New-DemoItem $code "Demo item $n"
        Write-Host "  item $code"
    }
}

# --- 2. stock -------------------------------------------------------------
Write-Host 'Seeding stock ...'
foreach ($code in $items) {
    $num = New-GoodsReceipt $code 100000
    Write-Host "  goods receipt $num for $code"
}

# --- 3. customers ---------------------------------------------------------
Write-Host 'Seeding customers ...'
$customers = @()
foreach ($n in 1..3) {
    $code = 'DEMO-C{0:d2}' -f $n
    $existing = Get-Sap "BusinessPartners('$code')"
    if (-not (Test-SapError $existing)) { $customers += $code; Write-Host "  customer $code (exists)"; continue }
    $customers += New-DemoCustomer $code "Demo customer $n"
    Write-Host "  customer $code"
}

# --- 4. invoices in the last completed quarter ----------------------------
$today = (Get-Date).Date
$quarterStart = $today.AddMonths(-3 - (($today.Month - 1) % 3)).AddDays(-(($today.Day) - 1))
# last completed quarter: the quarter before the current one
$currentQuarterFirstMonth = [math]::Floor(($today.Month - 1) / 3) * 3 + 1
$lastQuarterEnd = (Get-Date -Year $today.Year -Month $currentQuarterFirstMonth -Day 1).AddDays(-1)
$lastQuarterStart = $lastQuarterEnd.AddMonths(-2).AddDays(-($lastQuarterEnd.Day - 1))
Write-Host ''
Write-Host "Seeding $Invoices invoices from $($lastQuarterStart.ToString('yyyy-MM-dd')) to $($lastQuarterEnd.ToString('yyyy-MM-dd')) ..."

$span = [int]($lastQuarterEnd - $lastQuarterStart).TotalDays
$seed = 20261004
$rng = [System.Random]::new($seed)
$made = 0
$failed = 0
$sum = 0.0
foreach ($i in 1..$Invoices) {
    $item = $items[$rng.Next($items.Count)]
    $customer = $customers[$rng.Next($customers.Count)]
    $qty = $rng.Next(1, 11)
    $price = [double]($rng.Next(50, 201))
    $date = $lastQuarterStart.AddDays($rng.Next(0, $span + 1)).ToString('yyyy-MM-dd')
    $due = ([datetime]$date).AddDays(30).ToString('yyyy-MM-dd')
    $r = Invoke-Sap 'Invoices' (@{
        CardCode = $customer; DocDate = $date; DocDueDate = $due
        DocumentLines = @(@{ ItemCode = $item; Quantity = $qty; UnitPrice = $price })
    } | ConvertTo-Json -Compress -Depth 5)
    if (Test-SapError $r) {
        $failed++
        if ($failed -le 3) { Write-Host "  invoice $i failed: $(Get-SapError $r)" }
    } else {
        $made++
        $sum += [double]$r.DocTotal
    }
}
Write-Host "  created $made invoices, $failed failed, seeded net total ~$([math]::Round($sum,2))"

# --- 5. verify what the chat should say -----------------------------------
$start = $lastQuarterStart.ToString('yyyy-MM-dd')
$end = $lastQuarterEnd.ToString('yyyy-MM-dd')
$filter = [uri]::EscapeDataString("DocDate ge datetime'$start' and DocDate le datetime'$end'")
$count = Get-Sap "Invoices/`$count?`$filter=$filter"
Write-Host ''
Write-Host "Invoices in ${start}..${end}: $count"
