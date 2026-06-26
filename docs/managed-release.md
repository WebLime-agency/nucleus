# Managed Releases

Managed releases are the normal product install path for public users.

Contributor checkouts can still run from source and use git-based updates, but installed products track release channels:

- `stable`
- `beta`
- `nightly`

Nucleus does not describe installed users as tracking `main` or `dev`. Maintainers publish artifacts from source refs into product channels, and installed Nucleus instances follow those channel manifests.

## Channel Manifests

The public channel manifests are GitHub release assets:

```text
stable:  https://github.com/WebLime-agency/nucleus/releases/download/nucleus-channel-stable/manifest-stable.json
beta:    https://github.com/WebLime-agency/nucleus/releases/download/nucleus-channel-beta/manifest-beta.json
nightly: https://github.com/WebLime-agency/nucleus/releases/download/nucleus-channel-nightly/manifest-nightly.json
```

The `nucleus release install` command defaults to the correct manifest URL for the selected channel. Use `--manifest-url` only for mirrors, staging buckets, or local validation.

## Public Install

Install the current stable channel:

```bash
nucleus release install --channel stable --enable
```

That command:

- downloads the latest compatible artifact for the current platform
- verifies the artifact checksum
- stages the release under the managed install root
- activates it through the `current` symlink
- writes Nucleus-owned update state
- installs a `systemd --user` service unless `--install-service false` is passed

Default managed install root:

```text
~/.local/share/nucleus/managed
```

Important paths:

```text
current/                    active release symlink
previous/                   previous release symlink after an update
releases/<release_id>/      unpacked release payload
current/bin/nucleus-daemon  server binary for the active release
current/bin/nucleus         operator CLI included in official channel artifacts
current/web/                web bundle matching the active Nucleus release
current/scripts/            Browser sidecar runtime assets
current/node_modules/       Browser sidecar Playwright runtime modules
```

The service unit points at `current/bin/nucleus-daemon` and `current/web`, so an update swaps both the Nucleus process and the served web client together. Browser-capable releases also ship `current/scripts/browser-sidecar.mjs`, `current/node_modules/playwright`, and `current/node_modules/playwright-core`; the daemon must launch Browser from these managed-release assets rather than from the current working directory.

Managed installs default to `127.0.0.1:5201` so a new install is local-only unless the operator explicitly chooses otherwise. Bind modes are intentionally separate from Vault safe-origin rules:

- Localhost only: `127.0.0.1:<port>`; default and safest for local use.
- Tailscale/private interface only: bind to the specific Tailscale/private interface IP and prefer HTTPS/Tailscale certificates for browser access.
- LAN/all interfaces: binding to a LAN IP or `0.0.0.0` requires `--allow-unsafe-bind` and should be paired with clear operator intent.
- Custom/public: advanced only; prefer a localhost bind behind an HTTPS reverse proxy rather than direct public binding.

Example explicit LAN install:

```bash
nucleus release install --channel stable --enable --bind 0.0.0.0:5201 --allow-unsafe-bind
```

Plain HTTP remote origins remain unsafe for Vault plaintext operations. Vault unlock/create/update flows require localhost or HTTPS by default even when the daemon is intentionally reachable over LAN or VPN.

## Switching Channels

Use Settings to change the tracked release channel for an existing managed install. Nucleus persists the tracked channel and update history in the state database.

For a fresh install on another channel:

```bash
nucleus release install --channel beta --enable
nucleus release install --channel nightly --enable
```

`dev_checkout` installs do not accept release channels. They may track a git ref and use git-based self-update. `managed_release` installs do not track git refs and never shell out to `git pull`.

## Updating

Nucleus owns update checks and apply state.

From Settings:

1. Check for updates.
2. Apply the update.
3. Let Nucleus restart itself when restart control is available.

For managed releases, the apply path downloads the selected channel artifact, verifies checksum and size, stages the release, atomically moves `current`, records `previous`, and restarts onto `current/bin/nucleus-daemon`.

## Local Tokens

Installed users should not need to inspect systemd units to find the state directory for a token.
List local Nucleus instances:

```bash
nucleus instances
```

Then retrieve the token for the intended instance:

