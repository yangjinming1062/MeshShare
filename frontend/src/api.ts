import { invoke } from '@tauri-apps/api/core';
import type { BrowseListing, BrowseRoots, ClipboardHistoryDetail, ClipboardHistoryResponse, ClipboardSnapshot, ClipboardStatus, HardwareMetrics, MobileSession, MobileTransferView, NodeInfo, NodeSettings, NodeSettingsPatch, OutgoingPrepareView, OutgoingStagingView, PeerInfo, TransferHistoryCleared, TransferHistoryEntry, TransferOffer, TransferProgress } from './types';

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
  try {
    baseUrl = await invoke<string>('node_base_url');
  } catch {
    // 浏览器上下文没有 Tauri 桥；页面由节点提供，来源就是节点地址。
    baseUrl = window.location.origin;
  }
  return baseUrl.replace(/\/$/, '');
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
  const httpError = `请求失败 (${response.status})`;
  const payload: unknown = await response.json().catch((cause: unknown) => {
    if (!response.ok) throw new ApiError(httpError, response.status);
    const isHtml = response.headers.get('content-type')?.startsWith('text/html');
    const message = isHtml
      ? '未连接到 MeshShare 节点，请启动桌面应用'
      : `节点返回了无效的 JSON (${path})`;
    throw new Error(message, { cause });
  });
  if (!response.ok) {
    const message = typeof payload === 'object' && payload !== null && 'error' in payload && typeof payload.error === 'string'
      ? payload.error
      : httpError;
    throw new ApiError(message, response.status);
  }
  return payload as T;
}

