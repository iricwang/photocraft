# Local verification — 2026-10-17

Verified on macOS / Apple silicon with Rust 1.95.0. The feed was a loopback HTTP server;
this task did not publish release assets or a channel manifest to GitHub.

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

Current runnable bundle: `dist/PhotoCraft.app` (0.9.0-patch.2). The matching DMG and CLI ZIP
are in `dist/release/`; immutable bootstraps, inventories, patch packages and the local test
manifest are in `dist/patch/`. That manifest has loopback artifact URLs and is a test feed;
regenerate with production HTTPS URLs before publication, following [README.md](README.md).

Intel/universal builds and the published GitHub feed were not exercised on this machine.
