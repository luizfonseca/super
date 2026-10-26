//! The key of a response in the cache: its hash is Pingora's (Blake2b, 128 bits), so entries
//! stored by Proksi are found again

use blake2::{digest::consts::U16, Blake2b, Digest};

const KEY_SIZE: usize = 16;

pub type HashBinary = [u8; KEY_SIZE];

type Blake2b128 = Blake2b<U16>;

/// The hash of a key
pub trait CacheHashKey {
    fn primary_bin(&self) -> HashBinary;

    fn user_tag(&self) -> &str;

    /// The hash, in hexadecimal
    fn primary(&self) -> String {
        hex(&self.primary_bin())
    }
}

/// A response's key: what it's for (`primary`), and who it belongs to (`user_tag`, e.g. a host)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    primary: Vec<u8>,
    pub user_tag: String,
}

impl CacheKey {
    pub fn new(primary: impl Into<Vec<u8>>, user_tag: impl Into<String>) -> Self {
        Self {
            primary: primary.into(),
            user_tag: user_tag.into(),
        }
    }

    pub fn primary_key(&self) -> &[u8] {
        &self.primary
    }
}

impl CacheHashKey for CacheKey {
    fn primary_bin(&self) -> HashBinary {
        let mut hasher = Blake2b128::new();
        hasher.update(&self.primary);
        hasher.finalize().into()
    }

    fn user_tag(&self) -> &str {
        &self.user_tag
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        for digit in [byte >> 4, byte & 0xf] {
            hex.push(char::from(*DIGITS.get(usize::from(digit)).unwrap_or(&b'0')));
        }
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::{CacheHashKey, CacheKey};

    #[test]
    fn test_hashes_are_pingora_s() {
        // From pingora-cache's own tests
        assert_eq!(
            CacheKey::new("aa", "1").primary(),
            "ac10f2aef117729f8dad056b3059eb7e"
        );
        assert_eq!(
            CacheKey::new("tenant_a/path", "1").primary(),
            "6c79e74e88bacb8eb370adb7617068c8"
        );
    }
}
