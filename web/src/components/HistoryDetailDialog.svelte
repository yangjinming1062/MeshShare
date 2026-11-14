<script lang="ts">
  import Dialog from './Dialog.svelte';
  import { clipboardHistoryKind, formatTime } from '../format';
  import type { ClipboardHistoryDetail } from '../types';

  export let open = false;
  export let detail: ClipboardHistoryDetail | null = null;
  export let busy = false;
  export let onRestore: () => void = () => {};
  export let onClose: () => void = () => {};
</script>

<Dialog {open} title="剪贴板历史" width={560} {onClose}>
  {#if detail}
    <div class="history-detail-meta">
      <strong>{detail.item.source_name}</strong>
      <span class="muted">{clipboardHistoryKind(detail.item)} · {formatTime(detail.item.updated_at_ms)}</span>
    </div>
    <div class="history-detail-grid">
      <textarea class="field" readonly value={detail.snapshot.text ?? ''} placeholder="图片内容"></textarea>
      {#if detail.snapshot.image_png}
        <img class="clip-image" src={'data:image/png;base64,' + detail.snapshot.image_png} alt="历史共享图片" />
      {/if}
    </div>
    <p class="hint">恢复会写入本机系统剪贴板，并向在线设备广播新事件。</p>
  {:else}
    <div class="empty">历史条目已不存在。</div>
  {/if}
  <svelte:fragment slot="footer">
    <button type="button" class="btn" on:click={onClose}>关闭</button>
    <button type="button" class="btn btn-primary" disabled={busy || !detail} on:click={onRestore}>恢复并同步</button>
  </svelte:fragment>
</Dialog>
