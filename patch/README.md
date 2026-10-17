# PhotoCraft incremental updates

The native app exposes **Help → Check for Updates…**. Checking and downloading run in the
background. **Install and Restart** becomes available after every document and the Defold
project have been closed and background jobs have finished. It prepares a complete replacement
before quitting, waits for all instances of that application bundle to exit, installs it, and
restarts PhotoCraft. The new window must report successful startup within 60 seconds; otherwise
the installer restores and relaunches the previous version. Closing the dialog postpones the
update; prepared downloads can be removed when the application exits normally.

This first implementation supports **macOS application bundles** (Apple silicon, Intel and
universal). Linux/Windows installers and browser builds retain their existing release flow.
The implementation is Rust only: `apps/photocraft-updater` is both the library used by the
native dialog and the release/installer executable. The bundled helper is copied outside the
application before installing so it can replace itself too.

## Release identity and baselines

`patch/channel.json` tracks the `ai-p0` patch revision. Before each new release:

```sh
cargo run --locked -p photocraft-updater -- bump patch/channel.json "Describe the change"
bash packaging/macos/package.sh --arch aarch64
```

Packaging embeds `Contents/Resources/patch-config.json` **before signing**, bundles and signs
`photocraft-updater`, then signs the outer app. Override `PHOTOCRAFT_PATCH_REVISION` only when
reproducing an existing release. `PHOTOCRAFT_PATCH_FEED` overrides the embedded feed at build
time and the feed queried at runtime (for a local test server). The default Apple silicon feed is:

`https://github.com/iricwang/photocraft/releases/download/patch-ai-p0/manifest-macos-aarch64.json`

Keep the exact, final signed/notarized bundle from every released revision. Code signatures,
permissions and embedded configuration are part of its inventory. Rebuilding the same revision
is not a substitute for retaining the actual baseline. Do not modify the bundle after releasing
it. A full bootstrap archive must remain available for first installations and incompatible or
locally modified baselines.

## Create a patch

After building the next signed application, run:

```sh
bash patch/package.sh \
  --base /path/to/previous/PhotoCraft.app \
  --target /path/to/next/PhotoCraft.app \
  --output dist/patch \
  --notes "Describe the change"
```

The script writes an immutable `photocraft-patch-<revision>-<platform>.pcp`, a full bootstrap
ZIP, the target inventory, and `manifest-<platform>.json`. Before generating later releases,
put the currently published platform manifest into that output directory: the packager retains
its transition history. A client chooses a transition matching **both revision and full tree
hash**, so it can take multiple patches through successive checks/restarts. Feed and package
platform/channel must agree; revisions must increase.

A `.pcp` contains a bounded gzip stream: `PCPATCH1`, a little-endian u64 JSON header length,
base and target inventories and per-file block plans, then the changed data blocks. Files are
split into 64 KiB blocks: matching SHA-256 blocks are copied from the exact local baseline,
and only unmatched blocks are transferred. This also avoids re-downloading a large executable
when only its macOS resource signature changes. Unchanged files are reused locally and deleted
files disappear from the replacement. Every reconstructed file is verified as a whole.

The feed contains the exact compressed size and SHA-256, both tree hashes, revision transition,
immutable package URL and release notes. Every target file is also checked against its hash,
size and permissions. Symlinks/special files, traversal, case collisions, file/directory conflicts,
extra payload and oversized inputs are refused. Bundle paths currently must be ASCII. The app
code signature is verified before install; the updater preserves the supplied signature and does
not re-sign downloaded code. Download trust is the configured HTTPS feed/repository plus
SHA-256 and macOS code-signature integrity; this does not add a separate publisher signing key.

## Publish (maintainer action)

The script generates artifacts locally and does not upload anything. Publish the immutable
revision release and bootstrap first, then advance the moving channel's platform manifest.
Commit and push the release source first, then build from that exact commit.
For example (replace revision/platform with the generated names):

```sh
gh release create patch-ai-p0-2 --repo iricwang/photocraft --prerelease --target "$(git rev-parse HEAD)" \
  --title "PhotoCraft patch.2" --notes "Release notes" \
  dist/patch/photocraft-patch-2-macos-aarch64.pcp \
  dist/patch/photocraft-bootstrap-2-macos-aarch64.zip \
  dist/patch/inventory-2-macos-aarch64.json
# Create the moving patch-ai-p0 release once, then upload each platform manifest:
gh release upload patch-ai-p0 --repo iricwang/photocraft --clobber \
  dist/patch/manifest-macos-aarch64.json
```

Do not remove the previous bootstrap until a patch-only upgrade from that exact baseline has
passed download, installation and restart verification. Production macOS distribution still
requires the normal Developer ID signing/notarization credentials.

## Local verification and recovery

```sh
# Use a separate writable copy of the baseline app, not the app you are currently editing.
python3 -m http.server 18791 --bind 127.0.0.1 --directory dist/patch
PHOTOCRAFT_PATCH_FEED=http://127.0.0.1:18791/manifest-macos-aarch64.json \
  /path/to/test/PhotoCraft.app/Contents/MacOS/PhotoCraft
```

For local packages, pass `--url-prefix http://127.0.0.1:18791` to `patch/package.sh`.
HTTPS is required for all other hosts; credentials in URLs, unsafe redirects and HTTPS
downgrades are rejected. The runtime override is never baked back into the bundle.

The same operations can be driven headlessly:

```sh
photocraft-updater check /path/to/PhotoCraft.app http://127.0.0.1:18791/manifest-macos-aarch64.json
photocraft-updater prepare /path/to/PhotoCraft.app http://127.0.0.1:18791/manifest-macos-aarch64.json
# After closing all instances, use the printed ticket path:
photocraft-updater install /path/to/.photocraft-patch-XXXX/ticket.json
```

The app's parent directory must be writable; there is no administrator escalation. The previous
bundle remains beside it as `.PhotoCraft.app.patch-backup-<revision>`. The sibling staging
folder retains `ticket.json`, `installer.log` and `install-status.json` (`complete` or
`rolled_back`). It is safe to remove old backups/staging folders after verifying the new app.
Never remove a staging folder while its installer is running. Replacement uses same-volume
renames and rolls back normal installation/startup errors. For a machine shutdown between the
two renames, quit PhotoCraft and run `photocraft-updater recover TICKET` from the retained helper;
it verifies and restores the baseline. This also supports an explicit manual rollback after a
successful update. It refuses to overwrite an application whose inventory has changed.

Tests: `cargo test -p photocraft-updater` covers file-delta reconstruction, signature checks,
exact baseline refusal, path/payload limits, transport policy, loopback downloads, corrupt hashes,
multiple-instance locks and recovery. Desktop tests cover the menu and installation guard.

```sh
cargo test -p photocraft-updater
cargo test --release -p photocraft --bin photocraft --features heif patch_update::tests
```
