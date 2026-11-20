# Release scripts

Scripts that build ree's release archive: Node.js 20 or later, no npm packages, run from the repository root. The
release workflow (`.github/workflows/release.yml`) runs the same commands on Windows, Linux and macOS. It builds clean
with Rust 1.90.0, the lockfile and the default features.

Windows sets two environment variables for the build:

- `RUSTFLAGS="-C target-feature=+crt-static"` puts the C/C++ runtime into the one executable.
- `CXXFLAGS_x86_64_pc_windows_msvc="-DMI_WIN_INIT_USE_FLS=1"` makes mimalloc v2 initialize through its existing
  CRT/FLS path after the static runtime has started, instead of from an earlier TLS callback. The pinned crate compiles
  a C++ wrapper on MSVC, so this is a CXXFLAGS setting. The `-D` spelling matters: Git Bash would turn a `/D` value
  into a path.

The Windows build checks `--version` before packaging, as well as in the installed smoke test.

```sh
node --test tools/release/test.mjs
node tools/release/version.mjs
node tools/release/version.mjs --tag v0.1.0
node tools/release/licenses.mjs --target x86_64-pc-windows-msvc --out out/THIRD_PARTY_LICENSES.txt
node tools/release/package.mjs --binary target/release/ree.exe --version 0.1.0 --os windows --arch x86_64 --licenses out/THIRD_PARTY_LICENSES.txt --out out/release
node tools/release/smoke.mjs --archive out/release/ree-0.1.0-windows-x86_64.zip --version 0.1.0
node tools/release/notes.mjs --tag v0.1.0 --out out/release-notes.md
node tools/release/notes.mjs --canary <commit SHA> --built 2026-11-16T01:31:00Z --out out/canary-notes.md
node tools/release/nightly.mjs --repository <owner/name> --sha <main's head> --artifacts <name,...>
```

## What each script does

- `test.mjs` tests version agreement and tag rejection, which dependencies are walked, the ZIP bytes, CRCs, order and
  modes, the notes of a first and a later release and of the canary, the choice of a passing run, the nightly
  decision and which paths ship, and the workflow's publication boundaries.
- `version.mjs` prints the version. The engine and profile-view manifests and their workspace lockfile must agree on
  it. The hardware-counter service has its own version and is not shipped. With `--tag` it fails unless the tag is
  exactly `v<version>`.
- `licenses.mjs` writes `THIRD_PARTY_LICENSES.txt` for one target. It starts at the engine and visits every normal
  dependency edge of Cargo's resolved graph once; dev, build and inactive optional edges are left out. The profile
  viewer's terminal and font dependencies are therefore not included. Package and file names are sorted by code units,
  so the output does not depend on the runner's locale. The texts of the native Luau/Lua, mimalloc v2, Zstd and the UI
  Labs stand-in are added separately. A package without a license file is listed with its declared license expression,
  and each standard text it needs appears once in an appendix, taken from another local package without network access.
  Run as it stands, it is also the repository's self-check: it fails for missing license text. It needs the
  dependencies downloaded but no build. `--metadata <file>` reads an already resolved metadata JSON instead, for when a
  build wrapper owns the cargo calls; it must have been made for that `--target`, with
  `cargo metadata --locked --format-version 1 --filter-platform <target>` and the default features.
- `package.mjs` writes `SOURCE.md` and the zip. `SOURCE.md` names and links the exact commit (`GITHUB_SHA`, else
  HEAD; `--commit <SHA>` overrides it for a self-check) and its source archive, names the version as text (a canary is
  a build of a later commit than its version's tag), and goes beside the zip and inside it. Before writing, the script
  checks that the layout is flat, that the binary is the first local and central entry, and every entry's CRC and Unix
  mode. The documents have mode 0644, only ree has mode 0755.
- `smoke.mjs` extracts the archive into a fresh folder under the system temp folder, redirects the user cache and temp
  variables into it, and runs the version, an offline harness, a recording and the service. The harness reads a
  generated PNG relative to the current project, draws into it and records a snapshot in the redirected cache. It
  needs no GPU, Studio, credentials or network. File hashes show that only the expected cache, output and temp folders
  changed. On Linux and macOS it also checks that the worker lookup skips a file without execute permission on PATH,
  in direct and in served runs. Its Unix mock workers launch the installed Node by absolute path: copying Homebrew's
  executable alone loses its relative libraries, and the helper checks deliberately restrict PATH. On GitHub Actions
  it checks that the input checkout stayed unchanged. Successful checks remove the temp folder; failures keep it and
  print its path. Temporary paths are resolved through the OS, including linked folders and Windows verbatim paths
  from Rust's `canonicalize`. `cargo test --release` runs the smoke too
  (`tests/standalone.rs`), with only the built binary copied into a fresh folder (`--binary <path>`), so the tests need
  Node.js.
- `notes.mjs` writes the release notes. It needs a full-history checkout that has the tag. Commit subjects are read
  through git's argument array and written to a file, never put into a shell command. Only `vMAJOR.MINOR.PATCH` tags
  count as releases, so the previous release is never `canary`. With `--canary <commit> --built <time>` it writes the
  canary's notes: the line that it is a build of main and not a release, the commit, when its zips were built, the
  commits since the latest release with a compare link (the newest 50 listed), the fixed download links and the source.
- `dry-run.mjs` finds the passing run whose zips a tag publishes: a run of the release workflow on main at that commit,
  started by a push, by hand or by the nightly schedule, whose jobs all passed and whose zips are all still kept. It
  prints `run-id=` and `run-date=` for `$GITHUB_OUTPUT`, both empty when there is none, and the run then builds. With
  `--required` it fails instead, as the canary does.
- `nightly.mjs` decides whether the nightly run builds main's head. It takes the newest commit of the head's
  first-parent history with a run that `dry-run.mjs` would take, and builds nothing when that commit is the head or
  when no path that ships changed since it (`git diff --name-only`, a renamed file counted under both names).
  Otherwise, or with no such commit among the newest passing runs, it builds. A nightly run that builds nothing uploads
  nothing, and its summary names the run that built that commit; one that builds names the changed paths that ship.
  Any failure to decide builds, with a warning. A path ships unless it is under `.github/`, a dot-file or in a
  dot-folder, Markdown, or under `docs/`, `notes/`, `tools/` or a `tests`, `benches`, `examples`, `fixtures` or
  `goldens` folder. The paths a push to main starts a dry run for (`on.push.paths` in the workflow, such as
  `tools/release/**`, `tools/vendor.mjs` and `patches/**`) always ship, except the workflow itself. This is the same
  rule that decides when a release is due. It prints `build=` and `reason=` for `$GITHUB_OUTPUT`.

The `publish` job runs none of these scripts and has no checkout. It only checksums, attests and publishes the
archives the other jobs built.

## Canary

`canary` is a tag on main, moved by the export to the newest commit with a passing run of the release workflow. Its
run publishes the zips of that run (found by `dry-run.mjs --required`) as the prerelease `canary`: the `canary` job,
like `publish`, runs no repository code and has no checkout. It renames `ree-<version>-<os>-<arch>.zip` to
`ree-canary-<os>-<arch>.zip`, writes `SHA256SUMS`, and creates the release of the existing tag when there is none (a
prerelease, never latest, no tag created), else uploads the new assets over the old ones, deletes the assets no longer
published, and then sets the title `Canary (main <first 10 characters of the commit>)` and the notes. A run whose tag
has moved on since it started leaves the release to the newer run. Canary runs share a concurrency group, so a newer
one only replaces a run still waiting.
