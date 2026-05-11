import * as fs from 'fs';
import { Credentials } from '../credentials';
import { RefreshDeps } from '../refresh';
import { getAccessToken } from '../auth';

export interface AudioSpeechOptions {
  model?: string;
  voice?: string;
  format?: string;
  speed?: string;
  instructions?: string;
  output?: string;
  azure?: boolean;
}

export interface AudioSpeechDeps {
  credentials?: Credentials;
  refreshDeps?: RefreshDeps;
  fetch?: typeof globalThis.fetch;
  now?: () => Date;
  stdin?: NodeJS.ReadableStream;
  stdout?: NodeJS.WritableStream;
  stderr?: NodeJS.WritableStream;
  writeFile?: (path: string, data: Buffer) => void;
  stdoutIsTTY?: boolean;
}

const DEFAULT_MODEL = 'gpt-4o-mini-tts';
const DEFAULT_VOICE = 'alloy';
const DEFAULT_FORMAT = 'mp3';

function readStdin(stdin: NodeJS.ReadableStream): Promise<string> {
  return new Promise((resolve, reject) => {
    let data = '';
    stdin.setEncoding?.('utf-8');
    stdin.on('data', (chunk: string | Buffer) => {
      data += typeof chunk === 'string' ? chunk : chunk.toString('utf-8');
    });
    stdin.on('end', () => resolve(data));
    stdin.on('error', reject);
  });
}

export async function audioSpeechCommand(
  textArg: string | undefined,
  options: AudioSpeechOptions,
  deps?: AudioSpeechDeps,
): Promise<void> {
  const fetchFn = deps?.fetch ?? globalThis.fetch;
  const stdout = deps?.stdout ?? process.stdout;
  const stderr = deps?.stderr ?? process.stderr;
  const writeFile = deps?.writeFile ?? ((p: string, d: Buffer) => fs.writeFileSync(p, d));
  const stdoutIsTTY =
    deps?.stdoutIsTTY ?? Boolean((process.stdout as any).isTTY);

  let text = textArg ?? '';
  if (!text || text === '-') {
    text = (await readStdin(deps?.stdin ?? process.stdin)).trim();
  }
  if (!text) {
    throw new Error(
      'No input text. Pass text as an argument or pipe it via stdin.',
    );
  }

  if (!options.output && stdoutIsTTY) {
    throw new Error(
      'Refusing to write binary audio to a terminal. Use `-o <path>` or pipe stdout to a file.',
    );
  }

  const { token, routerUrl } = await getAccessToken(deps);

  const azure = options.azure === true;
  const endpointPath = azure
    ? '/v1/azure/text-to-speech/cognitiveservices/v1'
    : '/v1/audio/speech';

  let response: Response;
  if (azure) {
    // Azure Speech endpoint expects raw SSML (or text) with the format
    // negotiated through the X-Microsoft-OutputFormat header. We pass the
    // body verbatim — the user is responsible for valid SSML — and let
    // them override the format via --format. Default to a common 24kHz mp3.
    const outputFormat = options.format ?? 'audio-24khz-48kbitrate-mono-mp3';
    const contentType = text.trim().startsWith('<') ? 'application/ssml+xml' : 'text/plain';
    response = (await fetchFn(`${routerUrl}${endpointPath}`, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${token}`,
        'Content-Type': contentType,
        'X-Microsoft-OutputFormat': outputFormat,
      },
      body: text,
    })) as Response;
  } else {
    const payload: Record<string, unknown> = {
      model: options.model ?? DEFAULT_MODEL,
      voice: options.voice ?? DEFAULT_VOICE,
      input: text,
      response_format: options.format ?? DEFAULT_FORMAT,
    };
    if (options.speed) payload.speed = parseFloat(options.speed);
    if (options.instructions) payload.instructions = options.instructions;

    response = (await fetchFn(`${routerUrl}${endpointPath}`, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${token}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(payload),
    })) as Response;
  }

  if (!response.ok) {
    const body = await response.text();
    stderr.write(`API error ${response.status} ${response.statusText}\n`);
    stderr.write(body);
    if (!body.endsWith('\n')) stderr.write('\n');
    process.exit(1);
  }

  const buffer = Buffer.from(await response.arrayBuffer());
  if (options.output) {
    writeFile(options.output, buffer);
    stderr.write(`Wrote ${buffer.length} bytes to ${options.output}\n`);
  } else {
    stdout.write(buffer);
  }
}
