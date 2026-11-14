<script lang="ts">
  import { ICON_PATHS, type IconName } from '../icons';

  export let name: IconName;
  export let size = 16;
  export let stroke = 1.75;
</script>

<svg
  xmlns="http://www.w3.org/2000/svg"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width={stroke}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  focusable="false"
>
  {#each ICON_PATHS[name] ?? [] as path}
    <path d={path} />
  {/each}
</svg>
