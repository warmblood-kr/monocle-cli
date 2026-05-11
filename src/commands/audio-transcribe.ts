import * as fs from 'fs';
import * as path from 'path';
import { Credentials } from '../credentials';
import { RefreshDeps } from '../refresh';
import { getAccessToken } from '../auth';

export interface AudioTranscribeOptions {
  model?: string;
  language?: string;
  prompt?: string;
  responseFormat?: string;
  temperature?: string;
  filename?: string;
  contentType?: string;
  azureFast?: boolean;
}

export interface AudioTranscribeDeps {
  credentials?: Credentials;
  refreshDeps?: RefreshDeps;
  fetch?: typeof globalThis.fetch;
  now?: () => Date;
  stdin?: NodeJS.ReadableStream;
  stdout?: NodeJS.WritableStream;
  stderr?: NodeJS.WritableStream;
  readFile?: (path: string) => Buffer;
  fileExists?: (path: string) => boolean;
}

const MIME_BY_EXT: Record<string, string> = {
  '.wav': 'audio/wav',
  '.mp3': 'audio/mpeg',
  '.mp4': 'audio/mp4',
  '.m4a': 'audio/mp4',
  '.aac': 'audio/aac',
  '.flac': 'audio/flac',
  '.ogg': 'audio/ogg',
  '.oga': 'audio/ogg',
  '.opus': 'audio/ogg',
  '.webm': 'audio/webm',
};

function readStdin(stdin: NodeJS.ReadableStream): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    stdin.on('data', (chunk: Buffer | string) => {
      chunks.push(typeof chunk === 'string' ? Buffer.from(chunk) : chunk);
    });
    stdin.on('end', () => resolve(Buffer.concat(chunks)));
    stdin.on('error', reject);
  });
}

function resolveAudio(
  fileArg: string | undefined,
  opts: AudioTranscribeOptions,
  deps: AudioTranscribeDeps,
): Promise<{ data: Buffer; filename: string; contentType: string }> {
  const readFile = deps.readFile ?? ((p: string) => fs.readFileSync(p));
  const fileExists = deps.fileExists ?? ((p: string) => fs.existsSync(p));
  const stdin = deps.stdin ?? process.stdin;

  if (fileArg && fileArg !== '-') {
    if (!fileExists(fileArg)) {
      throw new Error(`Audio file not found: ${fileArg}`);
    }
    const ext = path.extname(fileArg).toLowerCase();
    const filename = opts.filename ?? path.basename(fileArg);
    const contentType =
      opts.contentType ?? MIME_BY_EXT[ext] ?? 'application/octet-stream';
    return Promise.resolve({
      data: readFile(fileArg),
      filename,
      contentType,
    });
  }

  return readStdin(stdin).then((data) => {
    if (data.length === 0) {
      throw new Error(
        'No audio input. Pass a file path or pipe audio to stdin (`--filename` recommended when piping).',
      );
    }
    const filename = opts.filename ?? 'audio.wav';
    const ext = path.extname(filename).toLowerCase();
    const contentType =
      opts.contentType ?? MIME_BY_EXT[ext] ?? 'application/octet-stream';
    return { data, filename, contentType };
  });
}

export async function audioTranscribeCommand(
  fileArg: string | undefined,
  options: AudioTranscribeOptions,
  deps?: AudioTranscribeDeps,
): Promise<void> {
  const fetchFn = deps?.fetch ?? globalThis.fetch;
  const stdout = deps?.stdout ?? process.stdout;
  const stderr = deps?.stderr ?? process.stderr;

  const { token, routerUrl } = await getAccessToken(deps);

  const { data, filename, contentType } = await resolveAudio(
    fileArg,
    options,
    deps ?? {},
  );

  const endpointPath = options.azureFast
    ? '/v1/speechtotext/transcriptions:transcribe'
    : '/v1/audio/transcriptions';

  const form = new FormData();
  // Blob is the platform-portable way to attach a binary part. Node 18+ has it.
  form.append('file', new Blob([data], { type: contentType }), filename);
  if (options.model) form.append('model', options.model);
  if (options.language) form.append('language', options.language);
  if (options.prompt) form.append('prompt', options.prompt);
  if (options.responseFormat) form.append('response_format', options.responseFormat);
  if (options.temperature) form.append('temperature', options.temperature);

  const response = await fetchFn(`${routerUrl}${endpointPath}`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${token}` },
    body: form,
  });

  const body = await response.text();
  if (!response.ok) {
    stderr.write(`API error ${response.status} ${response.statusText}\n`);
    stderr.write(body);
    if (!body.endsWith('\n')) stderr.write('\n');
    process.exit(1);
  }

  stdout.write(body);
  if (!body.endsWith('\n')) stdout.write('\n');
}
