# pi vs monocle-cli agent mode — design analysis (reconstructed)

> **⚠️ This document is a reconstruction, not the original analysis.** The
> original 1:1 file:line comparison between `earendil-works/pi` and
> monocle-cli's agent mode was produced during Phase 0 development
> (2026-07-01) and lived only in a session scratchpad
> (`pi-vs-ours-analysis.md`) that was never committed. That scratchpad is
> permanently lost — only a short summary survived, persisted in a
> [`monocle-cli#44`](https://github.com/warmblood-kr/monocle-cli/issues/44)
> progress comment. This document does **not** recover the original
> analysis from memory (no worker session ever held that file in context
> after it was written). Instead, every citation below was **freshly
> re-verified against the current state of both repos** by re-reading the
> actual source — it answers "does this claim still hold, and where,
> today?" rather than "what did the original diff say?". Where re-verification
> surfaced a **discrepancy** with the archived summary, that is called out
> explicitly rather than silently smoothed over (see "Corrections" at the
> bottom — most notably, the steering/follow-up queue claimed as adopted
> was not actually found in current code).
>
> No file:line below is quoted from memory. Every citation was located by
> grep/read against the checked-out trees at
> `topics/monocle-cli-diagnostics/pi` and
> `topics/monocle-cli-diagnostics/monocle-cli` on 2026-08-14.
>
> Related: design decision log —
> [`monocle#158`](https://github.com/warmblood-kr/monocle/issues/158) (SDD,
> §9 has the Phase 0–4 sequence); implementation status —
> [`monocle-cli#44`](https://github.com/warmblood-kr/monocle-cli/issues/44)
> (the original summary this document expands on).

## Why pi at all

pi (`earendil-works/pi`, MIT) is a headless TypeScript coding agent — engine
for OpenClaw. It was reviewed as a **structural reference only** (no code
borrowed; license cleared 2026-06-30, see `monocle-cli/CLAUDE.md`). monocle-cli's
agent mode is not a coding agent — it's a model-agnostic **server-agent
backend that Craft/desktop drive** (Path B, decided over wrapping `opencode`
— see `monocle#158` comment log, 2026-07-01) — so pi's shape was mined for
structure, not copied wholesale.

Phase reference (from `monocle-cli/CLAUDE.md`): Phase 0 (skeleton) → Phase 1
(streaming + light multi-turn) → Phase 2 (session/dual-channel) → Phase 3
(ACP surface) → Phase 4 (retry, parallel tools, grep/ls, compaction — not
yet done).

---

## Adopted

### 1. RPC/event protocol → ACP surface (Phase 3)

- **pi**: `packages/coding-agent/src/modes/rpc/jsonl.ts:5-11` — LF-only
  framing, deliberately not Node's `readline` (which splits on other
  Unicode separators). `packages/coding-agent/src/modes/rpc/rpc-types.ts:20-73`
  — `RpcCommand` union, every variant carries an optional `id` for
  correlation (`prompt`, `steer`, `follow_up`, `abort`, `fork`,
  `new_session`). `rpc-types.ts:238-273` — `RpcExtensionUIRequest`
  (`select`/`confirm`/`input`/…) puts UI needs on the *same* wire as the
  core protocol. `rpc-mode.ts:748-798` — `handleInputLine` dispatches each
  parsed line by `command.id`.
- **monocle-cli**: `src/acp.rs:966-970` — `serve_over`'s doc comment:
  "raw byte streams carrying newline-delimited JSON-RPC" — same LF framing,
  the standardized Zed `agent-client-protocol` crate instead of a bespoke
  schema. `src/acp.rs:192-420` — `impl Agent for MonocleAgent`:
  `initialize`/`authenticate`/`new_session`/`prompt`/`cancel`. `src/acp.rs:47-50`
  — `PERM_ALLOW`/`PERM_REJECT`, the same "approval is just another wire
  round trip" idea as pi's extension-UI requests.
- **Why adopted**: both sides frame messages as LF-delimited JSON over
  stdio, correlate by id, and treat approval/UI prompts as first-class wire
  traffic rather than a side channel. monocle-cli didn't reuse pi's bespoke
  schema — it adopted Zed's standard ACP instead — but the *shape* (framing
  + id correlation + approval-on-the-wire) is a direct structural match.

