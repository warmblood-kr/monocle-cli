import * as child_process from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { Credentials } from '../credentials';
import { setupCommand } from './setup';

export interface ClaudeDeps {
  credentials?: Credentials;
  env?: Record<string, string | undefined>;
  spawn?: typeof child_process.spawn;
  skipSetup?: boolean;
}

export async function claudeCommand(args: string[], deps?: ClaudeDeps): Promise<void> {
  const credentials = deps?.credentials ?? new Credentials();
  const parentEnv = deps?.env ?? process.env;
  const spawnFn = deps?.spawn ?? child_process.spawn;

  const creds = credentials.read();
  if (!creds) {
    process.stderr.write('Not logged in. Run `monocle login --tenant <domain>` first.\n');
    process.exit(1);
    return;
  }

  // Ensure Claude Code is configured (auto-setup if needed)
  if (!deps?.skipSetup) {
    const settingsPath = path.join(os.homedir(), '.claude', 'settings.json');
    let needsSetup = true;
    try {
      const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf-8'));
      needsSetup = settings.apiKeyHelper !== 'monocle token';
    } catch {
      // File doesn't exist or invalid JSON
    }
    if (needsSetup) {
      process.stderr.write('Claude Code not configured for Monocle. Running setup...\n');
      await setupCommand();
    }
  }

  // Build clean env — remove vars that override apiKeyHelper
  const childEnv: Record<string, string> = {};
  for (const [key, value] of Object.entries(parentEnv)) {
    if (value !== undefined && key !== 'ANTHROPIC_API_KEY' && key !== 'ANTHROPIC_AUTH_TOKEN') {
      childEnv[key] = value;
    }
  }

  // Set base URL to Chat Proxy
  if (creds.router_url) {
    childEnv.ANTHROPIC_BASE_URL = creds.router_url;
  } else {
    const isLocal = creds.tenant_domain.startsWith('localhost') || creds.tenant_domain.startsWith('127.0.0.1');
    childEnv.ANTHROPIC_BASE_URL = `${isLocal ? 'http' : 'https'}://${creds.tenant_domain}`;
  }

  const child = spawnFn('claude', args, {
    env: childEnv,
    stdio: 'inherit',
  });

  let exited = false;

  child.on('close', (code) => {
    if (!exited) { exited = true; process.exit(code ?? 0); }
  });

  child.on('error', (err: NodeJS.ErrnoException) => {
    if (!exited) {
      exited = true;
      if (err.code === 'ENOENT') {
        process.stderr.write('Error: `claude` command not found. Is Claude Code installed?\n');
      } else {
        process.stderr.write(`Error: ${err.message}\n`);
      }
      process.exit(1);
    }
  });
}
