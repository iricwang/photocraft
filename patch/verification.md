# Patch verification — 2026-10-17

## Local feed: legacy → patch.1 → patch.2

Verified on macOS / Apple silicon with Rust 1.95.0. The feed was a loopback HTTP server;
these initial checks preceded GitHub publication.

- Actual legacy executable → patch.1: downloaded 37,000,687 bytes, reconstructed the bundle,
  verified its signature/inventory, installed it and received startup acknowledgement.
- Native Help → Check for Updates → Download → Install and Restart: patch.1 → patch.2 passed.
  This metadata/signature update transferred 57,539 bytes using baseline block reuse.
- An open document disabled installation. Closing/saving work allowed installation.
- Creating, painting, native save/reopen and rendering worked after the upgrade.
- A validly signed test target with a deliberately non-starting executable triggered automatic
  rollback; patch.2 was restored byte-for-byte and relaunched.
- The extracted bootstrap ZIP, published-file inventory and installed application matched exactly.
  Bootstrap revisions 0, 1 and 2 were retained. The test failure package is outside the release folder.
- 12 updater tests, 2 native dialog tests and 19 existing settings tests passed. Desktop dialog
  tests used the same release libraries as the shipped application.
- Strict clippy, dependency layers, all xtask WebAssembly checks (including HEIF), touched-file
  formatting, shell syntax and the menu parity generator passed (631/631).
- Screenshots were inspected for layout and the disabled-install state.

Local evidence: `target/patch-verification/verification.json`, `verify.py`,
`update-offered.png`, `update-ready.png`, `update-document-guard.png`,
`post-update-editing.png`, and the test/build logs under `log/`.

At completion of these local checks, the runnable bundle was `dist/PhotoCraft.app` (0.9.0-patch.2). The matching DMG and CLI ZIP
are in `dist/release/`; immutable bootstraps, inventories, patch packages and the local test
manifest are in `dist/patch/`. That manifest has loopback artifact URLs and is a test feed;
regenerate with production HTTPS URLs before publication, following [README.md](README.md).

Intel/universal builds were not exercised on this machine.


## Published GitHub feed: patch.2 → patch.3

Built from clean, committed source `591f9288863135125d27a988a052f8f3e4d63cc3`, pushed to
`codex/photocraft-ai-p0`, then published as
[patch-ai-p0-3](https://github.com/iricwang/photocraft/releases/tag/patch-ai-p0-3).
The [moving channel](https://github.com/iricwang/photocraft/releases/tag/patch-ai-p0) supplies
`manifest-macos-aarch64.json`. Both tags point to the actual build-source commit; this
verification record is a subsequent documentation-only commit.

- Downloaded the retained patch.2 bootstrap, both inventories and the channel manifest from
  public GitHub HTTPS URLs without GitHub authentication. The baseline matched exactly.
- Launched the downloaded app with isolated settings and its embedded production feed;
  `PHOTOCRAFT_PATCH_FEED` was absent. Native **Help → Check for Updates** offered patch.3.
- Native **Download** fetched the published package, verified hashes and the code signature,
  and reconstructed a target matching the released inventory exactly.
- Native **Install and Restart** completed with `phase=complete`, revision 3, and startup
  acknowledgement. The installed tree matched the target; the retained backup matched patch.2.
- The restarted app created and painted a document, saved and reopened `.pcraft`, rendered
  correctly, and checked the production feed again: **PhotoCraft is up to date.**
- Visually inspected the downloaded/ready dialog, edited document and final patch.3 dialog.
- Also upgraded `dist/PhotoCraft.app` through the published feed with the CLI installer,
  confirmed restart and the same target inventory, then exited the verification instance.
- The 12 updater tests passed again; the release DMG and CLI signature checks passed.

Patch bytes: **177,663**. SHA-256:
`eb8facafa69ad8c5ed6014a9677b7c47f9c1a181d57fb51b2386c25e4105b3e1`.
Target tree: `96cec07c5fb4630a2baf54e8a31746c5e27ca3b9b7f0811e3a46683e0e51564c`.
The release retains both full bootstrap ZIPs, inventories, patch and `SHA256SUMS`.

Reproducible local evidence: `target/github-patch-verification-3/verify.py`, `verification.json`,
`online-update-offered.png`, `online-update-ready.png`, `online-post-update-editing.png`,
`online-up-to-date.png`; logs: `log/patch-github-*.log`.
Current runnable bundle: `dist/PhotoCraft.app` (**0.9.0-patch.3**). Published artifacts are also
retained under `dist/github-patch-3/`; older local test artifacts remain under `dist/patch/`.

This published-feed verification used macOS Apple silicon. Bundles are **ad-hoc signed and
not Apple notarized**; it does not establish Gatekeeper acceptance on another Mac.
