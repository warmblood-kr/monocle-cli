import { describe, it, expect, vi } from 'vitest';
import { Readable } from 'stream';
import { audioTranscribeAzureCommand } from '../commands/audio-transcribe-azure';
import { makeCredentialsStub } from './helpers/credentials-stub';
import { makeStream } from './helpers/streams';

describe('audioTranscribeAzureCommand', () => {
  it('POSTs to the Azure Fast endpoint with audio + definition parts', async () => {
    let capturedUrl = '';
    let capturedBody: any = null;
    const fetchFn = (async (url: string, init: any) => {
      capturedUrl = url;
      capturedBody = init.body;
      return {
        ok: true,
        status: 200,
        statusText: 'OK',
        text: async () => '{"combinedRecognizedPhrases":[{"display":"hi"}]}',
      };
    }) as any;

    const stdout = makeStream();
    const stderr = makeStream();

    await audioTranscribeAzureCommand(
      undefined,
      {
        locales: ['en-US', 'ko-KR'],
        diarization: true,
        profanity: 'Masked',
        channels: '0,1',
        filename: 'a.wav',
      },
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        stdin: Readable.from([Buffer.from('audiobytes')]) as any,
        stdout: stdout.out as any,
        stderr: stderr.out as any,
      },
    );

    expect(capturedUrl).toBe(
      'https://router.example.com/v1/speechtotext/transcriptions:transcribe',
    );
    expect(capturedBody).toBeInstanceOf(FormData);
    expect(capturedBody.get('audio')).toBeTruthy();
    const definitionPart = capturedBody.get('definition');
    expect(typeof definitionPart).toBe('string');
    const definition = JSON.parse(definitionPart as string);
    expect(definition).toMatchObject({
      locales: ['en-US', 'ko-KR'],
      diarizationEnabled: true,
      profanityFilterMode: 'Masked',
      channels: [0, 1],
    });
    expect(stdout.text()).toContain('combinedRecognizedPhrases');
  });

  it('refuses to send when no Azure-specific options are given', async () => {
    await expect(
      audioTranscribeAzureCommand(
        undefined,
        { filename: 'a.wav' },
        {
          credentials: makeCredentialsStub(),
          now: () => new Date('2026-05-11T00:00:00.000Z'),
          fetch: (async () => ({ ok: true, status: 200, statusText: 'OK', text: async () => '{}' })) as any,
          stdin: Readable.from([Buffer.from('x')]) as any,
          stdout: makeStream().out as any,
          stderr: makeStream().out as any,
        },
      ),
    ).rejects.toThrow(/requires a `definition` JSON/);
  });

  it('uses raw --definition JSON verbatim when provided', async () => {
    let capturedBody: any = null;
    const fetchFn = (async (_url: string, init: any) => {
      capturedBody = init.body;
      return { ok: true, status: 200, statusText: 'OK', text: async () => '{}' };
    }) as any;

    await audioTranscribeAzureCommand(
      undefined,
      {
        definition: '{"locales":["ja-JP"],"customProperty":true}',
        diarization: true, // should be ignored when --definition is set
        filename: 'a.wav',
      },
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        stdin: Readable.from([Buffer.from('x')]) as any,
        stdout: makeStream().out as any,
        stderr: makeStream().out as any,
      },
    );

    const def = JSON.parse(capturedBody.get('definition') as string);
    expect(def).toEqual({ locales: ['ja-JP'], customProperty: true });
  });

  it('rejects invalid --definition JSON', async () => {
    await expect(
      audioTranscribeAzureCommand(
        undefined,
        { definition: 'not-json', filename: 'a.wav' },
        {
          credentials: makeCredentialsStub(),
          now: () => new Date('2026-05-11T00:00:00.000Z'),
          fetch: (async () => ({ ok: true, status: 200, statusText: 'OK', text: async () => '' })) as any,
          stdin: Readable.from([Buffer.from('x')]) as any,
          stdout: makeStream().out as any,
          stderr: makeStream().out as any,
        },
      ),
    ).rejects.toThrow();
  });

  it('prints status + body to stderr and exits non-zero on API error', async () => {
    const fetchFn = (async () => ({
      ok: false,
      status: 415,
      statusText: 'Unsupported Media Type',
      text: async () => '{"error":"unsupported audio"}',
    })) as any;

    const exitSpy = vi
      .spyOn(process, 'exit')
      .mockImplementation(((code?: number) => {
        throw new Error(`exit:${code}`);
      }) as any);

    const stderr = makeStream();

    await expect(
      audioTranscribeAzureCommand(
        undefined,
        { filename: 'a.wav', locales: ['en-US'] },
        {
          credentials: makeCredentialsStub(),
          now: () => new Date('2026-05-11T00:00:00.000Z'),
          fetch: fetchFn,
          stdin: Readable.from([Buffer.from('x')]) as any,
          stdout: makeStream().out as any,
          stderr: stderr.out as any,
        },
      ),
    ).rejects.toThrow('exit:1');
    expect(stderr.text()).toContain('415');
    expect(stderr.text()).toContain('unsupported audio');
    exitSpy.mockRestore();
  });
});
