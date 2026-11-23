import { invoke, isTauri } from '@tauri-apps/api/core';
import type { BrowseListing, BrowseRoots, ClipboardHistoryDetail, ClipboardHistoryResponse, ClipboardSnapshot, HardwareMetrics, MobileSession, MobileTransferView, NodeInfo, NodeSettings, NodeSettingsPatch, OutgoingPrepareView, OutgoingStagingView, PeerInfo, TransferHistoryCleared, TransferHistoryEntry, TransferOffer, TransferProgress } from './types';

export const NODE_PROTOCOL_VERSION = 2;

let baseUrl = '';
let suspendedBase = '';
type ManagedTarget = { nodeId: string; onMismatch: () => void };
let managedTarget: ManagedTarget | undefined;
export type ManagedRequestTarget = ManagedTarget & {
  base: string;
  isCurrent: () => boolean;
};
export type RequestTarget = string | ManagedRequestTarget;
const READ_TIMEOUT_MS = 6000;
const SETTINGS_WRITE_TIMEOUT_MS = 30_000;
// 目录浏览可能逐项读取 NAS 或网络挂载上的元数据。
const FILE_BROWSE_TIMEOUT_MS = 60_000;

/** HTTP 状态码保留在错误上，调用方才能区分对端拒绝与网络不可达。 */
export class ApiError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
  }
}

export async function initApi(): Promise<string> {
  return setApiTarget(isTauri() ? await invoke<string>('node_base_url') : window.location.origin);
}

export function setApiTarget(target: string, managed?: ManagedTarget): string {
  // 后台任务可能持有原地址；重连期间该地址只允许探测身份。
  suspendedBase = target ? '' : baseUrl;
  baseUrl = target.replace(/\/$/, '');
  managedTarget = managed;
  return baseUrl;
}

export function apiUrl(path: string, target = baseUrl): string {
  const base = target.replace(/\/$/, '');
  if (!base || (base === suspendedBase && path !== '/api/v1/info')) throw new ApiError('尚未连接到目标节点', 421);
  return `${base}${path}`;
}

export function websocketUrl(target = baseUrl): string {
  return apiUrl('/api/v1/ws', target).replace(/^http/, 'ws');
}

async function fetchNode(path: string, options: RequestInit, target: RequestTarget): Promise<Response> {
  const explicit = typeof target === 'string' ? undefined : target;
  const base = typeof target === 'string' ? target : target.base;
  const managed = path === '/api/v1/info' ? undefined : explicit ?? (base === baseUrl ? managedTarget : undefined);
  const isCurrent = () => explicit ? explicit.isCurrent() : managed === managedTarget;
  if (explicit && !isCurrent()) throw new ApiError('管理目标已变更', 421);
  const url = apiUrl(path, base);
  if (managed && options.method && options.method !== 'GET') {
    // 地址可能在重新发现前已被其他设备使用。只向当前验证过的节点写入。
    try {
      await verifyPeerAddress(managed.nodeId, base);
    } catch (error) {
      if (isCurrent() && error instanceof ApiError && error.status === 421) managed.onMismatch();
      throw error;
    }
    if (!isCurrent()) throw new ApiError('管理目标已变更', 421);
  }
  const headers = new Headers(options.headers);
  if (options.body && !(options.body instanceof FormData) && !headers.has('Content-Type')) headers.set('Content-Type', 'application/json');
  if (managed) {
    const expected = headers.get('X-MeshShare-Target');
    if (expected && expected !== managed.nodeId) throw new ApiError('管理目标已变更', 421);
    headers.set('X-MeshShare-Target', managed.nodeId);
  }
  const response = await fetch(url, {
    ...options,
    cache: 'no-store',
    redirect: 'error',
    headers,
  });
  if (!response.ok) {
    if (response.status === 421 && managed && isCurrent()) managed.onMismatch();
    throw await responseError(response);
  }
  return response;
}

