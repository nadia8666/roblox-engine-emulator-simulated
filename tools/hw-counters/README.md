# ree-hw-counters

The Windows service `ree ... --profile hw` reads the CPU's hardware counters through: a process cannot read them
itself, and the kernel trace session that can needs the system's profiling right, which an account's ordinary
processes never hold. Installed once from an elevated PowerShell (`& .\setup.ps1`; `-Uninstall` removes it), it counts
for the one account it was installed for, only that account's processes' own threads. Up to eight clients count at
once, in one kernel trace session: only one session at a time may hold the CPU's counters, so the first client starts
it with its counters, later ones count those or some of them (one asking for others is told the service is busy), and
the last one out stops it.

What it grants, the counters, how the counts are booked and how to check them: docs/cli.md "Hardware counters". Its
wire format and client are ree's own (src/profiler/hw), shared by path.

```text
cargo test --release --manifest-path tools/hw-counters/Cargo.toml    # its tests (one reads a real trace session)
ree-hw-counters probe     # every counter against kernels whose counts are known, through the installed service
ree-hw-counters sources   # the counters this CPU offers
```
