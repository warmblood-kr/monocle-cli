import { Credentials, CredentialsData } from '../../credentials';

export function makeCreds(overrides: Partial<CredentialsData> = {}): CredentialsData {
  return {
    tenant_domain: 'tenant.example.com',
    tenant_name: 'Tenant',
    email: 'user@tenant.com',
    access_token: 'access-abc',
    refresh_token: 'refresh-abc',
    id_token: 'id-abc',
    access_token_expires_at: '2099-01-01T00:00:00.000Z',
    refresh_token_expires_at: '2099-01-31T00:00:00.000Z',
    router_url: 'https://router.example.com',
    ...overrides,
  };
}

export function makeCredentialsStub(
  initial: CredentialsData | null = makeCreds(),
): Credentials {
  let stored = initial;
  return {
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
  } as unknown as Credentials;
}
