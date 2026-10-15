# Defold GUI as a native PhotoCraft document

## Working model

Opening `.gui` now uses `defold.openDesign` and creates an ordinary PhotoCraft `Document`. Each GUI node maps to a normal layer group; its pixels or editable text live inside that group, alongside any child-node groups. The standard canvas, tools, Layers panel, selection, masks, layer styles, text tools, native undo/redo and AI generation remain active. PSD and ordinary image documents keep their existing behavior.

The independent **Defold GUI Properties** panel follows the editor's field order: Id, XYZ position/rotation/scale/size, enabled/visible, node-specific text or texture, material, color/alpha, text appearance, layer/blend and layout/clipping. It uses PhotoCraft number fields, sliders, color pickers, text fields and dropdowns. The resource arrow opens the assigned font/material/atlas in the running Defold editor. PhotoCraft's original **Properties** panel continues to show native layer properties independently.

Opening a GUI reveals Defold GUI Properties as a separate tab alongside native Properties and Adjustments on first use. Drag its title/tab to dock it, resize it, or close it. The Defold Sync panel and Window menu reopen it at its remembered location. Workspace persistence includes its docking and visibility state. Selection follows the current document/node; an ordinary image or an unrelated layer shows an empty-state prompt instead of stale GUI fields.

**Defold Sync** is a separate floating, dockable, resizable panel, opened with linked GUI documents or Window → Defold Sync. It contains the linked resource path, project details, manual synchronization and its result, plus node creation and the properties shortcut. These controls no longer occupy the canvas header. Closing and reopening the panel restores its last location and size; switching to an ordinary document shows an empty state.

## Open, edit, save and synchronize

1. Open the game in Defold, then open `game.project` in PhotoCraft and select a GUI. Direct `.gui` opening also works. The Windows launcher is `Start-FortySix-PhotoCraft.cmd`.
2. Expand a node group and select its Artwork or Text layer to edit that node with PhotoCraft's existing tools. New ordinary layers inside the group publish as child GUI nodes; new outer layers publish as root nodes. Imported Artwork/Text layers and AI result versions continue to edit their owning node's image. Existing child-node groups are exported separately.
3. The Defold Sync panel provides Add child node and Add GUI node. Rename, delete, duplicate and reorder groups using the normal layer system. Changes enter normal document history.
4. Generation uses the existing provider, reference, prompt and version workflow. With no selection, a linked node's artwork bounds are the default generation area. Results are inserted inside that node group; switching documents while generation runs still routes the result back to its original document. No generation is triggered just by opening a GUI.
5. Ctrl+S saves the editable native document, normally `<scene>.gui.pcraft`. It does not publish to Defold. That source preserves the complete editable layer tree and AI history; it is necessary because a GUI/atlas cannot represent all PhotoCraft operations.
6. Click **Sync to Defold** to publish. Unchanged artwork keeps its authored GUI resources. Modified node artwork is composited separately, converted to sRGB and written as a transparent PNG plus atlas under `photocraft_generated/<gui>/<revision>/`. Generated resources are isolated from the original shared atlas images. The editable source is saved before publication.
7. With Defold running, publication registers the textures and updates its in-memory GUI graph through the authenticated loopback editor API. The status asks you to save the GUI in Defold afterward. PhotoCraft does not silently save unrelated Defold resources. With the editor closed, synchronization atomically writes the GUI on disk.

Manual synchronization is intentional. Brushes, erasing, property inputs and AI completions do not send intermediate changes to Defold.

### Brush rendering

GUI scene artwork outside the reference frame shares the native brush damage tracking. Live dabs update only affected texture regions, preserving brush/eraser pixels and the reduction grid in large scenes. Undo, unknown changes and expanding scene bounds rebuild the cache to avoid stale artwork. This reduces per-dab rendering and upload work without changing drawing quality or publishing intermediate edits to Defold.

### Canvas dimensions

GUI documents use a Defold-style scene view: `game.project` → `[display] width/height` defines the default reference frame. Root-node sizes never replace it and off-frame nodes never enlarge it. The scene view uses a dark transparent background, a reference-frame outline, and visible artwork outside all four edges. Opening/Fit on Screen frames the reference rectangle and visible nodes together; normal PSD documents keep their existing image-canvas behavior. Defold `(x, y)` maps to native document `(x, reference_height - y)` without rotating, scaling or rewriting nodes. Properties remain in Defold coordinates. Extremely distant nodes are excluded from automatic framing above 16384 pixels / 64 MP, with a warning; their pixel data remains intact. Named display-profile layouts remain outside the supported publication scope.

Version 3 retains a migration origin so interim sources with a shifted editing envelope can return to the Defold reference frame. Normal GUI sources have origin `(0, 0)`. Existing v1/v2 sources, including the previously shipped root-size heuristic, are corrected on GUI/native opening or manual synchronization. Whole-pixel translation preserves artwork, masks, locks and geometry; supported AI history regions follow that translation. Save the source to retain the correction. Unavailable projects leave saved sources accessible with a warning. Publication preserves exact authored positions unless a raster crop actually changed a node or parent origin.

