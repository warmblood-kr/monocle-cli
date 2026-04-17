import { describe, it, expect, vi, afterEach } from 'vitest';
import { loginCommand, pollForToken } from '../commands/login';
import { Credentials, CredentialsData } from '../credentials';

function createMockCredentials() {
  let stored: CredentialsData | null = null;
  return {
    instance: {
      read: () => stored,
      write: (d: CredentialsData) => { stored = d; },
      delete: () => { stored = null; },
      getCredentialsPath: () => '/fake/.monocle/credentials.json',
      getCredentialsDir: () => '/fake/.monocle',
      getFileMode: () => 0o600,
    } as any as Credentials,
    getStored: () => stored,
  };
}

function createDeviceCodeMockFetch(tokenResponses?: Array<{ ok: boolean; status: number; json: () => Promise<any> }>) {
  let tokenCallIndex = 0;

  return async (url: string, _init?: any) => {
    if (url.includes('.well-known/openid-configuration')) {
      return {
        ok: true, status: 200,
        json: async () => ({
          issuer: 'https://test.stark.com',
          authorization_endpoint: 'https://test.stark.com/oauth/authorize',
          token_endpoint: 'https://test.stark.com/oauth/token',
          device_authorization_endpoint: 'https://test.stark.com/oauth/device',
          router_url: 'https://api.monocle-ai.com',
        }),
      };
    }

    if (url.includes('/oauth/device')) {
      return {
        ok: true, status: 200,
        json: async () => ({
          device_code: 'dev_code_123',
          user_code: 'ABCD-1234',
          verification_uri: 'https://test.stark.com/device',
          verification_uri_complete: 'https://test.stark.com/device?user_code=ABCD-1234',
          expires_in: 600,
          interval: 0.01, // Very short for tests
        }),
      };
    }

    // Token endpoint
    if (tokenResponses && tokenCallIndex < tokenResponses.length) {
      return tokenResponses[tokenCallIndex++];
    }

    // Default: return success
    return {
      ok: true, status: 200,
      json: async () => ({
        access_token: 'at_device',
        refresh_token: 'rt_device',
        id_token: 'eyJhbGciOiJSUzI1NiJ9.' + Buffer.from(JSON.stringify({
          email: 'user@test.com',
          tenant_name: 'Test Org',
          tenant_domain: 'test.stark.com',
        })).toString('base64url') + '.sig',
        expires_in: 3600,
      }),
    };
  };
}

describe('SSH environment detection', () => {
  const originalEnv = { ...process.env };

  afterEach(() => {
    process.env = { ...originalEnv };
  });

  it('detects SSH_CLIENT and uses device code flow', async () => {
    process.env.SSH_CLIENT = '192.168.1.1 12345 22';
    const { instance, getStored } = createMockCredentials();
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await loginCommand(
      { tenantDomain: 'test.stark.com' },
      {
        credentials: instance,
        fetch: createDeviceCodeMockFetch() as any,
        skipSetup: true,
      },
    );

    expect(getStored()).not.toBeNull();
    expect(getStored()!.access_token).toBe('at_device');
    expect(stderrSpy).toHaveBeenCalledWith(expect.stringContaining('ABCD-1234'));

    stderrSpy.mockRestore();
  });

  it('detects SSH_TTY and uses device code flow', async () => {
    process.env.SSH_TTY = '/dev/pts/0';
    const { instance, getStored } = createMockCredentials();
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await loginCommand(
      { tenantDomain: 'test.stark.com' },
      {
        credentials: instance,
        fetch: createDeviceCodeMockFetch() as any,
        skipSetup: true,
      },
    );

    expect(getStored()).not.toBeNull();
    expect(getStored()!.access_token).toBe('at_device');

    stderrSpy.mockRestore();
  });

  it('detects SSH_CONNECTION and uses device code flow', async () => {
    process.env.SSH_CONNECTION = '192.168.1.1 12345 192.168.1.2 22';
    const { instance, getStored } = createMockCredentials();
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await loginCommand(
      { tenantDomain: 'test.stark.com' },
      {
        credentials: instance,
        fetch: createDeviceCodeMockFetch() as any,
        skipSetup: true,
      },
    );

    expect(getStored()).not.toBeNull();
    expect(getStored()!.access_token).toBe('at_device');

    stderrSpy.mockRestore();
  });

  it('uses device code flow when --device-code flag is set', async () => {
    const { instance, getStored } = createMockCredentials();
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await loginCommand(
      { tenantDomain: 'test.stark.com', deviceCode: true },
      {
        credentials: instance,
        fetch: createDeviceCodeMockFetch() as any,
        skipSetup: true,
      },
    );

    expect(getStored()).not.toBeNull();
    expect(getStored()!.access_token).toBe('at_device');
    expect(getStored()!.email).toBe('user@test.com');
    expect(getStored()!.tenant_name).toBe('Test Org');

    stderrSpy.mockRestore();
  });
});

