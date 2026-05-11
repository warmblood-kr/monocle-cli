import * as fs from 'fs';
import * as path from 'path';

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

export function readStdinBuffer(stdin: NodeJS.ReadableStream): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    stdin.on('data', (chunk: Buffer | string) => {
      chunks.push(typeof chunk === 'string' ? Buffer.from(chunk) : chunk);
    });
    stdin.on('end', () => resolve(Buffer.concat(chunks)));
    stdin.on('error', reject);
  });
}

export function readStdinText(stdin: NodeJS.ReadableStream): Promise<string> {
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

export interface AudioInputOptions {
  filename?: string;
  contentType?: string;
}

export interface AudioInputDeps {
  stdin?: NodeJS.ReadableStream;
  readFile?: (path: string) => Buffer;
  fileExists?: (path: string) => boolean;
}

export interface AudioInput {
  data: Buffer;
  filename: string;
  contentType: string;
}

export async function resolveAudioInput(
  fileArg: string | undefined,
  opts: AudioInputOptions,
  deps: AudioInputDeps,
): Promise<AudioInput> {
  const readFile = deps.readFile ?? ((p: string) => fs.readFileSync(p));
  const fileExists = deps.fileExists ?? ((p: string) => fs.existsSync(p));
  const stdin = deps.stdin ?? process.stdin;

  if (fileArg && fileArg !== '-') {
    if (!fileExists(fileArg)) {
      throw new Error(`Audio file not found: ${fileArg}`);
    }
    const ext = path.extname(fileArg).toLowerCase();
    return {
      data: readFile(fileArg),
      filename: opts.filename ?? path.basename(fileArg),
      contentType:
        opts.contentType ?? MIME_BY_EXT[ext] ?? 'application/octet-stream',
    };
  }

  const data = await readStdinBuffer(stdin);
  if (data.length === 0) {
    throw new Error(
      'No audio input. Pass a file path or pipe audio to stdin (`--filename` recommended when piping).',
    );
  }
  const filename = opts.filename ?? 'audio.wav';
  const ext = path.extname(filename).toLowerCase();
  return {
    data,
    filename,
    contentType:
      opts.contentType ?? MIME_BY_EXT[ext] ?? 'application/octet-stream',
  };
}

interface HttpResponseLike {
  ok: boolean;
  status: number;
  statusText: string;
  text: () => Promise<string>;
}

export async function writeApiErrorAndExit(
  response: HttpResponseLike,
  stderr: NodeJS.WritableStream,
): Promise<never> {
  const body = await response.text();
  stderr.write(`API error ${response.status} ${response.statusText}\n`);
  stderr.write(body);
  if (!body.endsWith('\n')) stderr.write('\n');
  process.exit(1);
}

export interface BinaryOutputDeps {
  stdout?: NodeJS.WritableStream;
  stderr?: NodeJS.WritableStream;
  writeFile?: (path: string, data: Buffer) => void;
  stdoutIsTTY?: boolean;
}

export function ensureNotWritingBinaryToTTY(
  output: string | undefined,
  deps: BinaryOutputDeps,
): void {
  const isTTY = deps.stdoutIsTTY ?? Boolean((process.stdout as any).isTTY);
  if (!output && isTTY) {
    throw new Error(
      'Refusing to write binary audio to a terminal. Use `-o <path>` or pipe stdout to a file.',
    );
  }
}

export function writeBinaryOutput(
  buffer: Buffer,
  output: string | undefined,
  deps: BinaryOutputDeps,
): void {
  const stdout = deps.stdout ?? process.stdout;
  const stderr = deps.stderr ?? process.stderr;
  const writeFile = deps.writeFile ?? ((p: string, d: Buffer) => fs.writeFileSync(p, d));

  if (output) {
    writeFile(output, buffer);
    stderr.write(`Wrote ${buffer.length} bytes to ${output}\n`);
  } else {
    stdout.write(buffer);
  }
}
