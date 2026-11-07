# Third-party notices

roblox-engine-emulator is licensed under the GNU General Public License, version 2 or (at your option) any later version
([LICENSE](LICENSE)), GPL-2.0-or-later for short. It includes, or is built from,
the third-party components below, each under its own license.

## Vendored source code

| Component | Where | License | License text | Changes |
|---|---|---|---|---|
| [Luau](https://github.com/luau-lang/luau) 0.746 (Copyright Roblox Corporation), including code from Lua 5.1 (Copyright Lua.org, PUC-Rio) | `vendor/luau0-src/luau/` | MIT | [vendor/luau0-src/luau/LICENSE.txt](vendor/luau0-src/luau/LICENSE.txt), [lua_LICENSE.txt](vendor/luau0-src/luau/lua_LICENSE.txt) | modified by the topic series in [patches/luau/](patches/luau/) |
| [luau0-src](https://github.com/mlua-rs/luau-src-rs) 0.22.0 (Copyright Aleksandr Orlenko), the crate that builds Luau | `vendor/luau0-src/` | MIT | [vendor/luau0-src/LICENSE](vendor/luau0-src/LICENSE) | its build script and manifest, by the same series |
| [mlua-sys](https://github.com/mlua-rs/mlua) 0.13.0 (Copyright A. Orlenko, rlua), mlua's FFI layer | `vendor/mlua-sys/` | MIT | [vendor/mlua-sys/LICENSE](vendor/mlua-sys/LICENSE) (mlua's: the crate ships none) | its bytecode version range, set to the vendored Luau's by `tools/vendor.mjs` |
| [Lune](https://github.com/lune-org/lune) 0.10.4: the `lune-roblox` crate | `vendor/lune-roblox/` | MPL-2.0 | [vendor/lune-roblox/LICENSE.txt](vendor/lune-roblox/LICENSE.txt) | modified by [patches/lune.patch](patches/lune.patch) |
| [Lune](https://github.com/lune-org/lune) 0.10.4: the `lune-utils` crate | `vendor/lune-utils/` | MPL-2.0 | [vendor/lune-utils/LICENSE.txt](vendor/lune-utils/LICENSE.txt) | modified by [patches/lune.patch](patches/lune.patch) |

`vendor/` is generated: `node tools/vendor.mjs` downloads the pinned upstream archives (SHA-256 checked), applies the
patches in `patches/` (and the changes the table names) and writes the result, and `node tools/vendor.mjs --check`
proves `vendor/` is exactly that.
The Lune files are covered by the Mozilla Public License 2.0, which applies file by file: the modified files stay
under the MPL-2.0, their source is the files in `vendor/` (and the upstream archive plus `patches/lune.patch`), and
the rest of this project stays under GPL-2.0-or-later. The MPL-2.0 allows its files to be combined with
GPL code (its section 3.3).

## Rust crates

`cargo build` fetches the crates in [Cargo.lock](Cargo.lock) from crates.io and git (mlua, rbx-dom's crates, glam,
serde, zstd, mimalloc and others). They are not part of this repository; the `ree` binary links them, each under its
own license (most are MIT or Apache-2.0; tools such as `cargo about` list them). `ror-mesh` comes from
[roblox-object-renderer](https://github.com/Anime-Vanguards/roblox-object-renderer), which is GPL-2.0-or-later like
this project.

## Fonts

| Font | Where | License | License text |
|---|---|---|---|
| [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono) 2.304, Regular and Bold (Copyright 2020 The JetBrains Mono Project Authors) | `profile-view/fonts/` | SIL Open Font License 1.1 | [profile-view/fonts/OFL.txt](profile-view/fonts/OFL.txt) |

The profile viewer draws its PNG screenshots with them, unmodified and compiled into its binary.

## Distributing a binary

ree's own code is GPL-2.0-or-later. A built ree also links Rust crates that are licensed under Apache-2.0 alone
(draco-core, through ror-mesh). The Free Software Foundation considers Apache-2.0 compatible with version 3 of the
GPL but not version 2, so distribute a binary under GPL-3.0-or-later, which the "or later" allows. Include its text
(https://www.gnu.org/licenses/gpl-3.0.txt), the corresponding source of the whole work and the crates' license
notices. The source itself can be used under version 2 or later.

## Data

- `data/reflection.json` is generated in Roblox Studio by `tools/dump-reflection.luau` from Studio's ReflectionService
  (Studio 0.740): the names, types, permissions and serialization of Roblox's classes, enums and members, plus each
  creatable class's `Instance.new` defaults. It describes Roblox's public engine API, which Roblox also publishes as
  its API dump; it contains no Roblox code. The engine merges it over
  [rbx_reflection_database](https://github.com/rojo-rbx/rbx-dom) (MIT), a crates.io dependency.
- `data/text-advances.json` and `data/fredoka-sizes.json` are text metrics measured in Studio (glyph advances and
  kerning by font and size); `data/font-enums.json` maps Enum.Font to Roblox's font family names. They hold no font
  files.

## Stand-ins

`src/lua/packages/ui-labs.luau` is a stand-in for the UI Labs utility package
([ui-labs-utils](https://github.com/PepeElToro41/ui-labs-utils), published as `@rbxts/ui-labs` and
`pepeeltoro41/ui-labs`) for places whose copy of the package lives only in Studio. It is adapted from that package's
controls, story creators and Environment API, without the plugin's environment injection, and used under its MIT
license, below (the license names no holder; the package's author is PepeElToro41). Nothing in it comes from the UI Labs
plugin, which is a separate project under GPL-3.0.

> MIT License
>
> Copyright (c) 2023-present
>
> Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
> documentation files (the "Software"), to deal in the Software without restriction, including without limitation the
> rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit
> persons to whom the Software is furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or substantial portions of the
> Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE
> WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
> COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
> OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

## Trademarks

Roblox and Roblox Studio are trademarks of Roblox Corporation. Luau is a trademark of Roblox Corporation. This project
is not affiliated with, sponsored by, or endorsed by Roblox Corporation.
