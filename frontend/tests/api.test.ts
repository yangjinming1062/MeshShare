import assert from 'node:assert/strict';
import test from 'node:test';
import { setImmediate as nextTurn } from 'node:timers/promises';
import { ApiError, browseManagedPath, clipboardPreviewPath, getClipboard, isNewerClipboardSnapshot, getManagedRoots, getSettings, outgoingPathsApi, patchSettings, request, requestUpdateCheck, restartService, setApiTarget, withPeerAddress, type ManagedRequestTarget } from '../src/api.ts';

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

test('clipboard requests and previews identify the snapshot version and work with older nodes', async t => {
  const calls: string[] = [];
  const snapshot = {
    event_id: 'image & text',
    clock: { physical_ms: 12, logical: 3, node_id: 'peer/a' },
    text: '共享',
  };
  t.mock.method(globalThis, 'fetch', (url: string) => {
    calls.push(url);
    return url.startsWith('http://new.local') ? new Response(null, { status: 204 }) : Response.json(snapshot);
  });
  assert.equal(await getClipboard('http://new.local', new AbortController(), snapshot), undefined);
  assert.deepEqual(await getClipboard('http://old.local', new AbortController(), { ...snapshot, event_id: 'older' }), snapshot);
  assert.deepEqual(calls, [
    'http://new.local/api/v1/clipboard?known_event_id=image%20%26%20text&known_clock=12%3A3%3Apeer%2Fa',
    'http://old.local/api/v1/clipboard?known_event_id=older&known_clock=12%3A3%3Apeer%2Fa',
  ]);
  assert.equal(clipboardPreviewPath(snapshot), '/api/v1/clipboard/preview/image%20%26%20text?clock=12%3A3%3Apeer%2Fa');
});

test('clipboard responses use the node clock ordering when they reach the interface out of order', () => {
  const snapshot = (physical_ms: number, logical: number, node_id: string) =>
    ({ event_id: 'event', clock: { physical_ms, logical, node_id } });
  const current = snapshot(100, 5, 'peer');
  assert.equal(isNewerClipboardSnapshot(current, null), true);
  assert.equal(isNewerClipboardSnapshot(current, current), false);
  assert.equal(isNewerClipboardSnapshot(snapshot(101, 0, 'a'), current), true);
  assert.equal(isNewerClipboardSnapshot(snapshot(99, 8, 'z'), current), false);
  assert.equal(isNewerClipboardSnapshot(snapshot(100, 6, 'a'), current), true);
  assert.equal(isNewerClipboardSnapshot(snapshot(100, 4, 'z'), current), false);
  assert.equal(isNewerClipboardSnapshot(snapshot(100, 5, '😺'), snapshot(100, 5, '豈')), true);
  assert.equal(isNewerClipboardSnapshot(snapshot(100, 5, 'peer-2'), current), true);
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

test('explicit management sends reads and writes to the chosen node while the local target stays active', async t => {
  const local = 'http://home.local';
  const remote = 'http://desktop.local';
  const calls: string[] = [];
  setApiTarget(local);
  t.after(() => setApiTarget(''));
  const target: ManagedRequestTarget = {
    base: remote, nodeId: 'desk', isCurrent: () => true, onMismatch: () => assert.fail('unexpected mismatch'),
  };
  t.mock.method(globalThis, 'fetch', async (url: string, options: RequestInit) => {
    const method = options.method ?? 'GET';
    const header = new Headers(options.headers).get('X-MeshShare-Target');
    calls.push(method + ' ' + url);
    if (url === remote + '/api/v1/info') {
      assert.equal(header, null);
      return Response.json({ node_id: 'desk', protocol_version: 2 });
    }
    assert.equal(header, url.startsWith(remote) ? 'desk' : null);
    return Response.json({});
  });
  await getSettings(target);
  await getManagedRoots(target);
  await browseManagedPath('/incoming', target);
  await patchSettings({ receive_settings: { mode: 'auto', default_path: '/incoming' } }, target);
  await requestUpdateCheck(target);
  await request('/api/v1/peers');
  assert.deepEqual(calls, [
    'GET ' + remote + '/api/v1/settings',
    'GET ' + remote + '/api/v1/management/files/roots',
    'GET ' + remote + '/api/v1/management/files?path=%2Fincoming',
    'GET ' + remote + '/api/v1/info',
    'PATCH ' + remote + '/api/v1/settings',
    'GET ' + remote + '/api/v1/info',
    'POST ' + remote + '/api/v1/settings/update-check',
    'GET ' + local + '/api/v1/peers',
  ]);
});

test('remote file browsing can finish after the standard read timeout', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const roots = { home: '/nas', roots: [] };
  const listing = { path: '/nas', parent: '/', entries: [], truncated: false };
  t.mock.method(globalThis, 'fetch', (url: string, { signal }: RequestInit) => {
    assert.ok(signal);
    return new Promise<Response>((resolve, reject) => {
      const timer = setTimeout(() => resolve(Response.json(url.endsWith('/roots') ? roots : listing)), 7000);
      signal.addEventListener('abort', () => {
        clearTimeout(timer);
        reject(signal.reason);
      }, { once: true });
    });
  });
  const base = 'http://nas.local';
  const result = Promise.all([getManagedRoots(base), browseManagedPath('/nas', base)]);
  t.mock.timers.tick(7000);
  assert.deepEqual(await result, [roots, listing]);
});

test('remote file browsing aborts stalled reads and honors caller cancellation', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  t.mock.method(globalThis, 'fetch', (_url: string, { signal }: RequestInit) => {
    assert.ok(signal);
    return new Promise<Response>((_, reject) => {
      signal.addEventListener('abort', () => reject(signal.reason), { once: true });
    });
  });
  const base = 'http://nas.local';
  const stalled = assert.rejects(browseManagedPath('/nas', base), { name: 'AbortError' });
  t.mock.timers.tick(60_000);
  await stalled;

  const controller = new AbortController();
  const pending = assert.rejects(getManagedRoots(base, controller.signal), { name: 'AbortError' });
  controller.abort();
  await pending;
});

