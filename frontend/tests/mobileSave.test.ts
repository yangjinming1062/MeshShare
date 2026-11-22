import assert from 'node:assert/strict';
import test from 'node:test';
import { saveMobileResponse, type MobileDirectoryHandle, type MobileFileHandle, type MobileWritable } from '../src/mobileSave.ts';
import { Sha256 } from '../src/sha256.ts';

// Node 22 没有浏览器的 Web Locks，按同名请求的先后顺序运行回调。
if (!navigator.locks) {
  const pending = new Map<string, Promise<unknown>>();
  Object.defineProperty(navigator, 'locks', { value: {
    request(name: string, run: () => Promise<unknown>) {
      const next = (pending.get(name) ?? Promise.resolve()).then(run, run);
      pending.set(name, next);
      return next;
    },
  } });
}

function fakeDirectory() {
  const files = new Map<string, Uint8Array>();
  const handles = new Map<string, MobileFileHandle>();
  const directory: MobileDirectoryHandle = {
    name: 'test',
    getDirectoryHandle: async () => directory,
    getFileHandle: async (name, options) => {
      if (!files.has(name)) {
        if (!options?.create) throw new DOMException('missing', 'NotFoundError');
        files.set(name, new Uint8Array());
      }
      const existing = handles.get(name);
      if (existing) return existing;
      const handle: MobileFileHandle = {
        getFile: async () => ({ size: files.get(name)!.length, lastModified: 0 }),
        isSameEntry: async (other) => handle === other,
        createWritable: async () => {
          let pending = new Uint8Array();
          const writable: MobileWritable = {
            write: async (chunk) => {
              const next = new Uint8Array(pending.length + chunk.length);
              next.set(pending);
              next.set(chunk, pending.length);
              pending = next;
            },
            close: async () => { files.set(name, pending); },
            abort: async () => undefined,
          };
          return writable;
        },
      };
      handles.set(name, handle);
      return handle;
    },
    removeEntry: async (name) => {
      files.delete(name);
      handles.delete(name);
    },
  };
  return { directory, files };
}

test('two downloads with the same destination preserve the first successful file', async () => {
  const { directory, files } = fakeDirectory();
  const payloads = [new Uint8Array([1]), new Uint8Array([2])];
  const results = await Promise.allSettled(payloads.map((bytes) => {
    const hash = new Sha256();
    hash.update(bytes);
    return saveMobileResponse(directory, 'shared.bin', new Response(bytes), bytes.length, hash.digestHex());
  }));
  assert.equal(results.filter((result) => result.status === 'fulfilled').length, 1);
  const winner = results.findIndex((result) => result.status === 'fulfilled');
  assert.deepEqual(files.get('shared.bin'), payloads[winner]);
  assert.match(String(results.find((result) => result.status === 'rejected')?.reason), /目标文件已存在/);
});

test('a failed writer acquisition leaves another download intact', async () => {
  const { directory, files } = fakeDirectory();
  const open = directory.getFileHandle;
  const other = new Uint8Array([8, 9]);
  directory.getFileHandle = async (name, options) => {
    const handle = await open(name, options);
    if (!options?.create) return handle;
    return { ...handle, createWritable: async () => {
      files.set(name, other);
      throw new Error('writer unavailable');
    } };
  };
  await assert.rejects(saveMobileResponse(directory, 'busy.bin', new Response('x'), 1, ''), /writer unavailable/);
  assert.deepEqual(files.get('busy.bin'), other);
});

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

test('download only creates a new file after a missing-file result', async () => {
  const { directory, files } = fakeDirectory();
  const saveEmpty = (dir: MobileDirectoryHandle, path: string) =>
    saveMobileResponse(dir, path, new Response(''), 0, new Sha256().digestHex());
  files.set('已有.txt', new Uint8Array([1]));
  await assert.rejects(saveEmpty(directory, '已有.txt'), /目标文件已存在/);
  await assert.rejects(saveEmpty(directory, '../文件'), /传输路径无效/);
  const denied = { ...directory, getFileHandle: async () => { throw new Error('directory unavailable'); } };
  await assert.rejects(saveEmpty(denied, '文件'), /directory unavailable/);
  const target = await saveEmpty(directory, '子目录/文件');
  assert.equal(target.fileName, '文件');
});

test('verification failure removes only the original empty entry', async () => {
  const { directory, files } = fakeDirectory();
  await assert.rejects(saveMobileResponse(directory, 'bad.bin', new Response('x'), 1, ''), /文件校验失败/);
  assert.ok(!files.has('bad.bin'));

  const response = new Response(new ReadableStream({
    async pull(controller) {
      await directory.removeEntry('replaced.bin');
      await directory.getFileHandle('replaced.bin', { create: true });
      controller.close();
    },
  }, { highWaterMark: 0 }));
  await assert.rejects(saveMobileResponse(directory, 'replaced.bin', response, 0, new Sha256().digestHex()), /目标文件已变化/);
  assert.deepEqual(files.get('replaced.bin'), new Uint8Array());
});
