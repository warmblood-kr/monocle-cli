import { describe, it, expect, vi } from 'vitest';
import { Readable } from 'stream';
import { chatCommand } from '../commands/chat';
import { makeCredentialsStub } from './helpers/credentials-stub';
import { makeStream } from './helpers/streams';

describe('chatCommand entrypoint header', () => {
  it('sends x-monocle-entrypoint: cli on chat-proxy requests', async () => {
    const capturedHeaders: any[] = [];
    const fetchFn = (async (_url: string, init: any) => {
      capturedHeaders.push(init?.headers ?? {});
      // First call is the model-list validation, second is the chat completion.
      if (String(_url).endsWith('/v1/models')) {
        return {
          ok: true,
          status: 200,
          json: async () => ({ data: [{ id: 'claude-sonnet-4-6' }] }),
        };
      }
      return {
        ok: true,
        status: 200,
        json: async () => ({ choices: [{ message: { content: 'hi there' } }] }),
      };
    }) as any;

    // Force non-interactive (stdin piped) path.
    const isTTY = process.stdin.isTTY;
    Object.defineProperty(process.stdin, 'isTTY', { value: false, configurable: true });
    const dataListeners: Array<(c: string) => void> = [];
    const endListeners: Array<() => void> = [];
    const onSpy = vi.spyOn(process.stdin, 'on').mockImplementation(((event: string, cb: any) => {
      if (event === 'data') dataListeners.push(cb);
      if (event === 'end') endListeners.push(cb);
      return process.stdin;
    }) as any);
    const setEncSpy = vi.spyOn(process.stdin, 'setEncoding').mockReturnValue(process.stdin as any);

    const stdout = makeStream();
    const stderr = makeStream();

    const promise = chatCommand(
      { model: 'claude-sonnet-4-6' },
      {
        credentials: makeCredentialsStub(),
        now: () => new Date('2026-05-11T00:00:00.000Z'),
        fetch: fetchFn,
        stdout: stdout.out,
        stderr: stderr.out,
      },
    );

    // Wait for chatCommand to reach readStdin() and register its listeners
    // (it awaits getAccessToken + model-list validation first), then drive them.
    await vi.waitFor(() => expect(dataListeners.length).toBeGreaterThan(0));
    dataListeners.forEach((cb) => cb('Hello'));
    endListeners.forEach((cb) => cb());

    await promise;

    Object.defineProperty(process.stdin, 'isTTY', { value: isTTY, configurable: true });
    onSpy.mockRestore();
    setEncSpy.mockRestore();

    // Every request to chat-proxy carries the entrypoint header.
    expect(capturedHeaders.length).toBeGreaterThan(0);
    for (const headers of capturedHeaders) {
      expect(headers['x-monocle-entrypoint']).toBe('cli');
    }
  });
});
