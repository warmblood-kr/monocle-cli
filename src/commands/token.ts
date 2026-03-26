import { Credentials } from '../credentials';
import { refreshAccessToken, RefreshDeps } from '../refresh';

export interface TokenDeps {
  credentials?: Credentials;
  refreshDeps?: RefreshDeps;
  now?: () => Date;
}

const EXPIRY_BUFFER_MS = 5 * 60 * 1000; // 5 minutes

export async function tokenCommand(deps?: TokenDeps): Promise<void> {
  const credentials = deps?.credentials ?? new Credentials();
  const now = deps?.now ?? (() => new Date());

  // Step 1: Read credentials
  const creds = credentials.read();
  if (!creds) {
    process.stderr.write('Not logged in. Run `monocle login --tenant <domain>` first.\n');
    process.exit(1);
  }

  // Step 2: Check expiration with 5-minute buffer
  const expiresAt = new Date(creds.access_token_expires_at).getTime();
  const currentTime = now().getTime();
  const isExpired = currentTime + EXPIRY_BUFFER_MS > expiresAt;

  if (isExpired) {
    // Step 3: Refresh token
    const refreshDepsOverride: RefreshDeps = {
      credentials,
      ...deps?.refreshDeps,
    };
    const result = await refreshAccessToken(creds, refreshDepsOverride);
    if (!result.success || !result.credentials) {
      process.stderr.write(`${result.error}\n`);
      process.exit(1);
    }

    // Step 4: Output new token to stdout (ONLY this, nothing else)
    process.stdout.write(result.credentials.access_token);
    return;
  }

  // Step 4: Output token to stdout (ONLY this, nothing else)
  process.stdout.write(creds.access_token);
}
