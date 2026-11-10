<#
.SYNOPSIS
Installs (or removes) ree's counter service: the one elevated step after which `ree ... --profile hw` reads the CPU's
hardware counters from an ordinary process.

.DESCRIPTION
Two steps, so that no program the account can change runs elevated. The script itself comes from the checkout,
which the account can change: run it from a checkout you have read. Step 1 prints the script's SHA-256 too, and the
command it prints for step 2 reads the script once, checks those bytes' hash and runs those bytes (a script block made
from them, told the script's path), so the script cannot change between the two steps, nor between the check and the
run. The uninstall command it prints at the end is guarded the same way.

 1. From an ordinary PowerShell: `powershell -NoProfile -ExecutionPolicy Bypass -File <checkout>\tools\hw-counters\setup.ps1`.
    It builds the service (a Rust program with no dependencies; cargo must be on PATH) in
    "%LOCALAPPDATA%\ree-hw-counters-build", and prints the build's SHA-256, the script's, how to open an elevated
    PowerShell that runs no profile, and the command to run there. It warns when PSReadLine sits in the account's own
    module folder: an interactive PowerShell loads it from there, -NoProfile too.
 2. In that elevated PowerShell, the printed command (`setup.ps1 -Binary <exe> -Sha256 <hash>`, behind the script's
    check). It builds nothing and runs no program from the account's folders: only Windows' own (sc.exe by its full
    path, PowerShell's own modules, imported from PowerShell's folder before anything else) and the installed copy. It
    gives the Program Files folder "ree-hw-counters" an ACL of its own (Administrators and SYSTEM full control, Users
    read and run, nothing inherited, owned by Administrators), reads the build into memory and checks its SHA-256
    against the one step 1 printed (a build changed in between is refused, and nothing is installed), writes those
    bytes there as a file Administrators own, checks the file, removes everything else in the folder, and only then
    registers it as the Windows service "ree-hw-counters". What that grants:

  - The service runs as LocalSystem holding two privileges only: the system's profiling right
    (SeSystemProfilePrivilege, what a kernel trace session with the CPU's counters needs) and the right to traverse
    folders (SeChangeNotifyPrivilege, which every process has). The one exception: when the probe fails for want of a
    right and counts only with LocalSystem's own privileges, the service keeps those, and the script says so (when it
    fails either way, the two come back).
  - It starts on demand. The account given (default: the one running this script) may start it and read its status;
    nothing else (not stop, reconfigure or delete it).
  - It serves one named pipe that only that account and SYSTEM may open, from this machine only, one connection a
    process. The client process must run as that account; it gets counter slices for its own threads only, never
    another process's, and at most two million slices.
  - While a client counts (30 minutes at most), it runs one kernel trace session of context switches carrying the
    counters' values. It stops itself after a minute with no client.
  - On AMD Family 1Ah (Zen 5) CPUs it registers AMD's events with Windows (through wpr.exe, until the next restart,
    each once): the cycles stalled waiting for load data, the L1 data cache's misses outstanding and allocated, the
    demand fills by source and the load-store dispatch.

Nothing else on the machine changes: no driver, no group membership, no policy. Then it runs the installed copy's
probe, which checks every counter against kernels whose counts are known. The check that ree books the counts to the
Luau functions that paid them runs ree, which this elevated script does not run: it prints the command for an ordinary
shell (attribution.ps1).

.PARAMETER User
The account that may use the service (a name or a SID). Default: the account running this script.

.PARAMETER Binary
The ree-hw-counters.exe step 1 built (it prints the path).

.PARAMETER Sha256
The SHA-256 step 1 printed for that build: the copy installed must have it.

.PARAMETER Uninstall
Stops and removes the service and its folder.

.PARAMETER ScriptPath
Given by the commands the script prints, which run it from the bytes they hashed (a script block has no file): the
path they read it from. Ignored when the script runs as a file.
#>
param([string]$User, [string]$Binary, [string]$Sha256, [switch]$Uninstall, [string]$ScriptPath)

