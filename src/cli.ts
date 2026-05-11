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
import { audioTranscribeCommand } from './commands/audio-transcribe';

const { version } = require('../package.json') as { version: string };

const program = new Command();

program
  .name('monocle')
  .description('CLI authentication tool for Claude Code with Stark OIDC integration')
  .version(version);

program
  .command('login')
  .description('Authenticate with Stark OIDC provider')
  .option('--tenant <domain>', 'Stark tenant domain (e.g., example.monocle-ai.com)')
  .option('--env <environment>', 'Environment: prod, stg, local (default: prod)', 'prod')
  .option('--device-code', 'Use Device Authorization Grant (for headless/SSH environments)')
  .action(async (options) => {
    try {
      await loginCommand({ tenantDomain: options.tenant, env: options.env, deviceCode: options.deviceCode });
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

program
  .command('models')
  .description('List available models from the Monocle router')
  .action(async () => {
    try {
      await modelListCommand();
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

program
  .command('chat')
  .description('Chat with LLM via Monocle router (interactive REPL or pipe from stdin)')
  .option('--model <model>', 'Model ID to use', 'claude-sonnet-4-6')
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

const audio = program
  .command('audio')
  .description('Call audio (STT / TTS) endpoints directly for debugging');

audio
  .command('transcribe [file]')
  .description('Transcribe audio via /v1/audio/transcriptions (file path or stdin)')
  .option('--model <id>', 'Model ID (e.g., gpt-4o-mini-transcribe, whisper-1)')
  .option('--language <code>', 'ISO-639-1 language hint')
  .option('--prompt <text>', 'Optional prompt to guide the transcription')
  .option('--response-format <fmt>', 'json | text | srt | verbose_json | vtt')
  .option('--temperature <n>', 'Sampling temperature (0-1)')
  .option('--filename <name>', 'Filename to send (required when piping stdin without extension)')
  .option('--content-type <mime>', 'Override MIME type (e.g., audio/wav)')
  .option('--azure-fast', 'Use Azure Fast endpoint /v1/speechtotext/transcriptions:transcribe instead')
  .action(async (file: string | undefined, options) => {
    try {
      await audioTranscribeCommand(file, options);
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

// Deprecated aliases — kept for one release while users migrate.
const model = program
  .command('model', { hidden: true })
  .description('[Deprecated] Use `monocle chat` / `monocle models` instead');

model
  .command('list')
  .description('[Deprecated] Use `monocle models` instead')
  .action(async () => {
    try {
      process.stderr.write('Warning: `monocle model list` is deprecated. Use `monocle models` instead.\n');
      await modelListCommand();
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

model
  .command('chat')
  .description('[Deprecated] Use `monocle chat` instead')
  .option('--model <model>', 'Model ID to use', 'claude-sonnet-4-6')
  .option('--system-prompt <text>', 'System prompt text')
  .option('--system-prompt-file <path>', 'Load system prompt from file')
  .option('--max-tokens <n>', 'Maximum output tokens', '4096')
  .action(async (options) => {
    try {
      process.stderr.write('Warning: `monocle model chat` is deprecated. Use `monocle chat` instead.\n');
      await chatCommand(options);
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  });

program.parse(process.argv);
