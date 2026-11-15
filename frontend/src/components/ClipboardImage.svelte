<script lang="ts">
  import Icon from './Icon.svelte';
  import { formatSize } from '../format';

  export let imagePng: string;
  export let sizeBytes = 0;

  let viewerEl: HTMLDialogElement | null = null;
  let measured: { key: string; width: number; height: number } | null = null;

  // 图片内容变化后按 base64 比对，旧尺寸标注自动失效
  $: imageSrc = 'data:image/png;base64,' + imagePng;
  $: dimensions = measured && measured.key === imagePng ? measured : null;

  function measure(event: Event) {
    const image = event.currentTarget as HTMLImageElement;
    measured = { key: imagePng, width: image.naturalWidth, height: image.naturalHeight };
  }

  function openViewer() {
    if (viewerEl && !viewerEl.open) viewerEl.showModal();
  }

  function closeViewer(event: MouseEvent) {
    if (event.target === viewerEl) viewerEl?.close();
  }
</script>

<figure class="clip-preview">
  <button type="button" class="clip-preview-stage" aria-label="放大查看图片" on:click={openViewer}>
    <img src={imageSrc} alt="剪贴板图片" on:load={measure} />
  </button>
  <dialog bind:this={viewerEl} class="clip-viewer" aria-label="图片放大预览" on:click={closeViewer}>
    <img src={imageSrc} alt="剪贴板图片原图" />
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
