<script lang="ts">
  import Dialog from './Dialog.svelte';

  export let open = false;
  export let title = '确认操作';
  export let message = '';
  export let confirmLabel = '确认';
  export let onConfirm: () => void = () => {};
  export let onCancel: () => void = () => {};
</script>

<Dialog {open} {title} width={400} onClose={onCancel}>
  <p class="confirm-text">{message}</p>
  <svelte:fragment slot="footer">
    <button type="button" class="btn" on:click={onCancel}>取消</button>
    <button type="button" class="btn btn-danger" on:click={onConfirm}>{confirmLabel}</button>
  </svelte:fragment>
</Dialog>
