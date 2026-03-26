import { describe, it, expect, vi } from 'vitest';
import { statusCommand } from '../commands/status';
import { Credentials, CredentialsData } from '../credentials';

const now = new Date('2025-01-15T12:00:00.000Z');

const validCredentials: CredentialsData = {
  tenant_domain: 'test.stark.com',
  tenant_name: 'Test Org',
  email: 'user@test.com',
  access_token: 'at_123',
  refresh_token: 'rt_456',
  id_token: 'idt_789',
  access_token_expires_at: new Date(now.getTime() + 45 * 60000).toISOString(), // 45 min
  refresh_token_expires_at: new Date(now.getTime() + 25 * 86400000).toISOString(), // 25 days
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

describe('statusCommand', () => {
  it('shows not logged in when no credentials', async () => {
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);
    await statusCommand({
      credentials: createMockCredentials(null),
      homedir: () => '/fakehome',
      existsSync: () => false,
      readFileSync: () => '{}',
      now: () => now,
    });

    expect(stderrSpy).toHaveBeenCalledWith('Not logged in.\n');
    stderrSpy.mockRestore();
  });

  it('shows valid token status', async () => {
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);
    await statusCommand({
      credentials: createMockCredentials(validCredentials),
      homedir: () => '/fakehome',
      existsSync: () => false,
      readFileSync: () => '{}',
      now: () => now,
    });

    const output = stderrSpy.mock.calls.map(c => c[0]).join('');
    expect(output).toContain('Tenant: test.stark.com (Test Org)');
    expect(output).toContain('User: user@test.com');
    expect(output).toContain('Access Token: Valid');
    expect(output).toContain('Refresh Token: Valid');
    expect(output).toContain('Not configured');
    stderrSpy.mockRestore();
  });

  it('shows expired access token', async () => {
    const expiredCreds = {
      ...validCredentials,
      access_token_expires_at: new Date(now.getTime() - 60000).toISOString(), // expired 1 min ago
    };
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);
    await statusCommand({
      credentials: createMockCredentials(expiredCreds),
      homedir: () => '/fakehome',
      existsSync: () => false,
      readFileSync: () => '{}',
      now: () => now,
    });

    const output = stderrSpy.mock.calls.map(c => c[0]).join('');
    expect(output).toContain('Access Token: Expired');
    stderrSpy.mockRestore();
  });

  it('shows expired refresh token with re-login message', async () => {
    const expiredCreds = {
      ...validCredentials,
      refresh_token_expires_at: new Date(now.getTime() - 86400000).toISOString(), // expired 1 day ago
    };
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);
    await statusCommand({
      credentials: createMockCredentials(expiredCreds),
      homedir: () => '/fakehome',
      existsSync: () => false,
      readFileSync: () => '{}',
      now: () => now,
    });

    const output = stderrSpy.mock.calls.map(c => c[0]).join('');
    expect(output).toContain('Refresh Token: Expired');
    expect(output).toContain('monocle login');
    stderrSpy.mockRestore();
  });

  it('shows Claude Code as configured when apiKeyHelper is set', async () => {
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);
    await statusCommand({
      credentials: createMockCredentials(validCredentials),
      homedir: () => '/fakehome',
      existsSync: () => true,
      readFileSync: () => JSON.stringify({ apiKeyHelper: 'monocle token' }),
      now: () => now,
    });

    const output = stderrSpy.mock.calls.map(c => c[0]).join('');
    expect(output).toContain('Claude Code: Configured');
    stderrSpy.mockRestore();
  });
});
