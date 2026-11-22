import assert from 'node:assert/strict';
import test from 'node:test';
import { ApiError, outgoingPathsApi, setApiTarget, withPeerAddress } from '../src/api.ts';

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
