// Identifies this CLI as the call surface ("entrypoint") so chat-proxy can
// attribute LLM usage/billing per surface (chat/craft/cli).
// Tracking: warmblood-kr/chat-proxy#216

export const MONOCLE_ENTRYPOINT = 'cli';

// Spread into fetch request headers hitting chat-proxy.
export const ENTRYPOINT_HEADER = {
  'x-monocle-entrypoint': MONOCLE_ENTRYPOINT,
} as const;