test('explicit writes reject a replacement before submission or when the address changes after verification', async t => {
  for (const takeoverBeforeProbe of [true, false]) {
    const calls: string[] = [];
    let mismatches = 0;
    const target: ManagedRequestTarget = {
      base: 'http://desk.local', nodeId: 'desk', isCurrent: () => true, onMismatch: () => { mismatches += 1; },
    };
    const fetch = t.mock.method(globalThis, 'fetch', async (url: string, options: RequestInit) => {
      calls.push(options.method ?? 'GET');
      if (url.endsWith('/info')) {
        return Response.json({ node_id: takeoverBeforeProbe ? 'different' : 'desk', protocol_version: 2 });
      }
      assert.equal(new Headers(options.headers).get('X-MeshShare-Target'), 'desk');
      return Response.json({ error: '地址上的设备与目标不匹配' }, { status: 421 });
    });
    await assert.rejects(patchSettings({ autostart_enabled: true }, target), { status: 421 });
    assert.deepEqual(calls, takeoverBeforeProbe ? ['GET'] : ['GET', 'PATCH']);
    assert.equal(mismatches, 1);
    fetch.mock.restore();
  }
});

test('closing an explicit settings session stops discovery or a pending write before submission', async t => {
  const base = 'http://desk.local';
  const calls: string[] = [];
  let open = true;
  const target: ManagedRequestTarget = {
    base, nodeId: 'desk', isCurrent: () => open, onMismatch: () => assert.fail('closed session'),
  };
  t.mock.method(globalThis, 'fetch', async (url: string) => {
    calls.push(url);
    assert.ok(url.endsWith('/info'));
    open = false;
    return Response.json({ node_id: 'desk', protocol_version: 2 });
  });
  await assert.rejects(withPeerAddress('desk', [base, 'http://next.local'], () => getSettings(target), target.isCurrent));
  open = true;
  await assert.rejects(patchSettings({ autostart_enabled: true }, target), { status: 421 });
  assert.deepEqual(calls, [base + '/api/v1/info', base + '/api/v1/info']);
});

test('service restart verifies and binds the requested node, then accepts the empty 202 response', async t => {
  const calls: string[] = [];
  t.mock.method(globalThis, 'fetch', async (url: string, options: RequestInit) => {
    calls.push(url);
    if (url.endsWith('/info')) return Response.json({ node_id: 'nas', protocol_version: 2 });
    assert.equal(options.method, 'POST');
    assert.equal(new Headers(options.headers).get('X-MeshShare-Target'), 'nas');
    return new Response(null, { status: 202 });
  });
  await restartService({ base: 'http://nas.local', nodeId: 'nas', isCurrent: () => true, onMismatch: () => assert.fail('unexpected mismatch') });
  assert.deepEqual(calls, ['http://nas.local/api/v1/info', 'http://nas.local/api/v1/management/service/restart']);
});
