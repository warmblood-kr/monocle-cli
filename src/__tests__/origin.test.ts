import { describe, it, expect } from 'vitest';
import { ORIGIN_HEADER, MONOCLE_ORIGIN } from '../origin';

describe('origin header', () => {
  it('identifies this surface as "cli"', () => {
    expect(MONOCLE_ORIGIN).toBe('cli');
  });

  it('exposes a header object usage events can be attributed by', () => {
    expect(ORIGIN_HEADER).toEqual({ 'x-monocle-origin': 'cli' });
  });
});
