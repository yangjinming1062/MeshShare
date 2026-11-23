<script lang="ts">
  import { onDestroy } from 'svelte';
  import Dialog from './Dialog.svelte';
  import Icon from './Icon.svelte';
  import { browseManagedPath, getManagedRoots, type RequestTarget } from '../api';
  import { formatSize } from '../format';
  import type { BrowseEntry, BrowseRoots } from '../types';

  export let open = false;
  /** send：多选文件与文件夹；directory：把当前目录作为选择结果；view：只读浏览位置。 */
  export let mode: 'send' | 'directory' | 'view' = 'send';
  export let target: RequestTarget;
  export let targetLabel = '';
  export let targetAddress = '';
  export let startPath = '';
  export let onConfirm: (paths: string[]) => void = () => {};
  export let onFallback: (() => void) | null = null;
  export let onClose: () => void = () => {};

  let roots: BrowseRoots | null = null;
  let currentPath = '';
  let requestedPath = '';
  let parent: string | null = null;
  let entries: BrowseEntry[] = [];
  let truncated = false;
  let loading = false;
  let loadError = '';
  let selected: string[] = [];
  let activeRead: AbortController | null = null;
  let wasOpen = false;

  $: label = targetLabel || '目标机器';
  $: displayPath = currentPath || requestedPath;
  $: canConfirm = mode !== 'view' && !loading && (mode === 'directory' ? !!currentPath && !loadError : selected.length > 0);
  $: if (open !== wasOpen) {
    wasOpen = open;
    reset();
    if (open) void load(startPath);
  }

  onDestroy(cancelRead);

  function cancelRead() {
    activeRead?.abort();
    activeRead = null;
  }

  function reset() {
    cancelRead();
    roots = null;
    currentPath = '';
    requestedPath = '';
    parent = null;
    entries = [];
    truncated = false;
    loading = false;
    loadError = '';
    selected = [];
  }

  async function load(path: string) {
    cancelRead();
    const read = new AbortController();
    activeRead = read;
    const { signal } = read;
    const source = target;
    requestedPath = path;
    loading = true;
    loadError = '';
    try {
      if (!roots) {
        const next = await getManagedRoots(source, signal);
        if (signal.aborted) return;
        path = path || next.home || next.roots[0]?.path || '';
        if (!path) throw new Error('目标机器没有可浏览的目录');
        roots = next;
      }
      requestedPath = path;
      const listing = await browseManagedPath(path, source, signal);
      if (signal.aborted) return;
      currentPath = listing.path;
      parent = listing.parent;
      entries = listing.entries;
      truncated = listing.truncated;
    } catch (cause) {
      if (signal.aborted) return;
      entries = [];
      loadError = describeError(cause);
    } finally {
      if (activeRead === read) {
        activeRead = null;
        loading = false;
      }
    }
  }

  function describeError(cause: unknown): string {
    if (cause instanceof DOMException && cause.name === 'AbortError') return '读取目录超时，请重试';
    return cause instanceof Error && cause.message ? cause.message : '无法读取目标机器目录';
  }

  function isSelected(entry: BrowseEntry): boolean {
    return selected.includes(entry.path);
  }

  function toggle(entry: BrowseEntry) {
    if (mode !== 'send') return;
    selected = isSelected(entry)
      ? selected.filter((value) => value !== entry.path)
      : [...selected, entry.path];
  }

  function confirm() {
    if (!canConfirm) return;
    if (mode === 'directory') onConfirm([currentPath]);
    else if (selected.length) onConfirm(selected);
  }
</script>

<Dialog
  {open}
  title={mode === 'send' ? '从 ' + label + ' 选择要发送的文件' : mode === 'view' ? '查看 ' + label + ' 上的文件' : '在 ' + label + ' 上选择接收目录'}
  subtitle={targetAddress}
  remote
  width={620}
  {onClose}
>
  <div class="browse">
    <div class="browse-bar">
      <button
        type="button"
        class="btn-icon"
        title="上一级"
        aria-label="上一级"
        disabled={!parent || loading}
        on:click={() => parent && void load(parent)}
      >
        <Icon name="arrow-left" size={16} />
      </button>
      <span class="browse-path mono" title={displayPath}>{displayPath || '—'}</span>
    </div>
    {#if roots && roots.roots.length}
      <div class="browse-roots" aria-label="常用位置">
        {#each roots.roots as root (root.path)}
          <button type="button" class="btn btn-ghost btn-sm" disabled={loading} on:click={() => void load(root.path)}>
            {root.label}
          </button>
        {/each}
      </div>
    {/if}
    {#if loadError}
      <div class="action-row">
        <p class="browse-error" role="alert">{loadError}</p>
        <button type="button" class="btn btn-sm" on:click={() => void load(requestedPath)}>重试</button>
      </div>
      {#if loadError.includes('远程文件访问')}
        <p class="hint">到 {label} 的 MeshShare 设置里打开「允许远程浏览本机文件」后重试。</p>
      {/if}
    {:else if loading}
      <p class="browse-empty" role="status">正在读取目录…</p>
    {:else if !entries.length}
      <p class="browse-empty">这个目录是空的。</p>
    {:else}
      <ul class="browse-list" role="list">
        {#each entries as entry (entry.path)}
          <li class="browse-row" class:selected={isSelected(entry)}>
            {#if mode === 'send'}
              <button
                type="button"
                class="browse-pick"
                aria-pressed={isSelected(entry)}
                aria-label={isSelected(entry) ? '取消选择 ' + entry.name : '选择 ' + entry.name}
                on:click={() => toggle(entry)}
              >
                <span class="browse-check" aria-hidden="true">{isSelected(entry) ? '✓' : ''}</span>
                <Icon name={entry.directory ? 'folder' : 'file'} size={16} />
                <span class="browse-name" title={entry.name}>{entry.name}</span>
                <span class="muted">{entry.directory ? '文件夹' : formatSize(entry.size)}</span>
              </button>
              {#if entry.directory}
                <button type="button" class="btn btn-ghost btn-sm" on:click={() => void load(entry.path)}>打开</button>
              {/if}
            {:else if entry.directory}
              <button type="button" class="browse-pick" on:click={() => void load(entry.path)}>
                <Icon name="folder" size={16} />
                <span class="browse-name" title={entry.name}>{entry.name}</span>
              </button>
            {:else}
              <span class="browse-pick muted">
                <Icon name="file" size={16} />
                <span class="browse-name" title={entry.name}>{entry.name}</span>
              </span>
            {/if}
          </li>
        {/each}
      </ul>
      {#if truncated}
        <p class="hint">目录项目较多，只显示前 {entries.length} 项；进入更具体的目录可以继续查看。</p>
      {/if}
    {/if}
  </div>
  <svelte:fragment slot="footer">
    {#if mode === 'view'}
      <button type="button" class="btn btn-primary" on:click={onClose}>关闭</button>
    {:else}
      {#if onFallback && mode === 'directory'}
        <button type="button" class="btn btn-ghost" on:click={onFallback}>手动输入路径</button>
      {/if}
      <button type="button" class="btn btn-ghost" on:click={onClose}>取消</button>
      <button type="button" class="btn btn-primary" disabled={!canConfirm} on:click={confirm}>
        {#if mode === 'send'}发送 {selected.length} 项{:else}使用当前目录{/if}
      </button>
    {/if}
  </svelte:fragment>
</Dialog>