$ErrorActionPreference = 'Stop'
# PowerShell's own modules from its own folder, before any of their commands runs: a fresh Windows PowerShell has none
# loaded, and would take a script of the same name in a folder on PATH (one the account can write) over the cmdlet
foreach ($module in 'Microsoft.PowerShell.Management', 'Microsoft.PowerShell.Utility', 'Microsoft.PowerShell.Security') {
    Import-Module ([System.IO.Path]::Combine($PSHOME, 'Modules', $module, "$module.psd1"))
}
$name = 'ree-hw-counters'
# Windows' own service tool, by its full path; the Program Files folder from Windows, not from an environment variable
$sc = [System.IO.Path]::Combine([Environment]::SystemDirectory, 'sc.exe')
$programFiles = [Environment]::GetFolderPath('ProgramFiles')
# Administrators, SYSTEM, Users
$admins = 'S-1-5-32-544'
$system = 'S-1-5-18'
$users = 'S-1-5-32-545'
# how to open an elevated PowerShell that runs no profile (right-click, Run as administrator, runs the account's
# PowerShell profile, a script the account can change, as administrator)
$adminShell = "Start-Process powershell.exe -Verb RunAs -ArgumentList '-NoProfile -ExecutionPolicy Bypass'"
# this script: its file, or, run by a printed command from the bytes it hashed, the path that command read; and the
# SHA-256 of the text that runs (the same bytes: the script is ASCII), for the commands it prints
$scriptPath = if ($PSCommandPath) { $PSCommandPath } else { $ScriptPath }
$scriptRoot = if ($scriptPath) { [System.IO.Path]::GetDirectoryName($scriptPath) } else { '' }
$selfSha = [System.Security.Cryptography.SHA256]::Create()
$selfHash = ([BitConverter]::ToString($selfSha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($MyInvocation.MyCommand.ScriptBlock.ToString())))).Replace('-', '')
$selfSha.Dispose()

# A string as a single-quoted PowerShell literal ('' for each ')
function Format-Quoted([string]$Text) {
    return "'" + $Text.Replace("'", "''") + "'"
}

# A command that runs this script elevated only as the bytes whose SHA-256 is $Hash: it reads the file once, hashes
# those bytes and runs them (so nothing written to the file after the hash runs), passing the script its path
# ($LASTEXITCODE is 1 from the start and stays 1 when the hash differs or the file cannot be read: the command is run
# from a script block, where an exit would close the shell, so the code is left for the shell's user to read)
function Format-Guarded([string]$Hash, [string]$Arguments) {
    $path = Format-Quoted $scriptPath
    $readFile = "`$reeSetup = [IO.File]::ReadAllBytes($path)"
    $hashOfFile = "[BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash(`$reeSetup)).Replace('-','')"
    $runChecked = "& ([ScriptBlock]::Create([Text.Encoding]::UTF8.GetString(`$reeSetup))) -ScriptPath $path $Arguments"
    $refuseChanged = "'setup.ps1 is not the one that printed this: run step 1 again'; `$global:LASTEXITCODE = 1"
    $reportUnreadable = "'could not run setup.ps1: ' + `$_.Exception.Message; `$global:LASTEXITCODE = 1"
    return "`$global:LASTEXITCODE = 1; try { $readFile; if ($hashOfFile -eq '$Hash') { $runChecked } else { $refuseChanged } } catch { $reportUnreadable }"
}

function Test-Elevated {
    $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# A file's SHA-256 (or of bytes in memory), by .NET's own class
function Get-Sha256([byte[]]$Bytes) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($sha.ComputeHash($Bytes))).Replace('-', '') } finally { $sha.Dispose() }
}

# The rules the folder and the copy get: Administrators and SYSTEM full control, Users read and run
function Add-ProtectedRules($Security, [bool]$Inherit) {
    $inheritance = if ($Inherit) { [System.Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit' } else { [System.Security.AccessControl.InheritanceFlags]::None }
    $none = [System.Security.AccessControl.PropagationFlags]::None
    $Security.SetAccessRuleProtection($true, $false)
    foreach ($grant in @(@($admins, 'FullControl'), @($system, 'FullControl'), @($users, 'ReadAndExecute'))) {
        $sid = New-Object System.Security.Principal.SecurityIdentifier($grant[0])
        $Security.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($sid, $grant[1], $inheritance, $none, 'Allow')))
    }
}

