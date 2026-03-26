import * as crypto from 'crypto';

export interface OIDCConfig {
  issuer: string;
  authorization_endpoint: string;
  token_endpoint: string;
  router_url?: string;
}

export interface OIDCDeps {
  fetch?: (url: string) => Promise<{ ok: boolean; status: number; json: () => Promise<any> }>;
}

/**
 * Generate a PKCE code_verifier (43-128 chars, unreserved characters per RFC 7636)
 */
export function generateCodeVerifier(length: number = 64): string {
  if (length < 43 || length > 128) {
    throw new Error('code_verifier length must be between 43 and 128');
  }
  const unreserved = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~';
  const bytes = crypto.randomBytes(length);
  let result = '';
  for (let i = 0; i < length; i++) {
    result += unreserved[bytes[i] % unreserved.length];
  }
  return result;
}

/**
 * Generate PKCE code_challenge using S256: BASE64URL(SHA256(code_verifier))
 */
export function generateCodeChallenge(verifier: string): string {
  const hash = crypto.createHash('sha256').update(verifier, 'ascii').digest();
  return hash.toString('base64url');
}

/**
 * Generate a random state string for CSRF prevention
 */
export function generateState(): string {
  return crypto.randomBytes(32).toString('base64url');
}

/**
 * Discover OIDC endpoints from tenant domain
 */
export async function discoverOIDC(
  tenantDomain: string,
  deps?: OIDCDeps
): Promise<OIDCConfig> {
  const fetchFn = deps?.fetch ?? globalThis.fetch;
  const scheme = tenantDomain.startsWith('localhost') || tenantDomain.startsWith('127.0.0.1') ? 'http' : 'https';
  const url = `${scheme}://${tenantDomain}/.well-known/openid-configuration`;

  let response: any;
  try {
    response = await fetchFn(url);
  } catch (err: any) {
    throw new Error(`Failed to connect to OIDC provider at ${tenantDomain}: ${err.message}`);
  }

  if (!response.ok) {
    throw new Error(`OIDC Discovery failed (HTTP ${response.status}) for ${tenantDomain}`);
  }

  const config = await response.json();

  if (!config.authorization_endpoint || !config.token_endpoint || !config.issuer) {
    throw new Error(`Invalid OIDC Discovery response from ${tenantDomain}: missing required fields`);
  }

  return {
    issuer: config.issuer,
    authorization_endpoint: config.authorization_endpoint,
    token_endpoint: config.token_endpoint,
    router_url: config.router_url,
  };
}
