<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { open } from '@tauri-apps/plugin-dialog';
  import { invoke, isTauri } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { buildOffer, uploadItems, randomId, uploadFileWithRetry } from './upload';
  import { snapshotRead } from './snapshotRead';
  import { filesFromDataTransfer } from './drop';
  import { HEADLESS_CAPABILITY, REMOTE_MANAGEMENT_CAPABILITY, canOpenRemoteSettings, peerTargetBase, peerTargetCandidates, type PeerWebContext } from './peers';
  import { ApiError, NODE_PROTOCOL_VERSION, responseError, isTransportFailure, withPeerAddress, apiUrl, cancelOutgoingStaging, clearClipboardHistory, clearTransferHistory, completeMobileItem, createMobileSession, createMobileTransfer, sendManagedPathsToMobile, createOutgoingStaging, dismissMobileTransfer, dismissTransfer, getClipboard, isNewerClipboardSnapshot, getClipboardHistory, getClipboardHistoryDetail, getInfo, getMetrics, getMobileSessions, getMobileTransfers, getPeers, getSettings, getTransferHistory, getTransfers, initApi, mobileDownloadUrl, patchSettings, request, requestUpdateCheck, restartService, requestWithTimeout, outgoingPathsApi, restoreClipboardHistory, setApiTarget, startOutgoingStaging, websocketUrl, type ManagedRequestTarget, type RequestTarget } from './api';
  import { saveMobileResponse, type MobileDirectoryHandle } from './mobileSave';
  import { mergeTransferProgress } from './transferProgress';
  import type { AddressInfo, ClipboardHistoryDetail, ClipboardHistoryItem, ClipboardHistoryMode, ClipboardHistorySettings, ClipboardSnapshot, ClipboardStatus, HardwareMetrics, MobileSession, MobileTransferView, MonitorCapabilities, NodeInfo, NodeSettings, PeerInfo, PhoneTransfer, ReceiveMode, ReceiveSettings, TransferFailure, TransferHistoryEntry, TransferItem, TransferOffer, TransferPrepare, TransferProgress, UpdateFrequency, UpdateStatus, WireMessage } from './types';
  import { clipboardHistoryRetentionLabel, isActiveTransferStatus, transferHistoryForSessionView, transferHistoryLocation, transferItemKey, updateFrequencyLabel, webSessionLabel } from './format';
  import type TerminalWorkspace from './components/TerminalWorkspace.svelte';
  import { canOpenTerminal } from './terminal';
  import ClipboardHistoryDialog from './components/ClipboardHistoryDialog.svelte';
  import ConfirmDialog from './components/ConfirmDialog.svelte';

  const inTauri = isTauri();
  const isAndroidApp = inTauri && /Android/i.test(navigator.userAgent);
  // 浏览器访问（根地址或 /mobile）渲染网页临时加入界面。
  const isWeb = !inTauri;
  const isDesktopWindow = inTauri && !isAndroidApp;
  const isPhone = isWeb || isAndroidApp;
  const desktopUi = isDesktopWindow ? import('./desktopUi') : null;
  const mobileUi = isPhone ? import('./components/MobileApp.svelte') : null;
  // 原生界面可以把源文件交给本机节点，上传因此不再依赖页面存活。
  const isNativeSender = isDesktopWindow || isAndroidApp;
  document.documentElement.dataset.shell = isPhone ? (isWeb ? 'web' : 'android') : 'desktop';
  let node: NodeInfo | null = null;
  let activeBaseUrl = '';
  let remoteTarget: PeerInfo | null = null;
  let remoteAddress = '';
  let localBaseUrl = '';
  let localNodeId = '';
  let peers: PeerInfo[] = [];
  let clipboard: ClipboardSnapshot | null = null;
  let clipboardReadError = '';
  let clipboardRequest: AbortController | undefined;
  let clipboardHistory: ClipboardHistoryItem[] = [];
  const defaultHistory: ClipboardHistorySettings = { mode: 'count', count: 50, days: 30 };
  let webHistorySettings = defaultHistory;
  let selectedClipboardHistory: ClipboardHistoryDetail | null = null;
  let clipboardHistoryBusy = false;
  const loadingClipboard: ClipboardStatus = { session: 'unknown', available: false, retrying: true, message: '正在连接系统剪贴板…' };
  type AndroidReceiveStorage = { directory: string; public_download: boolean };
  let androidReceiveStorage: AndroidReceiveStorage | null = null;
  let metrics: HardwareMetrics | null = null;
  let selectedMetricsPeer: PeerInfo | null = null;
  let clipboardDraft = '';
  let clipboardDraftEdited = false;
  let clipboardDraftRevision = 0;
  let status = '正在连接…';
  let statusKind: 'ok' | 'warn' | 'error' = 'warn';
  let mobileAddresses: AddressInfo[] = [];
  let selectedAddress = '';
  let mobileQr: { url: string; image: string } | null = null;
  const UNKNOWN_MONITOR_CAPABILITIES: MonitorCapabilities = { backend: 'unknown', can_persist_position: false, can_click_through: false, can_keep_above: false };
  let settingsSnapshot: NodeSettings | null = null;
  const defaultReceive: ReceiveSettings = { mode: 'confirm' };
  $: settingsReady = settingsSnapshot !== null;
  $: syncEnabled = settingsSnapshot?.clipboard_sync_enabled ?? true;
  $: autostartEnabled = settingsSnapshot?.autostart_enabled ?? true;
  $: receiveSettings = settingsSnapshot?.receive_settings ?? defaultReceive;
  $: clipboardHistorySettings = settingsSnapshot?.clipboard_history_settings ?? webHistorySettings;
  $: clipboardStatus = settingsSnapshot?.clipboard_status ?? loadingClipboard;
  $: serviceControlError = settingsSnapshot?.service_control_error ?? '';
  $: monitorLocked = settingsSnapshot?.monitor.locked ?? false;
  $: monitorVisible = settingsSnapshot?.monitor.visible ?? true;
  $: monitorCapabilities = settingsSnapshot?.monitor.capabilities ?? UNKNOWN_MONITOR_CAPABILITIES;
  let settingsRead: Promise<NodeSettings> | null = null;
  let settingsWrites = new Map<string, Promise<NodeSettings | void>>();
  let remoteFileAccess = false;
  let remoteFileAccessBusy = false;
  let browseOpen = false;
  let browseMode: 'send' | 'directory' | 'view' = 'send';
  let browseStartPath = '';
  type BrowseRecipient = { peer: PeerInfo } | { mobile: MobileSession };
  let browseRecipient: BrowseRecipient | null = null;
  let browseTransfer: TransferProgress | null = null;
  let browseReceiveSettings: { scope: SettingsScope; mode?: ReceiveMode } | null = null;
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
  let settingsOpen = false;
  type RemoteSettingsSession = {
    peer: PeerInfo;
    node: NodeInfo | null;
    target: ManagedRequestTarget | null;
    address: string;
    snapshot: NodeSettings | null;
    error: string;
  };
  type SettingsScope = {
    target: RequestTarget;
    nodeId: string;
    snapshot: NodeSettings;
    isCurrent: () => boolean;
    dialog: boolean;
  };
  let settingsSession = 0;
  let settingsTrigger: HTMLButtonElement | null = null;
  let remoteSettings: RemoteSettingsSession | null = null;
  let remoteSettingsRead: Promise<NodeSettings> | null = null;
  let settingsFeedback: { text: string; error: boolean } | null = null;
  let restartBusy = false;
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
  let clipboardHistoryOpen = false;
  let historyDetailLoading = false;
  let historyDetailToken = 0;
  let metricsPeerOpen = false;
  let clearHistoryOpen = false;
  // window.prompt 在部分桌面 WebView（macOS）上不可用，统一走内联输入对话框。
  let pathPrompt: { title: string; initial: string; settings: boolean; resolve: (value: string | null) => void } | null = null;

  function askPath(title: string, initial: string, settings = false): Promise<string | null> {
    return new Promise((resolve) => {
      pathPrompt = { title, initial, settings, resolve };
    });
  }

  function resolvePathPrompt(value: string | null) {
    pathPrompt?.resolve(value);
    pathPrompt = null;
  }
  type TransferRoute = { target: string; peer: PeerInfo };
  type UploadFiles = { files: File[]; offer: TransferOffer; completedItemIds: Set<string>; attempts: number; controller?: AbortController };
  type PendingUpload = TransferRoute & UploadFiles;
  type MobileUpload = UploadFiles & { sessionId: string; readonly base: string; failure?: TransferFailure };
  const pendingUploads = new Map<string, PendingUpload>();
  let remoteMode = false;
  const watching = new Set<string>();
  const pendingMobileUploads = new Map<string, MobileUpload>();
  let localMobileRead: Promise<MobileTransferView[]> | undefined;
  // 用户取消时通知任务所属节点；离开界面只停止页面的准备工作。
  const prepareTasks = new Map<string, { controller: AbortController; cancel: (id: string) => Promise<unknown> }>();
  let mobileDownloading = new Set<string>();
  type PendingMobileCompletion = { directory: MobileDirectoryHandle; fileName: string };
  const pendingMobileCompletions = new Map<string, PendingMobileCompletion>();
  let mobileBrowserDownloadStarted = new Set<string>();
  const MAX_UPLOAD_ATTEMPTS = 6;
  const MOBILE_SESSION_KEY = 'meshshare-mobile-session';
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
  let rediscoveringGeneration: number | undefined;
  const nodeRead = snapshotRead(refreshTarget);
  const clipboardRead = snapshotRead(readSharedClipboard);
  const historyRead = snapshotRead(loadTransferHistory);
  const refresh = () => nodeRead.refresh(true);
  const FAILURE_RETENTION_MS = 24 * 60 * 60 * 1000;
  const MAX_FAILURES = 20;
  const dismissedFailures = new Map<string, number>();

  function resetSnapshotReads() {
    nodeRead.reset();
    clipboardRead.reset();
    historyRead.reset();
    clipboardRequest?.abort();
  }

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
  $: remoteMode = !!remoteTarget;
  $: headless = remoteMode && !!(node ?? remoteTarget)?.capabilities.includes(HEADLESS_CAPABILITY);
  $: settingsDisabled = !settingsReady || !node || settingsWrites.has(node.node_id);
  $: dialogNode = remoteSettings ? remoteSettings.node : node;
  $: dialogSnapshot = remoteSettings ? remoteSettings.snapshot : settingsSnapshot;
  $: dialogSettingsDisabled = !dialogSnapshot || !dialogNode || settingsWrites.has(dialogNode.node_id);
  $: dialogRemote = remoteSettings && {
    name: remoteSettings.node?.node_name ?? remoteSettings.peer.node_name,
    address: !remoteSettings.target && remoteSettings.address ? '上次连接 ' + remoteSettings.address : remoteSettings.address,
    platform: remoteSettings.node?.platform ?? remoteSettings.peer.platform,
    headless: (remoteSettings.node ?? remoteSettings.peer).capabilities.includes(HEADLESS_CAPABILITY),
  };
  // 远程管理页的发送由目标机器选文件，需要目标节点声明对应的管理能力。
  $: managedFilesSupported = remoteMode && !!node?.capabilities.includes(REMOTE_FILES_CAPABILITY);
  $: if (isDesktopWindow && mobileAccessOpen && mobileQr?.url !== mobileUrl) void refreshQr(mobileUrl);
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
    (pendingUploads.get(transferId) ?? pendingMobileUploads.get(transferId))?.controller?.abort();
    pendingUploads.delete(transferId);
    pendingMobileUploads.delete(transferId);
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
    browseRecipient = null;
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
    let timer: number | undefined;
    const eventUnlisteners: (() => void)[] = [];
    if (isDesktopWindow) {
      const saveUnlisten = (unlisten: () => void) => {
        if (pageStopped) unlisten();
        else eventUnlisteners.push(unlisten);
      };
      void listen<UpdateStatus>('update-status', ({ payload }) => {
        if (!remoteTarget && settingsSnapshot) settingsSnapshot = { ...settingsSnapshot, update_status: payload };
      }).then(saveUnlisten).catch((error) => console.error('监听更新状态失败', error));
      void listen('monitor-unlocked', () => void refreshLocalMonitorSettings())
        .then(saveUnlisten).catch((error) => console.error('监听悬浮条解锁失败', error));
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
          void refreshRemoteSettings();
          if (remoteTarget && [...pendingMobileUploads.values()].some(upload => !upload.failure)) {
            void readLocalMobileTransfers().catch(error => console.error('读取本机网页传输失败', error));
          }
          const generation = targetGeneration;
          void nodeRead.refresh().catch(async (error) => {
            if (pageStopped || generation !== targetGeneration) return;
            if (remoteTarget) {
              settingsSnapshot = null;
              if (await rediscoverRemote(generation, activeBaseUrl)) return;
            }
            if (generation !== targetGeneration) return;
            status = remoteTarget ? remoteConnectionError(error) : error instanceof Error ? error.message : '无法刷新节点状态';
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
    if (isWeb) mobileSessionId = window.sessionStorage.getItem(MOBILE_SESSION_KEY) ?? '';
    void start();
    return () => {
      pageStopped = true;
      resetSnapshotReads();
      document.removeEventListener('visibilitychange', resumeNotice);
      window.removeEventListener('focus', resumeNotice);
      window.clearTimeout(noticeTimer);
      if (timer) window.clearInterval(timer);
      for (const { controller } of prepareTasks.values()) controller.abort();
      for (const pending of [...pendingUploads.values(), ...pendingMobileUploads.values()]) pending.controller?.abort();
      if (wsReconnectTimer) window.clearTimeout(wsReconnectTimer);
      wsReconnectTimer = undefined;
      if (wsPingTimer) window.clearInterval(wsPingTimer);
      wsPingTimer = undefined;
      ws?.close();
      ws = null;
      for (const unlisten of eventUnlisteners) unlisten();
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

  async function refreshTarget() {
    const generation = targetGeneration;
    const target = activeBaseUrl;
    if (remoteTarget && !target) {
      await rediscoverRemote(generation, '');
      return;
    }
    const current = () => !pageStopped && generation === targetGeneration;

    const nextNode = await getInfo(target);
    if (!current()) return;
    if (remoteTarget && (nextNode.node_id !== remoteTarget.node_id || nextNode.protocol_version !== NODE_PROTOCOL_VERSION)) {
      suspendRemoteTarget();
      return;
    }
    void clipboardRead.refresh().catch(error => {
      if (!current()) return;
      const message = error instanceof Error ? error.message : '无法读取共享剪贴板';
      if (message !== clipboardReadError) showTransientNotice(message, 6000);
      clipboardReadError = message;
    });
    const results = await Promise.allSettled([
      refreshOverview(current, generation, target, nextNode),
      refreshTransfers(current, target),
    ]);
    for (const result of results) if (result.status === 'rejected') throw result.reason;
  }

  async function refreshOverview(current: () => boolean, generation: number, target: string, nextNode: NodeInfo) {
    const [nextPeers, nextMetrics, sessions] = await Promise.all([
      getPeers(target), getMetrics(target),
      isDesktopWindow ? getMobileSessions(target) : [],
    ]);
    if (!current()) return;
    mobileSessions = sessions;
    if (mobileAccessOpen && sessions.some((session) => !mobileAccessKnownSessions.has(session.session_id))) mobileAccessOpen = false;
    node = nextNode;
    if (!remoteMode) localNodeId = nextNode.node_id;
    peers = nextPeers.filter((peer) => peer.online);
    metrics = nextMetrics;
    if (selectedMetricsPeer) {
      selectedMetricsPeer = peers.find((peer) => peer.node_id === selectedMetricsPeer?.node_id) ?? null;
      if (!selectedMetricsPeer) metricsPeerOpen = false;
    }
    mobileAddresses = nextNode.addresses.filter((address) => !address.address.startsWith('127.'));
    if (!mobileAddresses.some((address) => address.url === selectedAddress)) {
      selectedAddress = mobileAddresses[0]?.url ?? '';
    }
    status = '节点在线';
    statusKind = 'ok';
    if (!isWeb) await refreshSettings(generation, target);
  }

  /** 文件由持有源内容的一端推进，节点概览的可用性不影响传输。 */
  async function refreshTransfers(current: () => boolean, target: string) {
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
    await historyRead.refresh();
  }

  /** 页面持有的源文件只属于本机。查看其他节点期间仍按本机的确认、取消和完成状态推进上传。 */
  function readLocalMobileTransfers(): Promise<MobileTransferView[]> {
    if (localMobileRead) return localMobileRead;
    const uploads = [...pendingMobileUploads];
    localMobileRead = getMobileTransfers(undefined, localBaseUrl).then(views => {
      if (pageStopped || !uploads.length) return views;
      const progressById = new Map(views.map(({ progress }) => [progress.transfer_id, progress]));
      for (const [id, upload] of uploads) {
        if (pendingMobileUploads.get(id) !== upload) continue;
        const progress = progressById.get(id);
        if (!progress) {
          // 建单失败的源文件仍用于手动重试，已从节点清单移除的任务可以释放。
          if (!upload.failure) forgetPendingTransfer(id);
        } else if (progress.status === 'accepted' || progress.status === 'uploading') {
          void uploadPendingFiles(upload);
        } else if (progress.status === 'failed') {
          upload.controller?.abort();
          upload.failure ??= { progress: { ...progress, direction: 'send' }, failed_at_ms: Date.now(), retryable: true };
        } else if (progress.status !== 'waiting') {
          forgetPendingTransfer(id);
        }
      }
      return views;
    }).finally(() => { localMobileRead = undefined; });
    return localMobileRead;
  }

  async function refreshMobileTransfers(current: () => boolean, target: string): Promise<MobileTransferView[]> {
    let views: MobileTransferView[];
    if (isWeb) views = await getMobileTransfersWithRecovery();
    else if (target === localBaseUrl) views = await readLocalMobileTransfers();
    else views = await getMobileTransfers(undefined, target);
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
    } else if (!remoteTarget) {
      for (const [id, pending] of pendingMobileUploads) {
        if (pending.failure && !failures.some((failure) => failure.progress.transfer_id === id)) failures = [pending.failure, ...failures];
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
      settingsSnapshot = settings;
    } catch (error) {
      if (pageStopped || settingsRead !== read) return;
      settingsSnapshot = null;
      status = error instanceof Error ? error.message : '无法读取节点设置';
      statusKind = 'warn';
    } finally {
      if (settingsRead === read) settingsRead = null;
    }
  }

  async function refreshLocalMonitorSettings() {
    if (pageStopped || remoteTarget || !node) return;
    const generation = targetGeneration;
    const target = activeBaseUrl;
    settingsRead = null;
    await settingsWrites.get(node.node_id)?.catch(() => undefined);
    await refreshSettings(generation, target);
  }

  function openSettings(trigger: HTMLButtonElement, peer = remoteTarget) {
    if (settingsOpen) return;
    settingsSession += 1;
    settingsTrigger = trigger;
    settingsOpen = true;
    if (peer) void connectRemoteSettings(peer);
  }

  function openPeerSettings(peer: PeerInfo, trigger: HTMLButtonElement) {
    if (!remoteMode && canOpenRemoteSettings(peer, localNodeId)) openSettings(trigger, peer);
  }

  function closeSettingsChildren() {
    if (browseReceiveSettings) closeBrowse();
    if (pathPrompt?.settings) resolvePathPrompt(null);
  }

  function closeSettings(restoreFocus = true) {
    if (!settingsOpen) return;
    const trigger = settingsTrigger;
    settingsOpen = false;
    settingsSession += 1;
    settingsTrigger = null;
    settingsFeedback = null;
    remoteSettings = null;
    remoteSettingsRead = null;
    restartBusy = false;
    closeSettingsChildren();
    if (restoreFocus) void tick().then(() => {
      if (!settingsOpen && !pageStopped) trigger?.focus();
    });
  }

  function disconnectRemoteSettings(error: string) {
    if (!remoteSettings) return;
    settingsSession += 1;
    remoteSettings = { ...remoteSettings, target: null, snapshot: null, error };
    remoteSettingsRead = null;
    settingsFeedback = null;
    restartBusy = false;
    closeSettingsChildren();
  }

  async function connectRemoteSettings(peer: PeerInfo, rediscover = false) {
    const session = ++settingsSession;
    const isCurrent = () => !pageStopped && settingsOpen && session === settingsSession;
    remoteSettings = {
      peer, node: remoteSettings?.node ?? null, address: remoteSettings?.address ?? '',
      target: null, snapshot: null, error: '',
    };
    try {
      const online = rediscover ? await getPeers(localBaseUrl).catch(() => []) : [];
      const candidate = online.find((item) => item.online && item.node_id === peer.node_id) ?? peer;
      const preferred = remoteTarget?.node_id === peer.node_id && activeBaseUrl ? [activeBaseUrl] : [];
      const connected = await withPeerAddress(peer.node_id, [...preferred, ...targetCandidates(candidate)], async (base, info) => {
        if (!info.capabilities.includes(REMOTE_MANAGEMENT_CAPABILITY)) throw new Error('目标节点暂不支持远程管理');
        const target: ManagedRequestTarget = {
          base, nodeId: peer.node_id, isCurrent,
          onMismatch: () => {
            if (remoteSettings?.target !== target) return;
            disconnectRemoteSettings('地址上的设备与目标不匹配');
            if (remoteTarget?.node_id === target.nodeId && activeBaseUrl === target.base) suspendRemoteTarget();
          },
        };
        await settingsWrites.get(peer.node_id)?.catch(() => undefined);
        const snapshot = await getSettings(target);
        return { peer: candidate, node: info, target, address: new URL(base).host, snapshot, error: '' };
      }, isCurrent);
      if (isCurrent()) remoteSettings = connected;
    } catch (error) {
      if (isCurrent()) disconnectRemoteSettings(remoteConnectionError(error));
    }
  }

  function remoteConnectionError(error: unknown): string {
    return error instanceof Error && error.name !== 'AbortError' && !(error instanceof TypeError)
      ? error.message : '无法连接目标节点，请确认设备在线';
  }

  async function refreshRemoteSettings() {
    const target = remoteSettings?.target;
    if (!target || remoteSettingsRead || settingsWrites.has(target.nodeId)) return;
    const read = getSettings(target);
    remoteSettingsRead = read;
    try {
      const snapshot = await read;
      if (target.isCurrent() && remoteSettingsRead === read && remoteSettings?.target === target) {
        remoteSettings = { ...remoteSettings, snapshot };
      }
    } catch (error) {
      if (target.isCurrent() && remoteSettingsRead === read) disconnectRemoteSettings(remoteConnectionError(error));
    } finally {
      if (remoteSettingsRead === read) remoteSettingsRead = null;
    }
  }

  function settingsScope(dialog = false): SettingsScope | null {
    if (pageStopped || (dialog && !settingsOpen)) return null;
    if (dialog && remoteSettings) {
      const { target, snapshot } = remoteSettings;
      return target && snapshot ? { target, snapshot, nodeId: target.nodeId, isCurrent: target.isCurrent, dialog } : null;
    }
    if (!node || !settingsSnapshot) return null;
    const generation = targetGeneration;
    const session = settingsSession;
    const isCurrent = () => !pageStopped && generation === targetGeneration && (!dialog || session === settingsSession);
    return { target: activeBaseUrl, snapshot: settingsSnapshot, nodeId: node.node_id, isCurrent, dialog };
  }

  function showSettingsFeedback(scope: SettingsScope, text: string, error = false) {
    if (scope.dialog) settingsFeedback = { text, error };
    else showTransientNotice(text, error ? 6000 : undefined);
  }

  /** 同一节点串行写入，旧上下文的结果不更新当前界面。 */
  async function saveSettings(
    scope: SettingsScope | null,
    change: (target: RequestTarget) => Promise<NodeSettings | void>,
    success: string | ((settings: NodeSettings) => string),
    failure: string,
  ): Promise<boolean> {
    if (!scope || !scope.isCurrent() || settingsWrites.has(scope.nodeId)) return false;
    const { target, nodeId } = scope;
    const explicit = typeof target !== 'string';
    const base = explicit ? target.base : target;
    const ownsOverview = () => node?.node_id === nodeId && base === activeBaseUrl;
    if (ownsOverview()) settingsRead = null;
    if (explicit) remoteSettingsRead = null;
    if (scope.dialog) settingsFeedback = null;
    const write = Promise.resolve().then(() => change(target));
    settingsWrites = new Map(settingsWrites).set(nodeId, write);
    let failed = false;
    try {
      const settings = await write;
      if (!scope.isCurrent()) return false;
      if (settings) {
        if (ownsOverview()) settingsSnapshot = settings;
        if (explicit && remoteSettings?.target === target) remoteSettings = { ...remoteSettings, snapshot: settings };
      }
      showSettingsFeedback(scope, typeof success === 'string' ? success : success(settings ?? scope.snapshot));
      return true;
    } catch (error) {
      failed = true;
      if (scope.isCurrent()) {
        if (explicit && (error instanceof TypeError || (error instanceof ApiError && error.status === 421)
          || (error instanceof DOMException && error.name === 'AbortError'))) {
          disconnectRemoteSettings(error instanceof ApiError
            ? error.message : failure + '，未收到目标节点确认。请重新连接后核对节点状态。');
        } else {
          if (!explicit && ownsOverview()) settingsSnapshot = null;
          showSettingsFeedback(scope, error instanceof Error ? error.message : failure, true);
        }
      }
      return false;
    } finally {
      const remaining = new Map(settingsWrites);
      remaining.delete(nodeId);
      settingsWrites = remaining;
      if (ownsOverview() && (!settingsSnapshot || failed || !scope.isCurrent())) void refreshSettings();
      if (failed && explicit && remoteSettings?.target === target) void refreshRemoteSettings();
    }
  }

  async function createSession(): Promise<string> {
    const session = await createMobileSession(webSessionLabel(navigator.userAgent));
    mobileSessionId = session.session_id;
    window.sessionStorage.setItem(MOBILE_SESSION_KEY, mobileSessionId);
    return mobileSessionId;
  }

  async function ensureMobileSession(): Promise<string> {
    if (mobileSessionId) return mobileSessionId;
    creatingMobileSession ??= createSession().finally(() => { creatingMobileSession = null; });
    return creatingMobileSession;
  }

  function connectWebSocket() {
    if (pageStopped || !activeBaseUrl) return;
    if (wsReconnectTimer) {
      window.clearTimeout(wsReconnectTimer);
      wsReconnectTimer = undefined;
    }
    if (wsPingTimer) {
      window.clearInterval(wsPingTimer);
      wsPingTimer = undefined;
    }
    const expectedId = remoteTarget?.node_id;
    let identified = !expectedId;
    const socket = new WebSocket(websocketUrl());
    ws?.close();
    ws = socket;
    socket.onopen = () => {
      if (socket !== ws) return;
      wsReconnectAttempt = 0;
      if (identified) {
        status = '已连接';
        statusKind = 'ok';
      }
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
        if (expectedId && message.type === 'Hello') {
          identified = message.payload.node.node_id === expectedId && message.payload.node.protocol_version === NODE_PROTOCOL_VERSION;
          if (!identified) { suspendRemoteTarget(); return; }
        }
        if (identified) handleWireMessage(message);
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
      void refresh().catch(() => { /* 定期刷新负责重新发现并报告离线。 */ });
      return;
    }
    if (type === 'ClipboardChanged' || type === 'ClipboardUpdate' || type === 'ClipboardSnapshot') {
      void clipboardRead.refresh(true).catch(() => {});
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
      void historyRead.refresh(true);
      return;
    }
    // 进度同时携带清单与接收状态。网页只展示页面发起的发送和会话下载。
    if (type === 'TransferProgress' && !isWeb) {
      const progress = payload.progress;
      const next = recordProgress(progress);
      if (!isActiveTransferStatus(next.status)) void historyRead.refresh(true);
    }
  }

  function openMobileAccess() {
    mobileAccessKnownSessions = new Set(mobileSessions.map((session) => session.session_id));
    mobileAccessOpen = true;
  }

  async function refreshQr(url: string): Promise<void> {
    if (!url) return;
    try {
      const { default: QRCode } = await import('qrcode');
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
        window.sessionStorage.removeItem(MOBILE_SESSION_KEY);
      }
      return getMobileTransfers(await ensureMobileSession());
    }
  }

  async function toggleSync(dialog = false) {
    const scope = settingsScope(dialog);
    if (!scope) return;
    const remoteNode = dialog && remoteSettings ? remoteSettings.node : remoteMode ? node : null;
    const feature = remoteNode?.capabilities.includes(HEADLESS_CAPABILITY) ? '剪贴板发布' : '剪贴板自动同步';
    await saveSettings(
      scope,
      (target) => patchSettings({ clipboard_sync_enabled: !scope.snapshot.clipboard_sync_enabled }, target),
      (settings) => settings.clipboard_sync_enabled ? `${feature}已恢复` : `${feature}已暂停`,
      '无法更新剪贴板同步设置',
    );
  }

  async function toggleAutostart(dialog = false) {
    const scope = settingsScope(dialog);
    if (!scope || scope.snapshot.service_control_error) return;
    await saveSettings(
      scope,
      (target) => patchSettings({ autostart_enabled: !scope.snapshot.autostart_enabled }, target),
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
    const scope = settingsScope(true);
    if (!scope || next === scope.snapshot.update_frequency) return;
    await saveSettings(
      scope,
      (target) => patchSettings({ update_frequency: next }, target),
      next === 'disabled' ? '自动更新已关闭' : `自动更新已设为${updateFrequencyLabel(next)}`,
      '无法更新自动更新设置',
    );
  }

  async function setReceiveMode(mode: ReceiveMode) {
    const scope = settingsScope(true);
    if (!scope || mode === scope.snapshot.receive_settings.mode || settingsWrites.has(scope.nodeId)) return;
    const path = scope.snapshot.receive_settings.default_path;
    if (mode === 'auto' && !path) {
      await chooseReceiveDirectory(mode, scope);
      return;
    }
    await saveReceiveSettings(scope, mode, path);
  }

  async function chooseReceiveDirectory(mode?: ReceiveMode, scope = settingsScope(true)) {
    if (!scope || !scope.isCurrent() || settingsWrites.has(scope.nodeId)) return;
    if (typeof scope.target !== 'string') {
      if (remoteSettings?.node?.capabilities.includes(REMOTE_FILES_CAPABILITY)) {
        browseMode = 'directory';
        browseStartPath = scope.snapshot.receive_settings.default_path ?? '';
        browseReceiveSettings = { scope, mode };
        browseOpen = true;
      } else {
        await enterSettingsDirectory(scope, mode);
      }
      return;
    }
    let destination: string | string[] | null = null;
    try {
      destination = await open({ directory: true, multiple: false, title: '选择默认接收目录' });
    } catch {
      destination = null;
    }
    const path = Array.isArray(destination) ? destination[0] : destination;
    if (path) await saveReceiveSettings(scope, mode, path);
  }

  async function enterSettingsDirectory(scope: SettingsScope, mode?: ReceiveMode) {
    if (!scope.isCurrent() || !dialogRemote) return;
    const path = await askPath(`在 ${dialogRemote.name} 上输入接收目录`, scope.snapshot.receive_settings.default_path ?? '', true);
    if (path) await saveReceiveSettings(scope, mode, path);
  }

  async function saveReceiveSettings(scope: SettingsScope, mode: ReceiveMode | undefined, defaultPath?: string | null) {
    await saveSettings(
      scope,
      async (target) => {
        // 只改目录时，保留选择期间其他客户端已经保存的接收方式。
        const nextMode = mode ?? (await getSettings(target)).receive_settings.mode;
        if (!scope.isCurrent()) return;
        return patchSettings({ receive_settings: { mode: nextMode, default_path: defaultPath ?? null } }, target);
      },
      ({ receive_settings: saved }) => mode === undefined || (saved.default_path && saved.default_path !== scope.snapshot.receive_settings.default_path)
        ? '默认接收目录已保存'
        : saved.mode === 'auto' ? '已启用自动接收' : '已设为每次确认',
      '无法更新接收设置',
    );
  }

  async function setClipboardHistoryMode(mode: ClipboardHistoryMode) {
    const scope = settingsScope(true);
    if (!scope || mode === scope.snapshot.clipboard_history_settings.mode) return;
    const retention = scope.snapshot.clipboard_history_settings;
    await saveClipboardHistorySettings(scope, mode, mode === 'count' ? retention.count : retention.days);
  }

  async function setClipboardHistoryValue(value: number) {
    const scope = settingsScope(true);
    if (!scope || !Number.isFinite(value)) return;
    await saveClipboardHistorySettings(scope, scope.snapshot.clipboard_history_settings.mode, value);
  }

  async function saveClipboardHistorySettings(scope: SettingsScope, mode: ClipboardHistoryMode, value: number) {
    const saved = await saveSettings(
      scope,
      (base) => patchSettings({ clipboard_history_settings: { mode, value } }, base),
      ({ clipboard_history_settings: settings }) => settings.mode === 'count'
        ? `已保留最近 ${settings.count} 条剪贴板内容`
        : `已保留最近 ${settings.days} 天剪贴板内容`,
      '无法更新剪贴板历史设置',
    );
    if (saved && scope.isCurrent() && node?.node_id === scope.nodeId) {
      await clipboardRead.refresh(true).catch((error) => console.error('刷新剪贴板历史失败', error));
    }
  }

  function editClipboardDraft(text: string) {
    clipboardDraft = text;
    clipboardDraftEdited = true;
    clipboardDraftRevision += 1;
  }

  function syncClipboardDraft() {
    if (!clipboardDraftEdited) clipboardDraft = clipboard?.text ?? '';
  }

  async function readSharedClipboard() {
    const generation = targetGeneration;
    const target = activeBaseUrl;
    const knownSnapshot = clipboard;
    const controller = new AbortController();
    clipboardRequest = controller;
    const [snapshot, response] = await Promise.all([
      getClipboard(target, controller, knownSnapshot ?? undefined),
      getClipboardHistory(target),
    ]).finally(() => {
      // 历史请求可能先失败，此时快照仍在传输。
      controller.abort();
      if (clipboardRequest === controller) clipboardRequest = undefined;
    });
    if (generation !== targetGeneration || pageStopped) return;
    clipboardReadError = '';
    if (snapshot !== undefined && (clipboard === knownSnapshot || (snapshot && isNewerClipboardSnapshot(snapshot, clipboard)))) {
      clipboard = snapshot;
    }
    if (document.activeElement?.id !== 'clipboard-editor') syncClipboardDraft();
    if (isWeb) webHistorySettings = response.settings;
    clipboardHistory = response.items;
    if (selectedClipboardHistory && !clipboardHistory.some((item) => item.event_id === selectedClipboardHistory?.item.event_id)) {
      selectedClipboardHistory = null;
      historyDetailToken += 1;
      historyDetailLoading = false;
    }
  }

  async function checkForUpdates() {
    await saveSettings(settingsScope(true), requestUpdateCheck, '已发起更新检查', '无法检查更新');
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

  async function toggleMonitorLock(dialog = false) {
    const scope = settingsScope(dialog);
    if (!scope?.snapshot.monitor.capabilities.can_click_through) return;
    await saveSettings(
      scope,
      async (target) => {
        const { monitor } = await getSettings(target);
        if (!scope.isCurrent()) return;
        return patchSettings({ monitor: { locked: !monitor.locked } }, target);
      },
      (settings) => settings.monitor.locked ? '悬浮监控条已锁定，鼠标交互已忽略' : '悬浮监控条已解锁，可以拖动',
      '无法更新悬浮监控条状态',
    );
  }

  async function toggleMonitorVisibility(dialog = false) {
    const scope = settingsScope(dialog);
    if (!scope) return;
    await saveSettings(
      scope,
      (target) => patchSettings({ monitor: { visible: !scope.snapshot.monitor.visible } }, target),
      (settings) => settings.monitor.visible ? '悬浮监控条已显示' : '悬浮监控条已隐藏',
      '无法更新悬浮监控条可见性',
    );
  }

  async function publishClipboard() {
    const draft = clipboardDraft;
    if (!node || !draft.trim()) return;
    const generation = targetGeneration;
    const draftRevision = clipboardDraftRevision;
    const knownSnapshot = clipboard;
    try {
      const published = await request<ClipboardSnapshot>('/api/v1/clipboard', {
        method: 'POST',
        body: JSON.stringify({ text: draft, source_name: node.node_name }),
      });
      if (generation !== targetGeneration || pageStopped) return;
      if (clipboard === knownSnapshot || isNewerClipboardSnapshot(published, clipboard)) {
        clipboard = published;
      } else {
        // 发布期间缓存已经更新，向节点补读一次以确定最新事件。
        void clipboardRead.refresh(true).catch(() => {});
      }
      if (clipboardDraftRevision === draftRevision) {
        clipboardDraftEdited = false;
        if (document.activeElement?.id !== 'clipboard-editor') syncClipboardDraft();
      }
      showTransientNotice('已同步到在线设备');
    } catch (error) {
      if (generation === targetGeneration) showTransientNotice(error instanceof Error ? error.message : '剪贴板同步失败', 6000);
    }
  }

  function openClipboardHistoryBrowser() {
    clipboardHistoryOpen = true;
    void clipboardRead.refresh(true).catch((error) => console.error('刷新剪贴板历史失败', error));
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
    const generation = targetGeneration;
    const draftRevision = clipboardDraftRevision;
    const knownSnapshot = clipboard;
    try {
      const restored = await restoreClipboardHistory(eventId);
      if (generation !== targetGeneration) return;
      if (clipboard === knownSnapshot || isNewerClipboardSnapshot(restored, clipboard)) clipboard = restored;
      if (clipboardDraftRevision === draftRevision) {
        clipboardDraftEdited = false;
        syncClipboardDraft();
      }
      await clipboardRead.refresh(true);
      if (generation === targetGeneration && !pageStopped) showTransientNotice('已恢复并同步剪贴板内容');
    } catch (error) {
      if (generation === targetGeneration && !pageStopped) showTransientNotice(error instanceof Error ? error.message : '无法恢复剪贴板历史', 6000);
    } finally {
      if (generation === targetGeneration) clipboardHistoryBusy = false;
    }
  }

  async function clearClipboardHistoryItems() {
    if (clipboardHistoryBusy || !clipboardHistory.length) return;
    clipboardHistoryBusy = true;
    const generation = targetGeneration;
    try {
      await clearClipboardHistory();
      if (generation !== targetGeneration) return;
      clipboardHistory = [];
      historyDetailToken += 1;
      historyDetailLoading = false;
      selectedClipboardHistory = null;
      clearHistoryOpen = false;
      showTransientNotice('已清空本机剪贴板历史');
    } catch (error) {
      showTransientNotice(error instanceof Error ? error.message : '无法清空剪贴板历史', 6000);
    } finally {
      if (generation === targetGeneration) clipboardHistoryBusy = false;
    }
  }

  async function readMobileClipboard() {
    const generation = targetGeneration;
    const draftRevision = clipboardDraftRevision;
    try {
      const text = await navigator.clipboard.readText();
      if (generation !== targetGeneration || draftRevision !== clipboardDraftRevision || pageStopped) return;
      editClipboardDraft(text);
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
    const value = remoteMode ? activeBaseUrl : node?.addresses[0]?.url;
    if (!value) return;
    try {
      await navigator.clipboard.writeText(value);
      showTransientNotice('已复制访问地址');
      return;
    } catch {
      const copied = fallbackCopy(value);
      showTransientNotice(copied ? '已复制访问地址' : '无法复制地址，请手动选择', copied ? 2400 : 6000);
    }
  }

  function browseForSend(recipient: BrowseRecipient) {
    if (!managedFilesSupported) {
      showTransientNotice('目标节点不支持远程文件选择', 6000);
      return;
    }
    browseRecipient = recipient;
    browseTransfer = null;
    browseMode = 'send';
    browseStartPath = '';
    browseOpen = true;
  }

  function startChoose(peer: PeerInfo, folder = false) {
    if (remoteMode) {
      browseForSend({ peer });
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
    if (managed && !remoteTarget) return;
    const api = outgoingPathsApi(activeBaseUrl, managed ? remoteTarget?.node_id : undefined);
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
        if (controller.signal.aborted) return;
        const status = await api.status(prepareId).catch((error) => {
          if (error instanceof ApiError && error.status === 404) return null;
          throw error;
        });
        // 准备条目消失表示清单已发给接收端（或任务被取消）。
        if (!status || controller.signal.aborted) break;
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
    const recipient = browseRecipient;
    const transfer = browseTransfer;
    const directory = browseMode === 'directory';
    const setting = browseReceiveSettings;
    closeBrowse();
    if (!paths.length) return;
    if (directory) {
      if (setting) await saveReceiveSettings(setting.scope, setting.mode, paths[0]);
      else if (transfer) await acceptTransferAt(transfer, paths[0]);
      return;
    }
    if (!recipient) return;
    if ('peer' in recipient) {
      await sendPaths(recipient.peer, paths, true);
      return;
    }
    const generation = targetGeneration;
    try {
      const view = await sendManagedPathsToMobile(recipient.mobile.session_id, paths, activeBaseUrl);
      if (generation !== targetGeneration) return;
      recordProgress({ ...view.progress, direction: 'send' });
      showTransientNotice('已发送，等待网页端确认');
    } catch (error) {
      if (generation === targetGeneration) showTransientNotice(error instanceof Error ? error.message : '无法发送到网页端', 6000);
    }
  }

  function closeBrowse() {
    browseOpen = false;
    browseStartPath = '';
    browseRecipient = null;
    browseTransfer = null;
    browseReceiveSettings = null;
  }

  /** 目标机器未开启远程文件访问时，接收目录仍可手输。 */
  async function requestManualAcceptPath() {
    const transfer = browseTransfer;
    const setting = browseReceiveSettings;
    const generation = targetGeneration;
    closeBrowse();
    if (setting) {
      await enterSettingsDirectory(setting.scope, setting.mode);
      return;
    }
    if (!transfer) return;
    const path = await askPath('输入接收目录', settingsReady ? receiveSettings.default_path ?? '' : '');
    if (!path) {
      showTransientNotice('需要输入目标节点上的接收目录', 6000);
      return;
    }
    if (generation !== targetGeneration) return;
    await acceptTransferAt(transfer, path);
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

  async function allowRemoteTerminal() {
    if (!isDesktopWindow || remoteMode) return;
    const updated = await invoke<NodeInfo>('terminal_accept_migration');
    if (!remoteMode && node?.node_id === updated.node_id) node = updated;
  }

  function startChooseMobile(session: MobileSession, folder = false) {
    if (remoteMode) {
      if (!node?.capabilities.includes('management.mobile-paths.v1')) {
        showTransientNotice('目标节点不支持从其磁盘发送文件到网页会话，请升级目标节点后重试', 6000);
        return;
      }
      browseForSend({ mobile: session });
      return;
    }
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

  async function dropFiles(recipient: BrowseRecipient, event: DragEvent) {
    event.preventDefault();
    dragPeer = '';
    if (remoteMode) {
      showTransientNotice('远程管理页面的发送从目标机器选择文件', 6000);
      return;
    }
    const generation = targetGeneration;
    const files = event.dataTransfer ? await filesFromDataTransfer(event.dataTransfer) : [];
    if (!files.length || generation !== targetGeneration || pageStopped) return;
    if ('peer' in recipient) await sendFiles(recipient.peer, files);
    else await sendFilesToMobile(recipient.mobile, files);
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

  async function onTransferRoute<T>(route: TransferRoute, perform: (base: string, nodeId: string) => Promise<T>): Promise<T> {
    const peer = peers.find((candidate) => candidate.node_id === route.peer.node_id) ?? route.peer;
    return withPeerAddress(peer.node_id, [route.target, ...targetCandidates(peer)], async (base) => {
      // HTTPS 网页的请求先由同源接入节点处理，代理在转发时重新绑定最终接收节点。
      const hop = isWeb && base.startsWith(window.location.origin + '/api/v1/mobile/peers/') ? localNodeId : peer.node_id;
      if (!hop) throw new ApiError('尚未取得节点标识', 421);
      const result = await perform(base, hop);
      route.target = base;
      return result;
    });
  }

  async function createTransferOnCandidates(offer: TransferOffer, route: TransferRoute): Promise<TransferProgress> {
    const post = (base: string, nodeId: string) => requestWithTimeout<TransferProgress>(
      '/api/v1/transfers',
      { method: 'POST', headers: { 'X-MeshShare-Target': nodeId }, body: JSON.stringify(offer) },
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

  function resetTargetUi(keepSettings = false) {
    targetGeneration += 1;
    resetSnapshotReads();
    webHistorySettings = defaultHistory;
    resetTransferHistory();
    settingsSnapshot = null;
    settingsRead = null;
    if (!keepSettings || !browseReceiveSettings) closeBrowse();
    if (!keepSettings || !pathPrompt?.settings) resolvePathPrompt(null);
    if (!keepSettings) closeSettings(false);
    node = null;
    peers = [];
    clipboard = null;
    clipboardReadError = '';
    clipboardDraft = '';
    clipboardDraftEdited = false;
    clipboardHistoryBusy = false;
    metrics = null;
    mobileAccessOpen = false;
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

  function disconnectTargetSocket() {
    ws?.close();
    ws = null;
    if (wsReconnectTimer) window.clearTimeout(wsReconnectTimer);
    wsReconnectTimer = undefined;
    if (wsPingTimer) window.clearInterval(wsPingTimer);
    wsPingTimer = undefined;
  }

  function suspendRemoteTarget() {
    if (!remoteTarget || !activeBaseUrl) return;
    const keepSettings = remoteSettings?.peer.node_id === remoteTarget.node_id;
    if (keepSettings && remoteSettings?.target?.base === activeBaseUrl) {
      disconnectRemoteSettings('地址上的设备与目标不匹配');
    }
    activeBaseUrl = setApiTarget('');
    disconnectTargetSocket();
    resetTargetUi(keepSettings);
    statusKind = 'warn';
    status = '节点地址已变更，正在重新发现目标节点';
  }

  async function applyTargetContext(baseUrl: string, target: PeerInfo | null) {
    disconnectTargetSocket();
    const keepSettings = !!target && target.node_id === remoteTarget?.node_id && target.node_id === remoteSettings?.peer.node_id;
    remoteTarget = target;
    activeBaseUrl = setApiTarget(baseUrl, target ? { nodeId: target.node_id, onMismatch: suspendRemoteTarget } : undefined);
    remoteAddress = target ? new URL(activeBaseUrl).host : '';
    resetTargetUi(keepSettings);
    const context = targetGeneration;
    status = target ? '正在连接目标节点…' : '正在返回本机…';
    statusKind = 'warn';
    wsReconnectAttempt = 0;
    await tick();
    if (context !== targetGeneration) return;
    await refresh().catch((error) => {
      if (context !== targetGeneration) return;
      status = target ? remoteConnectionError(error) : error instanceof Error ? error.message : '无法连接本机节点';
      statusKind = 'error';
    });
    if (context !== targetGeneration) return;
    connectWebSocket();
  }

  async function restartManagedNode() {
    const scope = settingsScope(true);
    if (!scope || typeof scope.target === 'string' || !dialogRemote?.headless || scope.snapshot.service_control_error
      || settingsWrites.has(scope.nodeId)) return;
    const target = scope.target;
    const notice = `已向 ${dialogRemote.name}（${dialogRemote.address}）发出重启请求`;
    restartBusy = true;
    try {
      if (await saveSettings(scope, () => restartService(target), notice, '未能确认重启请求') && scope.isCurrent()) {
        closeSettings();
        showTransientNotice(notice);
      }
    } finally {
      if (scope.isCurrent()) restartBusy = false;
    }
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
    const isCurrent = () => !pageStopped && generation === targetGeneration;
    const candidates = targetCandidates(peer).filter((url) => url !== skipBase);
    try {
      const target = await withPeerAddress(peer.node_id, candidates, async (base) => base, isCurrent);
      if (!isCurrent()) return false;
      await applyTargetContext(target, peer);
      return true;
    } catch {
      if (skipBase === undefined && isCurrent()) showTransientNotice('目标设备没有可访问地址', 6000);
      return false;
    }
  }

  async function rediscoverRemote(generation: number, skipBase: string): Promise<boolean> {
    if (!remoteTarget || rediscoveringGeneration === generation) return false;
    const expected = remoteTarget;
    rediscoveringGeneration = generation;
    try {
      const online = await getPeers(localBaseUrl).catch(() => []);
      if (pageStopped || generation !== targetGeneration) return false;
      const peer = online.find(p => p.node_id === expected.node_id) ?? expected;
      return await selectRemoteAddress(peer, skipBase);
    } finally {
      if (rediscoveringGeneration === generation) rediscoveringGeneration = undefined;
    }
  }

  async function returnToLocal() {
    if (!remoteMode) return;
    terminalVisible = false;
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
      await uploadItems(offer, files, controller.signal, async (item, file) => {
        const url = apiUrl(`/api/v1/outgoing/${encodeURIComponent(id)}/items/${encodeURIComponent(item.item_id)}`, owner);
        await uploadFileWithRetry(url, file, item.media_type, source.node_id, controller.signal);
        stagedBytes += item.size;
        updatePrepare(id, stagedBytes);
      });
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
      const pending: PendingUpload = { target: candidates[0], peer, files, offer, completedItemIds: new Set(), attempts: 0 };
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
    if (remoteTarget || !node) return;
    const offer = buildOffer(node, node.node_id, files);
    const base = localBaseUrl;
    const generation = targetGeneration;
    selectedMobileSession = null;
    const upload: MobileUpload = { files, completedItemIds: new Set(), attempts: 0, sessionId: session.session_id, offer, base };
    try {
      const view = await createMobileTransfer(session.session_id, offer, base);
      if (pageStopped) return;
      pendingMobileUploads.set(offer.transfer_id, upload);
      if (generation !== targetGeneration) return;
      recordProgress({ ...view.progress, direction: 'send' });
      showTransientNotice('已发送，等待网页端确认', 3600);
    } catch (error) {
      if (!pageStopped && generation === targetGeneration) {
        pendingMobileUploads.set(offer.transfer_id, upload);
        recordSendFailure(offer, error instanceof Error ? error.message : '无法发送到网页端');
      }
    }
  }

  async function uploadPendingFiles(pending: PendingUpload | MobileUpload) {
    if (pending.controller || pending.attempts >= MAX_UPLOAD_ATTEMPTS) return;
    const mobile = 'sessionId' in pending;
    const id = pending.offer.transfer_id;
    const generation = targetGeneration;
    const visible = () => !pageStopped && (mobile ? activeBaseUrl === pending.base : generation === targetGeneration);
    const path = (mobile ? '/api/v1/mobile/transfers/' : '/api/v1/transfers/') + encodeURIComponent(id);
    const route = <T,>(send: (base: string, nodeId: string) => Promise<T>) =>
      mobile ? send(pending.base, pending.offer.source_node_id) : onTransferRoute(pending, send);
    const controller = new AbortController();
    pending.controller = controller;
    try {
      await uploadItems(pending.offer, pending.files, controller.signal, (item, file) => route((base, nodeId) =>
        uploadFileWithRetry(apiUrl(path + '/items/' + encodeURIComponent(item.item_id), base), file, item.media_type, nodeId, controller.signal)
      ), pending.completedItemIds);
      pending.attempts = 0;
      if (visible()) showTransientNotice(mobile ? '文件已准备好，网页端可以下载' : '文件发送完成');
    } catch (error) {
      if (controller.signal.aborted) return;
      const message = mobile
        ? (error instanceof Error ? error.message : '网页端文件上传失败')
        : describeTransferFailure(error, pending.peer.node_name, pending.target);
      pending.attempts += 1;
      if (pending.attempts >= MAX_UPLOAD_ATTEMPTS) {
        const failed = message + '，已停止重试';
        await route((base, nodeId) => requestWithTimeout(path + '/fail', {
          method: 'POST', headers: { 'X-MeshShare-Target': nodeId }, body: JSON.stringify({ error: failed }),
        }, base)).catch(() => undefined);
        if (visible()) showTransientNotice(failed, 8000);
      } else if (visible()) {
        showTransientNotice(`${message}，连接恢复后将继续（${pending.attempts}/${MAX_UPLOAD_ATTEMPTS}）`, 6000);
      }
    } finally {
      pending.controller = undefined;
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
      const progress = await onTransferRoute(route, (base, nodeId) => requestWithTimeout<TransferProgress>(
        `/api/v1/transfers/${encodeURIComponent(id)}/status`, { headers: { 'X-MeshShare-Target': nodeId } }, base,
      ));
      const pending = pendingUploads.get(id);
      if (pageStopped || !pending) return;
      const next = recordProgress({ ...progress, direction: 'send' });
      if (next.status === 'accepted' || next.status === 'uploading') await uploadPendingFiles(pending);
      if (!isActiveTransferStatus(next.status) && next.status !== 'failed') forgetPendingTransfer(id);
    } catch {
      // 接收节点暂时不可达时保留页面持有的源文件。
    } finally {
      watching.delete(id);
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
        pendingMobileUploads.set(offer.transfer_id, { ...mobile, offer, failure: undefined, controller: undefined, attempts: 0, completedItemIds: new Set() });
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
        const progress = await onTransferRoute(pending ?? { target, peer }, (base, nodeId) => requestWithTimeout<TransferProgress>(
          `/api/v1/transfers/${encodeURIComponent(id)}/retry`, { method: 'POST', headers: { 'X-MeshShare-Target': nodeId } }, base,
        ));
        if (generation !== targetGeneration) return;
        if (pending) {
          pending.attempts = 0;
          pending.completedItemIds.clear();
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
        if (generation !== targetGeneration) return;
        if (!path) {
          showTransientNotice('需要输入目标节点上的接收目录', 6000);
          return;
        }
        await acceptTransferAt(transfer, path);
        return;
      }
      browseTransfer = transfer;
      browseRecipient = null;
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
    const cancel = (base: string, nodeId?: string) => request(
      `/api/v1/transfers/${encodeURIComponent(transfer.transfer_id)}/cancel`,
      { method: 'POST', headers: nodeId ? { 'X-MeshShare-Target': nodeId } : undefined },
      base,
    );
    try {
      if (pending) {
        pending.controller?.abort();
        await onTransferRoute(pending, cancel);
      } else await cancel(activeBaseUrl);
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
  <title>{isWeb ? 'MeshShare · 临时加入' : 'MeshShare · 局域网共享'}</title>
</svelte:head>

<div class:phone-shell={isPhone} class:app-shell={!isPhone} class:app-shell--remote={remoteMode && !terminalVisible}>
  {#if mobileUi}
    {#await mobileUi then mobile}
      <mobile.default
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
        bind:draft={() => clipboardDraft, editClipboardDraft}
        onDraftBlur={syncClipboardDraft}
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
    {/await}
  {:else if desktopUi}
    {#await desktopUi then desktop}
      <desktop.AppHeader
        {node}
        {remoteTarget}
        {remoteAddress}
        remoteAddressLive={!!activeBaseUrl && statusKind !== 'error'}
        {headless}
        {serviceControlError}
        {status}
        {statusKind}
        {syncEnabled}
        {autostartEnabled}
        {settingsReady}
        {settingsDisabled}
        onSync={() => toggleSync()}
        onAutostart={() => toggleAutostart()}
        onSettings={openSettings}
        {terminalCount}
        {terminalVisible}
        onTerminal={() => {
          if (terminalVisible) terminalVisible = false;
          else if (headless && remoteTarget) terminalWorkspace.showPeer({ peerId: remoteTarget.node_id, name: remoteTarget.node_name });
          else terminalWorkspace.showWorkspace();
        }}
        onOpenProject={openProjectPage}
        onBack={returnToLocal}
        onCopyAddress={copyAccessAddress}
      />
      {#if isDesktopWindow && node && !headless && !clipboardStatus.available}
        <div class="platform-notice" role="status">{clipboardStatus.message ?? '系统剪贴板暂不可用'}{clipboardStatus.retrying ? '（正在重试）' : ''}；共享内容仍可在设备间同步。</div>
      {/if}
      <desktop.TerminalWorkspace bind:this={terminalWorkspace} bind:visible={terminalVisible} bind:count={terminalCount} peers={remoteMode ? null : peers} />
      <main class="app-main" hidden={terminalVisible || (remoteMode && !node)}>
        <desktop.DevicePanel
          peers={peers}
          homeNodeId={localNodeId}
          activePeerIds={activeLinkPeers}
          {mobileSessions}
          draggingKey={dragPeer}
          onOpenPeer={openRemotePeer}
          onSettings={remoteMode ? null : openPeerSettings}
          onTerminal={remoteMode ? null : openPeerTerminal}
          onMobileAccess={openMobileAccess}
          onSendPeer={(peer) => startChoose(peer)}
          onSendPeerFolder={(peer) => startChoose(peer, true)}
          onDetails={selectMetricsPeer}
          onSendMobile={(session) => startChooseMobile(session)}
          onSendMobileFolder={(session) => startChooseMobile(session, true)}
          onDragEnter={(key) => (dragPeer = key)}
          onDragLeave={(key) => { if (dragPeer === key) dragPeer = ''; }}
          onDropPeer={(peer, event) => dropFiles({ peer }, event)}
          onDropMobile={(mobile, event) => dropFiles({ mobile }, event)}
          allowFolderSend={!remoteMode}
        />
        <desktop.HardwarePanel
          remote={remoteMode}
          showMonitorControls={!headless}
          metrics={metrics}
          {monitorLocked}
          {monitorVisible}
          {settingsDisabled}
          canClickThrough={monitorCapabilities.can_click_through}
          onToggleLock={() => toggleMonitorLock()}
          onToggleVisible={() => toggleMonitorVisibility()}
        />
        <desktop.ClipboardPanel
          {headless}
          {clipboard}
          bind:draft={() => clipboardDraft, editClipboardDraft}
          onDraftBlur={syncClipboardDraft}
          historyCount={clipboardHistory.length}
          onPublish={publishClipboard}
          onOpenHistory={openClipboardHistoryBrowser}
        />
        <desktop.TransferPanel {transfers} {prepares} {failures} {transferHistory} {transferHistoryBusy} {remoteMode} onAccept={acceptTransfer} onReject={rejectTransfer} onRetry={retryFailure} onDismissFailure={dismissFailure} onCancelPrepare={cancelPrepare} onClearTransferHistory={requestClearTransferHistory} onOpenLocation={openHistoryLocation} onViewLocation={viewHistoryLocation} />
      </main>
      <desktop.SettingsDialog
        open={settingsOpen}
        node={dialogNode}
        snapshot={dialogSnapshot}
        remote={dialogRemote}
        settingsDisabled={dialogSettingsDisabled}
        connectionError={remoteSettings?.error ?? ''}
        feedback={settingsFeedback}
        {restartBusy}
        onReconnect={() => remoteSettings && connectRemoteSettings(remoteSettings.peer, true)}
        onSync={() => toggleSync(true)}
        onAutostart={() => toggleAutostart(true)}
        onToggleMonitor={() => toggleMonitorVisibility(true)}
        onToggleMonitorLock={() => toggleMonitorLock(true)}
        onRestart={restartManagedNode}
        {remoteFileAccess}
        {remoteFileAccessBusy}
        onFrequency={setUpdateFrequency}
        onCheckUpdates={checkForUpdates}
        onReceiveMode={setReceiveMode}
        onChooseDirectory={() => chooseReceiveDirectory()}
        onHistoryMode={setClipboardHistoryMode}
        onHistoryValue={setClipboardHistoryValue}
        onRemoteFileAccess={setRemoteFileAccess}
        onAllowRemoteTerminal={isDesktopWindow && !remoteSettings ? allowRemoteTerminal : null}
        onClose={() => closeSettings()}
      />

      <desktop.MobileAccessDialog
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
      <desktop.MetricsDetailDialog
        open={metricsPeerOpen && !!selectedMetricsPeer}
        peer={selectedMetricsPeer}
        onClose={() => (metricsPeerOpen = false)}
      />
      <desktop.RemoteFileBrowser
        open={browseOpen}
        mode={browseMode}
        target={browseReceiveSettings?.scope.target ?? activeBaseUrl}
        targetLabel={browseReceiveSettings ? dialogRemote?.name ?? '' : node?.node_name ?? remoteTarget?.node_name ?? ''}
        targetAddress={browseReceiveSettings ? dialogRemote?.address ?? '' : remoteAddress}
        startPath={browseStartPath}
        onConfirm={confirmBrowse}
        onFallback={browseMode === 'directory' ? requestManualAcceptPath : null}
        onClose={closeBrowse}
      />
      <desktop.PromptDialog
        open={!!pathPrompt}
        title={pathPrompt?.title ?? ''}
        subtitle={pathPrompt?.settings ? dialogRemote?.address ?? '' : remoteAddress}
        remote={!!pathPrompt && (pathPrompt.settings || remoteMode)}
        label="接收目录"
        initialValue={pathPrompt?.initial ?? ''}
        onSubmit={(value) => resolvePathPrompt(value)}
        onClose={() => resolvePathPrompt(null)}
      />
    {/await}
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