# Checks a file's or folder's ACL is the protected one (and its owner Administrators, unless -KeepOwner)
function Test-Protected([string]$Path, [switch]$KeepOwner) {
    $set = Get-Acl -LiteralPath $Path
    $rules = @($set.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
    $expected = @{ $admins = 'FullControl'; $system = 'FullControl'; $users = 'ReadAndExecute, Synchronize' }
    if (-not $set.AreAccessRulesProtected -or $rules.Count -ne 3) { throw "the ACL of $Path did not take: $($set.Sddl)" }
    foreach ($rule in $rules) {
        $want = $expected[$rule.IdentityReference.Value]
        if ($rule.IsInherited -or $rule.AccessControlType -ne 'Allow' -or -not $want -or $rule.FileSystemRights.ToString() -ne $want) {
            throw "the ACL of $Path holds an unexpected rule: $($rule.IdentityReference.Value) $($rule.FileSystemRights)"
        }
    }
    $owner = $set.GetOwner([System.Security.Principal.SecurityIdentifier]).Value
    if (-not $KeepOwner -and $owner -ne $admins) { throw "$Path is owned by $owner, not Administrators" }
}

# The install folder's ACL, its own, for the folder and what it holds, nothing inherited, owned by Administrators (unless
# -KeepOwner); checked once set. A folder that is a link elsewhere is refused.
function Set-ProtectedFolder([string]$Path, [switch]$KeepOwner) {
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) { throw "$Path is a link elsewhere, not a folder: remove it and run this again" }
    $acl = New-Object System.Security.AccessControl.DirectorySecurity
    Add-ProtectedRules $acl $true
    if (-not $KeepOwner) { $acl.SetOwner((New-Object System.Security.Principal.SecurityIdentifier($admins))) }
    # (the folder's own SetAccessControl writes only what was set, the rules and the owner: Set-Acl would write the
    # audit list too, which takes a privilege this does not need)
    $item.SetAccessControl($acl)
    Test-Protected $Path -KeepOwner:$KeepOwner
}

# Reads the build into memory and checks its SHA-256 there, then writes those bytes into $Folder under a staging name, as
# a file created owned by Administrators (unless -KeepOwner) with the protected ACL and opened by nobody else meanwhile,
# and checks the file: the staged copy's path, or an error before anything is written when the build is not the one step
# 1 hashed
function Copy-Verified([string]$Source, [string]$Folder, [string]$FileName, [string]$Sha256, [switch]$KeepOwner) {
    if ($Sha256 -notmatch '^[0-9A-Fa-f]{64}$') { throw "-Sha256 must be the 64 hex digits step 1 printed, not '$Sha256'" }
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) { throw "no program at $Source" }
    $want = $Sha256.ToUpperInvariant()
    $bytes = [System.IO.File]::ReadAllBytes($Source)
    $read = Get-Sha256 $bytes
    if ($read -ne $want) {
        throw "the build's SHA-256 is $read, not the $want step 1 printed: the program changed after it was built. Nothing was installed; build it again (step 1) and install that build"
    }
    $staged = [System.IO.Path]::Combine($Folder, "$FileName.new")
    if (Test-Path -LiteralPath $staged) { Remove-Item -LiteralPath $staged -Force }
    $security = New-Object System.Security.AccessControl.FileSecurity
    Add-ProtectedRules $security $false
    if (-not $KeepOwner) { $security.SetOwner((New-Object System.Security.Principal.SecurityIdentifier($admins))) }
    $stream = New-Object System.IO.FileStream($staged, [System.IO.FileMode]::CreateNew, [System.Security.AccessControl.FileSystemRights]::FullControl, [System.IO.FileShare]::None, 65536, [System.IO.FileOptions]::None, $security)
    try { $stream.Write($bytes, 0, $bytes.Length) } finally { $stream.Dispose() }
    Test-Protected $staged -KeepOwner:$KeepOwner
    $written = Get-Sha256 ([System.IO.File]::ReadAllBytes($staged))
    if ($written -ne $want) {
        Remove-Item -LiteralPath $staged -Force
        throw "the copy written reads $written, not ${want}: nothing was installed"
    }
    return $staged
}

