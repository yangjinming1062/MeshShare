import type { ClipboardHistoryItem, ClipboardHistorySettings, DiskMetrics, HardwareMetrics, LoadAverage, PeerInfo, TransferHistoryEntry, TransferPrepare, TransferProgress, TransferStatus, UpdateFrequency, UpdateStatus } from './types';

export const METRICS_STALE_AFTER_MS = 5_000;

export function formatSize(bytes: number): string {
  if (bytes < 1024) return bytes + ' B';
  if (bytes < 1024 ** 2) return (bytes / 1024).toFixed(1) + ' KB';
  if (bytes < 1024 ** 3) return (bytes / 1024 ** 2).toFixed(1) + ' MB';
  return (bytes / 1024 ** 3).toFixed(1) + ' GB';
}

export function formatRate(value: number | null | undefined): string {
  if (value == null) return '—';
  if (value >= 1024 ** 2) return (value / 1024 ** 2).toFixed(1) + ' MB/s';
  if (value >= 1024) return (value / 1024).toFixed(1) + ' KB/s';
  return Math.round(value) + ' B/s';
}

export function formatRateCompact(value: number | null | undefined): string {
  if (value == null) return '—';
  if (value >= 1024 ** 2) return (value / 1024 ** 2).toFixed(1) + 'M';
  if (value >= 1024) return Math.round(value / 1024) + 'K';
  return Math.round(value) + 'B';
}

export function formatPercent(value: number | null | undefined): string {
  return value == null ? '—' : Math.round(value) + '%';
}

export function formatMemory(value: number | null | undefined): string {
  return value == null ? '—' : formatSize(value);
}

export function formatFrequency(value: number | null | undefined): string {
  if (value == null) return '—';
  if (value >= 1000) return (value / 1000).toFixed(2) + ' GHz';
  return Math.round(value) + ' MHz';
}

export function formatDuration(value: number | null | undefined): string {
  if (value == null) return '—';
  let remaining = Math.max(0, Math.floor(value));
  const days = Math.floor(remaining / 86_400);
  remaining %= 86_400;
  const hours = Math.floor(remaining / 3_600);
  remaining %= 3_600;
  const minutes = Math.floor(remaining / 60);
  const seconds = remaining % 60;
  const parts: string[] = [];
  if (days) parts.push(days + ' 天');
  if (hours || days) parts.push(hours + ' 小时');
  if (minutes || hours || days) parts.push(minutes + ' 分钟');
  if (!parts.length || seconds) parts.push(seconds + ' 秒');
  return parts.join(' ');
}

export function formatLoadAverage(value: LoadAverage | null | undefined): string {
  if (!value) return '—';
  return [value.one, value.five, value.fifteen].map((item) => Number.isFinite(item) ? item.toFixed(2) : '—').join(' / ');
}

export function diskUsedBytes(value: DiskMetrics): number {
  return Math.max(0, value.total_bytes - Math.min(value.total_bytes, value.available_bytes));
}

export function diskUsedPercent(value: DiskMetrics | null | undefined): number | undefined {
  if (!value || value.total_bytes <= 0) return undefined;
  return diskUsedBytes(value) / value.total_bytes * 100;
}

export function memoryPercent(value: HardwareMetrics | null | undefined): number | undefined {
  if (value?.memory_used_bytes == null || value.memory_total_bytes == null) return undefined;
  if (value.memory_total_bytes <= 0) return undefined;
  return value.memory_used_bytes / value.memory_total_bytes * 100;
}

export function gpuUtilization(value: HardwareMetrics | null | undefined): number | undefined {
  const values = (value?.gpus ?? []).map((gpu) => gpu.utilization_percent).filter((item): item is number => item != null);
  return values.length ? Math.max(...values) : undefined;
}

export function gpuMemoryPercent(value: HardwareMetrics | null | undefined): number | undefined {
  const values = (value?.gpus ?? [])
    .map((gpu) => (gpu.memory_used_bytes != null && gpu.memory_total_bytes ? gpu.memory_used_bytes / gpu.memory_total_bytes * 100 : undefined))
    .filter((item): item is number => item != null);
  return values.length ? Math.max(...values) : undefined;
}

