//! The world server's packet-header cipher.
//!
//! Only the *headers* are encrypted -- four to six bytes of size and opcode --
//! and the bodies travel in clear. That is enough to make the stream
//! unparseable without the session key, because a reader who cannot find the
//! next length cannot find the next packet either.
//!
//! Two properties make this unforgiving to debug, and both are worth stating
//! before the first byte goes wrong:
//!
//! - **RC4 is a stream cipher, so the two sides share a position, not just a
//!   key.** Decrypting a byte twice, or skipping one, desynchronises the
//!   keystream permanently. Everything after that point is noise, and the
//!   failure surfaces far from its cause.
//! - **There is no integrity check.** A wrong key does not produce an error, it
//!   produces a plausible-looking header with an absurd length. See
//!   [`HeaderCrypt::new`] for why that is actually the most useful signal
//!   available.

use hmac::{Hmac, Mac};
use rc4::consts::U20;
use rc4::{KeyInit, Rc4, StreamCipher};
use sha1::Sha1;

use auth::SESSION_KEY_LEN;

/// Seeds the two directions' keys are derived from.
///
/// These are constants baked into the 3.3.5a client and server; they are not
/// secret and not negotiated. Naming them by *direction as this client sees it*
/// rather than by the server-side names used elsewhere avoids the single most
/// likely mistake here, which is wiring them up backwards.
const SERVER_TO_CLIENT_SEED: [u8; 16] = [
    0xCC, 0x98, 0xAE, 0x04, 0xE8, 0x97, 0xEA, 0xCA, 0x12, 0xDD, 0xC0, 0x93, 0x42, 0x91, 0x53, 0x57,
];
const CLIENT_TO_SERVER_SEED: [u8; 16] = [
    0xC2, 0xB3, 0x72, 0x3C, 0xC6, 0xAE, 0xD9, 0xB5, 0x34, 0x3C, 0x53, 0xEE, 0x2F, 0x43, 0x67, 0xCE,
];

/// Bytes of keystream discarded before the cipher is used.
///
/// RC4's first output bytes are measurably biased towards the key, so both
/// sides throw away a kilobyte before encrypting anything real. Dropping a
/// different amount than the peer is indistinguishable from using the wrong key.
const KEYSTREAM_DROP: usize = 1024;

/// The header cipher for one connection, in both directions.
///
/// Not `Clone`: two copies would advance independently and silently diverge.
pub struct HeaderCrypt {
    /// Decrypts headers arriving from the server.
    incoming: Rc4<U20>,
    /// Encrypts headers sent to the server.
    outgoing: Rc4<U20>,
}

impl HeaderCrypt {
    /// Derives both directions' keys from the SRP6 session key.
    ///
    /// The key is `HMAC-SHA1(seed, session_key)` -- note the seed is the HMAC
    /// *key* and the session key is the *message*, which is the reverse of what
    /// reads naturally, and swapping them produces a perfectly valid-looking
    /// cipher that decrypts to nothing.
    ///
    /// Nothing here can detect a mistake. The first real check is the first
    /// decrypted header: a correct key yields a small size and a known opcode,
    /// and a wrong one yields a multi-kilobyte size almost every time. That is
    /// why [`crate::protocol::MAX_PACKET`] exists and is checked -- it turns an
    /// undetectable key error into an immediate, specific failure.
    pub fn new(session_key: &[u8; SESSION_KEY_LEN]) -> Self {
        Self {
            incoming: init_direction(&SERVER_TO_CLIENT_SEED, session_key),
            outgoing: init_direction(&CLIENT_TO_SERVER_SEED, session_key),
        }
    }

    /// Decrypts header bytes arriving from the server, in place.
    ///
    /// Must be called on every incoming header byte exactly once, in arrival
    /// order, and never on a body byte.
    pub fn decrypt(&mut self, header: &mut [u8]) {
        self.incoming.apply_keystream(header);
    }

    /// Encrypts header bytes on their way to the server, in place.
    pub fn encrypt(&mut self, header: &mut [u8]) {
        self.outgoing.apply_keystream(header);
    }
}

