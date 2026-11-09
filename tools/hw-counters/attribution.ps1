<#
.SYNOPSIS
Checks that ree's hardware-counter profile books the cycles stalled on loads to the Luau functions that paid them.

.DESCRIPTION
Run from an ordinary (not elevated) PowerShell once the counter service is installed (setup.ps1). It runs
tools/hw-counters/attribution.luau through ree (this checkout's target\release\ree.exe, or -Ree): two functions chase
pointers through 64 MB of array slots and two compute, once with calls long enough to be read every time and once with
shorter calls read one in several. The chases must hold 90 % or more of the four functions' stall cycles.

It refuses to run elevated: ree is a program the account can rebuild, which an elevated shell must never run. Exit 0
when the check passed, 1 when it failed, 2 when it could not run or count.

.PARAMETER Ree
The ree.exe to run (default: this checkout's target\release\ree.exe).
#>
param([string]$Ree)

$ErrorActionPreference = 'Stop'
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Host 'This runs ree, which an elevated shell must not run (the account can rebuild it): run it from an ordinary PowerShell.'
    exit 2
}
if (-not $Ree) { $Ree = Join-Path $PSScriptRoot '..\..\target\release\ree.exe' }
if (-not (Test-Path -LiteralPath $Ree)) {
    Write-Host "No ree at ${Ree}: build it (cargo build --release) or pass -Ree <ree.exe>."
    exit 2
}
$Ree = (Resolve-Path -LiteralPath $Ree).Path

# Runs ree with `ArgumentLine`, writes what it printed (standard output, then standard error) to `LogPath` and returns its
# exit code. The output is read here, not through PowerShell's streams: in Windows PowerShell 5.1 a native command's
# standard error becomes error records, which ErrorActionPreference Stop turns into a terminating error at ree's first
# line, and which decorate the message when it is redirected to a file
function Invoke-ReeCapturingOutput([string]$ReePath, [string]$ArgumentLine, [string]$LogPath) {
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.FileName = $ReePath
    $start.WorkingDirectory = (Get-Location).Path
    $start.Arguments = $ArgumentLine
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $start
    try {
        $null = $process.Start()
        # (both read at once: a full pipe on one would stop ree while this waits on the other)
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        [System.IO.File]::WriteAllText($LogPath, $stdout.Result + $stderr.Result)
        return $process.ExitCode
    } finally {
        $process.Dispose()
    }
}

Write-Host 'the attribution check (tools\hw-counters\attribution.luau, through ree):'
$out = Join-Path $env:TEMP ('ree-hw-attribution-' + [guid]::NewGuid().ToString('N'))
$log = "$out.log"
$code = 2
try {
    # (the script watchdog is off, as for any measurement: docs/cli.md "Hardware counters")
    $harness = Join-Path $PSScriptRoot 'attribution.luau'
    $arguments = "run --harness `"$harness`" --seconds 0.1 --script-timeout 0 --profile hw --profile-out `"$out`""
    $ran = Invoke-ReeCapturingOutput $Ree $arguments $log
    $json = Join-Path $out 'profile.json'
    if (-not (Test-Path -LiteralPath $json)) {
        $tail = if (Test-Path -LiteralPath $log) { (Get-Content -LiteralPath $log -Tail 5) -join ' | ' } else { '' }
        Write-Host "  could not run: ree exited $ran and wrote no profile ($tail)"
    } else {
        $counters = (Get-Content -Raw -LiteralPath $json | ConvertFrom-Json).counters
        if ($counters.error) {
            Write-Host "  could not count: $($counters.error)"
        } elseif (-not $counters.attribution -or -not ($counters.names -contains 'load-stall-cycles')) {
            Write-Host '  this ree or this service predates the readings around calls and the stall cycles: build ree (cargo build --release), install the service again (setup.ps1) and run this again'
        } else {
            $stalls = @{}
            foreach ($f in $counters.functions) {
                if ($f.name -in 'chaseLong', 'computeLong', 'chaseShort', 'computeShort') {
                    $stalled = [double]$f.self.'load-stall-cycles'
                    $cycles = [double]$f.self.cycles
                    $stalls[$f.name] = $stalled
                    $share = if ($cycles -gt 0) { 100 * $stalled / $cycles } else { 0 }
                    Write-Host ('  {0,-13} {1,16:N0} cycles, {2,16:N0} stalled on loads ({3:N1} %)' -f $f.name, $cycles, $stalled, $share)
                }
            }
            $chases = [double]$stalls['chaseLong'] + [double]$stalls['chaseShort']
            $computes = [double]$stalls['computeLong'] + [double]$stalls['computeShort']
            $held = if ($chases + $computes -gt 0) { $chases / ($chases + $computes) } else { 0 }
            if ($held -ge 0.9) {
                Write-Host ('  ok: the chases hold {0:N1} % of the four functions'' stall cycles' -f (100 * $held))
                $code = 0
            } else {
                Write-Host ('  FAILED: the chases hold {0:N1} % of the four functions'' stall cycles, expected 90 % or more' -f (100 * $held))
                $code = 1
            }
        }
    }
} finally {
    # (the profile's folder and the log go, whatever happened)
    $ErrorActionPreference = 'Continue'
    if (Test-Path -LiteralPath $out) { Remove-Item -LiteralPath $out -Recurse -Force -ErrorAction SilentlyContinue }
    if (Test-Path -LiteralPath $log) { Remove-Item -LiteralPath $log -Force -ErrorAction SilentlyContinue }
}
exit $code