function readJson<T>(response: Response, path: string): Promise<T> {
  return response.json().catch((cause: unknown) => {
    if (!(cause instanceof SyntaxError)) throw cause;
    const message = response.headers.get('content-type')?.startsWith('text/html')
      ? '未连接到 MeshShare 节点，请启动桌面应用'
      : `节点返回了无效的 JSON (${path})`;
    throw new Error(message, { cause });
  });
}

export async function request<T>(path: string, options: RequestInit = {}, target: RequestTarget = baseUrl): Promise<T> {
  return readJson<T>(await fetchNode(path, options, target), path);
}

export async function responseError(response: Response): Promise<ApiError> {
  const payload: unknown = await response.json().catch(() => null);
  const message = payload && typeof payload === 'object' && 'error' in payload && typeof payload.error === 'string'
    ? payload.error
    : `请求失败 (${response.status})`;
  return new ApiError(message, response.status);
}

export function isRetryableHttpStatus(status: number): boolean {
  return status >= 500 || [408, 425, 429].includes(status);
}

export function isTransportFailure(error: unknown): boolean {
  if (error instanceof ApiError) return error.status === 421 || isRetryableHttpStatus(error.status);
  return error instanceof TypeError || (error instanceof DOMException && error.name === 'AbortError');
}

async function verifyPeerAddress(nodeId: string, base: string, timeoutMs = READ_TIMEOUT_MS): Promise<NodeInfo> {
  const info = await requestWithTimeout<NodeInfo>('/api/v1/info', {}, base, timeoutMs);
  if (info.protocol_version !== NODE_PROTOCOL_VERSION) throw new ApiError('节点协议版本不匹配', 421);
  if (info.node_id !== nodeId) throw new ApiError('地址上的设备与目标不匹配', 421);
  return info;
}

/** 操作前核对候选地址上的节点身份，非暂时性的业务错误交还调用方。 */
export async function withPeerAddress<T>(
  nodeId: string,
  candidates: string[],
  perform: (base: string, node: NodeInfo) => Promise<T>,
  isCurrent: () => boolean = () => true,
): Promise<T> {
  const deadline = Date.now() + 12000;
  let lastError: unknown;
  for (const base of new Set(candidates)) {
    const remaining = deadline - Date.now();
    if (remaining <= 0 || !isCurrent()) break;
    let info: NodeInfo;
    try {
      info = await verifyPeerAddress(nodeId, base, Math.min(READ_TIMEOUT_MS, remaining));
    } catch (error) {
      lastError = error;
      continue;
    }
    if (!isCurrent()) break;
    try {
      return await perform(base, info);
    } catch (error) {
      if (!isTransportFailure(error)) throw error;
      lastError = error;
    }
  }
  throw lastError instanceof Error ? lastError : new Error('目标设备没有可访问地址');
}

export async function requestWithTimeout<T>(path: string, options: RequestInit, target: RequestTarget, timeoutMs = READ_TIMEOUT_MS): Promise<T> {
  const controller = new AbortController();
  const signal = options.signal;
  const cancel = () => controller.abort(signal?.reason);
  if (signal?.aborted) cancel();
  else signal?.addEventListener('abort', cancel, { once: true });
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    return await request<T>(path, { ...options, signal: controller.signal }, target);
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener('abort', cancel);
  }
}

async function requestList<T>(path: string, target = baseUrl): Promise<T[]> {
  const payload = await requestWithTimeout<unknown>(path, {}, target);
  if (!Array.isArray(payload)) throw new Error(`节点返回的列表格式无效 (${path})`);
  return payload;
}

