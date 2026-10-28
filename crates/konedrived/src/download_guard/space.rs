//! Whole-file allocations kept across concurrent downloads on the same filesystem.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Reserved {
    total: u64,
    /// The plan already counts its own running pins. Its free-space reader subtracts
    /// only allocations outside those steps, avoiding double counting.
    outside_plan: u64,
}
#[derive(Default)]
pub(crate) struct DiskReservations(Mutex<HashMap<u64, Reserved>>);
pub(crate) struct SpaceReservation {
    owner: Arc<DiskReservations>,
    filesystem: u64,
    bytes: u64,
    outside_plan: bool,
}

/// Unknown free space never authorizes a read-triggered whole-file allocation.
fn fits(free: Option<u64>, reserve: u64, running: u64, size: u64) -> bool {
    free.is_some_and(|free| free >= reserve && size <= free.saturating_sub(reserve).saturating_sub(running))
}

impl DiskReservations {
    pub(crate) fn reserve(self: &Arc<Self>, filesystem: u64, free: Option<u64>, reserve: u64, size: u64) -> Option<SpaceReservation> {
        let mut disks = crate::panic::lock(&self.0);
        let running = disks.entry(filesystem).or_default();
        if !fits(free, reserve, running.total, size) {
            return None;
        }
        running.total = running.total.saturating_add(size);
        running.outside_plan = running.outside_plan.saturating_add(size);
        Some(SpaceReservation { owner: Arc::clone(self), filesystem, bytes: size, outside_plan: true })
    }

    /// Pins were checked by the plan; explicit hydrate bypasses the read reserve. Both
    /// still count against FUTURE read admissions on the same disk, across accounts.
    pub(crate) fn keep(self: &Arc<Self>, filesystem: u64, size: u64, outside_plan: bool) -> SpaceReservation {
        let mut disks = crate::panic::lock(&self.0);
        let running = disks.entry(filesystem).or_default();
        running.total = running.total.saturating_add(size);
        if outside_plan {
            running.outside_plan = running.outside_plan.saturating_add(size);
        }
        SpaceReservation { owner: Arc::clone(self), filesystem, bytes: size, outside_plan }
    }

    pub(crate) fn available_to_plan(&self, filesystem: u64, free: u64) -> u64 {
        free.saturating_sub(crate::panic::lock(&self.0).get(&filesystem).map_or(0, |running| running.outside_plan))
    }
}
impl Drop for SpaceReservation {
    fn drop(&mut self) {
        let mut disks = crate::panic::lock(&self.owner.0);
        if let Some(running) = disks.get_mut(&self.filesystem) {
            running.total = running.total.saturating_sub(self.bytes);
            if self.outside_plan {
                running.outside_plan = running.outside_plan.saturating_sub(self.bytes);
            }
        }
    }
}
#[cfg(test)]
mod tests;
