import { describe, it, expect } from 'vitest';
import { ENTRYPOINT_HEADER, MONOCLE_ENTRYPOINT } from '../entrypoint';

describe('entrypoint header', () => {
  it('identifies this surface as "cli"', () => {
    expect(MONOCLE_ENTRYPOINT).toBe('cli');
  });

  it('exposes a header object usage events can be attributed by', () => {
    expect(ENTRYPOINT_HEADER).toEqual({ 'x-monocle-entrypoint': 'cli' });
  });
});
