import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { c } from '../colors';

export interface UnsetDeps {
  homedir?: () => string;
  readFileSync?: (path: string, encoding: BufferEncoding) => string;
  writeFileSync?: (path: string, data: string) => void;
  existsSync?: (path: string) => boolean;
}

export async function unsetCommand(deps?: UnsetDeps): Promise<void> {
  const homedir = deps?.homedir ?? (() => os.homedir());
  const readFileSyncFn = deps?.readFileSync ?? ((p: string, e: BufferEncoding) => fs.readFileSync(p, e));
  const writeFileSyncFn = deps?.writeFileSync ?? ((p: string, d: string) => fs.writeFileSync(p, d));
  const existsSyncFn = deps?.existsSync ?? fs.existsSync;

  // Step 1: Read settings.json
  const settingsPath = path.join(homedir(), '.claude', 'settings.json');

  if (!existsSyncFn(settingsPath)) {
    process.stderr.write(`${c.green('✓')} Monocle configuration removed. ${c.dim('Claude Code will use Anthropic directly.')}\n`);
    return;
  }

  let settings: Record<string, any> = {};
  try {
    const content = readFileSyncFn(settingsPath, 'utf-8');
    settings = JSON.parse(content);
  } catch {
    process.stderr.write(`${c.green('✓')} Monocle configuration removed. ${c.dim('Claude Code will use Anthropic directly.')}\n`);
    return;
  }

  // Step 2: Remove apiKeyHelper
  delete settings.apiKeyHelper;

  // Step 2: Remove env.ANTHROPIC_BASE_URL
  if (settings.env) {
    delete settings.env.ANTHROPIC_BASE_URL;

    // Step 3: Remove env if empty
    if (Object.keys(settings.env).length === 0) {
      delete settings.env;
    }
  }

  // Step 4: Save (preserving other settings)
  writeFileSyncFn(settingsPath, JSON.stringify(settings, null, 2));

  // Step 5: Success message
  process.stderr.write(`${c.green('✓')} Monocle configuration removed. ${c.dim('Claude Code will use Anthropic directly.')}\n`);
}
