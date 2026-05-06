# Managed Releases

Managed releases are the normal product install path for public users.

Contributor checkouts can still run from source and use git-based updates, but installed products track release channels:

- `stable`
- `beta`
- `nightly`

Nucleus does not describe installed users as tracking `main` or `dev`. Maintainers publish artifacts from source refs into product channels, and installed daemons follow those channel manifests.

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
nucleus release install --channel stable --enable --bind 0.0.0.0:5201
```

That command:

- downloads the latest compatible artifact for the current platform
- verifies the artifact checksum
- stages the release under the managed install root
- activates it through the `current` symlink
- writes daemon-owned update state
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
current/bin/nucleus-daemon  daemon for the active release
current/bin/nucleus         operator CLI included in official channel artifacts
current/web/                web bundle matching the active daemon release
```

The service unit points at `current/bin/nucleus-daemon` and `current/web`, so an update swaps both the daemon and the served web client together.

## Switching Channels

Use Settings to change the tracked release channel for an existing managed install. The daemon persists the tracked channel and update history in the state database.

For a fresh install on another channel:

```bash
nucleus release install --channel beta --enable
nucleus release install --channel nightly --enable
```

`dev_checkout` installs do not accept release channels. They may track a git ref and use git-based self-update. `managed_release` installs do not track git refs and never shell out to `git pull`.

## Updating

The daemon owns update checks and apply state.

From Settings:

1. Check for updates.
2. Apply the update.
3. Let the daemon restart itself when restart control is available.

For managed releases, the apply path downloads the selected channel artifact, verifies checksum and size, stages the release, atomically moves `current`, records `previous`, and restarts onto `current/bin/nucleus-daemon`.

## Recovery

If an update staged successfully but the new daemon does not come back:

```bash
systemctl --user stop nucleus-daemon.service
cd ~/.local/share/nucleus/managed
rollback_target="$(readlink previous)"
ln -sfn "${rollback_target}" .current-rollback
mv -Tf .current-rollback current
systemctl --user start nucleus-daemon.service
```

Then open Settings and run another update check. The daemon will continue to report any latest error and restart requirement until a successful check or apply clears it.

If the service itself is broken, run the active daemon directly to inspect the error:

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
- scheduled runs publish `nightly`

The workflow:

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
