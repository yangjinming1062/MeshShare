import assert from 'node:assert/strict';
import test from 'node:test';
import { snapshotRead } from '../src/snapshotRead.ts';

test('concurrent timers share one read and later events request one fresh snapshot', async () => {
  const finishes: Array<() => void> = [];
  const reader = snapshotRead(() => new Promise(resolve => finishes.push(resolve)));
  const first = reader.refresh();
  assert.equal(reader.refresh(), first);
  await Promise.resolve();
  assert.equal(finishes.length, 1);
  const second = reader.refresh(true);
  assert.notEqual(second, first);
  assert.equal(reader.refresh(true), second);
  assert.equal(reader.refresh(), first);
  finishes[0]();
  await first;
  assert.equal(finishes.length, 2);
  assert.equal(reader.refresh(), second);
  const third = reader.refresh(true);
  finishes[1]();
  await second;
  assert.equal(finishes.length, 3);
  finishes[2]();
  await third;
});

test('switching owner drops the old queued read and keeps the new read in flight', async () => {
  const finishes: Array<() => void> = [];
  const reader = snapshotRead(() => new Promise(resolve => finishes.push(resolve)));
  const old = reader.refresh();
  await Promise.resolve();
  reader.refresh(true);
  reader.reset();
  const current = reader.refresh();
  await Promise.resolve();
  finishes[0]();
  await old;
  assert.equal(reader.refresh(), current);
  assert.equal(finishes.length, 2);
  finishes[1]();
  await current;
});

test('a queued invalidation still reads after a failed request', async () => {
  const failed = Promise.withResolvers<void>();
  let reads = 0;
  const reader = snapshotRead(async () => {
    if (++reads === 1) await failed.promise;
  });
  const first = reader.refresh();
  await Promise.resolve();
  const retry = reader.refresh(true);
  failed.reject(new Error('offline'));
  await assert.rejects(first, /offline/);
  await retry;
  assert.equal(reads, 2);
});