export function isMetricsStale(value: HardwareMetrics | null | undefined): boolean {
  return value != null && Date.now() - value.sampled_at_ms > METRICS_STALE_AFTER_MS;
}

export function peerSummary(peer: PeerInfo): string {
  if (peer.metrics_stale) return '指标已过期';
  const value = peer.metrics;
  if (!value) return '指标采集中';
  return 'CPU ' + formatPercent(value.cpu_usage_percent)
    + ' · 内存 ' + formatPercent(memoryPercent(value))
    + ' · ↓ ' + formatRate(value.network_down_bytes_per_sec)
    + ' ↑ ' + formatRate(value.network_up_bytes_per_sec);
}

export function formatTime(value: number | undefined): string {
  return value ? new Date(value).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '';
}

export function formatConnectedSince(since: number): string {
  const minutes = Math.max(0, Math.round((Date.now() - since) / 60000));
  if (minutes < 1) return '刚刚';
  if (minutes < 60) return minutes + ' 分钟';
  const hours = Math.round(minutes / 60);
  if (hours < 24) return hours + ' 小时';
  return Math.round(hours / 24) + ' 天';
}

export function transferItemsLabel(transfer: TransferProgress): string {
  const names = transfer.offer?.items.map((item) => item.name) ?? [];
  if (names.length) return names.slice(0, 2).join('、') + (names.length > 2 ? ' 等 ' + names.length + ' 项' : '');
  return transfer.total_items + ' 个项目';
}

export function transferItemKey(transferId: string, itemId: string): string {
  return transferId + ':' + itemId;
}

export function isActiveTransferStatus(status: TransferStatus): boolean {
  return status === 'waiting' || status === 'accepted' || status === 'uploading' || status === 'ready';
}

export function prepareProgressRatio(prepare: TransferPrepare): number {
  if (!prepare.total_bytes) return 0;
  return Math.max(0, Math.min(1, prepare.hashed_bytes / prepare.total_bytes));
}

export function prepareStatusText(prepare: TransferPrepare): string {
  const label = prepare.phase === 'stage' ? '上传到本机' : '准备发送';
  if (!prepare.total_bytes) return label;
  return label + ' · ' + Math.round(prepareProgressRatio(prepare) * 100) + '%';
}

export function transferStatusText(transfer: TransferProgress, surface: 'desktop' | 'web' | 'android'): string {
  const ratio = transfer.total_bytes
    ? Math.max(0, Math.min(1, transfer.completed_bytes / transfer.total_bytes))
    : 0;
  if (transfer.status === 'waiting') return '等待确认';
  if (transfer.status === 'accepted') {
    return transfer.direction === 'send' ? '等待发送' : '准备接收';
  }
  if (transfer.status === 'ready') {
    if (surface === 'desktop') return '等待网页端保存';
    return surface === 'android' ? '正在写入应用目录' : '等待保存';
  }
  if (transfer.status === 'completed') return '已完成 · ' + formatSize(transfer.total_bytes);
  if (transfer.status === 'rejected') return '已拒绝';
  if (transfer.status === 'cancelled') return '已取消';
  if (transfer.status === 'failed') return transfer.error ?? '失败';
  return formatSize(transfer.completed_bytes) + ' / ' + formatSize(transfer.total_bytes)
    + ' · ' + Math.round(ratio * 100) + '%';
}

export function transferHistoryPeerText(entry: TransferHistoryEntry): string {
  return entry.direction === 'receive' ? '来自 ' + entry.peer_name : '发送到 ' + entry.peer_name;
}

export function transferHistoryPathText(entry: TransferHistoryEntry): string {
  if (entry.direction === 'receive') {
    return entry.destination ? '保存到 ' + entry.destination : '保存位置未记录';
  }
  const roots = entry.source_paths ?? [];
  if (!roots.length) return '来源未知';
  return '来源 ' + roots[0] + (roots.length > 1 ? ' 等 ' + roots.length + ' 处' : '');
}

