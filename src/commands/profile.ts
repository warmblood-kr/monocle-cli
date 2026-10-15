import { Profiles } from '../profiles';
import { Credentials } from '../credentials';

export async function profileListCommand(profiles = new Profiles(), selectedProfile?: string): Promise<void> {
  const current = profiles.resolve(selectedProfile);
  for (const name of profiles.list()) {
    const credentials = new Credentials({ profile: name });
    const environment = profiles.getEnvironment(name, credentials.read()?.tenant_domain);
    process.stdout.write(`${name === current ? '* ' : '  '}${name} (${environment})\n`);
  }
}

export async function profileShowCommand(name?: string, profiles = new Profiles()): Promise<void> {
  const selected = profiles.resolve(name);
  const credentials = new Credentials({ profile: selected });
  const data = credentials.read();
  process.stdout.write(`Profile: ${selected}\n`);
  process.stdout.write(`Environment: ${profiles.getEnvironment(selected, data?.tenant_domain)}\n`);
  if (data) {
    process.stdout.write(`Tenant: ${data.tenant_domain} (${data.tenant_name})\n`);
    process.stdout.write(`User: ${data.email}\n`);
  } else {
    process.stdout.write('Status: Not logged in\n');
  }
}

export async function profileUseCommand(name: string, profiles = new Profiles()): Promise<void> {
  profiles.setDefault(name);
  process.stderr.write(`Default Monocle profile set to "${name}".\n`);
}
