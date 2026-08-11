//! MPQ's homegrown hash and stream cipher.
//!
//! Both are driven by the same 0x500-entry table, generated at startup from a
//! fixed linear congruential generator. The archive stores no filenames -- a
//! path is located purely by three independent hashes of it -- so this table is
//! the entry point to everything else in the format.

use std::sync::OnceLock;

/// Selects which 0x100-entry slice of the crypt table a hash draws from.
/// The three name hashes are independent so that a 32-bit collision in one
/// does not, on its own, produce a false file match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum HashKind {
    /// Starting slot in the hash table.
    TableOffset = 0,
    /// First name verifier.
    NameA = 1,
    /// Second name verifier.
    NameB = 2,
    /// Per-file encryption key.
    FileKey = 3,
}

fn table() -> &'static [u32; 0x500] {
    static TABLE: OnceLock<[u32; 0x500]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 0x500];
        let mut seed: u32 = 0x0010_0001;
        let step = |seed: &mut u32| -> u32 {
            *seed = (seed.wrapping_mul(125).wrapping_add(3)) % 0x2A_AAAB;
            *seed & 0xFFFF
        };
        for i in 0..0x100usize {
            let mut idx = i;
            for _ in 0..5 {
                let hi = step(&mut seed);
                let lo = step(&mut seed);
                t[idx] = (hi << 16) | lo;
                idx += 0x100;
            }
        }
        t
    })
}

/// Uppercases and converts `/` to `\`, matching how the original client
/// normalizes a path before hashing it. Paths are ASCII in practice.
#[inline]
fn normalize(byte: u8) -> u8 {
    match byte {
        b'/' => b'\\',
        b'a'..=b'z' => byte - 32,
        _ => byte,
    }
}

/// Hashes an archive path. Case- and separator-insensitive by construction.
pub fn hash(path: &str, kind: HashKind) -> u32 {
    let t = table();
    let base = kind as usize * 0x100;
    let mut seed1: u32 = 0x7FED_7FED;
    let mut seed2: u32 = 0xEEEE_EEEE;
    for byte in path.bytes() {
        let ch = normalize(byte) as u32;
        seed1 = t[base + ch as usize] ^ seed1.wrapping_add(seed2);
        seed2 = ch
            .wrapping_add(seed1)
            .wrapping_add(seed2)
            .wrapping_add(seed2 << 5)
            .wrapping_add(3);
    }
    seed1
}

/// Decrypts a block in place. The cipher operates on 32-bit words, which is why
/// every encrypted structure in the format is word-aligned.
pub fn decrypt(data: &mut [u32], key: u32) {
    let t = table();
    let mut key = key;
    let mut seed: u32 = 0xEEEE_EEEE;
    for word in data.iter_mut() {
        seed = seed.wrapping_add(t[0x400 + (key & 0xFF) as usize]);
        let plain = *word ^ key.wrapping_add(seed);
        key = ((!key).wrapping_shl(0x15).wrapping_add(0x1111_1111)) | (key >> 0x0B);
        seed = plain
            .wrapping_add(seed)
            .wrapping_add(seed << 5)
            .wrapping_add(3);
        *word = plain;
    }
}

/// Derives the encryption key for a file's data from its path.
///
/// Only the base name participates -- the original tooling keyed files by the
/// name alone, so `A\B\c.blp` and `Z\c.blp` share a key.
pub fn file_key(path: &str) -> u32 {
    let base = path.rsplit(['\\', '/']).next().unwrap_or(path);
    hash(base, HashKind::FileKey)
}

/// Applies the `FIX_KEY` adjustment, which folds the file's position and size
/// into the key so that the same file at a different offset decrypts
/// differently.
pub fn fix_key(key: u32, block_offset: u32, file_size: u32) -> u32 {
    (key.wrapping_add(block_offset)) ^ file_size
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two tables every archive contains are located by these fixed
    /// hashes; if the table generator or the hash drifts, these change.
    #[test]
    fn known_table_keys() {
        assert_eq!(hash("(hash table)", HashKind::FileKey), 0xC3AF_3770);
        assert_eq!(hash("(block table)", HashKind::FileKey), 0xEC83_B3A3);
    }

    #[test]
    fn hash_is_case_and_separator_insensitive() {
        let a = hash("Interface/Icons/Foo.blp", HashKind::NameA);
        let b = hash("INTERFACE\\ICONS\\FOO.BLP", HashKind::NameA);
        assert_eq!(a, b);
    }

    /// Encryption is its own inverse only if we run the keystream forward the
    /// same way, so round-tripping is checked against a hand-rolled encryptor.
    #[test]
    fn decrypt_inverts_encrypt() {
        let t = table();
        let key = 0x1234_5678u32;
        let plain: Vec<u32> = (0..64).map(|i| i * 0x0101_0101 + 7).collect();

        let mut cipher = plain.clone();
        let mut k = key;
        let mut seed: u32 = 0xEEEE_EEEE;
        for word in cipher.iter_mut() {
            seed = seed.wrapping_add(t[0x400 + (k & 0xFF) as usize]);
            let p = *word;
            *word = p ^ k.wrapping_add(seed);
            k = ((!k).wrapping_shl(0x15).wrapping_add(0x1111_1111)) | (k >> 0x0B);
            seed = p.wrapping_add(seed).wrapping_add(seed << 5).wrapping_add(3);
        }
        assert_ne!(cipher, plain);

        decrypt(&mut cipher, key);
        assert_eq!(cipher, plain);
    }
}