export async function requestWithTimeout<T>(path: string, options: RequestInit, target: string, timeoutMs = 6000): Promise<T> {
  const controller = new AbortController();
  const timer = window.setTimeout(() => controller.abort(), timeoutMs);
  try {
    return await request<T>(path, { ...options, signal: controller.signal }, target);
  } finally {
    window.clearTimeout(timer);
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
export const getClipboardStatus = (target = baseUrl) => request<ClipboardStatus>('/api/v1/clipboard/status', {}, target);
export const getSettings = (target: string) => requestWithTimeout<NodeSettings>('/api/v1/settings', {}, target);
export const patchSettings = (patch: NodeSettingsPatch, target: string) => requestWithTimeout<NodeSettings>('/api/v1/settings', { method: 'PATCH', body: JSON.stringify(patch) }, target, SETTINGS_WRITE_TIMEOUT_MS);
export const requestUpdateCheck = (target: string) => requestWithTimeout<NodeSettings>('/api/v1/settings/update-check', { method: 'POST' }, target, SETTINGS_WRITE_TIMEOUT_MS);
export const getClipboardHistory = (target = baseUrl) => request<ClipboardHistoryResponse>('/api/v1/clipboard/history', {}, target);
export const getClipboardHistoryDetail = (eventId: string, target = baseUrl) => request<ClipboardHistoryDetail>(`/api/v1/clipboard/history/${encodeURIComponent(eventId)}`, {}, target);
export const restoreClipboardHistory = (eventId: string, target = baseUrl) => request<ClipboardSnapshot>(`/api/v1/clipboard/history/${encodeURIComponent(eventId)}/restore`, { method: 'POST' }, target);
export const clearClipboardHistory = (target = baseUrl) => request<{ cleared: number }>('/api/v1/clipboard/history', { method: 'DELETE' }, target);
export const getTransfers = (target = baseUrl) => requestList<TransferProgress>('/api/v1/transfers', target);
export const dismissTransfer = (transferId: string, target = baseUrl) => request<TransferProgress>(`/api/v1/transfers/${encodeURIComponent(transferId)}`, { method: 'DELETE' }, target);
export const getTransferHistory = (sessionId?: string, target = baseUrl) => requestList<TransferHistoryEntry>(`/api/v1/transfers/history${sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : ''}`, target);
export const clearTransferHistory = (sessionId?: string, target = baseUrl) => request<TransferHistoryCleared>(`/api/v1/transfers/history${sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : ''}`, { method: 'DELETE' }, target);
export const dismissMobileTransfer = (transferId: string, target = baseUrl) => request<TransferProgress>(`/api/v1/mobile/transfers/${encodeURIComponent(transferId)}`, { method: 'DELETE' }, target);
export const createMobileSession = (label?: string, target = baseUrl) => request<MobileSession>('/api/v1/mobile/sessions', { method: 'POST', body: JSON.stringify({ label }) }, target);
export const getMobileSessions = (target = baseUrl) => requestList<MobileSession>('/api/v1/mobile/sessions', target);
export const getMobileTransfers = (sessionId?: string, target = baseUrl) => requestList<MobileTransferView>(`/api/v1/mobile/transfers${sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : ''}`, target);
export const mobileDownloadUrl = (transferId: string, itemId: string, sessionId: string, autoComplete = false) => apiUrl(`/api/v1/mobile/transfers/${encodeURIComponent(transferId)}/items/${encodeURIComponent(itemId)}/download?session_id=${encodeURIComponent(sessionId)}${autoComplete ? '&auto_complete=true' : ''}`);
/** 原生发送：先把清单和源文件交给本机节点，之后的上传由节点负责。 */
export const createOutgoingStaging = (offer: TransferOffer, target = baseUrl) => request<OutgoingStagingView>('/api/v1/outgoing', { method: 'POST', body: JSON.stringify(offer) }, target);
/** 桌面端按原路径直传：节点枚举、计算摘要后自己发送清单。 */
export const createOutgoingPaths = (targetNodeId: string, paths: string[], target = baseUrl) => request<OutgoingPrepareView>('/api/v1/outgoing/paths', { method: 'POST', body: JSON.stringify({ target_node_id: targetNodeId, paths }) }, target);
export const getOutgoingPrepare = (transferId: string, target = baseUrl) => request<OutgoingPrepareView>(`/api/v1/outgoing/${encodeURIComponent(transferId)}`, {}, target);
/** 远程管理页浏览目标机器目录：只读元数据，需要目标机器开启远程文件访问。 */
export const getManagedRoots = (target = baseUrl) => request<BrowseRoots>('/api/v1/management/files/roots', {}, target);
export const browseManagedPath = (path: string, target = baseUrl) => request<BrowseListing>(`/api/v1/management/files?path=${encodeURIComponent(path)}`, {}, target);
/** 远程管理页按目标机器上的路径建立发送任务，文件由目标机器读取并直传。 */
export const createManagedOutgoingPaths = (targetNodeId: string, paths: string[], target = baseUrl) => request<OutgoingPrepareView>('/api/v1/management/outgoing/paths', { method: 'POST', body: JSON.stringify({ target_node_id: targetNodeId, paths }) }, target);
export const getManagedOutgoingPrepare = (transferId: string, target = baseUrl) => request<OutgoingPrepareView>(`/api/v1/management/outgoing/${encodeURIComponent(transferId)}`, {}, target);
export const cancelManagedOutgoingPrepare = (transferId: string, target = baseUrl) => request<unknown>(`/api/v1/management/outgoing/${encodeURIComponent(transferId)}/cancel`, { method: 'POST' }, target);
export const startOutgoingStaging = (transferId: string, target = baseUrl) => request<TransferProgress>(`/api/v1/outgoing/${encodeURIComponent(transferId)}/start`, { method: 'POST' }, target);
export const cancelOutgoingStaging = (transferId: string, target = baseUrl) => request<unknown>(`/api/v1/outgoing/${encodeURIComponent(transferId)}/cancel`, { method: 'POST' }, target);
export const completeMobileItem = (transferId: string, itemId: string, sessionId: string, target = baseUrl) => request<MobileTransferView>(`/api/v1/mobile/transfers/${encodeURIComponent(transferId)}/items/${encodeURIComponent(itemId)}/complete`, { method: 'POST', body: JSON.stringify({ session_id: sessionId }) }, target);