### 2. Streaming with in-stream error handling (Phase 1)

- **pi**: `packages/ai/src/api/openai-completions.ts:591-611` — the
  SSE-consuming loop's `catch` sets `output.stopReason =
  signal?.aborted ? "aborted" : "error"` and ships the **already-parsed
  partial content** in the terminal error event rather than discarding it.
  `packages/ai/src/utils/event-stream.ts:66-88` — `"error"` is a first-class
  terminal event type, not an exception that unwinds silently.
- **monocle-cli**: `src/agent/providers.rs:277-395` —
  `assemble_sse_stream`. On a read error: if `finish_reason.is_some()`
  already, treat the drop as benign end-of-stream (296-308); else if any
  content/tool-call text had already arrived, salvage it and set
  `truncated = true` (dropping any in-flight tool call as unreliable); else
  propagate the error. `providers.rs:382-386` — a stream that produced
  *nothing* and wasn't truncated is treated as a likely error-shaped `200`
  body and surfaced as `Err`, not a silent empty `Ok`.
- **Why adopted**: both converge on the same non-obvious design — salvage
  partial output on mid-stream failure, but keep it distinguishable from a
  genuinely complete response. pi encodes this as a terminal `error` event
  carrying the partial message; monocle-cli encodes it as a `truncated:
  bool` flag on the `Ok` `ChatResponse`. Same decision tree, different
  encoding.

### 3. Dual-channel tool results + arg schema validation (Phase 2)

- **pi**: `packages/agent/src/types.ts:361-375` — `AgentToolResult<T>`:
  `content` (model-facing) vs `details: T` (arbitrary structured data "for
  logs or UI rendering"). `packages/ai/src/utils/validation.ts:271-350` —
  `validateToolArguments` compiles the tool's JSON-Schema (TypeBox) via
  `Compile()` (cached), coerces types, and throws a formatted per-field
  error on mismatch — a real schema-driven validator, not spot checks.
- **monocle-cli**: `src/agent/tools.rs:262-296` — `ToolOutcome { pub llm:
  String, pub ui: Option<String>, pub is_error: bool }`, doc comment cites
  this as "SDD §9a": `llm` feeds back into the conversation, `ui` is the
  human/client-facing rendering (falls back to `llm` via `ui_text()`,
  293-295). Each tool declares a JSON-Schema `parameters()` (e.g.
  `tools.rs:351-360`), but argument checking itself is manual —
  `src/agent/runner.rs:197-231` catches malformed JSON and turns it into a
  `ToolOutcome::error`; `str_arg` (`tools.rs:316-320`) checks individual
  required fields at call time.
- **Why adopted, at reduced fidelity**: the dual-channel split
  (`ToolOutcome{llm, ui}` ↔ `AgentToolResult{content, details}`) is a
  direct, CLAUDE.md-cited match. The schema-validation half was adopted at
  a *smaller* scope — monocle-cli declares JSON-Schema per tool (matching
  pi's TypeBox schemas) but does field-presence/parse checks rather than a
  general JSON-Schema compiler/coercion engine, consistent with the
  "concrete over generic machinery" principle (see "Not adopted #2" below).

### 4. Append-only JSONL session + replay/resume (Phase 2)

- **pi**: `packages/agent/src/harness/session/jsonl/storage.ts:59-108` —
  `JsonlSessionStorage`: a v4 header line + a mutation log, replayed via
  `applyMutation` on load; tolerates a **torn tail** (`isTornTail`,
  84-92) by atomically republishing the valid prefix (`publishFileAtomically`,
  27-44). `storage.ts:267-269` — `appendMutation` appends one mutation per
  call.
- **monocle-cli**: `src/agent/session.rs:1-8` — module doc: "append-only
  JSONL of the conversation, replayed on resume... One `Message` per line."
  `session.rs:29-58` — `load()` tolerates a corrupt/truncated **final**
  line only ("a corrupt earlier line is real corruption"), otherwise
  errors. `session.rs:61-77` — `append()` opens `.create(true).append(true)`,
  one JSON object per line.
- **Why adopted, at reduced fidelity**: same core discipline (append-only
  JSONL, replay-to-reconstruct, tolerate a torn tail from a killed
  process) at very different scope — pi has a full mutation-log format with
  branching/fork lanes and atomic tmp-file republish; monocle-cli took only
  the essential idea (flat `Message` list, one per line, tolerate a corrupt
  last line), matching the Phase 2 scope note in `CLAUDE.md`.

### 5. Abort (Phase 1/3) — **not** the full "steering + abort" claim

- **pi**: `packages/agent/src/agent.ts:319-321` — `abort()` calls
  `this.activeRun?.abortController.abort()`.
  `packages/coding-agent/src/modes/rpc/rpc-mode.ts:418-431` — RPC
  `case "abort"` → `session.abort()`.
- **monocle-cli**: `src/agent/runner.rs:17-36` — `Cancel` (an
  `Arc<AtomicBool>`), checked at step boundaries. `src/acp.rs:414-419` —
  `async fn cancel` maps ACP's `session/cancel` directly to
  `st.cancel.cancel()`.
- **Why adopted**: straightforward cooperative-cancellation match on both
  sides — a shared cancel flag checked at safe points, wired to the
  protocol's cancel verb.
- **⚠️ See "Corrections" below — the "steering" half of this claim (pi's
  `PendingMessageQueue` / mid-turn message injection) has no counterpart in
  current monocle-cli source.** The archived summary bundled steering with
  abort as one adopted item; re-verification found only abort was actually
  built.

---

## Not adopted (deliberately)

### 1. 40-provider catalog (Phase 0 — provider abstraction / G1 seam)

- **pi**: `packages/ai/src/providers/*.models.ts` — 39 per-vendor model
  files. `packages/ai/src/model-catalog.ts:1-26` — a generic
  `ModelCatalog<TGroups, TProvider>` mapped-type machine flattening every
  vendor's model groups into one catalog entry type.
- **monocle-cli**: `src/agent/providers.rs:1-11` — module doc: `MonocleProvider`
  routes through monocle's chat-proxy (OpenAI-compatible
  `/v1/chat/completions`), so model selection is delegated server-side
  ("monocle-model-router / monocle-auto can select any model").
  `providers.rs:400-417` — one `LlmProvider` trait, no per-vendor variants.
  `providers.rs:419-490` — `MonocleProvider` posts to a single endpoint.
- **Why not adopted**: the client-side model-selection problem pi solves
  with ~39 per-vendor tables is solved server-side here (chat-proxy /
  model-router), so the client only needs a single trait + a `model:
  String` passthrough field — no client-owned catalog needed at all.

### 2. Generic type machinery (ongoing design principle, from Phase 0)

- **pi**: `packages/ai/src/types.ts:502-507` — `Tool<TParameters extends
  TSchema = TSchema>`. `packages/agent/src/types.ts:386` — `AgentTool`
  ties a schema type to a compile-time-inferred argument type
  (`Static<TParameters>`) plus a separate details type.
  `packages/ai/src/model-catalog.ts:5-20` — mapped types over mapped types
  to derive a flattened per-model union at compile time.
- **monocle-cli**: `src/agent/providers.rs:92-121` — `ToolDef`/`ToolFunction`,
  no generic parameters, `parameters: Value`. `src/agent/tools.rs:298-314`
  — `trait Tool` with plain methods, no associated types/generics.
- **Why not adopted**: pi leans on TypeScript's structural/conditional
  types to derive argument types from schemas at compile time. Rust's
  `serde_json::Value` + a concrete `ToolOutcome` struct sidestep that
  entirely — runtime `Value` plus a handful of field checks does the job at
  a fraction of the complexity, matching monocle-cli's stated
  small-and-concrete design principle (`monocle-cli/CLAUDE.md`, "설계
  원칙").

### 3. In-process-sandbox / coarse-trust assumption (Phase 3 — this is the flip side of "Approver seam", below)

- **pi**: `packages/coding-agent/docs/security.md:31-37` — "No Built-in
  Sandbox": tools run with the process's own permissions; isolation is
  explicitly deferred to an external OS/container/VM boundary, not
  attempted in-process. `packages/coding-agent/docs/security.md:5-18` —
  "Project Trust" is a **one-time, directory-scoped** decision made before
  a session starts, explicitly **not** a per-call gate (security.md:7: "it
  does not restrict what the model can ask tools to do after you start
  working in a directory"). No runtime approval hook exists in the built-in
  tools themselves (`bash.ts`/`edit.ts`/`write.ts` — confirmed by grep, no
  `confirm`/`approve`/`permission` call before executing).
- **monocle-cli**: `src/agent/tools.rs:117-140` — `LocalShell::exec` also
  spawns directly on the host via `std::process::Command`, no OS-level
  sandbox either — so this is **not** "we sandbox, pi doesn't." Neither
  side has real OS-level isolation today.
- **Why not adopted — reframed**: what monocle-cli actually rejected isn't
  "sandboxing" (neither side has it) but pi's *safety-anchoring choice* —
  gate coarsely once per directory (or not at all per call) and treat real
  isolation as an external concern. monocle-cli instead anchors safety with
  a mandatory **per-call** approval seam in the loop itself — which is the
  same fact as "Approver seam ahead," just stated from the other side.

---

## Where monocle-cli is ahead: the `Approver` seam (Phase 0 loop, Phase 3 ACP wiring)

- **pi**: no per-tool-call runtime approval hook exists anywhere in the
  agent-core loop or built-in tools (grep across
  `packages/agent/src/harness`, `packages/coding-agent/src/core` for
  `yolo|permission|approval|Approver` — no runtime gate found).
  `rpc-types.ts:240` has a `confirm` UI primitive, but it's opt-in and
  extension-authored — nothing in the built-in tools calls it before
  executing.
- **monocle-cli**: `src/agent/runner.rs:38-50` — `trait Approver { fn
  approve(&mut self, id: &str, tool_name: &str, args: &Value) -> bool; }`
  (`AllowAll` exists as the YOLO-equivalent, but is not the ACP-path
  default). `runner.rs:220-231` — every side-effecting tool call passes
  through this seam unconditionally before executing. `src/acp.rs:525-575`
  — `AcpApprover` bridges this to ACP's `session/request_permission`
  (builds `AllowOnce`/`RejectOnce` options, blocks cancel-aware on the
  client's answer). `src/agent/permission.rs:1-18` — a persisted
  per-tool/per-shell-pattern remembered-decision layer sits on top of the
  raw seam for the interactive CLI.
- **Why this is a real lead**: pi's documented posture is coarse,
  directory-level, one-time trust; monocle-cli built a first-class
  per-call gate consulted on *every* side-effecting call, with a concrete
  implementation mapped onto ACP's standard `session/request_permission`
  capability. This is a genuine place where monocle-cli's runtime-gating
  architecture is more developed than pi's, specifically for live,
  per-call approval (as opposed to project-level trust).

---

## Corrections to the archived `monocle-cli#44` summary

Re-verification against current source surfaced two places where the
archived summary doesn't match what's actually in the codebase today. Both
are recorded here rather than silently carried forward, per the instruction
that a plausible-looking-but-wrong citation is worse than an explicit gap:

1. **"스티어링/follow-up 큐 + abort" — only the abort half exists.**
   pi's `PendingMessageQueue` (`packages/agent/src/agent.ts:231-232,
   283-290`, `session.steer`/`session.followUp`) has **no counterpart**
   anywhere in current monocle-cli source (`grep -rn
   "steer|follow_up|followUp|fork" src/acp.rs src/agent/*.rs` and full
   git-log search — nothing relevant). A prompt arriving while a turn is
   already running is currently **rejected outright**, not queued:
   `src/acp.rs:274-276`, `if st.running { return
   Err(acp::Error::invalid_params()); } // session busy`. This may have
   been planned-but-not-yet-built at Phase 2, or the original summary
   overstated it — either way, mid-turn steering/follow-up injection is a
   real gap against pi's design, not a shipped feature. Worth flagging for
   whoever scopes Phase 4 or a Phase 2 backfill.
2. **"in-process 샌드박스 가정 (비채택)" needed a sharper frame.** As
   written above, neither side has OS-level sandboxing — the real
   not-adopted thing is pi's *coarse, one-time* trust-anchoring choice,
   not sandboxing itself. Folded into "Not adopted #3" above.

No other claim in the archived summary required correction — the RPC/ACP,
streaming+in-stream-error, dual-channel+schema, JSONL+replay, 40-provider
catalog, generic-type-machinery, and Approver-seam claims all held up
against current source with the citations above.