describe('pollForToken', () => {
  it('returns tokens on success', async () => {
    const mockFetch = async () => ({
      ok: true, status: 200,
      json: async () => ({
        access_token: 'at_success',
        refresh_token: 'rt_success',
        id_token: 'id_success',
        expires_in: 3600,
      }),
    });

    const result = await pollForToken(
      'https://test.stark.com/oauth/token',
      'dev_code',
      0.01, // very short interval for test
      600,
      'monocle-cli',
      mockFetch as any,
    );

    expect(result.access_token).toBe('at_success');
    expect(result.refresh_token).toBe('rt_success');
  });

  it('continues polling on authorization_pending', async () => {
    let callCount = 0;
    const mockFetch = async () => {
      callCount++;
      if (callCount < 3) {
        return {
          ok: false, status: 400,
          json: async () => ({ error: 'authorization_pending' }),
        };
      }
      return {
        ok: true, status: 200,
        json: async () => ({
          access_token: 'at_after_pending',
          refresh_token: 'rt_after_pending',
          expires_in: 3600,
        }),
      };
    };

    const result = await pollForToken(
      'https://test.stark.com/oauth/token',
      'dev_code',
      0.01,
      600,
      'monocle-cli',
      mockFetch as any,
    );

    expect(callCount).toBe(3);
    expect(result.access_token).toBe('at_after_pending');
  });

  it('increases interval on slow_down', async () => {
    // We verify slow_down handling by checking that:
    // 1. The function doesn't throw on slow_down
    // 2. It eventually returns the token
    // 3. The number of calls is correct
    // We use a tiny initial interval and mock setTimeout behavior
    let callCount = 0;

    const mockFetch = async () => {
      callCount++;
      if (callCount === 1) {
        return {
          ok: false, status: 400,
          json: async () => ({ error: 'slow_down' }),
        };
      }
      return {
        ok: true, status: 200,
        json: async () => ({
          access_token: 'at_slowed',
          expires_in: 3600,
        }),
      };
    };

    // Use vi.useFakeTimers to avoid actually waiting 5 seconds
    vi.useFakeTimers();

    const resultPromise = pollForToken(
      'https://test.stark.com/oauth/token',
      'dev_code',
      0.01, // initial interval: 10ms
      600,
      'monocle-cli',
      mockFetch as any,
    );

    // First sleep: 10ms (initial interval)
    await vi.advanceTimersByTimeAsync(10);
    // After slow_down, interval becomes 5.01s. Second sleep: 5010ms
    await vi.advanceTimersByTimeAsync(5100);

    const result = await resultPromise;

    expect(callCount).toBe(2);
    expect(result.access_token).toBe('at_slowed');

    vi.useRealTimers();
  });

  it('throws on expired_token', async () => {
    const mockFetch = async () => ({
      ok: false, status: 400,
      json: async () => ({ error: 'expired_token' }),
    });

    await expect(
      pollForToken(
        'https://test.stark.com/oauth/token',
        'dev_code',
        0.01,
        600,
        'monocle-cli',
        mockFetch as any,
      ),
    ).rejects.toThrow('Device code expired');
  });

  it('throws on access_denied', async () => {
    const mockFetch = async () => ({
      ok: false, status: 400,
      json: async () => ({ error: 'access_denied' }),
    });

    await expect(
      pollForToken(
        'https://test.stark.com/oauth/token',
        'dev_code',
        0.01,
        600,
        'monocle-cli',
        mockFetch as any,
      ),
    ).rejects.toThrow('Authorization request was denied');
  });

  it('surfaces HTTP status and raw body when server returns non-OAuth error', async () => {
    const htmlBody = '<!doctype html><html><head><title>Server Error (500)</title></head><body>oops</body></html>';
    const mockFetch = async () => ({
      ok: false,
      status: 500,
      json: async () => { throw new Error('not json'); },
      text: async () => htmlBody,
      clone: function() { return this; },
    });

    await expect(
      pollForToken(
        'https://test.stark.com/oauth/token',
        'dev_code',
        0.01,
        600,
        'monocle-cli',
        mockFetch as any,
      ),
    ).rejects.toThrow(/HTTP 500.*non-OAuth.*Server Error/);
  });
});