/** 网页会话记录由节点以「发送到本会话」写入；会话页面需要按接收方视角展示。 */
export function transferHistoryForSessionView(entry: TransferHistoryEntry, nodeId: string, nodeName: string): TransferHistoryEntry {
  if (!entry.session_id) return entry;
  return { ...entry, direction: 'receive', peer_node_id: nodeId, peer_name: nodeName };
}

export function transferHistoryStatusText(entry: TransferHistoryEntry): string {
  if (entry.status === 'completed') return '已完成 · ' + formatSize(entry.total_bytes);
  if (entry.status === 'rejected') return '已拒绝';
  if (entry.status === 'cancelled') return '已取消';
  return entry.error ?? '失败';
}

export function clipboardHistoryPreview(item: ClipboardHistoryItem): string {
  if (item.text_preview) return item.text_preview;
  return item.has_image ? 'PNG 图片' : '空内容';
}

export function clipboardHistoryKind(item: ClipboardHistoryItem): string {
  if (item.has_text && item.has_image) return '文字 + 图片';
  if (item.has_image) return 'PNG 图片';
  return '文字';
}

export function clipboardHistoryRetentionLabel(settings: ClipboardHistorySettings): string {
  return settings.mode === 'count' ? '最近 ' + settings.count + ' 条' : '最近 ' + settings.days + ' 天';
}

export function platformLabel(platform: string | undefined): string {
  if (!platform) return '—';
  if (platform === 'windows') return 'Windows';
  if (platform === 'macos') return 'macOS';
  if (platform === 'linux') return 'Linux';
  if (platform === 'ios' || platform === 'android') return '手机';
  return platform;
}

export type ClientPlatform = 'windows' | 'macos' | 'linux' | 'other';

const CLIENT_PLATFORM_NAMES: Record<ClientPlatform, string> = {
  windows: 'Windows',
  macos: 'macOS',
  linux: 'Linux',
  other: '其他系统',
};

export function clientPlatform(userAgent: string): ClientPlatform {
  if (/Windows/.test(userAgent)) return 'windows';
  if (/Macintosh|Mac OS X/.test(userAgent)) return 'macos';
  if (/Linux/.test(userAgent)) return 'linux';
  return 'other';
}

export function clientPlatformName(platform: ClientPlatform): string {
  return CLIENT_PLATFORM_NAMES[platform];
}

export function webSessionLabel(userAgent: string): string {
  if (/iPhone|iPod/.test(userAgent)) return 'iPhone 浏览器';
  if (/iPad/.test(userAgent)) return 'iPad 浏览器';
  if (/Android/.test(userAgent)) return 'Android 浏览器';
  const platform = clientPlatform(userAgent);
  return platform === 'other' ? '网页设备' : clientPlatformName(platform) + ' 浏览器';
}

export function updateFrequencyLabel(value: UpdateFrequency): string {
  return value === 'daily' ? '每天' : value === 'weekly' ? '每周' : '关闭';
}

export function updateStatusLabel(value: UpdateStatus | null): string {
  if (!value) return '更新状态读取中';
  if (value.state === 'unavailable') return '开发构建不执行自动更新';
  if (value.state === 'disabled') return '自动更新已关闭';
  if (value.state === 'idle') return '等待下次检查';
  if (value.state === 'checking') return '正在检查 GitHub Release…';
  if (value.state === 'available') return '发现 v' + (value.available_version ?? '新版本');
  if (value.state === 'deferred') return '发现 v' + (value.available_version ?? '新版本') + '，等待传输完成';
  if (value.state === 'downloading') {
    if (!value.content_length) return '正在下载更新…';
    return '正在下载更新 ' + formatSize(value.downloaded_bytes) + ' / ' + formatSize(value.content_length);
  }
  if (value.state === 'installing') return value.error ?? '正在安装更新，应用即将重启';
  if (value.state === 'up_to_date') return '已是最新版本';
  if (value.state === 'needs_authorization') {
    return '待安装 v' + (value.available_version ?? '新版本') + '：' + (value.error ?? '需要在本机确认系统授权');
  }
  if (value.state === 'error') return value.error ? '更新失败：' + value.error : '更新失败';
  return value.error ?? '更新状态：' + value.state;
}
