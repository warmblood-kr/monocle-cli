import { describe, expect, it } from 'vitest';
import { Profiles, profileCredentialsPath, validateProfileName } from '../profiles';

describe('profile selection', () => {
  const homedir = () => '/profile-home';

  it('selects flag, environment, persisted default, then implicit default', () => {
    const files: Record<string, string> = {
      '/profile-home/.monocle/config.json': JSON.stringify({ version: 1, default_profile: 'persisted' }),
    };
    const profiles = new Profiles({
      homedir,
      env: { MONOCLE_PROFILE: 'environment' },
      existsSync: (p) => p in files,
      readFileSync: (p) => files[p],
    });
    expect(profiles.resolve('flag')).toBe('flag');
    expect(profiles.resolve()).toBe('environment');

    const withoutEnv = new Profiles({ homedir, env: {}, existsSync: (p) => p in files, readFileSync: (p) => files[p] });
    expect(withoutEnv.resolve()).toBe('persisted');
    expect(new Profiles({ homedir, env: {}, existsSync: () => false }).resolve()).toBe('default');
  });

  it('rejects path traversal and profile names containing shell metacharacters', () => {
    expect(() => validateProfileName('../other')).toThrow(/Invalid profile name/);
    expect(() => validateProfileName('team; cat token')).toThrow(/Invalid profile name/);
  });

  it('stores each profile environment while preserving the selected default', () => {
    let config: any = null;
    const files: Record<string, string> = {};
    const profiles = new Profiles({
      homedir,
      env: {},
      existsSync: (p) => p in files,
      readFileSync: (p) => files[p],
      writeFileSync: (p, contents) => { files[p] = contents; },
      renameSync: (from, to) => { files[to] = files[from]; delete files[from]; },
      mkdirSync: () => undefined,
      chmodSync: () => undefined,
    });
    profiles.setDefault('warmblood');
    profiles.setEnvironment('warmblood', 'prod');
    profiles.setEnvironment('warmblood-stg', 'stg');
    config = profiles.readConfig();

    expect(config.default_profile).toBe('warmblood');
    expect(profiles.getEnvironment('warmblood')).toBe('prod');
    expect(profiles.getEnvironment('warmblood-stg')).toBe('stg');
    expect(profiles.list()).toContain('warmblood-stg');
  });

  it('infers environment for legacy credentials without profile metadata', () => {
    const profiles = new Profiles({ homedir, env: {}, existsSync: () => false });
    expect(profiles.getEnvironment('default', 'stg-example.monocle-ai.com')).toBe('stg');
    expect(profiles.getEnvironment('default', 'localhost:8080')).toBe('local');
    expect(profiles.getEnvironment('default', 'example.monocle-ai.com')).toBe('prod');
  });

  it('keeps default credentials at the legacy path and isolates named profiles', () => {
    expect(profileCredentialsPath('default', '/home/user')).toBe('/home/user/.monocle/credentials.json');
    expect(profileCredentialsPath('work', '/home/user')).toBe('/home/user/.monocle/profiles/work/credentials.json');
    expect(() => profileCredentialsPath('a/b', '/home/user')).toThrow(/Invalid profile name/);
  });
});
