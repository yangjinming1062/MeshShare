<script lang="ts">
  import ClipboardImage from './ClipboardImage.svelte';
  import { clipboardPreviewPath } from '../api';
  import { formatTime } from '../format';
  import type { ClipboardSnapshot } from '../types';

  export let headless = false;
  export let clipboard: ClipboardSnapshot | null = null;
  export let draft = '';
  export let onDraftBlur: () => void;
  export let historyCount = 0;
  export let onPublish: () => void = () => {};
  export let onOpenHistory: () => void = () => {};
</script>

<section class="surface area-clipboard">
  <div class="surface-head">
    <div class="surface-title">
      <h2>共享剪贴板</h2>
      <span class="muted">{clipboard ? clipboard.source_name + ' · ' + formatTime(clipboard.updated_at_ms) : headless ? '等待设备发布' : '复制后自动同步'}</span>
    </div>
    <div class="surface-actions">
      <button type="button" class="btn btn-ghost btn-sm" on:click={onOpenHistory}>历史 {historyCount}</button>
    </div>
  </div>
  <div class="surface-body">
    <div class="clip-grid">
      {#if clipboard?.image_png}
        <ClipboardImage imagePng={clipboard.image_png} previewPath={clipboardPreviewPath(clipboard)} />
      {/if}
      <textarea
        id="clipboard-editor"
        class="field clip-editor"
        bind:value={draft}
        on:blur={onDraftBlur}
        placeholder="在任意设备复制文字或图片，这里会显示最近内容"
      ></textarea>
    </div>
    <div class="action-row">
      <button type="button" class="btn btn-primary btn-sm" disabled={!draft.trim()} on:click={onPublish}>立即同步当前文字</button>
      <span class="hint">{headless ? '从此节点发布到已连接的设备' : '复制会自动推送；这里用于同步编辑后的文字'}</span>
    </div>
  </div>
</section>
