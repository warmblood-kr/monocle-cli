import { describe, it, expect, vi } from 'vitest';
import { Readable } from 'stream';
import { audioSpeechAzureCommand } from '../commands/audio-speech-azure';
import { Credentials, CredentialsData } from '../credentials';

function makeCreds(): CredentialsData {
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
  };
}

function makeCredentialsStub() {
  let stored: CredentialsData | null = makeCreds();
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

function makeStream() {
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

describe('audioSpeechAzureCommand', () => {
  it('posts SSML body with application/ssml+xml and X-Microsoft-OutputFormat', async () => {
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
        arrayBuffer: async () => new Uint8Array([1, 2, 3]).buffer,
      };
    }) as any;

    let writtenPath = '';
    let writtenBytes = Buffer.alloc(0);

    await audioSpeechAzureCommand(
      '<speak version="1.0" xml:lang="en-US"><voice name="en-US-JennyNeural">hi</voice></speak>',
      { output: '/tmp/o.mp3', format: 'audio-16khz-32kbitrate-mono-mp3' },
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        writeFile: (p, b) => {
          writtenPath = p;
          writtenBytes = b;
        },
        stdoutIsTTY: false,
        stderr: makeStream().out,
      },
    );

    expect(capturedUrl).toBe(
      'https://router.example.com/v1/azure/text-to-speech/cognitiveservices/v1',
    );
    expect(capturedHeaders.Authorization).toBe('Bearer access-abc');
    expect(capturedHeaders['Content-Type']).toBe('application/ssml+xml');
    expect(capturedHeaders['X-Microsoft-OutputFormat']).toBe(
      'audio-16khz-32kbitrate-mono-mp3',
    );
    expect(capturedBody).toContain('<speak');
    expect(writtenPath).toBe('/tmp/o.mp3');
    expect(writtenBytes.equals(Buffer.from([1, 2, 3]))).toBe(true);
  });

  it('uses text/plain when body does not look like SSML', async () => {
    let capturedHeaders: any = null;
    const fetchFn = (async (_url: string, init: any) => {
      capturedHeaders = init.headers;
      return { ok: true, status: 200, statusText: 'OK', arrayBuffer: async () => new ArrayBuffer(0) };
    }) as any;

    await audioSpeechAzureCommand(
      'plain text body',
      { output: '/tmp/o.mp3' },
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        writeFile: () => {},
        stdoutIsTTY: false,
        stderr: makeStream().out,
      },
    );

    expect(capturedHeaders['Content-Type']).toBe('text/plain');
  });

  it('--plain forces text/plain even when body starts with `<`', async () => {
    let capturedHeaders: any = null;
    const fetchFn = (async (_url: string, init: any) => {
      capturedHeaders = init.headers;
      return { ok: true, status: 200, statusText: 'OK', arrayBuffer: async () => new ArrayBuffer(0) };
    }) as any;

    await audioSpeechAzureCommand(
      '<not-really-ssml>',
      { plain: true, output: '/tmp/o.mp3' },
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        writeFile: () => {},
        stdoutIsTTY: false,
        stderr: makeStream().out,
      },
    );

    expect(capturedHeaders['Content-Type']).toBe('text/plain');
  });

  it('falls back to the default X-Microsoft-OutputFormat', async () => {
    let capturedHeaders: any = null;
    const fetchFn = (async (_url: string, init: any) => {
      capturedHeaders = init.headers;
      return { ok: true, status: 200, statusText: 'OK', arrayBuffer: async () => new ArrayBuffer(0) };
    }) as any;

    await audioSpeechAzureCommand(
      '<speak></speak>',
      { output: '/tmp/o.mp3' },
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        writeFile: () => {},
        stdoutIsTTY: false,
        stderr: makeStream().out,
      },
    );

    expect(capturedHeaders['X-Microsoft-OutputFormat']).toBe(
      'audio-24khz-48kbitrate-mono-mp3',
    );
  });

  it('reads SSML from stdin when no argument is given', async () => {
    let capturedBody: any = null;
    const fetchFn = (async (_url: string, init: any) => {
      capturedBody = init.body;
      return { ok: true, status: 200, statusText: 'OK', arrayBuffer: async () => new ArrayBuffer(0) };
    }) as any;

    await audioSpeechAzureCommand(
      undefined,
      { output: '/tmp/o.mp3' },
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        stdin: Readable.from(['<speak>piped</speak>']) as any,
        writeFile: () => {},
        stdoutIsTTY: false,
        stderr: makeStream().out,
      },
    );

    expect(capturedBody).toContain('<speak>piped</speak>');
  });

  it('refuses to dump binary to a TTY without -o', async () => {
    await expect(
      audioSpeechAzureCommand(
        '<speak></speak>',
        {},
        {
          credentials: makeCredentialsStub(),
          now: () => new Date('2026-05-11T00:00:00.000Z'),
          fetch: (async () => ({ ok: true, status: 200, statusText: 'OK', arrayBuffer: async () => new ArrayBuffer(0) })) as any,
          stdoutIsTTY: true,
          stderr: makeStream().out,
        },
      ),
    ).rejects.toThrow(/binary audio to a terminal/);
  });

  it('prints status + body to stderr and exits non-zero on API error', async () => {
    const fetchFn = (async () => ({
      ok: false,
      status: 400,
      statusText: 'Bad Request',
      text: async () => 'Invalid SSML',
    })) as any;

    const exitSpy = vi
      .spyOn(process, 'exit')
      .mockImplementation(((code?: number) => {
        throw new Error(`exit:${code}`);
      }) as any);

    const stderr = makeStream();

    await expect(
      audioSpeechAzureCommand(
        '<bad/>',
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

    expect(stderr.text()).toContain('400');
    expect(stderr.text()).toContain('Invalid SSML');
    exitSpy.mockRestore();
  });
});
