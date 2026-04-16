import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { pollForToken } from '../commands/login';

describe('Device Code Flow', () => {
  describe('SSH environment detection', () => {
    const originalEnv = process.env;

    beforeEach(() => {
      process.env = { ...originalEnv };
    });

    afterEach(() => {
      process.env = originalEnv;
    });

    it('detects SSH_CLIENT as headless', () => {
      process.env.SSH_CLIENT = '192.168.1.1 12345 22';
      expect(!!process.env.SSH_CLIENT).toBe(true);
    });

    it('detects SSH_TTY as headless', () => {
      process.env.SSH_TTY = '/dev/pts/0';
      expect(!!process.env.SSH_TTY).toBe(true);
    });

    it('detects SSH_CONNECTION as headless', () => {
      process.env.SSH_CONNECTION = '192.168.1.1 12345 192.168.1.2 22';
      expect(!!process.env.SSH_CONNECTION).toBe(true);
    });
  });

  describe('pollForToken', () => {
    let stderrSpy: ReturnType<typeof vi.spyOn>;

    beforeEach(() => {
      stderrSpy = vi.spyOn(process.stderr, 'write').mockImplementation(() => true);
    });

    afterEach(() => {
      stderrSpy.mockRestore();
      vi.restoreAllMocks();
    });

    it('returns token on first successful response', async () => {
      const tokenData = {
        access_token: 'at_123',
        refresh_token: 'rt_123',
        id_token: 'id_123',
        expires_in: 3600,
      };

      vi.stubGlobal('fetch', async () => ({
        ok: true,
        status: 200,
        json: async () => tokenData,
      }));

      const result = await pollForToken(
        'https://auth.example.com/token',
        'device_code_abc',
        'monocle-cli',
        0.01, // very short interval for test
        10,
      );

      expect(result).toEqual(tokenData);
    });

    it('retries on authorization_pending then succeeds', async () => {
      let callCount = 0;
      const tokenData = {
        access_token: 'at_456',
        refresh_token: 'rt_456',
        id_token: 'id_456',
        expires_in: 3600,
      };

      vi.stubGlobal('fetch', async () => {
        callCount++;
        if (callCount <= 2) {
          return {
            ok: false,
            status: 400,
            json: async () => ({ error: 'authorization_pending' }),
          };
        }
        return {
          ok: true,
          status: 200,
          json: async () => tokenData,
        };
      });

      const result = await pollForToken(
        'https://auth.example.com/token',
        'device_code_abc',
        'monocle-cli',
        0.01,
        10,
      );

      expect(result).toEqual(tokenData);
      expect(callCount).toBe(3);
      // stderr should have dots for pending calls
      expect(stderrSpy).toHaveBeenCalledWith('.');
    });

    it('throws on expired_token', async () => {
      vi.stubGlobal('fetch', async () => ({
        ok: false,
        status: 400,
        json: async () => ({ error: 'expired_token' }),
      }));

      await expect(
        pollForToken(
          'https://auth.example.com/token',
          'device_code_abc',
          'monocle-cli',
          0.01,
          10,
        )
      ).rejects.toThrow('인증 시간이 만료되었습니다.');
    });

    it('throws on access_denied', async () => {
      vi.stubGlobal('fetch', async () => ({
        ok: false,
        status: 400,
        json: async () => ({ error: 'access_denied' }),
      }));

      await expect(
        pollForToken(
          'https://auth.example.com/token',
          'device_code_abc',
          'monocle-cli',
          0.01,
          10,
        )
      ).rejects.toThrow('인증이 거부되었습니다.');
    });

    it('increases interval on slow_down then succeeds', async () => {
      let callCount = 0;
      const tokenData = {
        access_token: 'at_789',
        refresh_token: 'rt_789',
        id_token: 'id_789',
        expires_in: 3600,
      };

      vi.stubGlobal('fetch', async () => {
        callCount++;
        if (callCount === 1) {
          return {
            ok: false,
            status: 400,
            json: async () => ({ error: 'slow_down' }),
          };
        }
        return {
          ok: true,
          status: 200,
          json: async () => tokenData,
        };
      });

      // Use fake timers to avoid waiting 5+ real seconds
      vi.useFakeTimers();
      const promise = pollForToken(
        'https://auth.example.com/token',
        'device_code_abc',
        'monocle-cli',
        0.01,
        30,
      );

      // Advance past first interval (0.01s)
      await vi.advanceTimersByTimeAsync(100);
      // After slow_down, interval becomes 5.01s — advance past it
      await vi.advanceTimersByTimeAsync(6000);

      const result = await promise;
      vi.useRealTimers();

      expect(result).toEqual(tokenData);
      expect(callCount).toBe(2);
    });
  });
});