# Stops and deletes the service; an error when the service manager keeps it (a tool holding it open marks it for
# deletion only)
function Remove-CounterService {
    $existing = Get-Service -Name $name -ErrorAction SilentlyContinue
    if (-not $existing) { return }
    if ($existing.Status -ne 'Stopped') {
        Stop-Service -Name $name -Force -ErrorAction SilentlyContinue
        try {
            $existing.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
        } catch {
            throw 'the old service did not stop within 30 s: it may still be serving a client (a ree run with --profile hw), or another program may have its exe open. End those and run this again; the old service was not deleted or replaced'
        }
    }
    $deleted = (& $sc delete $name) -join ' '
    $code = $LASTEXITCODE
    # (1072: already marked for deletion, which ends once no handle to it is open)
    if ($code -ne 0 -and $code -ne 1072) { throw "deleting the old service failed ($code): $deleted" }
    for ($i = 0; $i -lt 100 -and (Get-Service -Name $name -ErrorAction SilentlyContinue); $i++) { Start-Sleep -Milliseconds 100 }
    if (Get-Service -Name $name -ErrorAction SilentlyContinue) {
        throw 'the old service is marked for deletion, but something still holds it open: the Services console, Event Viewer or Task Manager''s Services tab, or a client starting it again (a ree run with --profile hw). Close those, end the run, and run this again'
    }
}

# PSReadLine in the account's own module folder under $Documents, which an interactive PowerShell loads from there
# first, -NoProfile too: its path, or nothing
function Find-UserReadLine([string]$Documents) {
    $path = [System.IO.Path]::Combine($Documents, 'WindowsPowerShell', 'Modules', 'PSReadLine')
    if (Test-Path -LiteralPath $path) { return $path }
}

# Step 1, in an ordinary shell: the build, its hash and the script's, and how to run step 2
function Invoke-Build {
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargo) { throw 'cargo is not on PATH: install Rust (rustup), then run this again' }
    $target = [System.IO.Path]::Combine($env:LOCALAPPDATA, "$name-build")
    Write-Host "Building $name in $target ..."
    & $cargo.Source build --release --locked --manifest-path ([System.IO.Path]::Combine($scriptRoot, 'Cargo.toml')) --target-dir $target | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "building $name failed" }
    $built = [System.IO.Path]::Combine($target, 'release', "$name.exe")
    $hash = Get-Sha256 ([System.IO.File]::ReadAllBytes($built))
    $scriptHash = Get-Sha256 ([System.IO.File]::ReadAllBytes($scriptPath))
    $who = if ($User) { " -User $(Format-Quoted $User)" } else { '' }
    Write-Host ''
    Write-Host "Built   $built"
    Write-Host "SHA-256 $hash"
    Write-Host "Script  $scriptPath"
    Write-Host "SHA-256 $scriptHash"
    $readLine = Find-UserReadLine ([Environment]::GetFolderPath('MyDocuments'))
    if ($readLine) {
        Write-Host ''
        Write-Host "WARNING: PSReadLine is in your own module folder ($readLine). An interactive PowerShell loads it from there,"
        Write-Host '-NoProfile too, so the elevated shell would run a module the account can change. Move that folder out of your'
        Write-Host 'module folder first (Windows PowerShell has its own PSReadLine, in Program Files).'
    }
    Write-Host ''
    Write-Host 'Now open an elevated PowerShell that runs no profile (not right-click, Run as administrator, which runs your'
    Write-Host 'PowerShell profile, a script the account can change, as administrator):'
    Write-Host "  $adminShell"
    Write-Host 'and in it run this: it reads the script once, checks the bytes are the ones hashed above, and runs those bytes:'
    Write-Host ('  ' + (Format-Guarded $scriptHash ('-Binary {0} -Sha256 {1}{2}' -f (Format-Quoted $built), $hash, $who)))
    return 0
}

