#!/usr/bin/env node

import { Command } from 'commander';
import { loginCommand } from './commands/login';
import { tokenCommand } from './commands/token';
import { setupCommand } from './commands/setup';
import { unsetCommand } from './commands/unset';
import { statusCommand } from './commands/status';
import { claudeCommand } from './commands/claude';

const program = new Command();

program
  .name('monocle')
  .description('CLI authentication tool for Claude Code with Stark OIDC integration')
  .version('0.4.0');

program
  .command('login')
  .description('Authenticate with Stark OIDC provider')
  .option('--tenant <domain>', 'Stark tenant domain (e.g., example.monocle-ai.com)')
  .option('--env <environment>', 'Environment: prod, stg, local (default: prod)', 'prod')
  .action(async (options) => {
    try {
      await loginCommand({ tenantDomain: options.tenant, env: options.env });
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

program
  .command('token')
  .description('Output access token to stdout (for apiKeyHelper)')
  .action(async () => {
    try {
      await tokenCommand();
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

program
  .command('setup')
  .description('Configure Claude Code to use Monocle authentication')
  .action(async () => {
    try {
      await setupCommand();
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

program
  .command('unset')
  .description('Remove Monocle configuration from Claude Code')
  .action(async () => {
    try {
      await unsetCommand();
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

program
  .command('status')
  .description('Show authentication and configuration status')
  .action(async () => {
    try {
      await statusCommand();
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

program
  .command('claude')
  .description('Launch Claude Code with Monocle authentication (clears conflicting env vars)')
  .allowUnknownOption(true)
  .helpOption(false)
  .action(async (_options, cmd) => {
    try {
      await claudeCommand(cmd.args);
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

program.parse(process.argv);
