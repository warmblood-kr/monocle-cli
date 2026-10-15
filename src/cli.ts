#!/usr/bin/env node

import { Command } from 'commander';
import { loginCommand } from './commands/login';
import { tokenCommand } from './commands/token';
import { setupCommand } from './commands/setup';
import { unsetCommand } from './commands/unset';
import { statusCommand } from './commands/status';
import { claudeCommand } from './commands/claude';
import { chatCommand } from './commands/chat';
import { modelListCommand } from './commands/model-list';
import { profileListCommand, profileShowCommand, profileUseCommand } from './commands/profile';
import { handoffCommand } from './commands/handoff';
import { Credentials } from './credentials';

const { version } = require('../package.json') as { version: string };

const program = new Command();

program
  .name('monocle')
  .description('CLI authentication tool for Claude Code with Stark OIDC integration')
  .option('--profile <name>', 'Credential profile (or set MONOCLE_PROFILE)')
  .version(version);

const selectedCredentials = (options: { profile?: string }) =>
  new Credentials({ profile: options.profile ?? program.opts().profile });

program
  .command('login')
  .description('Authenticate with Stark OIDC provider')
  .option('--tenant <domain>', 'Stark tenant domain (e.g., example.monocle-ai.com)')
  .option('--env <environment>', 'Environment: prod, stg, local (defaults to this profile\'s environment)')
  .option('--profile <name>', 'Credential profile (or set MONOCLE_PROFILE)')
  .option('--force', 'Replace credentials when changing an existing profile environment')
  .option('--device-code', 'Use Device Authorization Grant (for headless/SSH environments)')
  .action(async (options) => {
    try {
      await loginCommand({ tenantDomain: options.tenant, env: options.env, deviceCode: options.deviceCode, force: options.force, profile: options.profile ?? program.opts().profile });
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

program
  .command('token')
  .description('Output access token to stdout (for apiKeyHelper)')
  .option('--profile <name>', 'Credential profile (or set MONOCLE_PROFILE)')
  .action(async (options) => {
    try {
      await tokenCommand({ credentials: selectedCredentials(options) });
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

program
  .command('setup')
  .description('Configure Claude Code to use Monocle authentication')
  .option('--profile <name>', 'Credential profile (or set MONOCLE_PROFILE)')
  .action(async (options) => {
    try {
      await setupCommand({ credentials: selectedCredentials(options) });
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
  .option('--profile <name>', 'Credential profile (or set MONOCLE_PROFILE)')
  .action(async (options) => {
    try {
      await statusCommand({ credentials: selectedCredentials(options) });
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
  .option('--profile <name>', 'Credential profile (or set MONOCLE_PROFILE)')
  .action(async (options, cmd) => {
    try {
      await claudeCommand(cmd.args, { credentials: selectedCredentials(options) });
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

const model = program
  .command('model')
  .description('Manage and interact with LLM models');

model
  .command('list')
  .description('List available models from the Monocle router')
  .option('--profile <name>', 'Credential profile (or set MONOCLE_PROFILE)')
  .action(async (options) => {
    try {
      await modelListCommand({ credentials: selectedCredentials(options) });
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

model
  .command('chat')
  .description('Chat with LLM via Monocle router (interactive REPL or pipe from stdin)')
  .option('--model <model>', 'Model ID to use', 'claude-sonnet-4-6')
  .option('--system-prompt <text>', 'System prompt text')
  .option('--system-prompt-file <path>', 'Load system prompt from file')
  .option('--max-tokens <n>', 'Maximum output tokens', '4096')
  .option('--profile <name>', 'Credential profile (or set MONOCLE_PROFILE)')
  .action(async (options) => {
    try {
      const { profile: _profile, ...chatOptions } = options;
      await chatCommand(chatOptions, { credentials: selectedCredentials(options) });
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

const profile = program.command('profile').description('List profiles, inspect a connection, or choose the default');
profile
  .command('show [name]')
  .description('Show the current or named profile and its tenant/environment')
  .action(async (name) => {
    try {
      await profileShowCommand(name ?? program.opts().profile);
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });
profile
  .command('list')
  .description('List known profiles and show the active default')
  .action(async () => {
    try {
      await profileListCommand(undefined, program.opts().profile);
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

program
  .command('handoff [chat-id]')
  .description('Select one hosted Monocle Chat conversation and hand it to Claude Code')
  .option('--profile <name>', 'Credential profile (or set MONOCLE_PROFILE)')
  .option('--yes', 'Confirm the selected conversation for non-interactive use')
  .action(async (chatId, options) => {
    try {
      await handoffCommand(chatId, { confirm: options.yes }, { credentials: selectedCredentials(options) });
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });
profile
  .command('use <name>')
  .description('Set the default credential profile')
  .action(async (name) => {
    try {
      await profileUseCommand(name);
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

program.parse(process.argv);
