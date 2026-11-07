# Contributing to roblox-engine-emulator

Thanks for helping out! This guide covers how changes get in, what we need from you, and what we can't accept.

## How changes land here

This repository is published from the maintainers' internal repository by an automated sync, so `main` only moves through sync commits.

Pull requests are still the way in, and they're reviewed here like anywhere else. Once a maintainer accepts yours, they apply it internally. It then comes back in the next sync commit with you as its author, and the PR is closed with a link to that commit. So a closed PR with a "Landed in ..." comment means it was merged.

## Before you start

- **Small fixes** (typos, an obvious bug with a test): open a PR straight away.
- **Anything bigger** (new features, new flags, behavior changes, refactors): open an issue first and wait for a maintainer to agree on the approach. PRs that skip this step may be closed so nobody's time is wasted.
- **Differences from Roblox Studio**: use the "Looks different from Studio" issue form, and include Studio's output next to ours. Parity claims need evidence; see [Measuring against Studio](#measuring-against-studio).
- **Security problems**: never in a public issue. See [SECURITY.md](SECURITY.md).

Search existing issues before filing. A thumbs-up on an existing issue helps more than a duplicate.

## Development setup

1. Install Rust 1.90 or later (`rustup`), a C++ toolchain for the Luau VM (on Windows, the Visual Studio Build
   Tools with the C++ workload) and Git. Node.js 20 or later is needed only to re-vendor the VM.
2. Clone, build and test:

   ```sh
   git clone https://github.com/Anime-Vanguards/roblox-engine-emulator
   cd roblox-engine-emulator
   cargo build --release
   cargo test --release
   ```

   The first build compiles Luau's C++ and takes a few minutes; later `--release` builds are incremental.
3. The tests need about 2 GB of free memory (the `ree serve` tests check for it) and run the built `ree`
   (tests/process_guards.rs). None needs Roblox Studio, a game checkout, a GPU or the network.
4. If you change a file in `patches/`, run `node tools/vendor.mjs` and commit `vendor/` with it;
   `node tools/vendor.mjs --check` must pass. Never edit `vendor/` by hand. The Luau VM's patches are a
   topic series (`patches/luau/`, one topic a file; docs/building.md "Updating Luau").

[docs/engine.md](docs/engine.md) describes how the engine works inside; read the section for the part you change.

## Making a change

- Keep a PR to one change. Unrelated fixes go in their own PRs.
- Add or update tests for every change in behavior. Run the whole test command above before you push, and say in the PR what you ran.
- Follow the style of the surrounding code (its indentation and naming). Rust here uses 4 spaces and long lines and is not rustfmt-formatted, so don't run `cargo fmt` on the tree; Luau, JavaScript and JSON use tabs. A comment that states a Roblox rule cites the Studio measurement or the documentation page behind it.
- Update the docs (README, `docs/`) when flags, outputs or behavior change.
- Write commit messages and PR titles as one plain sentence saying what changed ("Particles with ZOffset move by view depth").
- Don't commit generated output, build folders, renders or logs.

### Measuring against Studio

These tools aim to match Roblox Studio exactly, so a change that claims parity needs:

- the Studio version and graphics settings you compared against;
- a minimal scene or script that shows the difference, built from scratch (parts, a script, a small .rbxm you made yourself);
- the numbers or images before and after, side by side with Studio's.

Never copy code, shaders, tables or other data out of Roblox's binaries or installed files. Fit values from measurements instead.

## Content and code we can't accept

- Models, places, images, meshes, audio, fonts or text you don't own or have the right to share. That includes assets from Roblox games (ours included) and Roblox's own fonts and textures. Tests and fixtures build their own scenes.
- Code copied from Roblox, decompiled or reverse-engineered from Roblox's binaries, or taken from projects whose license isn't compatible with GPL-2.0-or-later.
- Network calls to anything other than the Roblox endpoints the tools already use, telemetry, or anything that sends user data elsewhere.
- Credentials of any kind in code, tests, logs or screenshots (never paste a `.ROBLOSECURITY` cookie anywhere).

## AI-assisted contributions

You're welcome to use AI tools. The maintainers use coding agents heavily too. The rules:

- **You're responsible for every line.** Read and understand the change before you open the PR, and be ready to explain it. Don't submit code you couldn't defend in review.
- **Say so in the PR.** The template asks which tool you used and how.
- **No unattended bulk PRs.** Agent-generated drive-by PRs across many files, or PRs opened in batches with no issue behind them, will be closed without review.
- **Agents follow [AGENTS.md](AGENTS.md).** Point your agent at it.
- **Tests are required.** Generated code with no test, or tests that don't fail without the fix, won't be merged.

## Review

A maintainer aims to reply within a week. If two weeks pass without a word, a polite ping on the PR is welcome. We may push small fixups on accepted PRs ourselves while landing them.

## License

roblox-engine-emulator is licensed under the GNU General Public License, version 2 or (at your option) any later version ([LICENSE](LICENSE)). By submitting a contribution you agree that it's licensed under the same terms, and you confirm that you have the right to submit it.

## Code of conduct

Everyone taking part follows our [Code of Conduct](CODE_OF_CONDUCT.md).
