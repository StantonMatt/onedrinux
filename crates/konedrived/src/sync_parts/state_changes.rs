//! One weakly captured mount listener. Sources release their locks before notifying.
//! The listener also knows whether a completed download changed content, so its final
//! state and page-cache invalidation can share one announcement.
// test: a_pinned_files_fetch_announces_its_state_even_when_its_aggregate_stays_hydrating
use konedrive_tree::LocalItemId;
use std::sync::{Arc, Mutex};

type Listener = Arc<dyn Fn(Vec<LocalItemId>, bool) + Send + Sync>;
#[derive(Default)]
pub(crate) struct StateChanges(Mutex<Option<Listener>>);
impl StateChanges {
    pub(crate) fn listen(&self, listener: Listener) {
        *crate::panic::lock(&self.0) = Some(listener);
    }
    pub(crate) fn changed(&self, items: Vec<LocalItemId>) {
        self.notify(items, false);
    }
    /// A whole download landed: refresh its state and invalidate cached content in one
    /// announcement, after the running state has ended.
    pub(crate) fn content_changed(&self, items: Vec<LocalItemId>) {
        self.notify(items, true);
    }
    pub(crate) fn has_listener(&self) -> bool {
        crate::panic::lock(&self.0).is_some()
    }
    fn notify(&self, items: Vec<LocalItemId>, content_changed: bool) {
        let listener = crate::panic::lock(&self.0).clone();
        if let Some(listener) = listener {
            listener(items, content_changed);
        }
    }
}
