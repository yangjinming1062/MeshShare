import assert from 'node:assert/strict';
import test from 'node:test';
import { isAllowedPeerBase, normalizePeerEndpoint, peerTargetBase, peerTargetCandidates, type PeerWebContext } from '../src/peers.ts';
import type { AddressInfo, PeerInfo } from '../src/types.ts';

function address(url: string, kind = 'lan'): AddressInfo {
  return { address: url, url, label: url, kind };
}

function peer(overrides: Partial<PeerInfo> = {}): PeerInfo {
  return {
    node_id: 'node-a',
    node_name: 'A',
    platform: 'desktop',
    addresses: [],
    mobile_addresses: [],
    metrics_stale: false,
    capabilities: [],
    online: true,
    ...overrides,
  };
}

const web: PeerWebContext = { selfNodeId: 'node-a', origin: 'https://hub.test', secure: true };

test('normalizePeerEndpoint maps websocket endpoints to http bases', () => {
  assert.equal(normalizePeerEndpoint('ws://192.168.1.5:7880/api/v1/ws'), 'http://192.168.1.5:7880');
  assert.equal(normalizePeerEndpoint('wss://hub.local/api/v1/ws'), 'https://hub.local');
});

test('normalizePeerEndpoint rejects endpoints that are not websocket roots', () => {
  assert.equal(normalizePeerEndpoint('ws://192.168.1.5:7880/api/v1/info'), null);
  assert.equal(normalizePeerEndpoint('ws://user:pass@192.168.1.5:7880/api/v1/ws'), null);
  assert.equal(normalizePeerEndpoint('ws://192.168.1.5:7880/api/v1/ws?x=1'), null);
  assert.equal(normalizePeerEndpoint('ws://8.8.8.8:7880/api/v1/ws'), null);
  assert.equal(normalizePeerEndpoint('http://10.0.0.2:7880/api/v1/ws'), null);
  assert.equal(normalizePeerEndpoint('ws://10.0.0.2:7880/nested/api/v1/ws'), null);
  assert.equal(normalizePeerEndpoint('not a url'), null);
  assert.equal(normalizePeerEndpoint(null), null);
});

test('isAllowedPeerBase accepts only link-scope hosts', () => {
  for (const raw of [
    'http://localhost:7880',
    'http://printer.local:7880',
    'http://127.0.0.1:7880',
    'http://10.1.2.3:7880',
    'http://172.16.0.1:7880',
    'http://172.31.255.255:7880',
    'http://192.168.1.5:7880',
    'http://169.254.1.2:7880',
    'http://100.64.0.1:7880',
    'http://100.127.255.255:7880',
  ]) {
    assert.equal(isAllowedPeerBase(raw), true, raw);
  }
  for (const raw of [
    'http://8.8.8.8:7880',
    'http://172.32.0.1:7880',
    'http://100.128.0.1:7880',
    'http://example.com:7880',
    'http://[fe80::1]:7880',
    'http://192.168.1.5:7880?x=1',
    'ftp://192.168.1.5:7880',
  ]) {
    assert.equal(isAllowedPeerBase(raw), false, raw);
  }
});

test('peerTargetBase prefers the confirmed endpoint then lan addresses', () => {
  const target = peer({
    endpoint: 'ws://192.168.1.5:7880/api/v1/ws',
    addresses: [address('http://100.64.0.1:7880', 'tailscale'), address('http://192.168.1.5:7880')],
  });
  assert.equal(peerTargetBase(target, null), 'http://192.168.1.5:7880');
  assert.equal(peerTargetBase(peer({ addresses: [address('http://100.64.0.1:7880', 'tailscale'), address('http://192.168.1.5:7880')] }), null), 'http://192.168.1.5:7880');
  assert.equal(peerTargetBase(peer({ addresses: [address('http://8.8.8.8:7880'), address('http://100.64.0.1:7880', 'tailscale')] }), null), 'http://100.64.0.1:7880');
  assert.equal(peerTargetBase(peer({ addresses: [address('http://8.8.8.8:7880')] }), null), null);
});

test('peerTargetBase uses the web proxy for remote nodes and origin for self', () => {
  assert.equal(peerTargetBase(peer(), web), 'https://hub.test');
  assert.equal(peerTargetBase(peer({ node_id: 'node-b' }), web), 'https://hub.test/api/v1/mobile/peers/node-b');
  const insecure: PeerWebContext = { ...web, secure: false };
  assert.equal(peerTargetBase(peer({ node_id: 'node-b', addresses: [address('http://192.168.1.5:7880')] }), insecure), 'http://192.168.1.5:7880');
});

test('peerTargetCandidates dedupes and drops link-local when alternatives exist', () => {
  const target = peer({
    endpoint: 'ws://192.168.1.5:7880/api/v1/ws',
    addresses: [address('http://192.168.1.5:7880/'), address('http://169.254.1.2:7880'), address('http://100.64.0.1:7880', 'tailscale')],
  });
  assert.deepEqual(peerTargetCandidates(target, null), ['http://192.168.1.5:7880', 'http://100.64.0.1:7880']);
  assert.deepEqual(peerTargetCandidates(peer({ addresses: [address('http://169.254.1.2:7880')] }), null), ['http://169.254.1.2:7880']);
  assert.deepEqual(peerTargetCandidates(peer({ node_id: 'node-b' }), web), ['https://hub.test/api/v1/mobile/peers/node-b']);
});
