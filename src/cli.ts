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
import { audioTranscribeAzureCommand } from './commands/audio-transcribe-azure';
import { audioSpeechCommand } from './commands/audio-speech';
import { audioSpeechAzureCommand } from './commands/audio-speech-azure';

const { version } = require('../package.json') as { version: string };

const program = new Command();

program
  .name('monocle')
  .description('CLI authentication tool for Claude Code with Stark OIDC integration')
  .version(version);

function runAction<A extends any[]>(
  fn: (...args: A) => Promise<unknown>,
): (...args: A) => Promise<void> {
  return async (...args: A) => {
    try {
      await fn(...args);
    } catch (err: any) {
      process.stderr.write(`Error: ${err.message}\n`);
      process.exit(1);
    }
  };
}

function addChatOptions(cmd: Command): Command {
  return cmd
    .option('--model <model>', 'Model ID to use', 'claude-sonnet-4-6')
    .option('--system-prompt <text>', 'System prompt text')
    .option('--system-prompt-file <path>', 'Load system prompt from file')
    .option('--max-tokens <n>', 'Maximum output tokens', '4096');
}

program
  .command('login')
  .description('Authenticate with Stark OIDC provider')
  .option('--tenant <domain>', 'Stark tenant domain (e.g., example.monocle-ai.com)')
  .option('--env <environment>', 'Environment: prod, stg, local (default: prod)', 'prod')
  .option('--device-code', 'Use Device Authorization Grant (for headless/SSH environments)')
  .action(
    runAction(async (options) => {
      await loginCommand({
        tenantDomain: options.tenant,
        env: options.env,
        deviceCode: options.deviceCode,
      });
    }),
  );

program
  .command('token')
  .description('Output access token to stdout (for apiKeyHelper)')
  .action(runAction(tokenCommand));

program
  .command('setup')
  .description('Configure Claude Code to use Monocle authentication')
  .action(runAction(setupCommand));

program
  .command('unset')
  .description('Remove Monocle configuration from Claude Code')
  .action(runAction(unsetCommand));

program
  .command('status')
  .description('Show authentication and configuration status')
  .action(runAction(statusCommand));

program
  .command('claude')
  .description('Launch Claude Code with Monocle authentication (clears conflicting env vars)')
  .allowUnknownOption(true)
  .helpOption(false)
  .action(
    runAction(async (_options, cmd) => {
      await claudeCommand(cmd.args);
    }),
  );

program
  .command('models')
  .description('List available models from the Monocle router')
  .action(runAction(modelListCommand));

addChatOptions(
  program
    .command('chat')
    .description('Chat with LLM via Monocle router (interactive REPL or pipe from stdin)'),
).action(runAction(chatCommand));

const audio = program
  .command('audio')
  .description('Call audio (STT / TTS) endpoints directly for debugging');

audio
  .command('transcribe [file]')
  .description('Transcribe audio via /v1/audio/transcriptions (OpenAI compatible)')
  .option('--model <id>', 'Model ID (e.g., gpt-4o-mini-transcribe, whisper-1)')
  .option('--language <code>', 'ISO-639-1 language hint')
  .option('--prompt <text>', 'Optional prompt to guide the transcription')
  .option('--response-format <fmt>', 'json | text | srt | verbose_json | vtt')
  .option('--temperature <n>', 'Sampling temperature (0-1)')
  .option('--filename <name>', 'Filename to send (required when piping stdin without extension)')
  .option('--content-type <mime>', 'Override MIME type (e.g., audio/wav)')
  .action(runAction(audioTranscribeCommand));

audio
  .command('transcribe-azure [file]')
  .description('Transcribe via Azure Fast endpoint /v1/speechtotext/transcriptions:transcribe')
  .option('--locale <code...>', 'Locale (e.g., en-US, ko-KR) — repeatable')
  .option('--diarization', 'Enable speaker diarization')
  .option('--profanity <mode>', 'None | Removed | Masked | Tags')
  .option('--channels <list>', 'Comma-separated channel indices (e.g., "0,1")')
  .option('--definition <json>', 'Raw definition JSON (escape hatch; overrides individual flags)')
  .option('--filename <name>', 'Filename to send (required when piping stdin without extension)')
  .option('--content-type <mime>', 'Override MIME type (e.g., audio/wav)')
  .action(
    runAction(async (file: string | undefined, options) => {
      await audioTranscribeAzureCommand(file, {
        locales: options.locale,
        diarization: options.diarization,
        profanity: options.profanity,
        channels: options.channels,
        definition: options.definition,
        filename: options.filename,
        contentType: options.contentType,
      });
    }),
  );

audio
  .command('speech [text]')
  .description('Synthesize speech via /v1/audio/speech (OpenAI compatible)')
  .option('--model <id>', 'Model ID (e.g., gpt-4o-mini-tts)')
  .option('--voice <name>', 'Voice ID (e.g., alloy, echo, fable, onyx, nova, shimmer)')
  .option('--format <fmt>', 'Output format (mp3 | opus | aac | flac | wav | pcm)')
  .option('--speed <n>', 'Speech speed (0.25-4.0)')
  .option('--instructions <text>', 'Style/delivery instructions (model-dependent)')
  .option('-o, --output <path>', 'Write audio to this path instead of stdout')
  .action(runAction(audioSpeechCommand));

audio
  .command('speech-azure [ssml]')
  .description('Synthesize speech via Azure /v1/azure/texttospeech/cognitiveservices/v1 (SSML body)')
  .option('--format <fmt>', 'X-Microsoft-OutputFormat (e.g., audio-24khz-48kbitrate-mono-mp3)')
  .option('-o, --output <path>', 'Write audio to this path instead of stdout')
  .action(runAction(audioSpeechAzureCommand));

// Deprecated aliases — kept for one release while users migrate.
const model = program
  .command('model', { hidden: true })
  .description('[Deprecated] Use `monocle chat` / `monocle models` instead');

model
  .command('list')
  .description('[Deprecated] Use `monocle models` instead')
  .action(
    runAction(async () => {
      process.stderr.write('Warning: `monocle model list` is deprecated. Use `monocle models` instead.\n');
      await modelListCommand();
    }),
  );

addChatOptions(
  model
    .command('chat')
    .description('[Deprecated] Use `monocle chat` instead'),
).action(
  runAction(async (options) => {
    process.stderr.write('Warning: `monocle model chat` is deprecated. Use `monocle chat` instead.\n');
    await chatCommand(options);
  }),
);

program.parse(process.argv);
