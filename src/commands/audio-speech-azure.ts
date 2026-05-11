import * as fs from 'fs';
import { Credentials } from '../credentials';
import { RefreshDeps } from '../refresh';
import { getAccessToken } from '../auth';

export interface AudioSpeechAzureOptions {
  format?: string;
  output?: string;
  plain?: boolean;
}

export interface AudioSpeechAzureDeps {
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

const DEFAULT_FORMAT = 'audio-24khz-48kbitrate-mono-mp3';

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

export async function audioSpeechAzureCommand(
  bodyArg: string | undefined,
  options: AudioSpeechAzureOptions,
  deps?: AudioSpeechAzureDeps,
): Promise<void> {
  const fetchFn = deps?.fetch ?? globalThis.fetch;
  const stdout = deps?.stdout ?? process.stdout;
  const stderr = deps?.stderr ?? process.stderr;
  const writeFile = deps?.writeFile ?? ((p: string, d: Buffer) => fs.writeFileSync(p, d));
  const stdoutIsTTY =
    deps?.stdoutIsTTY ?? Boolean((process.stdout as any).isTTY);

  let body = bodyArg ?? '';
  if (!body || body === '-') {
    body = await readStdin(deps?.stdin ?? process.stdin);
  }
  body = body.trim();
  if (!body) {
    throw new Error(
      'No input. Pass SSML (or plain text with --plain) as an argument or pipe it via stdin.',
    );
  }

  if (!options.output && stdoutIsTTY) {
    throw new Error(
      'Refusing to write binary audio to a terminal. Use `-o <path>` or pipe stdout to a file.',
    );
  }

  // Azure expects application/ssml+xml for SSML; text/plain works for raw text
  // when the deployment supports it. We sniff a leading `<` so users can pipe
  // SSML without thinking about the header, while `--plain` forces text/plain
  // even if the body starts with `<` for some reason.
  const looksLikeSsml = body.startsWith('<');
  const contentType =
    options.plain || !looksLikeSsml ? 'text/plain' : 'application/ssml+xml';

  const { token, routerUrl } = await getAccessToken(deps);

  const response = (await fetchFn(
    `${routerUrl}/v1/azure/texttospeech/cognitiveservices/v1`,
    {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${token}`,
        'Content-Type': contentType,
        'X-Microsoft-OutputFormat': options.format ?? DEFAULT_FORMAT,
      },
      body,
    },
  )) as Response;

  if (!response.ok) {
    const errBody = await response.text();
    stderr.write(`API error ${response.status} ${response.statusText}\n`);
    stderr.write(errBody);
    if (!errBody.endsWith('\n')) stderr.write('\n');
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
