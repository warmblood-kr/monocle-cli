import { EventEmitter } from 'events';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { PassThrough } from 'stream';
import { describe, expect, it, vi } from 'vitest';
import { handoffCommand, listHostedChats } from '../commands/handoff';
import { Credentials, CredentialsData } from '../credentials';
import { Profiles } from '../profiles';

const data: CredentialsData = {
  tenant_domain: 'warmblood.monocle-ai.com',
  tenant_name: 'Warmblood',
  email: 'user@example.com',
  access_token: 'sensitive-bearer-token',
  refresh_token: 'sensitive-refresh-token',
  id_token: 'sensitive-id-token',
  access_token_expires_at: new Date(Date.now() + 60 * 60_000).toISOString(),
  refresh_token_expires_at: new Date(Date.now() + 30 * 86_400_000).toISOString(),
  router_url: 'https://api.monocle-ai.com',
};

const credentials = {
  read: () => data,
  getProfileName: () => 'warmblood',
  write: vi.fn(),
  delete: vi.fn(),
} as any as Credentials;

const profileStore = {
  getEnvironment: () => 'prod',
} as any as Profiles;

function response(value: unknown, status = 200): Response {
  return { ok: status >= 200 && status < 300, status, json: async () => value } as Response;
}

describe('hosted chat handoff', () => {
  it('lists only chat metadata using the selected profile bearer token', async () => {
    const fetch = vi.fn(async () => response([{ id: 'chat-1', title: 'Planning', updated_at: 123 }])) as any;
    const result = await listHostedChats('https://warmblood.monocle-ai.com', data.access_token, fetch);
    expect(fetch).toHaveBeenCalledWith('https://warmblood.monocle-ai.com/api/v1/chats/', expect.objectContaining({
      method: 'GET',
      headers: { Authorization: `Bearer ${data.access_token}`, Accept: 'application/json' },
    }));
    expect(result).toEqual([{ id: 'chat-1', title: 'Planning', updated_at: 123 }]);
  });

  it('requires authorization and does not include server error bodies in diagnostics', async () => {
    const fetch = vi.fn(async () => ({ ...response({ detail: 'sensitive-server-response' }, 403) })) as any;
    await expect(listHostedChats('https://warmblood.monocle-ai.com', data.access_token, fetch))
      .rejects.toThrow('not authorized for the selected profile (HTTP 403)');
    await expect(listHostedChats('https://warmblood.monocle-ai.com', data.access_token, fetch))
      .rejects.not.toThrow('sensitive-server-response');
  });

  it('passes one confirmed chat as a private temporary file and removes it after Claude exits', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'monocle-handoff-test-'));
    const stderr = new PassThrough();
    const logs: string[] = [];
    stderr.on('data', (chunk) => logs.push(chunk.toString()));
    const fetch = vi.fn(async (url: string) => {
      expect(url).toBe('https://warmblood.monocle-ai.com/api/v1/chats/chat%2Fone');
      return response({
        title: 'Quarterly planning',
        updated_at: 1_760_000_000,
        chat: { history: {
          currentId: 'a2',
          messages: {
            a1: { role: 'user', content: 'Plan the release.', parentId: null },
            a2: { role: 'assistant', content: 'Start with a checklist.', parentId: 'a1' },
          },
        } },
      });
    }) as any;
    const child = new EventEmitter();
    let launchedArgs: string[] = [];
    const spawn = vi.fn((command: string, args: string[]) => {
      expect(command).toBe('claude');
      launchedArgs = args;
      const contextPath = args[2].split(' ').at(-1)!;
      expect(fs.statSync(contextPath).mode & 0o777).toBe(0o600);
      const transcript = fs.readFileSync(contextPath, 'utf8');
      expect(transcript).toContain('Plan the release.');
      expect(transcript).toContain('Start with a checklist.');
      setTimeout(() => child.emit('close', 0), 0);
      return child;
    }) as any;

    await handoffCommand('chat/one', { confirm: true }, {
      credentials,
      profiles: profileStore,
      fetch,
      spawn,
      tempRoot: root,
      interactive: false,
      stderr,
    });

    expect(fetch).toHaveBeenCalledOnce();
    expect(spawn).toHaveBeenCalledOnce();
    expect(launchedArgs.slice(0, 2)[0]).toBe('--settings');
    expect(launchedArgs[1]).toContain('monocle token --profile warmblood');
    expect(launchedArgs[2]).not.toContain('Plan the release.');
    expect(fs.readdirSync(root)).toEqual([]);
    expect(logs.join('')).toContain('Environment: prod');
    expect(logs.join('')).not.toContain('sensitive-bearer-token');
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('does not launch Claude Code without explicit non-interactive confirmation', async () => {
    const fetch = vi.fn(async () => response({
      title: 'Title', chat: { history: { currentId: null, messages: {} } },
    })) as any;
    const spawn = vi.fn() as any;
    await handoffCommand('chat-1', {}, {
      credentials,
      profiles: profileStore,
      fetch,
      spawn,
      interactive: false,
      stderr: new PassThrough(),
    });
    expect(spawn).not.toHaveBeenCalled();
  });
});
