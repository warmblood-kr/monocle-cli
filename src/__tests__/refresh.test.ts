import { describe, it, expect, vi, beforeEach } from 'vitest';
import { refreshAccessToken } from '../refresh';
import { Credentials, CredentialsData } from '../credentials';

const sampleCredentials: CredentialsData = {
  tenant_domain: 'test.stark.com',
  tenant_name: 'Test Org',
  email: 'user@test.com',
  access_token: 'old_access_token',
  refresh_token: 'old_refresh_token',
  id_token: 'eyJhbGciOiJSUzI1NiJ9.eyJlbWFpbCI6InVzZXJAdGVzdC5jb20iLCJ0ZW5hbnRfbmFtZSI6IlRlc3QgT3JnIn0.sig',
  access_token_expires_at: '2025-01-01T00:00:00.000Z',
  refresh_token_expires_at: '2025-01-31T00:00:00.000Z',
};

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

function mockDiscoveryAndTokenFetch(tokenResponse: any, tokenStatus = 200, routerUrl?: string | null) {
  const includeRouterUrl = routerUrl !== null;
  const effectiveRouterUrl = routerUrl ?? 'https://api.monocle-ai.com';
  return async (url: string, init?: any) => {
    if (url.includes('.well-known/openid-configuration')) {
      const discovery: any = {
        issuer: 'https://test.stark.com',
        authorization_endpoint: 'https://test.stark.com/oauth/authorize',
        token_endpoint: 'https://test.stark.com/oauth/token',
      };
      if (includeRouterUrl) {
        discovery.router_url = effectiveRouterUrl;
      }
      return {
        ok: true,
        status: 200,
        json: async () => discovery,
      };
    }
    return {
      ok: tokenStatus >= 200 && tokenStatus < 300,
      status: tokenStatus,
      json: async () => tokenResponse,
    };
  };
}

describe('refreshAccessToken', () => {
  it('successfully refreshes tokens', async () => {
    const { instance, getStored } = createMockCredentials();
    const mockFetch = mockDiscoveryAndTokenFetch({
      access_token: 'new_access_token',
      refresh_token: 'new_refresh_token',
      id_token: sampleCredentials.id_token,
      expires_in: 3600,
    });

    const result = await refreshAccessToken(sampleCredentials, {
      fetch: mockFetch as any,
      credentials: instance,
    });

    expect(result.success).toBe(true);
    expect(result.credentials?.access_token).toBe('new_access_token');
    expect(result.credentials?.refresh_token).toBe('new_refresh_token');
    expect(result.credentials?.router_url).toBe('https://api.monocle-ai.com');
    expect(getStored()?.access_token).toBe('new_access_token');
  });

  it('preserves existing router_url when discovery omits it', async () => {
    const { instance, getStored } = createMockCredentials();
    const credsWithRouter = { ...sampleCredentials, router_url: 'https://existing-router.com' };
    const mockFetch = mockDiscoveryAndTokenFetch({
      access_token: 'new_at',
      refresh_token: 'new_rt',
      id_token: sampleCredentials.id_token,
      expires_in: 3600,
    }, 200, null);

    const result = await refreshAccessToken(credsWithRouter, {
      fetch: mockFetch as any,
      credentials: instance,
    });

    expect(result.success).toBe(true);
    expect(result.credentials?.router_url).toBe('https://existing-router.com');
  });

  it('deletes credentials on 400 response', async () => {
    const { instance, getStored } = createMockCredentials();
    instance.write(sampleCredentials);
    const mockFetch = mockDiscoveryAndTokenFetch({ error: 'invalid_grant' }, 400);

    const result = await refreshAccessToken(sampleCredentials, {
      fetch: mockFetch as any,
      credentials: instance,
    });

    expect(result.success).toBe(false);
    expect(result.error).toContain('invalid or expired');
    expect(getStored()).toBeNull();
  });

  it('deletes credentials on 401 response', async () => {
    const { instance, getStored } = createMockCredentials();
    instance.write(sampleCredentials);
    const mockFetch = mockDiscoveryAndTokenFetch({ error: 'unauthorized' }, 401);

    const result = await refreshAccessToken(sampleCredentials, {
      fetch: mockFetch as any,
      credentials: instance,
    });

    expect(result.success).toBe(false);
    expect(result.error).toContain('invalid or expired');
    expect(getStored()).toBeNull();
  });

  it('does not include client_secret in request', async () => {
    const { instance } = createMockCredentials();
    let capturedBody = '';
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
      capturedBody = init?.body ?? '';
      return {
        ok: true, status: 200,
        json: async () => ({
          access_token: 'new_at', refresh_token: 'new_rt',
          id_token: sampleCredentials.id_token, expires_in: 3600,
        }),
      };
    };

    await refreshAccessToken(sampleCredentials, {
      fetch: mockFetch as any,
      credentials: instance,
    });

    expect(capturedBody).not.toContain('client_secret');
    expect(capturedBody).toContain('client_id=monocle-cli');
  });
});
