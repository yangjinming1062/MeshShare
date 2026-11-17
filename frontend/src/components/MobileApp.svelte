<script lang="ts">
  import AndroidUpdatePanel from './AndroidUpdatePanel.svelte';
  import ClipboardImage from './ClipboardImage.svelte';
  import ClipboardPreview from './ClipboardPreview.svelte';
  import Icon from './Icon.svelte';
  import {
    clipboardHistoryKind,
    clipboardHistoryPreview,
    formatPercent,
    formatRate,
    formatSize,
    formatTime,
    gpuUtilization,
    memoryPercent,
    peerSummary,
    platformLabel,
    prepareProgressRatio,
    prepareStatusText,
    transferHistoryPathText,
    transferHistoryPeerText,
    transferHistoryStatusText,
    transferItemKey,
    transferItemsLabel,
    transferStatusText,
    isActiveTransferStatus,
  } from '../format';
  import type { ClipboardHistoryDetail, ClipboardHistoryItem, ClipboardSnapshot, HardwareMetrics, NodeInfo, PeerInfo, PhoneTransfer, TransferFailure, TransferHistoryEntry, TransferItem, TransferPrepare } from '../types';

  export let mode: 'web' | 'android' = 'web';
  export let node: NodeInfo | null = null;
  export let status = '';
  export let statusKind: 'ok' | 'warn' | 'error' = 'warn';
  export let peers: PeerInfo[] = [];
  export let transfers: PhoneTransfer[] = [];
  export let prepares: TransferPrepare[] = [];
  export let failures: TransferFailure[] = [];
  export let transferHistory: TransferHistoryEntry[] = [];
  export let transferHistoryBusy = false;
  export let onClearTransferHistory: () => void = () => {};
  export let clipboard: ClipboardSnapshot | null = null;
  export let draft = '';
  export let history: ClipboardHistoryItem[] = [];
  export let historyBusy = false;
  export let selectedHistory: ClipboardHistoryDetail | null = null;
  export let metrics: HardwareMetrics | null = null;
  export let canChooseDirectory = false;
  export let receiveStorage: { directory: string; public_download: boolean } | null = null;
  export let onRequestReceiveStorage: () => void = () => {};
  export let receiveDirectoryName = '';
  export let actionBusyId = '';
  export let downloadingIds: string[] = [];
  export let startedDownloadKeys: string[] = [];

  export let onSendFile: (peer: PeerInfo) => void = () => {};
  export let onSendFolder: (peer: PeerInfo) => void = () => {};
  export let onAcceptTransfer: (transfer: PhoneTransfer) => void = () => {};
  export let onRejectTransfer: (transfer: PhoneTransfer) => void = () => {};
  export let onCancelTransfer: (transfer: PhoneTransfer) => void = () => {};
  export let onCancelPrepare: (prepareId: string) => void = () => {};
  export let onSaveToDirectory: (transfer: PhoneTransfer) => void = () => {};
  export let onStartDownload: (transfer: PhoneTransfer, item: TransferItem) => void = () => {};
  export let onRetryFailure: (failure: TransferFailure) => void = () => {};
  export let onDismissFailure: (failure: TransferFailure) => void = () => {};
  export let onPublish: () => void = () => {};
  export let onReadLocal: () => void = () => {};
  export let onWriteLocal: () => void = () => {};
  export let onChooseDirectory: () => void = () => {};
  export let onOpenHistory: (item: ClipboardHistoryItem) => void = () => {};
  export let onRestoreHistory: () => void = () => {};
  export let onCloseHistory: () => void = () => {};
  export let onClearHistory: () => void = () => {};

  let clipboardTab: 'current' | 'history' = 'current';
  let transfersOpen = false;
  let transfersAutoOpened = false;

  $: pending = transfers
    .filter((transfer) => transfer.status === 'waiting' && transfer.direction !== 'send')
    .sort((left, right) => (left.offer?.created_at_ms ?? 0) - (right.offer?.created_at_ms ?? 0));
  $: shownPending = pending.slice(0, 2);
  $: hiddenPending = pending.length - shownPending.length;
  $: pendingBytes = pending.reduce((sum, transfer) => sum + (transfer.offer?.total_bytes ?? transfer.total_bytes), 0);
  $: pendingItems = pending.reduce((sum, transfer) => sum + (transfer.offer?.items.length ?? transfer.total_items), 0);
  $: activeTransfers = transfers.filter((transfer) => isActiveTransferStatus(transfer.status) && !pending.some((item) => item.transfer_id === transfer.transfer_id));
  $: historyEntries = transferHistory.filter((entry) => !failures.some((failure) => failure.progress.transfer_id === entry.transfer_id));
  $: activeIncoming = transfers
    .filter((transfer) => transfer.direction !== 'send' && (transfer.status === 'accepted' || transfer.status === 'uploading' || transfer.status === 'ready'))
    .sort((left, right) => (right.offer?.created_at_ms ?? 0) - (left.offer?.created_at_ms ?? 0));
  $: inbox = pending.length ? null : (activeIncoming[0] ?? null);
  $: recentDownloads = mode === 'web' ? transfers.filter((transfer) =>
    transfer.status === 'completed' && transfer.retryable_download_item_ids?.length,
  ) : [];
  $: acceptLabel = mode === 'android'
    ? '接收'
    : canChooseDirectory
      ? (receiveDirectoryName ? '开始接收' : '选择目录并接收')
      : '接收并下载';
  $: if (!transfersAutoOpened) autoOpenTransfers();

  function autoOpenTransfers() {
    if (!activeTransfers.some((transfer) => transfer.status === 'accepted' || transfer.status === 'uploading' || transfer.status === 'ready')) return;
    transfersOpen = true;
    transfersAutoOpened = true;
  }

  function ratio(transfer: PhoneTransfer): number {
    if (!transfer.total_bytes) return 0;
    return Math.max(0, Math.min(1, transfer.completed_bytes / transfer.total_bytes));
  }

  function gpuValue(peer: PeerInfo): number | undefined {
    return gpuUtilization(peer.metrics);
  }

  function downloaded(transfer: PhoneTransfer, item: TransferItem): boolean {
    return transfer.downloaded_item_ids?.includes(item.item_id) ?? false;
  }

  function canRetryDownload(transfer: PhoneTransfer, item: TransferItem): boolean {
    return transfer.retryable_download_item_ids?.includes(item.item_id) ?? false;
  }

  function downloadStarted(transfer: PhoneTransfer, item: TransferItem): boolean {
    return startedDownloadKeys.includes(transferItemKey(transfer.transfer_id, item.item_id));
  }

  function transferTitle(transfer: PhoneTransfer): string {
    if (transfer.direction === 'send') return '发送到 ' + (transfer.peer_name ?? '设备');
    return '来自 ' + (transfer.offer?.source_name ?? '设备');
  }

  function transferState(transfer: PhoneTransfer): string {
    if (downloadingIds.includes(transfer.transfer_id)) return '正在保存';
    if (transfer.status === 'ready' && mode === 'web' && !canChooseDirectory) {
      const inFlight = (transfer.offer?.items ?? []).some((item) => downloadStarted(transfer, item) && !downloaded(transfer, item));
      if (inFlight) return '正在下载';
    }
    return transferStatusText(transfer, mode === 'android' ? 'android' : 'web');
  }
