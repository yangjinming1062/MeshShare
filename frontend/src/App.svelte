<script lang="ts">
  import { onMount } from 'svelte';
  import QRCode from 'qrcode';
  import { open } from '@tauri-apps/plugin-dialog';
  import { invoke, isTauri } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { apiUrl, clearClipboardHistory, completeMobileItem, createMobileSession, getClipboard, getClipboardHistory, getClipboardHistoryDetail, getClipboardStatus, getInfo, getMetrics, getMobileSessions, getMobileTransfers, getPeers, getSettings, getTransfers, initApi, mobileDownloadUrl, patchSettings, request, requestUpdateCheck, restoreClipboardHistory, setApiTarget, websocketUrl } from './api';
  import { Sha256, digestFile } from './sha256';
  import type { AddressInfo, ClipboardHistoryDetail, ClipboardHistoryItem, ClipboardHistoryMode, ClipboardHistorySettings, ClipboardSnapshot, ClipboardStatus, HardwareMetrics, MobileSession, MobileTransferView, MonitorCapabilities, NodeInfo, NodeSettings, PeerInfo, PhoneTransfer, ReceiveMode, ReceiveSettings, TransferFailure, TransferItem, TransferOffer, TransferProgress, UpdateFrequency, UpdateStatus, WireMessage } from './types';
  import { formatPercent, formatRate, gpuMemoryPercent, gpuUtilization, isActiveTransferStatus, isMetricsStale, memoryPercent, updateFrequencyLabel, webSessionLabel } from './format';
  import AppHeader from './components/AppHeader.svelte';
  import DevicePanel from './components/DevicePanel.svelte';
  import HardwarePanel from './components/HardwarePanel.svelte';
  import ClipboardPanel from './components/ClipboardPanel.svelte';
  import TransferPanel from './components/TransferPanel.svelte';
  import MobileApp from './components/MobileApp.svelte';
  import SettingsDialog from './components/SettingsDialog.svelte';
  import MobileAccessDialog from './components/MobileAccessDialog.svelte';
  import HistoryDetailDialog from './components/HistoryDetailDialog.svelte';
  import MetricsDetailDialog from './components/MetricsDetailDialog.svelte';
  import ConfirmDialog from './components/ConfirmDialog.svelte';

  const isMonitor = new URLSearchParams(window.location.search).get('window') === 'monitor';
  if (isMonitor) document.documentElement.dataset.window = 'monitor';
  const inTauri = isTauri();
  const isAndroidApp = inTauri && /Android/i.test(navigator.userAgent);
  // 浏览器访问（根地址或 /mobile）一律渲染网页临时加入界面，Tauri 窗口按窗口类型区分
  const isWeb = !inTauri && !isMonitor;
  const isDesktopWindow = inTauri && !isMonitor && !isAndroidApp;
  const isPhone = isWeb || isAndroidApp;
  document.documentElement.dataset.shell = isMonitor ? 'monitor' : isPhone ? (isWeb ? 'web' : 'android') : 'desktop';
  let node: NodeInfo | null = null;
  let activeBaseUrl = '';
  let remoteTarget: PeerInfo | null = null;
  let targetHistory: Array<{ baseUrl: string; node: NodeInfo | null; peer: PeerInfo | null }> = [];
  let peers: PeerInfo[] = [];
  let clipboard: ClipboardSnapshot | null = null;
  let clipboardHistory: ClipboardHistoryItem[] = [];
  let clipboardHistorySettings: ClipboardHistorySettings = { mode: 'count', count: 50, days: 30 };
  let selectedClipboardHistory: ClipboardHistoryDetail | null = null;
  let clipboardHistoryBusy = false;
  let clipboardHistorySettingsBusy = false;
  let clipboardStatus: ClipboardStatus = { session: 'unknown', available: false, retrying: true, message: '正在连接系统剪贴板…' };
  let metrics: HardwareMetrics | null = null;
  let selectedMetricsPeer: PeerInfo | null = null;
  let clipboardDraft = '';
  let status = '正在连接…';
  let statusKind: 'ok' | 'warn' | 'error' = 'warn';
  let mobileAddresses: AddressInfo[] = [];
  let selectedAddress = '';
  let mobileQr: { url: string; image: string } | null = null;
  let syncEnabled = true;
  let autostartEnabled = true;
  let autostartBusy = false;
  let updateFrequency: UpdateFrequency = 'weekly';
  let updateStatus: UpdateStatus | null = null;
  let monitorCapabilities: MonitorCapabilities = { backend: 'unknown', can_persist_position: false, can_click_through: false, can_keep_above: false };
  let updateBusy = false;
  let receiveSettings: ReceiveSettings = { mode: 'confirm' };
  let receiveSettingsBusy = false;
  let ws: WebSocket | null = null;
  let fileInput: HTMLInputElement;
  let folderInput: HTMLInputElement;
  let selectedPeer: PeerInfo | null = null;
  let selectedMobileSession: MobileSession | null = null;
  let dragPeer = '';
  let notice = '';
  let transfers: TransferProgress[] = [];
  let failures: TransferFailure[] = [];
  let mobileSessions: MobileSession[] = [];
  let mobileTransfers: MobileTransferView[] = [];
  let phoneFailures: TransferFailure[] = [];
  let mobileSessionId = '';
  let transferActionBusy = '';
  let manualAddress = '';
  let savedNodeUrls: string[] = [];
  let monitorLocked = false;
  let monitorVisible = true;
  let settingsOpen = false;
  let mobileAccessOpen = false;
  let addDeviceOpen = false;
  let clipboardTab: 'current' | 'history' = 'current';
  let historyDetailOpen = false;
  let metricsPeerOpen = false;
  let clearHistoryOpen = false;
  const pendingUploads = new Map<string, { target: string; files: File[]; items: TransferItem[]; offer: TransferOffer; relay?: boolean }>();
  let remoteMode = false;
  let remotePath = '';
  const uploading = new Set<string>();
  const watching = new Set<string>();
  const pendingMobileUploads = new Map<string, { files: File[]; items: TransferItem[]; completedItemIds: Set<string>; sessionId: string; offer: TransferOffer }>();
  const mobileUploading = new Set<string>();
  const mobileWatching = new Set<string>();
  const mobileUploadAttempts = new Map<string, number>();
  const mobileUploadStopped = new Set<string>();
  const uploadAttempts = new Map<string, number>();
  const uploadStopped = new Set<string>();
  const mobileDownloading = new Set<string>();
  let mobileBrowserDownloadStarted = new Set<string>();
  let mobileBrowserDownloadConfirming = new Set<string>();
  const MOBILE_UPLOAD_MAX_ATTEMPTS = 6;
  const DESKTOP_UPLOAD_MAX_ATTEMPTS = 6;
  const FILE_UPLOAD_ATTEMPTS = 4;
  const REMOTE_MANAGEMENT_CAPABILITY = 'management.remote.v1';
  const TRANSFER_RELAY_CAPABILITY = 'transfer.relay.v1';
  const WS_RECONNECT_BASE_MS = 1500;
  const WS_RECONNECT_MAX_MS = 30000;
  let mobileReceiveDirectory: MobileDirectoryHandle | null = null;
  let mobileReceiveDirectoryName = '';
  let pageStopped = false;
  let wsReconnectTimer: number | undefined;
  let wsReconnectAttempt = 0;
  let wsGeneration = 0;
  let wsPingTimer: number | undefined;
  let targetGeneration = 0;
  let refreshRequestId = 0;
  const FAILURE_RETENTION_MS = 24 * 60 * 60 * 1000;
  const MAX_FAILURES = 20;
  const dismissedFailures = new Map<string, number>();

  $: mobileAddress = mobileAddresses.find((address) => address.url === selectedAddress);
  $: mobileUrl = mobileAddress ? mobileAddress.url : '';
  $: secureMobileUrl = node?.mobile_addresses?.find((address) => address.address === mobileAddress?.address)?.url;
  $: phoneTransfers = isWeb
    ? mobileTransfers.map((transfer): PhoneTransfer => ({ ...transfer.progress, direction: 'receive', downloadedItemIds: transfer.downloaded_item_ids }))
    : transfers.map((transfer): PhoneTransfer => ({
        ...transfer,
        peerName: transfer.direction === 'send'
          ? peers.find((peer) => peer.node_id === transfer.offer?.target_node_id)?.node_name
          : undefined,
      }));
  $: phoneFailures = failures;
  $: monitorMetrics = isMetricsStale(metrics) ? null : metrics;
  $: remoteMode = !!remoteTarget;
  $: remotePath = remoteMode
    ? [...targetHistory.map((entry) => entry.node?.node_name ?? '本机'), node?.node_name ?? '目标节点'].join(' / ')
    : '';
  $: if (!isWeb && !isMonitor) void refreshQr(mobileUrl);
  $: mobileTargets = node ? [{
    node_id: node.node_id,
    node_name: node.node_name,
    platform: node.platform,
    addresses: node.addresses,
    mobile_addresses: node.mobile_addresses,
    capabilities: node.capabilities,
    online: true,
    last_seen_ms: Date.now(),
    endpoint: undefined,
    source: 'local',
    metrics: undefined,
    metrics_received_at_ms: undefined,
    metrics_stale: false,
  } satisfies PeerInfo, ...peers] : peers;

  function failureRetryable(progress: TransferProgress): boolean {
    if (progress.direction !== 'send') return false;
    return pendingUploads.has(progress.transfer_id) || pendingMobileUploads.has(progress.transfer_id);
  }

  function forgetPendingTransfer(transferId: string) {
    pendingUploads.delete(transferId);
    pendingMobileUploads.delete(transferId);
    uploadAttempts.delete(transferId);
    uploadStopped.delete(transferId);
    mobileUploadAttempts.delete(transferId);
    mobileUploadStopped.delete(transferId);
  }

  function pruneFailures() {
    const now = Date.now();
    for (const [transferId, dismissedAt] of dismissedFailures) {
      if (now - dismissedAt > FAILURE_RETENTION_MS) dismissedFailures.delete(transferId);
    }
    const next = failures
      .filter((failure) => now - failure.failed_at_ms <= FAILURE_RETENTION_MS)
      .sort((left, right) => right.failed_at_ms - left.failed_at_ms)
      .slice(0, MAX_FAILURES);
    const kept = new Set(next.map((failure) => failure.progress.transfer_id));
    for (const failure of failures) {
      if (!kept.has(failure.progress.transfer_id)) forgetPendingTransfer(failure.progress.transfer_id);
    }
    failures = next.map((failure) => ({ ...failure, retryable: failureRetryable(failure.progress) }));
  }

  function observeFailure(progress: TransferProgress) {
    if (progress.status !== 'failed') return;
    if (dismissedFailures.has(progress.transfer_id)) return;
    const existing = failures.find((failure) => failure.progress.transfer_id === progress.transfer_id);
    failures = [
      {
        progress,
        failed_at_ms: existing?.failed_at_ms ?? Date.now(),
        retryable: failureRetryable(progress),
      },
      ...failures.filter((failure) => failure.progress.transfer_id !== progress.transfer_id),
    ];
    pruneFailures();
  }

  function dismissFailure(failure: TransferFailure) {
    failures = failures.filter((item) => item.progress.transfer_id !== failure.progress.transfer_id);
    dismissedFailures.set(failure.progress.transfer_id, Date.now());
    forgetPendingTransfer(failure.progress.transfer_id);
  }

  function showTransientNotice(message: string, timeout = 2400) {
    notice = message;
    window.setTimeout(() => {
      if (notice === message) notice = '';
    }, timeout);
  }

  onMount(() => {
    pageStopped = false;
    try {
      const stored = window.localStorage.getItem('meshshare-saved-node-urls');
      const parsed = stored ? JSON.parse(stored) : [];
      if (Array.isArray(parsed)) {
        savedNodeUrls = parsed
          .filter((value): value is string => typeof value === 'string')
          .slice(0, 8);
      }
    } catch {
      savedNodeUrls = [];
    }
    folderInput?.setAttribute('webkitdirectory', '');
    folderInput?.setAttribute('directory', '');
    let timer: number | undefined;
    let stopped = false;
    let monitorPositionTimer: number | undefined;
    let pendingMonitorPosition: { x: number; y: number } | null = null;
    let monitorPositionWriteInFlight = false;
    let monitorPositionUnlisten: (() => void) | undefined;
    let updateStatusUnlisten: (() => void) | undefined;
    if (isDesktopWindow) {
      void listen<UpdateStatus>('update-status', ({ payload }) => {
        if (!remoteMode) updateStatus = payload;
      }).then((unlisten) => {
        if (stopped) unlisten();
        else updateStatusUnlisten = unlisten;
      }).catch((error) => console.error('监听更新状态失败', error));
    }
    const flushMonitorPosition = async () => {
      if (!pendingMonitorPosition || monitorPositionWriteInFlight) return;
      const position = pendingMonitorPosition;
      monitorPositionWriteInFlight = true;
      try {
        await invoke('set_monitor_position', position);
        if (pendingMonitorPosition?.x === position.x && pendingMonitorPosition?.y === position.y) {
          pendingMonitorPosition = null;
        }
      } catch (error) {
        console.error('保存悬浮监控条位置失败', error);
      } finally {
        monitorPositionWriteInFlight = false;
        if (!stopped && pendingMonitorPosition && (pendingMonitorPosition.x !== position.x || pendingMonitorPosition.y !== position.y)) {
          void flushMonitorPosition();
        }
      }
    };
    if (inTauri && isMonitor) {
      void getCurrentWindow().onMoved(({ payload }) => {
        if (!monitorCapabilities.can_persist_position) return;
        pendingMonitorPosition = { x: payload.x, y: payload.y };
        if (monitorPositionTimer) window.clearTimeout(monitorPositionTimer);
        monitorPositionTimer = window.setTimeout(() => {
          monitorPositionTimer = undefined;
          void flushMonitorPosition();
        }, 200);
      }).then((unlisten) => {
        if (stopped) unlisten();
        else monitorPositionUnlisten = unlisten;
      }).catch((error) => console.error('监听悬浮监控条移动失败', error));
    }
    const start = async () => {
      if (pageStopped) return;
      try {
        const base = await initApi();
        if (!activeBaseUrl) activeBaseUrl = base;
        if (isDesktopWindow) {
          monitorCapabilities = await invoke<MonitorCapabilities>('monitor_capabilities').catch(() => monitorCapabilities);
          [monitorLocked, monitorVisible] = await Promise.all([
            invoke<boolean>('monitor_locked').catch(() => monitorLocked),
            invoke<boolean>('monitor_visible').catch(() => monitorVisible),
          ]);
        }
        if (isDesktopWindow) {
          updateFrequency = await invoke<UpdateFrequency>('update_frequency').catch(() => 'weekly');
          updateStatus = await invoke<UpdateStatus>('update_status').catch(() => null);
          clipboardHistorySettings = await invoke<ClipboardHistorySettings>('clipboard_history_settings').catch(() => clipboardHistorySettings);
          receiveSettings = await invoke<ReceiveSettings>('receive_settings').catch(() => receiveSettings);
          clipboardStatus = await invoke<ClipboardStatus>('clipboard_status').catch(() => clipboardStatus);
        }
        await refresh();
        if (isDesktopWindow) {
          syncEnabled = await invoke<boolean>('clipboard_sync_enabled').catch(() => true);
          autostartEnabled = await invoke<boolean>('autostart_enabled').catch(() => true);
        }
        connectWebSocket();
        timer = window.setInterval(() => {
          void refresh().catch((error) => {
            if (pageStopped) return;
            status = error instanceof Error ? error.message : '无法刷新节点状态';
            statusKind = 'error';
          });
        }, 2500);
      } catch (error) {
        if (pageStopped) return;
        status = error instanceof Error ? error.message : '无法连接节点';
        statusKind = 'error';
        timer = window.setTimeout(start, 3000);
      }
    };
    void start();
    return () => {
      stopped = true;
      pageStopped = true;
      if (timer) window.clearInterval(timer);
      if (wsReconnectTimer) window.clearTimeout(wsReconnectTimer);
      wsReconnectTimer = undefined;
      if (wsPingTimer) window.clearInterval(wsPingTimer);
      wsPingTimer = undefined;
      wsGeneration += 1;
      ws?.close();
      ws = null;
      if (monitorPositionTimer) window.clearTimeout(monitorPositionTimer);
      monitorPositionUnlisten?.();
      updateStatusUnlisten?.();
      void flushMonitorPosition();
      if (stopped) window.clearTimeout(timer);
    };
  });

  async function refresh() {
    const requestId = ++refreshRequestId;
    const generation = targetGeneration;
    const target = activeBaseUrl;
    if (isDesktopWindow && !remoteMode) {
      [monitorLocked, monitorVisible] = await Promise.all([
        invoke<boolean>('monitor_locked').catch(() => monitorLocked),
        invoke<boolean>('monitor_visible').catch(() => monitorVisible),
      ]);
    }
    if (isMonitor) {
      const [nextNode, nextMetrics] = await Promise.all([getInfo(), getMetrics()]);
      if (generation !== targetGeneration || target !== activeBaseUrl) return;
      node = nextNode;
      metrics = nextMetrics;
      status = '节点在线';
      statusKind = 'ok';
      return;
    }
    if (isWeb && !mobileSessionId) await ensureMobileSession();
    const [nextNode, nextPeers, nextClipboard, nextMetrics, nextHistory, nextClipboardStatus] = await Promise.all([
      getInfo(),
      getPeers(),
      getClipboard(),
      getMetrics(),
      getClipboardHistory(),
      getClipboardStatus().catch(() => remoteMode
        ? clipboardStatus
        : isDesktopWindow
          ? invoke<ClipboardStatus>('clipboard_status').catch(() => clipboardStatus)
          : clipboardStatus),
    ]);
    const nextTransfers = isWeb ? [] : await getTransfers();
    if (isWeb) {
      mobileTransfers = await getMobileTransfersWithRecovery();
      for (const view of mobileTransfers) {
        if (view.progress.status === 'ready' && mobileReceiveDirectory && !mobileDownloading.has(view.progress.transfer_id)) {
          void downloadMobileTransfer(view);
        }
      }
    } else {
      mobileSessions = await getMobileSessions();
      mobileTransfers = await getMobileTransfers();
      nextTransfers.push(...mobileTransfers.map((view) => ({ ...view.progress, direction: 'send' as const })));
      for (const view of mobileTransfers) {
        if (!['ready', 'completed', 'rejected', 'failed', 'cancelled'].includes(view.progress.status) && !mobileWatching.has(view.progress.transfer_id)) {
          void watchMobileTransfer(view.progress.transfer_id);
        }
      }
    }
    if (requestId !== refreshRequestId || generation !== targetGeneration || target !== activeBaseUrl) return;
    pruneFailures();
    node = nextNode;
    peers = nextPeers.filter((peer) => peer.online);
    clipboard = nextClipboard;
    clipboardStatus = nextClipboardStatus;
    clipboardHistory = nextHistory.items;
    if (isWeb || isMonitor) clipboardHistorySettings = nextHistory.settings;
    if (selectedClipboardHistory && !clipboardHistory.some((item) => item.event_id === selectedClipboardHistory?.item.event_id)) {
      selectedClipboardHistory = null;
    }
    metrics = nextMetrics;
    const refreshedTransfers = nextTransfers.map((next) => {
      const current = transfers.find((item) => item.transfer_id === next.transfer_id);
      if (current && shouldKeepTransferProgress(current, next)) {
        return { ...next, ...current, offer: next.offer ?? current.offer };
      }
      return { ...current, ...next, offer: next.offer ?? current?.offer };
    });
    transfers = [
      ...refreshedTransfers,
      ...transfers.filter((item) => isActiveTransferStatus(item.status) && !nextTransfers.some((next) => next.transfer_id === item.transfer_id)),
    ];
    for (const transfer of refreshedTransfers) observeFailure(transfer);
    if (isWeb) {
      for (const view of mobileTransfers) {
        observeFailure({ ...view.progress, direction: 'receive' });
      }
    }
    for (const transfer of nextTransfers) {
      if (mobileTransfers.some((view) => view.progress.transfer_id === transfer.transfer_id)) continue;
      if (transfer.direction !== 'send' || !transfer.offer || ['completed', 'rejected', 'failed', 'cancelled'].includes(transfer.status) || watching.has(transfer.transfer_id)) continue;
      const relayTransfer = remoteMode && transfer.direction === 'send';
      const targetPeer = peers.find((peer) => peer.node_id === transfer.offer?.target_node_id);
      const target = relayTransfer ? activeBaseUrl : (targetPeer ? targetBase(targetPeer) : null);
      if (target) void watchTransfer(transfer.transfer_id, target, relayTransfer);
    }
    if (selectedMetricsPeer) {
      selectedMetricsPeer = peers.find((peer) => peer.node_id === selectedMetricsPeer?.node_id) ?? null;
      if (!selectedMetricsPeer) metricsPeerOpen = false;
    }
    if (clipboard && document.activeElement?.id !== 'clipboard-editor') clipboardDraft = clipboard.text ?? '';
    mobileAddresses = nextNode.addresses.filter((address) => !address.address.startsWith('127.'));
    if (!mobileAddresses.some((address) => address.url === selectedAddress)) {
      selectedAddress = mobileAddresses[0]?.url ?? '';
    }
    status = '节点在线';
    statusKind = 'ok';
    if (remoteMode) await refreshRemoteSettings(generation, target);
  }

  async function refreshRemoteSettings(generation = targetGeneration, target = activeBaseUrl) {
    if (!remoteMode) return;
    try {
      const settings = await getSettings();
      if (generation !== targetGeneration || target !== activeBaseUrl) return;
      applyRemoteSettings(settings);
    } catch (error) {
      if (error instanceof Error && !error.message.includes('404')) {
        status = error.message;
        statusKind = 'warn';
      }
    }
  }

  function applyRemoteSettings(settings: NodeSettings) {
    syncEnabled = settings.clipboard_sync_enabled;
    autostartEnabled = settings.autostart_enabled;
    updateFrequency = settings.update_frequency;
    updateStatus = settings.update_status;
    receiveSettings = settings.receive_settings;
    clipboardHistorySettings = settings.clipboard_history_settings;
    clipboardStatus = settings.clipboard_status;
    monitorLocked = settings.monitor.locked;
    monitorVisible = settings.monitor.visible;
    monitorCapabilities = settings.monitor.capabilities;
  }

  async function refreshLocalSettings() {
    if (!isDesktopWindow || remoteMode) return;
    const [nextFrequency, nextStatus, nextHistory, nextReceive, nextClipboard, nextSync, nextAutostart] = await Promise.all([
      invoke<UpdateFrequency>('update_frequency').catch(() => updateFrequency),
      invoke<UpdateStatus>('update_status').catch(() => updateStatus),
      invoke<ClipboardHistorySettings>('clipboard_history_settings').catch(() => clipboardHistorySettings),
      invoke<ReceiveSettings>('receive_settings').catch(() => receiveSettings),
      invoke<ClipboardStatus>('clipboard_status').catch(() => clipboardStatus),
      invoke<boolean>('clipboard_sync_enabled').catch(() => syncEnabled),
      invoke<boolean>('autostart_enabled').catch(() => true),
    ]);
    updateFrequency = nextFrequency;
    updateStatus = nextStatus;
    clipboardHistorySettings = nextHistory;
    receiveSettings = nextReceive;
    clipboardStatus = nextClipboard;
    syncEnabled = nextSync;
    autostartEnabled = nextAutostart;
  }

  async function ensureMobileSession() {
    if (!isWeb || mobileSessionId) return;
    const stored = window.sessionStorage.getItem('meshshare-mobile-session');
    if (stored) {
      try {
        await getMobileTransfers(stored);
        mobileSessionId = stored;
        return;
      } catch {
        window.sessionStorage.removeItem('meshshare-mobile-session');
      }
    }
    const session = await createMobileSession(webSessionLabel(navigator.userAgent));
    mobileSessionId = session.session_id;
    window.sessionStorage.setItem('meshshare-mobile-session', mobileSessionId);
  }

  function connectWebSocket() {
    if (pageStopped) return;
    if (wsReconnectTimer) {
      window.clearTimeout(wsReconnectTimer);
      wsReconnectTimer = undefined;
    }
    if (wsPingTimer) {
      window.clearInterval(wsPingTimer);
      wsPingTimer = undefined;
    }
    const socket = new WebSocket(websocketUrl());
    const generation = ++wsGeneration;
    ws?.close();
    ws = socket;
    socket.onopen = () => {
      if (generation !== wsGeneration || socket !== ws) return;
      wsReconnectAttempt = 0;
      status = '已连接';
      statusKind = 'ok';
      const sendPing = () => {
        if (generation !== wsGeneration || socket !== ws || socket.readyState !== WebSocket.OPEN) return;
        socket.send(JSON.stringify({ type: 'Ping', payload: { at_ms: Date.now() } }));
      };
      sendPing();
      if (wsPingTimer) window.clearInterval(wsPingTimer);
      wsPingTimer = window.setInterval(sendPing, 15000);
    };
    socket.onclose = () => {
      if (generation !== wsGeneration || socket !== ws || pageStopped) return;
      ws = null;
      if (wsPingTimer) window.clearInterval(wsPingTimer);
      wsPingTimer = undefined;
      status = '连接暂时中断，正在恢复…';
      statusKind = 'warn';
      scheduleWebSocketReconnect();
    };
    socket.onerror = () => {
      if (generation !== wsGeneration || socket !== ws || pageStopped) return;
      // onerror is followed by onclose in browsers. Keep one user-facing
      // recovery state instead of starting another connection here.
      status = '同步连接暂时不可用，正在恢复…';
      statusKind = 'warn';
    };
    socket.onmessage = (event) => {
      if (generation !== wsGeneration || socket !== ws) return;
      try {
        const message = JSON.parse(event.data) as WireMessage;
        handleWireMessage(message);
      } catch {
        // A malformed event must not break the live UI.
      }
    };
  }

  function scheduleWebSocketReconnect() {
    if (pageStopped || wsReconnectTimer) return;
    const exponential = Math.min(WS_RECONNECT_MAX_MS, WS_RECONNECT_BASE_MS * 2 ** wsReconnectAttempt);
    const jitter = Math.round(exponential * (0.1 * Math.random()));
    const delay = Math.min(WS_RECONNECT_MAX_MS, exponential + jitter);
    wsReconnectAttempt = Math.min(wsReconnectAttempt + 1, 8);
    wsReconnectTimer = window.setTimeout(() => {
      wsReconnectTimer = undefined;
      if (!pageStopped && !ws) connectWebSocket();
    }, delay);
  }

  function handleWireMessage(message: WireMessage) {
    const { type, payload } = message;
    if (type === 'Ping') {
      if (ws?.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: 'Pong', payload: { at_ms: payload.at_ms } }));
      }
      return;
    }
    if (type === 'Hello') {
      void refresh();
      return;
    }
    if (type === 'ClipboardUpdate') {
      void refresh();
      return;
    }
    if (type === 'ClipboardSnapshot') {
      void refresh();
      return;
    }
    if (type === 'MetricsUpdate') {
      const nextMetrics = payload.metrics;
      if (nextMetrics.node_id === node?.node_id) {
        metrics = nextMetrics;
      } else {
        peers = peers.map((peer) => peer.node_id === nextMetrics.node_id
          ? { ...peer, metrics: nextMetrics, metrics_received_at_ms: Date.now(), metrics_stale: false }
          : peer);
        if (selectedMetricsPeer?.node_id === nextMetrics.node_id) {
          selectedMetricsPeer = { ...selectedMetricsPeer, metrics: nextMetrics, metrics_received_at_ms: Date.now(), metrics_stale: false };
        }
      }
      return;
    }
    if (type === 'TransferOffer') {
      const offer = payload.offer;
      const existing = transfers.find((item) => item.transfer_id === offer.transfer_id);
      // An auto-accepted transfer can deliver its offer after the accepted
      // progress event during reconnect. Keep the newest known lifecycle
      // state instead of turning it back into a confirmation request.
      const next = existing
        ? {
            ...existing,
            offer: existing.offer ?? offer,
            total_bytes: existing.total_bytes || offer.total_bytes,
            total_items: existing.total_items || offer.items.length,
          }
        : {
            transfer_id: offer.transfer_id,
            status: 'waiting' as const,
            completed_bytes: 0,
            total_bytes: offer.total_bytes,
            completed_items: 0,
            total_items: offer.items.length,
            offer,
            direction: 'receive' as const,
          };
      transfers = [{
        ...next,
      }, ...transfers.filter((item) => item.transfer_id !== offer.transfer_id)];
      return;
    }
    if (type === 'TransferProgress') {
      const progress = payload.progress;
      const existing = transfers.find((item) => item.transfer_id === progress.transfer_id);
      const next = existing && shouldKeepTransferProgress(existing, progress)
        ? { ...progress, ...existing, offer: progress.offer ?? existing.offer }
        : { ...existing, ...progress, offer: progress.offer ?? existing?.offer };
      observeFailure(next);
      transfers = [next, ...transfers.filter((item) => item.transfer_id !== progress.transfer_id)];
    }
  }

  function transferStatusRank(status: TransferProgress['status']): number {
    if (status === 'waiting') return 0;
    if (status === 'accepted') return 1;
    if (status === 'uploading') return 2;
    if (status === 'ready') return 3;
    if (status === 'completed') return 5;
    return 4;
  }

  function shouldKeepTransferProgress(current: TransferProgress, next: TransferProgress): boolean {
    const currentRetry = current.retry_count ?? 0;
    const nextRetry = next.retry_count ?? 0;
    if (nextRetry !== currentRetry) return nextRetry < currentRetry;
    const currentRank = transferStatusRank(current.status);
    const nextRank = transferStatusRank(next.status);
    return nextRank < currentRank;
  }

  async function refreshQr(url: string): Promise<void> {
    if (!url) return;
    try {
      const image = await QRCode.toDataURL(url, { width: 192, margin: 1, color: { dark: '#111827', light: '#ffffff' } });
      if (url === mobileUrl) mobileQr = { url, image };
    } catch (error) {
      console.error('生成浏览器访问二维码失败', error);
      if (url === mobileUrl) mobileQr = { url, image: '' };
    }
  }

  async function getMobileTransfersWithRecovery(): Promise<MobileTransferView[]> {
    try {
      return await getMobileTransfers(mobileSessionId);
    } catch (error) {
      const message = error instanceof Error ? error.message : '';
      if (!message.includes('404') && !message.includes('过期')) throw error;
      mobileSessionId = '';
      window.sessionStorage.removeItem('meshshare-mobile-session');
      await ensureMobileSession();
      return await getMobileTransfers(mobileSessionId);
    }
  }

  async function toggleSync() {
    if (remoteMode) {
      try {
        const settings = await patchSettings({ clipboard_sync_enabled: !syncEnabled });
        applyRemoteSettings(settings);
        notice = syncEnabled ? '目标节点的剪贴板自动同步已恢复' : '目标节点的剪贴板自动同步已暂停';
      } catch (error) {
        notice = error instanceof Error ? error.message : '无法更新目标节点同步设置';
      }
      window.setTimeout(() => (notice = ''), 2400);
      return;
    }
    syncEnabled = await invoke<boolean>('toggle_clipboard_sync').catch(() => !syncEnabled);
    notice = syncEnabled ? '剪贴板自动同步已恢复' : '剪贴板自动同步已暂停';
    window.setTimeout(() => (notice = ''), 2200);
  }

  async function toggleAutostart() {
    if (autostartBusy) return;
    const next = !autostartEnabled;
    autostartBusy = true;
    try {
      if (remoteMode) {
        const settings = await patchSettings({ autostart_enabled: next });
        applyRemoteSettings(settings);
      } else {
        autostartEnabled = await invoke<boolean>('set_autostart_enabled', { enabled: next });
      }
      notice = autostartEnabled ? '开机自启动已开启' : '开机自启动已关闭';
    } catch (error) {
      notice = typeof error === 'string' ? error : error instanceof Error ? error.message : '无法更新开机自启动设置';
    } finally {
      autostartBusy = false;
    }
    window.setTimeout(() => (notice = ''), 2400);
  }

  async function setUpdateFrequency(next: UpdateFrequency) {
    const previous = updateFrequency;
    try {
      if (remoteMode) {
        const settings = await patchSettings({ update_frequency: next });
        applyRemoteSettings(settings);
      } else {
        updateFrequency = await invoke<UpdateFrequency>('set_update_frequency', { frequency: next });
        updateStatus = await invoke<UpdateStatus>('update_status').catch(() => updateStatus);
      }
      notice = next === 'disabled' ? '自动更新已关闭' : `自动更新已设为${updateFrequencyLabel(next)}`;
    } catch (error) {
      updateFrequency = previous;
      notice = error instanceof Error ? error.message : '无法更新自动更新设置';
    }
    window.setTimeout(() => (notice = ''), 2400);
  }

  async function setReceiveMode(mode: ReceiveMode) {
    if (mode === 'auto' && !receiveSettings.default_path) {
      await chooseReceiveDirectory();
      if (!receiveSettings.default_path) return;
    }
    await saveReceiveSettings(mode, receiveSettings.default_path);
  }

  async function chooseReceiveDirectory() {
    if (remoteMode) {
      const path = window.prompt('输入目标节点上的默认接收目录', receiveSettings.default_path ?? '');
      if (path === null) return;
      await saveReceiveSettings(receiveSettings.mode, path.trim() || null);
      return;
    }
    let destination: string | string[] | null = null;
    try {
      destination = await open({ directory: true, multiple: false, title: '选择默认接收目录' });
    } catch {
      destination = null;
    }
    const path = Array.isArray(destination) ? destination[0] : destination;
    if (!path) return;
    await saveReceiveSettings(receiveSettings.mode, path);
  }

  async function saveReceiveSettings(mode: ReceiveMode, defaultPath?: string | null) {
    if (receiveSettingsBusy) return;
    receiveSettingsBusy = true;
    try {
      if (remoteMode) {
        const settings = await patchSettings({ receive_settings: {
          mode,
          default_path: defaultPath ?? receiveSettings.default_path ?? null,
        } });
        applyRemoteSettings(settings);
      } else {
        receiveSettings = await invoke<ReceiveSettings>('set_receive_settings', {
          mode,
          defaultPath: defaultPath ?? receiveSettings.default_path ?? null,
        });
      }
      notice = mode === 'auto' ? '已启用自动接收' : '已设为每次确认';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法更新接收设置';
    } finally {
      receiveSettingsBusy = false;
    }
    window.setTimeout(() => (notice = ''), 2400);
  }

  async function setClipboardHistoryMode(mode: ClipboardHistoryMode) {
    await saveClipboardHistorySettings(mode, historyValue(mode));
  }

  async function setClipboardHistoryValue(value: number) {
    if (!Number.isFinite(value)) return;
    await saveClipboardHistorySettings(clipboardHistorySettings.mode, value);
  }

  function historyValue(mode: ClipboardHistoryMode) {
    return mode === 'count' ? clipboardHistorySettings.count : clipboardHistorySettings.days;
  }

  async function saveClipboardHistorySettings(mode: ClipboardHistoryMode, value: number) {
    if (clipboardHistorySettingsBusy) return;
    clipboardHistorySettingsBusy = true;
    try {
      if (remoteMode) {
        const settings = await patchSettings({ clipboard_history_settings: { mode, value } });
        applyRemoteSettings(settings);
      } else {
        clipboardHistorySettings = await invoke<ClipboardHistorySettings>('set_clipboard_history_settings', { mode, value });
      }
      await refreshClipboardHistory();
      notice = mode === 'count'
        ? `已保留最近 ${clipboardHistorySettings.count} 条剪贴板内容`
        : `已保留最近 ${clipboardHistorySettings.days} 天剪贴板内容`;
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法更新剪贴板历史设置';
    } finally {
      clipboardHistorySettingsBusy = false;
    }
    window.setTimeout(() => (notice = ''), 2400);
  }

  async function refreshClipboardHistory() {
    const response = await getClipboardHistory();
    clipboardHistorySettings = response.settings;
    clipboardHistory = response.items;
    if (selectedClipboardHistory && !clipboardHistory.some((item) => item.event_id === selectedClipboardHistory?.item.event_id)) {
      selectedClipboardHistory = null;
    }
  }

  async function checkForUpdates() {
    if (updateBusy) return;
    updateBusy = true;
    try {
      if (remoteMode) {
        applyRemoteSettings(await requestUpdateCheck());
      } else {
        await invoke('check_for_updates');
        updateStatus = await invoke<UpdateStatus>('update_status').catch(() => updateStatus);
      }
      notice = '已发起更新检查';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法检查更新';
    } finally {
      updateBusy = false;
    }
    window.setTimeout(() => (notice = ''), 2400);
  }

  function selectMetricsPeer(peer: PeerInfo) {
    selectedMetricsPeer = peer;
    metricsPeerOpen = true;
  }

  function closeHistoryDetail() {
    selectedClipboardHistory = null;
    historyDetailOpen = false;
  }

  function requestClearHistory() {
    clearHistoryOpen = true;
  }

  function cancelClearHistory() {
    clearHistoryOpen = false;
  }

  function confirmClearHistory() {
    void clearClipboardHistoryItems();
  }

  async function toggleMonitorLock() {
    try {
      if (remoteMode) {
        applyRemoteSettings(await patchSettings({ monitor: { locked: !monitorLocked } }));
      } else {
        monitorLocked = await invoke<boolean>('set_monitor_locked', { locked: !monitorLocked });
      }
      notice = monitorLocked ? '悬浮监控条已锁定，鼠标交互已忽略' : '悬浮监控条已解锁，可以拖动';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法更新悬浮监控条状态';
    }
    window.setTimeout(() => (notice = ''), 2400);
  }

  async function toggleMonitorVisibility() {
    try {
      if (remoteMode) {
        applyRemoteSettings(await patchSettings({ monitor: { visible: !monitorVisible } }));
      } else {
        monitorVisible = await invoke<boolean>('set_monitor_visible', { visible: !monitorVisible });
      }
      notice = monitorVisible ? '悬浮监控条已显示' : '悬浮监控条已隐藏';
    } catch (error) {
      notice = typeof error === 'string' ? error : error instanceof Error ? error.message : '无法更新悬浮监控条可见性';
    }
    window.setTimeout(() => (notice = ''), 2400);
  }

  async function openMonitorMenu(event: MouseEvent) {
    event.preventDefault();
    if (!inTauri || monitorLocked) return;
    try {
      await invoke('popup_monitor_menu');
    } catch (error) {
      console.warn('打开悬浮监控条菜单失败', error);
    }
  }

  async function publishClipboard() {
    if (!clipboardDraft.trim()) return;
    try {
      clipboard = await request<ClipboardSnapshot>('/api/v1/clipboard', {
        method: 'POST',
        body: JSON.stringify({ text: clipboardDraft, source_name: node?.node_name }),
      });
      notice = '已同步到在线设备';
    } catch (error) {
      notice = error instanceof Error ? error.message : '剪贴板同步失败';
    }
    window.setTimeout(() => (notice = ''), 2400);
  }

  async function openClipboardHistory(item: ClipboardHistoryItem) {
    try {
      selectedClipboardHistory = await getClipboardHistoryDetail(item.event_id);
      clipboardDraft = selectedClipboardHistory.snapshot.text ?? '';
      historyDetailOpen = true;
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法读取历史剪贴板内容';
      window.setTimeout(() => (notice = ''), 2400);
    }
  }

  async function restoreClipboardHistoryItem() {
    const eventId = selectedClipboardHistory?.item.event_id;
    if (!eventId || clipboardHistoryBusy) return;
    clipboardHistoryBusy = true;
    try {
      clipboard = await restoreClipboardHistory(eventId);
      clipboardDraft = clipboard.text ?? '';
      await refreshClipboardHistory();
      notice = '已恢复并同步剪贴板内容';
      closeHistoryDetail();
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法恢复剪贴板历史';
    } finally {
      clipboardHistoryBusy = false;
    }
    window.setTimeout(() => (notice = ''), 2400);
  }

  async function clearClipboardHistoryItems() {
    if (clipboardHistoryBusy || !clipboardHistory.length) return;
    clipboardHistoryBusy = true;
    try {
      await clearClipboardHistory();
      clipboardHistory = [];
      selectedClipboardHistory = null;
      clearHistoryOpen = false;
      notice = '已清空本机剪贴板历史';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法清空剪贴板历史';
    } finally {
      clipboardHistoryBusy = false;
    }
    window.setTimeout(() => (notice = ''), 2400);
  }

  async function connectManualPeer() {
    const url = manualAddress.trim().replace(/\/$/, '');
    if (!url) return;
    await connectSavedPeer(url);
  }

  async function connectSavedPeer(url: string) {
    try {
      await request('/api/v1/peers/connect', { method: 'POST', body: JSON.stringify({ url }) });
      const next = [url, ...savedNodeUrls.filter((item) => item !== url)].slice(0, 8);
      savedNodeUrls = next;
      try {
        window.localStorage.setItem('meshshare-saved-node-urls', JSON.stringify(next));
      } catch {
        // Persisting a convenience shortcut must not block the connection.
      }
      manualAddress = '';
      addDeviceOpen = false;
      await refresh();
      notice = '已发起节点连接';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法连接节点';
    }
    window.setTimeout(() => (notice = ''), 2400);
  }

  async function readMobileClipboard() {
    try {
      clipboardDraft = await navigator.clipboard.readText();
      notice = '已读取本机剪贴板';
    } catch {
      notice = '浏览器未授权读取剪贴板，请手动粘贴';
    }
  }

  async function writeMobileClipboard() {
    try {
      await navigator.clipboard.writeText(clipboardDraft);
      notice = '已写入本机剪贴板';
    } catch {
      notice = '浏览器拒绝写入，请手动选择并复制';
    }
  }

  function startChoose(peer: PeerInfo, folder = false) {
    selectedPeer = peer;
    selectedMobileSession = null;
    if (folder) folderInput?.click();
    else fileInput?.click();
  }

  function startChooseMobile(session: MobileSession, folder = false) {
    selectedMobileSession = session;
    selectedPeer = null;
    if (folder) folderInput?.click();
    else fileInput?.click();
  }

  async function onFileInput(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    if (input.files && selectedMobileSession) await sendFilesToMobile(selectedMobileSession, Array.from(input.files));
    else if (input.files && selectedPeer) await sendFiles(selectedPeer, Array.from(input.files));
    input.value = '';
  }

  async function onDrop(peer: PeerInfo, event: DragEvent) {
    event.preventDefault();
    dragPeer = '';
    const files = event.dataTransfer ? await filesFromDataTransfer(event.dataTransfer) : [];
    if (files.length) await sendFiles(peer, files);
  }

  async function onDropMobile(session: MobileSession, event: DragEvent) {
    event.preventDefault();
    dragPeer = '';
    const files = event.dataTransfer ? await filesFromDataTransfer(event.dataTransfer) : [];
    if (files.length) await sendFilesToMobile(session, files);
  }

  type DropEntry = FileSystemEntry & { isFile: boolean; isDirectory: boolean; file?: (callback: (file: File) => void) => void; createReader?: () => { readEntries: (callback: (entries: DropEntry[]) => void) => void } };

  async function filesFromDataTransfer(data: DataTransfer): Promise<File[]> {
    const files: File[] = [];
    const items = Array.from(data.items);
    for (const item of items) {
      const entry = (item as DataTransferItem & { webkitGetAsEntry?: () => DropEntry | null }).webkitGetAsEntry?.();
      if (entry) await readDropEntry(entry, '', files);
    }
    return files.length ? files : Array.from(data.files);
  }

  async function readDropEntry(entry: DropEntry, prefix: string, files: File[]): Promise<void> {
    if (entry.isFile && entry.file) {
      const file = await new Promise<File>((resolve) => entry.file!(resolve));
      const relativePath = `${prefix}${file.name}`;
      Object.defineProperty(file, 'webkitRelativePath', { value: relativePath, configurable: true });
      files.push(file);
      return;
    }
    if (!entry.isDirectory || !entry.createReader) return;
    const reader = entry.createReader();
    const entries: DropEntry[] = [];
    while (true) {
      const batch = await new Promise<DropEntry[]>((resolve) => reader.readEntries(resolve));
      if (!batch.length) break;
      entries.push(...batch);
    }
    for (const child of entries) await readDropEntry(child, `${prefix}${entry.name}/`, files);
  }

  function targetBase(peer: PeerInfo): string | null {
    if (isWeb) {
      if (peer.node_id === node?.node_id) return window.location.origin;
      if (window.location.protocol === 'https:') return `${window.location.origin}/api/v1/mobile/peers/${encodeURIComponent(peer.node_id)}`;
    }
    const url = peer.addresses.find((address) => address.kind === 'lan')?.url ?? peer.addresses[0]?.url;
    return url ?? (peer.endpoint ? peer.endpoint.replace(/\/api\/v1\/ws$/, '') : null);
  }

  function resetTargetUi() {
    targetGeneration += 1;
    selectedClipboardHistory = null;
    historyDetailOpen = false;
    metricsPeerOpen = false;
    selectedMetricsPeer = null;
    selectedPeer = null;
    selectedMobileSession = null;
    clipboardHistory = [];
    transfers = [];
    for (const failure of failures) forgetPendingTransfer(failure.progress.transfer_id);
    failures = [];
    dismissedFailures.clear();
    mobileSessions = [];
    mobileTransfers = [];
    mobileBrowserDownloadStarted = new Set();
    mobileBrowserDownloadConfirming = new Set();
    mobileAddresses = [];
    selectedAddress = '';
    mobileQr = null;
  }

  async function openRemotePeer(peer: PeerInfo) {
    if (isWeb || !peer.online) return;
    if (!peer.capabilities.includes(REMOTE_MANAGEMENT_CAPABILITY)) {
      notice = '目标节点不支持远程管理，请先升级目标节点';
      window.setTimeout(() => (notice = ''), 3000);
      return;
    }
    const target = targetBase(peer);
    if (!target) {
      notice = '目标设备没有可访问地址';
      window.setTimeout(() => (notice = ''), 2400);
      return;
    }
    wsGeneration += 1;
    ws?.close();
    ws = null;
    if (wsReconnectTimer) window.clearTimeout(wsReconnectTimer);
    wsReconnectTimer = undefined;
    if (wsPingTimer) window.clearInterval(wsPingTimer);
    wsPingTimer = undefined;
    targetHistory = [...targetHistory, { baseUrl: activeBaseUrl, node, peer: remoteTarget }];
    remoteTarget = peer;
    activeBaseUrl = target;
    setApiTarget(target);
    resetTargetUi();
    wsReconnectAttempt = 0;
    await refresh().catch((error) => {
      status = error instanceof Error ? error.message : '无法连接目标节点';
      statusKind = 'error';
    });
    connectWebSocket();
  }

  async function returnToPreviousTarget() {
    const previous = targetHistory.pop();
    if (!previous) return;
    wsGeneration += 1;
    ws?.close();
    ws = null;
    if (wsReconnectTimer) window.clearTimeout(wsReconnectTimer);
    wsReconnectTimer = undefined;
    if (wsPingTimer) window.clearInterval(wsPingTimer);
    wsPingTimer = undefined;
    targetHistory = targetHistory;
    remoteTarget = previous.peer;
    activeBaseUrl = previous.baseUrl;
    setApiTarget(activeBaseUrl);
    resetTargetUi();
    wsReconnectAttempt = 0;
    await refresh().catch((error) => {
      status = error instanceof Error ? error.message : '无法返回上一级节点';
      statusKind = 'error';
    });
    await refreshLocalSettings();
    connectWebSocket();
  }

  async function createItem(file: File, index: number): Promise<TransferItem> {
    const relativePath = (file as File & { webkitRelativePath?: string }).webkitRelativePath || file.name;
    const sha256 = await digestFile(file);
    return {
      item_id: `${index}-${randomId()}`,
      name: file.name,
      relative_path: relativePath,
      size: file.size,
      sha256,
      media_type: file.type || 'application/octet-stream',
      directory: false,
    };
  }

  function randomId() {
    if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
    return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
  }

  async function uploadFileWithRetry(url: string, file: File, mediaType: string): Promise<Response> {
    let lastError: unknown;
    for (let attempt = 0; attempt < FILE_UPLOAD_ATTEMPTS; attempt += 1) {
      let stopRetry = false;
      try {
        const response = await fetch(url, {
          method: 'PUT',
          headers: { 'Content-Type': mediaType },
          body: file,
        });
        if (response.ok) return response;
        const payload = await response.json().catch(() => ({}));
        const message = typeof payload === 'object' && payload !== null && 'error' in payload && typeof payload.error === 'string'
          ? payload.error
          : `上传失败 (${response.status})`;
        // Validation and state errors cannot be fixed by sending the same
        // request repeatedly. Retry only transport and server back-pressure.
        if (![408, 425, 429].includes(response.status) && response.status < 500) {
          lastError = new Error(message);
          stopRetry = true;
        } else {
          lastError = new Error(message);
        }
      } catch (error) {
        if (!stopRetry) lastError = error;
      }
      if (stopRetry) break;
      if (attempt + 1 < FILE_UPLOAD_ATTEMPTS) {
        await new Promise((resolve) => window.setTimeout(resolve, Math.min(8000, 800 * 2 ** attempt)));
      }
    }
    throw lastError instanceof Error ? lastError : new Error('上传失败');
  }

  async function sendFiles(peer: PeerInfo, files: File[]) {
    if (remoteMode && !peer.capabilities.includes(TRANSFER_RELAY_CAPABILITY)) {
      notice = '目标节点不支持远程文件中继，请先升级目标节点';
      return;
    }
    const target = targetBase(peer);
    if (!target || !node) {
      notice = '目标设备没有可访问地址';
      return;
    }
    selectedPeer = null;
    notice = `正在准备发送 ${files.length} 个项目…`;
    try {
      const items = await Promise.all(files.map(createItem));
      const offer: TransferOffer = {
        transfer_id: randomId(),
        source_node_id: node.node_id,
        source_name: node.node_name,
        target_node_id: peer.node_id,
        items,
        total_bytes: items.reduce((sum, item) => sum + item.size, 0),
        created_at_ms: Date.now(),
      };
      const localProgress = remoteMode
        ? undefined
        : await invoke<TransferProgress>('register_outgoing_transfer', { offer }).catch(() => undefined);
      if (localProgress?.status === 'failed') {
        observeFailure({ ...localProgress, offer, direction: 'send' });
        throw new Error(localProgress.error ?? '传输任务数量已达到上限');
      }
      try {
        const created = await request<TransferProgress>(remoteMode ? '/api/v1/relay/transfers' : '/api/v1/transfers', {
          method: 'POST',
          body: JSON.stringify(offer),
        }, remoteMode ? activeBaseUrl : target);
        if (created.status === 'failed') {
          throw new Error(created.error ?? '无法创建传输');
        }
      } catch (error) {
        if (localProgress) {
          const failedProgress = { ...localProgress, status: 'failed' as const, error: error instanceof Error ? error.message : '无法创建传输', offer, direction: 'send' as const };
          observeFailure(failedProgress);
          await invoke('update_transfer_progress', {
            progress: failedProgress,
          }).catch(() => undefined);
        } else {
          observeFailure({
            transfer_id: offer.transfer_id,
            status: 'failed',
            completed_bytes: 0,
            total_bytes: offer.total_bytes,
            completed_items: 0,
            total_items: offer.items.length,
            error: error instanceof Error ? error.message : '无法创建传输',
            offer,
            direction: 'send',
          });
        }
        throw error;
      }
      pendingUploads.set(offer.transfer_id, { target: remoteMode ? activeBaseUrl : target, files, items, offer, relay: remoteMode });
      transfers = [{ transfer_id: offer.transfer_id, status: 'waiting', completed_bytes: 0, total_bytes: offer.total_bytes, completed_items: 0, total_items: items.length, offer, direction: 'send' }, ...transfers];
      void watchTransfer(offer.transfer_id, remoteMode ? activeBaseUrl : target, remoteMode);
      notice = '已发送传输请求，等待接收端确认';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法创建传输';
    }
  }

  async function sendFilesToMobile(session: MobileSession, files: File[]) {
    if (!node) return;
    selectedMobileSession = null;
    notice = `正在准备发送 ${files.length} 个项目…`;
    let offerForFailure: TransferOffer | undefined;
    try {
      const items = await Promise.all(files.map(createItem));
      const offer: TransferOffer = {
        transfer_id: randomId(),
        source_node_id: node.node_id,
        source_name: node.node_name,
        target_node_id: node.node_id,
        items,
        total_bytes: items.reduce((sum, item) => sum + item.size, 0),
        created_at_ms: Date.now(),
      };
      offerForFailure = offer;
      pendingMobileUploads.set(offer.transfer_id, { files, items, completedItemIds: new Set(), sessionId: session.session_id, offer });
      await request<MobileTransferView>('/api/v1/mobile/transfers', {
        method: 'POST',
        body: JSON.stringify({ session_id: session.session_id, offer }),
      });
      transfers = [{ transfer_id: offer.transfer_id, status: 'waiting', completed_bytes: 0, total_bytes: offer.total_bytes, completed_items: 0, total_items: items.length, offer, direction: 'send' }, ...transfers];
      void watchMobileTransfer(offer.transfer_id);
      notice = '已发送，等待网页端确认';
    } catch (error) {
      const message = error instanceof Error ? error.message : '无法发送到网页端';
      if (offerForFailure) {
        observeFailure({
          transfer_id: offerForFailure.transfer_id,
          status: 'failed',
          completed_bytes: 0,
          total_bytes: offerForFailure.total_bytes,
          completed_items: 0,
          total_items: offerForFailure.items.length,
          error: message,
          offer: offerForFailure,
          direction: 'send',
        });
      }
      notice = message;
    }
  }

  async function watchMobileTransfer(id: string) {
    if (mobileWatching.has(id)) return;
    const generation = targetGeneration;
    mobileWatching.add(id);
    for (let attempt = 0; attempt < 360; attempt += 1) {
      if (generation !== targetGeneration) {
        mobileWatching.delete(id);
        return;
      }
      try {
        const view = (await getMobileTransfers()).find((candidate) => candidate.progress.transfer_id === id);
        if (generation !== targetGeneration) {
          mobileWatching.delete(id);
          return;
        }
        if (!view) throw new Error('网页端传输任务不存在');
        const progress = { ...view.progress, direction: 'send' as const };
        const existing = transfers.find((item) => item.transfer_id === id);
        const nextProgress = existing && shouldKeepTransferProgress(existing, progress)
          ? { ...progress, ...existing, offer: progress.offer ?? existing.offer, direction: 'send' as const }
          : { ...existing, ...progress, direction: 'send' as const, offer: progress.offer ?? existing?.offer };
        observeFailure(nextProgress);
        transfers = [nextProgress, ...transfers.filter((item) => item.transfer_id !== id)];
        if (['accepted', 'uploading'].includes(nextProgress.status) && !mobileUploading.has(id) && !mobileUploadStopped.has(id)) {
          await uploadMobileTransfer(id);
        }
        if (['ready', 'completed', 'rejected', 'failed', 'cancelled'].includes(nextProgress.status)) {
          if (nextProgress.status !== 'failed') forgetPendingTransfer(id);
          if (nextProgress.status === 'failed') {
            mobileUploadAttempts.delete(id);
          } else {
            mobileUploadAttempts.delete(id);
            mobileUploadStopped.delete(id);
          }
          mobileWatching.delete(id);
          return;
        }
      } catch (error) {
        if (attempt === 359 && error instanceof Error) notice = error.message;
      }
      await new Promise((resolve) => window.setTimeout(resolve, 1000));
    }
    mobileWatching.delete(id);
  }

  async function uploadMobileTransfer(id: string) {
    const pending = pendingMobileUploads.get(id);
    if (!pending || mobileUploadStopped.has(id)) return;
    mobileUploading.add(id);
    try {
      const latest = (await getMobileTransfers()).find((candidate) => candidate.progress.transfer_id === id);
      if (!latest || ['ready', 'rejected', 'failed', 'cancelled'].includes(latest.progress.status)) return;
      for (const item of pending.items) {
        // Keep the exact item IDs confirmed by this page. The server also
        // treats an already-staged item as idempotent after a lost response.
        if (pending.completedItemIds.has(item.item_id)) continue;
        const file = pending.files.find((candidate) => ((candidate as File & { webkitRelativePath?: string }).webkitRelativePath || candidate.name) === item.relative_path) ?? pending.files[pending.items.indexOf(item)];
        if (!file) throw new Error(`找不到待上传文件：${item.relative_path}`);
        await uploadFileWithRetry(
          apiUrl(`/api/v1/mobile/transfers/${encodeURIComponent(id)}/items/${encodeURIComponent(item.item_id)}`),
          file,
          item.media_type,
        );
        pending.completedItemIds.add(item.item_id);
      }
      mobileUploadAttempts.delete(id);
      showTransientNotice('文件已准备好，网页端可以下载');
    } catch (error) {
      const attempts = (mobileUploadAttempts.get(id) ?? 0) + 1;
      mobileUploadAttempts.set(id, attempts);
      const message = error instanceof Error ? error.message : '网页端文件上传失败';
      if (attempts >= MOBILE_UPLOAD_MAX_ATTEMPTS) {
        mobileUploadStopped.add(id);
        await request(`/api/v1/mobile/transfers/${encodeURIComponent(id)}/fail`, {
          method: 'POST',
          body: JSON.stringify({ error: message }),
        }).catch(() => undefined);
        showTransientNotice(`${message}，已停止重试`);
      } else {
        notice = `${message}，正在重试（${attempts}/${MOBILE_UPLOAD_MAX_ATTEMPTS}）`;
      }
    } finally {
      mobileUploading.delete(id);
    }
  }

  async function acceptMobileTransfer(view: MobileTransferView) {
    if (!mobileSessionId) return;
    if (mobileDirectoryPickerAvailable() && !mobileReceiveDirectory && !(await chooseMobileReceiveDirectory())) return;
    await request(`/api/v1/mobile/transfers/${encodeURIComponent(view.progress.transfer_id)}/accept`, {
      method: 'POST',
      body: JSON.stringify({ session_id: mobileSessionId }),
    }).catch((error) => { notice = error instanceof Error ? error.message : '无法接收文件'; });
    await refresh();
  }

  async function rejectMobileTransfer(view: MobileTransferView) {
    if (!mobileSessionId) return;
    await request(`/api/v1/mobile/transfers/${encodeURIComponent(view.progress.transfer_id)}/reject`, {
      method: 'POST',
      body: JSON.stringify({ session_id: mobileSessionId }),
    }).catch((error) => { notice = error instanceof Error ? error.message : '无法拒绝文件'; });
    await refresh();
  }

  async function cancelMobileTransfer(view: MobileTransferView) {
    if (!mobileSessionId) return;
    await request(`/api/v1/mobile/transfers/${encodeURIComponent(view.progress.transfer_id)}/cancel`, {
      method: 'POST',
      body: JSON.stringify({ session_id: mobileSessionId }),
    }).catch((error) => { notice = error instanceof Error ? error.message : '无法取消文件'; });
    await refresh();
  }

  async function watchTransfer(id: string, target: string, relay = false) {
    if (watching.has(id)) return;
    const generation = targetGeneration;
    watching.add(id);
    for (let attempt = 0; attempt < 360; attempt += 1) {
      if (generation !== targetGeneration) {
        watching.delete(id);
        return;
      }
      try {
        const progress = await request<TransferProgress>(`/api/v1/transfers/${encodeURIComponent(id)}/status`, {}, target);
        if (generation !== targetGeneration) {
          watching.delete(id);
          return;
        }
        if (!remoteMode) void invoke('update_transfer_progress', { progress }).catch(() => undefined);
        const existing = transfers.find((item) => item.transfer_id === id);
        const nextProgress = existing && shouldKeepTransferProgress(existing, progress)
          ? { ...progress, ...existing, offer: progress.offer ?? existing.offer }
          : { ...existing, ...progress, offer: progress.offer ?? existing?.offer };
        observeFailure(nextProgress);
        transfers = [nextProgress, ...transfers.filter((item) => item.transfer_id !== id)];
        if (['accepted', 'uploading'].includes(nextProgress.status) && !uploading.has(id) && !uploadStopped.has(id)) await uploadTransfer(id, target, relay);
        if (['completed', 'rejected', 'failed', 'cancelled'].includes(nextProgress.status)) {
          if (nextProgress.status !== 'failed') forgetPendingTransfer(id);
          else uploadAttempts.delete(id);
          watching.delete(id);
          return;
        }
      } catch {
        // The target may be briefly offline; the next poll will recover it.
      }
      await new Promise((resolve) => window.setTimeout(resolve, 800));
    }
    watching.delete(id);
  }

  async function uploadTransfer(id: string, target: string, relay = false) {
    const pending = pendingUploads.get(id);
    if (!pending || uploadStopped.has(id)) return;
    uploading.add(id);
    try {
      for (const item of pending.items) {
        const file = pending.files.find((candidate) => ((candidate as File & { webkitRelativePath?: string }).webkitRelativePath || candidate.name) === item.relative_path) ?? pending.files[pending.items.indexOf(item)];
        if (!file) throw new Error(`找不到待上传文件：${item.relative_path}`);
        await uploadFileWithRetry(
          `${target}${relay ? '/api/v1/relay/transfers' : '/api/v1/transfers'}/${encodeURIComponent(id)}/items/${encodeURIComponent(item.item_id)}`,
          file,
          item.media_type,
        );
      }
      uploadAttempts.delete(id);
      showTransientNotice('文件发送完成');
    } catch (error) {
      notice = error instanceof Error ? error.message : '文件发送失败';
      const attempts = (uploadAttempts.get(id) ?? 0) + 1;
      uploadAttempts.set(id, attempts);
      if (attempts >= DESKTOP_UPLOAD_MAX_ATTEMPTS) {
        uploadStopped.add(id);
        const message = `${notice}，已停止重试`;
        const failPath = relay
          ? `/api/v1/relay/transfers/${encodeURIComponent(id)}/fail`
          : `/api/v1/transfers/${encodeURIComponent(id)}/fail`;
        await request(failPath, { method: 'POST', body: JSON.stringify({ error: message }) }, target).catch(() => undefined);
        showTransientNotice(message);
      } else {
        // Keep the accepted task alive. watchTransfer will retry the current
        // item after the peer becomes reachable again; PUT is idempotent for
        // an already committed item.
        notice = `${notice}，连接恢复后将继续（${attempts}/${DESKTOP_UPLOAD_MAX_ATTEMPTS}）`;
      }
    } finally {
      uploading.delete(id);
    }
  }

  async function retryFailure(failure: TransferFailure) {
    if (transferActionBusy) return;
    const id = failure.progress.transfer_id;
    dismissedFailures.delete(id);
    const pending = pendingUploads.get(id);
    if (pending) {
      transferActionBusy = id;
      try {
        const path = pending.relay
          ? `/api/v1/relay/transfers/${encodeURIComponent(id)}/retry`
          : `/api/v1/transfers/${encodeURIComponent(id)}/retry`;
        const progress = await request<TransferProgress>(path, { method: 'POST' }, pending.target);
        uploadAttempts.delete(id);
        uploadStopped.delete(id);
        failures = failures.filter((item) => item.progress.transfer_id !== id);
        transfers = [{ ...progress, direction: 'send', offer: progress.offer ?? pending.offer }, ...transfers.filter((item) => item.transfer_id !== id)];
        void watchTransfer(id, pending.target, pending.relay);
      } catch (error) {
        notice = error instanceof Error ? error.message : '无法重试传输';
      } finally {
        transferActionBusy = '';
      }
      return;
    }
    const mobilePending = pendingMobileUploads.get(id);
    if (!mobilePending) return;
    transferActionBusy = id;
    try {
      const offer: TransferOffer = {
        ...mobilePending.offer,
        transfer_id: randomId(),
        created_at_ms: Date.now(),
      };
      await request<MobileTransferView>('/api/v1/mobile/transfers', {
        method: 'POST',
        body: JSON.stringify({ session_id: mobilePending.sessionId, offer }),
      });
      pendingMobileUploads.delete(id);
      pendingMobileUploads.set(offer.transfer_id, { ...mobilePending, offer, completedItemIds: new Set() });
      failures = failures.filter((item) => item.progress.transfer_id !== id);
      const progress: TransferProgress = {
        transfer_id: offer.transfer_id,
        status: 'waiting',
        completed_bytes: 0,
        total_bytes: offer.total_bytes,
        completed_items: 0,
        total_items: offer.items.length,
        offer,
        direction: 'send',
      };
      transfers = [progress, ...transfers.filter((item) => item.transfer_id !== id)];
      void watchMobileTransfer(offer.transfer_id);
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法重试网页传输';
    } finally {
      transferActionBusy = '';
    }
  }

  type MobileWritable = {
    write: (data: Uint8Array) => Promise<void>;
    close: () => Promise<void>;
    abort?: () => Promise<void>;
  };
  type MobileFileHandle = {
    getFile: () => Promise<File>;
    createWritable: () => Promise<MobileWritable>;
  };
  type MobileDirectoryHandle = {
    name: string;
    getDirectoryHandle: (name: string, options?: { create?: boolean }) => Promise<MobileDirectoryHandle>;
    getFileHandle: (name: string, options?: { create?: boolean }) => Promise<MobileFileHandle>;
    removeEntry: (name: string, options?: { recursive?: boolean }) => Promise<void>;
  };
  type MobilePickerWindow = Window & {
    showDirectoryPicker?: (options?: { mode?: 'readwrite' }) => Promise<MobileDirectoryHandle>;
  };

  function mobileDirectoryPickerAvailable() {
    return isWeb && window.isSecureContext && typeof (window as MobilePickerWindow).showDirectoryPicker === 'function';
  }

  async function chooseMobileReceiveDirectory() {
    if (!mobileDirectoryPickerAvailable()) {
      notice = window.isSecureContext ? '当前浏览器不支持选择存储目录，可使用浏览器默认下载' : '当前页面将使用浏览器默认下载，选择目录请打开 HTTPS 地址';
      window.setTimeout(() => (notice = ''), 3000);
      return false;
    }
    try {
      const picker = (window as MobilePickerWindow).showDirectoryPicker;
      if (!picker) return false;
      const directory = await picker({ mode: 'readwrite' });
      mobileReceiveDirectory = directory;
      mobileReceiveDirectoryName = directory.name;
      notice = `已选择存储目录：${mobileReceiveDirectoryName}`;
      window.setTimeout(() => (notice = ''), 2400);
      return true;
    } catch (error) {
      if (error instanceof DOMException && error.name === 'AbortError') return false;
      notice = error instanceof Error ? error.message : '无法选择接收目录';
      window.setTimeout(() => (notice = ''), 3000);
      return false;
    }
  }

  function mobilePathParts(relativePath: string) {
    if (!relativePath || relativePath.includes('\\')) throw new Error('传输路径无效');
    const parts = relativePath.split('/');
    if (parts.some((part) => !part || part === '.' || part === '..')) throw new Error('传输路径无效');
    return parts;
  }

  async function mobileTargetHandle(relativePath: string) {
    if (!mobileReceiveDirectory) throw new Error('请先选择接收目录');
    const parts = mobilePathParts(relativePath);
    const fileName = parts.pop();
    if (!fileName) throw new Error('传输文件名无效');
    let directory = mobileReceiveDirectory;
    for (const part of parts) directory = await directory.getDirectoryHandle(part, { create: true });
    try {
      await directory.getFileHandle(fileName);
      throw new Error(`目标文件已存在：${relativePath}`);
    } catch (error) {
      if (error instanceof Error && error.message.startsWith('目标文件已存在：')) throw error;
      if (error instanceof DOMException && error.name !== 'NotFoundError') throw error;
    }
    return { directory, fileName };
  }

  async function downloadMobileItem(view: MobileTransferView, item: TransferItem) {
    if (!mobileSessionId) throw new Error('网页会话已过期');
    const { directory, fileName } = await mobileTargetHandle(item.relative_path);
    const response = await fetch(mobileDownloadUrl(view.progress.transfer_id, item.item_id, mobileSessionId));
    if (!response.ok) {
      const payload = await response.json().catch(() => ({}));
      throw new Error(payload.error ?? `下载失败 (${response.status})`);
    }
    if (!response.body) throw new Error('浏览器不支持流式下载');
    const fileHandle = await directory.getFileHandle(fileName, { create: true });
    const writable = await fileHandle.createWritable();
    const reader = response.body.getReader();
    const hasher = new Sha256();
    let total = 0;
    try {
      while (true) {
        const chunk = await reader.read();
        if (chunk.done) break;
        const bytes = chunk.value;
        total += bytes.byteLength;
        hasher.update(bytes);
        await writable.write(bytes);
      }
      if (total !== item.size || hasher.digestHex().toLowerCase() !== item.sha256.toLowerCase()) {
        throw new Error(`文件校验失败：${item.name}`);
      }
      await writable.close();
      await completeMobileItem(view.progress.transfer_id, item.item_id, mobileSessionId);
    } catch (error) {
      await reader.cancel().catch(() => undefined);
      await writable.abort?.().catch(() => undefined);
      await directory.removeEntry(fileName).catch(() => undefined);
      throw error;
    }
  }

  async function downloadMobileTransfer(view: MobileTransferView) {
    if (mobileDownloading.has(view.progress.transfer_id) || !mobileReceiveDirectory) return;
    if (view.progress.status !== 'ready') return;
    mobileDownloading.add(view.progress.transfer_id);
    try {
      const downloaded = new Set(view.downloaded_item_ids ?? []);
      for (const item of view.progress.offer?.items ?? []) {
        if (downloaded.has(item.item_id)) continue;
        await downloadMobileItem(view, item);
      }
      showTransientNotice('文件已保存到接收目录');
      await refresh();
    } catch (error) {
      const message = error instanceof Error ? error.message : '网页端文件保存失败';
      notice = message;
      await request(`/api/v1/mobile/transfers/${encodeURIComponent(view.progress.transfer_id)}/fail`, {
        method: 'POST',
        body: JSON.stringify({ error: message }),
      }).catch(() => undefined);
    } finally {
      mobileDownloading.delete(view.progress.transfer_id);
    }
  }

  async function chooseAndDownloadMobileTransfer(view: MobileTransferView) {
    if (!mobileDirectoryPickerAvailable()) return;
    if (!mobileReceiveDirectory && !(await chooseMobileReceiveDirectory())) return;
    await downloadMobileTransfer(view);
  }

  function mobileBrowserDownloadKey(view: MobileTransferView, item: TransferItem) {
    return `${view.progress.transfer_id}:${item.item_id}`;
  }

  function startMobileBrowserDownload(view: MobileTransferView, item: TransferItem) {
    if (!mobileSessionId || view.progress.status !== 'ready') return;
    if (view.downloaded_item_ids?.includes(item.item_id)) return;
    const anchor = document.createElement('a');
    anchor.href = mobileDownloadUrl(view.progress.transfer_id, item.item_id, mobileSessionId);
    anchor.download = item.name;
    anchor.rel = 'noreferrer';
    document.body.append(anchor);
    anchor.click();
    anchor.remove();
    const key = mobileBrowserDownloadKey(view, item);
    mobileBrowserDownloadStarted = new Set(mobileBrowserDownloadStarted).add(key);
    notice = `已开始下载 ${item.name}，完成后点击“确认完成”`;
  }

  async function confirmMobileBrowserDownload(view: MobileTransferView, item: TransferItem) {
    if (!mobileSessionId || view.progress.status !== 'ready') return;
    const key = mobileBrowserDownloadKey(view, item);
    if (!mobileBrowserDownloadStarted.has(key) || mobileBrowserDownloadConfirming.has(key)) return;
    mobileBrowserDownloadConfirming = new Set(mobileBrowserDownloadConfirming).add(key);
    try {
      await completeMobileItem(view.progress.transfer_id, item.item_id, mobileSessionId);
      const started = new Set(mobileBrowserDownloadStarted);
      started.delete(key);
      mobileBrowserDownloadStarted = started;
      showTransientNotice(`已确认下载 ${item.name}`);
      await refresh();
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法确认文件下载';
    } finally {
      const confirming = new Set(mobileBrowserDownloadConfirming);
      confirming.delete(key);
      mobileBrowserDownloadConfirming = confirming;
    }
  }

  function phoneTransferView(transfer: PhoneTransfer): MobileTransferView {
    return { session_id: mobileSessionId, progress: transfer, downloaded_item_ids: transfer.downloadedItemIds };
  }

  function acceptPhoneTransfer(transfer: PhoneTransfer) {
    if (isWeb) void acceptMobileTransfer(phoneTransferView(transfer));
    else void acceptTransfer(transfer);
  }

  function rejectPhoneTransfer(transfer: PhoneTransfer) {
    if (isWeb) void rejectMobileTransfer(phoneTransferView(transfer));
    else void rejectTransfer(transfer);
  }

  function cancelPhoneTransfer(transfer: PhoneTransfer) {
    if (isWeb) void cancelMobileTransfer(phoneTransferView(transfer));
  }

  function savePhoneTransfer(transfer: PhoneTransfer) {
    if (isWeb) void chooseAndDownloadMobileTransfer(phoneTransferView(transfer));
  }

  function downloadPhoneItem(transfer: PhoneTransfer, item: TransferItem) {
    if (isWeb) startMobileBrowserDownload(phoneTransferView(transfer), item);
  }

  function confirmPhoneItem(transfer: PhoneTransfer, item: TransferItem) {
    if (isWeb) void confirmMobileBrowserDownload(phoneTransferView(transfer), item);
  }

  async function acceptTransfer(transfer: TransferProgress) {
    if (!transfer.offer || transferActionBusy) return;
    let destination: string | string[] | null = null;
    let path: string | undefined;
    if (remoteMode) {
      const remotePath = window.prompt('输入目标节点上的接收目录', receiveSettings.default_path ?? '');
      path = remotePath?.trim() || undefined;
    } else if (isAndroidApp) {
      try {
        path = await invoke<string>('android_receive_directory');
      } catch (error) {
        notice = error instanceof Error ? error.message : '无法准备 Android 接收目录';
        return;
      }
    } else {
      try {
        destination = await open({ directory: true, multiple: false, title: '选择接收目录' });
      } catch {
        destination = null;
      }
      path = Array.isArray(destination) ? destination[0] : destination ?? undefined;
    }
    if (!path) {
      notice = remoteMode ? '需要输入目标节点上的接收目录' : '当前环境无法选择接收目录';
      return;
    }
    transferActionBusy = transfer.transfer_id;
    try {
      if (remoteMode) {
        await request<TransferProgress>(`/api/v1/transfers/${encodeURIComponent(transfer.transfer_id)}/accept`, {
          method: 'POST',
          body: JSON.stringify({ destination: path }),
        });
      } else {
        await invoke<TransferProgress>('accept_transfer', { transferId: transfer.transfer_id, destination: path });
      }
      notice = isAndroidApp && !remoteMode ? '已接受，文件将保存到 MeshShare 应用目录' : '已接受，等待文件直传';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法接受传输';
    } finally {
      transferActionBusy = '';
    }
  }

  async function rejectTransfer(transfer: TransferProgress) {
    if (transferActionBusy) return;
    transferActionBusy = transfer.transfer_id;
    try {
      await request(`/api/v1/transfers/${encodeURIComponent(transfer.transfer_id)}/reject`, { method: 'POST' }).catch(() => undefined);
    } finally {
      transferActionBusy = '';
    }
  }