```bash
nucleus auth local-token --instance nucleus-dev-projects
```

The selector can also be a local URL:

```bash
nucleus auth local-token --url http://127.0.0.1:5202
```

Token discovery commands never print tokens from broad instance listing. If multiple instances are
installed and no selector is provided, the CLI prints the matching instances and exact selector
commands instead of guessing.

To rotate one instance token:

```bash
nucleus auth rotate-token --instance nucleus-dev-projects
```

The new token is printed once. Existing browser and client sessions using the old token must
reconnect or re-authenticate. The explicit state-dir form remains supported for operator workflows:

```bash
nucleus --state-dir <state-dir> auth local-token
nucleus --state-dir <state-dir> auth rotate-token
```

## Recovery

If an update staged successfully but the new Nucleus process does not come back:

```bash
systemctl --user stop nucleus-daemon.service
cd ~/.local/share/nucleus/managed
rollback_target="$(readlink previous)"
ln -sfn "${rollback_target}" .current-rollback
mv -Tf .current-rollback current
systemctl --user start nucleus-daemon.service
```

Then open Settings and run another update check. Nucleus will continue to report any latest error and restart requirement until a successful check or apply clears it.

If the service itself is broken, run the active Nucleus binary directly to inspect the error:

```bash
NUCLEUS_INSTALL_KIND=managed_release \
NUCLEUS_INSTALL_ROOT="$HOME/.local/share/nucleus/managed" \
NUCLEUS_WEB_DIST_DIR="$HOME/.local/share/nucleus/managed/current/web" \
NUCLEUS_BIND=127.0.0.1:5201 \
"$HOME/.local/share/nucleus/managed/current/bin/nucleus-daemon"
```

## Publishing

Maintainers publish channel artifacts with the `Publish Managed Release` workflow.

Defaults:

- `stable` publishes from `main`
- `beta` publishes from `dev`
- `nightly` publishes from `dev`
- scheduled runs publish `stable` from `main` when `main` has advanced past the latest stable `vX.Y.Z` tag; otherwise they complete as a no-op and write a no-op summary instead of a published-release summary

The workflow:

- resolves the stable release version before packaging
- verifies Rust and web checks
- builds release binaries
- packages `bin/nucleus-daemon`, `bin/nucleus`, and the built web bundle
- restores the existing channel manifest
- appends the new release
- keeps the newest configured release count, default `10`
- validates a local managed install from the generated manifest
- uploads the artifact and manifest to the channel release

The channel release tags are moving distribution tags, not source-control branch names:

```text
nucleus-channel-stable
nucleus-channel-beta
nucleus-channel-nightly
```

## Versioning

Stable managed releases use git tags as the version source of truth. At the start
of a stable publish, the workflow fetches tags, finds the latest `vX.Y.Z` tag,
computes the next version, writes that version into the workspace `Cargo.toml`,
`Cargo.lock`, and `apps/web/package.json`, opens an automated version-bump PR to
`main`, waits for the protected-branch checks to pass and merge, and creates an
annotated `vX.Y.Z` release tag on the merged `main` commit.

The default manual publish input is:

```text
version_mode=auto
bump=patch
```

With the bootstrap tag `v0.1.0`, the default stable publish computes `0.1.1`.
Use `bump=minor` or `bump=major` when the next stable release should move to the
next minor or major version. To publish an exact semantic version, run the workflow
with `version_mode=explicit` and set `version` to the intended value, for example
`0.5.0`. The `version` input is ignored while `version_mode=auto`.

If no `vX.Y.Z` tag exists, the workflow fails before packaging. Bootstrap once by
cutting an annotated `v0.1.0` tag against the current `main` HEAD:

```bash
git fetch origin main
git tag -a v0.1.0 origin/main -m "Release v0.1.0"
git push origin v0.1.0
```

Stable write-back requires the `RELEASE_PUSH_TOKEN` repository secret with
`contents: write` and `pull_requests: write` permission, plus enough scope to push
the generated release branch, open and merge the version-bump PR, and push
annotated release tags. The release workflow does not direct-push `main`; `main`
continues to move through the normal protected PR path.
