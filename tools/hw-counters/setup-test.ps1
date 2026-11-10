<#
.SYNOPSIS
Checks setup.ps1's install steps that need no elevation, in temporary folders: the build is installed only when its
SHA-256 is the one step 1 printed (a changed build is refused before anything is written), the copy and the folder get
the ACL setup.ps1 means (Administrators and SYSTEM full control, Users read and run, nothing inherited), a folder that
is a link elsewhere is refused, and a script on PATH named like a cmdlet setup.ps1 uses does not run in its place.

.DESCRIPTION
Run from an ordinary PowerShell (Windows PowerShell 5.1 or later): `powershell -NoProfile -ExecutionPolicy Bypass -File
tools\hw-counters\setup-test.ps1`. Exit 0 when every check passed. (As an ordinary account the copies and folders stay
the account's own: setup.ps1, elevated, makes them Administrators'.)
#>
$ErrorActionPreference = 'Stop'
$setup = Join-Path $PSScriptRoot 'setup.ps1'
. $setup

$failed = 0
function Check([string]$What, [bool]$Passed, [string]$Detail = '') {
    if ($Passed) { Write-Host "ok      $What" } else { Write-Host "FAILED  $What $Detail"; $script:failed++ }
}

# Runs the printed guarded command in a fresh Windows PowerShell, with $LASTEXITCODE set to `StaleCode` first, then prints
# 'after <$LASTEXITCODE>' to show what the shell is left with. Returns everything the shell printed, as one string
function Invoke-GuardedCommand([string]$Guard, [int]$StaleCode) {
    $command = "`$global:LASTEXITCODE = $StaleCode; $Guard; 'after ' + `$LASTEXITCODE"
    return (& powershell.exe -NoProfile -ExecutionPolicy Bypass -Command $command) -join "`n"
}

