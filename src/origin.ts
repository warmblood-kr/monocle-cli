// Identifies this CLI as the call surface ("origin") so chat-proxy can
// attribute LLM usage/billing per surface (chat/craft/cli).
// Tracking: warmblood-kr/chat-proxy#216

export const MONOCLE_ORIGIN = 'cli';

// Spread into fetch request headers hitting chat-proxy.
export const ORIGIN_HEADER = {
  'x-monocle-origin': MONOCLE_ORIGIN,
} as const;
