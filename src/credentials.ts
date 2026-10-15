import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { Profiles, profileCredentialsPath } from './profiles';

export interface CredentialsData {
  tenant_domain: string;
  tenant_name: string;
  email: string;
  access_token: string;
  refresh_token: string;
  id_token: string;
  access_token_expires_at: string; // ISO 8601
  refresh_token_expires_at: string; // ISO 8601
  router_url?: string;
}

export interface CredentialsDeps {
  homedir?: () => string;
  profile?: string;
  env?: Record<string, string | undefined>;
  readFileSync?: (path: string, encoding: BufferEncoding) => string;
  writeFileSync?: (path: string, data: string, options?: fs.WriteFileOptions) => void;
  mkdirSync?: (path: string, options?: { recursive?: boolean; mode?: number }) => string | undefined;
  chmodSync?: (path: string, mode: fs.Mode) => void;
  unlinkSync?: (path: string) => void;
  existsSync?: (path: string) => boolean;
  statSync?: (path: string) => fs.Stats;
}

const defaultDeps: Required<CredentialsDeps> = {
  homedir: () => os.homedir(),
  profile: undefined as any,
  env: process.env,
  readFileSync: fs.readFileSync as any,
  writeFileSync: fs.writeFileSync as any,
  mkdirSync: fs.mkdirSync as any,
  chmodSync: fs.chmodSync,
  unlinkSync: fs.unlinkSync,
  existsSync: fs.existsSync,
  statSync: fs.statSync,
};

export class Credentials {
  private deps: Required<CredentialsDeps>;
  private profile: string;

  constructor(deps?: CredentialsDeps) {
    this.deps = { ...defaultDeps, ...deps } as Required<CredentialsDeps>;
    this.profile = new Profiles({ homedir: this.deps.homedir, env: this.deps.env }).resolve(deps?.profile);
  }

  getCredentialsPath(): string {
    return profileCredentialsPath(this.profile, this.deps.homedir());
  }

  getCredentialsDir(): string {
    return path.dirname(this.getCredentialsPath());
  }

  getProfileName(): string {
    return this.profile;
  }

  read(): CredentialsData | null {
    const filePath = this.getCredentialsPath();
    try {
      if (!this.deps.existsSync(filePath)) {
        return null;
      }
      const content = this.deps.readFileSync(filePath, 'utf-8');
      return JSON.parse(content) as CredentialsData;
    } catch (err: any) {
      process.stderr.write(`Warning: Failed to read credentials: ${err.message}\n`);
      return null;
    }
  }

  write(data: CredentialsData): void {
    const dir = this.getCredentialsDir();
    const filePath = this.getCredentialsPath();

    this.deps.mkdirSync(dir, { recursive: true, mode: 0o700 });
    this.deps.chmodSync(dir, 0o700);
    this.deps.writeFileSync(filePath, JSON.stringify(data, null, 2), { mode: 0o600 });
    this.deps.chmodSync(filePath, 0o600);
  }

  delete(): void {
    const filePath = this.getCredentialsPath();
    try {
      if (this.deps.existsSync(filePath)) {
        this.deps.unlinkSync(filePath);
      }
    } catch {
      // ignore
    }
  }

  getFileMode(): number | null {
    const filePath = this.getCredentialsPath();
    try {
      const stats = this.deps.statSync(filePath);
      return stats.mode & 0o777;
    } catch {
      return null;
    }
  }
}
