//! Readiness gate for the interactive control socket.
//!
//! The control stream is deliberately opened only after the current QEMU
//! launch has produced a real main frame and two stable jog seqlock advances.
//! This prevents stale `ctrl.sock` consumers surviving a restart epoch.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaunchEpoch(u64);

pub struct ControlConnectionGate {
    epoch: AtomicU64,
    application_started_epoch: AtomicU64,
    main_ready_epoch: AtomicU64,
    jog_ready_epoch: AtomicU64,
    last_jog_seq: AtomicU32,
    jog_advances: AtomicU32,
}

impl ControlConnectionGate {
    pub fn new() -> Self {
        Self {
            epoch: AtomicU64::new(1),
            application_started_epoch: AtomicU64::new(0),
            main_ready_epoch: AtomicU64::new(0),
            jog_ready_epoch: AtomicU64::new(0),
            last_jog_seq: AtomicU32::new(0),
            jog_advances: AtomicU32::new(0),
        }
    }

    /// Start a fresh QEMU launch epoch and invalidate all prior readiness.
    pub fn begin_launch(&self) -> LaunchEpoch {
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.application_started_epoch.store(0, Ordering::Release);
        self.main_ready_epoch.store(0, Ordering::Release);
        self.jog_ready_epoch.store(0, Ordering::Release);
        self.last_jog_seq.store(0, Ordering::Release);
        self.jog_advances.store(0, Ordering::Release);
        LaunchEpoch(epoch)
    }

    /// Compatibility name for callers being migrated to launch-scoped tokens.
    pub fn begin_epoch(&self) -> LaunchEpoch {
        self.begin_launch()
    }

    /// Invalidate the active launch and all readiness observed for it.
    pub fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.application_started_epoch.store(0, Ordering::Release);
        self.main_ready_epoch.store(0, Ordering::Release);
        self.jog_ready_epoch.store(0, Ordering::Release);
        self.last_jog_seq.store(0, Ordering::Release);
        self.jog_advances.store(0, Ordering::Release);
    }

    /// Mark the serial milestone for `epoch`. A stale observer cannot release
    /// a later launch; display readiness is still required by `is_ready`.
    pub fn release(&self, epoch: LaunchEpoch) -> bool {
        if self.epoch.load(Ordering::Acquire) != epoch.0 {
            return false;
        }
        self.application_started_epoch
            .store(epoch.0, Ordering::Release);
        true
    }

    pub fn observe_main(&self, generation: u32, width: u32, height: u32) {
        if generation != 0 && width == 1280 && height == 720 {
            self.main_ready_epoch
                .store(self.epoch.load(Ordering::Acquire), Ordering::Release);
        }
    }

    pub fn observe_jog(&self, seq: u32) {
        if seq == 0 || seq & 1 != 0 {
            return;
        }
        let previous = self.last_jog_seq.swap(seq, Ordering::AcqRel);
        if previous != seq {
            let advances = self.jog_advances.fetch_add(1, Ordering::AcqRel) + 1;
            if advances >= 2 {
                self.jog_ready_epoch
                    .store(self.epoch.load(Ordering::Acquire), Ordering::Release);
            }
        }
    }

    pub fn is_ready(&self) -> bool {
        let epoch = self.epoch.load(Ordering::Acquire);
        self.application_started_epoch.load(Ordering::Acquire) == epoch
            && self.main_ready_epoch.load(Ordering::Acquire) == epoch
            && self.jog_ready_epoch.load(Ordering::Acquire) == epoch
    }

    pub fn is_current(&self, epoch: LaunchEpoch) -> bool {
        self.epoch.load(Ordering::Acquire) == epoch.0
    }
}

impl Default for ControlConnectionGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::ControlConnectionGate;

    #[test]
    fn requires_serial_main_and_two_jog_advances_per_epoch() {
        let gate = ControlConnectionGate::new();
        let epoch = gate.begin_launch();
        gate.observe_main(1, 1280, 720);
        gate.observe_jog(2);
        assert!(!gate.is_ready());
        gate.observe_jog(4);
        assert!(!gate.is_ready());
        assert!(gate.release(epoch));
        assert!(gate.is_ready());

        gate.begin_launch();
        assert!(!gate.is_ready());
    }

    #[test]
    fn stale_observer_cannot_release_a_new_launch() {
        let gate = ControlConnectionGate::new();
        let first = gate.begin_launch();
        let second = gate.begin_launch();
        assert!(!gate.release(first));
        assert!(!gate.is_ready());
        assert!(gate.release(second));
    }
}
