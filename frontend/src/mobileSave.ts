import { Sha256 } from './sha256.ts';

export type MobileWritable = {
  write: (data: Uint8Array) => Promise<void>;
  close: () => Promise<void>;
  abort?: () => Promise<void>;
};

export type MobileFileHandle = {
  createWritable: () => Promise<MobileWritable>;
};

export type MobileDirectoryHandle = {
  name: string;
  getDirectoryHandle: (name: string, options?: { create?: boolean }) => Promise<MobileDirectoryHandle>;
  getFileHandle: (name: string, options?: { create?: boolean }) => Promise<MobileFileHandle>;
  removeEntry: (name: string, options?: { recursive?: boolean }) => Promise<void>;
};

/**
 * Save and verify a directory-download response. Completion is deliberately
 * outside this function so a notification failure cannot enter the cleanup
 * path after the writable has been successfully closed.
 */
export async function saveMobileResponse(
  directory: MobileDirectoryHandle,
  fileName: string,
  response: Response,
  expectedSize: number,
  expectedSha256: string,
): Promise<void> {
  if (!response.body) throw new Error('浏览器不支持流式下载');
  const fileHandle = await directory.getFileHandle(fileName, { create: true });
  const writable = await fileHandle.createWritable();
  const reader = response.body.getReader();
  const hasher = new Sha256();
  let total = 0;
  let closed = false;
  try {
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      const bytes = chunk.value;
      total += bytes.byteLength;
      hasher.update(bytes);
      await writable.write(bytes);
    }
    if (total !== expectedSize || hasher.digestHex().toLowerCase() !== expectedSha256.toLowerCase()) {
      throw new Error('文件校验失败');
    }
    await writable.close();
    closed = true;
  } catch (error) {
    await reader.cancel().catch(() => undefined);
    if (!closed) await writable.abort?.().catch(() => undefined);
    await directory.removeEntry(fileName).catch(() => undefined);
    throw error;
  }
}