Actual-editor verification for FortySix identified the currently linked resource as `/assets/defold/gui/ui_window_character.gui` (45 nodes). Its root is `(0,45)` with manual size `(0,90)`, not a full-screen board. Defold itself displays its content partly left/below the default 2340×1080 reference frame. `/game/modules/ui/ui_window_character/ui_window_character.gui` is a different 46-node resource, including an extremely distant node; it is not silently substituted. The Defold Sync panel displays the project-relative resource path to distinguish these files. The correction makes existing coordinates accessible; it does not invent a runtime layout for either resource.

## Persistence and conflicts

The native format stores an optional versioned Defold attachment with project/GUI paths, authored TextProto, node-to-layer IDs, appearance baselines and the last acknowledged editor snapshot. Older native files have no attachment. Closing and reopening the GUI restores its `.gui.pcraft` source when present. If the external GUI changed, the editable source still opens with a warning and publication stops on the conflict.

Undo/redo uses PhotoCraft's normal history. Publication stamps are additionally tracked outside that history, so undoing an edit after publishing does not restore a stale conflict baseline. Deleting a node group removes its corresponding GUI node on the next sync. Child nodes are excluded when flattening a parent's own artwork.

Disk hashes and live node snapshots are checked before publication. External changes or an interrupted request stop publication; there is no automatic overwrite, automatic retry or silent offline fallback. Generated paths reject traversal and escaping symlinks. Existing generated resources with different bytes are never overwritten.

The initial disk/editor comparison accounts for Defold's protobuf float precision. Editor snapshot JSON uses lossless floating-point parsing; older saved snapshots tolerate only double-precision rounding noise. Actual changes, including adjacent representable GUI float values, remain conflicts. A normal pixel layer added inside a linked node contributes to that node's published artwork; an outermost layer becomes its own GUI node.

The live API performs several transactions: register resources, attach/remove nodes, set properties, reorder. It cannot offer a transaction spanning all files and graph operations. A failure can leave staged files or a partially changed editor graph. The editable source remains available; inspect and reconcile the Defold scene before retrying.

## Current compatibility boundaries

- The native editing toolset is available. Rendering every Defold feature identically is separate work: custom materials, stencil clipping, nine-slice scaling, Pie/ParticleFX and template/dependency previews are not yet equivalent to Defold. Font files in the project are loaded when available; unavailable built-in/dependency fonts use a local fallback.
- Template instances and layout overrides currently block publication rather than dropping their data. No Lua business logic is generated or modified.
- Static PNG atlas entries can be imported as editable pixels. Animated entries, tile sources and rename-pattern atlases are not yet editable source imports. Their authored links are retained, with import warnings.
- Changed composite artwork publishes as a Box node backed by a PNG. A text-only edit through the native text tool is kept as Text when its remaining appearance matches the baseline; complex native text styling or raster artwork can require a Box representation. This can affect scripts that expect a runtime Text node. Property-only edits retain the original node kind.
- Publication composites each node independently. Node-local effects and ancestor masks are included; cross-node adjustment/clipping stacks and effects that depend on siblings' backdrop do not yet have full export parity. Keep such effects inside the intended node's artwork group. Cropping published artwork can normalize the exported Box size/pivot/position while preserving descendants' world placement; the native design retains authored coordinates.
- Defold 1.13.1 has an incompatible rotation setter. Online rotation changes and rebuilding a rotated hierarchy stop explicitly. Close that editor and synchronize to disk for those operations.
- A design first opened offline establishes a live baseline at sync only if the disk version and the editor's current nodes still agree. Root resource declarations initially come from disk; unsaved atlas/font registration is not mirrored into PhotoCraft.
- Simultaneous changes to both editors need reconciliation; three-way merging is not implemented. Generated revision assets are retained; there is no automatic garbage collection.
- After an online publication, keep Defold running when reopening the design so PhotoCraft can recognize Defold's subsequent save against the acknowledged live snapshot. Reopening offline immediately after that save can report a disk conflict; reopen with Defold running to validate that baseline. The source is preserved and conflicting files are not overwritten.

The older independent GUI commands remain for compatibility and bridge diagnostics. Normal File → Open no longer enters that separate GUI canvas/tool mode.

## Commands and verification

| Command | Parameters |
| --- | --- |
| `defold.openProject` | `path`: project or game.project |
| `defold.openDesign` | `path`: absolute GUI file |
| `defold.setDesignProperties` | `values`: property/value map for the selected node group or descendant |
| `defold.addDesignNode` | `child`: boolean; optional `kind` |
| `defold.syncDesign` | No parameters; explicit publication |
| `defold.openDesignResource` | `property`: texture, font or material |

All other painting, erasing, layers, text, saving and AI operations use existing engine commands.

