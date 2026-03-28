import { Credentials, CredentialsData } from './credentials';
import { discoverOIDC, resolveStarkDomain, OIDCDeps } from './oidc';

export interface RefreshDeps {
  fetch?: (url: string, init?: any) => Promise<{ ok: boolean; status: number; json: () => Promise<any> }>;
  credentials?: Credentials;
}

export interface RefreshResult {
  success: boolean;
  credentials?: CredentialsData;
  error?: string;
}

const CLIENT_ID = 'monocle-cli';
const REFRESH_TOKEN_TTL_DAYS = 30;

/**
 * Refresh the access token using the refresh_token grant
 */
export async function refreshAccessToken(
  currentCredentials: CredentialsData,
  deps?: RefreshDeps
): Promise<RefreshResult> {
  const fetchFn = deps?.fetch ?? globalThis.fetch;
  const credentials = deps?.credentials ?? new Credentials();

  // Discover token endpoint and router URL
  let tokenEndpoint: string;
  let discoveredRouterUrl: string | undefined;
  try {
    const starkDomain = resolveStarkDomain(currentCredentials.tenant_domain);
    const oidc = await discoverOIDC(starkDomain, { fetch: deps?.fetch } as OIDCDeps);
    tokenEndpoint = oidc.token_endpoint;
    discoveredRouterUrl = oidc.router_url;
  } catch (err: any) {
    return { success: false, error: `OIDC Discovery failed: ${err.message}` };
  }

  // Request new tokens
  const body = new URLSearchParams({
    grant_type: 'refresh_token',
    refresh_token: currentCredentials.refresh_token,
    client_id: CLIENT_ID,
  });

  let response: any;
  try {
    response = await fetchFn(tokenEndpoint, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: body.toString(),
    });
  } catch (err: any) {
    return { success: false, error: `Token refresh request failed: ${err.message}` };
  }

  if (!response.ok) {
    // 400 or 401 means refresh token is invalid/expired
    if (response.status === 400 || response.status === 401) {
      credentials.delete();
      return {
        success: false,
        error: 'Refresh token is invalid or expired. Please run `monocle login --tenant <domain>` to re-authenticate.',
      };
    }
    return { success: false, error: `Token refresh failed (HTTP ${response.status})` };
  }

  const tokenData = await response.json();

  const now = new Date();
  const accessTokenExpiresAt = new Date(now.getTime() + (tokenData.expires_in ?? 3600) * 1000);
  const refreshTokenExpiresAt = new Date(now.getTime() + REFRESH_TOKEN_TTL_DAYS * 24 * 60 * 60 * 1000);

  // Decode ID token to extract email and tenant_name (if new id_token provided)
  let email = currentCredentials.email;
  let tenantName = currentCredentials.tenant_name;
  if (tokenData.id_token) {
    try {
      const payload = decodeIdTokenPayload(tokenData.id_token);
      if (payload.email) email = payload.email;
      if (payload.tenant_name) tenantName = payload.tenant_name;
    } catch {
      // Keep existing values
    }
  }

  const newCredentials: CredentialsData = {
    tenant_domain: currentCredentials.tenant_domain,
    tenant_name: tenantName,
    email: email,
    access_token: tokenData.access_token,
    refresh_token: tokenData.refresh_token ?? currentCredentials.refresh_token,
    id_token: tokenData.id_token ?? currentCredentials.id_token,
    access_token_expires_at: accessTokenExpiresAt.toISOString(),
    refresh_token_expires_at: refreshTokenExpiresAt.toISOString(),
    router_url: discoveredRouterUrl ?? currentCredentials.router_url,
  };

  credentials.write(newCredentials);

  return { success: true, credentials: newCredentials };
}

export function decodeIdTokenPayload(idToken: string): any {
  const parts = idToken.split('.');
  if (parts.length !== 3) {
    throw new Error('Invalid ID token format');
  }
  const payload = Buffer.from(parts[1], 'base64url').toString('utf-8');
  return JSON.parse(payload);
}
