<#
.SYNOPSIS
Checks that attribution.ps1 prints ree's own failure message in Windows PowerShell 5.1.
#>
param([string]$Ree = (Join-Path $PSScriptRoot '..\..\target\release\ree.exe'))

$ErrorActionPreference = 'Stop'
$Ree = (Resolve-Path -LiteralPath $Ree).Path
$root = Join-Path $env:TEMP ('ree-hw-attribution-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
try {
    # Make the profile directory unwritable so ree fails without writing a profile.json.
    $blocked = Join-Path $root 'not-a-folder'
    [System.IO.File]::WriteAllText($blocked, 'x')
    $script = Join-Path $root 'attribution.ps1'
    $text = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'attribution.ps1')
    $outLine = @($text -split "`r?`n" | Where-Object { $_.StartsWith('$out = Join-Path ') })[0]
    $forcedOut = "`$out = '" + $blocked.Replace("'", "''") + "'"
    [System.IO.File]::WriteAllText($script, $text.Replace($outLine, $forcedOut))
    $output = (& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $script -Ree $Ree) -join "`n"
    if ($LASTEXITCODE -ne 2 -or $output -notmatch 'could not run: ree exited' -or $output -notmatch '--profile-out') {
        throw "ree's failure message was not printed: $output"
    }
    if ($output -match 'NativeCommandError|CategoryInfo|FullyQualifiedErrorId') {
        throw "PowerShell decorated ree's output: $output"
    }
    Write-Host 'ok: ree printed its profile-output error without PowerShell error decoration'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force
}
