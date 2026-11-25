<script lang="ts">
  import { onDestroy } from 'svelte';
  import Icon from './Icon.svelte';
  import { apiUrl } from '../api';
  import { formatSize } from '../format';

  const PREVIEW_PIXELS = 4 * 1024 * 1024;
  const PREVIEW_EDGE = 720;
  type Dimensions = { width: number; height: number };

  export let imagePng: string;
  export let sizeBytes = 0;
  export let previewPath: string | undefined = undefined;

  let viewerEl: HTMLDialogElement | null = null;
  let viewerOpen = false;
  let previewSrc = '';
  let previewFromNode = false;
  let rendering = false;
  let pendingPreview: { encoded: string; size: Dimensions; version: number } | null = null;
  let previewVersion = 0;

  $: dimensions = pngDimensions(imagePng);
  $: updatePreview(imagePng, dimensions, previewPath);
  onDestroy(() => {
    previewVersion += 1;
    pendingPreview = null;
  });

  function pngDimensions(encoded: string): Dimensions | null {
    try {
      const header = atob(encoded.slice(0, 32));
      if (header.length < 24 || header.slice(12, 16) !== 'IHDR') return null;
      const data = new DataView(Uint8Array.from(header, character => character.charCodeAt(0)).buffer);
      return { width: data.getUint32(16), height: data.getUint32(20) };
    } catch {
      return null;
    }
  }

  function imageUrl(encoded: string): string {
    return 'data:image/png;base64,' + encoded;
  }

  function pngBlob(encoded: string): Blob {
    const decoded = atob(encoded);
    const bytes = new Uint8Array(decoded.length);
    for (let i = 0; i < decoded.length; i += 1) bytes[i] = decoded.charCodeAt(i);
    return new Blob([bytes], { type: 'image/png' });
  }

  function updatePreview(encoded: string, size: Dimensions | null, path?: string) {
    const version = ++previewVersion;
    previewFromNode = false;
    pendingPreview = null;
    if (!size || size.width * size.height <= PREVIEW_PIXELS) {
      previewSrc = imageUrl(encoded);
      return;
    }
    if (path) {
      try {
        previewSrc = apiUrl(path);
        previewFromNode = true;
        return;
      } catch {
        // 切换节点期间仍可使用当前快照渲染。
      }
    }
    renderLocalPreview(encoded, size, version);
  }

  function renderLocalPreview(encoded: string, size: Dimensions, version: number) {
    if (typeof createImageBitmap !== 'function') {
      previewSrc = imageUrl(encoded);
      return;
    }
    previewSrc = '';
    pendingPreview = { encoded, size, version };
    if (!rendering) void renderPreviews();
  }

  function retryPreview() {
    if (!previewFromNode || !dimensions) return;
    previewFromNode = false;
    renderLocalPreview(imagePng, dimensions, previewVersion);
  }

  async function renderPreviews() {
    rendering = true;
    // 连续复制期间只保留一张正在解码的图片和一个最新待处理项。
    while (pendingPreview) {
      const { encoded, size, version } = pendingPreview;
      pendingPreview = null;
      let bitmap: ImageBitmap | undefined;
      try {
        const scale = PREVIEW_EDGE / Math.max(size.width, size.height);
        bitmap = await createImageBitmap(pngBlob(encoded), {
          resizeWidth: Math.max(1, Math.round(size.width * scale)),
          resizeHeight: Math.max(1, Math.round(size.height * scale)),
        });
        if (version !== previewVersion) continue;
        const canvas = document.createElement('canvas');
        canvas.width = bitmap.width;
        canvas.height = bitmap.height;
        const context = canvas.getContext('2d');
        if (!context) throw new Error('无法绘制剪贴板预览');
        context.drawImage(bitmap, 0, 0);
        previewSrc = canvas.toDataURL();
        canvas.width = 0;
        canvas.height = 0;
      } catch {
        if (version === previewVersion) previewSrc = imageUrl(encoded);
      } finally {
        bitmap?.close();
      }
    }
    rendering = false;
  }

  function openViewer() {
    if (viewerEl && !viewerEl.open) {
      viewerEl.showModal();
      viewerOpen = true;
    }
  }

  function closeViewer(event: MouseEvent) {
    if (event.target === viewerEl) viewerEl?.close();
  }
</script>

<figure class="clip-preview">
  <button type="button" class="clip-preview-stage" aria-label="放大查看图片" aria-busy={!previewSrc} on:click={openViewer}>
    {#if previewSrc}{#key previewSrc}<img src={previewSrc} alt="剪贴板图片" decoding="async" on:error={retryPreview} />{/key}{/if}
  </button>
  <dialog bind:this={viewerEl} class="clip-viewer" aria-label="图片放大预览" on:click={closeViewer} on:close={() => { viewerOpen = false; }}>
    {#if viewerOpen}<img src={imageUrl(imagePng)} alt="剪贴板图片原图" />{/if}
    <button type="button" class="btn-icon clip-viewer-close" aria-label="关闭图片预览" on:click={() => viewerEl?.close()}>
      <Icon name="close" size={16} />
    </button>
  </dialog>
  <figcaption class="clip-preview-caption">
    {#if dimensions}<span>{dimensions.width} × {dimensions.height}</span>{/if}
    {#if sizeBytes}<span>{formatSize(sizeBytes)}</span>{/if}
    <span class="clip-preview-open">点击放大查看</span>
  </figcaption>
</figure>
