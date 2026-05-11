// Centralized chat-proxy endpoint paths. Keeping them as constants keeps the
// command files and tests honest — the `texttospeech` vs `text-to-speech`
// path bug (commit 4c6eb3f) is the kind of thing a constant prevents.

export const ENDPOINTS = {
  models: '/v1/models',
  chatCompletions: '/v1/chat/completions',
  audioTranscriptions: '/v1/audio/transcriptions',
  audioSpeech: '/v1/audio/speech',
  azureSpeechToText: '/v1/speechtotext/transcriptions:transcribe',
  azureTextToSpeech: '/v1/azure/texttospeech/cognitiveservices/v1',
} as const;
