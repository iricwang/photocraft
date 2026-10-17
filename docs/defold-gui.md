# Defold GUI in PhotoCraft

## Documents and tools

A GUI opens as a normal PhotoCraft document. GUI nodes use the existing Layers tree, canvas, tools, selections, masks, effects, text editing, undo/redo and AI generation. A new ordinary drawable layer in the GUI root or a GUI node's structural area creates a GUI node with its own stable identity. Inside that node's designated Artwork/Text subtree, a new layer changes only the owning node's appearance. Masks, adjustments and clipped artwork do not silently become independent GUI nodes. AI versions stay artwork of their owning node; adding artwork to a node represented by a single pixel layer creates the required container once while preserving its identity. Renaming a display layer does not rename a script-facing GUI node ID.

Defold GUI Properties is its own floating, resizable, dockable panel, initially beside the original Properties tab. Its property order follows Defold, using PhotoCraft widgets. Defold Sync is a separate dockable panel. Ordinary PSD/PCraft/image documents keep their original tools and save behavior.

## Save and publish are different actions

1. Open `game.project` and choose a GUI, or open the `.gui` directly. A connection belongs to the project. Connecting or restarting Defold does not close existing documents or discard their edits.
2. Edit with the native tools. Painting, erasing, moving, property edits and AI completion do not encode exports, poll Defold or write game assets.
3. **Save creative source** / Ctrl+S saves a complete native revision to the project's `.photocraft/sources/`. It preserves editable layers and AI history. Linked smart-object inputs inside the project are pinned by content hash into the source package and restored into a local cache on another computer. Embed inputs outside the project before sharing them.
4. **Sync to Defold** first saves the creative source, freezes a publication plan, acquires resource leases, then asks the compatible running Defold editor to apply and save the affected resources. No unrelated dirty editor resources are saved. With no compatible editor, source saving still works and publication stops without an offline GUI-write fallback.
5. Success means a durable `COMMITTED` receipt covering the actual saved GUI, PNG and atlas hashes. `APPLYING` or `SAVING` means confirmation is pending. **Check publication status** queries or continues the same prepared transaction instead of creating another one. **Cancel prepared publication** is available only before applying starts. A lost editing lease remains visible so that a prepared transaction can be cancelled instead of appearing to wait indefinitely.
6. **Release editing permission** hands the source to another computer. Save the creative source before handing it over. Unsaved local edits remain local; a later source conflict never silently replaces the host's head.

Source/publish acknowledgements, workspace epochs and editing leases are runtime state outside Undo. Undo after a save does not roll back the host's revision. Repeated saves use compare-and-swap. A lost acknowledgement is recovered by operation/transaction ID. Source saves and manual publication upload a captured revision on a worker, so drawing can continue while the host replies. Their acknowledgement marks only the captured revision; newer strokes remain dirty and cannot be replaced by exported metadata. A second project write waits for that operation to finish. Once an external save starts, its acknowledgement is kept; the document cannot close in the middle of that save.

Structural layer commands update the GUI-role mapping; individual paint samples do not traverse the whole tree for synchronization. Manual publication can still render and encode previously painted artwork again. Content hashes prevent redundant output writes, but a complete transform-aware export cache is not yet implemented.

## Project data

| Location | Purpose |
| --- | --- |
| `.photocraft/project.json` | Stable project identity |
| `.photocraft/sources/index.json` | Scene identities, source heads and save receipts |
| `.photocraft/sources/<package>/revisions/<revision>/` | Immutable complete native source, baseline, dependency manifest and pinned inputs |
| `.photocraft/assets/index.json` | Asset ownership, current outputs and committed publication receipts, atomically updated together |
| `.photocraft/publications/<transaction>/` | Publication plan, original/actual output bytes and adapter receipt |
| `.photocraft/local/` | Ignored checkout identity, leases, locks and disposable source caches |
| `.internal/photocraft/transactions/` | Defold staging; not the only backup of published/original bytes |
| `photocraft_generated/<scene UUID>/` | Stable runtime output folder with one `scene.atlas` and named images |

Keep project identity, formal sources, ownership and publication records with the project in the team's existing version-control/backup workflow. A project host is not a replacement for source control. Credentials and TLS private keys live in private per-user settings, never in source documents or shared project metadata.

