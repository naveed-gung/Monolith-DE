//! The codec that seals the developer identity.
//!
//! Deliberately free of dependencies so the installer can `include!` this file
//! and decode the same sealed bytes without linking the player crate. Keep it
//! that way: adding a crate here forces the installer to rebuild the world.

/// Published constant, not a secret. This is obfuscation, not encryption: it
/// keeps the identity out of `strings` and out of the repo, but anyone with a
/// disassembler can recover the values. Anything stronger needs a key the
/// binary cannot carry.
pub const SEED: u64 = 0x9E37_79B9_7F4A_7C1;

/// xorshift64. Each field starts from its own state so a known plaintext in one
/// reveals nothing about another. Must stay byte-for-byte identical to
/// `tools/encrypt_identity.py`, or every field decodes to noise.
pub struct Keystream {
    state: u64,
}

impl Keystream {
    pub fn new(field: u8) -> Self {
        Self {
            state: SEED ^ (field as u64).wrapping_mul(0x9E37_79B9_7F4A_7C1),
        }
    }

    pub fn next(&mut self) -> u8 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        (self.state >> 56) as u8
    }
}

pub fn decode(bytes: &[u8], field: u8) -> String {
    let mut key = Keystream::new(field);
    let text: Vec<u8> = bytes.iter().map(|byte| byte ^ key.next()).collect();
    // Every field is authored as UTF-8, so a failure here means the keystream
    // drifted rather than that the payload is damaged.
    String::from_utf8(text).expect("identity field must decode as UTF-8")
}

/// The inverse of `decode`. Only the tests need it, and it proves the generator
/// and the decoder have not drifted apart.
#[cfg(test)]
pub fn encode(text: &str, field: u8) -> Vec<u8> {
    let mut key = Keystream::new(field);
    text.bytes().map(|byte| byte ^ key.next()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keystream_actually_transforms_its_input() {
        // Without this, a broken generator would leave the plaintext in place
        // and every other test would still pass.
        assert_ne!(encode("Naveed", 0).as_slice(), b"Naveed");
        assert_eq!(encode("Naveed", 0), encode("Naveed", 0));
        assert_ne!(encode("Naveed", 0), encode("Naveed", 1));
        assert_eq!(decode(&encode("Naveed Gung", 0), 0), "Naveed Gung");
    }
}
