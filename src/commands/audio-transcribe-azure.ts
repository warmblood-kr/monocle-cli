import { Credentials } from '../credentials';
import { RefreshDeps } from '../refresh';
import { getAccessToken } from '../auth';
import {
  AudioInputDeps,
  resolveAudioInput,
  writeApiErrorAndExit,
} from '../audio-io';
import { ENDPOINTS } from '../endpoints';
import { ENTRYPOINT_HEADER } from '../entrypoint';

export interface AudioTranscribeAzureOptions {
  locales?: string[];
  diarization?: boolean;
  profanity?: string;
  channels?: string;
  definition?: string;
  filename?: string;
  contentType?: string;
}

export interface AudioTranscribeAzureDeps extends AudioInputDeps {
  credentials?: Credentials;
  refreshDeps?: RefreshDeps;
  fetch?: typeof globalThis.fetch;
  now?: () => Date;
  stdout?: NodeJS.WritableStream;
  stderr?: NodeJS.WritableStream;
}

function buildDefinition(opts: AudioTranscribeAzureOptions): string | null {
  if (opts.definition) {
    JSON.parse(opts.definition); // fail fast on bad JSON
    return opts.definition;
  }
  const def: Record<string, unknown> = {};
  if (opts.locales && opts.locales.length > 0) def.locales = opts.locales;
  if (opts.diarization) def.diarizationEnabled = true;
  if (opts.profanity) def.profanityFilterMode = opts.profanity;
  if (opts.channels) {
    def.channels = opts.channels
      .split(',')
      .map((c) => parseInt(c.trim(), 10))
      .filter((n) => !Number.isNaN(n));
  }
  return Object.keys(def).length > 0 ? JSON.stringify(def) : null;
}

export async function audioTranscribeAzureCommand(
  fileArg: string | undefined,
  options: AudioTranscribeAzureOptions,
  deps?: AudioTranscribeAzureDeps,
): Promise<void> {
  const fetchFn = deps?.fetch ?? globalThis.fetch;
  const stdout = deps?.stdout ?? process.stdout;
  const stderr = deps?.stderr ?? process.stderr;

  const definition = buildDefinition(options);
  if (!definition) {
    throw new Error(
      'Azure Fast Transcription requires a `definition` JSON. Pass at least one of:\n' +
        '  --locale <code>   (repeatable, e.g. --locale en-US --locale ko-KR)\n' +
        '  --diarization\n' +
        '  --profanity <None|Removed|Masked|Tags>\n' +
        '  --channels <0,1>\n' +
        '  --definition <raw JSON>',
    );
  }

  const { token, routerUrl } = await getAccessToken(deps);

  const { data, filename, contentType } = await resolveAudioInput(
    fileArg,
    options,
    deps ?? {},
  );

  const form = new FormData();
  form.append('audio', new Blob([data], { type: contentType }), filename);
  // Server-side handler expects `definition` as a plain string form field;
  // Starlette parses Blob parts as UploadFile and rejects them as not-a-string.
  form.append('definition', definition);

  const response = await fetchFn(`${routerUrl}${ENDPOINTS.azureSpeechToText}`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${token}`, ...ENTRYPOINT_HEADER },
    body: form,
  });

  if (!response.ok) await writeApiErrorAndExit(response as any, stderr);

  const body = await response.text();
  stdout.write(body);
  if (!body.endsWith('\n')) stdout.write('\n');
}
