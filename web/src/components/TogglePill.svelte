<script lang="ts">
  export let active = false;
  export let label = '';
  export let pressedLabel = '';
  export let disabled = false;
  export let onToggle: () => void = () => {};
</script>

<button type="button" class="pill" aria-pressed={active} {disabled} on:click={onToggle}>
  <span class="dot" aria-hidden="true"></span>
  {active ? label : (pressedLabel || label)}
</button>
