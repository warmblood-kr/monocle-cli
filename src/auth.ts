import { Credentials } from './credentials';
import { refreshAccessToken, RefreshDeps } from './refresh';

export interface AuthDeps {
  credentials?: Credentials;
  refreshDeps?: RefreshDeps;
  now?: () => Date;
  stderr?: NodeJS.WritableStream;
}

export interface AuthSession {
  token: string;
  routerUrl: string;
}

const EXPIRY_BUFFER_MS = 5 * 60 * 1000;

/**
 * Resolve a usable access token and router URL, refreshing if needed.
 * Exits the process with a friendly message when no credentials exist
 * or when refresh fails — callers don't need to handle that case.
 */
export async function getAccessToken(deps?: AuthDeps): Promise<AuthSession> {
  const credentials = deps?.credentials ?? new Credentials();
  const now = deps?.now ?? (() => new Date());
  const stderr = deps?.stderr ?? process.stderr;

  const creds = credentials.read();
  if (!creds) {
    stderr.write('Not logged in. Run `monocle login --tenant <domain>` first.\n');
    process.exit(1);
  }

  let activeCreds = creds;
  const expiresAt = new Date(creds.access_token_expires_at).getTime();
  if (now().getTime() + EXPIRY_BUFFER_MS > expiresAt) {
    const result = await refreshAccessToken(creds, {
      credentials,
      ...deps?.refreshDeps,
    });
    if (!result.success || !result.credentials) {
      stderr.write(`Token refresh failed: ${result.error}\n`);
      process.exit(1);
    }
    activeCreds = result.credentials;
  }

  let routerUrl: string;
  if (activeCreds.router_url) {
    routerUrl = activeCreds.router_url;
  } else {
    const isLocal =
      activeCreds.tenant_domain.startsWith('localhost') ||
      activeCreds.tenant_domain.startsWith('127.0.0.1');
    routerUrl = `${isLocal ? 'http' : 'https'}://${activeCreds.tenant_domain}`;
  }

  return { token: activeCreds.access_token, routerUrl };
}
