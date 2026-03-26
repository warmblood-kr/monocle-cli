import { describe, it, expect, vi } from 'vitest';
import { setupCommand } from '../commands/setup';
import { Credentials, CredentialsData } from '../credentials';

const sampleCredentials: CredentialsData = {
  tenant_domain: 'test.stark.com',
  tenant_name: 'Test Org',
  email: 'user@test.com',
  access_token: 'at_123',
  refresh_token: 'rt_456',
  id_token: 'idt_789',
  access_token_expires_at: '2025-01-01T01:00:00.000Z',
  refresh_token_expires_at: '2025-01-31T00:00:00.000Z',
  router_url: 'https://api.monocle-ai.com',
};

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

describe('setupCommand', () => {
  it('sets apiKeyHelper and ANTHROPIC_BASE_URL', async () => {
    let written = '';
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await setupCommand({
      credentials: createMockCredentials(sampleCredentials),
      homedir: () => '/fakehome',
      existsSync: () => false,
      readFileSync: () => '{}',
      writeFileSync: (_p: string, data: string) => { written = data; },
      mkdirSync: () => undefined,
      env: {},
    });

    const settings = JSON.parse(written);
    expect(settings.apiKeyHelper).toBe('monocle token');
    expect(settings.env.ANTHROPIC_BASE_URL).toBe('https://api.monocle-ai.com');
    stderrSpy.mockRestore();
  });

  it('preserves existing settings', async () => {
    let written = '';
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await setupCommand({
      credentials: createMockCredentials(sampleCredentials),
      homedir: () => '/fakehome',
      existsSync: () => true,
      readFileSync: () => JSON.stringify({
        existingKey: 'value',
        env: { SOME_VAR: 'keep_me' },
      }),
      writeFileSync: (_p: string, data: string) => { written = data; },
      mkdirSync: () => undefined,
      env: {},
    });

    const settings = JSON.parse(written);
    expect(settings.existingKey).toBe('value');
    expect(settings.env.SOME_VAR).toBe('keep_me');
    expect(settings.apiKeyHelper).toBe('monocle token');
    expect(settings.env.ANTHROPIC_BASE_URL).toBe('https://api.monocle-ai.com');
    stderrSpy.mockRestore();
  });

  it('errors when not logged in', async () => {
    const exitSpy = vi.spyOn(process, 'exit').mockImplementation(() => { throw new Error('exit'); });
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await expect(setupCommand({
      credentials: createMockCredentials(null),
      homedir: () => '/fakehome',
      existsSync: () => false,
      readFileSync: () => '{}',
      writeFileSync: () => {},
      mkdirSync: () => undefined,
      env: {},
    })).rejects.toThrow('exit');

    expect(stderrSpy).toHaveBeenCalledWith(expect.stringContaining('Not logged in'));
    exitSpy.mockRestore();
    stderrSpy.mockRestore();
  });

  it('overwrites existing apiKeyHelper', async () => {
    let written = '';
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await setupCommand({
      credentials: createMockCredentials(sampleCredentials),
      homedir: () => '/fakehome',
      existsSync: () => true,
      readFileSync: () => JSON.stringify({ apiKeyHelper: 'old_helper' }),
      writeFileSync: (_p: string, data: string) => { written = data; },
      mkdirSync: () => undefined,
      env: {},
    });

    const settings = JSON.parse(written);
    expect(settings.apiKeyHelper).toBe('monocle token');
    stderrSpy.mockRestore();
  });

  it('warns about ANTHROPIC_API_KEY conflict', async () => {
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await setupCommand({
      credentials: createMockCredentials(sampleCredentials),
      homedir: () => '/fakehome',
      existsSync: () => false,
      readFileSync: () => '{}',
      writeFileSync: () => {},
      mkdirSync: () => undefined,
      env: { ANTHROPIC_API_KEY: 'sk-ant-xxx' },
    });

    expect(stderrSpy).toHaveBeenCalledWith(expect.stringContaining('ANTHROPIC_API_KEY'));
    stderrSpy.mockRestore();
  });

  it('uses router_url for localhost', async () => {
    let written = '';
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await setupCommand({
      credentials: createMockCredentials({
        ...sampleCredentials,
        tenant_domain: 'localhost:9000',
        router_url: 'http://localhost:8000',
      }),
      homedir: () => '/fakehome',
      existsSync: () => false,
      readFileSync: () => '{}',
      writeFileSync: (_p: string, data: string) => { written = data; },
      mkdirSync: () => undefined,
      env: {},
    });

    const settings = JSON.parse(written);
    expect(settings.env.ANTHROPIC_BASE_URL).toBe('http://localhost:8000');
    stderrSpy.mockRestore();
  });

  it('falls back to tenant_domain when router_url is absent', async () => {
    let written = '';
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    const { router_url, ...credsWithoutRouter } = sampleCredentials;
    await setupCommand({
      credentials: createMockCredentials(credsWithoutRouter as CredentialsData),
      homedir: () => '/fakehome',
      existsSync: () => false,
      readFileSync: () => '{}',
      writeFileSync: (_p: string, data: string) => { written = data; },
      mkdirSync: () => undefined,
      env: {},
    });

    const settings = JSON.parse(written);
    expect(settings.env.ANTHROPIC_BASE_URL).toBe('https://test.stark.com');
    expect(stderrSpy).toHaveBeenCalledWith(expect.stringContaining('router_url not found'));
    stderrSpy.mockRestore();
  });

  it('warns about ANTHROPIC_AUTH_TOKEN conflict', async () => {
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await setupCommand({
      credentials: createMockCredentials(sampleCredentials),
      homedir: () => '/fakehome',
      existsSync: () => false,
      readFileSync: () => '{}',
      writeFileSync: () => {},
      mkdirSync: () => undefined,
      env: { ANTHROPIC_AUTH_TOKEN: 'some-token' },
    });

    expect(stderrSpy).toHaveBeenCalledWith(expect.stringContaining('ANTHROPIC_AUTH_TOKEN'));
    stderrSpy.mockRestore();
  });
});
