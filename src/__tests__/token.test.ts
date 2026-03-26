import { describe, it, expect, vi, beforeEach } from 'vitest';
import { tokenCommand } from '../commands/token';
import { Credentials, CredentialsData } from '../credentials';

const sampleCredentials: CredentialsData = {
  tenant_domain: 'test.stark.com',
  tenant_name: 'Test Org',
  email: 'user@test.com',
  access_token: 'valid_access_token',
  refresh_token: 'valid_refresh_token',
  id_token: 'idt_789',
  access_token_expires_at: new Date(Date.now() + 3600000).toISOString(), // 1h from now
  refresh_token_expires_at: new Date(Date.now() + 30 * 86400000).toISOString(),
};

function createMockCredentials(data: CredentialsData | null) {
  let stored = data;
  return {
    read: () => stored,
    write: (d: CredentialsData) => { stored = d; },
    delete: () => { stored = null; },
    getCredentialsPath: () => '/fake/.monocle/credentials.json',
    getCredentialsDir: () => '/fake/.monocle',
    getFileMode: () => 0o600,
  } as any as Credentials;
}

describe('tokenCommand', () => {
  it('outputs valid access token to stdout', async () => {
    const stdoutSpy = vi.spyOn(process.stdout, 'write').mockImplementation(() => true);

    await tokenCommand({
      credentials: createMockCredentials(sampleCredentials),
      now: () => new Date(),
    });

    expect(stdoutSpy).toHaveBeenCalledWith('valid_access_token');
    // Verify ONLY one stdout write (apiKeyHelper contract)
    expect(stdoutSpy).toHaveBeenCalledTimes(1);
    stdoutSpy.mockRestore();
  });

  it('refreshes expired token and outputs new one', async () => {
    const expiredCreds = {
      ...sampleCredentials,
      access_token: 'expired_token',
      access_token_expires_at: new Date(Date.now() - 60000).toISOString(), // expired 1 min ago
    };

    const stdoutSpy = vi.spyOn(process.stdout, 'write').mockImplementation(() => true);

    const mockFetch = async (url: string, init?: any) => {
      if (url.includes('.well-known')) {
        return {
          ok: true, status: 200,
          json: async () => ({
            issuer: 'https://test.stark.com',
            authorization_endpoint: 'https://test.stark.com/oauth/authorize',
            token_endpoint: 'https://test.stark.com/oauth/token',
          }),
        };
      }
      return {
        ok: true, status: 200,
        json: async () => ({
          access_token: 'refreshed_access_token',
          refresh_token: 'new_rt',
          id_token: 'new_idt',
          expires_in: 3600,
        }),
      };
    };

    const mockCreds = createMockCredentials(expiredCreds);

    await tokenCommand({
      credentials: mockCreds,
      now: () => new Date(),
      refreshDeps: { fetch: mockFetch as any, credentials: mockCreds },
    });

    expect(stdoutSpy).toHaveBeenCalledWith('refreshed_access_token');
    expect(stdoutSpy).toHaveBeenCalledTimes(1);
    stdoutSpy.mockRestore();
  });

  it('refreshes token within 5-minute buffer', async () => {
    const almostExpiredCreds = {
      ...sampleCredentials,
      access_token: 'almost_expired',
      access_token_expires_at: new Date(Date.now() + 4 * 60000).toISOString(), // 4 min from now (< 5 min buffer)
    };

    const stdoutSpy = vi.spyOn(process.stdout, 'write').mockImplementation(() => true);

    const mockFetch = async (url: string) => {
      if (url.includes('.well-known')) {
        return {
          ok: true, status: 200,
          json: async () => ({
            issuer: 'https://test.stark.com',
            authorization_endpoint: 'https://test.stark.com/oauth/authorize',
            token_endpoint: 'https://test.stark.com/oauth/token',
          }),
        };
      }
      return {
        ok: true, status: 200,
        json: async () => ({
          access_token: 'refreshed_token',
          refresh_token: 'new_rt',
          expires_in: 3600,
        }),
      };
    };

    const mockCreds = createMockCredentials(almostExpiredCreds);
    await tokenCommand({
      credentials: mockCreds,
      now: () => new Date(),
      refreshDeps: { fetch: mockFetch as any, credentials: mockCreds },
    });

    expect(stdoutSpy).toHaveBeenCalledWith('refreshed_token');
    stdoutSpy.mockRestore();
  });

  it('exits 1 when not logged in', async () => {
    const exitSpy = vi.spyOn(process, 'exit').mockImplementation(() => { throw new Error('exit'); });
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await expect(tokenCommand({
      credentials: createMockCredentials(null),
    })).rejects.toThrow('exit');

    expect(exitSpy).toHaveBeenCalledWith(1);
    expect(stderrSpy).toHaveBeenCalledWith(expect.stringContaining('Not logged in'));
    exitSpy.mockRestore();
    stderrSpy.mockRestore();
  });

  it('stdout purity - no other output besides token', async () => {
    const stdoutSpy = vi.spyOn(process.stdout, 'write').mockImplementation(() => true);
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await tokenCommand({
      credentials: createMockCredentials(sampleCredentials),
      now: () => new Date(),
    });

    // Only ONE stdout call with just the token
    const stdoutCalls = stdoutSpy.mock.calls;
    expect(stdoutCalls.length).toBe(1);
    expect(stdoutCalls[0][0]).toBe('valid_access_token');

    // No stderr output during normal operation
    expect(stderrSpy).not.toHaveBeenCalled();

    stdoutSpy.mockRestore();
    stderrSpy.mockRestore();
  });
});
