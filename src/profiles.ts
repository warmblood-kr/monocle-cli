import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

export interface ProfileConfig {
  version: 1;
  default_profile: string;
  profiles?: Record<string, { environment?: MonocleEnvironment }>;
}

export type MonocleEnvironment = 'prod' | 'stg' | 'local';

export function parseMonocleEnvironment(value: string): MonocleEnvironment {
  if (value === 'prod' || value === 'stg' || value === 'local') return value;
  throw new Error(`Invalid environment "${value}". Choose prod, stg, or local.`);
}

export interface ProfileDeps {
  homedir?: () => string;
  env?: Record<string, string | undefined>;
  readFileSync?: (path: string, encoding: BufferEncoding) => string;
  writeFileSync?: (path: string, data: string, options?: fs.WriteFileOptions) => void;
  mkdirSync?: (path: string, options?: { recursive?: boolean; mode?: number }) => string | undefined;
  chmodSync?: (path: string, mode: fs.Mode) => void;
  renameSync?: (oldPath: string, newPath: string) => void;
  readdirSync?: (path: string, options?: { withFileTypes?: boolean }) => any[];
  existsSync?: (path: string) => boolean;
}

const defaults: Required<ProfileDeps> = {
  homedir: () => os.homedir(),
  env: process.env,
  readFileSync: fs.readFileSync as any,
  writeFileSync: fs.writeFileSync as any,
  mkdirSync: fs.mkdirSync as any,
  chmodSync: fs.chmodSync,
  renameSync: fs.renameSync,
  readdirSync: fs.readdirSync as any,
  existsSync: fs.existsSync,
};

export function validateProfileName(name: string): string {
  if (!/^[A-Za-z0-9._-]+$/.test(name) || name === '.' || name === '..') {
    throw new Error(`Invalid profile name "${name}". Use only letters, numbers, dot, underscore, and hyphen.`);
  }
  return name;
}

export class Profiles {
  private deps: Required<ProfileDeps>;

  constructor(deps?: ProfileDeps) {
    this.deps = { ...defaults, ...deps };
  }

  getConfigPath(): string {
    return path.join(this.deps.homedir(), '.monocle', 'config.json');
  }

  getProfilesDir(): string {
    return path.join(this.deps.homedir(), '.monocle', 'profiles');
  }

  readConfig(): ProfileConfig | null {
    const file = this.getConfigPath();
    if (!this.deps.existsSync(file)) return null;
    let config: any;
    try {
      config = JSON.parse(this.deps.readFileSync(file, 'utf-8'));
    } catch {
      throw new Error(`Could not read profile configuration at ${file}. Fix or remove the file before continuing.`);
    }
    if (config?.version !== 1 || typeof config.default_profile !== 'string') {
      throw new Error(`Unsupported profile configuration at ${file}. Expected version 1 and a default_profile.`);
    }
    validateProfileName(config.default_profile);
    const profileEntries = config.profiles ?? {};
    if (!profileEntries || typeof profileEntries !== 'object' || Array.isArray(profileEntries)) {
      throw new Error(`Invalid profiles in ${file}.`);
    }
    for (const [name, value] of Object.entries(profileEntries) as Array<[string, any]>) {
      validateProfileName(name);
      if (value?.environment !== undefined && !['prod', 'stg', 'local'].includes(value.environment)) {
        throw new Error(`Invalid environment for profile "${name}" in ${file}.`);
      }
    }
    return { version: 1, default_profile: config.default_profile, profiles: profileEntries };
  }

  resolve(flag?: string): string {
    if (flag !== undefined) return validateProfileName(flag);
    const fromEnv = this.deps.env.MONOCLE_PROFILE;
    if (fromEnv) return validateProfileName(fromEnv);
    return this.readConfig()?.default_profile ?? 'default';
  }

  setDefault(name: string): void {
    const profile = validateProfileName(name);
    const config = this.readConfig() ?? { version: 1 as const, default_profile: 'default', profiles: {} };
    this.writeConfig({ ...config, default_profile: profile });
  }

  setEnvironment(name: string, environment: MonocleEnvironment): void {
    const profile = validateProfileName(name);
    if (!['prod', 'stg', 'local'].includes(environment)) throw new Error(`Invalid environment "${environment}".`);
    const config = this.readConfig() ?? { version: 1 as const, default_profile: 'default', profiles: {} };
    this.writeConfig({ ...config, profiles: { ...config.profiles, [profile]: { environment } } });
  }

  getEnvironment(name: string, tenantDomain?: string): MonocleEnvironment {
    const profile = validateProfileName(name);
    const configured = this.readConfig()?.profiles?.[profile]?.environment;
    if (configured) return configured;
    if (tenantDomain?.startsWith('localhost') || tenantDomain?.startsWith('127.0.0.1')) return 'local';
    if (tenantDomain?.split('.')[0].startsWith('stg-')) return 'stg';
    return 'prod';
  }

  private writeConfig(config: ProfileConfig): void {
    const dir = path.dirname(this.getConfigPath());
    this.deps.mkdirSync(dir, { recursive: true, mode: 0o700 });
    const temp = `${this.getConfigPath()}.${process.pid}.tmp`;
    this.deps.writeFileSync(temp, JSON.stringify(config, null, 2), { mode: 0o600 });
    this.deps.chmodSync(dir, 0o700);
    this.deps.chmodSync(temp, 0o600);
    this.deps.renameSync(temp, this.getConfigPath());
  }

  list(): string[] {
    const names = new Set<string>(['default']);
    const configured = this.readConfig()?.default_profile;
    if (configured) names.add(configured);
    for (const name of Object.keys(this.readConfig()?.profiles ?? {})) names.add(name);
    if (this.deps.env.MONOCLE_PROFILE) names.add(validateProfileName(this.deps.env.MONOCLE_PROFILE));
    const dir = this.getProfilesDir();
    if (this.deps.existsSync(dir)) {
      for (const entry of this.deps.readdirSync(dir, { withFileTypes: true })) {
        const name = typeof entry === 'string' ? entry : entry.name;
        try {
          validateProfileName(name);
          names.add(name);
        } catch {
          // Ignore unexpected entries; names are user-facing selectors and path components.
        }
      }
    }
    return [...names].sort();
  }
}

export function profileCredentialsPath(profile: string, homedir = os.homedir()): string {
  const name = validateProfileName(profile);
  return name === 'default'
    ? path.join(homedir, '.monocle', 'credentials.json')
    : path.join(homedir, '.monocle', 'profiles', name, 'credentials.json');
}
