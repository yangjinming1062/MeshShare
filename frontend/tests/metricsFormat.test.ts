import assert from 'node:assert/strict';
import test from 'node:test';
import {
  diskUsedBytes,
  diskUsedPercent,
  formatDuration,
  formatFrequency,
  formatLoadAverage,
  formatPercent,
  formatRateCompact,
  gpuMemoryPercent,
} from '../src/format.ts';
import type { GpuMetrics, HardwareMetrics } from '../src/types.ts';

test('formats extended hardware metrics', () => {
  assert.equal(formatFrequency(4_200), '4.20 GHz');
  assert.equal(formatFrequency(800), '800 MHz');
  assert.equal(formatDuration(90_061), '1 天 1 小时 1 分钟 1 秒');
  assert.equal(formatLoadAverage({ one: 1.234, five: 0.5, fifteen: 0.25 }), '1.23 / 0.50 / 0.25');
});

test('calculates disk usage from bounded available space', () => {
  const disk = {
    name: 'System',
    mount_point: '/',
    file_system: 'ext4',
    total_bytes: 100,
    available_bytes: 40,
    removable: false,
  };
  assert.equal(diskUsedBytes(disk), 60);
  assert.equal(diskUsedPercent(disk), 60);
  assert.equal(diskUsedBytes({ ...disk, available_bytes: 120 }), 0);
});

test('the device list and monitor use only readable dedicated GPU memory', () => {
  const dedicated: GpuMetrics = { name: 'First GPU', memory_kind: 'dedicated', memory_used_bytes: 2, memory_total_bytes: 8 };
  const second: GpuMetrics = { ...dedicated, name: 'Second GPU', memory_used_bytes: 6 };
  const unified: GpuMetrics = { ...dedicated, name: 'Integrated GPU', memory_kind: 'unified', memory_used_bytes: 7 };
  const unknown: GpuMetrics = { ...dedicated, name: 'Unknown GPU', memory_kind: 'unknown', memory_used_bytes: 8 };
  const metrics: HardwareMetrics = { node_id: 'sample', sampled_at_ms: 0, gpus: [], disks: [] };

  assert.equal(gpuMemoryPercent({ ...metrics, gpus: [dedicated, second, unified, unknown] }), 75);
  assert.equal(formatPercent(gpuMemoryPercent({ ...metrics, gpus: [unified, unknown] })), '—');
  assert.equal(formatPercent(gpuMemoryPercent({ ...metrics, gpus: [{ ...dedicated, memory_used_bytes: undefined }] })), '—');
  assert.equal(formatPercent(gpuMemoryPercent({ ...metrics, gpus: [{ ...dedicated, memory_total_bytes: 0 }] })), '—');
  assert.equal(formatPercent(gpuMemoryPercent({ ...metrics, gpus: [{ ...dedicated, memory_used_bytes: 0 }] })), '0%');
});

test('compact network rates distinguish missing data, zero, and gigabytes per second', () => {
  assert.equal(formatRateCompact(null), '—');
  assert.equal(formatRateCompact(0), '0B');
  assert.equal(formatRateCompact(9 * 1024), '9K');
  assert.equal(formatRateCompact(9 * 1024 ** 2), '9.0M');
  assert.equal(formatRateCompact(2.5 * 1024 ** 3), '2.5G');
});
