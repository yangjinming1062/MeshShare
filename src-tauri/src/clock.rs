use crate::model::Hlc;
use parking_lot::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or_default()
}

pub struct LogicalClock {
    node_id: String,
    state: Mutex<ClockState>,
}

#[derive(Debug, Default)]
struct ClockState {
    last_physical_ms: i64,
    last_logical: u64,
}

impl LogicalClock {
    pub fn new(node_id: String) -> Self {
        Self {
            node_id,
            state: Mutex::new(ClockState::default()),
        }
    }

    pub fn tick(&self) -> Hlc {
        let mut state = self.state.lock();
        let wall_clock = now_ms();
        let physical = wall_clock.max(state.last_physical_ms);
        let logical = if physical > state.last_physical_ms {
            0
        } else {
            state.last_logical.saturating_add(1)
        };
        state.last_physical_ms = physical;
        state.last_logical = logical;
        Hlc {
            physical_ms: physical,
            logical,
            node_id: self.node_id.clone(),
        }
    }

    pub fn observe(&self, remote: &Hlc) -> Hlc {
        let mut state = self.state.lock();
        let wall_clock = now_ms();
        let physical = wall_clock
            .max(state.last_physical_ms)
            .max(remote.physical_ms);
        let logical = if physical == state.last_physical_ms && physical == remote.physical_ms {
            state.last_logical.max(remote.logical).saturating_add(1)
        } else if physical == state.last_physical_ms {
            state.last_logical.saturating_add(1)
        } else if physical == remote.physical_ms {
            remote.logical.saturating_add(1)
        } else {
            0
        };
        state.last_physical_ms = physical;
        state.last_logical = logical;
        Hlc {
            physical_ms: physical,
            logical,
            node_id: self.node_id.clone(),
        }
    }
}

pub fn now_millis() -> i64 {
    now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_are_monotonic_for_one_node() {
        let clock = LogicalClock::new("node-a".to_string());
        let first = clock.tick();
        let second = clock.tick();
        assert!(second.compare(&first).is_gt());
    }

    #[test]
    fn observing_remote_clock_moves_local_clock_forward() {
        let clock = LogicalClock::new("node-a".to_string());
        let remote = Hlc {
            physical_ms: now_millis() + 10_000,
            logical: 9,
            node_id: "node-b".to_string(),
        };
        let local = clock.observe(&remote);
        assert!(local.compare(&remote).is_gt());
        assert_eq!(local.node_id, "node-a");
    }

    #[test]
    fn tick_keeps_remote_physical_time_until_wall_clock_catches_up() {
        let clock = LogicalClock::new("node-a".to_string());
        let remote = Hlc {
            physical_ms: now_millis() + 10_000,
            logical: 9,
            node_id: "node-b".to_string(),
        };
        let observed = clock.observe(&remote);
        let next = clock.tick();
        assert!(next.physical_ms >= observed.physical_ms);
        assert!(next.compare(&observed).is_gt());
    }
}
