import assert from 'node:assert/strict';
import test from 'node:test';
import { filesFromDataTransfer } from '../src/drop.ts';

function fileEntry(name: string, content = name) {
  return {
    name,
    isFile: true,
    isDirectory: false,
    file: (callback: (file: File) => void) => callback(new File([content], name)),
  };
}

function directoryEntry(name: string, children: unknown[]) {
  return {
    name,
    isFile: false,
    isDirectory: true,
    createReader: () => ({
      readEntries: (callback: (entries: unknown[]) => void) => {
        callback(children);
        children = [];
      },
    }),
  };
}

function transfer(entries: unknown[], files: File[] = []): DataTransfer {
  return {
    items: entries.map((entry) => ({ webkitGetAsEntry: () => entry })),
    files,
  } as unknown as DataTransfer;
}

test('falls back to plain files when the drop carries no entries', async () => {
  const plain = [new File(['x'], 'plain.txt')];
  const files = await filesFromDataTransfer(transfer([], plain));
  assert.deepEqual(files.map((file) => file.name), ['plain.txt']);
});

test('collects dropped entries recursively with relative paths', async () => {
  const nested = directoryEntry('folder', [fileEntry('b.txt', 'b'), directoryEntry('sub', [fileEntry('a.txt', 'a')])]);
  const files = await filesFromDataTransfer(transfer([fileEntry('top.txt', 'top'), nested]));
  assert.deepEqual(
    files.map((file) => [file.name, (file as File & { webkitRelativePath?: string }).webkitRelativePath]),
    [
      ['top.txt', 'top.txt'],
      ['b.txt', 'folder/b.txt'],
      ['a.txt', 'folder/sub/a.txt'],
    ],
  );
});

test('reads directory entries across multiple batches', async () => {
  const batches = [[fileEntry('one.txt', '1')], [fileEntry('two.txt', '2')], []];
  const folder = {
    name: 'batch',
    isFile: false,
    isDirectory: true,
    createReader: () => ({ readEntries: (callback: (entries: unknown[]) => void) => callback(batches.shift() ?? []) }),
  };
  const files = await filesFromDataTransfer(transfer([folder]));
  assert.deepEqual(files.map((file) => file.name), ['one.txt', 'two.txt']);
});
