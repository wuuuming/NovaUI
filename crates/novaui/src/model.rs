//! App state declared to the framework: `Model<T>` (`docs/runtime.md`, "Kinds of work").
//!
//! A model owns a value and a version. Reading is free (`Deref`); changing goes through
//! `update` / `edit` (or `set`, which drops an equal value), and that is the notify: the
//! model's version moves to a number no change has had before (one counter for all models).
//! A view that presents a model holds a `Subscription` (`subscribe`): the change reaches it as
//! its message in the same turn (the flush), once however many changes the turn made.

use std::cell::Cell;

thread_local! {
    static NEXT: Cell<u64> = const { Cell::new(1) };
}

fn next_version() -> u64 {
    NEXT.with(|n| {
        let v = n.get();
        n.set(v + 1);
        v
    })
}

/// A value with a change version (UI thread state: the counter is per thread).
#[derive(Debug, Default)]
pub struct Model<T> {
    value: T,
    version: u64,
    subs: crate::sched::Subscribers,
}

impl<T> Model<T> {
    pub fn new(value: T) -> Self {
        Model { value, version: next_version(), subs: Default::default() }
    }

    /// Changes the value; whoever observes the model presents again.
    pub fn update<R>(&mut self, f: impl FnOnce(&mut T) -> R) -> R {
        self.notify();
        f(&mut self.value)
    }

    /// Mutable access that counts as a change (`m.edit().x = 1`).
    pub fn edit(&mut self) -> &mut T {
        self.notify();
        &mut self.value
    }

    /// Marks the model changed without touching it (state it derives from moved elsewhere).
    pub fn notify(&mut self) {
        self.version = next_version();
        self.subs.notify();
    }

    /// Delivers `msg` (`Event::Message`) in the turn the model changes, after the change's
    /// cause was handled (phase 3, `runtime.md`, "One turn, in order"), once per round however often it changed.
    /// Dropping the `Subscription` ends it.
    pub fn subscribe(&mut self, msg: impl std::any::Any) -> crate::sched::Subscription {
        self.subs.add(std::rc::Rc::new(msg))
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

impl<T: PartialEq> Model<T> {
    /// Replaces the value; an equal value is not a change. Returns whether it changed.
    pub fn set(&mut self, v: T) -> bool {
        if self.value == v {
            return false;
        }
        self.value = v;
        self.notify();
        true
    }
}

impl<T> std::ops::Deref for Model<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_moves_the_version_and_an_equal_value_does_not() {
        let (mut a, mut b) = (Model::new(1), Model::new(String::from("x")));
        let (va, vb) = (a.version(), b.version());
        assert!(!a.set(1), "an equal value is no change");
        assert_eq!(a.version(), va);
        b.edit().push('y');
        assert!(b.version() > vb.max(va));
        a.update(|v| *v += 1);
        assert_eq!(*a, 2);
        assert!(a.version() > b.version(), "one counter: the newest change has the newest version");
    }
}
