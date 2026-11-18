<script lang="ts">
  import Icon from './Icon.svelte';
  import { formatTime, isActiveTransferStatus, prepareProgressRatio, prepareStatusText, transferHistoryLocation, transferHistoryPathText, transferHistoryPeerText, transferHistoryStatusText, transferItemsLabel, transferStatusText } from '../format';
  import type { TransferFailure, TransferHistoryEntry, TransferPrepare, TransferProgress } from '../types';

  export let transfers: TransferProgress[] = [];
  export let prepares: TransferPrepare[] = [];
  export let failures: TransferFailure[] = [];
  export let transferHistory: TransferHistoryEntry[] = [];
  export let transferHistoryBusy = false;
  export let onClearTransferHistory: () => void = () => {};
  export let onOpenLocation: (entry: TransferHistoryEntry) => void = () => {};
  export let onViewLocation: (entry: TransferHistoryEntry) => void = () => {};
  export let onAccept: (transfer: TransferProgress) => void = () => {};
  export let onReject: (transfer: TransferProgress) => void = () => {};
  export let onRetry: (failure: TransferFailure) => void = () => {};
  export let onDismissFailure: (failure: TransferFailure) => void = () => {};
  export let onCancelPrepare: (prepareId: string) => void = () => {};
  export let remoteMode = false;

  $: activeTransfers = transfers.filter((transfer) => isActiveTransferStatus(transfer.status));
  $: historyEntries = transferHistory.filter((entry) => !failures.some((failure) => failure.progress.transfer_id === entry.transfer_id));
  $: waiting = activeTransfers.some((transfer) => transfer.direction === 'receive' && transfer.status === 'waiting');
  $: hasContent = prepares.length > 0 || activeTransfers.length > 0 || failures.length > 0 || historyEntries.length > 0;

  function ratio(transfer: TransferProgress): number {
    if (!transfer.total_bytes) return 0;
    return Math.max(0, Math.min(1, transfer.completed_bytes / transfer.total_bytes));
  }

</script>

<section class="surface area-transfers">
  <div class="surface-head">
    <div class="surface-title">
      <h2>传输任务</h2>
      <span class="chip">{activeTransfers.length + prepares.length}</span>
      {#if waiting}
        <span class="status-dot status-dot--warning" aria-hidden="true"></span>
      {/if}
    </div>
    <div class="surface-actions">
      <span class="hint">{remoteMode ? '发送由目标机器读取文件后直传' : '文件只在两台设备之间直传'}</span>
    </div>
  </div>
  {#if hasContent}
  <div class="surface-body">
    <div class="transfer-list">
      {#each prepares as prepare (prepare.prepare_id)}
        <article class="transfer-row">
          <span class="transfer-icon" aria-hidden="true">
            <Icon name="arrow-up" size={14} />
          </span>
          <div class="transfer-copy">
            <strong title={prepare.target_name}>发送到 {prepare.target_name}</strong>
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
        <article class="transfer-row" class:waiting={transfer.direction === 'receive' && transfer.status === 'waiting'}>
          <span class="transfer-icon" aria-hidden="true">
            <Icon name={transfer.direction === 'receive' ? 'arrow-down' : 'arrow-up'} size={14} />
          </span>
          <div class="transfer-copy">
            <strong title={transfer.offer?.source_name ?? '文件传输'}>{transfer.offer?.source_name ?? '文件传输'}</strong>
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
            <span>{transferStatusText(transfer, 'desktop')}</span>
          </div>
          {#if transfer.direction === 'receive' && transfer.status === 'waiting'}
            <div class="transfer-actions">
              <button type="button" class="btn btn-primary btn-sm" on:click={() => onAccept(transfer)}>接收</button>
              <button type="button" class="btn btn-ghost btn-sm" on:click={() => onReject(transfer)}>拒绝</button>
            </div>
          {/if}
        </article>
      {/each}
      {#if !prepares.length && !activeTransfers.length}
        <div class="empty">没有进行中的传输。</div>
      {/if}
    </div>
    {#if failures.length}
      <div class="transfer-failures">
        <div class="transfer-subhead"><strong>失败任务</strong><span class="chip chip--danger">{failures.length}</span></div>
        <div class="transfer-list transfer-list--failures">
          {#each failures as failure (failure.progress.transfer_id)}
            <article class="transfer-row transfer-row--failure">
              <span class="transfer-icon transfer-icon--failure" aria-hidden="true"><Icon name="close" size={14} /></span>
              <div class="transfer-copy">
                <strong title={failure.progress.offer?.source_name ?? '文件传输'}>{failure.progress.offer?.source_name ?? '文件传输'}</strong>
                <span title={transferItemsLabel(failure.progress)}>{transferItemsLabel(failure.progress)} · {formatTime(failure.failed_at_ms)}</span>
              </div>
              <div class="transfer-progress"><span title={failure.progress.error ?? '传输失败'}>{failure.progress.error ?? '传输失败'}</span></div>
              <div class="transfer-actions">
                {#if failure.retryable}
                  <button type="button" class="btn btn-primary btn-sm" on:click={() => onRetry(failure)}>重试</button>
                {/if}
                <button type="button" class="btn btn-ghost btn-sm" on:click={() => onDismissFailure(failure)}>移除</button>
              </div>
            </article>
          {/each}
        </div>
      </div>
    {/if}
    {#if historyEntries.length}
    <div class="transfer-history">
      <div class="transfer-subhead">
        <strong>传输历史</strong>
        <span class="chip">{historyEntries.length}</span>
        <button type="button" class="btn btn-ghost btn-sm" disabled={transferHistoryBusy} on:click={onClearTransferHistory}>清空</button>
      </div>
      <div class="transfer-list transfer-list--history">
          {#each historyEntries as entry (entry.transfer_id)}
            {@const location = transferHistoryLocation(entry)}
            <article class="transfer-row transfer-row--history">
              <span class="transfer-icon" aria-hidden="true">
                <Icon name={entry.direction === 'receive' ? 'arrow-down' : 'arrow-up'} size={14} />
              </span>
              <div class="transfer-copy">
                <strong title={entry.peer_node_id}>{transferHistoryPeerText(entry)}</strong>
                <span title={transferHistoryPathText(entry)}>{transferHistoryPathText(entry)}</span>
              </div>
              <div class="transfer-progress">
                <span>{transferHistoryStatusText(entry)}</span>
                <span class="transfer-history-meta">{entry.completed_items}/{entry.total_items} 个项目 · {formatTime(entry.finished_at_ms)}</span>
              </div>
              {#if location}
                <div class="transfer-actions">
                  <button
                    type="button"
                    class="btn btn-ghost btn-sm"
                    title={remoteMode ? '在目标机器的文件浏览中查看' : '打开所在目录'}
                    on:click={() => (remoteMode ? onViewLocation(entry) : onOpenLocation(entry))}
                  >
                    <Icon name="folder" size={14} />
                    {remoteMode ? '查看位置' : '打开所在目录'}
                  </button>
                </div>
              {/if}
            </article>
          {/each}
        </div>
      </div>
    {/if}
  </div>
  {/if}
</section>
