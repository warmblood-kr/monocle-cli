import { describe, it, expect } from 'vitest';
import { generateCodeVerifier, generateCodeChallenge, generateState, discoverOIDC, resolveStarkDomain } from '../oidc';
import * as crypto from 'crypto';

describe('PKCE', () => {
  it('generates code_verifier with correct length', () => {
    const verifier = generateCodeVerifier(64);
    expect(verifier.length).toBe(64);
  });

  it('generates code_verifier with default length', () => {
    const verifier = generateCodeVerifier();
    expect(verifier.length).toBe(64);
  });

  it('rejects verifier length < 43', () => {
    expect(() => generateCodeVerifier(42)).toThrow('length must be between 43 and 128');
  });

  it('rejects verifier length > 128', () => {
    expect(() => generateCodeVerifier(129)).toThrow('length must be between 43 and 128');
  });

  it('generates code_verifier with only unreserved characters', () => {
    const verifier = generateCodeVerifier(128);
    expect(verifier).toMatch(/^[A-Za-z0-9\-._~]+$/);
  });

  it('generates valid code_challenge (S256)', () => {
    const verifier = 'dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk';
    const challenge = generateCodeChallenge(verifier);
    // Verify it's base64url without padding
    expect(challenge).not.toContain('=');
    expect(challenge).not.toContain('+');
    expect(challenge).not.toContain('/');
    // Verify the hash manually
    const expectedHash = crypto.createHash('sha256').update(verifier, 'ascii').digest('base64url');
    expect(challenge).toBe(expectedHash);
  });

  it('generates unique state values', () => {
    const state1 = generateState();
    const state2 = generateState();
    expect(state1).not.toBe(state2);
    expect(state1.length).toBeGreaterThan(0);
  });
});

describe('resolveStarkDomain', () => {
  it('resolves stg tenant to stg Stark domain', () => {
    expect(resolveStarkDomain('stg-warmblood091803.monocle-ai.com')).toBe('stg.monocle-ai.com');
  });

  it('resolves production tenant to base domain', () => {
    expect(resolveStarkDomain('warmblood.monocle-ai.com')).toBe('monocle-ai.com');
  });

  it('throws on bare domain (no subdomain)', () => {
    expect(() => resolveStarkDomain('monocle-ai.com')).toThrow('Invalid tenant domain');
    expect(() => resolveStarkDomain('monocle-ai.com')).toThrow('Tenant must be a subdomain');
  });

  it('preserves localhost as-is', () => {
    expect(resolveStarkDomain('localhost:8080')).toBe('localhost:8080');
  });

  it('preserves 127.0.0.1 as-is', () => {
    expect(resolveStarkDomain('127.0.0.1:8080')).toBe('127.0.0.1:8080');
  });
});

describe('OIDC Discovery', () => {
  it('parses discovery document correctly', async () => {
    const mockFetch = async () => ({
      ok: true,
      status: 200,
      json: async () => ({
        issuer: 'https://test.stark.com',
        authorization_endpoint: 'https://test.stark.com/oauth/authorize',
        token_endpoint: 'https://test.stark.com/oauth/token',
        router_url: 'https://api.monocle-ai.com',
      }),
    });

    const config = await discoverOIDC('test.stark.com', { fetch: mockFetch as any });
    expect(config.issuer).toBe('https://test.stark.com');
    expect(config.authorization_endpoint).toBe('https://test.stark.com/oauth/authorize');
    expect(config.token_endpoint).toBe('https://test.stark.com/oauth/token');
    expect(config.router_url).toBe('https://api.monocle-ai.com');
  });

  it('handles missing router_url gracefully', async () => {
    const mockFetch = async () => ({
      ok: true,
      status: 200,
      json: async () => ({
        issuer: 'https://test.stark.com',
        authorization_endpoint: 'https://test.stark.com/oauth/authorize',
        token_endpoint: 'https://test.stark.com/oauth/token',
      }),
    });

    const config = await discoverOIDC('test.stark.com', { fetch: mockFetch as any });
    expect(config.router_url).toBeUndefined();
  });

  it('throws on HTTP error', async () => {
    const mockFetch = async () => ({
      ok: false,
      status: 404,
      json: async () => ({}),
    });

    await expect(discoverOIDC('bad.com', { fetch: mockFetch as any })).rejects.toThrow('OIDC Discovery failed (HTTP 404)');
  });

  it('throws on missing fields', async () => {
    const mockFetch = async () => ({
      ok: true,
      status: 200,
      json: async () => ({ issuer: 'https://test.com' }),
    });

    await expect(discoverOIDC('test.com', { fetch: mockFetch as any })).rejects.toThrow('missing required fields');
  });

  it('throws on connection error', async () => {
    const mockFetch = async () => { throw new Error('ECONNREFUSED'); };

    await expect(discoverOIDC('offline.com', { fetch: mockFetch as any })).rejects.toThrow('Failed to connect');
  });
});
