<script lang="ts">
  import { tick } from 'svelte';
  import ClipboardPreview from './ClipboardPreview.svelte';
  import Dialog from './Dialog.svelte';
  import Icon from './Icon.svelte';
  import { clipboardHistoryThumbnailUrl } from '../api';
  import { filterClipboardHistory } from '../clipboardSearch';
  import { clipboardHistoryKind, formatHistoryTime, formatSize } from '../format';
  import type { ClipboardHistoryDetail, ClipboardHistoryItem } from '../types';

  export let open = false;
  export let items: ClipboardHistoryItem[] = [];
  export let selected: ClipboardHistoryDetail | null = null;
  export let detailLoading = false;
  export let busy = false;
  export let retentionLabel = '';
  export let onSelect: (item: ClipboardHistoryItem | null) => void = () => {};
  export let onRestore: () => void = () => {};
  export let onClear: () => void = () => {};
  export let onClose: () => void = () => {};

  let query = '';
  let failedThumbs = new Set<string>();
  let searchEl: HTMLInputElement | null = null;
  let listEl: HTMLDivElement | null = null;
  let lastQuery = '';
  let lastSelectedId = '';

  $: filtered = filterClipboardHistory(items, query);
  $: selectedId = selected?.item.event_id ?? '';
  $: if (!open && query) query = '';
  $: if (query !== lastQuery) {
    lastQuery = query;
    if (listEl) listEl.scrollTop = 0;
  }
  $: if (selectedId && selectedId !== lastSelectedId) {
    lastSelectedId = selectedId;
    void revealSelected();
  }

  function focusSearch() {
    searchEl?.focus();
  }

  function clearQuery() {
    query = '';
    searchEl?.focus();
  }

  async function revealSelected() {
    await tick();
    listEl?.querySelector<HTMLElement>('[aria-current="true"]')?.scrollIntoView({ block: 'nearest' });
  }

  function markThumbFailed(eventId: string) {
    failedThumbs = new Set(failedThumbs).add(eventId);
  }

  function handleSearchKey(event: KeyboardEvent) {
    // 输入法组合期间的按键属于候选词操作，不能用来切换历史选择。
    if (event.isComposing || event.keyCode === 229) return;
    if (!filtered.length) return;
    if (event.key === 'Enter') {
      if (!selectedId) {
        event.preventDefault();
        onSelect(filtered[0]);
      }
      return;
    }
    if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
    event.preventDefault();
    const current = filtered.findIndex((item) => item.event_id === selectedId);
    const next = event.key === 'ArrowDown'
      ? (current < 0 ? 0 : (current + 1) % filtered.length)
      : (current <= 0 ? filtered.length - 1 : current - 1);
    onSelect(filtered[next]);
  }

  function previewText(item: ClipboardHistoryItem): string {
    return item.text_preview?.trim() || (item.has_image ? '图片' : '空内容');
  }

  function rowLabel(item: ClipboardHistoryItem): string {
    return `${clipboardHistoryKind(item)}，${previewText(item)}，来源 ${item.source_name}，${formatHistoryTime(item.updated_at_ms)}`;
  }
</script>

<Dialog {open} title="剪贴板历史" width={840} bodyClass="sheet-body--clip-history" {onClose} onOpened={focusSearch}>
  <div class="clip-history" class:is-detail={!!selected}>
    <div class="clip-history-toolbar">
      <div class="clip-history-search">
        <span class="clip-history-search-icon" aria-hidden="true"><Icon name="search" size={14} /></span>
        <input
          bind:this={searchEl}
          class="field"
          type="search"
          placeholder="搜索文字、来源或类型"
          aria-label="搜索剪贴板历史"
          autocomplete="off"
          spellcheck="false"
          bind:value={query}
          on:keydown={handleSearchKey}
        />
        {#if query}
          <button type="button" class="btn-icon clip-history-search-clear" aria-label="清空搜索" on:click={clearQuery}>
            <Icon name="close" size={13} />
          </button>
        {/if}
      </div>
      <span class="clip-history-count" role="status">
        {#if query}{filtered.length} / {items.length} 条{:else}{items.length} 条{#if retentionLabel} · {retentionLabel}{/if}{/if}
      </span>
    </div>

    <div class="clip-history-panes">
      <div class="clip-history-list" bind:this={listEl}>
        {#each filtered as item (item.event_id)}
          <button
            type="button"
            class="clip-history-row"
            aria-current={selectedId === item.event_id ? 'true' : undefined}
            aria-label={rowLabel(item)}
            title={item.text_preview ?? ''}
            on:click={() => onSelect(item)}
          >
            <span class="clip-history-tile" aria-hidden="true">
              {#if item.has_image && !failedThumbs.has(item.event_id)}
                <img
                  src={clipboardHistoryThumbnailUrl(item.event_id)}
                  alt=""
                  loading="lazy"
                  decoding="async"
                  on:error={() => markThumbFailed(item.event_id)}
                />
              {:else if item.has_image}
                <span class="clip-history-tile-label">PNG</span>
              {:else}
                <span class="clip-history-tile-label">T</span>
              {/if}
            </span>
            <span class="clip-history-copy">
              <span class="clip-history-primary">{previewText(item)}</span>
              <span class="clip-history-meta">{item.source_name} · {formatSize(item.size_bytes)}</span>
            </span>
            <span class="clip-history-time">{formatHistoryTime(item.updated_at_ms)}</span>
          </button>
        {:else}
          {#if query}
            <div class="clip-history-placeholder">
              <strong>没有匹配「{query}」的记录</strong>
              <button type="button" class="btn btn-ghost btn-sm" on:click={clearQuery}>清空搜索</button>
            </div>
          {:else}
            <div class="clip-history-placeholder">
              <strong>还没有剪贴板历史</strong>
              <span>在任意设备复制文字或截图后，这里会保存最近内容。</span>
            </div>
          {/if}
        {/each}
      </div>

      <div class="clip-history-detail" aria-busy={detailLoading}>
        {#if selected}
          <div class="clip-history-detail-head">
            <button type="button" class="btn btn-ghost btn-sm clip-history-back" on:click={() => onSelect(null)}>
              <Icon name="arrow-left" size={14} /> 返回列表
            </button>
            <div class="clip-history-detail-meta">
              <strong>{selected.item.source_name}</strong>
              <span class="muted">
                {clipboardHistoryKind(selected.item)} · {formatHistoryTime(selected.item.updated_at_ms)} · {formatSize(selected.item.size_bytes)}
                {#if detailLoading} · 正在读取…{/if}
              </span>
            </div>
          </div>
          <div class="clip-history-detail-body">
            <ClipboardPreview snapshot={selected.snapshot} sizeBytes={selected.item.size_bytes} />
            <p class="hint">恢复会写入本机系统剪贴板，并向在线设备广播。</p>
          </div>
        {:else if detailLoading}
          <div class="clip-history-placeholder"><strong>正在读取…</strong></div>
        {:else}
          <div class="clip-history-placeholder">
            <strong>选择一条记录查看内容</strong>
            <span>可按 ↑ ↓ 切换</span>
          </div>
        {/if}
      </div>
    </div>
  </div>

  <svelte:fragment slot="footer">
    <button type="button" class="btn btn-ghost clip-history-clear" disabled={busy || !items.length} on:click={onClear}>清空历史</button>
    <button type="button" class="btn clip-history-footer-btn" on:click={onClose}>关闭</button>
    <button type="button" class="btn btn-primary clip-history-footer-btn" disabled={busy || detailLoading || !selected} on:click={onRestore}>恢复并同步</button>
  </svelte:fragment>
</Dialog>
