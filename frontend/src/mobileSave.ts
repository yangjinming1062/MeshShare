import { Sha256 } from './sha256.ts';

export type MobileWritable = {
  write: (data: Uint8Array) => Promise<void>;
  close: () => Promise<void>;
  abort: () => Promise<void>;
};

export type MobileFileHandle = {
  createWritable: (options: { mode: 'exclusive' }) => Promise<MobileWritable>;
  getFile: () => Promise<{ size: number; lastModified: number }>;
  isSameEntry: (other: MobileFileHandle) => Promise<boolean>;
};

export type MobileDirectoryHandle = {
  name: string;
  getDirectoryHandle: (name: string, options?: { create?: boolean }) => Promise<MobileDirectoryHandle>;
  getFileHandle: (name: string, options?: { create?: boolean }) => Promise<MobileFileHandle>;
  removeEntry: (name: string, options?: { recursive?: boolean }) => Promise<void>;
};

async function writeVerified(
  reader: ReadableStreamDefaultReader<Uint8Array>,
  writable: MobileWritable,
  expectedSize: number,
  expectedSha256: string,
) {
  const hasher = new Sha256();
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > expectedSize) throw new Error('文件校验失败');
    hasher.update(value);
    await writable.write(value);
  }
  if (total !== expectedSize || hasher.digestHex().toLowerCase() !== expectedSha256.toLowerCase()) {
    throw new Error('文件校验失败');
  }
}

async function createNewFile(directory: MobileDirectoryHandle, fileName: string, relativePath: string) {
  try {
    await directory.getFileHandle(fileName);
  } catch (error) {
    if (error instanceof DOMException && error.name === 'NotFoundError') return directory.getFileHandle(fileName, { create: true });
    throw error;
  }
  throw new Error(`目标文件已存在：${relativePath}`);
}

/** 校验并提交到本地文件后再由调用方通知节点；通知失败时文件仍可用。 */
export async function saveMobileResponse(
  root: MobileDirectoryHandle,
  relativePath: string,
  response: Response,
  expectedSize: number,
  expectedSha256: string,
) {
  if (!response.body) throw new Error('浏览器不支持流式下载');
  const reader = response.body.getReader();
  try {
    const parts = relativePath.split('/');
    if (relativePath.includes('\\') || parts.some((part) => !part || part === '.' || part === '..')) throw new Error('传输路径无效');
    const fileName = parts[parts.length - 1];
    let directory = root;
    for (const part of parts.slice(0, -1)) directory = await directory.getDirectoryHandle(part, { create: true });
    // 同源标签页可通过不同根目录选中同一文件。用末级名称互斥，独立文件仍可并行写入。
    return await navigator.locks.request(`meshshare-save/${fileName.normalize('NFC').toLowerCase()}`, async () => {
      const handle = await createNewFile(directory, fileName, relativePath);
      const empty = await handle.getFile();
      if (empty.size) throw new Error(`目标文件已存在：${relativePath}`);
      const writable = await handle.createWritable({ mode: 'exclusive' });
      const isOriginalEmptyFile = async () => {
        const current = await directory.getFileHandle(fileName);
        if (!(await handle.isSameEntry(current))) return false;
        const file = await current.getFile();
        return file.size === 0 && file.lastModified === empty.lastModified;
      };
      try {
        await writeVerified(reader, writable, expectedSize, expectedSha256);
        if (!(await isOriginalEmptyFile())) throw new Error(`目标文件已变化：${relativePath}`);
        await writable.close();
      } catch (error) {
        await writable.abort().catch(() => undefined);
        try {
          if (await isOriginalEmptyFile()) await directory.removeEntry(fileName);
        } catch { /* 文件或目录已不可访问，保留原始保存错误。 */ }
        throw error;
      }
      return { directory, fileName };
    });
  } catch (error) {
    await reader.cancel().catch(() => undefined);
    throw error;
  } finally {
    reader.releaseLock();
  }
}
