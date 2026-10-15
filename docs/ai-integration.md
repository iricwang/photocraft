# Native AI image generation (P1)

PhotoCraft remains Rust-only/native egui. AI Studio now connects to real image providers.
The P0 notes below describe the earlier deterministic fixture, not the current default UI.

## Compact Composer And Model Dock

Updated 2026-10-13. AI starts enabled and the prompt composer starts visible. A deliberate
AI-off choice is saved in provider preferences. Collapsing the composer leaves an AI restore
button and does not disable AI, cancel work, clear drafts/references, or remove candidate previews.

The composer has three permanent rows: AI switch and generation-mode segments; a multiline
prompt; references, target layer, status, and the primary generation action. Narrow windows
wrap the mode row and keep status beside the AI switch. Attached references and result summaries
appear only when present. Result summaries open a bounded, scrollable popup with full messages,
saved outputs, candidate review, and generation history. Clicking inside keeps it open for
comparison; clicking outside or pressing Escape closes it. Switching mode while viewing a history record starts a new
generation and retains the current prompt. Legacy component options remain available in the
reference menu's Advanced section where the existing provider/retry path supports them.

Model settings opens from either the composer sliders button or the right icon rail. Its initial
flyout sits beside the rail without moving Color, Properties, or Layers. Drag its grip/title to
float or dock it using the normal panel docking system. Hiding and restoring preserves placement
and values. Provider and model remain visible; Connection settings contains the executable path,
reasoning effort, or API endpoint/key. Opening or hiding settings never changes AI enablement.
Configuration editing is disabled while generation runs; model-discovery errors remain visible.

`scripts/ai-layout-preview.json` drives the actual UI in the offscreen snapshot example without
calling an image provider. `ai.configure` exposes `enabled` and `visible` independently;
`window.panel.aiSettings` and `window.toggle.aiSettings` control model-panel visibility.

## User Reference Images

### Aspect-Preserving Placement And Reference Intent

Updated 2026-10-14. Whole-image repaint results use one uniform scale and are centered inside
the original canvas. A mismatching output ratio never stretches, crops, resizes the document,
or starts another generation. New AI layers leave uncovered margins transparent; candidate
edits preserve the original pixels there. Selected repaint results keep the square-context
contract and selection mapping, including feathering. Isolated components keep their existing
alpha-aware fitting. Receive receipts expose `placement.fit`, `sourceSize`, `hasMargins`, and `bounds`
(`[x, y, width, height]` in original document pixels); history retains the original target area.
No automatic crop/fill decision is inferred when the user asks for another output ratio.
The completion message explicitly warns when the original canvas remains visible around a
fitted result. This is lossless placement, not a guarantee of a seamless replacement: source
headers or footers can remain visible in those margins.

Both Codex roles receive a shared edit-intent policy in the acknowledged developer item.
The agent interprets the current request to distinguish source artwork from composition,
style and object references. A reference does not automatically supply all three. It separates
requested changes from requested preservation using the inspected source and current user
words, without inventing style directions, unseen interaction behavior, or permanent layout
locks. Explicit latest requests override previous assumptions. The raw user message is not
rewritten. Only input-role, user-requested preservation, target geometry and output notes may
be clarified for the image tool; this is not a hidden aesthetic prompt optimizer.

An unresolved essential conflict is returned as a concise question without an image call.
PhotoCraft now displays the actual text-only agent reply instead of guessing that quota is
missing. It does not retry, fork, archive or change the persisted conversation. Semantic
interpretation is model-driven, not a deterministic classifier: offline policy and protocol
tests verify delivery, not visual accuracy. A new paid comparison needs explicit approval.

The opt-in engine test `replay_existing_image_with_aspect_preserving_placement` reads a local
PSD and an already-generated image, saves a separate composite for visual inspection and
asserts the PSD is unchanged. It never connects to a model or consumes generation quota.

Codex user messages no longer append the serialized project summary, layer lists, operation
history or old failure messages on every turn. Normal generation sends the user's original
text, one short label per actual reference image, the typed images and a typed Imagegen skill
attachment. Target-area and requested-transparency constraints remain where applicable; no
style or layout preference is inferred from a reference label. Multipart providers receive a
short input-role note because they cannot carry separate text labels beside each image.

Project persistence and the original conversation ID are unchanged. The read-only
`photocraft_canvas.project_context` tool returns the frozen current state and recent operations
on demand, without image pixels. `canvas_snapshot` independently returns the visible canvas
without repeating that history. Both resolve the same expiring request lease, reject other
project/file selectors and cannot mutate a document. Asset mode continues to expose paged
`project_history`. Existing messages already in a conversation are not deleted or rewritten.
Updating the stable helper/tool catalog requires the shared desktop's normal manual restart.