Existing `.gui.pcraft` sources remain readable. Formal acknowledged source takes priority. Legacy files are not overwritten or removed by ordinary saving. The engine's migration facilities inventory and back up original bytes before converting complete native sources; ambiguous baselines, incomplete linked inputs and independent shared sources are reported for review rather than flattened. The **Project assets and legacy sources** section refreshes an explicit host inventory, shows active/retained resources and retention reasons, and lets the user select a complete legacy native source for backup and migration. Migration leaves the current open draft and its source head unchanged. Preserve that draft separately before reopening the GUI to use a migrated source; ambiguous records remain review items. No physical-delete action is exposed.

## Runtime texture ownership

An asset has a stable UUID independent of layer name, GUI ID, output path and revision. PNG names follow layer names, including Chinese and spaces; invalid characters and collisions are normalized deterministically. Painting replaces the same owned output. Renaming preserves asset/frame identity and changes its readable path instead of creating another active asset. Reopening and Undo retain that identity.

Atlas entries use explicit stable single-frame animation IDs, so texture references do not depend on temporary PNG basenames. The ownership registry stores output checksums, scene references and export-recipe metadata. The native bridge records baked image dimensions, the exact node-local crop origin and the PNG-to-node transform. Shared consumers use the canonical owner's recipe, not the bounds of their resampled preview. The complete native source remains authoritative; an export recipe does not reconstruct editable layers from a PNG. Existing game textures are external resources and are not claimed merely because their folder name resembles a generated folder.

Deleting a layer detaches its generated asset on the next publication. Previous paths needed by Defold Undo or another scene are retained with reasons, and the formal publication archive keeps the original bytes. Garbage-collection previews remain conservative when reference/Undo roots are unknown. Automatic permanent deletion is intentionally unavailable until those roots can be proved complete; ordinary synchronization does not delete old game files.

The host stages resources and coordinates the durable journal. The Defold adapter is the sole executor of game-file mutations: it installs verified PNGs, updates an existing atlas through its original graph object, applies all GUI targets in one graph operation and writes a frozen save snapshot. Resource refresh is limited to safe additions/stateless updates. A refresh that would replace graph nodes retained by Undo stops with `UNDO_BARRIER_REQUIRED`; it never silently clears the editor's Undo history.

## Shared artwork across GUIs

In **Defold Sync → Shared artwork**, select a saved and published owner node and use **Copy shared artwork ID**. In another GUI in the same project, select the consumer node and choose **Use shared artwork for this node**. The consumer stores an immutable owner package revision, source node and stable carrier identity. Its displayed native subtree is a disposable read-only cache: source saves omit its pixels and smart-object cache inputs, and reopening rehydrates the pinned owner. Existing consumer artwork can be recovered through the native Undo history.

**Open artwork source** activates an already open owner without replacing its draft, or opens the latest formal owner source and selects the canonical node. **Make artwork independent** reads the pinned complete native subtree and gives the consumer a new asset identity. Masks, effects, text layers and matching AI generation provenance are copied; the old shared source remains unchanged. Normal paint, eraser, AI, masks and filters work on the independent copy. Editing a shared preview directly is refused, while the GUI carrier can still move, change structural properties and keep child GUI nodes.

Shared publication gathers the canonical owners and every current consumer, checks open drafts, saved heads and Defold target snapshots, then saves immutable sources in dependency order and publishes all affected GUI targets in one transaction. A dirty related GUI or unsupported reference stops the operation before those source heads advance. If a related GUI has saved changes that were never published, the normal Sync action stops before saving any source. **Review shared publication** lists every target and its saved version; **Publish these saved related changes** explicitly includes that scope. Changing the native draft or any target source head invalidates the review. Source saves are individually acknowledged and retained even if a later source CAS or the final game publication fails; they are not rolled back as one source transaction. Source archives retain older pinned versions. A second project write waits even when another GUI tab is active; painting remains local.

The initial supported scope is sharing between different GUI source packages in one project. Same-package self references and dependency cycles are refused. A source must have an actual managed artwork output and a verified node-local crop recipe. Legacy managed outputs missing this metadata are baked once on the next manual publication. Erasing an already shared owner preserves its asset and frame identity with a transparent 1×1 output. Ancestor masks, effects, opacity or backdrop-dependent blending cannot be detached safely and are rejected. Owner GUI instance styling is not yet separated from native artwork: the canonical owner must remain enabled/visible with unit carrier/artwork-root opacity and white GUI tint. Internal artwork layers remain editable. Saving a draft with other owner styles is allowed, but shared publication stops before source writes and identifies the owner; restore its instance style or make consumers independent. Consumer visibility and position remain local instance properties. Document pattern libraries and canvas-aligned fills cannot currently be transplanted as native shared content; keep them local or explicitly rasterize first. Consumer and owner native color modes/profiles must match. Cross-package missing versions or changed identities never fall back to a flattened editable replacement.

