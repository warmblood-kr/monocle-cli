import { describe, it, expect, vi } from 'vitest';
import { Readable } from 'stream';
import { audioSpeechCommand } from '../commands/audio-speech';
import { Credentials, CredentialsData } from '../credentials';

function makeCreds(overrides: Partial<CredentialsData> = {}): CredentialsData {
  return {
    tenant_domain: 'tenant.example.com',
    tenant_name: 'Tenant',
    email: 'user@tenant.com',
    access_token: 'access-abc',
    refresh_token: 'refresh-abc',
    id_token: 'id-abc',
    access_token_expires_at: '2099-01-01T00:00:00.000Z',
    refresh_token_expires_at: '2099-01-31T00:00:00.000Z',
    router_url: 'https://router.example.com',
    ...overrides,
  };
}

function makeCredentialsStub(initial: CredentialsData | null = makeCreds()) {
  let stored = initial;
  return {
    read: () => stored,
    write: (d: CredentialsData) => {
      stored = d;
    },
    delete: () => {
      stored = null;
    },
    getCredentialsPath: () => '/fake/.monocle/credentials.json',
    getCredentialsDir: () => '/fake/.monocle',
    getFileMode: () => 0o600,
  } as unknown as Credentials;
}

function makeStream(): {
  out: NodeJS.WritableStream;
  text: () => string;
  bytes: () => Buffer;
} {
  const chunks: Buffer[] = [];
  return {
    out: {
      write: (chunk: any) => {
        chunks.push(typeof chunk === 'string' ? Buffer.from(chunk) : chunk);
        return true;
      },
    } as any,
    text: () => Buffer.concat(chunks).toString('utf-8'),
    bytes: () => Buffer.concat(chunks),
  };
}

describe('audioSpeechCommand', () => {
  it('POSTs JSON to /v1/audio/speech and writes binary to -o file', async () => {
    let capturedUrl = '';
    let capturedHeaders: any = null;
    let capturedBody: any = null;
    const fetchFn = (async (url: string, init: any) => {
      capturedUrl = url;
      capturedHeaders = init.headers;
      capturedBody = init.body;
      return {
        ok: true,
        status: 200,
        statusText: 'OK',
        arrayBuffer: async () => new Uint8Array([0x49, 0x44, 0x33]).buffer,
      };
    }) as any;

    let writtenPath = '';
    let writtenBytes: Buffer = Buffer.alloc(0);
    const stderr = makeStream();

    await audioSpeechCommand(
      'hello world',
      { output: '/tmp/out.mp3', model: 'gpt-4o-mini-tts', voice: 'nova', format: 'mp3', speed: '1.2' },
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        writeFile: (p, b) => {
          writtenPath = p;
          writtenBytes = b;
        },
        stdoutIsTTY: false,
        stderr: stderr.out,
      },
    );

    expect(capturedUrl).toBe('https://router.example.com/v1/audio/speech');
    expect(capturedHeaders.Authorization).toBe('Bearer access-abc');
    expect(capturedHeaders['Content-Type']).toBe('application/json');
    const payload = JSON.parse(capturedBody);
    expect(payload).toMatchObject({
      model: 'gpt-4o-mini-tts',
      voice: 'nova',
      input: 'hello world',
      response_format: 'mp3',
      speed: 1.2,
    });
    expect(writtenPath).toBe('/tmp/out.mp3');
    expect(writtenBytes.equals(Buffer.from([0x49, 0x44, 0x33]))).toBe(true);
    expect(stderr.text()).toContain('Wrote 3 bytes');
  });

  it('writes binary to stdout when -o not given and stdout is piped', async () => {
    const fetchFn = (async () => ({
      ok: true,
      status: 200,
      statusText: 'OK',
      arrayBuffer: async () => new Uint8Array([1, 2, 3, 4]).buffer,
    })) as any;

    const stdout = makeStream();
    const stderr = makeStream();

    await audioSpeechCommand(
      'hi',
      {},
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        stdoutIsTTY: false,
        stdout: stdout.out,
        stderr: stderr.out,
      },
    );

    expect(stdout.bytes().equals(Buffer.from([1, 2, 3, 4]))).toBe(true);
  });

  it('refuses to dump binary to a TTY without -o', async () => {
    await expect(
      audioSpeechCommand(
        'hi',
        {},
        {
          credentials: makeCredentialsStub(),
          now: () => new Date('2026-05-11T00:00:00.000Z'),
          fetch: (async () => ({ ok: true, status: 200, statusText: 'OK', arrayBuffer: async () => new ArrayBuffer(0) })) as any,
          stdoutIsTTY: true,
          stdout: makeStream().out,
          stderr: makeStream().out,
        },
      ),
    ).rejects.toThrow(/binary audio to a terminal/);
  });

  it('reads text from stdin when no argument is given', async () => {
    let capturedBody: any = null;
    const fetchFn = (async (_url: string, init: any) => {
      capturedBody = init.body;
      return { ok: true, status: 200, statusText: 'OK', arrayBuffer: async () => new ArrayBuffer(0) };
    }) as any;

    await audioSpeechCommand(
      undefined,
      { output: '/tmp/out.mp3' },
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        stdin: Readable.from(['piped input']) as any,
        writeFile: () => {},
        stdoutIsTTY: false,
        stderr: makeStream().out,
      },
    );

    expect(JSON.parse(capturedBody).input).toBe('piped input');
  });

  it('with --azure, posts raw SSML and sets X-Microsoft-OutputFormat', async () => {
    let capturedUrl = '';
    let capturedHeaders: any = null;
    let capturedBody: any = null;
    const fetchFn = (async (url: string, init: any) => {
      capturedUrl = url;
      capturedHeaders = init.headers;
      capturedBody = init.body;
      return { ok: true, status: 200, statusText: 'OK', arrayBuffer: async () => new ArrayBuffer(0) };
    }) as any;

    await audioSpeechCommand(
      '<speak version="1.0"><voice name="en-US-Jenny">hi</voice></speak>',
      { azure: true, format: 'audio-16khz-32kbitrate-mono-mp3', output: '/tmp/o.mp3' },
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        writeFile: () => {},
        stdoutIsTTY: false,
        stderr: makeStream().out,
      },
    );

    expect(capturedUrl).toBe(
      'https://router.example.com/v1/azure/text-to-speech/cognitiveservices/v1',
    );
    expect(capturedHeaders['Content-Type']).toBe('application/ssml+xml');
    expect(capturedHeaders['X-Microsoft-OutputFormat']).toBe(
      'audio-16khz-32kbitrate-mono-mp3',
    );
    expect(capturedBody).toContain('<speak');
  });

  it('prints status + body to stderr and exits non-zero on API error', async () => {
    const fetchFn = (async () => ({
      ok: false,
      status: 401,
      statusText: 'Unauthorized',
      text: async () => '{"error":"bad token"}',
    })) as any;

    const exitSpy = vi
      .spyOn(process, 'exit')
      .mockImplementation(((code?: number) => {
        throw new Error(`exit:${code}`);
      }) as any);

    const stderr = makeStream();

    await expect(
      audioSpeechCommand(
        'hi',
        { output: '/tmp/o.mp3' },
        {
          credentials: makeCredentialsStub(),
          now: () => new Date('2026-05-11T00:00:00.000Z'),
          fetch: fetchFn,
          stdoutIsTTY: false,
          stderr: stderr.out,
          writeFile: () => {},
        },
      ),
    ).rejects.toThrow('exit:1');

    expect(stderr.text()).toContain('401');
    expect(stderr.text()).toContain('bad token');
    exitSpy.mockRestore();
  });
});
