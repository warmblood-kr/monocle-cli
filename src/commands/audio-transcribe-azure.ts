import * as fs from 'fs';
import * as path from 'path';
import { Credentials } from '../credentials';
import { RefreshDeps } from '../refresh';
import { getAccessToken } from '../auth';

export interface AudioTranscribeAzureOptions {
  locales?: string[];
  diarization?: boolean;
  profanity?: string;
  channels?: string;
  definition?: string;
  filename?: string;
  contentType?: string;
}

export interface AudioTranscribeAzureDeps {
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
  opts: AudioTranscribeAzureOptions,
  deps: AudioTranscribeAzureDeps,
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
    return Promise.resolve({ data: readFile(fileArg), filename, contentType });
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

function buildDefinition(opts: AudioTranscribeAzureOptions): string | null {
  if (opts.definition) {
    // User-provided raw JSON wins — we still validate it parses to fail fast.
    JSON.parse(opts.definition);
    return opts.definition;
  }
  const def: Record<string, unknown> = {};
  if (opts.locales && opts.locales.length > 0) def.locales = opts.locales;
  if (opts.diarization) def.diarizationEnabled = true;
  if (opts.profanity) def.profanityFilterMode = opts.profanity;
  if (opts.channels) {
    def.channels = opts.channels
      .split(',')
      .map((c) => parseInt(c.trim(), 10))
      .filter((n) => !Number.isNaN(n));
  }
  return Object.keys(def).length > 0 ? JSON.stringify(def) : null;
}

export async function audioTranscribeAzureCommand(
  fileArg: string | undefined,
  options: AudioTranscribeAzureOptions,
  deps?: AudioTranscribeAzureDeps,
): Promise<void> {
  const fetchFn = deps?.fetch ?? globalThis.fetch;
  const stdout = deps?.stdout ?? process.stdout;
  const stderr = deps?.stderr ?? process.stderr;

  const definition = buildDefinition(options);
  if (!definition) {
    throw new Error(
      'Azure Fast Transcription requires a `definition` JSON. Pass at least one of:\n' +
        '  --locale <code>   (repeatable, e.g. --locale en-US --locale ko-KR)\n' +
        '  --diarization\n' +
        '  --profanity <None|Removed|Masked|Tags>\n' +
        '  --channels <0,1>\n' +
        '  --definition <raw JSON>',
    );
  }

  const { token, routerUrl } = await getAccessToken(deps);

  const { data, filename, contentType } = await resolveAudio(
    fileArg,
    options,
    deps ?? {},
  );

  const form = new FormData();
  form.append('audio', new Blob([data], { type: contentType }), filename);
  // The server expects `definition` as a plain string form field (Starlette
  // parses Blob parts as UploadFile and fails the `isinstance(..., str)`
  // check), so we append it directly without wrapping in a Blob.
  form.append('definition', definition);

  const response = await fetchFn(
    `${routerUrl}/v1/speechtotext/transcriptions:transcribe`,
    {
      method: 'POST',
      headers: { Authorization: `Bearer ${token}` },
      body: form,
    },
  );

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
