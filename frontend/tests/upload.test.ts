import assert from 'node:assert/strict';
import test from 'node:test';
import { buildOffer, uploadFileWithRetry, uploadItems } from '../src/upload.ts';
import type { NodeInfo } from '../src/types.ts';

test('resume skips confirmed items and abort prevents another upload', async () => {
  const files = [new File(['a'], 'a'), new File(['b'], 'b')];
  const offer = buildOffer({ node_id: 'owner', node_name: 'test' } as NodeInfo, 'peer', files);
  const controller = new AbortController();
  const completed = new Set<string>();
  const calls: string[] = [];
  await assert.rejects(uploadItems(offer, files, controller.signal, async item => {
    calls.push(item.name);
    if (item.name === 'b') throw new Error('offline');
  }, completed), /offline/);
  await uploadItems(offer, files, controller.signal, async item => { calls.push(item.name); }, completed);
  assert.deepEqual(calls, ['a', 'b', 'b']);
  controller.abort();
  await assert.rejects(uploadItems(offer, files, controller.signal, async () => assert.fail(), completed), { name: 'AbortError' });
});

test('binary uploads carry the intended HTTP hop and propagate a rejection', async t => {
  let requests = 0;
  t.mock.method(globalThis, 'fetch', async (_url: string, options: RequestInit) => {
    ++requests;
    assert.equal(new Headers(options.headers).get('X-MeshShare-Target'), 'peer');
    return Response.json({ error: 'wrong node' }, { status: 421 });
  });
  await assert.rejects(uploadFileWithRetry('http://node.local/item', new File(['x'], 'x'), 'application/octet-stream', 'peer', new AbortController().signal), { status: 421 });
  assert.equal(requests, 1);
});
