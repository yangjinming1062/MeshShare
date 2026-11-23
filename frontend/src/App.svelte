<script lang="ts">
  import { onMount, tick } from 'svelte';
  import QRCode from 'qrcode';
  import { open } from '@tauri-apps/plugin-dialog';
  import { addPluginListener, invoke, isTauri } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { buildOffer, putFile, randomId, uploadFileWithRetry } from './upload';
  import { filesFromDataTransfer } from './drop';
  import { peerTargetBase, peerTargetCandidates, type PeerWebContext } from './peers';
  import { ApiError, responseError, isTransportFailure, withPeerAddress, apiUrl, cancelOutgoingStaging, clearClipboardHistory, clearTransferHistory, completeMobileItem, createMobileSession, createMobileTransfer, createOutgoingStaging, dismissMobileTransfer, dismissTransfer, getClipboard, getClipboardHistory, getClipboardHistoryDetail, getInfo, getMetrics, getMobileSessions, getMobileTransfers, getPeers, getSettings, getTransferHistory, getTransfers, initApi, mobileDownloadUrl, patchSettings, request, requestUpdateCheck, requestWithTimeout, outgoingPathsApi, restoreClipboardHistory, setApiTarget, startOutgoingStaging, websocketUrl } from './api';
  import { saveMobileResponse, type MobileDirectoryHandle } from './mobileSave';
  import { mergeTransferProgress } from './transferProgress';
  import type { AddressInfo, ClipboardHistoryDetail, ClipboardHistoryItem, ClipboardHistoryMode, ClipboardHistorySettings, ClipboardSnapshot, ClipboardStatus, HardwareMetrics, MobileSession, MobileTransferView, MonitorCapabilities, NodeInfo, NodeSettings, PeerInfo, PhoneTransfer, ReceiveMode, ReceiveSettings, TransferFailure, TransferHistoryEntry, TransferItem, TransferOffer, TransferPrepare, TransferProgress, UpdateFrequency, UpdateStatus, WireMessage } from './types';
  import { clipboardHistoryRetentionLabel, formatPercent, formatRate, gpuMemoryPercent, gpuUtilization, isActiveTransferStatus, isMetricsStale, memoryPercent, transferHistoryForSessionView, transferHistoryLocation, transferItemKey, updateFrequencyLabel, webSessionLabel } from './format';
  import TerminalWorkspace from './components/TerminalWorkspace.svelte';
  import { canOpenTerminal } from './terminal';
  import AppHeader from './components/AppHeader.svelte';
  import DevicePanel from './components/DevicePanel.svelte';
  import HardwarePanel from './components/HardwarePanel.svelte';
  import ClipboardPanel from './components/ClipboardPanel.svelte';
  import TransferPanel from './components/TransferPanel.svelte';
  import MobileApp from './components/MobileApp.svelte';
  import SettingsDialog from './components/SettingsDialog.svelte';
  import RemoteFileBrowser from './components/RemoteFileBrowser.svelte';
  import MobileAccessDialog from './components/MobileAccessDialog.svelte';
  import ClipboardHistoryDialog from './components/ClipboardHistoryDialog.svelte';
  import MetricsDetailDialog from './components/MetricsDetailDialog.svelte';
  import ConfirmDialog from './components/ConfirmDialog.svelte';
  import PromptDialog from './components/PromptDialog.svelte';

  const isMonitor = document.documentElement.dataset.window === 'monitor';
  const inTauri = isTauri();
  const isAndroidApp = inTauri && /Android/i.test(navigator.userAgent);
  // 浏览器访问（根地址或 /mobile）一律渲染网页临时加入界面，Tauri 窗口按窗口类型区分
  const isWeb = !inTauri && !isMonitor;
  const isDesktopWindow = inTauri && !isMonitor && !isAndroidApp;
  const isPhone = isWeb || isAndroidApp;
  // 原生界面可以把源文件交给本机节点，上传因此不再依赖页面存活。
  const isNativeSender = isDesktopWindow || isAndroidApp;
  document.documentElement.dataset.shell = isMonitor ? 'monitor' : isPhone ? (isWeb ? 'web' : 'android') : 'desktop';
  let node: NodeInfo | null = null;
  let activeBaseUrl = '';
  let remoteTarget: PeerInfo | null = null;
  let localBaseUrl = '';
  let localNodeId = '';
  let peers: PeerInfo[] = [];
  let clipboard: ClipboardSnapshot | null = null;
  let clipboardHistory: ClipboardHistoryItem[] = [];
  let clipboardHistorySettings: ClipboardHistorySettings = { mode: 'count', count: 50, days: 30 };
  let selectedClipboardHistory: ClipboardHistoryDetail | null = null;
  let clipboardHistoryBusy = false;
  let clipboardStatus: ClipboardStatus = { session: 'unknown', available: false, retrying: true, message: '正在连接系统剪贴板…' };
  type AndroidReceiveStorage = { directory: string; public_download: boolean };
  let androidReceiveStorage: AndroidReceiveStorage | null = null;
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
  let updateFrequency: UpdateFrequency = 'weekly';
  let updateStatus: UpdateStatus | null = null;
  const UNKNOWN_MONITOR_CAPABILITIES: MonitorCapabilities = { backend: 'unknown', can_persist_position: false, can_click_through: false, can_keep_above: false };
  let monitorCapabilities: MonitorCapabilities = UNKNOWN_MONITOR_CAPABILITIES;
  let receiveSettings: ReceiveSettings = { mode: 'confirm' };
  let settingsReady = false;
  let settingsRead: Promise<NodeSettings> | null = null;
  let settingsWrites = new Set<string>();
  let remoteFileAccess = false;
  let remoteFileAccessBusy = false;
  let browseOpen = false;
  let browseMode: 'send' | 'directory' | 'view' = 'send';
  let browseStartPath = '';
  let browsePeer: PeerInfo | null = null;
  let browseTransfer: TransferProgress | null = null;
  let ws: WebSocket | null = null;
  let fileInput: HTMLInputElement;
  let folderInput: HTMLInputElement;
  let selectedPeer: PeerInfo | null = null;
  let selectedMobileSession: MobileSession | null = null;
  let dragPeer = '';
  let notice = '';
  let noticeDeadline = 0;
  let noticeTimer = 0;
  let transfers: TransferProgress[] = [];
  let prepares: TransferPrepare[] = [];
  let failures: TransferFailure[] = [];
  let transferHistory: TransferHistoryEntry[] = [];
  let transferHistoryReady = false;
  let transferHistoryBusy = false;
  let transferHistoryRequestId = 0;
  let clearTransferHistoryOpen = false;
  let mobileSessions: MobileSession[] = [];
  let mobileTransfers: MobileTransferView[] = [];
  let mobileSessionId = '';
  let creatingMobileSession: Promise<string> | null = null;
  let transferActionBusy = '';
  let manualAddress = '';
  let savedNodeUrls: string[] = [];
  let monitorLocked = false;
  let monitorVisible = true;
  let settingsOpen = false;
  let terminalWorkspace: TerminalWorkspace;
  let terminalVisible = false;
  let terminalCount = 0;

  function openPeerTerminal(peer: PeerInfo) {
    if (!remoteMode && canOpenTerminal(peer)) {
      terminalWorkspace.openSession({ peerId: peer.node_id, name: peer.node_name });
    }
  }
  let mobileAccessOpen = false;
  // 打开二维码窗口时已知的网页会话；之后出现新会话说明有浏览器临时加入。
  let mobileAccessKnownSessions = new Set<string>();
  let addDeviceOpen = false;
  let clipboardHistoryOpen = false;
  let historyDetailLoading = false;
  let historyDetailToken = 0;
  let metricsPeerOpen = false;
  let clearHistoryOpen = false;
  // window.prompt 在部分桌面 WebView（macOS）上不可用，统一走内联输入对话框。
  let pathPrompt: { title: string; initial: string; resolve: (value: string | null) => void } | null = null;

  function askPath(title: string, initial: string): Promise<string | null> {
    return new Promise((resolve) => {
      pathPrompt = { title, initial, resolve };
    });
  }

  function resolvePathPrompt(value: string | null) {
    pathPrompt?.resolve(value);
    pathPrompt = null;
  }
  type TransferRoute = { target: string; peer: PeerInfo };
  type PendingUpload = TransferRoute & { files: File[]; offer: TransferOffer };
  const pendingUploads = new Map<string, PendingUpload>();
  let remoteMode = false;
  const uploading = new Set<string>();
  const watching = new Set<string>();
  const pendingMobileUploads = new Map<string, { files: File[]; completedItemIds: Set<string>; sessionId: string; offer: TransferOffer; base: string; failure?: TransferFailure }>();
  const uploadAttempts = new Map<string, number>();
  const uploadStopped = new Set<string>();
  // 用户取消时通知任务所属节点；离开界面只停止页面的准备工作。
  const prepareTasks = new Map<string, { controller: AbortController; cancel: (id: string) => Promise<unknown> }>();
  let mobileDownloading = new Set<string>();
  type PendingMobileCompletion = { directory: MobileDirectoryHandle; fileName: string };
  const pendingMobileCompletions = new Map<string, PendingMobileCompletion>();
  let mobileBrowserDownloadStarted = new Set<string>();
  const MAX_UPLOAD_ATTEMPTS = 6;
  const REMOTE_MANAGEMENT_CAPABILITY = 'management.remote.v1';
  const REMOTE_FILES_CAPABILITY = 'management.files.v1';
  const WS_RECONNECT_BASE_MS = 1500;
  const WS_RECONNECT_MAX_MS = 30000;
  let mobileReceiveDirectory: MobileDirectoryHandle | null = null;
  let mobileReceiveDirectoryName = '';
  let pageStopped = false;
  let wsReconnectTimer: number | undefined;
  let wsReconnectAttempt = 0;
  let wsPingTimer: number | undefined;
  let targetGeneration = 0;
  let refreshRequestId = 0;
  const FAILURE_RETENTION_MS = 24 * 60 * 60 * 1000;
  const MAX_FAILURES = 20;
  const dismissedFailures = new Map<string, number>();

  $: mobileAddress = mobileAddresses.find((address) => address.url === selectedAddress);
  $: mobileUrl = mobileAddress ? mobileAddress.url : '';
  $: secureMobileUrl = node?.mobile_addresses.find((address) => address.address === mobileAddress?.address)?.url;
  $: phoneTransfers = isWeb
    ? [
        ...transfers.map(asPhoneTransfer),
        ...mobileTransfers.map((transfer): PhoneTransfer => ({
          ...transfer.progress,
          direction: 'receive',
          web: transfer,
        })),
      ]
    : transfers.map(asPhoneTransfer);
  // 网页/手机页面是接收方：节点记录里的「发送到本会话」要按本会话视角显示。
  function asPhoneTransfer(transfer: TransferProgress): PhoneTransfer {
    const target = transfer.offer?.target_node_id;
    return {
      ...transfer,
      peer_name: transfer.direction === 'send'
        ? (target === node?.node_id ? node?.node_name : peers.find((peer) => peer.node_id === target)?.node_name)
        : undefined,
    };
  }

  $: phoneTransferHistory = isWeb
    ? transferHistory.map((entry) => transferHistoryForSessionView(entry, node?.node_id ?? entry.peer_node_id, node?.node_name ?? '接入节点'))
    : transferHistory;
  $: monitorMetrics = isMetricsStale(metrics) ? null : metrics;
  $: remoteMode = !!remoteTarget;
  $: settingsDisabled = !settingsReady || !node || settingsWrites.has(node.node_id);
  // 远程管理页的发送由目标机器选文件，需要目标节点声明对应的管理能力。
  $: managedFilesSupported = remoteMode && !!node?.capabilities.includes(REMOTE_FILES_CAPABILITY);
  $: if (!isWeb && !isMonitor) void refreshQr(mobileUrl);
  $: mobileTargets = node ? [{
    node_id: node.node_id,
    node_name: node.node_name,
    platform: node.platform,
    addresses: node.addresses,
    mobile_addresses: node.mobile_addresses,
    capabilities: node.capabilities,
    online: true,
    metrics_stale: false,
  } satisfies PeerInfo, ...peers] : peers;
  // 链路轨只表达「这台设备正在收发」；没有 offer 的本地合成行保持静态。
  $: activeLinkPeers = new Set(
    [...transfers, ...mobileTransfers.map((view) => view.progress)]
      .filter((transfer) => isActiveTransferStatus(transfer.status) && !!transfer.offer)
      .flatMap((transfer) => [transfer.offer?.source_node_id, transfer.offer?.target_node_id])
      .filter((id): id is string => !!id),
  );

  function failureRetryable(progress: TransferProgress): boolean {
    // 服务端刷新回来的进度可能没有方向，按 offer 的来源节点判断归属即可。
    if (progress.direction === 'receive') return false;
    if (pendingUploads.has(progress.transfer_id) || pendingMobileUploads.has(progress.transfer_id)) return true;
    if (mobileTransfers.some((view) => view.progress.transfer_id === progress.transfer_id)) return false;
    // 节点驱动的发送由源文件所在节点持有任务，可以按接收端的重试语义续传；
    // 建单失败（节点上没有记录）时不给无效的重试入口。
    return !isWeb
      && progress.offer?.source_node_id === node?.node_id
      && transfers.some((item) => item.transfer_id === progress.transfer_id);
  }

  function forgetPendingTransfer(transferId: string) {
    pendingUploads.delete(transferId);
    pendingMobileUploads.delete(transferId);
    uploadAttempts.delete(transferId);
    uploadStopped.delete(transferId);
  }

  function pruneFailures() {
    const now = Date.now();
    for (const [id, pending] of pendingMobileUploads) {
      if (pending.failure && now - pending.failure.failed_at_ms > FAILURE_RETENTION_MS) forgetPendingTransfer(id);
    }
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
    const pending = pendingMobileUploads.get(progress.transfer_id);
    const existing = failures.find((failure) => failure.progress.transfer_id === progress.transfer_id) ?? pending?.failure;
    const failure = { progress, failed_at_ms: existing?.failed_at_ms ?? Date.now(), retryable: failureRetryable(progress) };
    if (pending) pending.failure = failure;
    failures = [failure, ...failures.filter((entry) => entry.progress.transfer_id !== progress.transfer_id)];
    pruneFailures();
  }

  /** 定时刷新和推送共用请求顺序，晚到的历史不能覆盖当前节点或会话。 */
  async function loadTransferHistory(generation = targetGeneration, target = activeBaseUrl): Promise<void> {
    if (pageStopped || generation !== targetGeneration || (isWeb && !mobileSessionId)) return;
    const session = isWeb ? mobileSessionId : undefined;
    const requestId = ++transferHistoryRequestId;
    const current = () => !pageStopped && generation === targetGeneration
      && requestId === transferHistoryRequestId && (!isWeb || session === mobileSessionId);
    try {
      const entries = await getTransferHistory(session, target);
      if (!current()) return;
      transferHistory = entries;
      transferHistoryReady = true;
    } catch (error) {
      if (current()) console.error('读取传输历史失败', error);
    }
  }

  function resetTransferHistory(): void {
    transferHistoryRequestId += 1;
    transferHistory = [];
    transferHistoryReady = false;
    transferHistoryBusy = false;
    clearTransferHistoryOpen = false;
  }

  function requestClearTransferHistory() {
    if (!transferHistory.length) return;
    clearTransferHistoryOpen = true;
  }

  /** 打开历史条目的所在目录并定位条目；Android 由系统文件管理器尽力打开。 */
  async function openHistoryLocation(entry: TransferHistoryEntry): Promise<void> {
    if (remoteTarget || !transferHistory.includes(entry)) return;
    const generation = targetGeneration;
    const location = transferHistoryLocation(entry);
    if (!location) {
      showTransientNotice('该记录没有可用的本地路径', 3600);
      return;
    }
    try {
      if (isAndroidApp) {
        await invoke('android_reveal_path', { path: location.directory });
      } else {
        await invoke('open_transfer_location', { directory: location.directory, select: location.select ?? null });
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : error;
      if (!pageStopped && generation === targetGeneration) {
        showTransientNotice(typeof message === 'string' && message ? message : '无法打开所在目录', 6000);
      }
    }
  }

  /** 远程管理页的路径在目标机器上，打开目标的文件浏览视图查看位置。 */
  function viewHistoryLocation(entry: TransferHistoryEntry): void {
    if (!remoteTarget || !transferHistory.includes(entry)) return;
    const location = transferHistoryLocation(entry);
    if (!location) {
      showTransientNotice('该记录没有可用的路径', 3600);
      return;
    }
    browsePeer = null;
    browseTransfer = null;
    browseMode = 'view';
    browseStartPath = location.directory;
    browseOpen = true;
  }

  async function dismissFailure(failure: TransferFailure) {
    const id = failure.progress.transfer_id;
    const generation = targetGeneration;
    const mobile = pendingMobileUploads.has(id) || mobileTransfers.some((view) => view.progress.transfer_id === id);
    failures = failures.filter((item) => item.progress.transfer_id !== id);
    dismissedFailures.set(id, Date.now());
    if (failure.progress.error && notice === failure.progress.error) clearNotice();
    try {
      // 网页发送行只属于本页面；接收端独立管理自己的失败记录。
      if (mobile) await dismissMobileTransfer(id);
      else if (!isWeb) await dismissTransfer(id);
      forgetPendingTransfer(id);
    } catch (error) {
      // 记录本来就不在节点上（前端本地合成的失败行）时按移除成功处理。
      if (error instanceof ApiError && error.status === 404) {
        forgetPendingTransfer(id);
        return;
      }
      if (generation !== targetGeneration) return;
      dismissedFailures.delete(id);
      failures = [failure, ...failures.filter((item) => item.progress.transfer_id !== id)];
      const message = error instanceof Error ? error.message : '无法连接节点';
      showTransientNotice(`移除失败：${message}`, 6000);
    }
  }

  /**
   * 提示只从这里进出：每条提示替换上一条的定时器并记录到期时间，
   * 所以新提示不会被旧定时器清掉，也不存在永久停留的路径。
   */
  function showTransientNotice(message: string, timeout = 2400) {
    if (!message) {
      clearNotice();
      return;
    }
    notice = message;
    noticeDeadline = Date.now() + timeout;
    scheduleNoticeClear(timeout);
  }

  function scheduleNoticeClear(delay: number) {
    window.clearTimeout(noticeTimer);
    noticeTimer = window.setTimeout(clearNotice, Math.max(0, delay));
  }

  function clearNotice() {
    window.clearTimeout(noticeTimer);
    notice = '';
  }

  /** 后台节流会让定时器延后，回到前台按剩余时间重排，避免过期提示还挂在界面上。 */
  function resyncNotice() {
    if (!notice) return;
    const remaining = noticeDeadline - Date.now();
    if (remaining > 0) scheduleNoticeClear(remaining);
    else clearNotice();
  }

  onMount(() => {
    pageStopped = false;
    const resumeNotice = () => {
      if (document.visibilityState !== 'hidden') resyncNotice();
      // 从系统授权页返回后重新确认公共下载目录是否可用。
      if (isAndroidApp && document.visibilityState !== 'hidden') void refreshAndroidReceiveStorage();
    };
    document.addEventListener('visibilitychange', resumeNotice);
    window.addEventListener('focus', resumeNotice);
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
    let timer: number | undefined;
    let monitorPositionTimer: number | undefined;
    let pendingMonitorPosition: { x: number; y: number } | null = null;
    let monitorPositionWriteInFlight = false;
    let monitorPositionUnlisten: (() => void) | undefined;
    let updateStatusUnlisten: (() => void) | undefined;
    let androidNetworkUnlisten: (() => void) | undefined;
    if (isDesktopWindow) {
      void listen<UpdateStatus>('update-status', ({ payload }) => {
        if (!remoteMode) updateStatus = payload;
      }).then((unlisten) => {
        if (pageStopped) unlisten();
        else updateStatusUnlisten = unlisten;
      }).catch((error) => console.error('监听更新状态失败', error));
    }
    if (isAndroidApp) {
      void addPluginListener<{ network_handle: number }>('android-network', 'network_changed', () => {
        void invoke('android_network_changed').catch((error) => {
          console.error('通知 Rust 网络绑定变化失败', error);
        });
      }).then((listener) => {
        if (pageStopped) void listener.unregister();
        else androidNetworkUnlisten = () => { void listener.unregister(); };
      }).catch((error) => console.error('监听 Android 网络绑定变化失败', error));
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
        if (!pageStopped && pendingMonitorPosition && (pendingMonitorPosition.x !== position.x || pendingMonitorPosition.y !== position.y)) {
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
        if (pageStopped) unlisten();
        else monitorPositionUnlisten = unlisten;
      }).catch((error) => console.error('监听悬浮监控条移动失败', error));
    }
    const start = async () => {
      if (pageStopped) return;
      try {
        const base = await initApi();
        localBaseUrl = base;
        if (!activeBaseUrl) activeBaseUrl = base;
        if (isDesktopWindow) {
          remoteFileAccess = await invoke<boolean>('remote_file_access').catch(() => remoteFileAccess);
        }
        if (isAndroidApp) void refreshAndroidReceiveStorage();
        await refresh();
        if (pageStopped) return;
        connectWebSocket();
        timer = window.setInterval(() => {
          const generation = targetGeneration;
          void refresh().catch(async (error) => {
            if (pageStopped || generation !== targetGeneration) return;
            if (remoteTarget && await selectRemoteAddress(remoteTarget, activeBaseUrl)) return;
            if (generation !== targetGeneration) return;
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
    if (isWeb) mobileSessionId = window.sessionStorage.getItem('meshshare-mobile-session') ?? '';
    void start();
    return () => {
      pageStopped = true;
      document.removeEventListener('visibilitychange', resumeNotice);
      window.removeEventListener('focus', resumeNotice);
      window.clearTimeout(noticeTimer);
      if (timer) window.clearInterval(timer);
      for (const { controller } of prepareTasks.values()) controller.abort();
      if (wsReconnectTimer) window.clearTimeout(wsReconnectTimer);
      wsReconnectTimer = undefined;
      if (wsPingTimer) window.clearInterval(wsPingTimer);
      wsPingTimer = undefined;
      ws?.close();
      ws = null;
      if (monitorPositionTimer) window.clearTimeout(monitorPositionTimer);
      monitorPositionUnlisten?.();
      updateStatusUnlisten?.();
      androidNetworkUnlisten?.();
      void flushMonitorPosition();
    };
  });

  async function refreshAndroidReceiveStorage(): Promise<void> {
    if (!isAndroidApp) return;
    try {
      androidReceiveStorage = await invoke<AndroidReceiveStorage>('android_receive_storage');
    } catch {
      androidReceiveStorage = null;
    }
  }

  async function requestAndroidReceiveStorage(): Promise<void> {
    try {
      await invoke('android_request_receive_storage');
      showTransientNotice('在系统设置里允许「所有文件访问」后返回，文件会保存到 Download/MeshShare', 8000);
    } catch (error) {
      showTransientNotice(error instanceof Error ? error.message : '无法打开存储授权页面', 6000);
    }
  }

  async function refresh() {
    const requestId = ++refreshRequestId;
    const generation = targetGeneration;
    const target = activeBaseUrl;
    const current = () => !pageStopped && requestId === refreshRequestId && generation === targetGeneration;
    if (isMonitor) {
      const [nextNode, nextMetrics] = await Promise.all([getInfo(target), getMetrics(target)]);
      if (!current()) return;
      node = nextNode;
      metrics = nextMetrics;
      status = '节点在线';
      statusKind = 'ok';
      await refreshSettings(generation, target);
      return;
    }

    await Promise.all([
      refreshOverview(current, generation, target),
      refreshTransfers(current, generation, target),
    ]);
  }

  async function refreshOverview(current: () => boolean, generation: number, target: string) {
    const [nextNode, nextPeers, nextClipboard, nextMetrics, nextHistory, sessions] = await Promise.all([
      getInfo(target), getPeers(target), getClipboard(target), getMetrics(target), getClipboardHistory(target),
      isWeb ? [] : getMobileSessions(target),
    ]);
    if (!current()) return;
    mobileSessions = sessions;
    if (mobileAccessOpen && sessions.some((session) => !mobileAccessKnownSessions.has(session.session_id))) mobileAccessOpen = false;
    node = nextNode;
    if (!remoteMode) localNodeId = nextNode.node_id;
    peers = nextPeers.filter((peer) => peer.online);
    clipboard = nextClipboard;
    clipboardHistory = nextHistory.items;
    if (isWeb) clipboardHistorySettings = nextHistory.settings;
    if (selectedClipboardHistory && !clipboardHistory.some((item) => item.event_id === selectedClipboardHistory?.item.event_id)) {
      selectedClipboardHistory = null;
      historyDetailToken += 1;
      historyDetailLoading = false;
    }
    metrics = nextMetrics;
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
    if (!isWeb) await refreshSettings(generation, target);
  }

  /** 文件由持有源内容的一端推进，节点概览的可用性不影响传输。 */
  async function refreshTransfers(current: () => boolean, generation: number, target: string) {
    for (const [id, pending] of pendingUploads) {
      const progress = transfers.find((item) => item.transfer_id === id);
      if (progress && isActiveTransferStatus(progress.status)) void pollTransfer(id, pending);
    }
    const [native, views] = await Promise.all([
      isWeb ? [] : getTransfers(target),
      refreshMobileTransfers(current, target),
    ]);
    if (!current()) return;
    const nextTransfers = isWeb ? [] : [
      ...native, ...views.map(({ progress }) => ({ ...progress, direction: 'send' as const })),
    ];
    const refreshedTransfers = nextTransfers.map((next) => {
      const current = transfers.find((item) => item.transfer_id === next.transfer_id);
      return mergeTransferProgress(current, next);
    });
    transfers = [
      ...refreshedTransfers,
      ...transfers.filter((item) => isActiveTransferStatus(item.status) && !nextTransfers.some((next) => next.transfer_id === item.transfer_id)),
    ];
    for (const transfer of refreshedTransfers) observeFailure(transfer);
    pruneFailures();
    await loadTransferHistory(generation, target);
  }

  async function refreshMobileTransfers(current: () => boolean, target: string): Promise<MobileTransferView[]> {
    const views = isWeb ? await getMobileTransfersWithRecovery() : await getMobileTransfers(undefined, target);
    if (!current()) return views;
    mobileTransfers = views;
    if (isWeb) {
      for (const view of views) {
        observeFailure({ ...view.progress, direction: 'receive' });
        if (mobileReceiveDirectory) void downloadMobileTransfer(view);
        else {
          autoStartMobileDownloads(view);
          pruneMobileDownloadKeys(view);
        }
      }
    } else {
      const owner = remoteTarget?.node_id ?? localNodeId;
      for (const [id, pending] of pendingMobileUploads) {
        if (pending.offer.source_node_id !== owner) continue;
        pending.base = target;
        if (pending.failure && !failures.some((failure) => failure.progress.transfer_id === id)) failures = [pending.failure, ...failures];
      }
      for (const { progress } of views) {
        if (progress.status === 'accepted' || progress.status === 'uploading') void uploadMobileTransfer(progress.transfer_id);
        if (progress.status === 'ready' || (!isActiveTransferStatus(progress.status) && progress.status !== 'failed')) {
          forgetPendingTransfer(progress.transfer_id);
        }
      }
    }
    return views;
  }

  async function refreshSettings(generation = targetGeneration, target = activeBaseUrl) {
    if (pageStopped || generation !== targetGeneration || target !== activeBaseUrl) return;
    if (!node || settingsRead || settingsWrites.has(node.node_id)) return;
    const read = getSettings(target);
    settingsRead = read;
    try {
      const settings = await read;
      if (pageStopped || settingsRead !== read) return;
      applySettings(settings);
      settingsReady = true;
    } catch (error) {
      if (pageStopped || settingsRead !== read) return;
      settingsReady = false;
      status = error instanceof Error ? error.message : '无法读取节点设置';
      statusKind = 'warn';
    } finally {
      if (settingsRead === read) settingsRead = null;
    }
  }

  /** 同一节点串行写入，旧上下文的结果不更新当前界面。 */
  async function saveSettings(
    change: (target: string) => Promise<NodeSettings>,
    success: string | ((settings: NodeSettings) => string),
    failure: string,
  ): Promise<boolean> {
    if (pageStopped || !settingsReady || !node || settingsWrites.has(node.node_id)) return false;
    const nodeId = node.node_id;
    const generation = targetGeneration;
    const target = activeBaseUrl;
    settingsRead = null;
    settingsWrites = new Set(settingsWrites).add(nodeId);
    try {
      const settings = await change(target);
      if (pageStopped || generation !== targetGeneration) return false;
      applySettings(settings);
      showTransientNotice(typeof success === 'string' ? success : success(settings));
      return true;
    } catch (error) {
      if (!pageStopped && generation === targetGeneration) {
        settingsReady = false;
        showTransientNotice(error instanceof Error ? error.message : failure, 6000);
      }
      return false;
    } finally {
      const remaining = new Set(settingsWrites);
      remaining.delete(nodeId);
      settingsWrites = remaining;
      if (!settingsReady && node?.node_id === nodeId) void refreshSettings();
    }
  }

  function applySettings(settings: NodeSettings) {
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

  async function createSession(): Promise<string> {
    const session = await createMobileSession(webSessionLabel(navigator.userAgent));
    mobileSessionId = session.session_id;
    window.sessionStorage.setItem('meshshare-mobile-session', mobileSessionId);
    return mobileSessionId;
  }

  async function ensureMobileSession(): Promise<string> {
    if (mobileSessionId) return mobileSessionId;
    creatingMobileSession ??= createSession().finally(() => { creatingMobileSession = null; });
    return creatingMobileSession;
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
    ws?.close();
    ws = socket;
    socket.onopen = () => {
      if (socket !== ws) return;
      wsReconnectAttempt = 0;
      status = '已连接';
      statusKind = 'ok';
      const sendPing = () => {
        if (socket !== ws || socket.readyState !== WebSocket.OPEN) return;
        socket.send(JSON.stringify({ type: 'Ping', payload: { at_ms: Date.now() } }));
      };
      sendPing();
      if (wsPingTimer) window.clearInterval(wsPingTimer);
      wsPingTimer = window.setInterval(sendPing, 15000);
    };
    socket.onclose = () => {
      if (socket !== ws || pageStopped) return;
      ws = null;
      if (wsPingTimer) window.clearInterval(wsPingTimer);
      wsPingTimer = undefined;
      status = '连接暂时中断，正在恢复…';
      statusKind = 'warn';
      scheduleWebSocketReconnect();
    };
    socket.onerror = () => {
      if (socket !== ws || pageStopped) return;
      // onerror is followed by onclose in browsers. Keep one user-facing
      // recovery state instead of starting another connection here.
      status = '同步连接暂时不可用，正在恢复…';
      statusKind = 'warn';
    };
    socket.onmessage = (event) => {
      if (socket !== ws) return;
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
    if (type === 'Hello' || type === 'ClipboardUpdate' || type === 'ClipboardSnapshot') {
      void refresh();
      return;
    }
    if (type === 'MetricsUpdate') {
      const nextMetrics = payload.metrics;
      if (nextMetrics.node_id === node?.node_id) {
        metrics = nextMetrics;
      } else {
        peers = peers.map((peer) => peer.node_id === nextMetrics.node_id
          ? { ...peer, metrics: nextMetrics, metrics_stale: false }
          : peer);
        if (selectedMetricsPeer?.node_id === nextMetrics.node_id) {
          selectedMetricsPeer = { ...selectedMetricsPeer, metrics: nextMetrics, metrics_stale: false };
        }
      }
      return;
    }
    if (type === 'TransferHistoryDeleted') {
      void loadTransferHistory();
      return;
    }
    // 进度同时携带清单与接收状态。网页只展示页面发起的发送和会话下载。
    if (type === 'TransferProgress' && !isWeb) {
      const progress = payload.progress;
      const next = recordProgress(progress);
      if (!isActiveTransferStatus(next.status)) void loadTransferHistory();
    }
  }

  function openMobileAccess() {
    mobileAccessKnownSessions = new Set(mobileSessions.map((session) => session.session_id));
    mobileAccessOpen = true;
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
    const session = await ensureMobileSession();
    try {
      return await getMobileTransfers(session);
    } catch (error) {
      if (!(error instanceof ApiError && error.status === 404)) throw error;
      if (session === mobileSessionId) {
        mobileSessionId = '';
        resetTransferHistory();
        window.sessionStorage.removeItem('meshshare-mobile-session');
      }
      return getMobileTransfers(await ensureMobileSession());
    }
  }

  async function toggleSync() {
    const scope = remoteMode ? '目标节点的' : '';
    await saveSettings(
      (target) => patchSettings({ clipboard_sync_enabled: !syncEnabled }, target),
      (settings) => settings.clipboard_sync_enabled ? `${scope}剪贴板自动同步已恢复` : `${scope}剪贴板自动同步已暂停`,
      '无法更新剪贴板同步设置',
    );
  }

  async function toggleAutostart() {
    await saveSettings(
      (target) => patchSettings({ autostart_enabled: !autostartEnabled }, target),
      (settings) => settings.autostart_enabled ? '开机自启动已开启' : '开机自启动已关闭',
      '无法更新开机自启动设置',
    );
  }

  async function openProjectPage() {
    try {
      await invoke('open_project_page');
    } catch (error) {
      showTransientNotice(error instanceof Error ? error.message : String(error), 6000);
    }
  }

  async function setUpdateFrequency(next: UpdateFrequency) {
    await saveSettings(
      (target) => patchSettings({ update_frequency: next }, target),
      next === 'disabled' ? '自动更新已关闭' : `自动更新已设为${updateFrequencyLabel(next)}`,
      '无法更新自动更新设置',
    );
  }

  async function setReceiveMode(mode: ReceiveMode) {
    if (settingsDisabled) return;
    if (mode === 'auto' && !receiveSettings.default_path) {
      await chooseReceiveDirectory(mode);
      return;
    }
    await saveReceiveSettings(mode, receiveSettings.default_path);
  }

  async function chooseReceiveDirectory(mode?: ReceiveMode) {
    if (settingsDisabled) return;
    const generation = targetGeneration;
    if (remoteMode) {
      const path = await askPath('设置默认接收目录', receiveSettings.default_path ?? '');
      if (path === null || generation !== targetGeneration) return;
      await saveReceiveSettings(mode ?? receiveSettings.mode, path || receiveSettings.default_path);
      return;
    }
    let destination: string | string[] | null = null;
    try {
      destination = await open({ directory: true, multiple: false, title: '选择默认接收目录' });
    } catch {
      destination = null;
    }
    const path = Array.isArray(destination) ? destination[0] : destination;
    if (!path || generation !== targetGeneration) return;
    await saveReceiveSettings(mode ?? receiveSettings.mode, path);
  }

  async function saveReceiveSettings(mode: ReceiveMode, defaultPath?: string | null) {
    await saveSettings(
      (target) => patchSettings({ receive_settings: { mode, default_path: defaultPath ?? null } }, target),
      mode === 'auto' ? '已启用自动接收' : '已设为每次确认',
      '无法更新接收设置',
    );
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
    const generation = targetGeneration;
    const target = activeBaseUrl;
    const saved = await saveSettings(
      (base) => patchSettings({ clipboard_history_settings: { mode, value } }, base),
      ({ clipboard_history_settings: settings }) => settings.mode === 'count'
        ? `已保留最近 ${settings.count} 条剪贴板内容`
        : `已保留最近 ${settings.days} 天剪贴板内容`,
      '无法更新剪贴板历史设置',
    );
    if (saved) {
      await refreshClipboardHistory(generation, target).catch((error) => console.error('刷新剪贴板历史失败', error));
    }
  }

  async function refreshClipboardHistory(generation = targetGeneration, target = activeBaseUrl) {
    if (generation !== targetGeneration) return;
    const response = await getClipboardHistory(target);
    if (generation !== targetGeneration || pageStopped) return;
    if (isWeb) clipboardHistorySettings = response.settings;
    clipboardHistory = response.items;
    if (selectedClipboardHistory && !clipboardHistory.some((item) => item.event_id === selectedClipboardHistory?.item.event_id)) {
      selectedClipboardHistory = null;
      historyDetailToken += 1;
      historyDetailLoading = false;
    }
  }

  async function checkForUpdates() {
    await saveSettings(requestUpdateCheck, '已发起更新检查', '无法检查更新');
  }

  function selectMetricsPeer(peer: PeerInfo) {
    selectedMetricsPeer = peer;
    metricsPeerOpen = true;
  }

  function closeClipboardHistoryBrowser() {
    clipboardHistoryOpen = false;
    historyDetailToken += 1;
    historyDetailLoading = false;
    selectedClipboardHistory = null;
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

  async function confirmClearTransferHistory(): Promise<void> {
    if (!clearTransferHistoryOpen || transferHistoryBusy) return;
    const generation = targetGeneration;
    const target = activeBaseUrl;
    const session = isWeb ? mobileSessionId : undefined;
    const current = () => !pageStopped && generation === targetGeneration
      && (!isWeb || session === mobileSessionId);
    transferHistoryBusy = true;
    try {
      await clearTransferHistory(session, target);
      if (!current()) return;
      transferHistoryRequestId += 1;
      transferHistory = [];
      clearTransferHistoryOpen = false;
      showTransientNotice(isWeb ? '已清空本会话的传输历史' : '已清空传输历史，对端记录会同步删除');
    } catch (error) {
      if (current()) showTransientNotice(error instanceof Error ? error.message : '无法清空传输历史', 6000);
    } finally {
      if (current()) transferHistoryBusy = false;
    }
  }

  async function toggleMonitorLock() {
    await saveSettings(
      (target) => patchSettings({ monitor: { locked: !monitorLocked } }, target),
      (settings) => settings.monitor.locked ? '悬浮监控条已锁定，鼠标交互已忽略' : '悬浮监控条已解锁，可以拖动',
      '无法更新悬浮监控条状态',
    );
  }

  async function toggleMonitorVisibility() {
    await saveSettings(
      (target) => patchSettings({ monitor: { visible: !monitorVisible } }, target),
      (settings) => settings.monitor.visible ? '悬浮监控条已显示' : '悬浮监控条已隐藏',
      '无法更新悬浮监控条可见性',
    );
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
      showTransientNotice('已同步到在线设备');
    } catch (error) {
      showTransientNotice(error instanceof Error ? error.message : '剪贴板同步失败', 6000);
    }
  }

  function openClipboardHistoryBrowser() {
    clipboardHistoryOpen = true;
    void refreshClipboardHistory().catch((error) => console.error('刷新剪贴板历史失败', error));
  }

  async function selectClipboardHistory(item: ClipboardHistoryItem | null) {
    const token = ++historyDetailToken;
    if (!item) {
      selectedClipboardHistory = null;
      historyDetailLoading = false;
      return;
    }
    historyDetailLoading = true;
    try {
      const detail = await getClipboardHistoryDetail(item.event_id);
      if (token !== historyDetailToken) return;
      selectedClipboardHistory = detail;
    } catch (error) {
      if (token === historyDetailToken) {
        showTransientNotice(error instanceof Error ? error.message : '无法读取历史剪贴板内容', 6000);
      }
    } finally {
      if (token === historyDetailToken) historyDetailLoading = false;
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
      showTransientNotice('已恢复并同步剪贴板内容');
    } catch (error) {
      showTransientNotice(error instanceof Error ? error.message : '无法恢复剪贴板历史', 6000);
    } finally {
      clipboardHistoryBusy = false;
    }
  }

  async function clearClipboardHistoryItems() {
    if (clipboardHistoryBusy || !clipboardHistory.length) return;
    clipboardHistoryBusy = true;
    try {
      await clearClipboardHistory();
      clipboardHistory = [];
      historyDetailToken += 1;
      historyDetailLoading = false;
      selectedClipboardHistory = null;
      clearHistoryOpen = false;
      showTransientNotice('已清空本机剪贴板历史');
    } catch (error) {
      showTransientNotice(error instanceof Error ? error.message : '无法清空剪贴板历史', 6000);
    } finally {
      clipboardHistoryBusy = false;
    }
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
      showTransientNotice('已发起节点连接');
    } catch (error) {
      showTransientNotice(error instanceof Error ? error.message : '无法连接节点', 6000);
    }
  }

  async function readMobileClipboard() {
    try {
      clipboardDraft = await navigator.clipboard.readText();
      showTransientNotice('已读取本机剪贴板');
    } catch {
      showTransientNotice('浏览器未授权读取剪贴板，请手动粘贴', 6000);
    }
  }

  async function writeMobileClipboard() {
    try {
      await navigator.clipboard.writeText(clipboardDraft);
      showTransientNotice('已写入本机剪贴板');
    } catch {
      showTransientNotice('浏览器拒绝写入，请手动选择并复制', 6000);
    }
  }

  /** 桌面 WebView 可能没有异步剪贴板权限，退回选中复制。 */
  function fallbackCopy(value: string): boolean {
    const area = document.createElement('textarea');
    area.value = value;
    area.setAttribute('readonly', '');
    area.style.position = 'fixed';
    area.style.opacity = '0';
    document.body.appendChild(area);
    area.select();
    let copied = false;
    try {
      copied = document.execCommand('copy');
    } catch {
      copied = false;
    }
    area.remove();
    return copied;
  }

  async function copyAccessAddress() {
    const target = node?.addresses[0];
    if (!target) return;
    const value = target.url;
    try {
      await navigator.clipboard.writeText(value);
      showTransientNotice('已复制访问地址');
      return;
    } catch {
      const copied = fallbackCopy(value);
      showTransientNotice(copied ? '已复制访问地址' : '无法复制地址，请手动选择', copied ? 2400 : 6000);
    }
  }

  function startChoose(peer: PeerInfo, folder = false) {
    // 远程管理页的发送由目标机器选文件；本机发送仍由系统对话框给出绝对路径。
    if (remoteMode) {
      if (!managedFilesSupported) {
        showTransientNotice('目标节点不支持远程文件选择，请选择支持此功能的桌面节点', 6000);
        return;
      }
      browsePeer = peer;
      browseTransfer = null;
      browseMode = 'send';
      browseStartPath = '';
      browseOpen = true;
      return;
    }
    selectedPeer = peer;
    selectedMobileSession = null;
    // 桌面端由系统对话框给出绝对路径，节点直接读原文件，不再复制一份。
    if (isDesktopWindow) void pickAndSendPaths(peer, folder);
    else if (folder) folderInput?.click();
    else fileInput?.click();
  }

  async function pickAndSendPaths(peer: PeerInfo, folder: boolean) {
    const generation = targetGeneration;
    let picked: string | string[] | null = null;
    try {
      picked = folder
        ? await open({ directory: true, multiple: false, title: '选择要发送的文件夹' })
        : await open({ multiple: true, title: '选择要发送的文件' });
    } catch (error) {
      showTransientNotice(error instanceof Error ? error.message : '无法打开文件选择器', 6000);
      return;
    }
    const paths = typeof picked === 'string' ? [picked] : picked ?? [];
    if (!paths.length || generation !== targetGeneration) return;
    await sendPaths(peer, paths, false);
  }

  /**
   * 节点侧按路径准备并发送：本机节点与远程管理目标共用准备行、轮询、取消和失败语义。
   * 状态权威留在持有源文件的那台节点上，页面只展示。
   */
  async function sendPaths(peer: PeerInfo, paths: string[], managed: boolean) {
    const generation = targetGeneration;
    const api = outgoingPathsApi(activeBaseUrl, managed);
    let prepareId = '';
    const controller = new AbortController();
    try {
      const view = await api.create(peer.node_id, paths);
      if (generation !== targetGeneration || pageStopped) return;
      prepareId = view.transfer_id;
      prepareTasks.set(prepareId, { controller, cancel: api.cancel });
      prepares = [
        {
          prepare_id: prepareId,
          target_name: peer.node_name,
          item_count: view.total_items,
          total_bytes: view.total_bytes,
          hashed_bytes: 0,
          phase: 'hash',
        },
        ...prepares,
      ];
      while (!controller.signal.aborted) {
        await new Promise((resolve) => window.setTimeout(resolve, 600));
        const status = await api.status(prepareId).catch((error) => {
          if (error instanceof ApiError && error.status === 404) return null;
          throw error;
        });
        // 准备条目消失表示清单已发给接收端（或任务被取消）。
        if (!status) break;
        if (status.state === 'failed') {
          await api.cancel(prepareId).catch(() => undefined);
          throw new Error(status.error || '准备发送失败');
        }
        updatePrepare(prepareId, status.hashed_bytes);
      }
      if (controller.signal.aborted) return;
      showTransientNotice(managed ? '已交给目标机器' : '已交给本机节点', 3600);
      await refresh();
    } catch (error) {
      if (!controller.signal.aborted && generation === targetGeneration) {
        showTransientNotice(error instanceof Error ? error.message : '无法发送文件', 6000);
      }
    } finally {
      if (prepareId) finishPrepare(prepareId);
    }
  }

  async function confirmBrowse(paths: string[]) {
    const peer = browsePeer;
    const transfer = browseTransfer;
    const directory = browseMode === 'directory';
    closeBrowse();
    if (!paths.length) return;
    if (directory) {
      if (transfer) await acceptTransferAt(transfer, paths[0]);
      return;
    }
    if (peer) await sendPaths(peer, paths, true);
  }

  function closeBrowse() {
    browseOpen = false;
    browseStartPath = '';
    browsePeer = null;
    browseTransfer = null;
  }

  /** 目标机器未开启远程文件访问时，接收目录仍可手输。 */
  async function requestManualAcceptPath() {
    const transfer = browseTransfer;
    closeBrowse();
    if (!transfer) return;
    const path = await askPath('输入接收目录', settingsReady ? receiveSettings.default_path ?? '' : '');
    if (!path) {
      showTransientNotice('需要输入目标节点上的接收目录', 6000);
      return;
    }
    void acceptTransferAt(transfer, path);
  }

  async function setRemoteFileAccess(enabled: boolean) {
    if (remoteFileAccessBusy) return;
    remoteFileAccessBusy = true;
    try {
      remoteFileAccess = await invoke<boolean>('set_remote_file_access', { enabled });
      showTransientNotice(enabled ? '已允许远程浏览本机文件，其他设备可从本机选择发送' : '已关闭远程文件访问');
    } catch (error) {
      showTransientNotice(error instanceof Error ? error.message : '无法更新远程文件访问', 6000);
    } finally {
      remoteFileAccessBusy = false;
    }
  }

  async function acceptTerminalMigration() {
    if (!isDesktopWindow || remoteMode) return;
    const updated = await invoke<NodeInfo>('terminal_accept_migration');
    if (!remoteMode && node?.node_id === updated.node_id) node = updated;
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
    // 远程管理页面的文件只存在于目标机器上，本机拖放会被静默当作另一条来源。
    if (remoteMode) {
      showTransientNotice('远程管理页面的发送从目标机器选择文件', 6000);
      return;
    }
    const files = event.dataTransfer ? await filesFromDataTransfer(event.dataTransfer) : [];
    if (files.length) await sendFiles(peer, files);
  }

  async function onDropMobile(session: MobileSession, event: DragEvent) {
    event.preventDefault();
    dragPeer = '';
    if (remoteMode) {
      showTransientNotice('远程管理页面的发送从目标机器选择文件', 6000);
      return;
    }
    const files = event.dataTransfer ? await filesFromDataTransfer(event.dataTransfer) : [];
    if (files.length) await sendFilesToMobile(session, files);
  }

  const webContext = (): PeerWebContext | null => (isWeb
    ? { selfNodeId: node?.node_id ?? null, origin: window.location.origin, secure: window.location.protocol === 'https:' }
    : null);

  function targetBase(peer: PeerInfo): string | null {
    return peerTargetBase(peer, webContext());
  }

  function targetCandidates(peer: PeerInfo): string[] {
    return peerTargetCandidates(peer, webContext());
  }

  function describeTransferFailure(error: unknown, peerName: string, targets: string): string {
    if (error instanceof ApiError) return error.message;
    if (isTransportFailure(error)) return `无法连接 ${peerName}（${targets}）：请确认设备在同一网络且保持打开`;
    return error instanceof Error ? error.message : '文件传输失败';
  }

  async function onTransferRoute<T>(route: TransferRoute, perform: (base: string) => Promise<T>): Promise<T> {
    const peer = peers.find((candidate) => candidate.node_id === route.peer.node_id) ?? route.peer;
    return withPeerAddress(peer.node_id, [route.target, ...targetCandidates(peer)], async (base) => {
      const result = await perform(base);
      route.target = base;
      return result;
    });
  }

  async function createTransferOnCandidates(offer: TransferOffer, route: TransferRoute): Promise<TransferProgress> {
    const post = (base: string) => requestWithTimeout<TransferProgress>(
      '/api/v1/transfers',
      { method: 'POST', body: JSON.stringify(offer) },
      base,
    );
    try {
      return await onTransferRoute(route, post);
    } catch (error) {
      if (!isTransportFailure(error)) throw error;
      await new Promise((resolve) => window.setTimeout(resolve, 1200));
      return onTransferRoute(route, post);
    }
  }

  function resetTargetUi() {
    targetGeneration += 1;
    resetTransferHistory();
    settingsReady = false;
    settingsRead = null;
    closeBrowse();
    resolvePathPrompt(null);
    node = null;
    peers = [];
    clipboard = null;
    metrics = null;
    clipboardStatus = { session: 'unknown', available: false, retrying: true, message: '正在连接系统剪贴板…' };
    settingsOpen = false;
    mobileAccessOpen = false;
    addDeviceOpen = false;
    clearHistoryOpen = false;
    clipboardHistoryOpen = false;
    historyDetailToken += 1;
    historyDetailLoading = false;
    selectedClipboardHistory = null;
    metricsPeerOpen = false;
    selectedMetricsPeer = null;
    selectedPeer = null;
    selectedMobileSession = null;
    clipboardHistory = [];
    transfers = [];
    for (const { controller } of prepareTasks.values()) controller.abort();
    prepares = [];
    failures = [];
    dismissedFailures.clear();
    mobileSessions = [];
    mobileTransfers = [];
    mobileBrowserDownloadStarted = new Set();
    mobileAddresses = [];
    selectedAddress = '';
    mobileQr = null;
  }

  /**
   * 远程目标只有一层：进入其他设备直接替换当前目标，返回始终回到本机。
   * 目标设备列表包含本机节点，按 node_id 识别后等同于返回。
   */
  async function applyTargetContext(baseUrl: string, target: PeerInfo | null) {
    ws?.close();
    ws = null;
    if (wsReconnectTimer) window.clearTimeout(wsReconnectTimer);
    wsReconnectTimer = undefined;
    if (wsPingTimer) window.clearInterval(wsPingTimer);
    wsPingTimer = undefined;
    remoteTarget = target;
    activeBaseUrl = baseUrl;
    setApiTarget(baseUrl);
    resetTargetUi();
    const context = targetGeneration;
    status = target ? '正在连接目标节点…' : '正在返回本机…';
    statusKind = 'warn';
    wsReconnectAttempt = 0;
    await tick();
    if (context !== targetGeneration) return;
    await refresh().catch((error) => {
      if (context !== targetGeneration) return;
      status = error instanceof Error ? error.message : target ? '无法连接目标节点' : '无法连接本机节点';
      statusKind = 'error';
    });
    if (context !== targetGeneration) return;
    connectWebSocket();
  }

  async function openRemotePeer(peer: PeerInfo) {
    if (isWeb) return;
    if (peer.node_id === localNodeId) {
      await returnToLocal();
      return;
    }
    if (!peer.online) return;
    if (!peer.capabilities.includes(REMOTE_MANAGEMENT_CAPABILITY)) {
      showTransientNotice('目标节点不支持远程管理，请选择支持此功能的桌面节点', 6000);
      return;
    }
    await selectRemoteAddress(peer);
  }

  async function selectRemoteAddress(peer: PeerInfo, skipBase?: string): Promise<boolean> {
    const generation = targetGeneration;
    const candidates = targetCandidates(peer).filter((url) => url !== skipBase);
    try {
      const target = await withPeerAddress(peer.node_id, candidates, async (base) => base);
      if (generation === targetGeneration) await applyTargetContext(target, peer);
      return true;
    } catch {
      if (!skipBase && generation === targetGeneration) showTransientNotice('目标设备没有可访问地址', 6000);
      return false;
    }
  }

  async function returnToLocal() {
    if (!remoteMode) return;
    await applyTargetContext(localBaseUrl, null);
  }

  function updatePrepare(prepareId: string, hashedBytes: number) {
    prepares = prepares.map((item) => (item.prepare_id === prepareId ? { ...item, hashed_bytes: hashedBytes } : item));
  }

  function finishPrepare(prepareId: string) {
    prepareTasks.delete(prepareId);
    prepares = prepares.filter((item) => item.prepare_id !== prepareId);
  }

  function cancelPrepare(prepareId: string) {
    const task = prepareTasks.get(prepareId);
    if (!task) return;
    task.controller.abort();
    void task.cancel(prepareId).catch(() => undefined);
    finishPrepare(prepareId);
  }

  async function sendFiles(peer: PeerInfo, files: File[]) {
    // 远程管理页面的发送由目标机器读取文件，本机不再提供文件来源。
    if (remoteMode) {
      showTransientNotice('远程管理页面的发送从目标机器选择文件', 6000);
      return;
    }
    if (isNativeSender) await sendFilesThroughNode(peer, files);
    else await sendFilesFromPage(peer, files);
  }

  /**
   * 原生发送：把清单和源文件交给本机节点暂存，接收端确认后由节点上传。
   * 页面只负责选择文件和展示进度，窗口不在前台也不影响传输。
   */
  async function sendFilesThroughNode(peer: PeerInfo, files: File[]) {
    const source = node;
    const generation = targetGeneration;
    if (!source) {
      showTransientNotice('尚未连接本机节点，无法发送文件', 6000);
      return;
    }
    selectedPeer = null;
    const owner = activeBaseUrl;
    const controller = new AbortController();
    const offer = buildOffer(source, peer.node_id, files);
    const id = offer.transfer_id;
    const cancel = () => cancelOutgoingStaging(id, owner);
    let handedOff = false;
    try {
      await createOutgoingStaging(offer, owner);
      if (generation !== targetGeneration || pageStopped) return;
      prepareTasks.set(id, { controller, cancel });
      prepares = [{
        prepare_id: id, target_name: peer.node_name, item_count: offer.items.length,
        total_bytes: offer.total_bytes, hashed_bytes: 0, phase: 'stage',
      }, ...prepares];
      let stagedBytes = 0;
      for (const [index, item] of offer.items.entries()) {
        const url = apiUrl(`/api/v1/outgoing/${encodeURIComponent(id)}/items/${encodeURIComponent(item.item_id)}`, owner);
        await putFile(url, files[index], item.media_type, controller.signal);
        stagedBytes += item.size;
        updatePrepare(id, stagedBytes);
      }
      if (controller.signal.aborted || generation !== targetGeneration) return;
      // 从 start 起节点负责暂存的回收，响应丢失时接收方可能已经接受任务。
      finishPrepare(id);
      handedOff = true;
      const progress = await startOutgoingStaging(id, owner);
      if (generation !== targetGeneration) return;
      recordProgress({ ...progress, offer: progress.offer ?? offer, direction: 'send' });
      showTransientNotice('已交给本机节点', 3600);
    } catch (error) {
      if (!controller.signal.aborted && generation === targetGeneration) {
        recordSendFailure(offer, error instanceof Error ? error.message : '无法发送文件');
      }
    } finally {
      finishPrepare(id);
      if (!handedOff) await cancel().catch(() => undefined);
    }
  }

  function recordSendFailure(offer: TransferOffer, error: string) {
    observeFailure({
      transfer_id: offer.transfer_id, offer, status: 'failed', direction: 'send', error,
      awaiting_offer_reply: false, completed_items: 0, total_items: offer.items.length,
      completed_bytes: 0, total_bytes: offer.total_bytes,
    });
    showTransientNotice(error, 6000);
  }

  function recordProgress(progress: TransferProgress): TransferProgress {
    const id = progress.transfer_id;
    const next = mergeTransferProgress(transfers.find((item) => item.transfer_id === id), progress);
    transfers = [next, ...transfers.filter((item) => item.transfer_id !== id)];
    observeFailure(next);
    return next;
  }

  async function sendFilesFromPage(peer: PeerInfo, files: File[]) {
    const source = node;
    const candidates = targetCandidates(peer);
    if (!candidates[0] || !source) {
      showTransientNotice('目标设备没有可访问地址', 6000);
      return;
    }
    selectedPeer = null;
    const offer = buildOffer(source, peer.node_id, files);
    try {
      const pending: PendingUpload = { target: candidates[0], peer, files, offer };
      const created = await createTransferOnCandidates(offer, pending);
      if (created.status === 'failed') {
        throw new Error(created.error ?? '无法创建传输');
      }
      pendingUploads.set(offer.transfer_id, pending);
      transfers = [{ ...created, offer: created.offer ?? offer, direction: 'send' }, ...transfers];
      void pollTransfer(offer.transfer_id, pending);
      showTransientNotice('已发送传输请求', 3600);
    } catch (error) {
      const message = describeTransferFailure(error, peer.node_name, candidates.join('、'));
      recordSendFailure(offer, message);
    }
  }

  async function sendFilesToMobile(session: MobileSession, files: File[]) {
    if (!node) return;
    const offer = buildOffer(node, node.node_id, files);
    const base = activeBaseUrl;
    const generation = targetGeneration;
    selectedMobileSession = null;
    pendingMobileUploads.set(offer.transfer_id, { files, completedItemIds: new Set(), sessionId: session.session_id, offer, base });
    try {
      const view = await createMobileTransfer(session.session_id, offer, base);
      if (generation !== targetGeneration) return;
      recordProgress({ ...view.progress, direction: 'send' });
      showTransientNotice('已发送，等待网页端确认', 3600);
    } catch (error) {
      if (generation === targetGeneration) recordSendFailure(offer, error instanceof Error ? error.message : '无法发送到网页端');
      else forgetPendingTransfer(offer.transfer_id);
    }
  }

  async function uploadMobileTransfer(id: string) {
    const pending = pendingMobileUploads.get(id);
    if (!pending || pending.base !== activeBaseUrl || uploading.has(id) || uploadStopped.has(id)) return;
    uploading.add(id);
    try {
      for (const [index, item] of pending.offer.items.entries()) {
        // 已确认的项目跳过；节点对重复写入按幂等处理，丢失响应后可安全重试。
        if (pending.completedItemIds.has(item.item_id)) continue;
        const file = pending.files[index];
        if (!file) throw new Error(`找不到待上传文件：${item.relative_path}`);
        await uploadFileWithRetry(
          apiUrl(`/api/v1/mobile/transfers/${encodeURIComponent(id)}/items/${encodeURIComponent(item.item_id)}`, pending.base),
          file,
          item.media_type,
        );
        pending.completedItemIds.add(item.item_id);
      }
      uploadAttempts.delete(id);
      showTransientNotice('文件已准备好，网页端可以下载');
    } catch (error) {
      const attempts = (uploadAttempts.get(id) ?? 0) + 1;
      uploadAttempts.set(id, attempts);
      const message = error instanceof Error ? error.message : '网页端文件上传失败';
      if (attempts >= MAX_UPLOAD_ATTEMPTS) {
        uploadStopped.add(id);
        await request(`/api/v1/mobile/transfers/${encodeURIComponent(id)}/fail`, {
          method: 'POST',
          body: JSON.stringify({ error: message }),
        }, pending.base).catch(() => undefined);
        showTransientNotice(`${message}，已停止重试`);
      } else {
        showTransientNotice(`${message}，正在重试（${attempts}/${MAX_UPLOAD_ATTEMPTS}）`, 6000);
      }
    } finally {
      uploading.delete(id);
    }
  }

  async function acceptMobileTransfer(view: MobileTransferView) {
    if (!mobileSessionId) return;
    const sessionId = mobileSessionId;
    if (mobileDirectoryPickerAvailable() && !mobileReceiveDirectory && !(await chooseMobileReceiveDirectory())) return;
    try {
      await request(`/api/v1/mobile/transfers/${encodeURIComponent(view.progress.transfer_id)}/accept`, {
        method: 'POST',
        body: JSON.stringify({ session_id: sessionId }),
      });
    } catch (error) {
      showTransientNotice(error instanceof Error ? error.message : '无法接收文件', 6000);
      return;
    }
    if (!mobileDirectoryPickerAvailable()) showTransientNotice('已接受，正在等待发送方上传', 3000);
    await refresh();
  }

  function autoStartMobileDownloads(view: MobileTransferView) {
    if (view.progress.status !== 'ready' || mobileReceiveDirectory) return;
    for (const item of view.progress.offer?.items ?? []) {
      if (view.downloaded_item_ids.includes(item.item_id)) continue;
      startMobileBrowserDownload(view, item, true);
    }
  }

  function pruneMobileDownloadKeys(view: MobileTransferView) {
    const downloaded = view.downloaded_item_ids;
    if (!downloaded.length) return;
    const started = new Set(mobileBrowserDownloadStarted);
    let changed = false;
    for (const item of view.progress.offer?.items ?? []) {
      if (downloaded.includes(item.item_id) && started.delete(transferItemKey(view.progress.transfer_id, item.item_id))) changed = true;
    }
    if (changed) mobileBrowserDownloadStarted = started;
  }

  async function rejectMobileTransfer(view: MobileTransferView) {
    if (!mobileSessionId) return;
    await request(`/api/v1/mobile/transfers/${encodeURIComponent(view.progress.transfer_id)}/reject`, {
      method: 'POST',
      body: JSON.stringify({ session_id: mobileSessionId }),
    }).catch((error) => { showTransientNotice(error instanceof Error ? error.message : '无法拒绝文件', 6000); });
    await refresh();
  }

  async function cancelMobileTransfer(view: MobileTransferView) {
    if (!mobileSessionId) return;
    await request(`/api/v1/mobile/transfers/${encodeURIComponent(view.progress.transfer_id)}/cancel`, {
      method: 'POST',
      body: JSON.stringify({ session_id: mobileSessionId }),
    }).catch((error) => { showTransientNotice(error instanceof Error ? error.message : '无法取消文件', 6000); });
    await refresh();
  }

  /** 页面直发的状态由接收节点持有；一次只轮询同一任务一次，下次刷新继续。 */
  async function pollTransfer(id: string, route: TransferRoute) {
    if (watching.has(id)) return;
    watching.add(id);
    try {
      const progress = await onTransferRoute(route, (base) => requestWithTimeout<TransferProgress>(
        `/api/v1/transfers/${encodeURIComponent(id)}/status`, {}, base,
      ));
      if (pageStopped || !pendingUploads.has(id)) return;
      const next = recordProgress({ ...progress, direction: 'send' });
      if (next.status === 'accepted' || next.status === 'uploading') await uploadTransfer(id);
      if (!isActiveTransferStatus(next.status) && next.status !== 'failed') forgetPendingTransfer(id);
    } catch {
      // 接收节点暂时不可达时保留页面持有的源文件。
    } finally {
      watching.delete(id);
    }
  }

  async function uploadTransfer(id: string) {
    const pending = pendingUploads.get(id);
    if (!pending || uploading.has(id) || uploadStopped.has(id)) return;
    uploading.add(id);
    try {
      for (const [index, item] of pending.offer.items.entries()) {
        const file = pending.files[index];
        if (!file) throw new Error(`找不到待上传文件：${item.relative_path}`);
        await onTransferRoute(pending, (base) => uploadFileWithRetry(
          `${base}/api/v1/transfers/${encodeURIComponent(id)}/items/${encodeURIComponent(item.item_id)}`,
          file,
          item.media_type,
        ));
      }
      uploadAttempts.delete(id);
      showTransientNotice('文件发送完成');
    } catch (error) {
      const message = describeTransferFailure(error, pending.peer.node_name, pending.target);
      const attempts = (uploadAttempts.get(id) ?? 0) + 1;
      uploadAttempts.set(id, attempts);
      if (attempts >= MAX_UPLOAD_ATTEMPTS) {
        uploadStopped.add(id);
        const failed = `${message}，已停止重试`;
        const failPath = `/api/v1/transfers/${encodeURIComponent(id)}/fail`;
        await onTransferRoute(pending, (base) => requestWithTimeout(
          failPath, { method: 'POST', body: JSON.stringify({ error: failed }) }, base,
        )).catch(() => undefined);
        showTransientNotice(failed, 8000);
      } else {
        showTransientNotice(`${message}，连接恢复后将继续（${attempts}/${MAX_UPLOAD_ATTEMPTS}）`, 6000);
      }
    } finally {
      uploading.delete(id);
    }
  }

  async function retryFailure(failure: TransferFailure) {
    if (transferActionBusy) return;
    const generation = targetGeneration;
    const id = failure.progress.transfer_id;
    dismissedFailures.delete(id);
    transferActionBusy = id;
    try {
      const mobile = pendingMobileUploads.get(id);
      if (mobile) {
        const offer = { ...mobile.offer, transfer_id: randomId(), created_at_ms: Date.now() };
        const view = await createMobileTransfer(mobile.sessionId, offer, mobile.base);
        forgetPendingTransfer(id);
        pendingMobileUploads.set(offer.transfer_id, { ...mobile, offer, failure: undefined, completedItemIds: new Set() });
        if (generation !== targetGeneration) return;
        transfers = transfers.filter((item) => item.transfer_id !== id);
        recordProgress({ ...view.progress, direction: 'send' });
        if (mobileTransfers.some((view) => view.progress.transfer_id === id)) {
          await dismissMobileTransfer(id, mobile.base);
        }
      } else {
        const pending = pendingUploads.get(id);
        const peer = pending?.peer ?? peers.find((item) => item.node_id === failure.progress.offer?.target_node_id);
        const target = pending?.target ?? (peer && targetBase(peer));
        if (!peer || !target) throw new Error('目标设备当前不可用，无法重试');
        const progress = await onTransferRoute(pending ?? { target, peer }, (base) => requestWithTimeout<TransferProgress>(
          `/api/v1/transfers/${encodeURIComponent(id)}/retry`, { method: 'POST' }, base,
        ));
        if (generation !== targetGeneration) return;
        if (pending) {
          uploadAttempts.delete(id);
          uploadStopped.delete(id);
          recordProgress({ ...progress, offer: pending.offer, direction: 'send' });
          void pollTransfer(id, pending);
        } else await refresh();
      }
      failures = failures.filter((item) => item.progress.transfer_id !== id);
    } catch (error) {
      showTransientNotice(error instanceof Error ? error.message : '无法重试传输', 6000);
    } finally {
      transferActionBusy = '';
    }
  }

  class MobileCompletionError extends Error {
    constructor(message: string) {
      super(message);
      this.name = 'MobileCompletionError';
    }
  }
  type MobilePickerWindow = Window & {
    showDirectoryPicker?: (options?: { mode?: 'readwrite' }) => Promise<MobileDirectoryHandle>;
  };

  function mobileDirectoryPickerAvailable() {
    return isWeb && window.isSecureContext && !!navigator.locks && typeof (window as MobilePickerWindow).showDirectoryPicker === 'function';
  }

  async function chooseMobileReceiveDirectory() {
    if (!mobileDirectoryPickerAvailable()) {
      showTransientNotice(window.isSecureContext ? '当前浏览器不支持选择存储目录，可使用浏览器默认下载' : '当前页面将使用浏览器默认下载，选择目录请打开 HTTPS 地址', 6000);
      return false;
    }
    try {
      const picker = (window as MobilePickerWindow).showDirectoryPicker;
      if (!picker) return false;
      const directory = await picker.call(window, { mode: 'readwrite' });
      mobileReceiveDirectory = directory;
      mobileReceiveDirectoryName = directory.name;
      showTransientNotice(`已选择存储目录：${mobileReceiveDirectoryName}`);
      return true;
    } catch (error) {
      if (error instanceof DOMException && error.name === 'AbortError') return false;
      showTransientNotice(error instanceof Error ? error.message : '无法选择接收目录', 6000);
      return false;
    }
  }

  async function downloadMobileItem(view: MobileTransferView, item: TransferItem) {
    if (!mobileSessionId) throw new Error('网页会话已过期');
    const completionKey = transferItemKey(view.progress.transfer_id, item.item_id);
    let saved = pendingMobileCompletions.get(completionKey);
    if (!saved) {
      const directory = mobileReceiveDirectory;
      if (!directory) throw new Error('请先选择接收目录');
      const response = await fetch(mobileDownloadUrl(view.progress.transfer_id, item.item_id, mobileSessionId));
      if (!response.ok) throw await responseError(response);
      saved = await saveMobileResponse(directory, item.relative_path, response, item.size, item.sha256);
    } else {
      // 文件已落盘，只需补发完成通知。
      try {
        await saved.directory.getFileHandle(saved.fileName);
      } catch {
        pendingMobileCompletions.delete(completionKey);
        return downloadMobileItem(view, item);
      }
    }

    try {
      await completeMobileItem(view.progress.transfer_id, item.item_id, mobileSessionId);
      pendingMobileCompletions.delete(completionKey);
    } catch (error) {
      pendingMobileCompletions.set(completionKey, saved);
      const message = error instanceof Error ? error.message : '文件完成通知失败';
      throw new MobileCompletionError(`${message}，文件已保存，可稍后重试完成通知`);
    }
  }

  async function downloadMobileTransfer(view: MobileTransferView) {
    if (mobileDownloading.has(view.progress.transfer_id) || !mobileReceiveDirectory) return;
    if (view.progress.status !== 'ready') return;
    mobileDownloading = new Set(mobileDownloading).add(view.progress.transfer_id);
    try {
      const downloaded = new Set(view.downloaded_item_ids);
      for (const item of view.progress.offer?.items ?? []) {
        if (downloaded.has(item.item_id)) continue;
        await downloadMobileItem(view, item);
      }
      showTransientNotice('文件已保存到接收目录');
      await refresh();
    } catch (error) {
      const message = error instanceof Error ? error.message : '网页端文件保存失败';
      showTransientNotice(message, 6000);
      if (error instanceof MobileCompletionError) {
        await refresh();
        return;
      }
      await request(`/api/v1/mobile/transfers/${encodeURIComponent(view.progress.transfer_id)}/fail`, {
        method: 'POST',
        body: JSON.stringify({ error: message }),
      }).catch(() => undefined);
    } finally {
      mobileDownloading.delete(view.progress.transfer_id);
      mobileDownloading = mobileDownloading;
    }
  }

  async function chooseAndDownloadMobileTransfer(view: MobileTransferView) {
    if (!mobileDirectoryPickerAvailable()) return;
    if (!mobileReceiveDirectory && !(await chooseMobileReceiveDirectory())) return;
    await downloadMobileTransfer(view);
  }

  function startMobileBrowserDownload(view: MobileTransferView, item: TransferItem, auto = false) {
    if (!mobileSessionId || !['ready', 'completed'].includes(view.progress.status)) return;
    if (view.downloaded_item_ids.includes(item.item_id) && (auto || !view.retryable_download_item_ids?.includes(item.item_id))) return;
    const key = transferItemKey(view.progress.transfer_id, item.item_id);
    if (auto && mobileBrowserDownloadStarted.has(key)) return;
    const anchor = document.createElement('a');
    // 浏览器默认下载没有保存结果回调，节点会短时保留已提供的项目供重试。
    anchor.href = mobileDownloadUrl(view.progress.transfer_id, item.item_id, mobileSessionId, true);
    anchor.download = item.name;
    anchor.rel = 'noreferrer';
    document.body.append(anchor);
    anchor.click();
    anchor.remove();
    mobileBrowserDownloadStarted = new Set(mobileBrowserDownloadStarted).add(key);
    showTransientNotice(`正在下载 ${item.name}`, 3000);
  }

  async function acceptTransfer(transfer: TransferProgress) {
    if (!transfer.offer || transferActionBusy) return;
    const generation = targetGeneration;
    if (remoteMode) {
      const defaultPath = settingsReady ? receiveSettings.default_path ?? '' : '';
      if (!managedFilesSupported) {
        const path = await askPath('输入接收目录', defaultPath);
        if (!path) {
          showTransientNotice('需要输入目标节点上的接收目录', 6000);
          return;
        }
        await acceptTransferAt(transfer, path);
        return;
      }
      browseTransfer = transfer;
      browsePeer = null;
      browseMode = 'directory';
      browseStartPath = defaultPath;
      browseOpen = true;
      return;
    }
    let destination: string | string[] | null = null;
    let path: string | undefined;
    if (isAndroidApp) {
      try {
        path = (await invoke<AndroidReceiveStorage>('android_receive_storage')).directory;
      } catch (error) {
        showTransientNotice(error instanceof Error ? error.message : '无法准备 Android 接收目录', 6000);
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
      showTransientNotice('当前环境无法选择接收目录', 6000);
      return;
    }
    if (generation === targetGeneration) await acceptTransferAt(transfer, path);
  }

  /** 接收目录确定后提交接受；远程目标的目录存在性与权限由目标节点校验。 */
  async function acceptTransferAt(transfer: TransferProgress, path: string) {
    if (transferActionBusy) return;
    transferActionBusy = transfer.transfer_id;
    try {
      await request<TransferProgress>(`/api/v1/transfers/${encodeURIComponent(transfer.transfer_id)}/accept`, {
        method: 'POST',
        body: JSON.stringify({ destination: path }),
      });
      showTransientNotice(
        isAndroidApp && !remoteMode
          ? '已接受，文件将保存到 MeshShare 应用目录'
          : remoteMode
            ? '已接受，等待目标机器发送文件'
            : '已接受，等待文件直传',
        3600,
      );
    } catch (error) {
      showTransientNotice(error instanceof Error ? error.message : '无法接受传输', 6000);
    } finally {
      transferActionBusy = '';
    }
  }

  async function cancelTransfer(transfer: TransferProgress) {
    if (transferActionBusy) return;
    transferActionBusy = transfer.transfer_id;
    // 页面直发的任务在目标节点上，沿用建单时验证过的路由；本机任务直接取消。
    const pending = pendingUploads.get(transfer.transfer_id);
    const cancel = (base: string) => request(
      `/api/v1/transfers/${encodeURIComponent(transfer.transfer_id)}/cancel`,
      { method: 'POST' },
      base,
    );
    try {
      if (pending) await onTransferRoute(pending, cancel);
      else await cancel(activeBaseUrl);
      await refresh();
    } catch (error) {
      showTransientNotice(error instanceof Error ? error.message : '无法取消传输', 6000);
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
      prepares={prepares}
      {failures}
      transferHistory={phoneTransferHistory}
      {transferHistoryReady}
      {transferHistoryBusy}
      onClearTransferHistory={requestClearTransferHistory}
      {clipboard}
      bind:draft={clipboardDraft}
      historyCount={clipboardHistory.length}
      {metrics}
      canChooseDirectory={mobileDirectoryPickerAvailable()}
      receiveDirectoryName={mobileReceiveDirectoryName}
      actionBusyId={transferActionBusy}
      downloadingIds={[...mobileDownloading]}
      startedDownloadKeys={[...mobileBrowserDownloadStarted]}
      onSendFile={(peer) => startChoose(peer)}
      onSendFolder={(peer) => startChoose(peer, true)}
      receiveStorage={androidReceiveStorage}
      onRequestReceiveStorage={requestAndroidReceiveStorage}
      onAcceptTransfer={(transfer) => transfer.web ? acceptMobileTransfer(transfer.web) : acceptTransfer(transfer)}
      onRejectTransfer={(transfer) => transfer.web ? rejectMobileTransfer(transfer.web) : rejectTransfer(transfer)}
      onCancelTransfer={(transfer) => transfer.web ? cancelMobileTransfer(transfer.web) : cancelTransfer(transfer)}
      onCancelPrepare={cancelPrepare}
      onSaveToDirectory={(transfer) => transfer.web && chooseAndDownloadMobileTransfer(transfer.web)}
      onStartDownload={(transfer, item) => transfer.web && startMobileBrowserDownload(transfer.web, item)}
      onRetryFailure={retryFailure}
      onDismissFailure={dismissFailure}
      onPublish={publishClipboard}
      onReadLocal={readMobileClipboard}
      onWriteLocal={writeMobileClipboard}
      onChooseDirectory={chooseMobileReceiveDirectory}
      onOpenHistory={openClipboardHistoryBrowser}
      onOpenLocation={openHistoryLocation}
    />
  {:else}
    <AppHeader
      {node}
      remote={remoteMode}
      {status}
      {statusKind}
      {syncEnabled}
      {autostartEnabled}
      {settingsReady}
      {settingsDisabled}
      onSync={toggleSync}
      onAutostart={toggleAutostart}
      onSettings={() => (settingsOpen = true)}
      {terminalCount}
      {terminalVisible}
      onTerminal={() => { if (terminalVisible) terminalVisible = false; else terminalWorkspace.showWorkspace(); }}
      onOpenProject={openProjectPage}
      onBack={returnToLocal}
      onCopyAddress={copyAccessAddress}
    />
    {#if isDesktopWindow && node && !clipboardStatus.available}
      <div class="platform-notice" role="status">{clipboardStatus.message ?? '系统剪贴板暂不可用'}{clipboardStatus.retrying ? '（正在重试）' : ''}；共享内容仍可在设备间同步。</div>
    {/if}
    <TerminalWorkspace bind:this={terminalWorkspace} bind:visible={terminalVisible} bind:count={terminalCount} peers={remoteMode ? null : peers} />
    <main class="app-main" hidden={terminalVisible}>
      <DevicePanel
        peers={peers}
        homeNodeId={remoteMode ? localNodeId : ''}
        activePeerIds={activeLinkPeers}
        {mobileSessions}
        savedAddresses={savedNodeUrls}
        draggingKey={dragPeer}
        addOpen={addDeviceOpen}
        bind:manualAddress
        onToggleAdd={() => (addDeviceOpen = !addDeviceOpen)}
        onConnect={connectManualPeer}
        onConnectSaved={connectSavedPeer}
        onOpenPeer={openRemotePeer}
        onTerminal={remoteMode ? null : openPeerTerminal}
        onMobileAccess={openMobileAccess}
        onSendPeer={(peer) => startChoose(peer)}
        onSendPeerFolder={(peer) => startChoose(peer, true)}
        onDetails={selectMetricsPeer}
        onSendMobile={(session) => startChooseMobile(session)}
        onSendMobileFolder={(session) => startChooseMobile(session, true)}
        onDragEnter={(key) => (dragPeer = key)}
        onDragLeave={(key) => { if (dragPeer === key) dragPeer = ''; }}
        onDropPeer={onDrop}
        onDropMobile={onDropMobile}
        allowFolderSend={!remoteMode}
      />
      <HardwarePanel
        metrics={metrics}
        {monitorLocked}
        {monitorVisible}
        {settingsDisabled}
        canClickThrough={monitorCapabilities.can_click_through}
        onToggleLock={toggleMonitorLock}
        onToggleVisible={toggleMonitorVisibility}
      />
      <ClipboardPanel
        {clipboard}
        bind:draft={clipboardDraft}
        historyCount={clipboardHistory.length}
        onPublish={publishClipboard}
        onOpenHistory={openClipboardHistoryBrowser}
      />
      <TransferPanel {transfers} {prepares} {failures} {transferHistory} {transferHistoryBusy} {remoteMode} onAccept={acceptTransfer} onReject={rejectTransfer} onRetry={retryFailure} onDismissFailure={dismissFailure} onCancelPrepare={cancelPrepare} onClearTransferHistory={requestClearTransferHistory} onOpenLocation={openHistoryLocation} onViewLocation={viewHistoryLocation} />
    </main>
    <SettingsDialog
      open={settingsOpen}
      {node}
      {updateFrequency}
      {updateStatus}
      {settingsDisabled}
      localMode={!remoteMode}
      {receiveSettings}
      historySettings={clipboardHistorySettings}
      {remoteFileAccess}
      {remoteFileAccessBusy}
      onFrequency={setUpdateFrequency}
      onCheckUpdates={checkForUpdates}
      onReceiveMode={setReceiveMode}
      onChooseDirectory={() => chooseReceiveDirectory()}
      onHistoryMode={setClipboardHistoryMode}
      onHistoryValue={setClipboardHistoryValue}
      onRemoteFileAccess={setRemoteFileAccess}
      onAcceptTerminalMigration={isDesktopWindow && !remoteMode ? acceptTerminalMigration : null}
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
    <MetricsDetailDialog
      open={metricsPeerOpen && !!selectedMetricsPeer}
      peer={selectedMetricsPeer}
      onClose={() => (metricsPeerOpen = false)}
    />
    <RemoteFileBrowser
      open={browseOpen}
      mode={browseMode}
      targetBase={activeBaseUrl}
      targetLabel={remoteTarget?.node_name ?? ''}
      startPath={browseStartPath}
      onConfirm={confirmBrowse}
      onFallback={browseMode === 'directory' ? requestManualAcceptPath : null}
      onClose={closeBrowse}
    />
    <PromptDialog
      open={!!pathPrompt}
      title={pathPrompt?.title ?? ''}
      initialValue={pathPrompt?.initial ?? ''}
      onSubmit={(value) => resolvePathPrompt(value)}
      onClose={() => resolvePathPrompt(null)}
    />
  {/if}
  <ClipboardHistoryDialog
    open={clipboardHistoryOpen}
    items={clipboardHistory}
    selected={selectedClipboardHistory}
    detailLoading={historyDetailLoading}
    busy={clipboardHistoryBusy}
    retentionLabel={clipboardHistoryRetentionLabel(clipboardHistorySettings)}
    onSelect={selectClipboardHistory}
    onRestore={restoreClipboardHistoryItem}
    onClear={requestClearHistory}
    onClose={closeClipboardHistoryBrowser}
  />
  <ConfirmDialog
    open={clearHistoryOpen}
    title="清空剪贴板历史"
    message="本机保存的全部剪贴板历史会被删除，已同步到其他设备的内容不受影响。"
    confirmLabel="清空"
    onConfirm={confirmClearHistory}
    onCancel={cancelClearHistory}
  />
  <ConfirmDialog
    open={clearTransferHistoryOpen}
    title="清空传输历史"
    message={isWeb
      ? '本机会话的传输历史会被删除，浏览器已下载的文件不受影响。'
      : '本机保存的最近传输记录会被删除；桌面传输会同步删除对端节点上同一任务的记录。进行中的任务和失败任务不受影响。'}
    confirmLabel="清空"
    onConfirm={confirmClearTransferHistory}
    onCancel={() => (clearTransferHistoryOpen = false)}
  />
  {#if notice}
    <div class="notice" role="status">
      <span>{notice}</span>
      <button type="button" class="notice-close" aria-label="关闭提示" on:click={clearNotice}>×</button>
    </div>
  {/if}
</div>

<input bind:this={fileInput} class="hidden-input" type="file" multiple on:change={onFileInput} />
<input bind:this={folderInput} class="hidden-input" type="file" webkitdirectory multiple on:change={onFileInput} />
