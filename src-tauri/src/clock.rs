use crate::model::Hlc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or_default()
}

pub struct LogicalClock {
    node_id: String,
    logical: AtomicU64,
}

impl LogicalClock {
    pub fn new(node_id: String) -> Self {
        Self {
            node_id,
            logical: AtomicU64::new(0),
        }
    }

    pub fn tick(&self) -> Hlc {
        let physical = now_ms();
        let logical = self
            .logical
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        Hlc {
            physical_ms: physical,
            logical,
            node_id: self.node_id.clone(),
        }
    }

    pub fn observe(&self, remote: &Hlc) -> Hlc {
        let physical = now_ms();
        let next = self.logical.load(Ordering::SeqCst).max(remote.logical);
        let logical = if physical > remote.physical_ms {
            0
        } else {
            next.saturating_add(1)
        };
        self.logical.store(logical, Ordering::SeqCst);
        Hlc {
            physical_ms: physical.max(remote.physical_ms),
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
}
