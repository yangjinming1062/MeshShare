import type { PeerInfo } from './types';

/** 网页模式下其他节点只能经接入节点的同源代理访问；非网页环境传 null。 */
export type PeerWebContext = {
  selfNodeId: string | null;
  origin: string;
  secure: boolean;
};

export function normalizePeerEndpoint(endpoint: string | null | undefined): string | null {
  if (!endpoint) return null;
  try {
    const parsed = new URL(endpoint);
    if (!['http:', 'https:', 'ws:', 'wss:'].includes(parsed.protocol) || parsed.username || parsed.password || parsed.search || parsed.hash) return null;
    if (!parsed.pathname.endsWith('/api/v1/ws')) return null;
    parsed.protocol = parsed.protocol === 'ws:' ? 'http:' : parsed.protocol === 'wss:' ? 'https:' : parsed.protocol;
    parsed.pathname = '';
    const base = parsed.toString().replace(/\/$/, '');
    return isAllowedPeerBase(base) ? base : null;
  } catch {
    return null;
  }
}

export function isAllowedPeerBase(raw: string): boolean {
  try {
    const parsed = new URL(raw);
    if (!['http:', 'https:'].includes(parsed.protocol) || parsed.username || parsed.password || parsed.search || parsed.hash) return false;
    const host = parsed.hostname.toLowerCase();
    if (host === 'localhost' || host.endsWith('.local')) return true;
    const octets = host.split('.').map((value) => Number(value));
    if (octets.length !== 4 || octets.some((value) => !Number.isInteger(value) || value < 0 || value > 255)) return false;
    const [a, b] = octets;
    return a === 127 || a === 10 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168)
      || (a === 169 && b === 254) || (a === 100 && b >= 64 && b <= 127);
  } catch {
    return false;
  }
}

export function peerTargetBase(peer: PeerInfo, web: PeerWebContext | null): string | null {
  if (web) {
    if (peer.node_id === web.selfNodeId) return web.origin;
    if (web.secure) return `${web.origin}/api/v1/mobile/peers/${encodeURIComponent(peer.node_id)}`;
  }
  return normalizePeerEndpoint(peer.endpoint)
    ?? peer.addresses.find((address) => isAllowedPeerBase(address.url) && address.kind === 'lan')?.url
    ?? peer.addresses.find((address) => isAllowedPeerBase(address.url))?.url
    ?? null;
}

/**
 * 对端可能同时公布多个地址（WiFi、Tailscale、虚拟网卡），其中只有一部分能被本机访问。
 * 发送时按公布顺序逐个尝试，并受总探测预算限制，避免不可达地址拖住界面。
 */
export function peerTargetCandidates(peer: PeerInfo, web: PeerWebContext | null): string[] {
  if (web) {
    const base = peerTargetBase(peer, web);
    return base ? [base] : [];
  }
  const bases: string[] = [];
  const endpointBase = normalizePeerEndpoint(peer.endpoint);
  if (endpointBase) bases.push(endpointBase);
  bases.push(...peer.addresses.map((address) => address.url).filter((url): url is string => !!url && isAllowedPeerBase(url)));
  const unique = new Set<string>();
  for (const base of bases) {
    const normalized = base.replace(/\/$/, '');
    if (normalized) unique.add(normalized);
  }
  const candidates = [...unique];
  // 链路本地地址只有在没有其他入口时才值得尝试，否则先试常规地址。
  const routable = candidates.filter((base) => !base.includes('//169.254.'));
  return routable.length ? routable : candidates;
}
