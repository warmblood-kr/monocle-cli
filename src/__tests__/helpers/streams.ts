export interface CapturedStream {
  out: NodeJS.WritableStream;
  text: () => string;
  bytes: () => Buffer;
}

export function makeStream(): CapturedStream {
  const chunks: Buffer[] = [];
  return {
    out: {
      write: (chunk: any) => {
        chunks.push(typeof chunk === 'string' ? Buffer.from(chunk) : chunk);
        return true;
      },
    } as any,
    text: () => Buffer.concat(chunks).toString('utf-8'),
    bytes: () => Buffer.concat(chunks),
  };
}
