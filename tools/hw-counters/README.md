# ree-hw-counters

The Windows service `ree ... --profile hw` reads the CPU's hardware counters through: a process cannot read them
itself, and the kernel trace session that can needs the system's profiling right, which an account's ordinary
processes never hold. Installed once, in two steps (`powershell -NoProfile -ExecutionPolicy Bypass -File setup.ps1`
from an ordinary PowerShell builds it and prints how to open an elevated PowerShell without a profile and the command,
with the build's SHA-256 and the script's, that installs exactly that build; `-Uninstall` removes it), it counts
for the one account it was installed for, only that account's processes' own threads, one connection a process. Up to eight clients count at
once, in one kernel trace session: only one session at a time may hold the CPU's counters, so the first client starts
it with its counters, later ones count those or some of them (one asking for others is told the service is busy), and
the last one out stops it.

What it grants, the counters, how the counts reach functions and how to check them: docs/cli.md "Hardware counters".
Its wire format, client and counter table are ree's own (src/profiler/hw: protocol.rs, client.rs, counters.rs, the
AMD events it registers), shared by path: a change to the counter table takes effect in the installed service when
setup.ps1 runs again.

```text
cargo test --release --manifest-path tools/hw-counters/Cargo.toml    # its tests (one reads a real trace session)
ree-hw-counters probe     # every counter against kernels whose counts are known, the undercount of the fills, and the
                          # readings around calls, through the installed service
ree-hw-counters sources   # the counters this CPU offers
powershell -NoProfile -ExecutionPolicy Bypass -File tools/hw-counters/setup-test.ps1
                          # setup.ps1's install steps that need no elevation (the hash check, the ACLs, a link refused,
                          # PATH scripts named like its cmdlets kept out, the guarded commands it prints)
powershell -NoProfile -ExecutionPolicy Bypass -File tools/hw-counters/attribution.ps1
                          # attribution in Luau, from an ordinary (not elevated) shell: runs attribution.luau through
                          # this checkout's ree; the stall cycles must land on the functions that chase pointers
```
