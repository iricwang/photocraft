# Project protocol v1 fixtures

`publish-multi-target.json` is a valid two-target control plan. Its `planHash` is SHA-256 over UTF-8 JSON after removing `planHash` and sorting object keys recursively; no whitespace is emitted. Array order is significant. Hash strings include `sha256:` or `blake3:`; legacy untyped hashes are not accepted. Numeric values must retain the JSON representation emitted by the Rust DTO; adapters receive the canonical plan bytes/hash and do not reserialize floating-point properties to reconstruct its hash.

`source-envelope.json` validates the envelope contract only. Its native descriptor names the bytes `fixture-metadata-only`; those bytes deliberately are not an editable `.pcraft`. A real SaveSource request must include a valid native container and every dependency body, and the store validates the native file before acknowledging it. Binary source/image bodies travel separately from control JSON.

IDs are canonical UUID strings. `context` always describes the authority project/checkout/workspace, never the remote client's cache. Native document local layer numbers map to stable layer UUIDs; they are not network identities. Lease proofs use `{ "resource": { "resourceKind": "scene|package|asset", "resourceId": "UUID" }, "leaseEpoch": 1 }`.

`targets` and `nodeChanges` are arrays, including when only one GUI changes. A scene update is a full desired node set, not a partial patch. Rotation is exactly three finite Euler angles in degrees. GUI and atlas files use `editorResource`; PNG/blob files use `hostFile`. No write request accepts executable Lua or ambient absolute paths.

Unknown versions, unknown write fields, unsafe paths, hash/length mismatches, stale workspace/lease epochs, missing targets, duplicate resource writers, cyclic hierarchy and quaternion-shaped rotation are rejected before mutation. The Rust contract tests cover malformed inputs, idempotent retries, old source CAS, failed precommit writes, response loss and restart fencing.