export const getInfo = (target = baseUrl) => requestWithTimeout<NodeInfo>('/api/v1/info', {}, target);
export const getPeers = (target = baseUrl) => requestList<PeerInfo>('/api/v1/peers', target);
export const getMetrics = (target = baseUrl) => requestWithTimeout<HardwareMetrics | null>('/api/v1/metrics', {}, target);
/** 大快照按读取空闲时间超时，收到响应头或下一块内容即重新计时。 */
export async function getClipboard(target = baseUrl, controller = new AbortController()): Promise<ClipboardSnapshot | null> {
  const path = '/api/v1/clipboard';
  const abort = () => controller.abort(new DOMException('读取剪贴板超时', 'AbortError'));
  let timer = setTimeout(abort, READ_TIMEOUT_MS);
  const resetTimer = () => {
    clearTimeout(timer);
    timer = setTimeout(abort, READ_TIMEOUT_MS);
  };
  try {
    const response = await fetchNode(path, { signal: controller.signal }, target);
    resetTimer();
    const body = response.body?.pipeThrough(new TransformStream<Uint8Array, Uint8Array>({
      transform(chunk, output) {
        resetTimer();
        output.enqueue(chunk);
      },
    }));
    return await readJson<ClipboardSnapshot | null>(new Response(body, { headers: response.headers }), path);
  } finally {
    clearTimeout(timer);
  }
}

export const getSettings = (target: RequestTarget) => requestWithTimeout<NodeSettings>('/api/v1/settings', {}, target);
export const patchSettings = (patch: NodeSettingsPatch, target: RequestTarget) => requestWithTimeout<NodeSettings>('/api/v1/settings', { method: 'PATCH', body: JSON.stringify(patch) }, target, SETTINGS_WRITE_TIMEOUT_MS);
export const requestUpdateCheck = (target: RequestTarget) => requestWithTimeout<NodeSettings>('/api/v1/settings/update-check', { method: 'POST' }, target, SETTINGS_WRITE_TIMEOUT_MS);
// 节点可能在响应到达前重启；自动重试会对同一节点重复执行操作。
export async function restartService(target: ManagedRequestTarget): Promise<void> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), SETTINGS_WRITE_TIMEOUT_MS);
  try {
    const response = await fetchNode('/api/v1/management/service/restart', {
      method: 'POST', signal: controller.signal,
    }, target);
    if (response.status !== 202) throw new Error('节点未确认重启请求');
  } finally {
    clearTimeout(timer);
  }
}

export const getClipboardHistory = (target = baseUrl) => requestWithTimeout<ClipboardHistoryResponse>('/api/v1/clipboard/history', {}, target);
export const getClipboardHistoryDetail = (eventId: string, target = baseUrl) => request<ClipboardHistoryDetail>(`/api/v1/clipboard/history/${encodeURIComponent(eventId)}`, {}, target);
export const clipboardHistoryThumbnailUrl = (eventId: string, target = baseUrl) => apiUrl(`/api/v1/clipboard/history/${encodeURIComponent(eventId)}/thumbnail`, target);
export const restoreClipboardHistory = (eventId: string, target = baseUrl) => request<ClipboardSnapshot>(`/api/v1/clipboard/history/${encodeURIComponent(eventId)}/restore`, { method: 'POST' }, target);
export const clearClipboardHistory = (target = baseUrl) => request<{ cleared: number }>('/api/v1/clipboard/history', { method: 'DELETE' }, target);
export const getTransfers = (target = baseUrl) => requestList<TransferProgress>('/api/v1/transfers', target);
export const dismissTransfer = (transferId: string, target = baseUrl) => request<TransferProgress>(`/api/v1/transfers/${encodeURIComponent(transferId)}`, { method: 'DELETE' }, target);
export const getTransferHistory = (sessionId?: string, target = baseUrl) => requestList<TransferHistoryEntry>(`/api/v1/transfers/history${sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : ''}`, target);
export const clearTransferHistory = (sessionId?: string, target = baseUrl) => request<TransferHistoryCleared>(`/api/v1/transfers/history${sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : ''}`, { method: 'DELETE' }, target);
export const dismissMobileTransfer = (transferId: string, target = baseUrl) => request<TransferProgress>(`/api/v1/mobile/transfers/${encodeURIComponent(transferId)}`, { method: 'DELETE' }, target);
export const createMobileSession = (label?: string, target = baseUrl) => request<MobileSession>('/api/v1/mobile/sessions', { method: 'POST', body: JSON.stringify({ label }) }, target);
export const getMobileSessions = (target = baseUrl) => requestList<MobileSession>('/api/v1/mobile/sessions', target);
export const createMobileTransfer = (sessionId: string, offer: TransferOffer, target: string) => request<MobileTransferView>('/api/v1/mobile/transfers', { method: 'POST', body: JSON.stringify({ session_id: sessionId, offer }) }, target);
export const sendManagedPathsToMobile = (sessionId: string, paths: string[], target: string) => request<MobileTransferView>(
  '/api/v1/management/mobile/outgoing/paths', { method: 'POST', body: JSON.stringify({ session_id: sessionId, paths }) }, target,
);
export const getMobileTransfers = (sessionId?: string, target = baseUrl) => requestList<MobileTransferView>(`/api/v1/mobile/transfers${sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : ''}`, target);
export const mobileDownloadUrl = (transferId: string, itemId: string, sessionId: string, autoComplete = false) => apiUrl(`/api/v1/mobile/transfers/${encodeURIComponent(transferId)}/items/${encodeURIComponent(itemId)}/download?session_id=${encodeURIComponent(sessionId)}${autoComplete ? '&auto_complete=true' : ''}`);
/** 原生发送：先把清单和源文件交给本机节点，之后的上传由节点负责。 */
export const createOutgoingStaging = (offer: TransferOffer, target = baseUrl) => request<OutgoingStagingView>('/api/v1/outgoing', { method: 'POST', body: JSON.stringify(offer) }, target);
/** 远程管理页浏览目标机器目录：只读元数据，需要目标机器开启远程文件访问。 */
export const getManagedRoots = (target: RequestTarget = baseUrl, signal?: AbortSignal) =>
  requestWithTimeout<BrowseRoots>('/api/v1/management/files/roots', { signal }, target, FILE_BROWSE_TIMEOUT_MS);
