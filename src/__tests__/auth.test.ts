import { describe, it, expect, vi } from 'vitest';
import { getAccessToken } from '../auth';
import { Credentials, CredentialsData } from '../credentials';

function makeCreds(overrides: Partial<CredentialsData> = {}): CredentialsData {
  return {
    tenant_domain: 'tenant.monocle-ai.com',
    tenant_name: 'Tenant',
    email: 'user@tenant.com',
    access_token: 'access-abc',
    refresh_token: 'refresh-abc',
    id_token: 'id-abc',
    access_token_expires_at: '2099-01-01T00:00:00.000Z',
    refresh_token_expires_at: '2099-01-31T00:00:00.000Z',
    ...overrides,
  };
}

function makeCredentialsStub(initial: CredentialsData | null): {
  instance: Credentials;
  getStored: () => CredentialsData | null;
} {
  let stored = initial;
  const stub = {
    read: () => stored,
    write: (d: CredentialsData) => {
      stored = d;
    },
    delete: () => {
      stored = null;
    },
    getCredentialsPath: () => '/fake/.monocle/credentials.json',
    getCredentialsDir: () => '/fake/.monocle',
    getFileMode: () => 0o600,
  };
  return { instance: stub as unknown as Credentials, getStored: () => stored };
}

describe('getAccessToken', () => {
  it('returns token and router_url when credentials are fresh', async () => {
    const { instance } = makeCredentialsStub(
      makeCreds({ router_url: 'https://router.example.com' }),
    );

    const session = await getAccessToken({
      credentials: instance,
      now: () => new Date('2026-05-11T00:00:00.000Z'),
    });

    expect(session.token).toBe('access-abc');
    expect(session.routerUrl).toBe('https://router.example.com');
  });

  it('falls back to https://<tenant_domain> when no router_url', async () => {
    const { instance } = makeCredentialsStub(makeCreds());

    const session = await getAccessToken({
      credentials: instance,
      now: () => new Date('2026-05-11T00:00:00.000Z'),
    });

    expect(session.routerUrl).toBe('https://tenant.monocle-ai.com');
  });

  it('uses http for localhost tenants', async () => {
    const { instance } = makeCredentialsStub(
      makeCreds({ tenant_domain: 'localhost:8000' }),
    );

    const session = await getAccessToken({
      credentials: instance,
      now: () => new Date('2026-05-11T00:00:00.000Z'),
    });

    expect(session.routerUrl).toBe('http://localhost:8000');
  });

  it('refreshes the access token when near expiry', async () => {
    const { instance, getStored } = makeCredentialsStub(
      makeCreds({
        access_token: 'about-to-expire',
        access_token_expires_at: '2026-05-11T00:01:00.000Z',
      }),
    );

    const refreshFetch = async (url: string) => {
      if (url.includes('.well-known/openid-configuration')) {
        return {
          ok: true,
          status: 200,
          json: async () => ({
            issuer: 'https://tenant.monocle-ai.com',
            authorization_endpoint: 'https://tenant.monocle-ai.com/oauth/authorize',
            token_endpoint: 'https://tenant.monocle-ai.com/oauth/token',
            router_url: 'https://router.example.com',
          }),
        };
      }
      return {
        ok: true,
        status: 200,
        json: async () => ({
          access_token: 'refreshed-token',
          refresh_token: 'refresh-abc',
          id_token: 'id-abc',
          expires_in: 3600,
        }),
      };
    };

    const session = await getAccessToken({
      credentials: instance,
      now: () => new Date('2026-05-11T00:00:00.000Z'),
      refreshDeps: { fetch: refreshFetch as any },
    });

    expect(session.token).toBe('refreshed-token');
    expect(getStored()?.access_token).toBe('refreshed-token');
  });

  it('exits when no credentials exist', async () => {
    const { instance } = makeCredentialsStub(null);
    const exitSpy = vi
      .spyOn(process, 'exit')
      .mockImplementation(((code?: number) => {
        throw new Error(`exit:${code}`);
      }) as any);

    let stderrOut = '';
    const stderr = { write: (chunk: string) => (stderrOut += chunk, true) };

    await expect(
      getAccessToken({
        credentials: instance,
        stderr: stderr as any,
        now: () => new Date('2026-05-11T00:00:00.000Z'),
      }),
    ).rejects.toThrow('exit:1');
    expect(stderrOut).toContain('Not logged in');
    exitSpy.mockRestore();
  });
});
