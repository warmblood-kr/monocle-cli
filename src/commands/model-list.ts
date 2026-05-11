import { Credentials } from '../credentials';
import { RefreshDeps } from '../refresh';
import { getAccessToken } from '../auth';

export interface ModelListDeps {
  credentials?: Credentials;
  refreshDeps?: RefreshDeps;
  fetch?: typeof globalThis.fetch;
  now?: () => Date;
  stdout?: NodeJS.WritableStream;
  stderr?: NodeJS.WritableStream;
}

interface ModelInfo {
  id: string;
  name?: string;
  owned_by?: string;
  context_window?: number;
  modality?: string;
}

export async function modelListCommand(deps?: ModelListDeps): Promise<void> {
  const fetchFn = deps?.fetch ?? globalThis.fetch;
  const stdout = deps?.stdout ?? process.stdout;
  const stderr = deps?.stderr ?? process.stderr;

  const { token, routerUrl } = await getAccessToken(deps);

  const response = await fetchFn(`${routerUrl}/v1/models`, {
    headers: { Authorization: `Bearer ${token}` },
  });

  if (!response.ok) {
    const body = await response.text();
    throw new Error(`API error ${response.status}: ${body}`);
  }

  const data = (await response.json()) as { data: ModelInfo[] };
  const models = data.data ?? [];

  if (models.length === 0) {
    stderr.write('No models available.\n');
    return;
  }

  const idWidth = Math.max(8, ...models.map((m) => m.id.length));
  const nameWidth = Math.max(4, ...models.map((m) => (m.name ?? '').length));
  const modalityWidth = Math.max(8, ...models.map((m) => (m.modality ?? '').length));

  stdout.write(
    `${'MODEL ID'.padEnd(idWidth)}  ${'NAME'.padEnd(nameWidth)}  ${'MODALITY'.padEnd(modalityWidth)}  ${'OWNER'.padEnd(10)}  CONTEXT\n`,
  );
  stdout.write(
    `${'─'.repeat(idWidth)}  ${'─'.repeat(nameWidth)}  ${'─'.repeat(modalityWidth)}  ${'─'.repeat(10)}  ${'─'.repeat(7)}\n`,
  );

  for (const model of models) {
    const ctx = model.context_window ? `${(model.context_window / 1000).toFixed(0)}k` : '-';
    stdout.write(
      `${model.id.padEnd(idWidth)}  ${(model.name ?? '').padEnd(nameWidth)}  ${(model.modality ?? '-').padEnd(modalityWidth)}  ${(model.owned_by ?? '').padEnd(10)}  ${ctx}\n`,
    );
  }

  stderr.write(`\n${models.length} model(s) available.\n`);
}