Canvas discovery is conditional but not merely optional for edits: when the current request
depends on existing artwork or asks to preserve its elements/style/layout, the agent must call
`canvas_snapshot` before generation or asking the user for missing target pixels. Unrelated
new images and reference-only standalone assets do not trigger that read. Only an unavailable
or failed tool justifies requesting a screenshot, with that failure explained. This policy is
shared by the MCP server instructions, thread setup and each generation turn, not appended to
the user's message. Persisted conversations can retain their original developer message even
after a resume with new settings. Both renderer and asset director therefore send current task
instructions as a developer message using `thread/inject_items` and wait for acknowledgement
before `turn/start`. A rejected/malformed acknowledgement, cancellation or concurrent turn
prevents generation. `collaborationMode.settings.developer_instructions` is also set consistently,
but is not sufficient by itself: the real persisted conversation retained stale instructions
when its mode stayed `default`. The selected model and reasoning effort are preserved. This is
an append to the original conversation, not a new conversation, automatic image attachment, or
generation retry. See
the [App Server turn contract](https://learn.chatgpt.com/docs/app-server#turns).
The mock-provider bridge probe verifies the policy appears in model-visible developer messages
when continuing a conversation with legacy instructions (not merely the MCP catalog), and drives an
actual MCP call, then checks the next model request contains a typed image that was absent before
the call. This proves transport/tool execution, not a real model's choice to call the tool.
Server-level workflow instructions follow the [OpenAI MCP server guidance](https://developers.openai.com/plugins/build/mcp-server).

Real acceptance (2026-10-14): one explicitly approved image-generation turn used the original
saved PSD, its embedded reference, unchanged user prompt and original conversation. The agent
called `canvas_snapshot`; the model-visible result contained the exact source PNG (matching
SHA-256), then Imagegen was invoked with both images. No retry, fork or PSD write occurred.
The generated artwork retained recognizable source elements, but changed the four-column
card battle into a three-column perspective arrangement; input transport passing does not
certify visual fidelity. The read-only PSD harness does not validate unsaved UI state or import.
`scripts/verify_existing_canvas.py --audit-only --out <receipt-dir> --rollout <original-rollout>`
checks this evidence without model calls. MCP summary events can omit image blocks even when
raw model-visible tool outputs contain them; acceptance must audit the latter. The opt-in
`codex_real_existing_canvas_acceptance` test requires explicit approval and refuses a second
submission in the same output directory. Private PSD/reference/result files remain untracked.

The floating prompt's image button offers **Choose reference image** and **Paste reference
image**. Ctrl/Cmd+V with focus in the prompt attaches an image; ordinary text paste stays text.
Up to four project-owned references appear as removable, aspect-preserving thumbnails.
Adding/removing a reference is an undoable metadata edit, marks the project dirty, and never
adds image layers. `.pcraft`, PSD and PSB embed the normalized PNG bytes. Reopening restores
them without the original source file or clipboard. Closing the prompt leaves them intact.

Native file reads/decoding run off the UI thread. Inputs are limited to 32 MiB, 16 megapixels
and 16,384 pixels per axis, converted through the editor's color management to sRGB PNG,
proportionally reduced to a 1,536-pixel longest edge, and stripped of original metadata and
directory paths. PNG, JPEG, WebP, BMP and TIFF are offered by the picker. Clipboard images
use the existing OS image service. Attachments cannot change while a request is running.

The Codex asset director receives typed image inputs along with the prompt, not just file
names or descriptions. The same immutable images also reach the Imagegen generation turn.
Canvas context remains separate. The Images API path appends them as
multipart images, including when no canvas reference is enabled. Changing attachments
invalidates the previous asset task's resume identity; switching document tabs does not. No extra image-generation call is
introduced by attaching references.

Regression coverage includes input limits, alpha and 8/16/32-bit normalization, metadata
stripping, clipboard focus/text behavior, unchanged layers, project round-trips, request snapshots, task
invalidation, typed Codex images, multipart uploads and compact/wide floating-bar layout.
Live model interpretation and native macOS clipboard behavior require separate live testing;
the automated suite does not spend image-generation quota.

## On-Demand Canvas MCP

Updated 2026-10-13. The Codex renderer now exposes the read-only
`photocraft_canvas.canvas_snapshot` tool, including ordinary repaint requests, not only the
asset-placement agent. The agent decides from the dialogue whether it needs the current
artwork. It is not required to call the tool before every generation, and attaching an external
reference does not itself require a canvas read.

The host prepares a local immutable snapshot of the complete visible-layer composite at request
start. Ordinary repaint does **not** append this image to the default model inputs. Only a tool
call returns it as a native MCP `image/png` image block, with original document size, revision,
selection/target and coordinate metadata. Preview dimensions preserve aspect ratio and are
bounded to a 1536-pixel longest edge. The snapshot is separate from user-attached references,
optional active-layer references and the existing selected-area edit frame. Tool access does
not change the user prompt, requested style, output transparency or placement geometry.

The renderer MCP runs over stdio as a task-scoped PhotoCraft helper. It exposes one tool with no
arguments: no document switching, arbitrary paths, mutation or extra generation. A private
temporary snapshot file is removed after the request. New generation history retains the frozen
canvas so retry does not substitute a later generated layer or another open document. Full-canvas
capture is bounded to 64 megapixels. References and the canvas are source material, not instructions.
Existing transparent-asset and selected-frame workflows retain their explicit image inputs.

Tests cover ordinary repaint with an external reference but no automatically attached canvas,
visible lower layers beneath an empty active layer, 8/16/32-bit snapshots, retry consistency,
native MCP image blocks and rejection of foreign-path/document or write requests. No paid image
generation is required for these checks; live model choice and visual quality need a separate run.

## Project Memory And Prompt Policy

Implemented 2026-10-13. The first reference attachment or generation assigns a random project
identity. Subsequent committed edits, selected-layer changes, undo/redo and AI requests/results
are recorded without keys, raw image bytes or arbitrary command parameters. The document stores
the last prompt and a bounded ring of 256 events; reference limits are four PNGs, 8 MiB each.
Unlinked documents retain their existing edit/history semantics. Hover and view-only actions
are not journalled, and activity before the first AI link cannot be reconstructed.

The native host writes `project-context.json` and an append-only, rotated `operations.jsonl`
under a dedicated `.photocraft` workspace beside a saved document (application data for unsaved
documents). Writes run on a bounded background queue and drain at normal shutdown. Before a
generation the queue is flushed; disk errors surface instead of silently dropping context.
Recent events accompany each request; the restricted `project_history` MCP tool can page through
the local journal using its returned byte cursor. Recording edits does not call a model.

On generation, PhotoCraft creates the workspace automatically, even for an unsaved document.
New saved-document workspaces use a sanitized document filename; unsaved ones use
`Untitled-<short identity>`. No manual directory creation or path entry is required.
The installed Codex desktop is opened through its normal `codex://new?path=...` folder route,
without a prompt. Its normal folder-access consent remains required. The worker waits up to
90 seconds, cancellably, for a visible desktop project and its app-server mapping before
starting a model turn. Timeout, missing desktop support or incompatible desktop state is an
explicit error, not a silent server-only project. The desktop projection is a bounded,
read-only compatibility adapter; PhotoCraft never writes Codex global state or databases.

Codex `project/read` verifies the registered project's workspace before using it.
Older server-only bindings retain their thread IDs. Before migration, `thread/read` checks
that the thread still belongs to the known old project, then `thread/metadata/update` moves
it to the visible project. A thread manually moved elsewhere is not taken back. Migration
and readable conversation naming do not consume image quota.
Asset orchestration and image rendering have separate durable conversations in the same project;
later requests use `thread/resume`. Saved PSDs never contain Codex thread IDs or local trusted
bindings. The host keys bindings by canonical full document path and project identity, so a copy
or Save As does not silently inherit another file's conversation. The first save of the same
currently open unsaved document retains its workspace, conversations and operation history;
subsequent Save As paths remain isolated. Existing workspace roots and desktop labels are
preserved for compatibility: saving an initially unsaved document does not automatically rename
an already registered desktop project. Session replay is explicitly
marked when an older document is reopened; sequence rollback does not discard new operations.
Moving files to another machine retains embedded references/recent memory, not the old machine's
trusted Codex conversation or full local journal. Preserve the companion workspace when moving
complete local archives. Other PSD editors may discard unknown plugin metadata when resaving.

## Same-Conversation Ownership

Updated 2026-10-13. Each document's registered project/workspace and existing role conversation
IDs remain stable across generation attempts. A writer conflict must not clear the trusted
binding, start a replacement conversation, fork history, or automatically retry a paid turn.
`thread/resume.excludeTurns` only omits history from the RPC response; it does not reset model
context. Keeping a project ID alone is not a substitute for retaining the conversation ID.

Verified on the installed Windows Codex: opening a PhotoCraft conversation in the desktop can
load it as the desktop's writer. A separate PhotoCraft app-server then cannot resume it, even
when its last turn is completed. EOF shutdown of PhotoCraft's own server cannot release another
server's writer. PhotoCraft now closes stdin, drains output, and waits up to three seconds for
normal cleanup before terminating only its own unresponsive child. Completed images are kept.

Historical investigation verified archive/restore once with user approval, but the user rejected
it as a recurring workflow. It is not the product fix and must not be automated or recommended
for each generation. The built-in shared connection below replaces separate backend ownership.
Simply switching tabs is not a reliable release, since unsubscribed threads have an inactivity
grace period. See the official [app-server lifecycle](https://learn.chatgpt.com/docs/app-server)
and [SessionEnd hooks](https://learn.chatgpt.com/docs/hooks#sessionend).

Actual desktop acceptance remains pending, with this implementation contract:

1. Keep one persistent document subproject and its original design conversation; never branch as
   a conflict workaround. Preserve references, generated assets and the operation journal there.
2. Route PhotoCraft and Codex desktop writes through one supported shared app-server owner, or
   an explicit supported idle-session handoff. An independently launched daemon is not that owner.
3. Serialize requests for the same conversation, attach immutable canvas/reference snapshots,
   and match completion to the accepted turn before importing layers. Never replay a submitted
   paid turn after a disconnect without first reconciling its status/output.
4. Accept only after consecutive generation, desktop-open-between-generations, PhotoCraft restart,
   cancellation, and reopen-saved-PSD all keep the same original conversation and project.

The currently running desktop exposes neither a usable shared control socket nor an immediate
idle-writer release API on this machine. Keeping an independent PhotoCraft server alive would
merely reverse which client encounters the conflict. Do not patch Codex state/lock files or
terminate unrelated desktop processes. The opt-in `codex_real_same_thread_resume_smoke` verifies
repeated resume of an explicitly approved existing thread without consuming image quota.

### Built-In Shared Connection (Restart Verified)

Further inspection of the installed desktop found `CODEX_CLI_PATH` and
`CODEX_APP_SERVER_WS_URL` transport overrides. These are shipped implementation details, not a
stable public desktop integration contract. The user approved built-in setup on 2026-10-13, with
explicit notification before a manual Codex restart. The component ships with PhotoCraft and
provides reversible setup rather than a machine-specific
binary patch. New machines generate their own loopback endpoints and authentication material;
credentials, port numbers and absolute installation paths must not be synchronized.

The isolated `scripts/probe_shared_codex.py` test uses a temporary CODEX_HOME, no copied credentials,
an authenticated loopback app-server and a localhost mock model provider. With CLI
`0.162.0-alpha.17.2`, it established:

- Two independent WebSocket clients can resume the same persisted thread ID without a writer
  conflict. Unauthenticated connections are rejected with HTTP 401.
- Resuming an already loaded, even idle, thread ignores model, developer-instruction and MCP
  configuration overrides. A later mock request still uses the original model, instructions and tools.
- A second `turn/start` during an active turn succeeds with the existing turn ID and steers it.
  A PhotoCraft-only idle check therefore cannot guarantee a separate generation.

Consequently, a direct WebSocket adapter alone is NOT an acceptable fix. The bridge must own or
arbitrate both desktop and PhotoCraft request paths, retain one actual app-server writer, and
atomically serialize submissions per thread. It must route RPC responses to their originating
client and correlate notifications, cancellation and imported images with the acknowledged
thread/turn pair. Never automatically replay a possibly accepted paid request after disconnect.

MCP resources must become stable project-scoped endpoints that resolve the active immutable task
snapshot, instead of depending on per-request temporary files or random ports cached by a loaded
thread. The initial thread load, including a desktop-first load after restart, must install the
correct tools. Per-turn model and reasoning choices must be sent through supported turn settings;
resume overrides cannot be used to pretend that these or tool permissions changed.

Implementation order: stable task-context MCP broker; authenticated transport and two-client
arbitration; PhotoCraft routing and uncertain-submit reconciliation; reversible portable setup;
then actual desktop acceptance. Test desktop-open-between-generations, concurrent submission,
reconnect, fresh snapshot/changed references, model changes, cancellation, saved-PSD reopen and
desktop/PhotoCraft restart. Verify unchanged thread/project IDs and no automatic paid retry.
An isolated protocol probe is not desktop acceptance or proof of image quality.

Implemented in `apps/photocraft/src/codex_bridge/` and `codex_shared.rs`:

- `photocraft-codex-bridge` wraps the desktop's real CLI via `CODEX_CLI_PATH`, preserving its
  arguments/environment and one stdio app-server. Other CLI operations pass through unchanged.
- PhotoCraft uses `--photocraft-client` and an authenticated, numeric-loopback JSONL endpoint.
  Enabled shared mode never silently falls back to a second independent backend.
- Request IDs are multiplexed, replies routed to callers, active turns atomically reserved before
  forwarding, and cross-client mutation refused. Disconnects do not release an active turn.
- Per-project stable canvas/asset MCP helpers reread bounded, expiring, locked task leases for
  every call. No active request means no canvas/asset access. Canvas remains fetched on demand.
- Existing trusted role bindings are registered before desktop restart; first resume from either
  client receives the tool template. No thread/project ID is substituted. Model and effort are
  submitted with each turn instead of relying on ignored resume overrides.
- Windows setup copies a versioned helper, protects its local directory, backs up the previous
  user environment setting and creates a dedicated launcher. It does not alter Codex binaries,
  state databases or active processes. Disable restores the previous setting. Windows packages
  include the component/setup scripts; macOS bundles/signs the helper but setup is not yet verified.

`scripts/probe_codex_bridge.py` passed against the real installed CLI and a credential-free local
mock provider: persistent thread survives bridge restart; desktop-first resume installs live canvas
tools; another client resumes the same ID; three sequential mock turns retain it; concurrent start
is rejected; per-turn model updates work; wrong authentication is rejected; desktop EOF clears the
endpoint and owned backend. This does not exercise paid image generation or the actual desktop UI.

Deployment on this Windows machine: installed after the user closed PhotoCraft on 2026-10-13.
All three existing role mappings retain identical file hashes. The per-user CLI override, protected
versioned component and `Codex - PhotoCraft Shared` desktop shortcut are present. No running
Codex process was closed. A manual full quit/relaunch through that shortcut and original-thread
acceptance are still pending; configuration installation alone does not resolve the running
desktop's old backend ownership.

After the user's restart, the actual desktop-owned bridge was observed running and the original
design thread resumed twice successfully from PhotoCraft without creating a thread or model turn.
The user also reported a black main window: the launcher incorrectly requested Hidden for Codex
itself. It now requests Normal, hides only the PowerShell helper, and reports launch errors visibly.
The user subsequently confirmed the main window is normal after restarting through the corrected
launcher. Process inspection verified one desktop-owned bridge and exactly one real backend.
The original design thread resumed twice again through successive PhotoCraft client connections
without a writer conflict. Actual paid generation/import, cancellation and saved-PSD reopen
remain separate acceptance checks; this test did not submit a model or image turn.

A subsequent real request exposed a Windows socket-clone timeout defect: the authenticated
reader retained its three-second handshake timeout even after the other handle cleared it.
The backend continued the original turn while PhotoCraft lost its connection. Both actual
reader handles now explicitly clear their receive timeouts after authentication. Regression
coverage waits eight seconds for mock inference and round-trips a 2 MiB frame after idle.
Transport EOF is reported as an uncertain connection loss, not evidence of sign-in failure;
no automatic retry, replacement conversation or archive/restore is performed. The installed
desktop backend must be manually restarted after component updates to load this fix.

Portability of the bridge is separate from portability of conversation history. Running on a second
computer does not automatically synchronize the original local thread; continuing that exact
thread cross-device requires an explicitly selected shared execution host or supported migration.
Do not copy live Codex databases, lock files or authentication records. Existing asset-director
and renderer role bindings must not be silently merged: nested rendering currently uses a separate
role, and an eventual single-turn asset pipeline needs its own migration and acceptance tests.

## Codex Reasoning Effort

The Codex configuration includes model and reasoning-effort selectors. The effort list is taken from
the selected model's `supportedReasoningEfforts`; Automatic uses its reported default.
Model-list pagination, including hidden models, is retained. When no model is explicitly
selected, both UI and host choose the reported default, falling back to the first model.
Changing models resets an incompatible effort to Automatic after metadata loads. The choice
persists as `codexReasoningEffort` and is also configurable through `ai.configure`.
Both asset and rendering turns refresh capabilities, reject stale model/effort combinations,
and pass the resolved value through `turn/start.effort`. There is no hard-coded effort ladder
or silent downgrade. The setting affects the Codex agent's reasoning, not image dimensions
or a guaranteed visual-quality level.

## Prompt Policy

The current user prompt passes unchanged to Imagegen. Automatic art-direction/optimization
turns are removed. Application instructions only identify the target/selection, image roles,
required pixel geometry and transparency. There are no default material, lighting, camera,
bevel, 3D intensity or layout embellishments. The asset MCP rejects rewritten prompts before
reserving quota. A whole toolbar stays one asset; `separateAssets` is only for a user-requested
component set. One image call remains the default; the explicit legacy two-step option remains
available on its existing path. Existing old optimized prompts are historical data, not new
instructions. These changes remove conflicting guidance but do not guarantee a model's design
quality; live image comparisons remain a separate, quota-consuming validation.

Workflow research: [Krita AI Diffusion](https://github.com/Acly/krita-ai-diffusion) separates
selection-based edits and visual guidance; [InvokeAI Canvas](https://invoke-ai.github.io/InvokeAI-7/users-guide/canvas/introduction/)
distinguishes raster content, inpaint masks and regional prompts/references. PhotoCraft adopts
that separation of responsibilities, not their presets or diffusion backends. No third-party
source code was copied and no ComfyUI installation is required.

## Legacy Component Sets Prototype

The fixed-grid implementation below was not released before the MCP workflow was selected.
Its advanced controls remain on the Images API path only; Codex new tasks use the agent path.
The user selected the agent-driven MCP workflow in the next section instead of mandatory
component lists. A real single-call atlas contained all ten player controls with real alpha,
but did not follow exact cell boundaries, so the fixed-grid validator rejected splitting.
Fixed-grid splitting and manual lists are not the default Codex interaction.

In **Transparent component** mode, enable **Component set (1 image)** in the settings.
The player preset lists eight buttons and two tracks. Each line is a separate asset;
names can include states such as `Play - pressed`. Sets accept 2-12 unique names, with
at most four tracks. The source selection and full interface are style references only.

The provider receives one transparent atlas request with deterministic grid coordinates,
not one request per component. The two-image design/extraction option is bypassed and
hidden for sets. The user's prompt is unchanged; image generation is called once.
There is no automatic paid retry. Actual provider billing is still provider-defined.

The original atlas and a manifest are saved in the generation artifact folder before
validation. Local processing rejects opaque, empty, wrong-aspect or boundary-crossing
cells rather than silently cutting attached controls. Successful cells become tight
transparent `component-01.png`, `component-02.png`, etc.; the manifest maps names to files.
No user-entered name is used as a filesystem path.

The engine opens a new asset document at the atlas's actual resolution with one expanded
group of named ordinary raster layers. It does not scale the set into the selected bar
or change the source document. The group is one undoable edit and survives `.pcraft`
save/reopen. Sets currently use ordinary document history, not the single-component
generation-version selector. Return to the source document to generate another set.

The model must obey the requested grid and symbols; geometric validation cannot prove
that every symbol matches its name. A failed split keeps the atlas available for manual
recovery, without another image call. macOS compilation requires a macOS host.

## 智能体与 MCP 资产工作流

确认日期：2026-10-13。主流程改为“用户自然语言描述 → 智能体读取画布并理解需求 →
一次生成整套透明资产 → 检查实际生成图 → 通过 MCP 调用 PhotoCraft 拆图、建层、排版”。
不要求用户先勾选控件、填写组件清单或设置固定网格。下面保留完整目标方案；
当前实现边界与验证记录见本节末尾，未完成项不代表已经接通。

### 产品交互

- 保留画布底部自适应输入条。配置按钮主要收纳服务商、模型、连接与高级选项。
- 用户可以直接输入“把播放器底部改得更 3D，按钮和进度条分别做成图层并摆回去”。
- 智能体结合当前文档、可见图层和选区判断是局部修改、单组件生成还是多资产生成。
  选区是定位线索，不是必须操作；目标唯一时自动定位，存在多个播放器等歧义时才询问。
- 组件名称、数量、状态和布局由智能体提出。详细清单可以展开检查或修改，但不是必填表单。
- 按钮名称带状态，例如播放、暂停、收藏未选中和已选中。互斥状态不同时覆盖显示；
  未启用的状态放在同组隐藏图层或独立素材页中，仍需保存和导出。
- 进度条默认作为独立完整资产；用户要求可组装控件时，才规划轨道、填充、滑块等子资产。
  第一版交付透明像素图层，不宣称是可编程控件或矢量组件。
- 执行中显示识别需求、生成素材、检查拆分、排版预览、完成等状态；可以取消。
  关闭输入条不丢弃已经生成的图片。

### 分工与执行顺序

1. PhotoCraft 建立任务快照，返回文档 ID、修订号、原始尺寸、色彩信息、图层树、
   可见合成图、目标区域特写及参考图到文档像素的坐标变换。参考图缩小不改变落层坐标。
2. 智能体输出有结构的资产与布局意图，包括组件语义、状态、父子关系、参考对象和目标锚点。
   它可以调整内部控件层级和间距，不再用旧按钮尺寸或固定网格锁死设计。
3. 调用现有生成服务，原样传入用户提示词，一次输出所需透明 PNG。只有用户要求多个独立
   组件时才分开并留透明间隔；完整底栏或单个物体保持完整。不追加材质、光照、角度、
   立体程度等审美要求。整图与特写的用途单独说明，不把参考整图误当成输出整张界面的要求。
4. 保存实际返回的原始 PNG，并将其作为新资产交给智能体查看。所有拆分坐标必须基于这张
   实际图片，不能直接采用生图前计划里的格子坐标，也不能只相信模型文字声称的尺寸。
5. 智能体识别实际的播放、暂停、前后切换、循环、收藏、音量和轨道等资产，调用应用提取。
   应用结合区域、真实 alpha 和必要的蒙版细化边界，保留阴影、抗锯齿和透明孔洞。
   连通域只提供候选，不把一个按钮的图标、边框、断开的阴影误拆成多个资产。
6. 根据原界面的语义锚点安排位置，默认等比缩放，保持图层顺序和间距，检查邻近文字、
   歌曲信息与封面的遮挡。播放与暂停等变体必须区分默认显示状态。
7. 通过只读合成预览检查整体效果和每个提取结果。高置信度、无破坏性操作时直接完成；
   目标不清楚或涉及覆盖、删除原内容时再询问，不把每一步变成确认对话框。
8. 通过一个原子批次创建具名图层组，结果立即成为工程内容；一次撤销可移除本批次。
   保存并重开仍保留独立图层，同时保留原始资产图、提取 PNG 和语义清单供单独导出。

### MCP 与应用接口

复用 `crates/automation` 的现有 MCP bridge 和引擎命令，不另起一套文档模型。
现有 `doc_inspect`、`doc_place`、`doc_export_layers` 和 `command_run` 可作为基础，
但不足以提供任务快照、精确拆分和原子批量落层。新增的工具名暂定如下：

| 工具 | 关键输入 | 返回值和边界 |
| --- | --- | --- |
| `design_context` | documentId、可选选区或目标图层 | snapshotId、revision、画布像素尺寸、图层树、合成图与特写、坐标映射；不修改文档 |
| `asset_inspect` | 受信任的 assetId | 实际宽高、alpha 统计、预览、可选边界候选；不能读任意文件路径 |
| `asset_extract_batch` | assetId、items 数组；每项含语义名称、状态、实际像素区域、可选 maskId、透明留边 | 独立 assetId、真实边界、裁切偏移、预览及警告；不调用生图、不修改源像素 |
| `layers_preview` | snapshotId、目标 documentId、完整图层摆放清单 | 合成预览、越界和遮挡检查、planId；没有文档副作用 |
| `layers_apply_batch` | planId、expectedRevision、idempotencyKey | 文档 ID、图层组 ID、每个图层 ID、批次记录和新修订号；一个事务写入 |
| `generation_status` | jobId | 当前阶段、已发生的生图调用数、原图与拆分资产清单、可重试阶段；不暴露凭据 |
| `project_history` | offset、limit | 当前绑定工程的操作记录与 nextOffset；只读，不能访问任意路径 |

资产登记由 PhotoCraft 的受信任生成适配器完成，模型只取得 assetId，不自行提交任意
磁盘路径作为生成结果。`asset_extract_batch` 的区域采用实际 PNG 像素坐标，明确包含
左上角与宽高；边界越界、空裁切和无效蒙版返回可解释错误，而不是截断后继续。

摆放清单至少包含 `assetId`、`name`、`parentId`、`state`、`visible`、`opacity`、
`blendMode` 和 `transform`。变换明确采用文档像素中的 x/y、等比缩放和旋转，不混用
窗口坐标、缩放后截图坐标和原始画布坐标。返回每项最终边界，便于智能体再次检查。
所有数据使用结构化 schema、数量和内存上限，并经过引擎验证。

MCP 是工具通道，不负责图像理解，也不会把单纯的 Images API 自动变成智能体。
Codex 路径接入上述受限工具；其他图片服务仍负责生成，另由支持看图和工具调用的
智能体负责规划、识别与排版。沿用现有 Imagegen skill 接入方式，不假设桌面聊天中的
内置生图工具可以直接作为第三方 API 调用。

### 安全与额度

- 默认任务预算为一次生图请求。需求理解、看图识别、本地拆图、排版与重新预览均不得
  触发第二次生图；智能体的文本和看图推理仍可能产生相应费用，服务商计费规则另算。
- 生图预算由宿主调度器记录并限制，不能只靠提示词要求“调用一次”。生成结果登记后，
  拆图与排版阶段不再提供生图能力。网络超时先核对任务状态，不能盲目重发付费请求。
- 缺件、粘连或符号明显错误时，先基于现有 PNG 调整识别区域、蒙版或局部提取。
  无法修复则保留结果并告知具体问题；追加生成必须明确说明并取得用户同意。
- 只向任务内智能体暴露这组限定工具与资产。不直接开放全部 `command_run`、文件删除、
  原工程覆盖保存或任意 shell。MCP bridge 继续使用本机连接和任务级授权。
- 批量写入前核对目标文档与修订号。用户切换、关闭、缩放画布或编辑后，先重新读取上下文，
  不向当前活动标签页盲目落层，也不因冲突重复生图。任务取消不删除已产生的资产。
- 批量落层必须全成功或全失败；重复的幂等键返回原收据，不产生重复图层。
  撤销只针对本批次的合法历史位置，不能撤销期间用户做的其他编辑。
- 不自动删除、涂抹或隐藏用户原始图层。若源图已压平，旧按钮可能透过新贴图显露，
  单靠“生成透明资产并叠上去”不能解决；单独提出非破坏性遮盖或背景修复方案。

### 实施顺序与验收

| 阶段 | 实现工作 | 通过条件 |
| --- | --- | --- |
| A 资产工具 | 任务快照、原图登记、实际边界与蒙版提取、独立 PNG | 使用已生成的播放器图集拆出 10 个独立资产，无邻件残留、无新增生图 |
| B 图层事务 | 图层预览、具名分组、定位与原子批量提交 | 在原工程副本按语义放置；播放/暂停状态不叠放；一次撤销；保存重开一致 |
| C MCP 接入 | 将 A/B 引擎能力包装为窄接口，接入受限智能体会话 | 智能体能读取上下文、检查实际 PNG、调用拆图、看预览并落层，工具失败可诊断 |
| D 自然语言入口 | 从 prompt 生成结构化意图与资产计划；移除默认手填列表要求 | 只输入播放器需求即可完成，目标明确时不要求勾选或手写坐标 |
| E 单次生图闭环 | 宿主额度门控、取消、超时与幂等恢复、错误处理 | 正常一次生图；拆分或放置失败重试仍只使用同一张图；额外生成必须获准 |
| F 发布验证 | Windows/Mac 桌面路径、回归、截图、真实工程验收 | 原 PSD 未变化；独立图层可选中移动和导出；不同 DPI、画布比例不导致拉伸或错位 |

优先复用 `target/ai-p0/component-set-live/app-data/AI-Generations/generation-UDButs/atlas.png`
作为离线验收样本。该图为 1254×1254，角落和间隔 alpha 为 0，包含 8 个按钮和 2 条轨道；
固定网格拆分失败不意味着这些资产不可用。第一阶段不再重复生图。

布局验收同时使用原播放器的全图与底栏特写。自动识别的语义、分组、透明边缘和遮挡
需要视觉检查，不能只以接口成功或 alpha 非零判定“效果正确”。第一版不承诺模型每次
都画出全部资产，必须明确报告缺件、重复、符号不对与无法可靠分离的情况。

### 首版实现范围（2026-10-13）

- 入口为 Codex 的“透明组件”新任务。只填自然语言需求即可，无须选区和组件清单；
  原位重绘、旧单图历史重试及 Images API 暂不切换为智能体。
- 独立的任务 MCP 服务使用本机随机端口、随机 token 和 stdio 桥接，不修改用户全局 Codex 配置。
  只开放下表的 7 个工具，不开放任意文件路径或通用引擎命令。
- 生成沿用显式加载 Imagegen skill 的适配器。宿主先预占一次图片请求预算，
  再调用服务；超时或失败不自动重发。文本规划、看图和工具调用仍消耗模型额度。
- 原图、提取 PNG、拆图清单、布局清单和合成预览保存在配置目录的
  `AI-Generations/agent-*` 中。文件名由宿主生成，不使用模型指定的路径。
- 原图登记后，通过实际 alpha 候选和视觉判断确定语义裁切；当前只支持矩形区域、
  自动透明边缘裁切、等比缩放和可见性。尚不支持粘连素材蒙版分割、旋转、任意父子结构、
  独立混合模式/不透明度。无法可靠拆分时返回错误，不假装成功。
- 批量结果是原文档里的一个普通图层组，一次撤销/重做，保存后保留独立像素图层。
  幂等收据避免重复提交；文档修订冲突时不落层、不自动覆盖。旧版生成历史选择器不管理此组。
- 同一文档、同一提示词再次点击生成，会继续尚未完成的任务，刷新源图预览并保留已有素材及
  已消耗预算，重新排版而不重复生图。修改提示词或切换文档后点击生成是新任务。
  取消保留已接收素材；应用重启后只保证磁盘资产仍在，不自动恢复内存任务。
- 旧图被压平时不自动抹除旧控件；遮挡与排版由智能体检查合成预览，尚无几何语义遮挡保证。
  当前画布上下文提供根图层元数据，完整嵌套图层树和更丰富锚点接口仍待补充。

### 首版 MCP 合约

所有工具绑定当前任务，模型不能传入其他文档 ID、任务 ID 或文件路径。
图片作为 MCP image 内容返回，坐标、收据和错误作为结构化 JSON 文本返回。

| 工具 | 实际输入 | 实际输出 |
| --- | --- | --- |
| `design_context` | `{}` | 文档 ID、revision、原始尺寸、选区矩形、根图层、整图/特写及坐标映射 |
| `generate_assets` | `{prompt}` | 资产 1、实际尺寸、真实 PNG 和 alpha 候选；最多 1 次图片请求 |
| `asset_inspect` | `{assetId}` | 实际 PNG、尺寸和候选矩形；候选不是最终语义组件 |
| `asset_extract_batch` | `{assetId:1,items:[{name,rect:[x,y,width,height]}]}` | 独立资产 ID、名称、尺寸、原图裁切区域 |
| `layers_preview` | `{items:[{assetId,name,x,y,width,visible}]}` | planId、expectedRevision、最终矩形和合成预览；等比缩放 |
| `layers_apply_batch` | `{planId,expectedRevision,idempotencyKey}` | 文档/组 ID、组件数量和保存为图层的收据；全批次一次撤销 |
| `generation_status` | `{}` | 图片请求次数、所有已有资产 ID/名称/尺寸、提交收据 |

限制：原始图集最多 4096 单边、4 MP；每批最多 24 件，任务最多 49 个资产/16 MP，
落层每批最多 16 MP。裁切越界、空区域、切断明显边缘、两个裁切共享可见像素均报错。
`ai.assetBegin/Register/Reserve/Rebase/Cancel` 是宿主生命周期命令，不向智能体开放。
`Rebase` 只允许在原文档显式重试时刷新快照，清除旧布局但保留资产和预算。

MCP 配置参考 [官方 Codex MCP 文档](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)
和 [配置项说明](https://learn.chatgpt.com/docs/config-file/config-reference)。

### 真实验证记录

2026-10-13 已使用现有播放器图集跑通真实 Codex → stdio MCP → 本机鉴权桥 →
PhotoCraft 引擎：读取画布、检查实际 PNG、两次本地边缘提取修正、预览、原子提交。
结果为 10 个具名独立图层，Play 变体隐藏，图片请求数为 **0**。
验证副本：`target/ai-p0/agent-mcp-live/result.pcraft`，合成图：同目录 `after.png`，
收据：`status.json`，调用记录：`calls.json`。原 PSD 未写入。

这证明接口与拆图/建层闭环已接通，不代表排版质量全部达标：合成图中的旧压平进度条
仍露出，音量原滑块也有重影。保留原像素的约束下，需要进一步规划非破坏性背景修复
或隐藏冲突变体；当前不能宣称已达到网页整栏重设计效果。也没有为本次验证额外生成
新图，新版 `generate_assets` 复用的是已实测生成适配器，完整新图到落层仍需后续验收。

实测中修复了两处连接问题：任务内 7 个工具需要显式审批配置；Windows 非阻塞监听器
接受的连接需要切回阻塞 I/O，否则首个请求会被误当断线。新增延迟鉴权回归测试。
未修改全局权限配置，通用 shell、任意文件和其他工程仍未开放。

## Current provider workflow

1. Open **Window > AI Studio**. A compact prompt bar floats near the bottom of the canvas.
   It grows upward with wrapped text, then scrolls internally at its height limit. The settings
   icon opens provider, model, mode, history and artifact controls; there is no AI side dock.
   Choose **Codex** or **Images API** in **Generation settings**.
2. For Images API, expand **Connection settings**, enter the Base URL (including `/v1` when
   appropriate), image model, and API key. Keys are session-only, never saved or logged.
3. For Codex, install/sign in to the local Codex CLI or desktop app. Windows/macOS discovery
   and PATH are supported; an explicit executable path is optional. Windows discovery checks
   versioned desktop-update binaries as well as the original installer path, choosing the most
   recently updated executable; an explicitly configured path still takes precedence. A visible **Model** picker
   loads the local app-server's `model/list` catalog asynchronously when Codex is selected.
   All pages are requested with `includeHidden: true`; hidden entries are labeled, not discarded.
   Choose a returned model or **Automatic**, which uses the server's default. The refresh icon
   reloads the catalog, and model selection is saved immediately. Discovery does not start an
   image-generation turn. A returned catalog entry is not an entitlement guarantee; generation
   still checks the configured provider's image capability and account access.
4. Enter a prompt and choose **In-place edit** or **Transparent component**. In-place edits
   with a selection send a square context frame of the visible composite, converted to sRGB. Without a
   selection, **Use current layer as reference** retains the optional active-layer reference.
5. Click **Generate image**. Codex first discovers the enabled `imagegen` skill via `skills/list`.
   The rendering turn explicitly includes `$imagegen` and a typed `skill` input with the discovered
   absolute `SKILL.md` path, so Codex injects the skill rather than guessing from its name.
   A missing/disabled skill stops before inference; install/enable it in Codex and retry.
   This follows https://learn.chatgpt.com/docs/app-server#skills and is verified against the
   installed app-server's generated schema. No hardcoded user-specific path is used.
   The renderer receives the unchanged user request and separate target/reference/output details;
   there is no automatic art-direction or optimization turn. Existing protocol fields named
   `optimizedPrompt` are retained for compatibility and now record the submitted request.
   The transparent-asset agent may identify targets and separate components, but cannot rewrite
   the user's visual request. Images API also keeps its direct prompt flow. Rendering
   stays on Codex's built-in image tool; no CLI/API/model switch, shell tool, or background-removal
   fallback is permitted. This is the app-server integration, not a direct call to the desktop
   chat's `image_gen` tool; loading the same skill does not guarantee identical stochastic images.
6. The result is automatically added as a new, visible root-level
   **AI 1**, **AI 2**, etc. raster layer, without replacing any existing layer. Adding it is one
   Undo step, and normal native saves/exports include it. Use the Layers panel to hide, delete,
   rename or continue editing the result; no extra Apply step is required.

No selection means the full canvas. With no document, Generate creates a 1024x1024 document.
The target must be an unlocked raster layer in RGB, grayscale or CMYK, at most 16,777,216 pixels.
Reference images are proportionally reduced to a maximum edge of 1024 pixels before export;
the original document and candidate destination retain their full resolution. The local test
provider retains its separate 1,048,576-pixel limit.
Masks, separate channels, floating selections and special color modes are rejected with a reason.
Selected edits freeze a square frame around the selected area with surrounding context; the
selection's longest edge is limited to 4096 pixels to bound frame processing. Codex
and the Images API receive explicit normalized edit coordinates and full-frame preservation
instructions. The square result is uniformly scaled back to that frame, then only the selected
coordinates are extracted. Non-square local-edit responses are rejected without document changes,
never stretched. This prevents flattening a square image into a thin playback bar. Full-canvas
generation without a selection still resizes to the destination and can change aspect ratio.
Local selection
coverage constrains the generated layer's alpha; selection holes and outside pixels stay
transparent. Original layers and their locks are untouched. This is reference-image editing plus
deterministic local composition, not a provider-native inpainting mask. Decomposition is not implemented.
Models can still alter layout within the selection; exact glyph/control preservation is not guaranteed.
The original source layers remain available for comparison and undo.

### Transparent components

Select the component's destination rectangle, choose **Transparent component**, and generate.
This mode sends two sRGB references: the selected visible-composite close-up (up to 2048px per
edge) and the whole design (up to 1024px per edge). The provider receives the full design FIRST
as authoritative context, then the crop for component identification and readable text. Normalized
selection coordinates identify the target, but do not lock its old silhouette, grouping or sizes.
The default workflow generates ONE redesigned transparent component directly. It does not first
generate a full-page preview. This saves one image generation, but is not guaranteed to match
the quality of a separately reviewed full-context redesign followed by extraction.

**Design first, then extract (2 images)** is an opt-in setting. Stage one generates the revised
full interface without requiring transparency. Stage two receives ONLY that newly generated
design and extracts the component with real alpha. Neither original reference is attached to
extraction. Each stage uses the selected provider; Codex explicitly loads Imagegen without
automatic aesthetic prompt expansion. Cancelling or failing stage one prevents stage two.
This explicitly enabled mode consumes two image generations.

Intermediate `design.png` (two-step only) and raw `component.png` are saved in unique folders
under the per-user PhotoCraft data directory's `AI-Generations/`. These files survive extraction
failure, import failure, app restart and later project edits, but are separate from `.pcraft`
history and are not automatically cleaned up. Settings offers open buttons for this run's saved
images. Neither API keys nor source references are written there.

The renderer is explicitly asked for an isolated PNG asset, not a replacement screenshot. Images
API uses two `image[]` parts, `background=transparent`, `output_format=png`, and automatic size.
Codex is instructed to enable its image tool's transparent-background option. Support still
depends on the provider's image model; no silent opaque fallback or automatic background removal
is used. API parameter reference: https://developers.openai.com/api/reference/resources/images/methods/edit

Returned images must contain visible pixels and at least 1% fully transparent pixels. This
rejects opaque white/checkerboard results, but is not a visual-quality guarantee. Transparent
margins and near-invisible alpha dust are excluded from placement calculations. The body
(at least half the peak alpha) is uniformly fitted within the selection by default. The optional
**Allow component outside selection** setting permits wide/tall components to grow up to twice
their original minor-axis size, within the canvas, so redesigned raised controls are not always
shrunk to the old thin selection. Canvas-edge components stay anchored to that edge; others
grow around the selection center. The original selected region remains in retry history.
Softer edges and shadows are retained where they fall inside the computed placement rectangle.
Growth beyond this bounded allowance still fits down; the unscaled raw asset is always retained.
This setting is off by default because enlargement can cover neighboring UI content; it is captured
when generation starts, so changing settings during a request cannot change its placement policy.
Aspect ratio and
internal transparency are retained. The rectangle
is a placement box, not an inpainting mask: irregular selection holes do not clip the asset.
The output is an ordinary independent layer and can be moved/transformed using existing tools.
Existing artwork is not erased; on a flattened source, old controls may remain visible through
the new asset and must be handled separately. Canvases over 64 megapixels are rejected to bound
reference rendering. Both frozen references and the mode persist in `.pcraft` history; retry
never captures the currently generated version. Choose New generation to change modes.

## Versions and project history

Each successful desktop generation creates a generation family in the native `.pcraft` project.
**Generation history** selects a family or **New generation**. The **Original** and **Version N**
controls switch that family's result-layer visibility exclusively; they never remove result pixels
or change unrelated layers. Original reveals the underlying current document without this family's
AI layers. Selection changes are undoable and saved with the document. Manually edited, moved or
regrouped result layers still obey the normal layer stack and ancestor visibility.

**Generate again** uses the family's frozen source PNG, context frame and selection coverage,
even after choosing a different version, editing the document, changing the current selection,
or saving/reopening. It uses the currently edited prompt and selected provider/model. The new
result becomes the only visible version in that family. A resized canvas rejects retries rather
than silently placing old coordinates on a new geometry; choose New generation to capture new
geometry. Cancelled or failed requests do not create records.

**Properties** displays the selected version's original prompt, optimized prompt, provider,
model and selected bounds. Codex reports its resolved model even when Automatic was selected.
The reference is stored once per family as a content-addressed binary blob; the selection uses
native tiles. This data is private project content, not PNG/JPEG/PSD metadata. Credentials and
provider response bodies are never stored. Flat exports contain the currently visible result,
not editable versions or generation records. Use `.pcraft` to retain the complete workflow.

Limits are 32 families per project, 32 versions per family, 8 MiB per reference, 4096 bytes per
original prompt and 16384 bytes per optimized prompt. The trash icon removes only the selected
generation record and retry reference, keeping all result layers; Undo restores the record.
Older projects load with an empty history. AI layers made before this update remain editable,
but their unavailable prompts/references cannot be reconstructed as generation records.

## Connections and privacy

- Images API: `/images/generations` JSON or `/images/edits` multipart upload, returning
  `data[0].b64_json`. URL-only output and text/chat-only endpoints are unsupported.
- HTTPS is required except loopback HTTP. Redirects are disabled. Provider error bodies and
  credentials are not logged. There is no silent model or edit/generation fallback.
- Codex: stdio app-server, capability/model discovery, project-bound durable conversations,
  read-only sandbox, shell disabled, no approval escalation. The user's prompt is passed through
  without a separate optimization turn; image bytes are accepted only from typed
  `imageGeneration` events, never assistant text paths or commands. Codex handles
  its existing login; PhotoCraft does not read or copy auth tokens.
- Non-secret connection settings are saved beside preferences in `ai-settings.json` when
  generation starts or the Codex model selection changes. Keys, prompts and images are excluded. OS credential-store persistence
  remains future work; keys must be re-entered after restarting.
- Changing the Base URL through the UI or control API clears the old key. Codex requests never
  receive the Images API key, even while it remains entered in the panel.
- Generate sends the prompt and optional reference to the selected service. Cancel drops the
  local request/child and rejects late results; the provider may still bill already-started work.
  The timeout is ten minutes per image stage. Editing while waiting is allowed: layer output is appended to the
  original document without reverting those edits or stealing another active tab. If the source
  was resized, changed working format/profile, closed, has a floating selection or is locked by
  another background edit, the result is retained in a separate **AI result** document instead.

## New contracts

| Engine command | Parameters | Effect |
| --- | --- | --- |
| `ai.projectEnsure` | `{}` | Assign a portable project identity once; no undo step or model call |
| `ai.referenceAdd` | `{name, imageBase64}` | Add a bounded normalized PNG to project metadata; undoable, no image layer |
| `ai.referenceRemove` | `{id}` | Remove a project reference; undoable |
| `ai.projectRecord` | `{kind, summary, prompt?}` | Append bounded project memory; no undo step; completion events retry while the document is busy |
| `ai.beginGeneration` | `{prompt, provider: "codex" | "images-api", model?: string, reference?: bool, destination?: "candidate" | "layer", mode?: "repaint" | "component"}` | Freeze source; return candidateId, mode, optional sourcePng/contextPng and editInstructions. Default mode is repaint; component requires a selection and layer destination |
| `ai.receiveCandidate` | `{candidateId, imageBase64, optimizedPrompt?: string, model?: string, expandComponent?: boolean}` | Decode/ICC-convert result; optional component expansion defaults false; layer destination stores a record and returns groupId/layerId/documentId/name/recovered; candidate destination retains legacy read-only review |
| `ai.cancelGeneration` | `{candidateId}` | Release matching pending generation |
| `ai.retryGeneration` | `{groupId, prompt, provider, model?: string}` | Begin using the family's frozen reference/coverage without changing the current selection |
| `ai.selectVersion` | `{groupId, layerId: integer | null}` | Exclusively show a version, or hide all family results for Original; undoable |
| `ai.forgetHistory` | `{groupId}` | Remove retry inputs and records, but retain result layers; undoable |
| `ai.history` | `{}` | Query records and result availability/visibility without reference image bytes |

Desktop control: `ai.configure` accepts `provider`, `baseUrl`, `model`, `codexPath`, `codexModel`, `codexReasoningEffort`, `mode`,
`prompt`, `reference`, `designFirst`, `expandComponent` (booleans, default false). `ai.generate` returns immediately with `{candidateId, pending: true}`;
`ai.retry` with `{groupId}` uses the current UI prompt/provider/model and the stored reference;
poll `ai.status` (including `optimizedPrompt` and local `savedImages` paths) and `ai.inspect`, or call `ai.cancel`. Keys cannot be set/read through this
endpoint; enter them in the desktop panel. `ai.inspect.lastLayer` returns the last inserted layer
receipt. Existing Apply/Discard commands remain available for legacy candidate mode only.

Incoming PNG/JPEG/WebP is limited to 48 MiB encoded text, 4 megapixels, 4096px per dimension,
and a 64 MiB decoder budget. ICC conversion targets the document profile and preserves its depth.
Zero-coverage source pixels are not re-encoded. Generated layers are ordinary native document
content and persist in saves. Legacy candidate data remains excluded from saves and action replay.

## OpenPencil scope

The native provider/configuration separation follows research of `open-pencil/open-pencil`
at `328021d70ff68ccbb11efda30509385d5a8b5e5a` (`src/app/ai/providers/compatible.ts`). No
TypeScript, Vue or Tauri is imported. This is a native Rust image transport, not a claim that
OpenPencil's text-agent tool loop supplies image generation. Full chat orchestration is separate.

## P1 verification

Engine tests cover encoded image ingestion at multiple depths/modes, reference export,
unchanged outside pixels, Undo, malformed/cancelled/stale results and full-canvas generation.
UI tests cover async review, cancellation and credential exclusion. Desktop tests use a loopback
HTTP fixture for JSON/multipart/auth/error handling and a synthetic Codex event stream.
The opt-in `ai_generation::tests::codex_real_image_smoke` consumes Codex image quota;
`PHOTOCRAFT_AI_SMOKE_OUT` optionally retains its image. Live Images API tests require a service
key. macOS compilation requires a Mac host.
The opt-in `codex_real_local_edit_smoke` uses a synthetic playback-bar fixture and stores the
input, square context, generated frame, composited result and native project under
`PHOTOCRAFT_AI_LOCAL_OUT`. Deterministic tests cover thin horizontal/vertical selections,
undistorted 4x4 markers at three depths, visible-composite references, and wrong-aspect rejection.

Windows live verification (2026-10-12): Codex generated a test image in 39.30 seconds, then
edited that reference in 32.54 seconds. The latter test also ingested the result as a candidate,
applied it, saved/reopened a native document, and exercised Undo/Redo. Output fixtures are
`target/ai-p0/codex-real-generated.png`, `codex-real-edited.png`, and `codex-real-edited.pcraft`.
Live third-party/API-key generation was not tested; the Images API adapter was verified against
a local HTTP fixture. No user credentials were required for those adapter tests.

# P0 Historical Notes

The first milestone establishes PhotoCraft's native candidate review boundary. It is not an
OpenPencil provider integration yet. The local test provider fills the selected part of the
active raster layer with the current foreground color; it records the prompt but does not
interpret it. No image or prompt is sent to a network service.

## Try the local milestone

1. Build the desktop app with `cargo build -p photocraft`.
2. Open an image and enable **Window > AI Studio**.
3. Select an area on an unlocked raster layer and choose a foreground color.
4. Click **Create Test Candidate**. Toggle **Preview** to compare against the original.
5. Click **Apply** or **Discard**. Apply creates one native Undo step.
6. Save as `.pcraft`, reopen, and export using the existing File commands.

The local provider accepts selections whose bounding box contains at most 1,048,576 pixels.
Only RGB, grayscale and CMYK documents are accepted in this milestone. Masks, separate channels,
smart objects, floating selections and special color modes are intentionally unsupported.

## Candidate contract

All frontends use the engine registry:

| Command | Parameters | Effect |
| --- | --- | --- |
| `ai.prepareTestCandidate` | `{"prompt":"..."}` | Replaces the in-memory pending candidate; no document mutation |
| `ai.inspect` | `{}` | Candidate metadata and last Apply receipt |
| `ai.applyCandidate` | `{"candidateId":1}` | One `Session::edit` transaction |
| `ai.discardCandidate` | `{"candidateId":1}` | Releases the candidate without history |

The candidate freezes the source document, document revision, layer and selection. Apply
requires the same active source and revision, rejects mismatched identifiers and rechecks
locks and edit targets. Only the target layer's surface is replaced. Other layers, metadata
and zero-coverage pixels remain untouched. Repeating the most recent successful Apply returns
its receipt without editing again. Candidate IDs are session-local and excluded from action
replay. Closing the source document releases its candidate. Preview state is never saved into
the document format; saving while reviewing writes the committed document, not the candidate.
Save As can rename the source document and invalidate its snapshot; prepare a fresh candidate
after that metadata change.

Native fill provides feathered coverage, transparency locks, COW storage and runtime pixel
depth/color-mode handling. The test provider follows existing foreground-color semantics,
without adding an 8-bit or sRGB conversion. Real image-provider results will need an explicit
ICC-aware import and deterministic write-mask composition before they may enter this boundary.

## Next milestone

Extract OpenPencil's provider adapters and streaming tool loop into an isolated AI runtime.
Keep PhotoCraft's UI, raster storage, project format and Undo authoritative. Replace the local
test generator with validated candidate patches, not arbitrary engine-command execution.
The initial provider path will use explicit user-configured credentials; real generation,
localized repaint and layer decomposition are not implemented in this milestone.

## Verification

Engine regression tests cover native round-trip, undo/redo, repeated Apply, selection holes,
feathering, transparency locks, unrelated-layer bytes, stale revisions, wrong documents,
closing documents, malformed parameters and oversized requests. RGB and grayscale tests run
at 8/16/32 bits, with an additional CMYK test. UI tests exercise the production candidate
buttons in every theme at 800x480 and 1280x720. Native macOS builds still require a macOS host.
Run the full test suite with `PHOTOCRAFT_LOCALE=en-US`: several upstream integration fixtures
locate their controls by English text. This is a per-process test setting, not an app preference.

Windows executable: `target/debug/photocraft.exe`. The reproducible control fixture is
`scripts/ai-p0-preview.json`; retained local artifacts are in `target/ai-p0/` (gitignored).

The layering, adversarial command test and all 24 wasm checks pass. Strict Clippy is blocked
by pre-existing `collapsible_match` warnings in `affinity/src/preview.rs` and
`engine/src/color_to_alpha_cmds.rs`. Checking the touched packages with `--no-deps` and only
that lint temporarily excluded passes; no project lint configuration was weakened.
