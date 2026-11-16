<script lang="ts">
  import Icon from './Icon.svelte';
  import { formatTime, isActiveTransferStatus, prepareProgressRatio, prepareStatusText, transferItemsLabel, transferStatusText } from '../format';
  import type { TransferFailure, TransferPrepare, TransferProgress } from '../types';

  export let transfers: TransferProgress[] = [];
  export let prepares: TransferPrepare[] = [];
  export let failures: TransferFailure[] = [];
  export let onAccept: (transfer: TransferProgress) => void = () => {};
  export let onReject: (transfer: TransferProgress) => void = () => {};
  export let onRetry: (failure: TransferFailure) => void = () => {};
  export let onDismissFailure: (failure: TransferFailure) => void = () => {};
  export let onCancelPrepare: (prepareId: string) => void = () => {};
  export let remoteMode = false;

  $: activeTransfers = transfers.filter((transfer) => isActiveTransferStatus(transfer.status));
  $: waiting = activeTransfers.some((transfer) => transfer.direction === 'receive' && transfer.status === 'waiting');

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
        <div class="empty">当前没有待处理或进行中的传输任务。</div>
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
  </div>
</section>
