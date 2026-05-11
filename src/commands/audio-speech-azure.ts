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

export interface AudioSpeechAzureOptions {
  format?: string;
  output?: string;
}

export interface AudioSpeechAzureDeps extends BinaryOutputDeps {
  credentials?: Credentials;
  refreshDeps?: RefreshDeps;
  fetch?: typeof globalThis.fetch;
  now?: () => Date;
  stdin?: NodeJS.ReadableStream;
}

const DEFAULT_FORMAT = 'audio-24khz-48kbitrate-mono-mp3';

export async function audioSpeechAzureCommand(
  bodyArg: string | undefined,
  options: AudioSpeechAzureOptions,
  deps?: AudioSpeechAzureDeps,
): Promise<void> {
  const fetchFn = deps?.fetch ?? globalThis.fetch;
  const stderr = deps?.stderr ?? process.stderr;

  let body = bodyArg ?? '';
  if (!body || body === '-') {
    body = await readStdinText(deps?.stdin ?? process.stdin);
  }
  body = body.trim();
  if (!body) {
    throw new Error('No input. Pass an SSML document as an argument or pipe it via stdin.');
  }

  if (!body.startsWith('<speak')) {
    throw new Error(
      'Azure TTS requires SSML. Body must start with `<speak …>`. ' +
        'Tip: keep SSML in a file and pipe it in to avoid shell-escaping issues:\n' +
        '  monocle audio speech-azure -o out.mp3 < my.ssml',
    );
  }

  ensureNotWritingBinaryToTTY(options.output, deps ?? {});

  const { token, routerUrl } = await getAccessToken(deps);

  const response = await fetchFn(`${routerUrl}${ENDPOINTS.azureTextToSpeech}`, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/ssml+xml',
      'X-Microsoft-OutputFormat': options.format ?? DEFAULT_FORMAT,
    },
    body,
  });

  if (!response.ok) await writeApiErrorAndExit(response as any, stderr);

  const buffer = Buffer.from(await response.arrayBuffer());
  writeBinaryOutput(buffer, options.output, deps ?? {});
}
