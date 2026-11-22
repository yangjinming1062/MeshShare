import { Channel, invoke } from '@tauri-apps/api/core';
import type { TerminalEvent, TerminalTransport } from './terminal';

export function terminalTransport(id: string, peerId: string | null): TerminalTransport {
  return {
    open(cols, rows, receive) {
      const onEvent = new Channel<TerminalEvent>(receive);
      return invoke('terminal_open', { id, peerId, cols, rows, onEvent });
    },
    write: (data) => invoke('terminal_write', { id, data: Array.from(data) }),
    resize: (cols, rows) => invoke('terminal_resize', { id, cols, rows }),
    ack: (sequence) => invoke('terminal_ack', { id, sequence }),
    close: () => invoke('terminal_close', { id }),
  };
}
