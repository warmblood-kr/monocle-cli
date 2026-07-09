//! Centralized chat-proxy endpoint paths. Keeping them as constants keeps the
//! command files and tests honest — the `texttospeech` vs `text-to-speech`
//! path bug (commit 4c6eb3f) is the kind of thing a constant prevents.

pub const MODELS: &str = "/v1/models";
pub const CHAT_COMPLETIONS: &str = "/v1/chat/completions";
pub const AUDIO_TRANSCRIPTIONS: &str = "/v1/audio/transcriptions";
pub const AUDIO_SPEECH: &str = "/v1/audio/speech";
pub const AZURE_SPEECH_TO_TEXT: &str = "/v1/speechtotext/transcriptions:transcribe";
pub const AZURE_TEXT_TO_SPEECH: &str = "/v1/azure/texttospeech/cognitiveservices/v1";

// jarvice's MCP catalog + connector routes (`monocle mcp ls|connect|exec`).
// `{name}` is a literal placeholder the caller replaces (see `commands/mcp.rs`)
// — kept as plain consts (not format functions) to match this file's style.
pub const MCP_SERVERS: &str = "/api/v1/mcp/servers";
pub const MCP_SERVER_ENABLE: &str = "/api/v1/mcp/servers/{name}/enable";
pub const MCP_SERVER_DISABLE: &str = "/api/v1/mcp/servers/{name}/disable";
pub const CONNECTOR_CONNECT: &str = "/api/v1/connectors/{name}/connect";
pub const CONNECTOR_STATUS: &str = "/api/v1/connectors/status";
/// Non-OAuth (`api_key`-type) connector token submission — not in the Phase 0
/// spike's literal endpoint list but described in its prose; added here so
/// `connect`'s api-key path has a named constant like every other route.
pub const CONNECTOR_TOKEN: &str = "/api/v1/connectors/{name}/token";
