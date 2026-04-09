import { Credentials } from '../credentials';
import { refreshAccessToken, RefreshDeps } from '../refresh';

export interface ModelListDeps {
  credentials?: Credentials;
  refreshDeps?: RefreshDeps;
  fetch?: typeof globalThis.fetch;
  now?: () => Date;
}

interface ModelInfo {
  id: string;
  name?: string;
  owned_by?: string;
  context_window?: number;
}

const EXPIRY_BUFFER_MS = 5 * 60 * 1000;

export async function modelListCommand(deps?: ModelListDeps): Promise<void> {
  const credentials = deps?.credentials ?? new Credentials();
  const fetchFn = deps?.fetch ?? globalThis.fetch;
  const now = deps?.now ?? (() => new Date());

  const creds = credentials.read();
  if (!creds) {
    process.stderr.write('Not logged in. Run `monocle login --tenant <domain>` first.\n');
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
      process.stderr.write(`Token refresh failed: ${result.error}\n`);
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

  const response = await fetchFn(`${routerUrl}/v1/models`, {
    headers: {
      Authorization: `Bearer ${activeCreds.access_token}`,
    },
  });

  if (!response.ok) {
    const body = await response.text();
    throw new Error(`API error ${response.status}: ${body}`);
  }

  const data = (await response.json()) as { data: ModelInfo[] };
  const models = data.data ?? [];

  if (models.length === 0) {
    process.stderr.write('No models available.\n');
    return;
  }

  // Table output
  const idWidth = Math.max(10, ...models.map((m) => m.id.length));
  const nameWidth = Math.max(6, ...models.map((m) => (m.name ?? '').length));

  process.stdout.write(
    `${'MODEL ID'.padEnd(idWidth)}  ${'NAME'.padEnd(nameWidth)}  ${'OWNER'.padEnd(10)}  CONTEXT\n`,
  );
  process.stdout.write(`${'─'.repeat(idWidth)}  ${'─'.repeat(nameWidth)}  ${'─'.repeat(10)}  ${'─'.repeat(9)}\n`);

  for (const model of models) {
    const ctx = model.context_window ? `${(model.context_window / 1000).toFixed(0)}k` : '-';
    process.stdout.write(
      `${model.id.padEnd(idWidth)}  ${(model.name ?? '').padEnd(nameWidth)}  ${(model.owned_by ?? '').padEnd(10)}  ${ctx}\n`,
    );
  }

  process.stderr.write(`\n${models.length} model(s) available.\n`);
}
