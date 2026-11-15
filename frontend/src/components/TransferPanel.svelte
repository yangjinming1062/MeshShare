<script lang="ts">
  import Icon from './Icon.svelte';
  import { transferItemsLabel, transferStatusText } from '../format';
  import type { TransferProgress } from '../types';

  export let transfers: TransferProgress[] = [];
  export let onAccept: (transfer: TransferProgress) => void = () => {};
  export let onReject: (transfer: TransferProgress) => void = () => {};

  $: waiting = transfers.some((transfer) => transfer.direction === 'receive' && transfer.status === 'waiting');

  function ratio(transfer: TransferProgress): number {
    if (!transfer.total_bytes) return 0;
    return Math.max(0, Math.min(1, transfer.completed_bytes / transfer.total_bytes));
  }

</script>

<section class="surface area-transfers">
  <div class="surface-head">
    <div class="surface-title">
      <h2>传输任务</h2>
      <span class="chip">{transfers.length}</span>
      {#if waiting}
        <span class="status-dot status-dot--warning" aria-hidden="true"></span>
      {/if}
    </div>
    <div class="surface-actions">
      <span class="hint">文件只在两台设备之间直传</span>
    </div>
  </div>
  <div class="surface-body">
    <div class="transfer-list">
      {#each transfers as transfer (transfer.transfer_id)}
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
      {:else}
        <div class="empty">还没有传输任务。</div>
      {/each}
    </div>
  </div>
</section>
