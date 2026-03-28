import { describe, it, expect, vi, beforeEach } from 'vitest';
import { loginCommand } from '../commands/login';
import { Credentials, CredentialsData } from '../credentials';
import * as http from 'http';

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

function createMockFetch() {
  return async (url: string, init?: any) => {
    if (url.includes('.well-known/openid-configuration')) {
      return {
        ok: true, status: 200,
        json: async () => ({
          issuer: 'https://test.stark.com',
          authorization_endpoint: 'https://test.stark.com/oauth/authorize',
          token_endpoint: 'https://test.stark.com/oauth/token',
          router_url: 'https://api.monocle-ai.com',
        }),
      };
    }
    // Token endpoint
    return {
      ok: true, status: 200,
      json: async () => ({
        access_token: 'at_new',
        refresh_token: 'rt_new',
        id_token: 'eyJhbGciOiJSUzI1NiJ9.' + Buffer.from(JSON.stringify({
          email: 'user@test.com',
          tenant_name: 'Test Org',
        })).toString('base64url') + '.sig',
        expires_in: 3600,
      }),
    };
  };
}

describe('loginCommand', () => {
  it('completes full OIDC flow via mock server', async () => {
    const { instance, getStored } = createMockCredentials();
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);
    let capturedAuthUrl = '';

    const loginPromise = loginCommand(
      { tenantDomain: 'test.stark.com' },
      {
        credentials: instance,
        fetch: createMockFetch() as any,
        openBrowser: async (url: string) => {
          capturedAuthUrl = url;
          // Simulate callback
          const urlObj = new URL(url);
          const redirectUri = urlObj.searchParams.get('redirect_uri')!;
          const state = urlObj.searchParams.get('state')!;
          const callbackUrl = `${redirectUri}?code=auth_code_123&state=${state}`;

          // Small delay then make HTTP request to callback
          setTimeout(async () => {
            try {
              await fetch(callbackUrl);
            } catch {
              // It's ok if fetch isn't available, use http
              const cbUrl = new URL(callbackUrl);
              http.get(callbackUrl);
            }
          }, 50);
        },
      }
    );

    await loginPromise;

    // Verify auth URL params
    const authUrl = new URL(capturedAuthUrl);
    expect(authUrl.searchParams.get('client_id')).toBe('monocle-cli');
    expect(authUrl.searchParams.get('response_type')).toBe('code');
    expect(authUrl.searchParams.get('scope')).toBe('openid profile email');
    expect(authUrl.searchParams.get('code_challenge_method')).toBe('S256');
    expect(authUrl.searchParams.get('code_challenge')).toBeTruthy();
    expect(authUrl.searchParams.get('state')).toBeTruthy();
    expect(authUrl.searchParams.get('redirect_uri')).toContain('http://127.0.0.1:');
    expect(authUrl.searchParams.get('tenant')).toBe('test.stark.com');

    // Verify credentials saved
    const stored = getStored();
    expect(stored).not.toBeNull();
    expect(stored!.access_token).toBe('at_new');
    expect(stored!.refresh_token).toBe('rt_new');
    expect(stored!.email).toBe('user@test.com');
    expect(stored!.tenant_name).toBe('Test Org');
    expect(stored!.tenant_domain).toBe('test.stark.com');
    expect(stored!.router_url).toBe('https://api.monocle-ai.com');

    // Verify terminal output
    expect(stderrSpy).toHaveBeenCalledWith(expect.stringContaining('Logged in as user@test.com'));

    stderrSpy.mockRestore();
  });

  it('includes correct token exchange params (no client_secret)', async () => {
    const { instance } = createMockCredentials();
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);
    let tokenRequestBody = '';

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
      tokenRequestBody = init?.body ?? '';
      return {
        ok: true, status: 200,
        json: async () => ({
          access_token: 'at', refresh_token: 'rt',
          id_token: 'eyJhbGciOiJSUzI1NiJ9.' + Buffer.from('{"email":"a@b.com"}').toString('base64url') + '.s',
          expires_in: 3600,
        }),
      };
    };

    const loginPromise = loginCommand(
      { tenantDomain: 'test.stark.com' },
      {
        credentials: instance,
        fetch: mockFetch as any,
        openBrowser: async (url: string) => {
          const urlObj = new URL(url);
          const redirectUri = urlObj.searchParams.get('redirect_uri')!;
          const state = urlObj.searchParams.get('state')!;
          setTimeout(() => http.get(`${redirectUri}?code=code123&state=${state}`), 50);
        },
      }
    );

    await loginPromise;

    expect(tokenRequestBody).toContain('grant_type=authorization_code');
    expect(tokenRequestBody).toContain('code=code123');
    expect(tokenRequestBody).toContain('client_id=monocle-cli');
    expect(tokenRequestBody).toContain('code_verifier=');
    expect(tokenRequestBody).not.toContain('client_secret');

    stderrSpy.mockRestore();
  });

  it('rejects on state mismatch', async () => {
    const { instance } = createMockCredentials();
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    const loginPromise = loginCommand(
      { tenantDomain: 'test.stark.com' },
      {
        credentials: instance,
        fetch: createMockFetch() as any,
        openBrowser: async (url: string) => {
          const urlObj = new URL(url);
          const redirectUri = urlObj.searchParams.get('redirect_uri')!;
          setTimeout(() => http.get(`${redirectUri}?code=code123&state=wrong_state`), 50);
        },
      }
    );

    await expect(loginPromise).rejects.toThrow('State mismatch');
    stderrSpy.mockRestore();
  });
});