Regression coverage includes native painting/erasing/undo, source reopening, painted pixels followed by property edits, child creation/deletion/undo, conflict rejection, native text publication, invalid properties, project picker paths and AI completion after document switching. `defold_native_probe` is restricted to the workspace-owned `plan/defold-live-project/photocraft_live_validation.gui`; it checks native drawing through live texture registration, graph publication, Defold save and source reopening without touching the actual game.


## Manual publication and game texture ownership

Move Auto-Select defaults to **Layer** so a node's artwork remains directly selectable inside
folders. Switch the options bar to **Group** for automatic whole-group selection, or explicitly
select the desired folder in Layers and drag its visible artwork. Selecting a child in Layers
keeps that child when dragging it. The mode is remembered across restarts; Reset Tool restores
Layer. Hovering artwork previews the same target by outline and name. A folder selected merely
by import or an earlier Group-mode click does not prevent returning to individual artwork.

Native GUI edits, moves, brush strokes and undo operate on the local PhotoCraft document. They do not request editor snapshots, encode PNGs or write game assets. Only `defold.syncDesign` publishes; opening a linked source may read the editor baseline and explicitly opening a resource may contact the editor.

Generated assets live under `/photocraft_generated/<gui-name>_<project-relative-GUI-hash>/`. `images/img_<content-hash>.png` identifies the actual PNG bytes, independent of document history, node placement or the checkout location. Identical images from multiple nodes share a file. A GUI publication uses one shared atlas for its changed artwork, with two-pixel extrusion and padding. `atlases/<content-hash>.atlas` changes only when its image set changes. Unmodified imported resources continue using their original atlases.

`manifests/<content-hash>.json` records the source GUI, atlas and node-to-image mapping, dimensions and content hashes. These are immutable publication manifests, not editable source files. The `.gui.pcraft` remains the editable source; original game textures are never overwritten. Unchanged publication reuses existing files without rewriting them. Node movement alone does not create new image versions. Older generated versions are retained for undo, previous GUI references and version-control recovery; do not delete them by age or filename alone. Future cleanup must trace every project GUI/atlas and retained publication before presenting unreferenced resources for explicit removal. Texture compression profiles remain owned by the Defold project.

The scene renderer reuses off-canvas textures during Move previews and patches the union of previous/current bounds, including negative coordinates. A bounded transparent margin avoids reallocating textures for every pixel crossing the old scene edge. Unknown changes, large extent changes and undo retain a full-render fallback. GUI properties cache the parsed source until its source text or native text changes.


## IP binding and remote projects

PhotoCraft and Defold now have a dedicated binding channel, separate from the general control server and the editor's session token. PhotoCraft listens on TCP 17655; Defold listens on TCP 17654. Both accept an IP address with an optional port. `PHOTOCRAFT_BIND_PORT` and `-Ddefold.craft.port` override the respective listeners for multiple instances. This transport is intended for a trusted local network; it uses HTTP and does not encrypt project resources.

In PhotoCraft, open **Window → Defold Sync**, enter the Defold IP and select **Bind Defold**. In Defold, use **File → Bind PhotoCraft by IP**. Unbound applications periodically discover the other application on loopback. Automatic discovery does not take over an application already bound to another peer. A changed binding requires confirmation on the receiving side; each side with open files asks to save or close them. PhotoCraft reuses its per-document Save / Discard / Cancel prompts. Defold provides Save All and Close or closing already saved tabs, and refuses to switch while dirty resources remain. Cancellation, an expired ticket, unfinished workers, a save failure, or an open workspace prevents the switch. The old session is revoked after a successful change.

Defold owns the project. After confirmation it transfers a resource manifest and file contents into a per-project PhotoCraft cache. Every file is checked with SHA-256 before atomic installation. The cache persists across application restarts; editable `.gui.pcraft` files are retained and never downloaded over. Editor tokens, hidden directories, build output, symlinks and native PhotoCraft sources are excluded. Transfer limits are 20,000 files, 64 MiB per resource, 1 GiB per project and 128 directory levels. Files outside the project and path traversal are rejected. Projects depending on excluded directories or external linked files require those resources to be made available as ordinary project files.

The Sync panel lists the transferred GUI resources and also accepts a project-relative GUI path. **Open GUI in both editors** opens the corresponding Defold workspace tab before PhotoCraft imports the design. Reopening a linked native source does the same. Defold's **File → Open GUI in PhotoCraft** sends its active GUI to the bound instance. **Refresh Project Resources** fetches current project files after external resource changes; a newly created GUI may require this refresh before opening in PhotoCraft.

Manual synchronization retains its existing conflict checks. PhotoCraft uploads only immutable files under `photocraft_generated/`; different bytes at an existing path are rejected. It then applies the GUI through the bound Defold editor. Remote caches always require their live binding: they never silently fall back to offline publication into the cache. Save the edited GUI in Defold after publication as before.

Validation: desktop application compilation, isolated production Rust transport tests, production Clojure route tests with a headless UI adapter, cross-language HTTP binding in both directions, project download / digest checks / generated asset upload / session revocation, and an offscreen PhotoCraft panel capture. Full Defold UI interaction, cross-machine firewall behavior and incremental patch installation still require a running editor environment.
