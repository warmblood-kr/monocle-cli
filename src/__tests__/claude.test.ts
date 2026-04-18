import { describe, it, expect, vi, beforeEach } from 'vitest';
import { claudeCommand } from '../commands/claude';
import { Credentials, CredentialsData } from '../credentials';
import { EventEmitter } from 'events';

function makeCredentials(overrides?: Partial<CredentialsData>): CredentialsData {
  return {
    tenant_domain: 'example.stark.com',
    tenant_name: 'Example Corp',
    email: 'user@example.com',
    access_token: 'at-valid',
    refresh_token: 'rt-valid',
    id_token: 'id-valid',
    access_token_expires_at: new Date(Date.now() + 3600_000).toISOString(),
    refresh_token_expires_at: new Date(Date.now() + 30 * 86400_000).toISOString(),
    router_url: 'https://warmblood.krmonocle-ai.com',
    ...overrides,
  };
}

function createMockCredentials(data: CredentialsData | null) {
  return {
    read: () => data,
    write: () => {},
    delete: () => {},
    getCredentialsPath: () => '/fake/.monocle/credentials.json',
    getCredentialsDir: () => '/fake/.monocle',
    getFileMode: () => 0o600,
  } as any as Credentials;
}

function makeMockSpawn() {
  const child = new EventEmitter();
  const spawnFn = vi.fn().mockReturnValue(child);
  return { spawnFn, child };
}

describe('claudeCommand', () => {
  let stderrSpy: ReturnType<typeof vi.spyOn>;
  let exitSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    stderrSpy = vi.spyOn(process.stderr, 'write').mockReturnValue(true);
    exitSpy = vi.spyOn(process, 'exit').mockImplementation(() => undefined as never);
  });

  it('should exit if not logged in', async () => {
    await claudeCommand([], { credentials: createMockCredentials(null) });

    expect(exitSpy).toHaveBeenCalledWith(1);
    expect(stderrSpy).toHaveBeenCalledWith(
      expect.stringContaining('Not logged in')
    );
  });

  it('should remove ANTHROPIC_API_KEY and ANTHROPIC_AUTH_TOKEN from child env', async () => {
    const { spawnFn } = makeMockSpawn();
    const env = {
      PATH: '/usr/bin',
      ANTHROPIC_API_KEY: 'sk-ant-xxx',
      ANTHROPIC_AUTH_TOKEN: 'bearer-xxx',
      HOME: '/home/user',
    };

    await claudeCommand([], { credentials: createMockCredentials(makeCredentials()), env, spawn: spawnFn, skipSetup: true });

    const childEnv = spawnFn.mock.calls[0][2].env;
    expect(childEnv.ANTHROPIC_API_KEY).toBeUndefined();
    expect(childEnv.ANTHROPIC_AUTH_TOKEN).toBeUndefined();
    expect(childEnv.PATH).toBe('/usr/bin');
    expect(childEnv.HOME).toBe('/home/user');
  });

  it('should set ANTHROPIC_BASE_URL from router_url', async () => {
    const { spawnFn } = makeMockSpawn();

    await claudeCommand([], {
      credentials: createMockCredentials(makeCredentials({ router_url: 'https://warmblood.krmonocle-ai.com' })),
      env: { PATH: '/usr/bin' },
      spawn: spawnFn,
      skipSetup: true,
    });

    const childEnv = spawnFn.mock.calls[0][2].env;
    expect(childEnv.ANTHROPIC_BASE_URL).toBe('https://warmblood.krmonocle-ai.com');
  });

  it('should fallback to tenant_domain if router_url is absent', async () => {
    const { spawnFn } = makeMockSpawn();

    await claudeCommand([], {
      credentials: createMockCredentials(makeCredentials({ router_url: undefined })),
      env: {},
      spawn: spawnFn,
      skipSetup: true,
    });

    const childEnv = spawnFn.mock.calls[0][2].env;
    expect(childEnv.ANTHROPIC_BASE_URL).toBe('https://example.stark.com');
  });

  it('should pass arguments through to claude', async () => {
    const { spawnFn } = makeMockSpawn();

    await claudeCommand(['--model', 'opus'], {
      credentials: createMockCredentials(makeCredentials()),
      env: {},
      spawn: spawnFn,
      skipSetup: true,
    });

    expect(spawnFn).toHaveBeenCalledWith(
      'claude',
      ['--model', 'opus'],
      expect.any(Object)
    );
  });

  it('should spawn claude with stdio inherit', async () => {
    const { spawnFn } = makeMockSpawn();

    await claudeCommand([], {
      credentials: createMockCredentials(makeCredentials()),
      env: {},
      spawn: spawnFn,
      skipSetup: true,
    });

    expect(spawnFn.mock.calls[0][2].stdio).toBe('inherit');
  });

  it('should handle claude not found (ENOENT)', async () => {
    const { spawnFn, child } = makeMockSpawn();

    await claudeCommand([], {
      credentials: createMockCredentials(makeCredentials()),
      env: {},
      spawn: spawnFn,
      skipSetup: true,
    });

    const error = new Error('spawn claude ENOENT') as NodeJS.ErrnoException;
    error.code = 'ENOENT';
    child.emit('error', error);

    expect(exitSpy).toHaveBeenCalledWith(1);
    expect(stderrSpy).toHaveBeenCalledWith(
      expect.stringContaining('claude` command not found')
    );
  });
});
