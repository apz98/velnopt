//! AT-SPI accessibility for one panel surface, through AccessKit.

use std::sync::{Arc, Mutex};

use accesskit::{ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, TreeUpdate};
use accesskit_unix::Adapter;
use calloop::channel::Sender;

/// An assistive-technology request for the panel surface with this identifier.
pub(crate) type Request = (u64, ActionRequest);

/// The adapter registers with the accessibility bus lazily and serves the latest
/// tree. Handlers run on AccessKit's own thread and only forward requests.
pub(crate) struct Access {
    adapter: Adapter,
    latest: Arc<Mutex<Option<TreeUpdate>>>,
}

impl Access {
    pub(crate) fn new(surface: u64, requests: Sender<Request>) -> Self {
        let latest = Arc::new(Mutex::new(None));
        let adapter = Adapter::new(
            Activation(Arc::clone(&latest)),
            Actions { surface, requests },
            Deactivation,
        );
        Self { adapter, latest }
    }

    /// Publish a full tree built from the frame that was just drawn.
    pub(crate) fn update(&mut self, tree: TreeUpdate) {
        *self
            .latest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tree.clone());
        self.adapter.update_if_active(|| tree);
    }

    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.adapter.update_window_focus_state(focused);
    }
}

struct Activation(Arc<Mutex<Option<TreeUpdate>>>);

impl ActivationHandler for Activation {
    /// There is no tree before the first frame; the first draw sends it.
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

struct Actions {
    surface: u64,
    requests: Sender<Request>,
}

impl ActionHandler for Actions {
    fn do_action(&mut self, request: ActionRequest) {
        // closed channel: the panel is shutting down
        let _ = self.requests.send((self.surface, request));
    }
}

struct Deactivation;

impl DeactivationHandler for Deactivation {
    fn deactivate_accessibility(&mut self) {}
}
