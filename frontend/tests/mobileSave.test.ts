import assert from 'node:assert/strict';
import test from 'node:test';
import { saveMobileResponse, type MobileDirectoryHandle, type MobileWritable } from '../src/mobileSave.ts';
import { Sha256 } from '../src/sha256.ts';

function fakeDirectory() {
  const files = new Map<string, Uint8Array>();
  const directory: MobileDirectoryHandle = {
    name: 'test',
    getDirectoryHandle: async () => directory,
    getFileHandle: async (name, options) => {
      if (!options?.create && !files.has(name)) throw new DOMException('missing', 'NotFoundError');
      return {
        createWritable: async () => {
          let pending = new Uint8Array();
          const writable: MobileWritable = {
            write: async (chunk) => {
              const next = new Uint8Array(pending.length + chunk.length);
              next.set(pending);
              next.set(chunk, pending.length);
              pending = next;
            },
            close: async () => files.set(name, pending),
            abort: async () => undefined,
          };
          return writable;
        },
      };
    },
    removeEntry: async (name) => {
      files.delete(name);
    },
  };
  return { directory, files };
}

test('keeps a verified file when completion notification fails', async () => {
  const { directory, files } = fakeDirectory();
  const bytes = new TextEncoder().encode('保存后通知失败仍保留');
  const hasher = new Sha256();
  hasher.update(bytes);

  await saveMobileResponse(
    directory,
    '报告.txt',
    new Response(bytes),
    bytes.length,
    hasher.digestHex(),
  );
  const completion = async () => {
    throw new Error('HTTP 500');
  };
  await assert.rejects(async () => {
    await completion();
  }, /HTTP 500/);

  assert.deepEqual(files.get('报告.txt'), bytes);
  const savedHash = new Sha256();
  savedHash.update(files.get('报告.txt')!);
  assert.equal(savedHash.digestHex(), hasher.digestHex());
});
