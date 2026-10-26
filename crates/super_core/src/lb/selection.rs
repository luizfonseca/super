//! Which backend a request goes to

use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicUsize, Ordering},
};

use super::Backend;

/// A way to choose among backends, built again when they change
pub trait BackendSelection: Send + Sync {
    fn build(backends: &BTreeSet<Backend>) -> Self;

    /// The backends to try for `key`, in order
    fn iter<'a>(&'a self, key: &[u8]) -> impl Iterator<Item = &'a Backend>;
}

/// In turn, each backend as many times as its weight
pub struct RoundRobin {
    backends: Box<[Backend]>,
    /// Indexes into `backends`, each repeated by its weight
    turns: Box<[usize]>,
    next: AtomicUsize,
}

impl BackendSelection for RoundRobin {
    fn build(backends: &BTreeSet<Backend>) -> Self {
        let backends = backends.iter().cloned().collect::<Box<[_]>>();
        let turns = backends
            .iter()
            .enumerate()
            .flat_map(|(index, backend)| std::iter::repeat_n(index, backend.weight.max(1)))
            .collect();
        Self {
            backends,
            turns,
            next: AtomicUsize::new(0),
        }
    }

    fn iter<'a>(&'a self, _key: &[u8]) -> impl Iterator<Item = &'a Backend> {
        let turns = self.turns.len();
        // Every try takes the next turn, as concurrent requests do
        (0..turns).filter_map(move |_| {
            let turn = self.next.fetch_add(1, Ordering::Relaxed) % turns;
            self.backends.get(*self.turns.get(turn)?)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{BackendSelection, RoundRobin};
    use crate::lb::Backend;

    #[test]
    fn test_turns_by_weight() {
        let backends = BTreeSet::from([
            Backend::new_with_weight("10.0.0.1:80", 1).unwrap(),
            Backend::new_with_weight("10.0.0.2:80", 3).unwrap(),
        ]);
        let selection = RoundRobin::build(&backends);
        let heavy = (0..8)
            .filter(|_| selection.iter(b"").next().unwrap().addr.to_string() == "10.0.0.2:80")
            .count();
        assert_eq!(heavy, 6);
    }

    #[test]
    fn test_no_backends() {
        let selection = RoundRobin::build(&BTreeSet::new());
        assert!(selection.iter(b"").next().is_none());
    }
}
