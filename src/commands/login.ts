import * as http from 'http';
import * as url from 'url';
import { Credentials, CredentialsData } from '../credentials';
import { generateCodeVerifier, generateCodeChallenge, generateState, discoverOIDC, resolveStarkDomain, OIDCDeps } from '../oidc';
import { decodeIdTokenPayload } from '../refresh';

const CLIENT_ID = 'monocle-cli';
const SCOPES = 'openid profile email';
const REFRESH_TOKEN_TTL_DAYS = 30;

export interface LoginOptions {
  tenantDomain: string;
}

export interface LoginDeps {
  credentials?: Credentials;
  fetch?: (url: string, init?: any) => Promise<{ ok: boolean; status: number; json: () => Promise<any> }>;
  openBrowser?: (url: string) => Promise<void>;
  createServer?: (handler: (req: http.IncomingMessage, res: http.ServerResponse) => void) => {
    listen: (port: number, hostname: string, cb?: () => void) => void;
    close: (cb?: () => void) => void;
    address: () => { port: number } | null;
  };
}

const SUCCESS_HTML = `<!DOCTYPE html>
<html><head><title>Monocle - Authentication Successful</title>
<style>body{font-family:system-ui;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#f8f9fa}
.card{text-align:center;padding:2rem;border-radius:8px;background:white;box-shadow:0 2px 8px rgba(0,0,0,0.1)}
h1{color:#22c55e;margin-bottom:0.5rem}p{color:#6b7280}</style></head>
<body><div class="card"><h1>Authentication Successful</h1><p>You can close this window and return to the terminal.</p></div></body></html>`;

const ERROR_HTML = (msg: string) => `<!DOCTYPE html>
<html><head><title>Monocle - Authentication Failed</title>
<style>body{font-family:system-ui;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#f8f9fa}
.card{text-align:center;padding:2rem;border-radius:8px;background:white;box-shadow:0 2px 8px rgba(0,0,0,0.1)}
h1{color:#ef4444;margin-bottom:0.5rem}p{color:#6b7280}</style></head>
<body><div class="card"><h1>✗ Authentication Failed</h1><p>${msg}</p></div></body></html>`;

