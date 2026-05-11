import { describe, it, expect, vi } from 'vitest';
import { Readable } from 'stream';
import { audioTranscribeCommand } from '../commands/audio-transcribe';
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
  out: { write: (chunk: string) => boolean };
  flushed: () => string;
} {
  let buf = '';
  return {
    out: {
      write: (chunk: string) => {
        buf += chunk;
        return true;
      },
    },
    flushed: () => buf,
  };
}

describe('audioTranscribeCommand', () => {
  it('POSTs multipart to /v1/audio/transcriptions and writes body to stdout', async () => {
    let capturedUrl = '';
    let capturedAuth = '';
    let capturedBody: any = null;
    const fetchFn = (async (url: string, init: any) => {
      capturedUrl = url;
      capturedAuth = init.headers.Authorization;
      capturedBody = init.body;
      return {
        ok: true,
        status: 200,
        statusText: 'OK',
        text: async () => '{"text":"hello world"}',
      };
    }) as any;

    const stdout = makeStream();
    const stderr = makeStream();

    await audioTranscribeCommand(
      undefined,
      { model: 'gpt-4o-mini-transcribe', language: 'en', filename: 'sample.wav' },
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        stdin: Readable.from([Buffer.from('RIFFfakedata')]) as any,
        stdout: stdout.out as any,
        stderr: stderr.out as any,
      },
    );

    expect(capturedUrl).toBe('https://router.example.com/v1/audio/transcriptions');
    expect(capturedAuth).toBe('Bearer access-abc');
    expect(capturedBody).toBeInstanceOf(FormData);
    expect(capturedBody.get('model')).toBe('gpt-4o-mini-transcribe');
    expect(capturedBody.get('language')).toBe('en');
    const filePart = capturedBody.get('file') as File | Blob;
    expect((filePart as any).type).toBe('audio/wav');
    expect(stdout.flushed()).toContain('hello world');
    expect(stderr.flushed()).toBe('');
  });

  it('switches endpoint when --azure-fast is set', async () => {
    let capturedUrl = '';
    const fetchFn = (async (url: string) => {
      capturedUrl = url;
      return {
        ok: true,
        status: 200,
        statusText: 'OK',
        text: async () => '{}',
      };
    }) as any;

    const stdout = makeStream();
    const stderr = makeStream();

    await audioTranscribeCommand(
      undefined,
      { azureFast: true, filename: 'a.wav' },
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        stdin: Readable.from([Buffer.from('data')]) as any,
        stdout: stdout.out as any,
        stderr: stderr.out as any,
      },
    );

    expect(capturedUrl).toBe(
      'https://router.example.com/v1/speechtotext/transcriptions:transcribe',
    );
  });

  it('reads from a file path argument with inferred content-type', async () => {
    let capturedBody: any = null;
    const fetchFn = (async (_url: string, init: any) => {
      capturedBody = init.body;
      return { ok: true, status: 200, statusText: 'OK', text: async () => 'ok' };
    }) as any;

    await audioTranscribeCommand(
      '/tmp/sample.mp3',
      {},
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        readFile: () => Buffer.from('id3fakempegdata'),
        fileExists: () => true,
        stdout: makeStream().out as any,
        stderr: makeStream().out as any,
      },
    );

    const filePart = capturedBody.get('file') as Blob;
    expect((filePart as any).type).toBe('audio/mpeg');
    expect((filePart as any).name ?? 'sample.mp3').toContain('sample.mp3');
  });

  it('prints status + body to stderr and exits non-zero on API error', async () => {
    const fetchFn = (async () => ({
      ok: false,
      status: 400,
      statusText: 'Bad Request',
      text: async () => '{"error":"invalid model"}',
    })) as any;

    const exitSpy = vi
      .spyOn(process, 'exit')
      .mockImplementation(((code?: number) => {
        throw new Error(`exit:${code}`);
      }) as any);

    const stdout = makeStream();
    const stderr = makeStream();

    await expect(
      audioTranscribeCommand(
        undefined,
        { filename: 'a.wav' },
        {
          credentials: makeCredentialsStub(),
          now: () => new Date('2026-05-11T00:00:00.000Z'),
          fetch: fetchFn,
          stdin: Readable.from([Buffer.from('x')]) as any,
          stdout: stdout.out as any,
          stderr: stderr.out as any,
        },
      ),
    ).rejects.toThrow('exit:1');

    expect(stderr.flushed()).toContain('400');
    expect(stderr.flushed()).toContain('invalid model');
    expect(stdout.flushed()).toBe('');
    exitSpy.mockRestore();
  });

  it('errors when stdin is empty and no file given', async () => {
    const stdout = makeStream();
    const stderr = makeStream();

    await expect(
      audioTranscribeCommand(
        undefined,
        {},
        {
          credentials: makeCredentialsStub(),
          now: () => new Date('2026-05-11T00:00:00.000Z'),
          fetch: (async () => ({ ok: true, status: 200, statusText: 'OK', text: async () => '' })) as any,
          stdin: Readable.from([]) as any,
          stdout: stdout.out as any,
          stderr: stderr.out as any,
        },
      ),
    ).rejects.toThrow(/No audio input/);
  });

  it('errors when file path does not exist', async () => {
    await expect(
      audioTranscribeCommand(
        '/nope/missing.wav',
        {},
        {
          credentials: makeCredentialsStub(),
          now: () => new Date('2026-05-11T00:00:00.000Z'),
          fetch: (async () => ({ ok: true, status: 200, statusText: 'OK', text: async () => '' })) as any,
          fileExists: () => false,
          stdout: makeStream().out as any,
          stderr: makeStream().out as any,
        },
      ),
    ).rejects.toThrow(/not found/);
  });
});
