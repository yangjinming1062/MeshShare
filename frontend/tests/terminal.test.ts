import assert from 'node:assert/strict';
import test from 'node:test';
import { setImmediate as flush } from 'node:timers/promises';
import { TerminalSession, type TerminalEvent, type TerminalTransport } from '../src/terminal.ts';

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => { resolve = done; });
  return { promise, resolve };
}

function session(open = Promise.resolve(), firstWrite = Promise.resolve()) {
  let receive!: (event: TerminalEvent) => void;
  const writes: Uint8Array[] = [];
  const acknowledgements: number[] = [];
  const sizes: number[][] = [];
  const rendered: { data: Uint8Array; done: () => void }[] = [];
  const states: string[] = [];
  let closed = 0;
  const transport: TerminalTransport = {
    open: (_cols, _rows, callback) => { receive = callback; return open; },
    write: async (data) => { writes.push(data); if (writes.length === 1) await firstWrite; },
    ack: async (sequence) => { acknowledgements.push(sequence); },
    resize: async (cols, rows) => { sizes.push([cols, rows]); },
    close: async () => { closed++; },
  };
  const ui = new TerminalSession(transport, (data, done) => rendered.push({ data, done }), (state) => states.push(state));
  const opened = ui.start(80, 24);
  return { ui, opened, send: (event: TerminalEvent) => receive(event), writes, acknowledgements, sizes, rendered, states, closeCount: () => closed };
}

test('tabs acknowledge their own output only when the renderer finishes', async () => {
  const local = session();
  const remote = session();
  await Promise.all([local.opened, remote.opened]);
  local.send({ type: 'data', sequence: 1, data: [240, 159] });
  remote.send({ type: 'data', sequence: 10, data: [65, 66] });
  assert.deepEqual([...local.rendered[0].data], [240, 159]);
  assert.deepEqual([...remote.rendered[0].data], [65, 66]);
  assert.deepEqual(local.acknowledgements, []);
  remote.rendered[0].done();
  assert.deepEqual(remote.acknowledgements, [10]);
  assert.deepEqual(local.acknowledgements, []);
  local.rendered[0].done();
  assert.deepEqual(local.acknowledgements, [1]);
});

test('large pastes and following keystrokes stay ordered; a disconnected tab drops pending input', async () => {
  const writable = deferred();
  const tab = session(Promise.resolve(), writable.promise);
  await tab.opened;
  tab.ui.write(new Uint8Array(8193).fill(1));
  tab.ui.write(Uint8Array.of(2));
  await flush();
  assert.deepEqual(tab.writes.map((chunk) => chunk.length), [8192]);
  writable.resolve();
  await flush();
  assert.deepEqual(tab.writes.map((chunk) => [chunk.length, chunk[0]]), [[8192, 1], [1, 1], [1, 2]]);
  tab.ui.write(Uint8Array.of(3));
  tab.send({ type: 'exit', reason: '连接已断开' });
  tab.ui.write(Uint8Array.of(4));
  await flush();
  assert.equal(tab.writes.length, 3);
  assert.deepEqual(tab.states, ['connected', 'ended']);
});

test('resizes during pairing apply once after connection and closing a pending tab cancels its eventual session', async () => {
  const open = deferred();
  const tab = session(open.promise);
  tab.ui.resize(90, 24);
  tab.ui.resize(100, 30);
  open.resolve();
  await tab.opened;
  assert.deepEqual(tab.sizes, [[100, 30]]);

  const late = deferred();
  const cancelled = session(late.promise);
  cancelled.ui.dispose();
  late.resolve();
  await cancelled.opened;
  assert.equal(cancelled.closeCount(), 2);
  assert.deepEqual(cancelled.states, []);
  cancelled.send({ type: 'data', sequence: 1, data: [65] });
  assert.deepEqual(cancelled.rendered, []);
});

test('an early exit remains read-only even if the open acknowledgement arrives later', async () => {
  const open = deferred();
  const tab = session(open.promise);
  tab.send({ type: 'exit', reason: 'shell 已退出' });
  open.resolve();
  await tab.opened;
  tab.ui.write(Uint8Array.of(13));
  await flush();
  assert.deepEqual(tab.states, ['ended']);
  assert.deepEqual(tab.writes, []);
});