export async function loginCommand(options: LoginOptions, deps?: LoginDeps): Promise<void> {
  const credentials = deps?.credentials ?? new Credentials();
  const fetchFn = deps?.fetch ?? globalThis.fetch;
  const openBrowser = deps?.openBrowser ?? defaultOpenBrowser;
  const createServerFn = deps?.createServer ?? ((handler: any) => http.createServer(handler) as any);

  // Step 1: OIDC Discovery
  const starkDomain = resolveStarkDomain(options.tenantDomain);
  process.stderr.write(`Discovering OIDC configuration for ${starkDomain}...\n`);
  const oidc = await discoverOIDC(starkDomain, { fetch: fetchFn } as OIDCDeps);

  // Step 2: Generate PKCE + state
  const codeVerifier = generateCodeVerifier();
  const codeChallenge = generateCodeChallenge(codeVerifier);
  const state = generateState();

  // Step 3: Start local HTTP server on random port
  return new Promise<void>((resolve, reject) => {
    const server = createServerFn((req: http.IncomingMessage, res: http.ServerResponse) => {
      const parsed = url.parse(req.url ?? '', true);

      if (parsed.pathname !== '/oauth/oidc/callback') {
        res.writeHead(404);
        res.end('Not found');
        return;
      }

      const query = parsed.query;

      // Check for error
      if (query.error) {
        res.writeHead(200, { 'Content-Type': 'text/html' });
        res.end(ERROR_HTML(String(query.error_description || query.error)));
        server.close();
        reject(new Error(`Authorization failed: ${query.error_description || query.error}`));
        return;
      }

      // Validate state
      if (query.state !== state) {
        res.writeHead(200, { 'Content-Type': 'text/html' });
        res.end(ERROR_HTML('State mismatch - possible CSRF attack'));
        server.close();
        reject(new Error('State mismatch - possible CSRF attack'));
        return;
      }

      const code = query.code as string;
      if (!code) {
        res.writeHead(200, { 'Content-Type': 'text/html' });
        res.end(ERROR_HTML('No authorization code received'));
        server.close();
        reject(new Error('No authorization code received'));
        return;
      }

      // Step 6: Exchange code for tokens
      const addr = server.address();
      const port = addr?.port ?? 0;
      const redirectUri = `http://127.0.0.1:${port}/oauth/oidc/callback`;

      const tokenBody = new URLSearchParams({
        grant_type: 'authorization_code',
        code: code,
        redirect_uri: redirectUri,
        client_id: CLIENT_ID,
        code_verifier: codeVerifier,
      });

      fetchFn(oidc.token_endpoint, {
        method: 'POST',
        headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
        body: tokenBody.toString(),
      })
        .then(async (tokenResponse: any) => {
          if (!tokenResponse.ok) {
            const errText = await tokenResponse.json().catch(() => ({}));
            throw new Error(`Token exchange failed (HTTP ${tokenResponse.status}): ${JSON.stringify(errText)}`);
          }
          return tokenResponse.json();
        })
        .then((tokenData: any) => {
          // Step 7: Parse tokens
          const { access_token, refresh_token, id_token, expires_in } = tokenData;
          if (!access_token) throw new Error('No access_token in token response');

          // Step 8: Decode ID token
          let email = 'unknown';
          let tenantName = options.tenantDomain;
          if (id_token) {
            try {
              const payload = decodeIdTokenPayload(id_token);
              email = payload.email ?? 'unknown';
              tenantName = payload.tenant_name ?? options.tenantDomain;
            } catch {
              // Use defaults
            }
          }

          // Step 9: Save credentials
          const now = new Date();
          const accessTokenExpiresAt = new Date(now.getTime() + (expires_in ?? 3600) * 1000);
          const refreshTokenExpiresAt = new Date(now.getTime() + REFRESH_TOKEN_TTL_DAYS * 24 * 60 * 60 * 1000);

          const creds: CredentialsData = {
            tenant_domain: options.tenantDomain,
            tenant_name: tenantName,
            email,
            access_token,
            refresh_token: refresh_token ?? '',
            id_token: id_token ?? '',
            access_token_expires_at: accessTokenExpiresAt.toISOString(),
            refresh_token_expires_at: refreshTokenExpiresAt.toISOString(),
            router_url: oidc.router_url,
          };

          credentials.write(creds);

          // Step 10: Send success response and close server
          res.writeHead(200, { 'Content-Type': 'text/html' });
          res.end(SUCCESS_HTML);
          server.close();

          // Step 11: Terminal output
          process.stderr.write(`Logged in as ${email} (${tenantName})\n`);
          resolve();
        })
        .catch((err: Error) => {
          res.writeHead(200, { 'Content-Type': 'text/html' });
          res.end(ERROR_HTML(err.message));
          server.close();
          reject(err);
        });
    });

    server.listen(0, '127.0.0.1', () => {
      const addr = server.address();
      const port = addr?.port ?? 0;
      const redirectUri = `http://127.0.0.1:${port}/oauth/oidc/callback`;

      // Step 4: Build authorization URL
      const authParams = new URLSearchParams({
        client_id: CLIENT_ID,
        response_type: 'code',
        scope: SCOPES,
        redirect_uri: redirectUri,
        code_challenge: codeChallenge,
        code_challenge_method: 'S256',
        state: state,
        tenant: options.tenantDomain,
      });

      const authUrl = `${oidc.authorization_endpoint}?${authParams.toString()}`;

      process.stderr.write(`Opening browser for authentication...\n`);
      process.stderr.write(`If the browser doesn't open, visit: ${authUrl}\n`);

      openBrowser(authUrl).catch(() => {
        // Browser open failed, URL already printed above
      });
    });
  });
}

async function defaultOpenBrowser(url: string): Promise<void> {
  const { exec } = await import('child_process');
  const platform = process.platform;
  let cmd: string;
  if (platform === 'darwin') {
    cmd = `open "${url}"`;
  } else if (platform === 'win32') {
    cmd = `start "" "${url}"`;
  } else {
    cmd = `xdg-open "${url}"`;
  }
  return new Promise((resolve, reject) => {
    exec(cmd, (err) => {
      if (err) reject(err);
      else resolve();
    });
  });
}