</script>

<header class="phone-header">
  <img class="phone-logo" src="/meshshare.svg" width="22" height="22" alt="" />
  <div class="phone-title">
    <strong title={node?.node_name}>{node?.node_name ?? 'MeshShare 节点'}</strong>
    <span class="phone-sub">{mode === 'web' ? '临时加入' : '手机节点'}{node?.addresses[0] ? ' · ' + node.addresses[0].address : ''}</span>
  </div>
  {#if node}
    <div class="phone-chips">
      <span class="chip">{platformLabel(node.platform)}</span>
      <span class="chip mono">:{node.port}</span>
    </div>
  {/if}
  <span class="node-status phone-status" data-kind={statusKind} role="status">
    <span class="status-dot" aria-hidden="true"></span>
    <span class="status-text">{statusKind === 'ok' ? '在线' : status}</span>
  </span>
</header>

<main class="phone-main">
  {#if pending.length || inbox}
    <section class="phone-card phone-inbox" aria-label={pending.length ? '待接收文件' : '正在接收文件'}>
      <div class="inbox-head">
        <span class="inbox-icon" aria-hidden="true"><Icon name="arrow-down" size={16} /></span>
        {#if pending.length}
          <div class="inbox-title">
            <strong aria-live="polite">{pending.length} 个待接收文件</strong>
            <span class="muted">{pendingItems} 项 · {formatSize(pendingBytes)}</span>
          </div>
        {:else if inbox}
          <div class="inbox-title">
            <strong aria-live="polite">来自 {inbox.offer?.source_name ?? '电脑'}</strong>
            <span class="muted">{transferItemsLabel(inbox)} · {transferState(inbox)}</span>
          </div>
        {/if}
      </div>
      {#each shownPending as transfer (transfer.transfer_id)}
        <article class="inbox-item">
          <div class="inbox-copy">
            <strong title={transfer.offer?.source_name}>{transfer.offer?.source_name ?? '电脑'} 想发送文件</strong>
            <span title={transferItemsLabel(transfer)}>{transferItemsLabel(transfer)}</span>
          </div>
          <div class="inbox-actions">
            <button type="button" class="btn btn-primary" disabled={actionBusyId === transfer.transfer_id} on:click={() => onAcceptTransfer(transfer)}>
              {actionBusyId === transfer.transfer_id ? '处理中…' : acceptLabel}
            </button>
            <button type="button" class="btn" disabled={actionBusyId === transfer.transfer_id} on:click={() => onRejectTransfer(transfer)}>拒绝</button>
          </div>
        </article>
      {/each}
      {#if hiddenPending > 0}
        <p class="inbox-more muted">还有 {hiddenPending} 个待确认，处理完前面的会依次显示。</p>
      {/if}
      {#if !pending.length && inbox}
        <article class="inbox-item">
          <div class="inbox-copy">
            <strong>{inbox.offer?.source_name ?? '电脑'} 发来的文件</strong>
            <span title={transferItemsLabel(inbox)}>{transferItemsLabel(inbox)}</span>
          </div>
          <div class="inbox-actions">
            {#if mode === 'web' && inbox.status === 'ready'}
              {#if canChooseDirectory}
                <button type="button" class="btn btn-primary" disabled={downloadingIds.includes(inbox.transfer_id)} on:click={() => onSaveToDirectory(inbox)}>
                  {receiveDirectoryName ? '保存到已选目录' : '选择目录并保存'}
                </button>
              {:else}
                {#each inbox.offer?.items ?? [] as item (item.item_id)}
                  {#if !downloaded(inbox, item) || canRetryDownload(inbox, item)}
                    <button type="button" class="btn btn-primary" on:click={() => onStartDownload(inbox, item)}>
                      {downloadStarted(inbox, item) || downloaded(inbox, item) ? '重新下载 ' + item.name : '下载 ' + item.name}
                    </button>
                  {/if}
                {/each}
              {/if}
            {/if}
            {#if inbox.status === 'accepted' || inbox.status === 'uploading' || (mode === 'web' && inbox.status === 'ready')}
              <button type="button" class="btn btn-ghost btn-sm" on:click={() => onCancelTransfer(inbox)}>
                {mode === 'android' ? '取消接收' : '取消'}
              </button>
            {/if}
          </div>
        </article>
      {/if}
    </section>
  {/if}

  {#if mode === 'android'}
    <AndroidUpdatePanel />
  {/if}

  <section class="phone-card">
    <div class="phone-card-head">
      <h2>在线设备</h2>
      <span class="chip">{peers.length}</span>
    </div>
    <ul class="phone-list" role="list">
      {#each peers as peer (peer.node_id)}
        <li class="device-card">
          <div class="device-card-id">
            <span class="device-icon" aria-hidden="true"><Icon name="monitor" size={18} /></span>
            <span class="device-id">
              <strong title={peer.node_name}>{peer.node_name}</strong>
              <span class="device-meta">{platformLabel(peer.platform)} · {peer.addresses[0]?.address ?? '在线'}</span>
            </span>
          </div>
          <div class="device-chips" title={peerSummary(peer)}>
            {#if peer.metrics_stale}
              <span class="metric muted">指标已过期</span>
            {:else if peer.metrics}
              <span class="metric">CPU <b>{formatPercent(peer.metrics.cpu_usage_percent)}</b></span>
              <span class="metric">内存 <b>{formatPercent(memoryPercent(peer.metrics))}</b></span>
              {#if gpuValue(peer) !== undefined}
                <span class="metric">GPU <b>{formatPercent(gpuValue(peer))}</b></span>
              {/if}
              <span class="metric">↓ <b>{formatRate(peer.metrics.network_down_bytes_per_sec)}</b></span>
              <span class="metric">↑ <b>{formatRate(peer.metrics.network_up_bytes_per_sec)}</b></span>
            {:else}
              <span class="metric muted">指标采集中</span>
            {/if}
          </div>
          <div class="device-actions">
            <button type="button" class="btn btn-primary" on:click={() => onSendFile(peer)}>
              <Icon name="send" size={14} />
              发送文件
            </button>
            {#if mode === 'web'}
              <button type="button" class="btn phone-folder" aria-label={'向 ' + peer.node_name + ' 发送文件夹'} on:click={() => onSendFolder(peer)}>
                <Icon name="folder" size={14} />
                文件夹
              </button>
            {/if}
          </div>
        </li>
      {:else}
        <li class="empty">暂时没有可发送的目标。启动另一台 MeshShare，或让更多设备通过「浏览器访问」加入。</li>
      {/each}
    </ul>
  </section>

  <section class="phone-card">
    <div class="phone-card-head">
      <h2>共享剪贴板</h2>
      {#if mode === 'web'}
        <div class="clip-tabs" role="tablist" aria-label="剪贴板视图">
          <button type="button" class="clip-tab" role="tab" aria-selected={clipboardTab === 'current'} on:click={() => (clipboardTab = 'current')}>当前</button>
          <button type="button" class="clip-tab" role="tab" aria-selected={clipboardTab === 'history'} on:click={() => (clipboardTab = 'history')}>历史 {history.length}</button>
        </div>
      {:else}
        <span class="muted">{clipboard ? clipboard.source_name + ' · ' + formatTime(clipboard.updated_at_ms) : '仅手动读取和发布'}</span>
      {/if}
    </div>
    {#if clipboardTab === 'history' && mode === 'web'}
      {#if selectedHistory}
        <div class="history-detail-meta">
          <strong>{selectedHistory.item.source_name}</strong>
          <span class="muted">{clipboardHistoryKind(selectedHistory.item)} · {formatTime(selectedHistory.item.updated_at_ms)}</span>
        </div>
        <ClipboardPreview snapshot={selectedHistory.snapshot} sizeBytes={selectedHistory.item.size_bytes} />
        <div class="phone-actions">
          <button type="button" class="btn btn-primary" on:click={onRestoreHistory} disabled={historyBusy}>恢复并同步</button>
          <button type="button" class="btn" on:click={onCloseHistory}>返回历史列表</button>
        </div>
      {:else}
        <div class="history-list">
          {#each history as item (item.event_id)}
            <button type="button" class="history-row" on:click={() => onOpenHistory(item)}>
              <span class="history-kind">{item.has_image ? 'PNG' : '文字'}</span>
              <span class="history-preview">{clipboardHistoryPreview(item)}</span>
              <span class="history-time">{formatTime(item.updated_at_ms)}</span>
            </button>
          {:else}
            <div class="empty">还没有保存的剪贴板历史。</div>
          {/each}
        </div>
        <div class="phone-actions">
          <button type="button" class="btn" on:click={onClearHistory} disabled={historyBusy || !history.length}>清空历史</button>
        </div>
      {/if}
    {:else}
      <textarea class="field" id="clipboard-editor" bind:value={draft} placeholder={mode === 'web' ? '在任意设备复制文字或图片，这里会显示最近内容' : '输入或粘贴文字，然后手动发布'}></textarea>
      {#if clipboard?.image_png}
        <ClipboardImage imagePng={clipboard.image_png} />
      {/if}
      <div class="phone-actions">
        <button type="button" class="btn btn-primary" on:click={onPublish}>同步到设备</button>
        <button type="button" class="btn" on:click={onReadLocal}>读取本机剪贴板</button>
        <button type="button" class="btn" on:click={onWriteLocal}>写入本机剪贴板</button>
      </div>
      <p class="hint">不会自动监听或回写系统剪贴板</p>
    {/if}
  </section>

  <section class="phone-card">
    <div class="phone-card-head">
      <h2>传输记录</h2>
      <span class="chip">{activeTransfers.length + prepares.length}</span>
      <button type="button" class="btn btn-ghost btn-sm phone-toggle" aria-expanded={transfersOpen} on:click={() => (transfersOpen = !transfersOpen)}>
        {transfersOpen ? '收起' : '展开'}
      </button>
    </div>
    {#if transfersOpen}
      <div class="phone-list">
        {#each prepares as prepare (prepare.prepare_id)}
          <article class="transfer-card">
            <span class="transfer-icon" aria-hidden="true">
              <Icon name="arrow-up" size={14} />
            </span>
            <div class="transfer-copy">
              <strong>发送到 {prepare.target_name}</strong>
              <span>{prepare.item_count} 个项目</span>
            </div>
            <div class="transfer-progress">
              <div
                class="progress"
                role="progressbar"
                aria-valuemin="0"
                aria-valuemax="100"
                aria-valuenow={Math.round(prepareProgressRatio(prepare) * 100)}
                aria-label="准备进度"
              >
                <span class="progress-fill" style="--p:{prepareProgressRatio(prepare)}"></span>
              </div>
              <span>{prepareStatusText(prepare)}</span>
            </div>
            <div class="transfer-actions">
              <button type="button" class="btn btn-ghost btn-sm" on:click={() => onCancelPrepare(prepare.prepare_id)}>取消</button>
            </div>
          </article>
        {/each}
        {#each activeTransfers as transfer (transfer.transfer_id)}
          <article class="transfer-card">
            <span class="transfer-icon" aria-hidden="true">
              <Icon name={transfer.direction === 'send' ? 'arrow-up' : 'arrow-down'} size={14} />
            </span>
            <div class="transfer-copy">
              <strong>{transferTitle(transfer)}</strong>
              <span title={transferItemsLabel(transfer)}>{transferItemsLabel(transfer)}</span>
            </div>
            <div class="transfer-progress">
              <div
                class="progress"
                role="progressbar"
                aria-valuemin="0"
                aria-valuemax="100"
                aria-valuenow={Math.round(ratio(transfer) * 100)}
                aria-label="传输进度"
              >
                <span class="progress-fill" style="--p:{ratio(transfer)}"></span>
              </div>
              <span>{transferState(transfer)}</span>
            </div>
            <div class="transfer-actions">
              {#if mode === 'web' && transfer.status === 'ready'}
                {#if canChooseDirectory}
                  <button type="button" class="btn btn-primary btn-sm" on:click={() => onSaveToDirectory(transfer)} disabled={downloadingIds.includes(transfer.transfer_id)}>
                    {receiveDirectoryName ? '保存到已选目录' : '选择目录并保存'}
                  </button>
                {:else}
                  {#each transfer.offer?.items ?? [] as item (item.item_id)}
                    {#if !downloaded(transfer, item) || canRetryDownload(transfer, item)}
                      <button type="button" class="btn btn-primary btn-sm" on:click={() => onStartDownload(transfer, item)}>
                        {downloadStarted(transfer, item) || downloaded(transfer, item) ? '重新下载 ' + item.name : '下载 ' + item.name}
                      </button>
                    {/if}
                  {/each}
                {/if}
              {/if}
              {#if transfer.status === 'accepted' || transfer.status === 'uploading' || (mode === 'web' && transfer.status === 'ready')}
                <button type="button" class="btn btn-ghost btn-sm" on:click={() => onCancelTransfer(transfer)}>
                  {mode === 'android' ? '取消接收' : '取消'}
                </button>
              {/if}
            </div>
          </article>
        {/each}
        {#if !prepares.length && !activeTransfers.length}
          <div class="empty">还没有传输任务。</div>
        {/if}
      </div>
    {/if}
    {#if recentDownloads.length}
      <div class="phone-card-head phone-subhead"><h3>最近下载</h3></div>
      <div class="phone-list">
        {#each recentDownloads as transfer (transfer.transfer_id)}
          <article class="transfer-card">
            <span class="transfer-icon" aria-hidden="true"><Icon name="arrow-down" size={14} /></span>
            <div class="transfer-copy"><strong>{transferTitle(transfer)}</strong></div>
            <div class="transfer-actions">
              {#each (transfer.offer?.items ?? []).filter((item) => canRetryDownload(transfer, item)) as item (item.item_id)}
                <button type="button" class="btn btn-sm" on:click={() => onStartDownload(transfer, item)}>重新下载 {item.name}</button>
              {/each}
            </div>
          </article>
        {/each}
      </div>
    {/if}
    {#if failures.length}
      <div class="phone-failures">
        <div class="phone-card-head phone-subhead"><h3>失败任务</h3><span class="chip chip--danger">{failures.length}</span></div>
        <div class="phone-list">
          {#each failures as failure (failure.progress.transfer_id)}
            <article class="transfer-card transfer-card--failure">
              <span class="transfer-icon transfer-icon--failure" aria-hidden="true"><Icon name="close" size={14} /></span>
              <div class="transfer-copy">
                <strong>{failure.progress.offer?.source_name ?? '文件传输'}</strong>
                <span>{transferItemsLabel(failure.progress)} · {formatTime(failure.failed_at_ms)}</span>
              </div>
              <div class="transfer-progress"><span title={failure.progress.error ?? '传输失败'}>{failure.progress.error ?? '传输失败'}</span></div>
              <div class="transfer-actions">
                {#if failure.retryable}<button type="button" class="btn btn-primary btn-sm" on:click={() => onRetryFailure(failure)}>重试</button>{/if}
                <button type="button" class="btn btn-ghost btn-sm" on:click={() => onDismissFailure(failure)}>移除</button>
              </div>
            </article>
          {/each}
        </div>
      </div>
    {/if}
    <div class="phone-card-head phone-subhead phone-history-head">
      <h3>传输历史</h3>
      <span class="chip">{historyEntries.length}</span>
      <button type="button" class="btn btn-ghost btn-sm" disabled={transferHistoryBusy || !historyEntries.length} on:click={onClearTransferHistory}>清空</button>
    </div>
    {#if historyEntries.length}
      <div class="phone-list">
        {#each historyEntries as entry (entry.transfer_id)}
          <article class="transfer-card">
            <span class="transfer-icon" aria-hidden="true">
              <Icon name={entry.direction === 'receive' ? 'arrow-down' : 'arrow-up'} size={14} />
            </span>
            <div class="transfer-copy">
              <strong title={entry.peer_node_id}>{transferHistoryPeerText(entry)}</strong>
              <span title={transferHistoryPathText(entry)}>{transferHistoryPathText(entry)}</span>
            </div>
            <div class="transfer-progress">
              <span>{transferHistoryStatusText(entry)}</span>
              <span class="muted">{entry.completed_items}/{entry.total_items} 个项目 · {formatTime(entry.finished_at_ms)}</span>
            </div>
          </article>
        {/each}
      </div>
    {:else}
      <div class="empty">还没有传输历史。</div>
    {/if}
  </section>

  <section class="phone-card phone-meta">
    <div class="phone-card-head">
      <h2>{mode === 'android' ? '本机网络' : '连接信息'}</h2>
    </div>
    {#if mode === 'android'}
      <div class="meta-grid">
        <div class="meta-tile">
          <span class="meta-label">上传</span>
          <strong>↑ {formatRate(metrics?.network_up_bytes_per_sec)}</strong>
        </div>
        <div class="meta-tile">
          <span class="meta-label">下载</span>
          <strong>↓ {formatRate(metrics?.network_down_bytes_per_sec)}</strong>
        </div>
      </div>
    {:else}
      <p class="phone-copy">
        {canChooseDirectory
          ? '接收文件前可以选择存储目录，文件会流式写入且不会覆盖同名文件。'
          : '浏览器会逐项下载到默认位置，下载完成后需要手动确认。HTTP 不提供链路加密和完整性保护，仅适用于受信任网络。'}
      </p>
      {#if canChooseDirectory}
        <div class="phone-actions">
          <button type="button" class="btn" on:click={onChooseDirectory}>{receiveDirectoryName ? '更换接收目录：' + receiveDirectoryName : '选择接收目录'}</button>
        </div>
      {/if}
    {/if}
    {#if mode === 'android'}
      <div class="meta-rows">
        <span class="muted">保存位置</span>
        <strong class="mono" title={receiveStorage?.directory ?? ''}>
          {receiveStorage?.public_download ? 'Download/MeshShare' : receiveStorage ? '应用私有目录' : '—'}
        </strong>
      </div>
      {#if receiveStorage && !receiveStorage.public_download}
        <div class="phone-actions">
          <button type="button" class="btn" on:click={onRequestReceiveStorage}>保存到手机下载目录</button>
        </div>
        <p class="hint">未授权「所有文件访问」时文件写在应用私有目录，文件管理器里不容易找到。</p>
      {/if}
    {/if}
    <div class="meta-rows">
      <span class="muted">节点</span><strong>{node?.node_name ?? '—'}</strong>
      <span class="muted">平台</span><strong>{platformLabel(node?.platform)}</strong>
      <span class="muted">地址</span><strong class="mono">{node?.addresses[0]?.address ?? '—'}{node ? ':' + node.port : ''}</strong>
    </div>
  </section>
</main>