## Another computer

Package `photocraft-project-bridge` beside the desktop executable. Local projects start/reuse this lightweight host automatically when the binary is available. Engine-only development builds use the same store in process.

For an explicitly selected LAN project, run the host on the computer that owns the Defold working copy:

```powershell
photocraft-project-bridge --project "X:\Projs\FortySix-MiniGame" --listen <host-IP>:17656 --allow-lan --export-pairing <private-pairing-file.json>
```

Transfer the pairing file privately to the collaborator. In **Defold Sync → Project connection → Connect project host**, select it, then enter the exact project-relative GUI path and choose **Open project GUI**. A connection fetches only that GUI's render dependencies (including atlas images and project fonts), not the whole source repository. The source package and dependencies use bounded binary transfers over paired TLS. The host permits structured project operations; it does not expose arbitrary editor evaluation over the LAN.

Cached remote projects never become writable local authorities if the host goes offline. Keep the draft open or use native recovery, reconnect to the selected host and query a pending operation before publishing again. Source conflicts, changed targets, expired leases and changed workspace epochs stop writes and preserve the draft. Actual two-computer/firewall verification is a separate acceptance check from loopback transport tests.

If the host restarts with a new address or renewed pairing, select its updated pairing file again. Existing GUI documents in that exact authority workspace switch to the new connection without closing, reloading, changing the selected layer or adopting a newer source head. Documents from other projects/workspaces retain their own connection. Let a pending project save/publication finish before reconnecting. A disconnected save leaves the native draft dirty; **Save a Copy** to `.pcraft` can preserve a separate editable backup before closing it. Reconnecting does not merge a stale draft over another artist's saved source.

The older IP binding UI remains for compatibility and local resource navigation. It is not the new remote-publication transport, and its editor-evaluation route is loopback only.

## Canvas and compatibility

The default reference frame comes from `[display] width/height` in `game.project`. Root-node dimensions never replace it. Defold `(x,y)` maps to native `(x, reference_height-y)` without rotating the GUI. The scene view shows artwork outside the reference frame; opening/Fit frames the reference and visible nodes together, with bounded handling of extremely distant nodes. Normal PSD documents retain their normal canvas behavior.

For FortySix, `/assets/defold/gui/ui_window_character.gui` and `/game/modules/ui/ui_window_character/ui_window_character.gui` are different resources. The former has 45 nodes and a root at `(0,45)` with manual size `(0,90)`. The latter has 46 nodes, including a distant node. Neither is substituted by basename. The original project's reference frame is 2340×1080. The same full-path distinction applies to challenge.

Template instances/layout overrides and unsupported custom-node replacement stop publication rather than dropping data. Native drawing is available, but Defold rendering parity for custom materials, stencil clipping, nine-slice scaling, Pie/ParticleFX and complex dependency previews remains incomplete. Changed composite artwork can publish as a textured Box; this matters to scripts expecting a Text node. Cross-node adjustments/backdrop-dependent effects are rejected by the conservative publication check; keep effects inside the intended artwork group so independent interactive nodes are not implicitly flattened together.

The corrected adapter accepts finite three-component Euler rotations and performs the pose conversion in Defold. An old/incompatible editor is rejected before publication; the new path does not fall back to writing unsafe rotation data to disk.

**Review Defold changes** compares the last acknowledged common GUI, the current native draft projected to game properties, and a fresh Defold snapshot. It uses the durable published GUI archive when a reopened source predates its own publication, after verifying source ancestry. Independent supported properties can be accepted as one native Undo operation; layers, paint, masks, AI context and stable identities are retained. Same-field conflicts offer **Keep Craft** and, where safe, **Use Defold**. Published native artwork remains protected, even if it has no new paint since the last publication.

The review is tied to the exact native revision, source head and target snapshot. Any change invalidates it. Save a dirty target GUI in Defold and check again before applying; the review never saves unrelated resources or silently adopts another client's source. A restart may change Defold's graph serialization while leaving the saved GUI bytes identical; confirmed source ancestry plus the exact committed disk hash permits a fresh editor fingerprint.