</script>

<svelte:head>
  <title>{isMonitor ? 'MeshShare · 硬件监控' : isWeb ? 'MeshShare · 临时加入' : 'MeshShare · 局域网共享'}</title>
</svelte:head>

<div class:phone-shell={isPhone} class:monitor-shell={isMonitor} class:app-shell={!isPhone && !isMonitor}>
  {#if isMonitor}
    <main class="monitor-main" data-tauri-drag-region="deep" on:contextmenu={openMonitorMenu}>
      <div class="monitor-metric"><b>CPU</b><span>{formatPercent(monitorMetrics?.cpu_usage_percent)}</span></div>
      <div class="monitor-metric"><b>GPU</b><span>{formatPercent(gpuUtilization(monitorMetrics))}</span></div>
      <div class="monitor-metric"><b>↑</b><span>{formatRate(monitorMetrics?.network_up_bytes_per_sec)}</span></div>
      <div class="monitor-metric"><b>内存</b><span>{formatPercent(memoryPercent(monitorMetrics))}</span></div>
      <div class="monitor-metric"><b>显存</b><span>{formatPercent(gpuMemoryPercent(monitorMetrics))}</span></div>
      <div class="monitor-metric"><b>↓</b><span>{formatRate(monitorMetrics?.network_down_bytes_per_sec)}</span></div>
    </main>
  {:else if isPhone}
    <MobileApp
      mode={isWeb ? 'web' : 'android'}
      {node}
      {status}
      {statusKind}
      peers={isWeb ? mobileTargets : peers}
      transfers={phoneTransfers}
      failures={phoneFailures}
      {clipboard}
      bind:draft={clipboardDraft}
      history={clipboardHistory}
      historyBusy={clipboardHistoryBusy}
      selectedHistory={selectedClipboardHistory}
      {metrics}
      canChooseDirectory={mobileDirectoryPickerAvailable()}
      receiveDirectoryName={mobileReceiveDirectoryName}
      actionBusyId={transferActionBusy}
      downloadingIds={[...mobileDownloading]}
      startedDownloadKeys={[...mobileBrowserDownloadStarted]}
      onSendFile={(peer) => startChoose(peer)}
      onSendFolder={(peer) => startChoose(peer, true)}
      onAcceptTransfer={acceptPhoneTransfer}
      onRejectTransfer={rejectPhoneTransfer}
      onCancelTransfer={cancelPhoneTransfer}
      onSaveToDirectory={savePhoneTransfer}
      onStartDownload={downloadPhoneItem}
      onConfirmDownload={confirmPhoneItem}
      onRetryFailure={retryFailure}
      onDismissFailure={dismissFailure}
      onPublish={publishClipboard}
      onReadLocal={readMobileClipboard}
      onWriteLocal={writeMobileClipboard}
      onChooseDirectory={chooseMobileReceiveDirectory}
      onOpenHistory={openClipboardHistory}
      onRestoreHistory={restoreClipboardHistoryItem}
      onCloseHistory={closeHistoryDetail}
      onClearHistory={clearClipboardHistoryItems}
    />
  {:else}
    <AppHeader
      {node}
      remote={remoteMode}
      {remotePath}
      {status}
      {statusKind}
      {syncEnabled}
      {autostartEnabled}
      {autostartBusy}
      onSync={toggleSync}
      onAutostart={toggleAutostart}
      onSettings={() => (settingsOpen = true)}
      onBack={returnToPreviousTarget}
    />
    {#if isDesktopWindow && node && !clipboardStatus.available}
      <div class="platform-notice" role="status">{clipboardStatus.message ?? '系统剪贴板暂不可用'}{clipboardStatus.retrying ? '（正在重试）' : ''}；共享内容仍可在设备间同步。</div>
    {/if}
    <main class="app-main">
      <DevicePanel
        peers={peers}
        {mobileSessions}
        savedAddresses={savedNodeUrls}
        draggingKey={dragPeer}
        addOpen={addDeviceOpen}
        bind:manualAddress
        onToggleAdd={() => (addDeviceOpen = !addDeviceOpen)}
        onConnect={connectManualPeer}
        onConnectSaved={connectSavedPeer}
        onOpenPeer={openRemotePeer}
        onMobileAccess={() => (mobileAccessOpen = true)}
        onSendPeer={(peer) => startChoose(peer)}
        onSendPeerFolder={(peer) => startChoose(peer, true)}
        onDetails={selectMetricsPeer}
        onSendMobile={(session) => startChooseMobile(session)}
        onSendMobileFolder={(session) => startChooseMobile(session, true)}
        onDragEnter={(key) => (dragPeer = key)}
        onDragLeave={(key) => { if (dragPeer === key) dragPeer = ''; }}
        onDropPeer={onDrop}
        onDropMobile={onDropMobile}
      />
      <HardwarePanel
        metrics={metrics}
        {monitorLocked}
        {monitorVisible}
        canClickThrough={monitorCapabilities.can_click_through}
        onToggleLock={toggleMonitorLock}
        onToggleVisible={toggleMonitorVisibility}
      />
      <ClipboardPanel
        {clipboard}
        bind:draft={clipboardDraft}
        history={clipboardHistory}
        bind:tab={clipboardTab}
        busy={clipboardHistoryBusy}
        onPublish={publishClipboard}
        onOpenHistory={openClipboardHistory}
        onClearHistory={requestClearHistory}
      />
      <TransferPanel {transfers} {failures} onAccept={acceptTransfer} onReject={rejectTransfer} onRetry={retryFailure} onDismissFailure={dismissFailure} />
    </main>
    <SettingsDialog
      open={settingsOpen}
      {node}
      {updateFrequency}
      {updateStatus}
      {updateBusy}
      {receiveSettings}
      receiveBusy={receiveSettingsBusy}
      historySettings={clipboardHistorySettings}
      historyBusy={clipboardHistorySettingsBusy}
      onFrequency={setUpdateFrequency}
      onCheckUpdates={checkForUpdates}
      onReceiveMode={setReceiveMode}
      onChooseDirectory={chooseReceiveDirectory}
      onHistoryMode={setClipboardHistoryMode}
      onHistoryValue={setClipboardHistoryValue}
      onClose={() => (settingsOpen = false)}
    />

    <MobileAccessDialog
      open={mobileAccessOpen}
      nodeReady={!!node}
      addresses={mobileAddresses}
      {selectedAddress}
      {mobileUrl}
      qrImage={mobileQr && mobileQr.url === mobileUrl ? mobileQr.image : ''}
      secureUrl={secureMobileUrl ?? ''}
      caUrl={mobileAddress ? apiUrl('/api/v1/mobile/ca', mobileAddress.url) : ''}
      onSelect={(url) => (selectedAddress = url)}
      onClose={() => (mobileAccessOpen = false)}
    />
    <HistoryDetailDialog
      open={historyDetailOpen}
      detail={selectedClipboardHistory}
      busy={clipboardHistoryBusy}
      onRestore={restoreClipboardHistoryItem}
      onClose={closeHistoryDetail}
    />
    <MetricsDetailDialog
      open={metricsPeerOpen && !!selectedMetricsPeer}
      peer={selectedMetricsPeer}
      onClose={() => (metricsPeerOpen = false)}
    />
    <ConfirmDialog
      open={clearHistoryOpen}
      title="清空剪贴板历史"
      message="本机保存的全部剪贴板历史会被删除，已同步到其他设备的内容不受影响。"
      confirmLabel="清空"
      onConfirm={confirmClearHistory}
      onCancel={cancelClearHistory}
    />
  {/if}
  {#if notice}<div class="notice" role="status">{notice}</div>{/if}
</div>

<input bind:this={fileInput} class="hidden-input" type="file" multiple on:change={onFileInput} />
<input bind:this={folderInput} class="hidden-input" type="file" multiple on:change={onFileInput} />
