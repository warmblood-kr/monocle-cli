import { describe, it, expect, vi } from 'vitest';
import { unsetCommand } from '../commands/unset';

describe('unsetCommand', () => {
  it('removes only Monocle settings', async () => {
    let written = '';
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await unsetCommand({
      homedir: () => '/fakehome',
      existsSync: () => true,
      readFileSync: () => JSON.stringify({
        apiKeyHelper: 'monocle token',
        otherSetting: 'keep',
        env: { ANTHROPIC_BASE_URL: 'https://test.stark.com', OTHER_VAR: 'keep' },
      }),
      writeFileSync: (_p: string, data: string) => { written = data; },
    });

    const settings = JSON.parse(written);
    expect(settings.apiKeyHelper).toBeUndefined();
    expect(settings.otherSetting).toBe('keep');
    expect(settings.env.ANTHROPIC_BASE_URL).toBeUndefined();
    expect(settings.env.OTHER_VAR).toBe('keep');
    expect(stderrSpy).toHaveBeenCalledWith(expect.stringContaining('Monocle configuration removed'));
    stderrSpy.mockRestore();
  });

  it('removes empty env object', async () => {
    let written = '';
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await unsetCommand({
      homedir: () => '/fakehome',
      existsSync: () => true,
      readFileSync: () => JSON.stringify({
        apiKeyHelper: 'monocle token',
        env: { ANTHROPIC_BASE_URL: 'https://test.stark.com' },
      }),
      writeFileSync: (_p: string, data: string) => { written = data; },
    });

    const settings = JSON.parse(written);
    expect(settings.env).toBeUndefined();
    stderrSpy.mockRestore();
  });

  it('handles missing settings.json gracefully', async () => {
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await unsetCommand({
      homedir: () => '/fakehome',
      existsSync: () => false,
      readFileSync: () => { throw new Error('ENOENT'); },
      writeFileSync: () => {},
    });

    expect(stderrSpy).toHaveBeenCalledWith(expect.stringContaining('Monocle configuration removed'));
    stderrSpy.mockRestore();
  });

  it('handles corrupt settings.json gracefully', async () => {
    const stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);

    await unsetCommand({
      homedir: () => '/fakehome',
      existsSync: () => true,
      readFileSync: () => 'not json{{{',
      writeFileSync: () => {},
    });

    expect(stderrSpy).toHaveBeenCalledWith(expect.stringContaining('Monocle configuration removed'));
    stderrSpy.mockRestore();
  });
});
