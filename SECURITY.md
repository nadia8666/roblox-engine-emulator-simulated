# Security policy

## Reporting a vulnerability

Report security vulnerabilities privately via GitHub's **Security** tab by clicking **Report a vulnerability** ([direct link](https://github.com/Anime-Vanguards/roblox-engine-emulator/security/advisories/new)). Only maintainers can view these reports.

Do not open public issues, pull requests, or discussions for security problems.

Include what you found, reproduction steps, the commit hash tested, and potential impact. A maintainer will reply within a week and keep you updated until resolved. We will credit you in the advisory unless you prefer to stay anonymous.

## Supported versions

Security fixes apply only to the latest commit on `main`. There are no long-term release branches.

## Scope

**In scope:** Code, build scripts, and CI workflows in this repository, including:
- Crafted models, places, scripts, or meshes causing arbitrary code execution outside the Luau sandbox, path traversal outside output folders, or unintended file reads.
- Cross-site or remote access to `ree serve`.
- Credential or token leakage.

**Out of scope:** The Roblox platform, Roblox accounts or experiences, and already-public vulnerabilities in third-party dependencies (report those upstream).

## Credentials and local listeners

- **No built-in credentials or script networking:** `ree` never reads Roblox cookies, tokens, or keys, and Luau script HTTP requests are disabled.
- **External asset worker:** Optional manifest bundle and animation downloads run through the executable specified in `REE_ASSET_WORKER`. That worker handles any authentication itself; `ree` only sends JSON requests over stdin and never sees credentials.
- **Local service (`ree serve`):** Binds to `127.0.0.1` only with no authentication. Run it only on machines not shared with untrusted local users.

Never paste a cookie, token, or key into an issue, PR, or log. If you accidentally expose a `.ROBLOSECURITY` cookie, immediately sign out of all sessions in your Roblox account settings to invalidate it, then let us know so we can delete the post.
