//! Centralized chat-proxy endpoint paths. Keeping them as constants keeps the
//! command files and tests honest — the `texttospeech` vs `text-to-speech`
//! path bug (commit 4c6eb3f) is the kind of thing a constant prevents.

pub const MODELS: &str = "/v1/models";
pub const CHAT_COMPLETIONS: &str = "/v1/chat/completions";
pub const AUDIO_TRANSCRIPTIONS: &str = "/v1/audio/transcriptions";
pub const AUDIO_SPEECH: &str = "/v1/audio/speech";
pub const IMAGES_EDITS: &str = "/v1/images/edits";
pub const AZURE_SPEECH_TO_TEXT: &str = "/v1/speechtotext/transcriptions:transcribe";
pub const AZURE_TEXT_TO_SPEECH: &str = "/v1/azure/texttospeech/cognitiveservices/v1";
