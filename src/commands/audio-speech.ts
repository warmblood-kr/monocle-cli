import { Credentials } from '../credentials';
import { RefreshDeps } from '../refresh';
import { getAccessToken } from '../auth';
import {
  BinaryOutputDeps,
  ensureNotWritingBinaryToTTY,
  readStdinText,
  writeApiErrorAndExit,
  writeBinaryOutput,
} from '../audio-io';
import { ENDPOINTS } from '../endpoints';
import { ENTRYPOINT_HEADER } from '../entrypoint';

export interface AudioSpeechOptions {
  model?: string;
  voice?: string;
  format?: string;
  speed?: string;
  instructions?: string;
  output?: string;
}

export interface AudioSpeechDeps extends BinaryOutputDeps {
  credentials?: Credentials;
  refreshDeps?: RefreshDeps;
  fetch?: typeof globalThis.fetch;
  now?: () => Date;
  stdin?: NodeJS.ReadableStream;
}

const DEFAULT_MODEL = 'gpt-4o-mini-tts';
const DEFAULT_VOICE = 'alloy';
const DEFAULT_FORMAT = 'mp3';

export async function audioSpeechCommand(
  textArg: string | undefined,
  options: AudioSpeechOptions,
  deps?: AudioSpeechDeps,
): Promise<void> {
  const fetchFn = deps?.fetch ?? globalThis.fetch;
  const stderr = deps?.stderr ?? process.stderr;

  let text = textArg ?? '';
  if (!text || text === '-') {
    text = (await readStdinText(deps?.stdin ?? process.stdin)).trim();
  }
  if (!text) {
    throw new Error('No input text. Pass text as an argument or pipe it via stdin.');
  }

  ensureNotWritingBinaryToTTY(options.output, deps ?? {});

  const { token, routerUrl } = await getAccessToken(deps);

  const payload: Record<string, unknown> = {
    model: options.model ?? DEFAULT_MODEL,
    voice: options.voice ?? DEFAULT_VOICE,
    input: text,
    response_format: options.format ?? DEFAULT_FORMAT,
  };
  if (options.speed) payload.speed = parseFloat(options.speed);
  if (options.instructions) payload.instructions = options.instructions;

  const response = await fetchFn(`${routerUrl}${ENDPOINTS.audioSpeech}`, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json',
      ...ENTRYPOINT_HEADER,
    },
    body: JSON.stringify(payload),
  });

  if (!response.ok) await writeApiErrorAndExit(response as any, stderr);

  const buffer = Buffer.from(await response.arrayBuffer());
  writeBinaryOutput(buffer, options.output, deps ?? {});
}
