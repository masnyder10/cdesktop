<#
.SYNOPSIS
    Re-sync cdesktop's imported Claude Code chat history.

.DESCRIPTION
    Importing is a point-in-time snapshot, so a session that was still being
    written when it was imported stays frozen at that point. This script finds
    the running cdesktop instance and asks it to re-sync.

    The backend picks a fresh port on every launch, so the port is discovered
    from the running process rather than hardcoded.

.PARAMETER Scan
    Report what would happen without writing anything.

.PARAMETER SessionId
    Refresh only this Claude session id. Repeatable.

.EXAMPLE
    .\refresh-claude-history.ps1
    Import anything new and re-sync everything already imported.

.EXAMPLE
    .\refresh-claude-history.ps1 -Scan
    Show what is on disk and what has already been imported.
#>
[CmdletBinding()]
param(
    [switch]$Scan,
    [string[]]$SessionId
)

$ErrorActionPreference = 'Stop'

$proc = Get-Process cdesktop-tauri -ErrorAction SilentlyContinue
if (-not $proc) {
    Write-Error "cdesktop is not running. Start it first, then re-run this script."
    exit 1
}

# The app listens on two loopback ports (backend + preview proxy). The backend
# is the lower of the pair, and it is the one that answers /api/health.
$ports = Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue |
    Where-Object { $_.OwningProcess -in @($proc.Id) } |
    Select-Object -ExpandProperty LocalPort |
    Sort-Object -Unique

$base = $null
foreach ($p in $ports) {
    try {
        $h = Invoke-RestMethod -Uri "http://[::1]:$p/api/health" -TimeoutSec 5
        if ($h.success) { $base = "http://[::1]:$p"; break }
    } catch { }
}

if (-not $base) {
    Write-Error "Found cdesktop (pid $($proc.Id)) but no API port answered on $($ports -join ', ')."
    exit 1
}

Write-Host "cdesktop API: $base" -ForegroundColor DarkGray

if ($Scan) {
    $r = Invoke-RestMethod -Uri "$base/api/claude-import/scan" -TimeoutSec 300
    Write-Host "already imported : $($r.data.already_imported)"
    Write-Host "not yet imported : $($r.data.sessions.Count)"
    $r.data.sessions | ForEach-Object {
        Write-Host ("  [{0,4} msgs] {1}  ::  {2}" -f $_.message_count, $_.title, $_.cwd)
    }
    return
}

$body = @{ refresh = $true }
if ($SessionId) { $body.session_ids = @($SessionId) }

Write-Host "Refreshing (this rewrites imported transcripts in place)..." -ForegroundColor Cyan
$r = Invoke-RestMethod -Uri "$base/api/claude-import/run" `
    -Method Post `
    -Body ($body | ConvertTo-Json -Depth 4) `
    -ContentType "application/json" `
    -TimeoutSec 900

Write-Host ""
Write-Host "newly imported : $($r.data.imported)"   -ForegroundColor Green
Write-Host "refreshed      : $($r.data.refreshed)"  -ForegroundColor Green
Write-Host "skipped        : $($r.data.skipped)"
if ($r.data.failed.Count -gt 0) {
    Write-Host "failed         : $($r.data.failed.Count)" -ForegroundColor Yellow
    Write-Host "  (usually transcripts whose working directory no longer exists)" -ForegroundColor DarkGray
}
Write-Host ""
Write-Host "Reopen a session in the app to see refreshed content." -ForegroundColor DarkGray
