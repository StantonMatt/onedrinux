//! One weakly captured mount listener. Sources release their locks before notifying.
use konedrive_tree::LocalItemId;
use std::sync::{Arc, Mutex};

type Listener = Arc<dyn Fn(Vec<LocalItemId>) + Send + Sync>;
#[derive(Default)]
pub(crate) struct StateChanges(Mutex<Option<Listener>>);
impl StateChanges {
    pub(crate) fn listen(&self, listener: Listener) {
        *crate::panic::lock(&self.0) = Some(listener);
    }
    pub(crate) fn changed(&self, items: Vec<LocalItemId>) {
        let listener = crate::panic::lock(&self.0).clone();
        if let Some(listener) = listener {
            listener(items);
        }
    }
}
