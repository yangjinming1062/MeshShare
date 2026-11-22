import { invoke, isTauri } from '@tauri-apps/api/core';
import type { BrowseListing, BrowseRoots, ClipboardHistoryDetail, ClipboardHistoryResponse, ClipboardSnapshot, HardwareMetrics, MobileSession, MobileTransferView, NodeInfo, NodeSettings, NodeSettingsPatch, OutgoingPrepareView, OutgoingStagingView, PeerInfo, TransferHistoryCleared, TransferHistoryEntry, TransferOffer, TransferProgress } from './types';

let baseUrl = '';
const SETTINGS_WRITE_TIMEOUT_MS = 30_000;

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

export function setApiTarget(target: string): string {
  baseUrl = target.replace(/\/$/, '');
  return baseUrl;
}

export function apiUrl(path: string, target = baseUrl): string {
  return `${target.replace(/\/$/, '')}${path}`;
}

export function websocketUrl(target = baseUrl): string {
  return apiUrl('/api/v1/ws', target).replace(/^http/, 'ws');
}

export async function request<T>(path: string, options: RequestInit = {}, target = baseUrl): Promise<T> {
  const response = await fetch(apiUrl(path, target), {
    ...options,
    cache: 'no-store',
    redirect: 'error',
    headers: {
      ...(options.body && !(options.body instanceof FormData) ? { 'Content-Type': 'application/json' } : {}),
      ...(options.headers ?? {}),
    },
  });
  if (!response.ok) throw await responseError(response);
  return response.json().catch((cause: unknown) => {
    const message = response.headers.get('content-type')?.startsWith('text/html')
      ? '未连接到 MeshShare 节点，请启动桌面应用'
      : `节点返回了无效的 JSON (${path})`;
    throw new Error(message, { cause });
  });
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

/** 操作前核对候选地址上的节点身份，非暂时性的业务错误交还调用方。 */
export async function withPeerAddress<T>(nodeId: string, candidates: string[], perform: (base: string) => Promise<T>): Promise<T> {
  const deadline = Date.now() + 12000;
  let lastError: unknown;
  for (const base of new Set(candidates)) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) break;
    try {
      const info = await requestWithTimeout<NodeInfo>('/api/v1/info', {}, base, Math.min(6000, remaining));
      if (info.node_id !== nodeId) throw new ApiError('地址上的设备与目标不匹配', 421);
    } catch (error) {
      lastError = error;
      continue;
    }
    try {
      return await perform(base);
    } catch (error) {
      if (!isTransportFailure(error)) throw error;
      lastError = error;
    }
  }
  throw lastError instanceof Error ? lastError : new Error('目标设备没有可访问地址');
}

export async function requestWithTimeout<T>(path: string, options: RequestInit, target: string, timeoutMs = 6000): Promise<T> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    return await request<T>(path, { ...options, signal: controller.signal }, target);
  } finally {
    clearTimeout(timer);
  }
}

async function requestList<T>(path: string, target = baseUrl): Promise<T[]> {
  const payload = await request<unknown>(path, {}, target);
  if (!Array.isArray(payload)) throw new Error(`节点返回的列表格式无效 (${path})`);
  return payload;
}

export const getInfo = (target = baseUrl) => request<NodeInfo>('/api/v1/info', {}, target);
export const getPeers = (target = baseUrl) => requestList<PeerInfo>('/api/v1/peers', target);
export const getMetrics = (target = baseUrl) => request<HardwareMetrics | null>('/api/v1/metrics', {}, target);
export const getClipboard = (target = baseUrl) => request<ClipboardSnapshot | null>('/api/v1/clipboard', {}, target);
export const getSettings = (target: string) => requestWithTimeout<NodeSettings>('/api/v1/settings', {}, target);
export const patchSettings = (patch: NodeSettingsPatch, target: string) => requestWithTimeout<NodeSettings>('/api/v1/settings', { method: 'PATCH', body: JSON.stringify(patch) }, target, SETTINGS_WRITE_TIMEOUT_MS);
export const requestUpdateCheck = (target: string) => requestWithTimeout<NodeSettings>('/api/v1/settings/update-check', { method: 'POST' }, target, SETTINGS_WRITE_TIMEOUT_MS);
export const getClipboardHistory = (target = baseUrl) => request<ClipboardHistoryResponse>('/api/v1/clipboard/history', {}, target);
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
export const getMobileTransfers = (sessionId?: string, target = baseUrl) => requestList<MobileTransferView>(`/api/v1/mobile/transfers${sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : ''}`, target);
export const mobileDownloadUrl = (transferId: string, itemId: string, sessionId: string, autoComplete = false) => apiUrl(`/api/v1/mobile/transfers/${encodeURIComponent(transferId)}/items/${encodeURIComponent(itemId)}/download?session_id=${encodeURIComponent(sessionId)}${autoComplete ? '&auto_complete=true' : ''}`);
/** 原生发送：先把清单和源文件交给本机节点，之后的上传由节点负责。 */
export const createOutgoingStaging = (offer: TransferOffer, target = baseUrl) => request<OutgoingStagingView>('/api/v1/outgoing', { method: 'POST', body: JSON.stringify(offer) }, target);
/** 远程管理页浏览目标机器目录：只读元数据，需要目标机器开启远程文件访问。 */
export const getManagedRoots = (target = baseUrl) => request<BrowseRoots>('/api/v1/management/files/roots', {}, target);
export const browseManagedPath = (path: string, target = baseUrl) => request<BrowseListing>(`/api/v1/management/files?path=${encodeURIComponent(path)}`, {}, target);

/** 按路径发送的准备任务始终由创建它的节点读取、取消和清理。 */
export function outgoingPathsApi(owner: string, managed: boolean) {
  const root = managed ? '/api/v1/management/outgoing' : '/api/v1/outgoing';
  return {
    create: (targetNodeId: string, paths: string[]) => request<OutgoingPrepareView>(root + '/paths', {
      method: 'POST', body: JSON.stringify({ target_node_id: targetNodeId, paths }),
    }, owner),
    status: (id: string) => request<OutgoingPrepareView>(root + '/' + encodeURIComponent(id), {}, owner),
    cancel: (id: string) => request(root + '/' + encodeURIComponent(id) + '/cancel', { method: 'POST' }, owner),
  };
}

export const startOutgoingStaging = (transferId: string, target = baseUrl) => request<TransferProgress>(`/api/v1/outgoing/${encodeURIComponent(transferId)}/start`, { method: 'POST' }, target);
export const cancelOutgoingStaging = (transferId: string, target = baseUrl) => request<unknown>(`/api/v1/outgoing/${encodeURIComponent(transferId)}/cancel`, { method: 'POST' }, target);
export const completeMobileItem = (transferId: string, itemId: string, sessionId: string, target = baseUrl) => request<MobileTransferView>(`/api/v1/mobile/transfers/${encodeURIComponent(transferId)}/items/${encodeURIComponent(itemId)}/complete`, { method: 'POST', body: JSON.stringify({ session_id: sessionId }) }, target);
