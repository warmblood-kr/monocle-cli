import * as child_process from 'child_process';
import { Credentials } from '../credentials';

export interface ClaudeDeps {
  credentials?: Credentials;
  env?: Record<string, string | undefined>;
  spawn?: typeof child_process.spawn;
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

  // Resolve router URL (base URL for the Chat Proxy)
  let routerUrl: string;
  if (creds.router_url) {
    routerUrl = creds.router_url;
  } else {
    const isLocal = creds.tenant_domain.startsWith('localhost') || creds.tenant_domain.startsWith('127.0.0.1');
    routerUrl = `${isLocal ? 'http' : 'https'}://${creds.tenant_domain}`;
  }

  // Inline settings scoped to this child only — avoids mutating ~/.claude/settings.json.
  // `apiKeyHelper` keeps tokens fresh across long sessions by re-invoking `monocle token`.
  // `model` defaults to the 1M-context Sonnet to avoid surprise Opus costs; a user `--model` flag
  // (forwarded after --settings) still wins, since the CLI flag outranks the settings field.
  const inlineSettings = JSON.stringify({
    apiKeyHelper: 'monocle token',
    model: 'sonnet[1m]',
  });

  // Build child env — strip conflicting vars, inject base URL.
  const childEnv: Record<string, string> = {};
  for (const [key, value] of Object.entries(parentEnv)) {
    if (value !== undefined && key !== 'ANTHROPIC_API_KEY' && key !== 'ANTHROPIC_AUTH_TOKEN') {
      childEnv[key] = value;
    }
  }
  childEnv.ANTHROPIC_BASE_URL = routerUrl;

  const child = spawnFn('claude', ['--settings', inlineSettings, ...args], {
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
