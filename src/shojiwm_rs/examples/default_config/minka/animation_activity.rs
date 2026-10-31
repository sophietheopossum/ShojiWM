//! Which window animations are running, so the glass can pause while they
//! do. A port of dcrey7's `animation-activity.ts`.
//!
//! The liquid-glass blur behind a moving window must be redone on every
//! frame. When something else keeps the GPU busy, that extra work makes
//! the animation drop frames (he measured Magic Lamp late frames at
//! 7-10% with the glass, 2-3% paused). Animations mark themselves running
//! here; rendering suspends the backdrop blur while any of them is, and
//! the glass comes back when the last one ends.

use std::{cell::RefCell, collections::HashSet, rc::Rc};

use shojiwm_rs::prelude::*;

/// The running animations, as `kind:window_id` keys. Kept signal-free so
/// the bookkeeping can be tested without a compositor runtime.
#[derive(Default)]
struct ActivityKeys(HashSet<String>);

impl ActivityKeys {
    /// Mark one animation running or stopped; returns whether any run now.
    fn set(&mut self, kind: &str, window_id: &str, on: bool) -> bool {
        let key = format!("{kind}:{window_id}");
        if on {
            self.0.insert(key);
        } else {
            self.0.remove(&key);
        }
        !self.0.is_empty()
    }
}

struct Inner {
    running: Signal<bool>,
    keys: RefCell<ActivityKeys>,
}

/// Shared handle to the animation activity state. Clone it into whoever
/// starts animations and whoever renders the glass.
#[derive(Clone)]
pub struct AnimationActivity(Rc<Inner>);

impl AnimationActivity {
    pub fn new() -> Self {
        Self(Rc::new(Inner {
            running: signal(false),
            keys: RefCell::default(),
        }))
    }

    /// Mark the animation `kind` of window `window_id` as running or stopped.
    pub fn set_running(&self, kind: &str, window_id: &str, on: bool) {
        let any = self.0.keys.borrow_mut().set(kind, window_id, on);
        if self.0.running.get_untracked() != any {
            self.0.running.set(any);
        }
    }

    /// True while any window animation runs.
    pub fn running(&self) -> ReadSignal<bool> {
        self.0.running.read_only()
    }
}

#[cfg(test)]
mod tests {
    use super::ActivityKeys;

    #[test]
    fn runs_until_the_last_animation_stops() {
        let mut keys = ActivityKeys::default();
        assert!(keys.set("wobbly", "window-1", true));
        assert!(keys.set("wobbly", "window-2", true));
        assert!(keys.set("wobbly", "window-1", false));
        assert!(!keys.set("wobbly", "window-2", false));
    }

    #[test]
    fn kinds_and_repeated_marks_do_not_stack() {
        let mut keys = ActivityKeys::default();
        assert!(keys.set("wobbly", "window-1", true));
        // Marking the same animation again must not need two stops.
        assert!(keys.set("wobbly", "window-1", true));
        assert!(!keys.set("wobbly", "window-1", false));
        // A stop for something never started changes nothing.
        assert!(!keys.set("lamp", "window-9", false));
    }
}