# Step 2, elevated: the folder protected, the build checked and written, then the service registered and probed
function Invoke-Install {
    $folder = [System.IO.Path]::Combine($programFiles, $name)
    $exe = [System.IO.Path]::Combine($folder, "$name.exe")
    $key = "HKLM:\SYSTEM\CurrentControlSet\Services\$name"
    if ($Uninstall) {
        Remove-CounterService
        if (Test-Path -LiteralPath $folder) { Remove-Item -LiteralPath $folder -Recurse -Force }
        Write-Host "Removed the $name service and $folder. (Counters it registered with Windows go at the next restart.)"
        return 0
    }
    if (-not $Binary -or -not $Sha256) {
        Write-Host 'An elevated shell builds nothing here: cargo, rustc and the linker are the account''s to change, and so is a'
        Write-Host 'build until it is copied. First, from an ordinary (not elevated) PowerShell:'
        Write-Host "  powershell -NoProfile -ExecutionPolicy Bypass -File $(Format-Quoted $scriptPath)"
        Write-Host 'It builds the service and prints the command that installs that build here.'
        return 1
    }
    # the account that may use it
    if ($User) {
        if ($User -match '^S-1-') { $sid = $User }
        else { $sid = (New-Object Security.Principal.NTAccount($User)).Translate([Security.Principal.SecurityIdentifier]).Value }
    } else {
        $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    }
    $account = (New-Object Security.Principal.SecurityIdentifier($sid)).Translate([Security.Principal.NTAccount]).Value

    # the folder's own ACL first, then the build checked and written there: only a build that matches step 1's hash goes
    # further; then the old service goes, and everything else in the folder with it (an older program, its counters'
    # profile: the service writes that again), so nothing but the checked copy is there for the service to load
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
    Set-ProtectedFolder $folder
    $staged = Copy-Verified $Binary $folder "$name.exe" $Sha256
    Remove-CounterService
    foreach ($item in @(Get-ChildItem -LiteralPath $folder -Force)) {
        if ($item.FullName -ne $staged) { Remove-Item -LiteralPath $item.FullName -Recurse -Force }
    }
    Move-Item -LiteralPath $staged -Destination $exe -Force
    $left = @(Get-ChildItem -LiteralPath $folder -Force | ForEach-Object { $_.Name })
    if ($left.Count -ne 1 -or $left[0] -ne "$name.exe") { throw "$folder holds more than the service: $($left -join ', ')" }
    Test-Protected $exe
    if ((Get-Sha256 ([System.IO.File]::ReadAllBytes($exe))) -ne $Sha256.ToUpperInvariant()) { throw "$exe is not the build step 1 hashed" }
    Write-Host "Installed the build as $exe (SHA-256 $($Sha256.ToUpperInvariant()), as step 1 printed; owned by Administrators)."

    New-Service -Name $name -BinaryPathName ('"' + $exe + '" service --allow ' + $sid) -DisplayName 'ree hardware counters' -StartupType Manual `
        -Description "Counts CPU hardware events (instructions, cycles, cycles stalled on loads, cache misses outstanding, branch mispredictions) for ree's --profile hw, for processes of $account and only for their own threads. Starts on demand, stops itself when idle." | Out-Null
    # LocalSystem with only the profiling right (and folder traversal)
    $limited = 'SeSystemProfilePrivilege/SeChangeNotifyPrivilege'
    & $sc privs $name $limited | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'limiting the service to the profiling right failed' }
    # the account may start it and read its status (RP: start, LC: query status), beside the service's default access
    $sd = ((& $sc sdshow $name) -join '').Trim()
    if ($sd -notmatch '^D:') { throw "reading the service's security descriptor: $sd" }
    $sd = $sd -replace '^D:([A-Z]*)', ('D:$1(A;;RPLC;;;' + $sid + ')')
    & $sc sdset $name $sd | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "setting the service's security descriptor failed ($sd)" }
    Write-Host "Registered the $name service for $account ($sid)."

    # the check: the installed copy's probe counts known kernels through the service, as the account's processes will
    $probe = Invoke-Probe $exe
    if ($probe.Code -eq 2 -and $probe.Text -match 'error 5\b|1314|privilege|access denied|profiling right|0xc558063a') {
        # (a right the trace session needs beyond the profiling right: LocalSystem's own set, and the check again; the
        # limit comes back unless the service counted with them, however the retry ends, Ctrl+C included)
        $widened = $false
        $counted = $false
        try {
            Write-Host 'The service lacked a right it needed: giving it LocalSystem''s own privileges and checking again.'
            Stop-Service -Name $name -Force -ErrorAction SilentlyContinue
            # (set before the change: a Ctrl+C right after it still finds the flag, and the limit comes back)
            $widened = $true
            Remove-ItemProperty -Path $key -Name RequiredPrivileges -ErrorAction SilentlyContinue
            $probe = Invoke-Probe $exe
            $counted = $probe.Code -ne 2
        } finally {
            if ($widened -and -not $counted) {
                Stop-Service -Name $name -Force -ErrorAction SilentlyContinue
                & $sc privs $name $limited | Out-Null
                if ($LASTEXITCODE -eq 0) {
                    Write-Host 'It could not count with LocalSystem''s own privileges either: the service holds the profiling right alone again.'
                } else {
                    Write-Host "WARNING: putting the service back to the profiling right alone failed ($LASTEXITCODE): it holds LocalSystem's own privileges. Run, elevated:"
                    Write-Host "  & $(Format-Quoted $sc) privs $name $limited"
                }
            }
        }
        if ($counted) {
            Write-Host 'Note: the service holds LocalSystem''s own privileges (it counted only with them); run this again after a Windows update to try the profiling right alone.'
        }
    }
    $code = $probe.Code
    Write-Host ''
    if ($code -eq 0) {
        Write-Host "Done. Processes of $account now read the CPU's counters through the service:"
        Write-Host '  ree run|record ... --profile hw [--profile-counters a,b]   (docs/cli.md "Hardware counters")'
    } elseif ($code -eq 1) {
        Write-Host 'Installed, but some counters did not count what their names say (above): the profile reports them as they are.'
    } else {
        Write-Host 'Installed, but the service could not count (above). The service stays installed; remove it with:'
    }
    Write-Host 'To remove it later, in an elevated PowerShell that runs no profile (the same check as step 2''s):'
    Write-Host ('  ' + (Format-Guarded $selfHash '-Uninstall'))
    if ($code -ne 2) {
        # (the attribution check runs ree, which the account can rebuild: never from this elevated shell)
        Write-Host ''
        Write-Host 'Then, from an ordinary (not elevated) PowerShell, check that the counts land on the Luau functions that paid them:'
        Write-Host ('  powershell -NoProfile -ExecutionPolicy Bypass -File ' + (Format-Quoted ([System.IO.Path]::Combine($scriptRoot, 'attribution.ps1'))))
    }
    return $code
}

# The installed copy's probe, its lines shown as they come
function Invoke-Probe([string]$Exe) {
    Write-Host ''
    $lines = @(& $Exe probe | ForEach-Object { Write-Host $_; $_ })
    $exitCode = $LASTEXITCODE
    return @{ Code = $exitCode; Text = ($lines -join "`n") }
}

