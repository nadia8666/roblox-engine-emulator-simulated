# Contributing to roblox-engine-emulator

Thanks for contributing. This guide covers how pull requests are merged, how to set up a local build, and what we can accept.

## How pull requests are merged

This public repository is published from an internal repository via an automated sync tool, so `main` only advances through sync commits.

Open pull requests here as usual. Once approved, a maintainer imports your diff internally; the next sync pushes it back to public `main` with you preserved as the author and closes your PR with a `"Landed in ..."` link.

## Before you open a PR

- **Small fixes** (typos, clear bugs with a test): open a PR directly.
- **Larger changes** (new features, CLI flags, behavior changes, refactors): open an issue first to agree on the approach. Unsolicited large PRs may be closed to avoid wasted work.
- **Differences from Roblox Studio:** use the "Looks different from Studio" issue template with side-by-side Studio output ([Measuring against Studio](#measuring-against-studio)).
- **Security problems:** never report publicly—follow [SECURITY.md](SECURITY.md).

Search open issues before filing a duplicate.

## Development setup

1. Install **Rust 1.90+** (`rustup`), a **C++ toolchain** for the Luau VM (Visual Studio Build Tools with C++ workload on Windows), and **Git**. **Node.js 20+** is only needed when re-vendoring the VM or running parity/release scripts.
2. Clone, build, and test:
   ```sh
   git clone https://github.com/Anime-Vanguards/roblox-engine-emulator
   cd roblox-engine-emulator
   cargo build --release
   cargo test --release
   ```
   The first build compiles Luau's C++ sources and takes a few minutes; subsequent `--release` builds are incremental.
3. `cargo test --release` requires ~2 GB of free RAM (for `ree serve` tests) and runs `tests/process_guards.rs` against the compiled `ree` binary. No test requires Roblox Studio, a GPU, or network access.
4. **Editing VM/datatype patches:** Never edit `vendor/` directly. Edit the topic patches under `patches/luau/` (or `patches/lune.patch`, `patches/mlua-sys.patch`), rebuild with `node tools/vendor.mjs`, verify with `node tools/vendor.mjs --check`, and commit both `patches/` and `vendor/`. See [docs/building.md](docs/building.md#updating-luau).

Read the relevant section of [docs/engine.md](docs/engine.md) before changing engine behavior, and update it in the same commit.

## Pull request guidelines

- **One focused change per PR.** Split unrelated changes into separate PRs.
- **Add tests for behavior changes.** Run `cargo test --release` before pushing and note what you ran in the PR.
- **Match existing style.** Rust uses 4 spaces and long lines with no `rustfmt` config—do **not** run `cargo fmt` across the tree. Luau, JavaScript, and JSON use tabs. Cite the Studio measurement or Roblox doc page in comments explaining engine rules.
- **Update documentation.** Keep `README.md` and `docs/` in sync when flags, outputs, or engine rules change.
- **Plain commit and PR titles.** One clear sentence describing the change.
- **Keep the tree clean.** Never commit build outputs, renders, logs, or generated scratch files.

### Measuring against Studio

Parity PRs must include:
- The Roblox Studio version and graphics settings compared against.
- A minimal, self-contained scene or Luau script built from scratch.
- Before-and-after measurements or output compared side by side with Studio.

Never copy code, shaders, tables, or data from Roblox binaries or install directories. Derive constants from black-box measurements instead.

## What we cannot accept

- Models, places, images, meshes, audio, fonts, or text you do not own or cannot legally share (including assets from Roblox games or Roblox's built-in content). Tests must construct their own scenes.
- Code copied, decompiled, or reverse-engineered from Roblox binaries, or code under licenses incompatible with GPL-2.0-or-later.
- Telemetry or outbound network calls to hosts other than Roblox endpoints already used.
- Credentials or secrets anywhere in code, tests, logs, or screenshots (never paste `.ROBLOSECURITY`).

## AI-assisted contributions

AI coding tools are welcome—the maintainers use them heavily as well—under these rules:
- **You own every line.** Read, understand, and verify your changes before opening a PR.
- **Disclose AI use** in the PR template.
- **No unattended bulk PRs.** Drive-by batch PRs without a backing issue will be closed without review.
- **Point agents at [AGENTS.md](AGENTS.md).**
- **Tests are required.** Generated fixes without a test that fails prior to the fix will not be merged.

## Review process

Issues and PRs receive an initial automated triage/review (usually within an hour), escalating to a maintainer when needed. For bugs or feature requests filed as issues, maintainers handle fixes directly—you don't need to submit a PR.

PRs are screened for security first, then reviewed for quality and fit. Push requested edits to the same PR branch for re-review. Edits to build scripts, CI workflows, dependencies, or agent instructions always require maintainer sign-off.

## License and conduct

`roblox-engine-emulator` is licensed under GPL-2.0-or-later ([LICENSE](LICENSE)). By contributing, you confirm you have the right to submit your work under those terms. All participants follow the [Code of Conduct](CODE_OF_CONDUCT.md).
