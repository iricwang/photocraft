# PhotoCraft patch.6 acceptance — 2026-10-18

[Published patch.6](https://github.com/iricwang/photocraft/releases/tag/patch-ai-p0-6), source `f5b1f09d12a3e5ecdf0e6311bbb8d8b35140928a`. Native Apple Silicon macOS release with HEIF; two unmodified CI Defold patch.31 editors and two final signed PhotoCraft patch.6 instances. Only an incremental `.pcp` and target inventory were published. No new DMG or bootstrap ZIP.

## Disconnect policy

Explicitly disconnecting an endpoint pauses discovery of that endpoint for the current PhotoCraft session. An intentional Bind Defold operation can reconnect it. Temporarily offline endpoints remain registered and reconnect automatically. Restarting PhotoCraft begins a new discovery session. These choices are distinct from persistent project/client identity storage.

Per-endpoint epochs cancel pending handshakes and refresh work. Final peer insertion checks the epoch under the same state lock. A release can revoke its own pending ticket, without authorizing that ticket to open a resource or disturbing unrelated endpoints. Pending credentials are never accepted for GUI-open operations.

## Final gates

5,345 native tests passed; 41 inherited opt-in tests were ignored; zero failures across 93 test binaries. Strict all-targets Clippy, dependency layers, WASM and menu parity (631/631) passed. The separately enabled adversarial command panic test passed. `build-gates.json` records the source and exit statuses. No Defold source changes were required.

## Runtime cases

| Case | Result and evidence |
| --- | --- |
| Two clients × two editors | All four independent edges online: [client A](four-connections-6a.png), [client B](four-connections-6b.png), `four-connections.json`. |
| Client-initiated selective disconnect | Only client B → project B was removed; the other three stayed online. It remained absent through subsequent runtime cases: [window](manual-disconnect-one-edge-6b.png), `manual-disconnect-one-edge.json`. |
| Explicit reconnect | The native Bind Defold and project-confirmation buttons restored all four edges: [window](explicit-rebind-restored-6b.png), `explicit-rebind-restored.json`. |
| Editor-initiated selective disconnect | Defold's native chooser removed only project B → client A: [window](defold-initiated-disconnect-6a.png), `defold-initiated-disconnect.json`. |
| Reconnect after editor-side release | Native client binding restored the removed edge, with the independent draft still dirty: [window](both-directions-reconnected-6a.png). |
| Temporary offline and restart | Project B was normally closed after verifying zero dirty resources. Both clients marked B offline while A stayed connected: [window](project-b-offline-a-online-6a.png). Restarting B generated a fresh editor ID; both clients automatically reconnected to it and the strict four-edge matrix passed again. See `project-b-offline-a-online.json`, `offline-recovered-origin-a.json`. |
| Late transfer after cancellation | An authenticated local fixture paused its project manifest transfer. Releasing its pending ticket then delivering valid late data did not add the endpoint, change the existing two peers, or modify the dirty draft. Its old ticket was rejected and its request count stayed at one: [window](cancelled-transfer-draft-retained-6a.png), `delayed-cancel.json`. |
| Same-name GUI source routing | Each Defold opened its actual `main.gui` editing page and used the native client chooser. Client A retained both different project paths plus its dirty draft; client B received no document. Reopening the same resource reused its document: [window](same-name-gui-routed-by-origin-6a.png), [Defold chooser](defold-native-client-picker.png), `same-name-gui-routed-by-origin.json`. |

Defold UI operations used `cljfx.plorer` through its live REPL. PhotoCraft binding, project consent and disconnect operations used native controls. State inspection and screenshots used the authenticated app control interface. Configured PhotoCraft QA launches used dedicated configurations/listeners and denied network access to the user's default Defold port. One UI observer relaunched the closed old QA bundle without its launch options during the first local install attempt; that empty test instance and its temporary connection were normally closed, the original user connection was verified intact, and the installation was retried successfully. QA screenshots and JSON contain no credentials. LAN was not exercised.

## Incremental delivery

The immutable package is 34,274,185 bytes. Existing transitions `2→3` and `3→5` were retained and `5→6` appended to the moving GitHub feed. The prior bootstrap remains available.

Two independent exact signed patch.5 baselines completed actual download, reconstruction, install, automatic restart, startup-health acknowledgement and full nine-file hash/mode/config comparison with the signed target inventory:

- Local HTTP feed: successful patch.5→6 update.
- Published GitHub HTTPS feed: successful patch.5→6 update, [restarted application](github-installed6.png).

`acceptance.json` records both update results, native cases, source, package SHA-256 and exact base/target tree hashes. `${DEFOLD_REPO}` denotes the sibling Defold checkout. The first local install attempt safely refused an occupied bundle; no partially installed version was published.
