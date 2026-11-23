import assert from 'node:assert/strict';
import test from 'node:test';
import { ApiError, getClipboard, outgoingPathsApi, request, restartService, setApiTarget, withPeerAddress } from '../src/api.ts';

test('prepare status and cancellation stay on their creating node across target changes', async (t) => {
  const calls: string[] = [];
  t.mock.method(globalThis, 'fetch', async (url: string) => {
    calls.push(url);
    return Response.json({});
  });
  const paths = outgoingPathsApi('http://node-a.local', true);
  setApiTarget('http://node-b.local');
  await paths.status('prepare');
  await paths.cancel('prepare');
  assert.deepEqual(calls, [
    'http://node-a.local/api/v1/management/outgoing/prepare',
    'http://node-a.local/api/v1/management/outgoing/prepare/cancel',
  ]);
});

test('peer routes verify identity and propagate a business rejection immediately', async (t) => {
  t.mock.method(globalThis, 'fetch', async (url: string) => Response.json({
    node_id: url.includes('first.local') ? 'other-node' : 'target',
  }));
  const origins: string[] = [];
  const bases = ['http://first.local', 'http://second.local', 'http://third.local'];
  await assert.rejects(withPeerAddress('target', bases, async (base) => {
    origins.push(base);
    throw new ApiError('访问已关闭', 403);
  }), (error) => error instanceof ApiError && error.status === 403);
  assert.deepEqual(origins, [bases[1]]);
});


test('an address takeover suspends management until the original node is rediscovered', async t => {
  const oldBase = 'http://old.local';
  const newBase = 'http://new.local';
  const calls: string[] = [];
  t.mock.method(globalThis, 'fetch', async (url: string, options: RequestInit) => {
    calls.push(url);
    if (url.endsWith('/info')) return Response.json({ node_id: url.startsWith(oldBase) ? 'other' : 'nas' });
    assert.equal(new Headers(options.headers).get('X-MeshShare-Target'), 'nas');
    return Response.json({});
  });
  const managed = { nodeId: 'nas', onMismatch: () => setApiTarget('') };
  setApiTarget(oldBase, managed);
  t.after(() => setApiTarget(''));
  const publish = () => request('/api/v1/clipboard', { method: 'POST', body: JSON.stringify({ text: 'draft' }) });
  await assert.rejects(publish(), { status: 421 });
  await assert.rejects(getClipboard(), { status: 421 });
  await assert.rejects(getClipboard(oldBase), { status: 421 });
  await assert.rejects(publish(), { status: 421 });
  assert.deepEqual(calls, [oldBase + '/api/v1/info']);

  await withPeerAddress('nas', [oldBase, newBase], async base => setApiTarget(base, managed));
  await publish();
  assert.deepEqual(calls.slice(1), [
    oldBase + '/api/v1/info', newBase + '/api/v1/info', newBase + '/api/v1/info', newBase + '/api/v1/clipboard',
  ]);
});

test('a pending identity probe cannot publish after switching back to the local node', async t => {
  const calls: string[] = [];
  t.mock.method(globalThis, 'fetch', async (url: string) => {
    calls.push(url);
    setApiTarget('http://home.local');
    return Response.json({ node_id: 'nas' });
  });
  setApiTarget('http://nas.local', { nodeId: 'nas', onMismatch: () => assert.fail('obsolete context') });
  await assert.rejects(request('/api/v1/clipboard', { method: 'POST', body: '{}' }), { status: 421 });
  assert.deepEqual(calls, ['http://nas.local/api/v1/info']);
});

test('service restart binds the requested node and accepts the empty 202 response', async t => {
  const calls: string[] = [];
  t.mock.method(globalThis, 'fetch', async (url: string, options: RequestInit) => {
    calls.push(url);
    assert.equal(options.method, 'POST');
    assert.equal(new Headers(options.headers).get('X-MeshShare-Target'), 'nas');
    return new Response(null, { status: 202 });
  });
  await restartService('http://nas.local', 'nas');
  assert.deepEqual(calls, ['http://nas.local/api/v1/management/service/restart']);
});
