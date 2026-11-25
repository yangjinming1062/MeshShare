<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { formatPercent, formatRate, gpuMemoryPercent, gpuUtilization, isMetricsStale, memoryPercent } from '../format';
  import type { HardwareMetrics, MonitorCapabilities } from '../types';

  type MonitorView = {
    metrics: HardwareMetrics | null;
    capabilities: MonitorCapabilities;
    locked: boolean;
  };

  let view: MonitorView | null = null;
  $: metrics = isMetricsStale(view?.metrics) ? null : view?.metrics ?? null;

  onMount(() => {
    let active = true;
    let sampleTimer: number | undefined;
    let moveTimer: number | undefined;
    let pendingPosition: { x: number; y: number } | null = null;
    let positionWriteInFlight = false;
    let unlistenMove: (() => void) | undefined;

    async function sample() {
      try {
        const next = await invoke<MonitorView>('monitor_view');
        if (active) view = next;
      } catch {
        if (active) view = null;
      }
      if (active) sampleTimer = window.setTimeout(sample, 1000);
    }

    async function savePosition() {
      if (!active || !pendingPosition || positionWriteInFlight) return;
      const position = pendingPosition;
      pendingPosition = null;
      positionWriteInFlight = true;
      try {
        await invoke('set_monitor_position', position);
      } catch (error) {
        console.error('保存悬浮监控条位置失败', error);
      } finally {
        positionWriteInFlight = false;
        if (pendingPosition && moveTimer === undefined) void savePosition();
      }
    }

    void getCurrentWindow().onMoved(({ payload }) => {
      if (!active || !view?.capabilities.can_persist_position) return;
      pendingPosition = { x: payload.x, y: payload.y };
      window.clearTimeout(moveTimer);
      moveTimer = window.setTimeout(() => {
        moveTimer = undefined;
        void savePosition();
      }, 200);
    }).then((unlisten) => {
      if (active) unlistenMove = unlisten;
      else unlisten();
    }).catch((error) => console.error('监听悬浮监控条移动失败', error));

    void sample();
    return () => {
      active = false;
      window.clearTimeout(sampleTimer);
      window.clearTimeout(moveTimer);
      unlistenMove?.();
    };
  });

  async function openMenu(event: MouseEvent) {
    event.preventDefault();
    if (view?.locked) return;
    try {
      await invoke('popup_monitor_menu');
    } catch (error) {
      console.warn('打开悬浮监控条菜单失败', error);
    }
  }
</script>

<main data-tauri-drag-region="deep" on:contextmenu={openMenu}>
  <div><b>CPU</b><span>{formatPercent(metrics?.cpu_usage_percent)}</span></div>
  <div><b>GPU</b><span>{formatPercent(gpuUtilization(metrics))}</span></div>
  <div><b>↑</b><span>{formatRate(metrics?.network_up_bytes_per_sec)}</span></div>
  <div><b>内存</b><span>{formatPercent(memoryPercent(metrics))}</span></div>
  <div><b>显存</b><span>{formatPercent(gpuMemoryPercent(metrics))}</span></div>
  <div><b>↓</b><span>{formatRate(metrics?.network_down_bytes_per_sec)}</span></div>
</main>

<style>
  main {
    /* 切屏时实际视口可能暂时小于窗口的标称尺寸。 */
    --unit: min(1px, calc(100vw / 220), calc(100vh / 50));
    box-sizing: border-box;
    height: 100%;
    display: grid;
    grid-template-columns: repeat(3, max-content);
    justify-content: space-between;
    align-content: center;
    column-gap: calc(6 * var(--unit));
    row-gap: calc(2 * var(--unit));
    padding: calc(6 * var(--unit)) calc(10 * var(--unit));
    overflow: hidden;
    color: #fff;
    border: 1px solid #b9d0f53d;
    border-radius: 10px;
    background: rgba(13, 21, 38, .52);
    box-shadow: inset 0 1px 0 #ffffff1f;
    font-family: system-ui, sans-serif;
    line-height: 1.5;
    cursor: move;
    user-select: none;
  }
  div { min-width: 0; display: flex; align-items: baseline; gap: calc(4 * var(--unit)); white-space: nowrap; }
  b { color: #9db4d6; font-size: calc(10 * var(--unit)); font-weight: 700; letter-spacing: .06em; }
  span { font-family: "Cascadia Mono", "SF Mono", ui-monospace, Menlo, Consolas, "Liberation Mono", monospace; font-size: calc(11 * var(--unit)); font-variant-numeric: tabular-nums; }
</style>
