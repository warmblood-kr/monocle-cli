# SDK lane (jarvice → agent-sandbox) turn from a CLI — finding (2026-10-20)

Sources: jarvice@devel (socket/agent.py, utils/auth.py, routers/auths.py, routers/agent.py,
utils/stark_jwt_verifier.py), agent-sandbox@HEAD (wrangler.jsonc, src/index.ts, container/src/*).

- Turns only via Socket.IO `/agent` (events start_session, message, interrupt). No REST turn route.
- `/agent` on_connect auth = `auth["token"]` → `decode_token` = HS256/WEBUI_SECRET_KEY only (socket/agent.py:492-499).
- **A Stark bearer CAN obtain that token**: `GET /api/v1/auths/` (routers/auths.py:139-177) depends on plain
  `get_current_user` (accepts Stark bearer when STARK_JWT_VERIFIER_MODE audit/enforced and sub/email maps to a
  jarvice user) and returns a NEW HS256 `token` (`{"id": user.id}`). Verified live on stg 2026-10-20: HTTP 200,
  role admin, 141-char token, expires_at null. (Side effect: handler also sets a `token` cookie — irrelevant to CLI.)
- Flow for a CLI: POST /api/v1/agent/sessions → GET /api/v1/auths/ → Socket.IO connect ns `/agent` auth {token}
  → emit start_session {session_id} → wait session_status "active" → emit message {session_id, content} →
  stream text_delta/tool_use/result events (MESSAGE_MAPPINGS, socket/agent.py:57-129).
- Cost: needs an engine.io/socket.io v4 client (no crate in tree; `rust_socketio` or hand-rolled over tungstenite).
- Alternative, no Socket.IO: worker HTTP+SSE `POST {worker}/api/task/{sessionId}/message?userId=` (worker url from
  GET /api/v1/agent/status = https://jarvice-worker-staging.seongminc.workers.dev). Worker enforces nothing when
  TASK_INGRESS_ENFORCE_IDENTITY=off (repo wrangler; live value unknown) but needs a Bearer that chat-proxy accepts
  (jarvice sends an OBO token from CraftJWTService; whether a user access token works is unverified), plus
  X-Tenant-ID/X-User-ID/X-User-Oauth-Sub/X-Chat-ID headers. If enforcement is turned on, only monocle_token_type=obo
  tokens (aud chat-proxy-stg) pass → CLI path would break. Sends the user's token to a third-party-ish host; needs approval.
- chat delegation (`craft__delegate_session`): POST /api/responses with native function calling + saved chat +
  ENABLE_CRAFT_SESSION_DELEGATE + CRAFT_BACKEND=cf; model decides to call the tool → nondeterministic, not a test harness.
- Unknown: live verifier mode/audience, live worker enforce flag, chat-proxy behavior for user access token.
