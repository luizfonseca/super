//! Hashing of keys that only live in memory: fast (the default `SipHash` resists collisions on
//! purpose, at a cost paid on every request), and the same for the whole process

use std::{
    collections::HashMap,
    hash::{BuildHasher, BuildHasherDefault, Hash, Hasher},
};

/// The hash of `value`
pub fn hash_one(value: impl Hash) -> u64 {
    foldhash::fast::FixedState::with_seed(0).hash_one(value)
}

/// Hashes keys that are hashes already, as they are
#[derive(Default)]
pub struct Prehashed(u64);

impl Hasher for Prehashed {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(*byte);
        }
    }

    fn write_u64(&mut self, hash: u64) {
        self.0 = hash;
    }
}

/// A map whose keys are hashes
pub type PrehashedMap<V> = HashMap<u64, V, BuildHasherDefault<Prehashed>>;

#[cfg(test)]
mod tests {
    use super::{hash_one, PrehashedMap};

    #[test]
    fn test_hashes_are_the_same_for_the_process() {
        assert_eq!(hash_one(("10.0.0.1:80", 1)), hash_one(("10.0.0.1:80", 1)));
        assert_ne!(hash_one(("10.0.0.1:80", 1)), hash_one(("10.0.0.2:80", 1)));
        let mut map = PrehashedMap::default();
        map.insert(42, "a");
        assert_eq!(map.get(&42), Some(&"a"));
    }
}
