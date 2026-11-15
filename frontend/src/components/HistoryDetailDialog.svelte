<script lang="ts">
  import ClipboardPreview from './ClipboardPreview.svelte';
  import Dialog from './Dialog.svelte';
  import { clipboardHistoryKind, formatTime } from '../format';
  import type { ClipboardHistoryDetail } from '../types';

  export let open = false;
  export let detail: ClipboardHistoryDetail | null = null;
  export let busy = false;
  export let onRestore: () => void = () => {};
  export let onClose: () => void = () => {};
</script>

<Dialog {open} title="剪贴板历史" width={640} {onClose}>
  {#if detail}
    <div class="history-detail-meta">
      <strong>{detail.item.source_name}</strong>
      <span class="muted">{clipboardHistoryKind(detail.item)} · {formatTime(detail.item.updated_at_ms)}</span>
    </div>
    <ClipboardPreview snapshot={detail.snapshot} sizeBytes={detail.item.size_bytes} />
    <p class="hint">恢复会写入本机系统剪贴板，并向在线设备广播新事件。</p>
  {:else}
    <div class="empty">历史条目已不存在。</div>
  {/if}
  <svelte:fragment slot="footer">
    <button type="button" class="btn" on:click={onClose}>关闭</button>
    <button type="button" class="btn btn-primary" disabled={busy || !detail} on:click={onRestore}>恢复并同步</button>
  </svelte:fragment>
</Dialog>
