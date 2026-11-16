import assert from 'node:assert/strict';
import test from 'node:test';
import {
  diskUsedBytes,
  diskUsedPercent,
  formatDuration,
  formatFrequency,
  formatLoadAverage,
} from '../src/format.ts';

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