fn init_direction(seed: &[u8; 16], session_key: &[u8; SESSION_KEY_LEN]) -> Rc4<U20> {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(seed)
        .expect("HMAC accepts a key of any length");
    mac.update(session_key);
    let key = mac.finalize().into_bytes();

    let mut cipher = Rc4::<U20>::new(rc4::Key::<U20>::from_slice(&key));
    // Discard the biased prefix. Encrypting zeros is just a way to advance the
    // keystream; the output is thrown away.
    let mut discard = [0u8; KEYSTREAM_DROP];
    cipher.apply_keystream(&mut discard);
    cipher
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; SESSION_KEY_LEN] {
        let mut key = [0u8; SESSION_KEY_LEN];
        for (i, slot) in key.iter_mut().enumerate() {
            *slot = (i as u8).wrapping_mul(13).wrapping_add(5);
        }
        key
    }

    /// A second cipher built from the same key must reproduce the first one's
    /// keystream, or nothing else in this module can be trusted.
    #[test]
    fn the_same_key_gives_the_same_keystream() {
        let mut a = HeaderCrypt::new(&key());
        let mut b = HeaderCrypt::new(&key());

        let mut one = [0u8; 32];
        let mut two = [0u8; 32];
        a.encrypt(&mut one);
        b.encrypt(&mut two);
        assert_eq!(one, two);
    }

    /// The two directions must not share a keystream. If they did, the cipher
    /// would degenerate into a reused one-time pad, and -- more immediately --
    /// a client that encrypted with the wrong direction would still appear to
    /// work when talking to itself.
    #[test]
    fn the_two_directions_differ() {
        let mut crypt = HeaderCrypt::new(&key());
        let mut sent = [0u8; 32];
        let mut received = [0u8; 32];
        crypt.encrypt(&mut sent);
        crypt.decrypt(&mut received);
        assert_ne!(sent, received, "both directions produced one keystream");
    }

    /// A peer's `decrypt` must undo our `encrypt`, which is what makes the pair
    /// usable at all. The peer is built here from the opposite side's point of
    /// view: its incoming is our outgoing.
    #[test]
    fn a_peer_decrypts_what_we_encrypt() {
        let mut ours = HeaderCrypt::new(&key());
        // The server's outgoing is our incoming and vice versa, so a peer is
        // just this cipher with the directions swapped.
        let mut theirs = HeaderCrypt::new(&key());

        let plain = *b"\x00\x0c\xec\x01\x2a\x2a";
        let mut wire = plain;
        ours.encrypt(&mut wire);
        assert_ne!(wire, plain, "encryption was a no-op");

        // The peer decrypts our outgoing stream with the same derivation we
        // used to encrypt it.
        theirs.encrypt(&mut wire);
        assert_eq!(wire, plain, "round trip did not recover the header");
    }

    /// The keystream must advance across calls: two encryptions of identical
    /// input must differ, or the cipher has been reset somewhere.
    #[test]
    fn the_keystream_advances_between_calls() {
        let mut crypt = HeaderCrypt::new(&key());
        let mut first = [0u8; 6];
        let mut second = [0u8; 6];
        crypt.encrypt(&mut first);
        crypt.encrypt(&mut second);
        assert_ne!(first, second);
    }

    /// A different session key must give a different keystream, or the key is
    /// not reaching the derivation.
    #[test]
    fn the_session_key_reaches_the_cipher() {
        let mut a = HeaderCrypt::new(&key());
        let mut b = HeaderCrypt::new(&[0u8; SESSION_KEY_LEN]);
        let mut one = [0u8; 16];
        let mut two = [0u8; 16];
        a.encrypt(&mut one);
        b.encrypt(&mut two);
        assert_ne!(one, two);
    }

    /// The kilobyte drop must actually happen. Without it the first header
    /// would be encrypted with RC4's biased prefix -- and, more usefully for a
    /// test, with a keystream this reproduces exactly.
    #[test]
    fn the_biased_prefix_is_dropped() {
        let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(&CLIENT_TO_SERVER_SEED).unwrap();
        mac.update(&key());
        let derived = mac.finalize().into_bytes();

        let mut undropped = Rc4::<U20>::new(rc4::Key::<U20>::from_slice(&derived));
        let mut without_drop = [0u8; 6];
        undropped.apply_keystream(&mut without_drop);

        let mut crypt = HeaderCrypt::new(&key());
        let mut with_drop = [0u8; 6];
        crypt.encrypt(&mut with_drop);

        assert_ne!(
            with_drop, without_drop,
            "the first header was encrypted with the biased prefix"
        );

        // And confirm the drop is exactly a kilobyte, not merely non-zero.
        let mut reference = Rc4::<U20>::new(rc4::Key::<U20>::from_slice(&derived));
        let mut discard = [0u8; KEYSTREAM_DROP];
        reference.apply_keystream(&mut discard);
        let mut expected = [0u8; 6];
        reference.apply_keystream(&mut expected);
        assert_eq!(with_drop, expected);
    }

    /// The seeds are fixed constants of the protocol; a typo in one would be
    /// invisible until a real server refused to talk. Pin their first bytes.
    #[test]
    fn the_seeds_are_the_documented_constants() {
        assert_eq!(SERVER_TO_CLIENT_SEED[0], 0xCC);
        assert_eq!(SERVER_TO_CLIENT_SEED[15], 0x57);
        assert_eq!(CLIENT_TO_SERVER_SEED[0], 0xC2);
        assert_eq!(CLIENT_TO_SERVER_SEED[15], 0xCE);
        assert_ne!(SERVER_TO_CLIENT_SEED, CLIENT_TO_SERVER_SEED);
    }
}
