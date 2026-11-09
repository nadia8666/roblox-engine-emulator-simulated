# Documentation

- [Building](building.md): release binaries (platforms, where they keep state, verifying them), requirements, the
  build, cargo features, the allocator.
- [Command line](cli.md): `ree record`, `ree run` and `ree serve`; harnesses and their `Context`, engine options,
  limits and guards, what a recording holds, animation, what is not simulated, the warm service, profiles and caches,
  environment variables.
- [Rust API](api.md): `Engine`, mounting, running Luau, the clock, the 3D world, recording, layers and extensions,
  bundles, models; using the crate from another crate.
- [Engine contract](engine.md): how the engine works inside, the rules it follows and the formats it reads and writes,
  with its known limitations.
- [Verification](verification.md): the tests and the downstream checks.
- [Source layout](layout.md): where things are in the source tree.
