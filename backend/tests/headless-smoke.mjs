// Exercise an installed *disposable* systemd node. Requires Node 22+ on the same host.
import test from 'node:test';
import assert from 'node:assert/strict';
import { once } from 'node:events';
import { createHash, randomUUID } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { setTimeout as delay } from 'node:timers/promises';

async function waitFor(probe) {
  const end = Date.now() + 12_000;
  do {
    const result = await probe();
    if (result) return result;
    await delay(100);
  } while (Date.now() < end);
  assert.fail('Timed out waiting for the installed node');
}

test('installed headless service shares clipboard, accepts files and protects active work', async t => {
  const port = process.env.MESHSHARE_SMOKE_PORT ?? (await readFile('/run/meshshare-headless/port', 'utf8')).trim();
  const base = 'http://127.0.0.1:' + Number(port);
  const send = (path, method = 'GET', payload, headers = {}) => fetch(base + path, {
    method, headers: { ...(payload === undefined ? {} : { 'Content-Type': 'application/json' }), ...headers },
    body: JSON.stringify(payload), signal: AbortSignal.timeout(8000),
  });
  const json = async (...args) => {
    const response = await send(...args);
    if (!response.ok) assert.fail(response.status + ': ' + await response.text());
    return response.json();
  };
  const info = await json('/api/v1/info');
  assert.ok(info.capabilities.includes('node.headless.v1'));
  assert.ok(info.capabilities.includes('terminal.ssh.v3'));
  assert.ok(info.terminal.host_fingerprint);
  const settings = await json('/api/v1/settings');
  assert.equal(settings.clipboard_status.retrying, false);
  assert.equal(settings.receive_settings.mode, 'confirm');
  assert.match(await (await send('/mobile')).text(), /<script[^>]+src=/);
  const makePeer = async () => {
    const socket = new WebSocket(base.replace('http:', 'ws:') + '/api/v1/ws');
    t.after(() => socket.close());
    const messages = [];
    const id = randomUUID();
    socket.addEventListener('message', e => { messages.push(JSON.parse(e.data)); });
    await once(socket, 'open', {signal: AbortSignal.timeout(5000)});
    const tell = (type, payload) => socket.send(JSON.stringify({type, payload}));
    tell('Hello', {node: {...info, node_id: id, node_name: id, capabilities: ['clipboard.text'], terminal: null}});
    await waitFor(async () => (await json('/api/v1/peers')).some(p => p.node_id === id));
    return {id, messages, tell};
  };
  const sender = await makePeer();
  const receiver = await makePeer();
  const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNgYGAAAAAEAAH2FzhVAAAAAElFTkSuQmCC';
  for (const payload of [{text: randomUUID()}, {image_png: png}]) {
    const now = Date.now();
    const event = {event_id: randomUUID(), origin_node_id: sender.id, updated_at_ms: now,
      clock: {physical_ms: now, logical: 0, node_id: sender.id}, ...payload};
    sender.tell('ClipboardUpdate', {event});
    sender.tell('ClipboardUpdate', {event});
    await waitFor(() => receiver.messages.some(m => m.type === 'ClipboardUpdate' && m.payload.event.event_id === event.event_id));
    const {items} = await json('/api/v1/clipboard/history');
    assert.equal(items.filter(i => i.event_id === event.event_id).length, 1);
    assert.deepEqual((await json('/api/v1/clipboard'))[Object.keys(payload)[0]], Object.values(payload)[0]);
  }

  const roots = await json('/api/v1/management/files/roots');
  assert.ok(roots.home);
  const contents = Buffer.from(randomUUID());
  const id = randomUUID();
  const filename = 'acceptance-' + id;
  const offer = {transfer_id: id, source_node_id: sender.id, source_name: 'acceptance',
    target_node_id: info.node_id, created_at_ms: Date.now(), total_bytes: contents.length,
    items: [{item_id: 'sample', name: filename, relative_path: filename, size: contents.length,
      sha256: createHash('sha256').update(contents).digest('hex'), media_type: 'text/plain', directory: false}]};
  const path = '/api/v1/transfers/' + id;
  assert.equal((await json('/api/v1/transfers', 'POST', offer)).status, 'waiting');
  const restart = target => send('/api/v1/management/service/restart', 'POST', undefined, {'X-MeshShare-Target': target});
  assert.equal((await restart(randomUUID())).status, 421);
  assert.equal((await restart(info.node_id)).status, 409);
  assert.equal((await json(path + '/accept', 'POST', {destination: roots.home})).status, 'accepted');
  const uploaded = await fetch(base + path + '/items/sample', {method: 'PUT', body: contents});
  assert.ok(uploaded.ok, await uploaded.text());
  assert.equal((await json(path + '/status')).status, 'completed');
  const listing = await json('/api/v1/management/files?path=' + encodeURIComponent(roots.home));
  assert.ok(listing.entries.some(e => e.name === filename && e.size === contents.length));
  const mobile = await json('/api/v1/mobile/sessions', 'POST', {label: 'acceptance'});
  assert.equal(mobile.node_id, info.node_id);
  const view = await json('/api/v1/management/mobile/outgoing/paths', 'POST', {
    session_id: mobile.session_id, paths: [roots.home + '/' + filename],
  });
  assert.equal(view.progress.status, 'waiting');
  const mobilePath = '/api/v1/mobile/transfers/' + view.progress.transfer_id;
  await json(mobilePath + '/accept', 'POST', {session_id: mobile.session_id});
  const ready = await waitFor(async () => (await json('/api/v1/mobile/transfers?session_id=' + mobile.session_id))
    .find(v => v.progress.transfer_id === view.progress.transfer_id && v.progress.status === 'ready'));
  const item = ready.progress.offer.items[0];
  const download = await send(mobilePath + '/items/' + item.item_id + '/download?auto_complete=true&session_id=' + mobile.session_id);
  assert.equal(download.status, 200);
  assert.deepEqual(Buffer.from(await download.arrayBuffer()), contents);
});
