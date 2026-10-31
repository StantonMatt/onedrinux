//! Eligibility for speculative reads, with no admission charge or seen-app row.

// test: read_ahead_never_runs_for_hosts_background_or_unidentified_even_when_allowed
// test: read_ahead_stops_on_policy_changes_without_charging_the_app

use super::{guarded, Caller, DownloadGuard};

/// Identified facts retained for one short scan; policy is rechecked before every fetch.
#[derive(Debug, Clone)]
pub(crate) struct ReadAheadCaller(Caller);

impl ReadAheadCaller {
    pub(crate) fn identity(&self) -> &str {
        &self.0.identity
    }

    #[cfg(test)]
    pub(crate) fn testing(identity: &str, reader: &str) -> Self {
        Self(Caller {
            identity: identity.into(),
            reader: reader.into(),
            display_name: identity.into(),
        })
    }
}

impl DownloadGuard {
    pub(crate) fn read_ahead_caller(&self, pid: u32) -> Option<ReadAheadCaller> {
        let caller = ReadAheadCaller(self.callers.identify(pid)?);
        self.permits_read_ahead(&caller).then_some(caller)
    }

    pub(crate) fn launch_process(&self, pid: u32) -> Option<super::IdentifiedProcess> {
        let process = self.callers.process(pid)?;
        self.permits_read_ahead(&ReadAheadCaller(process.caller.clone())).then_some(process)
    }

    pub(crate) fn permits_read_ahead(&self, caller: &ReadAheadCaller) -> bool {
        let inner = crate::panic::lock(&self.inner);
        let who = &caller.0;
        // An allowance never makes a program host or background reader speculative.
        !guarded(&who.identity, &inner.limits)
            && who.reader != "unidentified"
            && !inner.limits.background_readers.contains(&who.reader)
            && !inner.limits.blocked_apps.contains(&who.identity)
            && !inner
                .apps
                .get(&who.identity)
                .is_some_and(|app| app.blocked.is_some())
    }

    pub(crate) fn read_ahead_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.read_ahead_changes.subscribe()
    }
}

#[cfg(test)]
mod tests;
