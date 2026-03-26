import { describe, it, expect, vi, beforeEach } from 'vitest';
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
};

describe('Credentials', () => {
  let mockFs: Record<string, string>;
  let mockModes: Record<string, number>;

  function createCredentials() {
    return new Credentials({
      homedir: () => '/fakehome',
      existsSync: (p: string) => p in mockFs,
      readFileSync: (p: string) => {
        if (!(p in mockFs)) throw new Error('ENOENT');
        return mockFs[p];
      },
      writeFileSync: (p: string, data: string) => {
        mockFs[p] = data;
      },
      mkdirSync: () => undefined,
      chmodSync: (p: string, mode: number) => {
        mockModes[p] = mode;
      },
      unlinkSync: (p: string) => {
        delete mockFs[p];
      },
      statSync: (p: string) => {
        if (!(p in mockFs)) throw new Error('ENOENT');
        return { mode: (mockModes[p] ?? 0o644) | 0o100000 } as any;
      },
    });
  }

  beforeEach(() => {
    mockFs = {};
    mockModes = {};
  });

  it('returns null when file does not exist', () => {
    const creds = createCredentials();
    expect(creds.read()).toBeNull();
  });

  it('writes and reads credentials', () => {
    const creds = createCredentials();
    creds.write(sampleCredentials);
    const result = creds.read();
    expect(result).toEqual(sampleCredentials);
  });

  it('applies chmod 600 on write', () => {
    const creds = createCredentials();
    creds.write(sampleCredentials);
    const filePath = creds.getCredentialsPath();
    expect(mockModes[filePath]).toBe(0o600);
  });

  it('returns null and warns on JSON parse failure', () => {
    const creds = createCredentials();
    const filePath = creds.getCredentialsPath();
    mockFs[filePath] = 'not json{{{';

    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);
    const result = creds.read();
    expect(result).toBeNull();
    expect(stderrSpy).toHaveBeenCalledWith(expect.stringContaining('Failed to read credentials'));
    stderrSpy.mockRestore();
  });

  it('deletes credentials file', () => {
    const creds = createCredentials();
    creds.write(sampleCredentials);
    creds.delete();
    expect(creds.read()).toBeNull();
  });

  it('reports file mode correctly', () => {
    const creds = createCredentials();
    creds.write(sampleCredentials);
    expect(creds.getFileMode()).toBe(0o600);
  });

  it('reads credentials without router_url (backward compat)', () => {
    const creds = createCredentials();
    const { router_url, ...legacyCreds } = { ...sampleCredentials, router_url: undefined };
    const filePath = creds.getCredentialsPath();
    mockFs[filePath] = JSON.stringify(legacyCreds);
    const result = creds.read();
    expect(result).not.toBeNull();
    expect(result!.router_url).toBeUndefined();
    expect(result!.tenant_domain).toBe('test.stark.com');
  });
});
