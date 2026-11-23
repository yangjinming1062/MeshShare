import assert from 'node:assert/strict';
import test from 'node:test';
import { setImmediate as nextTurn } from 'node:timers/promises';
import { ApiError, getClipboard, outgoingPathsApi, request, restartService, setApiTarget, withPeerAddress } from '../src/api.ts';

test('clipboard keeps reading while bytes arrive, including UTF-8 split across chunks', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const headers = Promise.withResolvers<Response>();
  let stream: ReadableStreamDefaultController<Uint8Array>;
  let signal: AbortSignal;
  const body = new ReadableStream<Uint8Array>({ start(controller) { stream = controller; } });
  t.mock.method(globalThis, 'fetch', (_url: string, options: RequestInit) => {
    signal = options.signal!;
    signal.addEventListener('abort', () => stream.error(signal.reason));
    return headers.promise;
  });
  const result = getClipboard('http://clipboard.local');
  t.mock.timers.tick(5000);
  headers.resolve(new Response(body));
  await nextTurn();
  const json = new TextEncoder().encode('{"text":"传输"}');
  for (const chunk of [json.slice(0, 10), json.slice(10)]) {
    t.mock.timers.tick(5000);
    assert.equal(signal!.aborted, false);
    stream!.enqueue(chunk);
    await nextTurn();
  }
  stream!.close();
  assert.deepEqual(await result, { text: '传输' });
  t.mock.timers.tick(6000);
  assert.equal(signal!.aborted, false);
});

test('clipboard timeouts stop a stalled connection or body and preserve the abort error', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  t.mock.method(globalThis, 'fetch', (url: string, { signal }: { signal: AbortSignal }) => {
    if (url.startsWith('http://no-headers.local/')) {
      return new Promise<Response>((_, reject) => signal.addEventListener('abort', () => reject(signal.reason)));
    }
    return new Response(new ReadableStream({
      start(stream) {
        stream.enqueue(new TextEncoder().encode('{"text":'));
        signal.addEventListener('abort', () => stream.error(signal.reason));
      },
    }));
  });
  for (const target of ['http://no-headers.local', 'http://stalled-body.local']) {
    const check = assert.rejects(getClipboard(target), { name: 'AbortError' });
    await nextTurn();
    t.mock.timers.tick(6001);
    await check;
  }
});

test('malformed clipboard JSON reports the endpoint and keeps the parse error', async t => {
  t.mock.method(globalThis, 'fetch', () => new Response('{"text":'));
  await assert.rejects(getClipboard('http://clipboard.local'), error =>
    error instanceof Error && error.message.includes('/api/v1/clipboard') && error.cause instanceof SyntaxError);
});

test('prepare status and cancellation stay on their creating node across target changes', async (t) => {
  const calls: string[] = [];
  t.mock.method(globalThis, 'fetch', async (url: string, options: RequestInit) => {
    calls.push(url);
    if (url.endsWith('/info')) return Response.json({ node_id: 'a', protocol_version: 2 });
    assert.equal(new Headers(options.headers).get('X-MeshShare-Target'), 'a');
    return Response.json({});
  });
  const paths = outgoingPathsApi('http://node-a.local', 'a');
  setApiTarget('http://node-b.local');
  await paths.status('prepare');
  await paths.cancel('prepare');
  assert.deepEqual(calls, [
    'http://node-a.local/api/v1/management/outgoing/prepare',
    'http://node-a.local/api/v1/info',
    'http://node-a.local/api/v1/management/outgoing/prepare/cancel',
  ]);
});

test('peer routes verify identity and propagate a business rejection immediately', async (t) => {
  t.mock.method(globalThis, 'fetch', async (url: string) => Response.json({
    node_id: url.includes('first.local') ? 'other-node' : 'target',
    protocol_version: url.includes('outdated.local') ? 1 : 2,
  }));
  const origins: string[] = [];
  const bases = ['http://first.local', 'http://outdated.local', 'http://second.local', 'http://third.local'];
  await assert.rejects(withPeerAddress('target', bases, async (base) => {
    origins.push(base);
    throw new ApiError('访问已关闭', 403);
  }), (error) => error instanceof ApiError && error.status === 403);
  assert.deepEqual(origins, [bases[2]]);
});


test('an address takeover suspends management until the original node is rediscovered', async t => {
  const oldBase = 'http://old.local';
  const newBase = 'http://new.local';
  const calls: string[] = [];
  t.mock.method(globalThis, 'fetch', async (url: string, options: RequestInit) => {
    calls.push(url);
    if (url.endsWith('/info')) return Response.json({ node_id: url.startsWith(oldBase) ? 'other' : 'nas', protocol_version: 2 });
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
    return Response.json({ node_id: 'nas', protocol_version: 2 });
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
