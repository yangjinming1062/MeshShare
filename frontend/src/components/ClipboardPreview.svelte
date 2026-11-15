<script lang="ts">
  import ClipboardImage from './ClipboardImage.svelte';
  import type { ClipboardSnapshot } from '../types';

  export let snapshot: ClipboardSnapshot;
  export let sizeBytes = 0;
</script>

{#if snapshot.image_png}
  <ClipboardImage imagePng={snapshot.image_png} {sizeBytes} />
{/if}
{#if snapshot.text}
  <textarea class="field clip-preview-text" readonly value={snapshot.text}></textarea>
{:else if !snapshot.image_png}
  <div class="empty">这条历史没有可预览的内容。</div>
{/if}