export const browseManagedPath = (path: string, target: RequestTarget = baseUrl, signal?: AbortSignal) =>
  requestWithTimeout<BrowseListing>(`/api/v1/management/files?path=${encodeURIComponent(path)}`, { signal }, target, FILE_BROWSE_TIMEOUT_MS);

/** 按路径发送的准备任务始终由创建它的节点读取、取消和清理。 */
export function outgoingPathsApi(owner: string, managedNodeId?: string) {
  const root = managedNodeId ? '/api/v1/management/outgoing' : '/api/v1/outgoing';
  const headers = managedNodeId ? { 'X-MeshShare-Target': managedNodeId } : undefined;
  return {
    create: (targetNodeId: string, paths: string[]) => request<OutgoingPrepareView>(root + '/paths', {
      method: 'POST', headers, body: JSON.stringify({ target_node_id: targetNodeId, paths }),
    }, owner),
    status: (id: string) => requestWithTimeout<OutgoingPrepareView>(root + '/' + encodeURIComponent(id), { headers }, owner),
    cancel: async (id: string) => {
      // 离开管理页后仍只能取消原节点上的准备任务。
      if (managedNodeId && owner !== baseUrl) await verifyPeerAddress(managedNodeId, owner);
      return request(root + '/' + encodeURIComponent(id) + '/cancel', { method: 'POST', headers }, owner);
    },
  };
}

export const startOutgoingStaging = (transferId: string, target = baseUrl) => request<TransferProgress>(`/api/v1/outgoing/${encodeURIComponent(transferId)}/start`, { method: 'POST' }, target);
export const cancelOutgoingStaging = (transferId: string, target = baseUrl) => request<unknown>(`/api/v1/outgoing/${encodeURIComponent(transferId)}/cancel`, { method: 'POST' }, target);
export const completeMobileItem = (transferId: string, itemId: string, sessionId: string, target = baseUrl) => request<MobileTransferView>(`/api/v1/mobile/transfers/${encodeURIComponent(transferId)}/items/${encodeURIComponent(itemId)}/complete`, { method: 'POST', body: JSON.stringify({ session_id: sessionId }) }, target);
