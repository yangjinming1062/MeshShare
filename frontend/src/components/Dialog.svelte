<script lang="ts">
  import Icon from './Icon.svelte';

  export let open = false;
  export let title = '';
  export let subtitle = '';
  export let remote = false;
  export let width = 520;
  export let bodyClass = '';
  export let onClose: () => void = () => {};
  export let onOpened: () => void = () => {};

  let dialogEl: HTMLDialogElement;

  $: if (dialogEl) {
    if (open && !dialogEl.open) {
      dialogEl.showModal();
      onOpened();
    } else if (!open && dialogEl.open) dialogEl.close();
  }

  function handleCancel(event: Event) {
    event.preventDefault();
    onClose();
  }

  function handleBackdrop(event: MouseEvent) {
    if (event.target === dialogEl) onClose();
  }
</script>

<dialog bind:this={dialogEl} class="sheet" class:sheet--remote={remote} style="--sheet-w:{width}px" aria-label={title} on:cancel={handleCancel} on:click={handleBackdrop}>
  <div class="sheet-head">
    <div class="sheet-title">
      <h2>{title}</h2>
      {#if subtitle}<p class="sheet-subtitle">{subtitle}</p>{/if}
    </div>
    <button type="button" class="btn-icon" aria-label="关闭" on:click={onClose}>
      <Icon name="close" size={16} />
    </button>
  </div>
  <div class="sheet-body {bodyClass}">
    <slot />
  </div>
  <div class="sheet-foot">
    <slot name="footer" />
  </div>
</dialog>
