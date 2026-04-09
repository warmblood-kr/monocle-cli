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

const program = new Command();

program
  .name('monocle')
  .description('CLI authentication tool for Claude Code with Stark OIDC integration')
  .version('0.4.1');

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

const model = program
  .command('model')
  .description('Manage and interact with LLM models');

model
  .command('list')
  .description('List available models from the Monocle router')
  .action(async () => {
    try {
      await modelListCommand();
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

model
  .command('chat')
  .description('Chat with LLM via Monocle router (interactive REPL or pipe from stdin)')
  .option('--model <model>', 'Model ID to use', 'claude-sonnet-4-5-20250514')
  .option('--system-prompt <text>', 'System prompt text')
  .option('--system-prompt-file <path>', 'Load system prompt from file')
  .option('--max-tokens <n>', 'Maximum output tokens', '4096')
  .action(async (options) => {
    try {
      await chatCommand(options);
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

program.parse(process.argv);