$root = Join-Path $env:TEMP ('ree-hw-setup-test-' + [guid]::NewGuid().ToString('N'))
$source = Join-Path $root 'build'
$install = Join-Path $root 'install'
$protected = Join-Path $root 'protected'
$link = Join-Path $root 'link'
$shadows = Join-Path $root 'path'
New-Item -ItemType Directory -Force -Path $source, $install, $protected, $shadows | Out-Null
try {
    $exe = Join-Path $source 'ree-hw-counters.exe'
    [System.IO.File]::WriteAllBytes($exe, [byte[]](1..200))
    $hash = (Get-Sha256 ([System.IO.File]::ReadAllBytes($exe)))

    # a build that matches the hash step 1 printed: staged in the folder, the same bytes, the protected ACL
    $staged = Copy-Verified $exe $install 'ree-hw-counters.exe' $hash.ToLowerInvariant() -KeepOwner
    Check 'a build with its own hash is staged' ((Test-Path -LiteralPath $staged) -and ((Get-Sha256 ([System.IO.File]::ReadAllBytes($staged))) -eq $hash))
    $protectedCopy = $true
    try { Test-Protected $staged -KeepOwner } catch { $protectedCopy = $false; $copyError = $_.Exception.Message }
    Check 'the staged copy has the protected ACL, nothing inherited' $protectedCopy $copyError
    # (the copy gives an ordinary account no write: deleting it takes the folder's own right, which DeleteFile uses)
    [System.IO.File]::Delete($staged)

    # a build changed after its hash was printed: refused before anything is written
    $refused = $null
    try { Copy-Verified $exe $install 'ree-hw-counters.exe' ('0' * 64) -KeepOwner | Out-Null } catch { $refused = $_.Exception.Message }
    Check 'a changed build is refused' ($refused -match 'changed after it was built') $refused
    Check 'and nothing is written' (@(Get-ChildItem -LiteralPath $install -Force).Count -eq 0)

    $malformed = $null
    try { Copy-Verified $exe $install 'ree-hw-counters.exe' 'abc' -KeepOwner | Out-Null } catch { $malformed = $_.Exception.Message }
    Check 'a malformed hash is refused' ($malformed -match '64 hex digits') $malformed

    # the install folder's ACL: exactly three rules, protected
    Set-ProtectedFolder $protected -KeepOwner
    $acl = Get-Acl -LiteralPath $protected
    $rules = @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
    $text = ($rules | ForEach-Object { "$($_.IdentityReference.Value)=$($_.FileSystemRights)" }) -join '; '
    Check 'the folder inherits nothing' $acl.AreAccessRulesProtected
    Check 'Administrators and SYSTEM full control, Users read and run' (
        $rules.Count -eq 3 -and
        ($rules | Where-Object { $_.IdentityReference.Value -eq 'S-1-5-32-544' -and $_.FileSystemRights -eq 'FullControl' }) -and
        ($rules | Where-Object { $_.IdentityReference.Value -eq 'S-1-5-18' -and $_.FileSystemRights -eq 'FullControl' }) -and
        ($rules | Where-Object { $_.IdentityReference.Value -eq 'S-1-5-32-545' -and $_.FileSystemRights.ToString() -eq 'ReadAndExecute, Synchronize' })) $text
    $denied = $false
    try { [System.IO.File]::WriteAllText((Join-Path $protected 'x.txt'), 'x') } catch { $denied = $true }
    Check 'an ordinary account cannot write into it' $denied
    $ownerChecked = $null
    try { Test-Protected $protected } catch { $ownerChecked = $_.Exception.Message }
    Check 'an owner other than Administrators is caught' ($ownerChecked -match 'not Administrators') $ownerChecked

    # a folder that is a link elsewhere (a junction to the build folder): refused, its target untouched
    & cmd.exe /c mklink /J "$link" "$source" | Out-Null
    $linked = $null
    try { Set-ProtectedFolder $link -KeepOwner } catch { $linked = $_.Exception.Message }
    Check 'a folder that is a link elsewhere is refused' ($linked -match 'link elsewhere') $linked
    Check 'and its target keeps its ACL' (-not (Get-Acl -LiteralPath $source).AreAccessRulesProtected)

    # scripts on PATH named like the cmdlets setup.ps1 uses: in a fresh Windows PowerShell they run in the cmdlets'
    # place, unless the modules were imported from PowerShell's folder first, as setup.ps1 does
    # (each script, when it runs, appends its cmdlet's name to the marker file)
    $marker = Join-Path $root 'shadowed.txt'
    foreach ($cmdlet in 'Get-FileHash', 'Get-Acl', 'Copy-Item', 'Join-Path', 'Get-Service', 'New-Service') {
        $shadowScript = "Add-Content -LiteralPath $(Format-Quoted $marker) -Value $(Format-Quoted $cmdlet)"
        Set-Content -LiteralPath (Join-Path $shadows "$cmdlet.ps1") -Value $shadowScript -Encoding Ascii
    }
    # The shadowed cmdlets that ran, in the order they ran (none when the marker file was never written)
    function Get-ShadowedRuns {
        if (Test-Path -LiteralPath $marker) { return @(Get-Content -LiteralPath $marker) }
        return @()
    }
    # Runs a fresh Windows PowerShell with the scripts' folder first on PATH, `BeforeCmdlets` first, then three of the
    # shadowed cmdlets. The shell stops at its first error, so a quoting mistake or a failed cmdlet gives a nonzero exit
    # code instead of a pass without the check. Returns the shell's exit code
    function Invoke-ShadowedPathShell([string]$BeforeCmdlets) {
        $quotedRoot = Format-Quoted $root
        $command = "`$ErrorActionPreference = 'Stop'; `$env:PATH = $(Format-Quoted ($shadows + ';')) + `$env:PATH; $BeforeCmdlets " +
            "`$null = Get-Acl -LiteralPath $quotedRoot; `$null = Join-Path $quotedRoot 'x'; `$null = Get-Service -Name 'EventLog'"
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -Command $command | Out-Null
        return $LASTEXITCODE
    }

    $controlCode = Invoke-ShadowedPathShell ''
    $control = Get-ShadowedRuns
    Check 'without the import, the PATH scripts run in the cmdlets'' place (the check below means something)' ($controlCode -eq 0 -and $control.Count -ge 1) "(exit $controlCode; $($control -join ', '))"
    if (Test-Path -LiteralPath $marker) { Remove-Item -LiteralPath $marker -Force }
    $importCode = Invoke-ShadowedPathShell (". $(Format-Quoted $setup);")
    $after = Get-ShadowedRuns
    Check 'with setup.ps1''s import, the cmdlets run and the PATH scripts do not' ($importCode -eq 0 -and $after.Count -eq 0) "(exit $importCode; $($after -join ', '))"

    # the script is ASCII and has no BOM, so the hash of its text as it runs is its file's hash
    $bytes = [System.IO.File]::ReadAllBytes($setup)
    Check 'setup.ps1 is ASCII, without a BOM' (-not ($bytes | Where-Object { $_ -gt 127 }) -and -not ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF))
    Check 'a quote in a printed string is doubled' ((Format-Quoted "it's") -eq "'it''s'")

    # PSReadLine in the account's own module folder is found (step 1 warns), and nothing is found without it
    $documents = Join-Path $root 'Documents'
    $readLine = Join-Path $documents 'WindowsPowerShell\Modules\PSReadLine'
    New-Item -ItemType Directory -Force -Path $readLine | Out-Null
    Check 'PSReadLine in the account''s module folder is found' ((Find-UserReadLine $documents) -eq $readLine)
    Remove-Item -LiteralPath $readLine -Recurse -Force
    Check 'and nothing without it' ($null -eq (Find-UserReadLine $documents))

    # the printed step-2 command, for a copy of setup.ps1 in a folder whose name holds a quote: run unelevated it runs the
    # bytes it hashed (which then refuse, unelevated) and leaves the shell running; on a changed copy it runs nothing
    $quoted = Join-Path $root "it's here"
    New-Item -ItemType Directory -Force -Path $quoted | Out-Null
    $copy = Join-Path $quoted 'setup.ps1'
    Copy-Item -LiteralPath $setup -Destination $copy
    $scriptPath = $copy
    $arguments = "-Binary $(Format-Quoted (Join-Path $quoted 'x.exe')) -Sha256 $('0' * 64)"
    $guard = Format-Guarded (Get-Sha256 ([System.IO.File]::ReadAllBytes($copy))) $arguments
    $ran = Invoke-GuardedCommand $guard 0
    Check 'the guarded command runs the checked bytes, and the shell goes on' (($ran -match 'take an elevated PowerShell') -and ($ran -match 'after 1')) $ran
    Add-Content -LiteralPath $copy -Value '# changed after its hash was printed' -Encoding Ascii
    $changed = Invoke-GuardedCommand $guard 0
    Check 'on a changed script it runs nothing and reports failure' (($changed -match 'not the one that printed this') -and ($changed -notmatch 'elevated PowerShell') -and ($changed -match 'after 1')) $changed
    Remove-Item -LiteralPath $copy
    # (with the copy gone, the command cannot read it; 73 is a stale code it must not leave behind)
    $missing = Invoke-GuardedCommand $guard 73
    Check 'a guard read failure reports failure and leaves the shell running' (($missing -match 'could not run setup.ps1') -and ($missing -match 'after 1')) $missing

    # Invoke-Setup with its steps replaced by mocks, so these never touch the service or run cargo. Each block's functions
    # last only as long as the block
    & {
        function Test-Elevated { return $true }
        function Invoke-Install { throw 'mock install failure' }
        $global:LASTEXITCODE = 0
        $result = Invoke-Setup
        Check 'an install exception returns failure instead of an earlier zero' ($result -eq 1 -and $LASTEXITCODE -eq 1)
    }
    & {
        function Test-Elevated { return $false }
        function Get-Command { return $null }
        $global:LASTEXITCODE = 73
        $result = Invoke-Setup
        Check 'missing cargo returns failure instead of a stale native exit code' ($result -eq 1 -and $LASTEXITCODE -eq 1)
    }
    & {
        function Test-Elevated { return $true }
        function Invoke-Install { return 0 }
        $global:LASTEXITCODE = 73
        $result = Invoke-Setup
        Check 'a successful dispatch clears a stale native exit code' ($result -eq 0 -and $LASTEXITCODE -eq 0)
    }
} finally {
    # (the protected folder is this account's: as its owner it may put the inherited ACL back, then everything goes)
    if (Test-Path -LiteralPath $link) { & cmd.exe /c rmdir "$link" | Out-Null }
    if (Test-Path -LiteralPath $root) { & icacls.exe $root /reset /T /C /Q | Out-Null }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host ''
if ($failed -eq 0) { Write-Host 'every check passed'; exit 0 }
Write-Host "$failed checks failed"
exit 1