# Says that installing and removing need an elevated PowerShell, and how to open one
function Show-ElevationNeeded {
    Write-Host 'Installing and removing the service take an elevated PowerShell that runs no profile: a profile is a script'
    Write-Host 'the account can change, which an elevated shell would run as administrator. Open one with:'
    Write-Host "  $adminShell"
    Write-Host 'Step 1 (this script, from an ordinary PowerShell, without -Binary) prints the command to run there.'
}

# Runs the step this invocation asks for and returns its exit code, which is also left in $LASTEXITCODE. A step that
# throws (before any native program has set LASTEXITCODE, or after an earlier one left a stale value) is code 1
function Invoke-Setup {
    $code = 1
    try {
        if (Test-Elevated) {
            $code = Invoke-Install
        } elseif ($Uninstall -or $Binary -or $Sha256) {
            Show-ElevationNeeded
            $code = 1
        } else {
            $code = Invoke-Build
        }
    } catch {
        Write-Host "Setup failed: $($_.Exception.Message)"
        $code = 1
    }
    $global:LASTEXITCODE = $code
    return $code
}

# (dot-sourced, the script only defines its functions: tools/hw-counters/setup-test.ps1 checks them unelevated)
if ($MyInvocation.InvocationName -ne '.') {
    $code = Invoke-Setup
    # (as a file, the script exits with the code; run by a printed command, from a script block, an exit would close
    # the shell and the output with it, so the code is left in $LASTEXITCODE)
    if ($PSCommandPath) { exit $code }
    return
}
