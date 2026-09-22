//! Readiness gate for the interactive control socket.
//!
//! The control stream is deliberately opened only after the current QEMU
//! launch has produced a real main frame and two stable jog seqlock advances.
//! This prevents stale `ctrl.sock` consumers surviving a restart epoch. The
//! serial Application Started marker is retained as a diagnostic milestone,
//! but is not a prerequisite for independent display readiness.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaunchEpoch(u64);

pub struct ControlConnectionGate {
    epoch: AtomicU64,
    application_started_epoch: AtomicU64,
    main_ready_epoch: AtomicU64,
    jog_ready_epoch: AtomicU64,
    last_jog_seq: AtomicU32,
    jog_advances: AtomicU32,
    jog_epoch_lock: Mutex<()>,
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
            jog_epoch_lock: Mutex::new(()),
        }
    }

    /// Start a fresh QEMU launch epoch and invalidate all prior readiness.
    pub fn begin_launch(&self) -> LaunchEpoch {
        let _guard = self.jog_epoch_lock.lock().unwrap();
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
        let _guard = self.jog_epoch_lock.lock().unwrap();
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.application_started_epoch.store(0, Ordering::Release);
        self.main_ready_epoch.store(0, Ordering::Release);
        self.jog_ready_epoch.store(0, Ordering::Release);
        self.last_jog_seq.store(0, Ordering::Release);
        self.jog_advances.store(0, Ordering::Release);
    }

    /// Mark the serial milestone for `epoch`. A stale observer cannot release
    /// a later launch. This is diagnostic only; display readiness is checked
    /// independently by `is_ready`.
    pub fn release(&self, epoch: LaunchEpoch) -> bool {
        if self.epoch.load(Ordering::Acquire) != epoch.0 {
            return false;
        }
        self.application_started_epoch
            .store(epoch.0, Ordering::Release);
        true
    }

    /// Snapshot the launch token when a display reader maps its SHM file.
    /// The token is carried with observations from that mapping until it is
    /// disconnected, so a reader on an old inode cannot ready a new launch.
    pub fn current_epoch(&self) -> LaunchEpoch {
        LaunchEpoch(self.epoch.load(Ordering::Acquire))
    }

    pub fn observe_main(&self, epoch: LaunchEpoch, generation: u32, width: u32, height: u32) {
        if self.is_current(epoch) && generation != 0 && width == 1280 && height == 720 {
            self.main_ready_epoch.store(epoch.0, Ordering::Release);
        }
    }

    pub fn observe_jog(&self, epoch: LaunchEpoch, seq: u32) {
        if seq == 0 || seq & 1 != 0 {
            return;
        }
        // Serialize with begin_launch/invalidate so a stale observation cannot
        // modify the new launch's jog sequence counters during the transition.
        let _guard = self.jog_epoch_lock.lock().unwrap();
        if !self.is_current(epoch) {
            return;
        }
        let previous = self.last_jog_seq.swap(seq, Ordering::AcqRel);
        if previous != seq {
            let advances = self.jog_advances.fetch_add(1, Ordering::AcqRel) + 1;
            if advances >= 2 {
                self.jog_ready_epoch.store(epoch.0, Ordering::Release);
            }
        }
    }

    pub fn is_ready(&self) -> bool {
        let epoch = self.epoch.load(Ordering::Acquire);
        self.main_ready_epoch.load(Ordering::Acquire) == epoch
            && self.jog_ready_epoch.load(Ordering::Acquire) == epoch
    }

    /// Whether the current launch has emitted the serial Application Started
    /// milestone. This is diagnostic only and does not affect readiness.
    pub fn application_started(&self) -> bool {
        let epoch = self.epoch.load(Ordering::Acquire);
        self.application_started_epoch.load(Ordering::Acquire) == epoch
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
    fn requires_main_and_two_jog_advances_without_serial_dependency() {
        let gate = ControlConnectionGate::new();
        let epoch = gate.begin_launch();
        gate.observe_main(epoch, 1, 1280, 720);
        gate.observe_jog(epoch, 2);
        assert!(!gate.is_ready());
        gate.observe_jog(epoch, 4);
        assert!(gate.is_ready());

        gate.begin_launch();
        assert!(!gate.is_ready());
    }

    #[test]
    fn readiness_order_is_independent() {
        let gate = ControlConnectionGate::new();
        let epoch = gate.begin_launch();
        gate.observe_jog(epoch, 2);
        gate.observe_jog(epoch, 4);
        assert!(!gate.is_ready());
        gate.observe_main(epoch, 1, 1280, 720);
        assert!(gate.is_ready());
    }

    #[test]
    fn invalid_main_and_duplicate_or_unstable_jog_samples_do_not_release_controls() {
        let gate = ControlConnectionGate::new();
        let epoch = gate.begin_launch();
        gate.observe_main(epoch, 0, 1280, 720);
        gate.observe_main(epoch, 2, 640, 480);
        gate.observe_jog(epoch, 0);
        gate.observe_jog(epoch, 1);
        gate.observe_jog(epoch, 2);
        gate.observe_jog(epoch, 2);
        assert!(!gate.is_ready());

        gate.observe_jog(epoch, 4);
        assert!(!gate.is_ready());
        gate.observe_main(epoch, 2, 1280, 720);
        assert!(gate.is_ready());
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

    #[test]
    fn old_mapping_observations_cannot_ready_a_new_launch() {
        let gate = ControlConnectionGate::new();
        let old_mapping = gate.begin_launch();
        assert_eq!(gate.current_epoch(), old_mapping);
        let new_mapping = gate.begin_launch();

        gate.observe_main(old_mapping, 2, 1280, 720);
        gate.observe_jog(old_mapping, 2);
        gate.observe_jog(old_mapping, 4);
        assert!(!gate.is_ready());

        gate.observe_main(new_mapping, 2, 1280, 720);
        gate.observe_jog(new_mapping, 2);
        assert!(!gate.is_ready());
        gate.observe_jog(new_mapping, 4);
        assert!(gate.is_ready());

        gate.invalidate();
        gate.observe_main(new_mapping, 4, 1280, 720);
        gate.observe_jog(new_mapping, 6);
        gate.observe_jog(new_mapping, 8);
        assert!(!gate.is_ready());
    }
}
