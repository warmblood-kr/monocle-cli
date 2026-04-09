import * as fs from 'fs';
import * as readline from 'readline';
import { Credentials } from '../credentials';
import { refreshAccessToken, RefreshDeps } from '../refresh';

export interface ChatOptions {
  model?: string;
  systemPrompt?: string;
  systemPromptFile?: string;
  maxTokens?: string;
}

export interface ChatDeps {
  credentials?: Credentials;
  refreshDeps?: RefreshDeps;
  fetch?: typeof globalThis.fetch;
  now?: () => Date;
  stdin?: NodeJS.ReadableStream;
  stdout?: NodeJS.WritableStream;
  stderr?: NodeJS.WritableStream;
}

const EXPIRY_BUFFER_MS = 5 * 60 * 1000;
const DEFAULT_MODEL = 'claude-sonnet-4-5-20250514';
const DEFAULT_MAX_TOKENS = 4096;

async function getAccessToken(
  credentials: Credentials,
  deps?: ChatDeps,
): Promise<{ token: string; routerUrl: string }> {
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

async function callChat(
  routerUrl: string,
  token: string,
  model: string,
  systemPrompt: string | undefined,
  userMessage: string,
  maxTokens: number,
  deps?: ChatDeps,
): Promise<string> {
  const fetchFn = deps?.fetch ?? globalThis.fetch;

  const messages: Array<{ role: string; content: string }> = [];
  if (systemPrompt) {
    messages.push({ role: 'system', content: systemPrompt });
  }
  messages.push({ role: 'user', content: userMessage });

  const response = await fetchFn(`${routerUrl}/v1/chat/completions`, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      model,
      messages,
      max_tokens: maxTokens,
      stream: false,
    }),
  });

  if (!response.ok) {
    const body = await response.text();
    throw new Error(`API error ${response.status}: ${body}`);
  }

  const data = (await response.json()) as any;
  return data.choices?.[0]?.message?.content ?? '';
}

function readStdin(): Promise<string> {
  return new Promise((resolve, reject) => {
    let data = '';
    process.stdin.setEncoding('utf-8');
    process.stdin.on('data', (chunk: string) => {
      data += chunk;
    });
    process.stdin.on('end', () => resolve(data));
    process.stdin.on('error', reject);
  });
}

export async function chatCommand(
  options: ChatOptions,
  deps?: ChatDeps,
): Promise<void> {
  const credentials = deps?.credentials ?? new Credentials();
  const stdout = deps?.stdout ?? process.stdout;
  const stderr = deps?.stderr ?? process.stderr;

  const model = options.model ?? DEFAULT_MODEL;
  const maxTokens = parseInt(options.maxTokens ?? String(DEFAULT_MAX_TOKENS), 10);

  // Resolve system prompt
  let systemPrompt: string | undefined;
  if (options.systemPromptFile) {
    if (!fs.existsSync(options.systemPromptFile)) {
      stderr.write(`System prompt file not found: ${options.systemPromptFile}\n`);
      process.exit(1);
    }
    systemPrompt = fs.readFileSync(options.systemPromptFile, 'utf-8');
  } else if (options.systemPrompt) {
    systemPrompt = options.systemPrompt;
  }

  // Get auth
  const { token, routerUrl } = await getAccessToken(credentials, deps);

  // Check if stdin is piped (non-interactive)
  if (!process.stdin.isTTY) {
    const input = await readStdin();
    if (!input.trim()) {
      stderr.write('No input provided via stdin.\n');
      process.exit(1);
    }
    stderr.write(`Using model: ${model}\n`);
    stderr.write(`Router: ${routerUrl}\n`);
    const result = await callChat(routerUrl, token, model, systemPrompt, input.trim(), maxTokens, deps);
    stdout.write(result);
    stdout.write('\n');
    return;
  }

  // Interactive REPL mode
  stderr.write(`Monocle Chat (model: ${model})\n`);
  stderr.write(`Router: ${routerUrl}\n`);
  if (systemPrompt) {
    stderr.write(`System prompt loaded (${systemPrompt.length} chars)\n`);
  }
  stderr.write('Type your message. Press Ctrl+D to exit.\n');
  stderr.write('---\n');

  const rl = readline.createInterface({
    input: deps?.stdin ?? process.stdin,
    output: deps?.stderr ?? process.stderr,
    prompt: '> ',
  });

  rl.prompt();

  rl.on('line', async (line: string) => {
    const trimmed = line.trim();
    if (!trimmed) {
      rl.prompt();
      return;
    }

    if (trimmed === '/quit' || trimmed === '/exit') {
      rl.close();
      return;
    }

    try {
      stderr.write('\n');
      const result = await callChat(routerUrl, token, model, systemPrompt, trimmed, maxTokens, deps);
      stdout.write(result);
      stdout.write('\n\n');
    } catch (err: any) {
      stderr.write(`Error: ${err.message}\n`);
    }

    rl.prompt();
  });

  rl.on('close', () => {
    stderr.write('\nBye.\n');
  });
}
