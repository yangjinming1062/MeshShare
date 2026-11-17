<script lang="ts">
  import Dialog from './Dialog.svelte';

  export let open = false;
  export let title = '输入路径';
  export let label = '';
  export let initialValue = '';
  export let onSubmit: (value: string) => void = () => {};
  export let onClose: () => void = () => {};

  let value = '';
  $: if (open) value = initialValue;

  function submit() {
    onSubmit(value.trim());
  }
</script>

<Dialog {open} {title} width={440} {onClose}>
  <form class="prompt-form" on:submit|preventDefault={submit}>
    {#if label}
      <label class="field-label" for="prompt-input">{label}</label>
    {/if}
    <input id="prompt-input" class="field mono" bind:value placeholder="目标节点上的绝对路径" />
  </form>
  <svelte:fragment slot="footer">
    <button type="button" class="btn" on:click={onClose}>取消</button>
    <button type="button" class="btn btn-primary" disabled={!value.trim()} on:click={submit}>确定</button>
  </svelte:fragment>
</Dialog>