Node creation/deletion/reparenting, order changes, resource-collection or unknown-field changes, and interacting parent/child transforms require manual reconciliation. Only verified scalar/native transform projections are applied automatically. Unsupported appearance replacement cannot discard painted content. Applying a review changes the local draft and its external baseline, not game files; save or manually publish when ready. The accepted external baseline and host source acknowledgement remain outside Undo.

## Commands and tests

| Command | Parameters |
| --- | --- |
| `defold.openProject` | `path`: project or `game.project` |
| `defold.openDesign` | `path`: absolute GUI file |
| `defold.saveDesignSource` | none |
| `defold.syncDesign` / `defold.publishProjectDesign` | optional `sharedReviewId`; explicit publication |
| `defold.reviewSharedPublication` / `defold.sharedPublicationReviewStatus` | none; inspect publication scope / cached report |
| `defold.refreshPublication` | none; resume/query an existing transaction |
| `defold.cancelPublication` | none; cancel only a prepared, unapplied transaction |
| `defold.requestProjectEditing` / `defold.releaseProjectEditing` | none; explicit editing handoff |
| `defold.reviewProjectChanges` | none; fresh three-way review |
| `defold.applyProjectReview` | `reviewId`; optional `choices` mapping item indexes to `mine` / `theirs` |
| `defold.refreshAssetManagement` / `defold.assetManagementStatus` | none; explicit refresh / cached report |
| `defold.migrateLegacySource` | selected `input`, reviewed `inventoryHash`; optional `operationId` |
| `defold.migrationStatus` | `operationId` |
| `defold.revealProjectResource` | exact project-relative `path`; explicitly open it in Defold |
| `defold.shareNodeArtwork` | none; inspect the selected saved/published owner and return its asset ID |
| `defold.attachSharedArtwork` | `assetId`; replace the selected node appearance with a pinned reference |
| `defold.openSharedArtworkSource` | none; open/select the referenced native owner |
| `defold.makeArtworkIndependent` | none; copy the pinned native subtree with a new asset identity |
| `defold.connectProjectHost` | `pairingFile`: private local JSON file |
| `defold.openProjectGui` | `path`: exact project-relative GUI resource |
| `defold.projectStatus` / `defold.projectConnectionStatus` | none; cached runtime information |
| `defold.setDesignProperties` | `values`: property map |
| `defold.addDesignNode` | `child`; optional `kind` |

New project commands are desktop-only where they require a host. The common engine/UI continues to compile for WASM. Capability-scoped automation cannot invoke ambient project side effects until explicit project capabilities are represented in its protocol; normal native UI commands can.

`crates/engine/tests/defold_project_live.rs` contains opt-in real-editor tests. They refuse projects without a `.internal/photocraft/test-project` marker. Set `PHOTOCRAFT_DEFOLD_TEST_GUI` to a disposable fixture, then run `cargo test -p photocraft-engine --test defold_project_live -- --ignored --exact native_layer_source_publish_rename_delete_and_reopen_against_real_defold`. This checks source-only saving, publication receipts, named rename without new asset identity, deletion and source reopening. Select tests by exact name; the separate host-shutdown helper must not run concurrently with publication tests. A test run is evidence only when it actually finishes; pure/FakeStore tests do not substitute for it.

## Multiple Defold / PhotoCraft connections

Binding protocol 2 keeps a collection of independent project sessions. Add a second project or client without releasing the first. The Defold Sync panel lists the connected projects, lets you select the resource-navigation target and disconnects only that connection. Its PhotoCraft port is shown for connecting another editor instance; if the default port is occupied, a new instance chooses a free port (an explicit PHOTOCRAFT_BIND_PORT remains strict).

Document synchronization routes by the document's original project root, even when another project is selected in the panel. GUI-open requests from Defold retain their originating peer and cannot be redirected by a later selection. `defold.openBoundResource` accepts an optional connection `id`; `defold.selectBinding` and `defold.unbind` manage a specific session. `defold.bindingStatus` includes `peers` with individual connection health, while retaining the legacy selected `peer` field. Credentials are never included in inspection results.

Renewing one session invalidates only its old token. Disconnecting or losing one project leaves other connections usable, and health checks cycle through all known endpoints. Reconnection to an already known project preserves dirty documents from other projects. Full many-to-many operation requires updated versions of both PhotoCraft and Defold. First-time connection handshakes are serialized; established sessions remain usable while another peer is being added.

Native multi-binding regression and exact patch upgrade evidence: [2026-10-17 acceptance](defold-multi-binding-validation/README.md).
