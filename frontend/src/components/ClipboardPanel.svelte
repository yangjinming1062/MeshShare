<script lang="ts">
  import ClipboardImage from './ClipboardImage.svelte';
  import { clipboardHistoryPreview, formatTime } from '../format';
  import type { ClipboardHistoryItem, ClipboardSnapshot } from '../types';

  export let clipboard: ClipboardSnapshot | null = null;
  export let draft = '';
  export let history: ClipboardHistoryItem[] = [];
  export let tab: 'current' | 'history' = 'current';
  export let busy = false;
  export let onPublish: () => void = () => {};
  export let onOpenHistory: (item: ClipboardHistoryItem) => void = () => {};
  export let onClearHistory: () => void = () => {};

  function handleTabKey(event: KeyboardEvent) {
    if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
    event.preventDefault();
    tab = tab === 'current' ? 'history' : 'current';
  }
</script>

<section class="surface area-clipboard">
  <div class="surface-head">
    <div class="surface-title">
      <h2>共享剪贴板</h2>
        <span class="muted">{clipboard ? clipboard.source_name + ' · ' + formatTime(clipboard.updated_at_ms) : '复制后自动同步'}</span>
    </div>
    <div class="surface-actions">
      <div class="clip-tabs" role="tablist" tabindex="-1" aria-label="剪贴板视图" on:keydown={handleTabKey}>
        <button type="button" class="clip-tab" role="tab" aria-selected={tab === 'current'} on:click={() => (tab = 'current')}>当前</button>
        <button type="button" class="clip-tab" role="tab" aria-selected={tab === 'history'} on:click={() => (tab = 'history')}>历史 {history.length}</button>
      </div>
    </div>
  </div>
  {#if tab === 'current'}
    <div class="surface-body">
      <div class="clip-grid">
        {#if clipboard?.image_png}
          <ClipboardImage imagePng={clipboard.image_png} />
        {/if}
        <textarea
          id="clipboard-editor"
          class="field clip-editor"
          bind:value={draft}
          placeholder="在任意设备复制文字或图片，这里会显示最近内容"
        ></textarea>
      </div>
      <div class="action-row">
        <button type="button" class="btn btn-primary btn-sm" disabled={!draft.trim()} on:click={onPublish}>立即同步当前文字</button>
        <span class="hint">复制会自动推送；这里用于同步编辑后的文字</span>
      </div>
    </div>
  {:else}
    <div class="surface-body">
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
      <div class="action-row">
        <button type="button" class="btn btn-ghost btn-sm" disabled={busy || !history.length} on:click={onClearHistory}>清空历史</button>
      </div>
    </div>
  {/if}
</section>
