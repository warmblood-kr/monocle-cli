/**
 * Tiny ANSI color helper. No dependencies.
 *
 * Honors NO_COLOR (https://no-color.org) and only colorizes when stderr is a TTY.
 * Tone choices follow brew / gh CLI: muted, no neon. Cyan for accents, green for
 * success, yellow for warnings, dim for hints.
 */

function colorEnabled(): boolean {
  if (process.env.NO_COLOR) return false;
  if (process.env.FORCE_COLOR) return true;
  return Boolean(process.stderr.isTTY);
}

function wrap(open: string, close: string): (s: string) => string {
  return (s: string) => (colorEnabled() ? `\x1b[${open}m${s}\x1b[${close}m` : s);
}

export const c = {
  bold: wrap('1', '22'),
  dim: wrap('2', '22'),
  green: wrap('32', '39'),
  yellow: wrap('33', '39'),
  cyan: wrap('36', '39'),
  red: wrap('31', '39'),
};
