<script lang="ts">
  import { onMount } from 'svelte';
  import QRCode from 'qrcode';
  import { open } from '@tauri-apps/plugin-dialog';
  import { invoke, isTauri } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { apiUrl, clearClipboardHistory, completeMobileItem, createMobileSession, getClipboard, getClipboardHistory, getClipboardHistoryDetail, getInfo, getMetrics, getMobileSessions, getMobileTransfers, getPeers, getTransfers, initApi, mobileDownloadUrl, request, restoreClipboardHistory, websocketUrl } from './api';
  import { Sha256, digestFile } from './sha256';
  import type { AddressInfo, ClipboardHistoryDetail, ClipboardHistoryItem, ClipboardHistoryMode, ClipboardHistorySettings, ClipboardSnapshot, ClipboardStatus, HardwareMetrics, MobileSession, MobileTransferView, MonitorCapabilities, NodeInfo, PeerInfo, ReceiveMode, ReceiveSettings, TransferItem, TransferOffer, TransferProgress, UpdateFrequency, UpdateStatus, WireMessage } from './types';
  import { clipboardHistoryKind, clipboardHistoryPreview, formatPercent, formatRate, formatTime, gpuMemoryPercent, gpuUtilization, memoryPercent, updateFrequencyLabel, webSessionLabel } from './format';
  import Icon from './components/Icon.svelte';
  import AppHeader from './components/AppHeader.svelte';
  import DevicePanel from './components/DevicePanel.svelte';
  import DeviceRow from './components/DeviceRow.svelte';
  import HardwarePanel from './components/HardwarePanel.svelte';
  import ClipboardPanel from './components/ClipboardPanel.svelte';
  import TransferPanel from './components/TransferPanel.svelte';
  import SettingsDialog from './components/SettingsDialog.svelte';
  import MobileAccessDialog from './components/MobileAccessDialog.svelte';
  import HistoryDetailDialog from './components/HistoryDetailDialog.svelte';
  import MetricsDetailDialog from './components/MetricsDetailDialog.svelte';
  import ConfirmDialog from './components/ConfirmDialog.svelte';

  const isMonitor = new URLSearchParams(window.location.search).get('window') === 'monitor';
  if (isMonitor) document.documentElement.dataset.window = 'monitor';
  const inTauri = isTauri();
  // 浏览器访问（根地址或 /mobile）一律渲染网页临时加入界面，Tauri 窗口按窗口类型区分
  const isWeb = !inTauri && !isMonitor;
  const isDesktopWindow = inTauri && !isMonitor;
  let node: NodeInfo | null = null;
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
  let mobileSessions: MobileSession[] = [];
  let mobileTransfers: MobileTransferView[] = [];
  let mobileSessionId = '';
  let manualAddress = '';
  let monitorLocked = false;
  let settingsOpen = false;
  let mobileAccessOpen = false;
  let addDeviceOpen = false;
  let clipboardTab: 'current' | 'history' = 'current';
  let historyDetailOpen = false;
  let metricsPeerOpen = false;
  let clearHistoryOpen = false;  const pendingUploads = new Map<string, { target: string; files: File[]; items: TransferItem[] }>();
  const uploading = new Set<string>();
  const watching = new Set<string>();
  const pendingMobileUploads = new Map<string, { files: File[]; items: TransferItem[]; completedItemIds: Set<string> }>();
  const mobileUploading = new Set<string>();
  const mobileWatching = new Set<string>();
  const mobileUploadAttempts = new Map<string, number>();
  const mobileUploadStopped = new Set<string>();
  const uploadAttempts = new Map<string, number>();
  const uploadStopped = new Set<string>();
  const mobileDownloading = new Set<string>();
  const MOBILE_UPLOAD_MAX_ATTEMPTS = 6;
  const DESKTOP_UPLOAD_MAX_ATTEMPTS = 6;
  const FILE_UPLOAD_ATTEMPTS = 4;
  const WS_RECONNECT_BASE_MS = 1500;
  const WS_RECONNECT_MAX_MS = 30000;
  let mobileReceiveDirectory: MobileDirectoryHandle | null = null;
  let mobileReceiveDirectoryName = '';
  let pageStopped = false;
  let wsReconnectTimer: number | undefined;
  let wsReconnectAttempt = 0;
  let wsGeneration = 0;
  let wsPingTimer: number | undefined;

  $: mobileAddress = mobileAddresses.find((address) => address.url === selectedAddress);
  $: mobileUrl = mobileAddress ? mobileAddress.url : '';
  $: secureMobileUrl = node?.mobile_addresses?.find((address) => address.address === mobileAddress?.address)?.url;
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

  onMount(() => {
    pageStopped = false;
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
        updateStatus = payload;
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
        await initApi();
        if (inTauri && !isWeb) {
          monitorCapabilities = await invoke<MonitorCapabilities>('monitor_capabilities').catch(() => monitorCapabilities);
          monitorLocked = await invoke<boolean>('monitor_locked').catch(() => false);
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
    if (inTauri && !isWeb) monitorLocked = await invoke<boolean>('monitor_locked').catch(() => monitorLocked);
    if (isMonitor) {
      const [nextNode, nextMetrics] = await Promise.all([getInfo(), getMetrics()]);
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
      isDesktopWindow ? invoke<ClipboardStatus>('clipboard_status').catch(() => clipboardStatus) : Promise.resolve(clipboardStatus),
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
    transfers = [...nextTransfers, ...transfers.filter((item) => !nextTransfers.some((next) => next.transfer_id === item.transfer_id))];
    for (const transfer of nextTransfers) {
      if (transfer.direction !== 'send' || !transfer.offer || ['completed', 'rejected', 'failed', 'cancelled'].includes(transfer.status) || watching.has(transfer.transfer_id)) continue;
      const targetPeer = peers.find((peer) => peer.node_id === transfer.offer?.target_node_id);
      const target = targetPeer ? targetBase(targetPeer) : null;
      if (target) void watchTransfer(transfer.transfer_id, target);
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
      transfers = [{
        transfer_id: offer.transfer_id,
        status: 'waiting',
        completed_bytes: 0,
        total_bytes: offer.total_bytes,
        completed_items: 0,
        total_items: offer.items.length,
        offer,
        direction: 'receive',
      }, ...transfers.filter((item) => item.transfer_id !== offer.transfer_id)];
      return;
    }
    if (type === 'TransferProgress') {
      const progress = payload.progress;
      const existing = transfers.find((item) => item.transfer_id === progress.transfer_id);
      transfers = [{ ...existing, ...progress }, ...transfers.filter((item) => item.transfer_id !== progress.transfer_id)];
    }
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
    syncEnabled = await invoke<boolean>('toggle_clipboard_sync').catch(() => !syncEnabled);
    notice = syncEnabled ? '剪贴板自动同步已恢复' : '剪贴板自动同步已暂停';
    window.setTimeout(() => (notice = ''), 2200);
  }

  async function toggleAutostart() {
    if (autostartBusy) return;
    const next = !autostartEnabled;
    autostartBusy = true;
    try {
      autostartEnabled = await invoke<boolean>('set_autostart_enabled', { enabled: next });
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
      updateFrequency = await invoke<UpdateFrequency>('set_update_frequency', { frequency: next });
      updateStatus = await invoke<UpdateStatus>('update_status').catch(() => updateStatus);
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
      receiveSettings = await invoke<ReceiveSettings>('set_receive_settings', {
        mode,
        defaultPath: defaultPath ?? receiveSettings.default_path ?? null,
      });
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
      clipboardHistorySettings = await invoke<ClipboardHistorySettings>('set_clipboard_history_settings', { mode, value });
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
      await invoke('check_for_updates');
      updateStatus = await invoke<UpdateStatus>('update_status').catch(() => updateStatus);
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
      monitorLocked = await invoke<boolean>('set_monitor_locked', { locked: !monitorLocked });
      notice = monitorLocked ? '悬浮监控条已锁定，鼠标交互已忽略' : '悬浮监控条已解锁，可以拖动';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法更新悬浮监控条状态';
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
    try {
      await request('/api/v1/peers/connect', { method: 'POST', body: JSON.stringify({ url }) });
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
      const localProgress = await invoke<TransferProgress>('register_outgoing_transfer', { offer }).catch(() => undefined);
      if (localProgress?.status === 'failed') {
        throw new Error(localProgress.error ?? '传输任务数量已达到上限');
      }
      try {
        await request<TransferProgress>('/api/v1/transfers', { method: 'POST', body: JSON.stringify(offer) }, target);
      } catch (error) {
        if (localProgress) {
          await invoke('update_transfer_progress', {
            progress: { ...localProgress, status: 'failed', error: error instanceof Error ? error.message : '无法创建传输' },
          }).catch(() => undefined);
        }
        throw error;
      }
      pendingUploads.set(offer.transfer_id, { target, files, items });
      transfers = [{ transfer_id: offer.transfer_id, status: 'waiting', completed_bytes: 0, total_bytes: offer.total_bytes, completed_items: 0, total_items: items.length, offer, direction: 'send' }, ...transfers];
      void watchTransfer(offer.transfer_id, target);
      notice = '已发送传输请求，等待接收端确认';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法创建传输';
    }
  }

  async function sendFilesToMobile(session: MobileSession, files: File[]) {
    if (!node) return;
    selectedMobileSession = null;
    notice = `正在准备发送 ${files.length} 个项目…`;
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
      await request<MobileTransferView>('/api/v1/mobile/transfers', {
        method: 'POST',
        body: JSON.stringify({ session_id: session.session_id, offer }),
      });
      pendingMobileUploads.set(offer.transfer_id, { files, items, completedItemIds: new Set() });
      transfers = [{ transfer_id: offer.transfer_id, status: 'waiting', completed_bytes: 0, total_bytes: offer.total_bytes, completed_items: 0, total_items: items.length, offer, direction: 'send' }, ...transfers];
      void watchMobileTransfer(offer.transfer_id);
      notice = '已发送，等待网页端确认';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法发送到网页端';
    }
  }

  async function watchMobileTransfer(id: string) {
    if (mobileWatching.has(id)) return;
    mobileWatching.add(id);
    for (let attempt = 0; attempt < 360; attempt += 1) {
      try {
        const view = (await getMobileTransfers()).find((candidate) => candidate.progress.transfer_id === id);
        if (!view) throw new Error('网页端传输任务不存在');
        const progress = view.progress;
        const existing = transfers.find((item) => item.transfer_id === id);
        transfers = [{ ...existing, ...progress, direction: 'send' }, ...transfers.filter((item) => item.transfer_id !== id)];
        if (['accepted', 'uploading'].includes(progress.status) && !mobileUploading.has(id) && !mobileUploadStopped.has(id)) {
          await uploadMobileTransfer(id);
        }
        if (['ready', 'completed', 'rejected', 'failed', 'cancelled'].includes(progress.status)) {
          pendingMobileUploads.delete(id);
          mobileUploadAttempts.delete(id);
          mobileUploadStopped.delete(id);
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
      notice = '文件已准备好，网页端可以下载';
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
        notice = `${message}，已停止重试`;
      } else {
        notice = `${message}，正在重试（${attempts}/${MOBILE_UPLOAD_MAX_ATTEMPTS}）`;
      }
    } finally {
      mobileUploading.delete(id);
    }
  }

  async function acceptMobileTransfer(view: MobileTransferView) {
    if (!mobileSessionId) return;
    if (!mobileReceiveDirectory && !(await chooseMobileReceiveDirectory())) return;
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

  async function watchTransfer(id: string, target: string) {
    if (watching.has(id)) return;
    watching.add(id);
    for (let attempt = 0; attempt < 360; attempt += 1) {
      try {
        const progress = await request<TransferProgress>(`/api/v1/transfers/${encodeURIComponent(id)}/status`, {}, target);
        void invoke('update_transfer_progress', { progress }).catch(() => undefined);
        const existing = transfers.find((item) => item.transfer_id === id);
        transfers = [{ ...existing, ...progress, direction: existing?.direction ?? progress.direction }, ...transfers.filter((item) => item.transfer_id !== id)];
        if (['accepted', 'uploading'].includes(progress.status) && !uploading.has(id) && !uploadStopped.has(id)) await uploadTransfer(id, target);
        if (['completed', 'rejected', 'failed', 'cancelled'].includes(progress.status)) {
          pendingUploads.delete(id);
          uploadAttempts.delete(id);
          uploadStopped.delete(id);
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

  async function uploadTransfer(id: string, target: string) {
    const pending = pendingUploads.get(id);
    if (!pending || uploadStopped.has(id)) return;
    uploading.add(id);
    try {
      for (const item of pending.items) {
        const file = pending.files.find((candidate) => ((candidate as File & { webkitRelativePath?: string }).webkitRelativePath || candidate.name) === item.relative_path) ?? pending.files[pending.items.indexOf(item)];
        if (!file) throw new Error(`找不到待上传文件：${item.relative_path}`);
        await uploadFileWithRetry(
          `${target}/api/v1/transfers/${encodeURIComponent(id)}/items/${encodeURIComponent(item.item_id)}`,
          file,
          item.media_type,
        );
      }
      uploadAttempts.delete(id);
      notice = '文件发送完成';
    } catch (error) {
      notice = error instanceof Error ? error.message : '文件发送失败';
      const attempts = (uploadAttempts.get(id) ?? 0) + 1;
      uploadAttempts.set(id, attempts);
      if (attempts >= DESKTOP_UPLOAD_MAX_ATTEMPTS) {
        uploadStopped.add(id);
        await request(`/api/v1/transfers/${encodeURIComponent(id)}/cancel`, { method: 'POST' }, target).catch(() => undefined);
        notice = `${notice}，已停止重试`;
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
      notice = window.isSecureContext ? '当前浏览器不支持选择存储目录' : '网页接收需要通过 HTTPS 打开页面';
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
      notice = '文件已保存到接收目录';
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

  async function acceptTransfer(transfer: TransferProgress) {
    if (!transfer.offer) return;
    let destination: string | string[] | null = null;
    try {
      destination = await open({ directory: true, multiple: false, title: '选择接收目录' });
    } catch {
      destination = null;
    }
    const path = Array.isArray(destination) ? destination[0] : destination;
    if (!path) {
      notice = '当前环境无法选择接收目录';
      return;
    }
    try {
      await invoke<TransferProgress>('accept_transfer', { transferId: transfer.transfer_id, destination: path });
      notice = '已接受，等待文件直传';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法接受传输';
    }
  }

  async function rejectTransfer(transfer: TransferProgress) {
    await request(`/api/v1/transfers/${encodeURIComponent(transfer.transfer_id)}/reject`, { method: 'POST' }).catch(() => undefined);
  }

  function mobileTransferStatus(view: MobileTransferView): string {
    const status = view.progress.status;
    if (status === 'waiting') return '等待选择目录';
    if (status === 'accepted') return '准备中';
    if (status === 'uploading') return '正在传输';
    if (status === 'ready') return mobileDownloading.has(view.progress.transfer_id) ? '正在保存' : '等待保存';
    if (status === 'completed') return '已保存';
    if (status === 'rejected') return '已拒绝';
    return view.progress.error ?? status;
  }

</script>

<svelte:head>
  <title>{isMonitor ? 'MeshShare · 硬件监控' : isWeb ? 'MeshShare · 临时加入' : 'MeshShare · 局域网共享'}</title>
</svelte:head>

<div class:mobile-shell={isWeb} class:monitor-shell={isMonitor} class:app-shell={!isWeb && !isMonitor}>
  {#if isMonitor}
    <main class="monitor-main" data-tauri-drag-region="deep" on:contextmenu={openMonitorMenu}>
      <div class="monitor-metric"><b>CPU</b><span>{formatPercent(metrics?.cpu_usage_percent)}</span></div>
      <div class="monitor-metric"><b>GPU</b><span>{formatPercent(gpuUtilization(metrics))}</span></div>
      <div class="monitor-metric"><b>↑</b><span>{formatRate(metrics?.network_up_bytes_per_sec)}</span></div>
      <div class="monitor-metric"><b>内存</b><span>{formatPercent(memoryPercent(metrics))}</span></div>
      <div class="monitor-metric"><b>显存</b><span>{formatPercent(gpuMemoryPercent(metrics))}</span></div>
      <div class="monitor-metric"><b>↓</b><span>{formatRate(metrics?.network_down_bytes_per_sec)}</span></div>
    </main>
  {:else if isWeb}
    <header class="mobile-topbar">
      <div class="brand">
        <img src="/meshshare.svg" width="28" height="28" alt="" />
        <div class="brand-copy">
          <div class="brand-name">MeshShare</div>
          <div class="brand-sub">浏览器临时加入</div>
        </div>
      </div>
      <div class="node-status" data-kind={statusKind} role="status" aria-live="polite">
        <span class="status-dot" aria-hidden="true"></span>
        <span class="status-text">{status}</span>
      </div>
    </header>
    {#if notice}<div class="notice" role="status">{notice}</div>{/if}
    <main class="mobile-main">
      <section class="surface">
        <div class="surface-body">
          <h1>{node?.node_name ?? 'MeshShare 节点'}</h1>
          <p class="muted">选择下面的设备，直接发送文件或文件夹。</p>
        </div>
      </section>

      <section class="surface">
        <div class="surface-head">
          <div class="surface-title"><h2>接收目录</h2></div>
          <div class="surface-actions"><span class="muted">接收前选择</span></div>
        </div>
        <div class="surface-body">
          <div class="setting">
            <div class="setting-copy">
              <strong>{mobileReceiveDirectoryName || '尚未选择存储目录'}</strong>
              <span>{mobileDirectoryPickerAvailable() ? '文件会流式写入此目录，不会覆盖同名文件。' : (window.isSecureContext ? '当前浏览器不支持目录选择。' : '请通过 HTTPS 打开页面后选择目录。')}</span>
            </div>
            <div class="setting-actions">
              <button type="button" class="btn btn-primary btn-sm" on:click={chooseMobileReceiveDirectory} disabled={!mobileDirectoryPickerAvailable()}>选择目录</button>
            </div>
          </div>
        </div>
      </section>

      <section class="surface">
        <div class="surface-head">
          <div class="surface-title"><h2>共享剪贴板</h2></div>
          <div class="surface-actions"><span class="muted">{clipboard ? formatTime(clipboard.updated_at_ms) : '暂无内容'}</span></div>
        </div>
        <div class="surface-body">
          <textarea class="field" id="clipboard-editor" bind:value={clipboardDraft} placeholder="粘贴或输入需要同步的文字"></textarea>
          <div class="action-row">
            <button type="button" class="btn btn-primary btn-sm" on:click={publishClipboard}>同步到设备</button>
            <button type="button" class="btn btn-sm" on:click={readMobileClipboard}>读取本机剪贴板</button>
            <button type="button" class="btn btn-sm" on:click={writeMobileClipboard}>写入本机剪贴板</button>
          </div>
        </div>
      </section>

      <section class="surface">
        <div class="surface-head">
          <div class="surface-title"><h2>剪贴板历史</h2><span class="chip">{clipboardHistory.length}</span></div>
        </div>
        <div class="surface-body">
          {#if selectedClipboardHistory}
            <div class="history-detail-meta">
              <strong>{selectedClipboardHistory.item.source_name}</strong>
              <span class="muted">{clipboardHistoryKind(selectedClipboardHistory.item)} · {formatTime(selectedClipboardHistory.item.updated_at_ms)}</span>
            </div>
            <div class="history-detail-grid">
              <textarea class="field" readonly value={selectedClipboardHistory.snapshot.text ?? ''} placeholder="图片内容"></textarea>
              {#if selectedClipboardHistory.snapshot.image_png}
                <img class="clip-image" src={'data:image/png;base64,' + selectedClipboardHistory.snapshot.image_png} alt="历史共享图片" />
              {/if}
            </div>
            <div class="action-row">
              <button type="button" class="btn btn-primary btn-sm" on:click={restoreClipboardHistoryItem} disabled={clipboardHistoryBusy}>恢复并同步</button>
              <button type="button" class="btn btn-ghost btn-sm" on:click={closeHistoryDetail}>返回历史列表</button>
            </div>
          {:else}
            <div class="history-list">
              {#each clipboardHistory as item (item.event_id)}
                <button type="button" class="history-row" on:click={() => openClipboardHistory(item)}>
                  <span class="history-kind">{item.has_image ? 'PNG' : '文字'}</span>
                  <span class="history-preview">{clipboardHistoryPreview(item)}</span>
                  <span class="history-time">{formatTime(item.updated_at_ms)}</span>
                </button>
              {:else}
                <div class="empty">还没有保存的剪贴板历史。</div>
              {/each}
            </div>
            <div class="action-row">
              <button type="button" class="btn btn-ghost btn-sm" on:click={clearClipboardHistoryItems} disabled={clipboardHistoryBusy || !clipboardHistory.length}>清空历史</button>
            </div>
          {/if}
        </div>
      </section>

      <section class="surface">
        <div class="surface-head">
          <div class="surface-title"><h2>在线设备</h2><span class="chip">{mobileTargets.length}</span></div>
        </div>
        <div class="surface-body">
          <ul class="device-list device-list--stacked" role="list">
            {#each mobileTargets as peer (peer.node_id)}
              <DeviceRow
                kind="peer"
                {peer}
                onSend={() => startChoose(peer)}
                onSendFolder={() => startChoose(peer, true)}
                onDropFile={(event) => onDrop(peer, event)}
              />
            {:else}
              <li class="empty">暂时没有发现其他在线桌面设备。</li>
            {/each}
          </ul>
        </div>
      </section>

      <section class="surface">
        <div class="surface-head">
          <div class="surface-title"><h2>收到的文件</h2><span class="chip">{mobileTransfers.length}</span></div>
        </div>
        <div class="surface-body">
          <div class="transfer-list">
            {#each mobileTransfers as view (view.progress.transfer_id)}
              <article class="transfer-row transfer-row--mobile">
                <span class="transfer-icon" aria-hidden="true"><Icon name="arrow-down" size={14} /></span>
                <div class="transfer-copy">
                  <strong>{view.progress.offer?.source_name ?? '电脑文件'}</strong>
                  <span>{view.progress.offer?.items.map((item) => item.name).join('、') ?? '文件传输'}</span>
                </div>
                <div class="transfer-progress">
                  <div
                    class="progress"
                    role="progressbar"
                    aria-valuemin="0"
                    aria-valuemax="100"
                    aria-valuenow={view.progress.total_bytes ? Math.round(Math.min(100, view.progress.completed_bytes / view.progress.total_bytes * 100)) : 0}
                    aria-label="传输进度"
                  >
                    <span class="progress-fill" style="--p:{view.progress.total_bytes ? Math.min(1, view.progress.completed_bytes / view.progress.total_bytes) : 0}"></span>
                  </div>
                  <span>{mobileTransferStatus(view)}</span>
                </div>
                <div class="transfer-actions">
                  {#if view.progress.status === 'waiting'}
                    <button type="button" class="btn btn-primary btn-sm" on:click={() => acceptMobileTransfer(view)}>{mobileReceiveDirectory ? '开始接收' : '选择目录并接收'}</button>
                    <button type="button" class="btn btn-ghost btn-sm" on:click={() => rejectMobileTransfer(view)}>拒绝</button>
                  {/if}
                  {#if ['accepted', 'uploading', 'ready'].includes(view.progress.status)}
                    <button type="button" class="btn btn-ghost btn-sm" on:click={() => cancelMobileTransfer(view)}>取消</button>
                  {/if}
                  {#if view.progress.status === 'ready' && !mobileDownloading.has(view.progress.transfer_id)}
                    <button type="button" class="btn btn-primary btn-sm" on:click={() => mobileReceiveDirectory ? downloadMobileTransfer(view) : chooseMobileReceiveDirectory().then((selected) => { if (selected) void downloadMobileTransfer(view); })}>{mobileReceiveDirectory ? '保存到已选目录' : '选择目录并保存'}</button>
                  {/if}
                </div>
              </article>
            {:else}
              <div class="empty">还没有收到文件。</div>
            {/each}
          </div>
        </div>
      </section>
    </main>
  {:else}
    <AppHeader
      {node}
      {status}
      {statusKind}
      {syncEnabled}
      {autostartEnabled}
      {autostartBusy}
      onSync={toggleSync}
      onAutostart={toggleAutostart}
      onSettings={() => (settingsOpen = true)}
    />
    {#if notice}<div class="notice" role="status">{notice}</div>{/if}
    {#if isDesktopWindow && node && !clipboardStatus.available}
      <div class="platform-notice" role="status">{clipboardStatus.message ?? '系统剪贴板暂不可用'}{clipboardStatus.retrying ? '（正在重试）' : ''}；共享内容仍可在设备间同步。</div>
    {/if}
    <main class="app-main">
      <DevicePanel
        peers={peers}
        mobileSessions={mobileSessions}
        draggingKey={dragPeer}
        addOpen={addDeviceOpen}
        bind:manualAddress
        onToggleAdd={() => (addDeviceOpen = !addDeviceOpen)}
        onConnect={connectManualPeer}
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
        canClickThrough={monitorCapabilities.can_click_through}
        onToggleLock={toggleMonitorLock}
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
      <TransferPanel {transfers} onAccept={acceptTransfer} onReject={rejectTransfer} />
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
</div>

<input bind:this={fileInput} class="hidden-input" type="file" multiple on:change={onFileInput} />
<input bind:this={folderInput} class="hidden-input" type="file" multiple on:change={onFileInput} />
