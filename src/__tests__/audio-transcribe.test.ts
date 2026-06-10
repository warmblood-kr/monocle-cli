import { describe, it, expect, vi } from 'vitest';
import { Readable } from 'stream';
import { audioTranscribeCommand } from '../commands/audio-transcribe';
import { makeCredentialsStub } from './helpers/credentials-stub';
import { makeStream } from './helpers/streams';

describe('audioTranscribeCommand', () => {
  it('POSTs multipart to /v1/audio/transcriptions and writes body to stdout', async () => {
    let capturedUrl = '';
    let capturedAuth = '';
    let capturedOrigin = '';
    let capturedBody: any = null;
    const fetchFn = (async (url: string, init: any) => {
      capturedUrl = url;
      capturedAuth = init.headers.Authorization;
      capturedOrigin = init.headers['x-monocle-origin'];
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
    expect(capturedOrigin).toBe('cli');
    expect(capturedBody).toBeInstanceOf(FormData);
    expect(capturedBody.get('model')).toBe('gpt-4o-mini-transcribe');
    expect(capturedBody.get('language')).toBe('en');
    const filePart = capturedBody.get('file') as File | Blob;
    expect((filePart as any).type).toBe('audio/wav');
    expect(stdout.text()).toContain('hello world');
    expect(stderr.text()).toBe('');
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
    expect((filePart as any).name).toBe('sample.mp3');
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

    expect(stderr.text()).toContain('400');
    expect(stderr.text()).toContain('invalid model');
    expect(stdout.text()).toBe('');
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
