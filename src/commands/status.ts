import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { Credentials } from '../credentials';

export interface StatusDeps {
  credentials?: Credentials;
  homedir?: () => string;
  readFileSync?: (path: string, encoding: BufferEncoding) => string;
  existsSync?: (path: string) => boolean;
  now?: () => Date;
}

export async function statusCommand(deps?: StatusDeps): Promise<void> {
  const credentials = deps?.credentials ?? new Credentials();
  const homedir = deps?.homedir ?? (() => os.homedir());
  const readFileSyncFn = deps?.readFileSync ?? ((p: string, e: BufferEncoding) => fs.readFileSync(p, e));
  const existsSyncFn = deps?.existsSync ?? fs.existsSync;
  const now = deps?.now ?? (() => new Date());

  // Step 1: Read credentials
  const creds = credentials.read();
  if (!creds) {
    process.stderr.write('Not logged in.\n');
    return;
  }

  const currentTime = now().getTime();

  // Step 2: Display status
  process.stderr.write(`Tenant: ${creds.tenant_domain} (${creds.tenant_name})\n`);
  process.stderr.write(`User: ${creds.email}\n`);

  // Access Token status
  const accessExpiresAt = new Date(creds.access_token_expires_at).getTime();
  if (currentTime > accessExpiresAt) {
    process.stderr.write('Access Token: Expired\n');
  } else {
    const remaining = accessExpiresAt - currentTime;
    process.stderr.write(`Access Token: Valid (${formatRemaining(remaining)} remaining)\n`);
  }

  // Refresh Token status
  const refreshExpiresAt = new Date(creds.refresh_token_expires_at).getTime();
  if (currentTime > refreshExpiresAt) {
    process.stderr.write('Refresh Token: Expired\n');
    process.stderr.write('\n⚠ Refresh token has expired. Run `monocle login --tenant <domain>` to re-authenticate.\n');
  } else {
    const remaining = refreshExpiresAt - currentTime;
    process.stderr.write(`Refresh Token: Valid (${formatRemaining(remaining)} remaining)\n`);
  }

  // Claude Code configuration
  const settingsPath = path.join(homedir(), '.claude', 'settings.json');
  let claudeConfigured = false;
  try {
    if (existsSyncFn(settingsPath)) {
      const content = readFileSyncFn(settingsPath, 'utf-8');
      const settings = JSON.parse(content);
      claudeConfigured = settings.apiKeyHelper === 'monocle token';
    }
  } catch {
    // ignore
  }

  process.stderr.write(`Claude Code: ${claudeConfigured ? 'Configured' : 'Not configured'}\n`);
}

function formatRemaining(ms: number): string {
  const totalMinutes = Math.floor(ms / (1000 * 60));
  const days = Math.floor(totalMinutes / (60 * 24));
  const hours = Math.floor((totalMinutes % (60 * 24)) / 60);
  const minutes = totalMinutes % 60;

  if (days > 0) {
    return `${days}d ${hours}h`;
  }
  if (hours > 0) {
    return `${hours}h ${minutes}m`;
  }
  return `${minutes}m`;
}
