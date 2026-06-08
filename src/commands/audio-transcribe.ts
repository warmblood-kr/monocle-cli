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

export interface AudioTranscribeOptions {
  model?: string;
  language?: string;
  prompt?: string;
  responseFormat?: string;
  temperature?: string;
  filename?: string;
  contentType?: string;
}

export interface AudioTranscribeDeps extends AudioInputDeps {
  credentials?: Credentials;
  refreshDeps?: RefreshDeps;
  fetch?: typeof globalThis.fetch;
  now?: () => Date;
  stdout?: NodeJS.WritableStream;
  stderr?: NodeJS.WritableStream;
}

export async function audioTranscribeCommand(
  fileArg: string | undefined,
  options: AudioTranscribeOptions,
  deps?: AudioTranscribeDeps,
): Promise<void> {
  const fetchFn = deps?.fetch ?? globalThis.fetch;
  const stdout = deps?.stdout ?? process.stdout;
  const stderr = deps?.stderr ?? process.stderr;

  const { token, routerUrl } = await getAccessToken(deps);

  const { data, filename, contentType } = await resolveAudioInput(
    fileArg,
    options,
    deps ?? {},
  );

  const form = new FormData();
  form.append('file', new Blob([data], { type: contentType }), filename);
  if (options.model) form.append('model', options.model);
  if (options.language) form.append('language', options.language);
  if (options.prompt) form.append('prompt', options.prompt);
  if (options.responseFormat) form.append('response_format', options.responseFormat);
  if (options.temperature) form.append('temperature', options.temperature);

  const response = await fetchFn(`${routerUrl}${ENDPOINTS.audioTranscriptions}`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${token}`, ...ENTRYPOINT_HEADER },
    body: form,
  });

  if (!response.ok) await writeApiErrorAndExit(response as any, stderr);

  const body = await response.text();
  stdout.write(body);
  if (!body.endsWith('\n')) stdout.write('\n');
}
