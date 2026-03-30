import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { Credentials } from '../credentials';

export interface SetupDeps {
  credentials?: Credentials;
  homedir?: () => string;
  readFileSync?: (path: string, encoding: BufferEncoding) => string;
  writeFileSync?: (path: string, data: string) => void;
  mkdirSync?: (path: string, options?: { recursive?: boolean }) => string | undefined;
  existsSync?: (path: string) => boolean;
  env?: Record<string, string | undefined>;
}

export async function setupCommand(deps?: SetupDeps): Promise<void> {
  const credentials = deps?.credentials ?? new Credentials();
  const homedir = deps?.homedir ?? (() => os.homedir());
  const readFileSyncFn = deps?.readFileSync ?? ((p: string, e: BufferEncoding) => fs.readFileSync(p, e));
  const writeFileSyncFn = deps?.writeFileSync ?? ((p: string, d: string) => fs.writeFileSync(p, d));
  const mkdirSyncFn = deps?.mkdirSync ?? ((p: string, o?: { recursive?: boolean }) => fs.mkdirSync(p, o));
  const existsSyncFn = deps?.existsSync ?? fs.existsSync;
  const env = deps?.env ?? process.env;

  // Step 1: Check credentials exist
  const creds = credentials.read();
  if (!creds) {
    process.stderr.write('Not logged in. Run `monocle login --tenant <domain>` first.\n');
    process.exit(1);
  }

  // Step 2: Read or create settings.json
  const claudeDir = path.join(homedir(), '.claude');
  const settingsPath = path.join(claudeDir, 'settings.json');

  let settings: Record<string, any> = {};
  try {
    if (existsSyncFn(settingsPath)) {
      const content = readFileSyncFn(settingsPath, 'utf-8');
      settings = JSON.parse(content);
    }
  } catch {
    settings = {};
  }

  // Step 3: Set apiKeyHelper
  settings.apiKeyHelper = 'monocle token';

  // Step 4: Set ANTHROPIC_BASE_URL in env
  if (!settings.env) {
    settings.env = {};
  }
  let routerUrl: string;
  if (creds.router_url) {
    routerUrl = creds.router_url;
  } else {
    const isLocal = creds.tenant_domain.startsWith('localhost') || creds.tenant_domain.startsWith('127.0.0.1');
    const protocol = isLocal ? 'http' : 'https';
    routerUrl = `${protocol}://${creds.tenant_domain}`;
    process.stderr.write('Warning: router_url not found. Using tenant domain as fallback.\n');
    process.stderr.write('Run `monocle login --tenant <domain>` to update credentials.\n');
  }
  settings.env.ANTHROPIC_BASE_URL = routerUrl;

  // Step 5: Write settings.json
  mkdirSyncFn(claudeDir, { recursive: true });
  writeFileSyncFn(settingsPath, JSON.stringify(settings, null, 2));

  // Step 6: Success message
  process.stderr.write('Claude Code configured to use Monocle authentication.\n');
  process.stderr.write(`  apiKeyHelper: monocle token\n`);
  process.stderr.write(`  ANTHROPIC_BASE_URL: ${routerUrl}\n`);

  // Step 7: Warn about conflicting env vars
  const conflicting = ['ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN'].filter(k => env[k]);
  if (conflicting.length > 0) {
    process.stderr.write(`\n⚠ Warning: ${conflicting.join(', ')} environment variable is set.\n`);
    process.stderr.write('  Use `monocle claude` to launch Claude Code — it clears conflicting env vars automatically.\n');
  }
}
