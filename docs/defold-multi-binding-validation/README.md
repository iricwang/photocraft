Latest follow-up: [patch.6 disconnect/cancellation acceptance](patch6/README.md) (2026-10-18).

# Native many-to-many acceptance — 2026-10-17

PhotoCraft patch.5, source `744d4f340e15ba719ba86fb45efc41b0bbb14759`, ARM macOS with HEIF. Defold patch.30 (many-to-many source `15eac21a6d`); patch.31 carries the identical binding code plus CI/download and migration fixes.

Two native Defold projects and two native PhotoCraft instances retained all four independent edges. Initial protocol/UI regression also verified independent disconnect in both directions, rebind from either end, per-connection credential rejection, offline detection and automatic reconnection after an editor restart, unsaved-document preservation and correct same-name GUI routing. Defold interactions used cljfx.plorer; PhotoCraft consent, binding, selection and publish used native UI.

Final signed patch.5 repeated the 2×2 graph. Selecting A, opening B from Defold, changing `box-b.position.x` to 450 and pressing Sync saved B and cleared the document dirty flag. A stayed at 420. Publication lease/revision guards remain active; independent drafts are not silently merged.

Exact patch.3→5 upgrade passed local download, installation, restart health acknowledgement and full signed target inventory comparison. A separate patch.3 baseline downloaded from the published GitHub feed, installed, started and matched the same target inventory. Existing bootstrap and historical patches are retained; no DMG or full application archive was generated.

Final tests: engine/integration 2140 passed / 23 ignored; final UI 3084 passed / 18 ignored, total 5224 passed / 41 ignored, zero final failures. Strict all-targets Clippy, layering (33 checks), WASM, command parity (631/631), and explicit adversarial panic test passed. Inherited translation/menu/async/layout failures and a socket-fixture race found by the full run were fixed before the frozen release build.

[Published patch.5](https://github.com/iricwang/photocraft/releases/tag/patch-ai-p0-5)

- [Project selector](09-craft-patch5-two-projects.png)
- [Actual B publication](10-craft-patch5-project-b-synced.png)
- `final-matrix.json`, `craft5-sync-routing.json`: runtime routing evidence
- `craft-update5-local.json`, `craft-update5-github.json`: exact baseline update evidence

`${DEFOLD_REPO}` denotes the sibling checkout. Only sanitized session identifiers are recorded, never credentials. LAN behavior was not manually exercised. Sessions reconnect while a Craft remains running; after both applications exit, non-default addresses must be added again.

Final follow-up: all Defold patch.31 platform and publisher jobs succeeded. The exact local Defold22→CI31 migration passed published-GitHub download, installation, restart and all 168 file hashes/modes. Two final CI31 native editors then automatically reconnected to both released Craft5 instances with exactly four edges and fresh editor IDs; the native target selector displayed both Craft addresses without source hot loading. See `final-ci31-patch5-matrix.json`, `ci31-status.json`, `defold-ci-migration-validation.json` and [CI selector](14-final-ci31-target-selection.png).
