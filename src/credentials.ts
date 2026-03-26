import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';

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
  readFileSync?: (path: string, encoding: BufferEncoding) => string;
  writeFileSync?: (path: string, data: string, options?: fs.WriteFileOptions) => void;
  mkdirSync?: (path: string, options?: { recursive?: boolean }) => string | undefined;
  chmodSync?: (path: string, mode: fs.Mode) => void;
  unlinkSync?: (path: string) => void;
  existsSync?: (path: string) => boolean;
  statSync?: (path: string) => fs.Stats;
}

const defaultDeps: Required<CredentialsDeps> = {
  homedir: () => os.homedir(),
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

  constructor(deps?: CredentialsDeps) {
    this.deps = { ...defaultDeps, ...deps };
  }

  getCredentialsPath(): string {
    return path.join(this.deps.homedir(), '.monocle', 'credentials.json');
  }

  getCredentialsDir(): string {
    return path.join(this.deps.homedir(), '.monocle');
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

    this.deps.mkdirSync(dir, { recursive: true });
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
