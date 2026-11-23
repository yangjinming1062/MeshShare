<script lang="ts">
  export let label: string;
  export let value: string;
  export let percent: number | null | undefined = undefined;
</script>

<span class="meter">
  <span class="meter-label">{label}</span>
  {#if percent != null}
    <span class="meter-track" aria-hidden="true">
      <span class="meter-fill" style:--p={Math.max(0, Math.min(1, percent / 100))}></span>
    </span>
  {/if}
  <b class="meter-value">{value}</b>
</span>
