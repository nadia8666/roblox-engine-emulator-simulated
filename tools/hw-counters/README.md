# `ree-hw-counters` (Windows hardware-counter service)

Local Windows service used by `ree ... --profile hw` to read CPU performance monitoring counters. Because normal user processes cannot open a kernel PMU trace session (which requires `SeSystemProfilePrivilege`), this lightweight service runs elevated once installed and serves counter samples strictly to the account that installed it.

## How it works

- **Two-step install:** Run `powershell -NoProfile -ExecutionPolicy Bypass -File setup.ps1` from a standard (non-elevated) PowerShell. It compiles the binary and prints an exact elevated install command pinned by SHA-256 of both the executable and script. Pass `-Uninstall` to remove it.
- **Scoping & concurrency:** Counts only threads belonging to processes owned by the installing user (one connection per process). Up to 8 clients can share a single active kernel PMU session: the first client starts the session with its requested counters, additional clients may sample any subset of those counters (requesting a different set returns busy), and the session stops when the last client disconnects.
- **Shared definitions:** Wire protocol, client, and counter tables (including AMD event definitions) are shared directly from `src/profiler/hw/` (`protocol.rs`, `client.rs`, `counters.rs`). Re-run `setup.ps1` after editing counter tables to update the installed service.

For granted permissions, counter descriptions, and function attribution details, see [docs/cli.md](../../docs/cli.md).

## Commands and tests

```powershell
cargo test --release --manifest-path tools/hw-counters/Cargo.toml   # Unit & live-trace tests
ree-hw-counters probe                                               # Verify counter accuracy on known kernels
ree-hw-counters sources                                             # List counters exposed by this CPU

# Non-elevated installer verification (hash checks, ACLs, symlink rejection, PATH hardening)
powershell -NoProfile -ExecutionPolicy Bypass -File tools/hw-counters/setup-test.ps1

# Verify Luau attribution (pointer-chasing stall cycles land on the right functions)
powershell -NoProfile -ExecutionPolicy Bypass -File tools/hw-counters/attribution.ps1
```
