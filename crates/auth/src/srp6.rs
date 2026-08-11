//! SRP6 as the 3.3.5a logon server speaks it.
//!
//! Written from the public protocol documentation. SRP proves knowledge of a
//! password without sending it and, in the process, agrees a session key that
//! the world server later uses to encrypt packet headers -- so this is not only
//! a login step, it is where [`Session::key`] comes from.
//!
//! Two things differ from textbook SRP6a and both are easy to get wrong:
//!
//! - **Everything on the wire is little-endian**, including the big integers.
//!   The maths is endian-agnostic but the hashing is not, so a byte order slip
//!   produces a proof that is wrong without being obviously malformed.
//! - **The session key is interleaved**, not hashed directly: `S` is split into
//!   even and odd bytes, each half hashed, and the digests woven back together.
//!   See [`interleave`].

use num_bigint::BigUint;
use sha1::{Digest, Sha1};

/// The safe prime every 3.3.5a realm uses, in the little-endian order it
/// appears on the wire.
pub const N_LE: [u8; 32] = [
    0xB7, 0x9B, 0x3E, 0x2A, 0x87, 0x82, 0x3C, 0xAB, 0x8F, 0x5E, 0xBF, 0xBF, 0x8E, 0xB1, 0x01, 0x08,
    0x53, 0x50, 0x06, 0x29, 0x8B, 0x5B, 0xAD, 0xBD, 0x5B, 0x53, 0xE1, 0x89, 0x5E, 0x64, 0x4B, 0x89,
];

/// Generator.
pub const G: u8 = 7;
/// Multiplier. Fixed at 3 in this version rather than derived from `N` and `g`.
pub const K: u8 = 3;

/// Bytes in `A`, `B` and `S`.
pub const KEY_LEN: usize = 32;
/// Bytes in the interleaved session key.
pub const SESSION_KEY_LEN: usize = 40;

fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// Reads a little-endian byte string as an integer.
fn from_le(bytes: &[u8]) -> BigUint {
    BigUint::from_bytes_le(bytes)
}

/// Writes an integer as a fixed-width little-endian byte string.
///
/// Fixed width matters: a value that happens to have a zero top byte must still
/// occupy its full length, or every hash computed over it changes.
fn to_le(value: &BigUint, len: usize) -> Vec<u8> {
    let mut bytes = value.to_bytes_le();
    bytes.resize(len, 0);
    bytes.truncate(len);
    bytes
}

/// Weaves `S` into the 40-byte session key.
///
/// The even and odd bytes are hashed separately and the two digests
/// interleaved. Leading zero bytes are skipped first -- `S` is a modular
/// result, so it is shorter than 32 bytes roughly once in every 256 logins, and
/// a client that ignores that authenticates fine until it happens to draw a
/// small `S` and then fails inexplicably.
pub fn interleave(s_le: &[u8; KEY_LEN]) -> [u8; SESSION_KEY_LEN] {
    let mut skip = 0;
    while skip < KEY_LEN && s_le[skip] == 0 {
        skip += 1;
    }
    // Keep the split aligned: dropping an odd number of bytes would swap which
    // half each byte belongs to.
    if skip % 2 == 1 {
        skip += 1;
    }
    let offset = skip / 2;

    let mut even = Vec::with_capacity(KEY_LEN / 2);
    let mut odd = Vec::with_capacity(KEY_LEN / 2);
    for i in 0..KEY_LEN / 2 {
        even.push(s_le[i * 2]);
        odd.push(s_le[i * 2 + 1]);
    }

    let hash_even = sha1(&[&even[offset..]]);
    let hash_odd = sha1(&[&odd[offset..]]);

    let mut key = [0u8; SESSION_KEY_LEN];
    for i in 0..20 {
        key[i * 2] = hash_even[i];
        key[i * 2 + 1] = hash_odd[i];
    }
    key
}

/// The password verifier's private exponent, `x = H(salt | H(USER:PASS))`.
///
/// Account name and password are upper-cased first. That is a property of the
/// protocol, not a convenience: the server stored its verifier from the
/// upper-cased form, so a lower-case attempt derives a different `x` and fails
/// as though the password were wrong.
pub fn derive_x(username: &str, password: &str, salt_le: &[u8; 32]) -> BigUint {
    let identity = format!("{}:{}", username.to_uppercase(), password.to_uppercase());
    let inner = sha1(&[identity.as_bytes()]);
    from_le(&sha1(&[salt_le, &inner]))
}

/// The verifier the server stores for an account, `v = g^x mod N`.
///
/// Only needed to test against a server implementation, but it is the same
/// derivation, so keeping it here keeps the two consistent.
pub fn verifier(username: &str, password: &str, salt_le: &[u8; 32]) -> BigUint {
    let x = derive_x(username, password, salt_le);
    BigUint::from(G).modpow(&x, &from_le(&N_LE))
}

/// A completed client-side exchange.
pub struct Session {
    /// Public ephemeral, sent to the server.
    pub a_pub: [u8; KEY_LEN],
    /// Proof that we know the password.
    pub m1: [u8; 20],
    /// Session key. Retained after login: the world server keys its header
    /// cipher with this.
    pub key: [u8; SESSION_KEY_LEN],
    /// The proof we expect back, which authenticates the *server* to us.
    expected_m2: [u8; 20],
}

impl Session {
    /// Checks the server's reply. A mismatch means we are talking to something
    /// that does not know the verifier.
    pub fn verify_server(&self, m2: &[u8; 20]) -> bool {
        // Not constant-time, and does not need to be: both sides are public
        // once the exchange is over, and a mismatch aborts the connection.
        &self.expected_m2 == m2
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("server sent B = 0, which cannot be a valid public ephemeral")]
    InvalidServerKey,
}

/// Runs the client's half of the exchange.
///
/// `a` is the client's private ephemeral, taken as an argument so tests can
/// pin it; [`Client::random`] supplies one otherwise.
pub struct Client;

impl Client {
    pub fn random_private() -> [u8; 19] {
        use rand::RngCore;
        let mut a = [0u8; 19];
        rand::rng().fill_bytes(&mut a);
        a
    }

    /// Computes the client's public key, proof and session key.
    pub fn respond(
        username: &str,
        password: &str,
        salt_le: &[u8; 32],
        b_pub_le: &[u8; KEY_LEN],
        private: &[u8],
    ) -> Result<Session, Error> {
        let n = from_le(&N_LE);
        let g = BigUint::from(G);
        let b_pub = from_le(b_pub_le);

        if (&b_pub % &n) == BigUint::ZERO {
            return Err(Error::InvalidServerKey);
        }

        let a = from_le(private);
        let a_pub = g.modpow(&a, &n);
        let a_pub_le: [u8; KEY_LEN] = to_le(&a_pub, KEY_LEN).try_into().unwrap();

        // u mixes both public keys, so neither side can steer the shared secret.
        let u = from_le(&sha1(&[&a_pub_le, b_pub_le]));
        let x = derive_x(username, password, salt_le);

        // S = (B - k*g^x)^(a + u*x) mod N. The subtraction is modular: k*g^x can
        // exceed B, and a plain subtraction on unsigned integers would panic or
        // wrap.
        let kgx = (BigUint::from(K) * g.modpow(&x, &n)) % &n;
        let base = (&b_pub + &n - kgx) % &n;
        let exponent = a + u * &x;
        let s = base.modpow(&exponent, &n);
        let s_le: [u8; KEY_LEN] = to_le(&s, KEY_LEN).try_into().unwrap();

        let key = interleave(&s_le);

        // M1 = H( H(N) xor H(g) | H(user) | salt | A | B | K )
        let hash_n = sha1(&[&N_LE]);
        let hash_g = sha1(&[&[G]]);
        let mut ng = [0u8; 20];
        for i in 0..20 {
            ng[i] = hash_n[i] ^ hash_g[i];
        }
        let hash_user = sha1(&[username.to_uppercase().as_bytes()]);
        let m1 = sha1(&[&ng, &hash_user, salt_le, &a_pub_le, b_pub_le, &key]);

        // The server proves itself with M2 = H(A | M1 | K).
        let expected_m2 = sha1(&[&a_pub_le, &m1, &key]);

        Ok(Session {
            a_pub: a_pub_le,
            m1,
            key,
            expected_m2,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal server side, written from the protocol definition rather than
    /// from the client above, so agreement between them is evidence rather than
    /// a shared mistake.
    struct Server {
        salt: [u8; 32],
        verifier: BigUint,
        b_private: BigUint,
        b_public: BigUint,
    }

    impl Server {
        fn new(username: &str, password: &str, salt: [u8; 32], b_private: &[u8]) -> Self {
            let n = from_le(&N_LE);
            let v = verifier(username, password, &salt);
            let b = from_le(b_private);
            // B = k*v + g^b mod N
            let public = ((BigUint::from(K) * &v) + BigUint::from(G).modpow(&b, &n)) % &n;
            Self {
                salt,
                verifier: v,
                b_private: b,
                b_public: public,
            }
        }

        fn b_le(&self) -> [u8; KEY_LEN] {
            to_le(&self.b_public, KEY_LEN).try_into().unwrap()
        }

        /// S = (A * v^u)^b mod N, the server's route to the same secret.
        fn session_key(&self, a_pub_le: &[u8; KEY_LEN]) -> [u8; SESSION_KEY_LEN] {
            let n = from_le(&N_LE);
            let a_pub = from_le(a_pub_le);
            let u = from_le(&sha1(&[a_pub_le, &self.b_le()]));
            let s = ((a_pub * self.verifier.modpow(&u, &n)) % &n).modpow(&self.b_private, &n);
            let s_le: [u8; KEY_LEN] = to_le(&s, KEY_LEN).try_into().unwrap();
            interleave(&s_le)
        }

        fn expected_m1(
            &self,
            username: &str,
            a_pub_le: &[u8; KEY_LEN],
            key: &[u8; SESSION_KEY_LEN],
        ) -> [u8; 20] {
            let hash_n = sha1(&[&N_LE]);
            let hash_g = sha1(&[&[G]]);
            let mut ng = [0u8; 20];
            for i in 0..20 {
                ng[i] = hash_n[i] ^ hash_g[i];
            }
            let hash_user = sha1(&[username.to_uppercase().as_bytes()]);
            sha1(&[
                &ng,
                &hash_user,
                &self.salt,
                a_pub_le,
                &self.b_le(),
                key,
            ])
        }
    }

    fn fixed_salt() -> [u8; 32] {
        let mut salt = [0u8; 32];
        for (i, slot) in salt.iter_mut().enumerate() {
            *slot = (i as u8).wrapping_mul(7).wrapping_add(11);
        }
        salt
    }

    /// The whole point: both sides reach the same session key, and the server
    /// accepts the client's proof.
    #[test]
    fn client_and_server_agree() {
        let (user, pass) = ("TESTER", "hunter2");
        let salt = fixed_salt();
        let server = Server::new(user, pass, salt, &[9u8; 19]);

        let session =
            Client::respond(user, pass, &salt, &server.b_le(), &[4u8; 19]).unwrap();

        let server_key = server.session_key(&session.a_pub);
        assert_eq!(session.key, server_key, "session keys diverged");
        assert_eq!(
            session.m1,
            server.expected_m1(user, &session.a_pub, &server_key),
            "server would reject the client's proof"
        );

        // And the client accepts the server's reply.
        let m2 = sha1(&[&session.a_pub, &session.m1, &server_key]);
        assert!(session.verify_server(&m2));
    }

    #[test]
    fn a_wrong_password_produces_a_different_proof() {
        let salt = fixed_salt();
        let server = Server::new("TESTER", "hunter2", salt, &[9u8; 19]);
        let session =
            Client::respond("TESTER", "wrong", &salt, &server.b_le(), &[4u8; 19]).unwrap();

        let server_key = server.session_key(&session.a_pub);
        assert_ne!(session.key, server_key);
        assert_ne!(
            session.m1,
            server.expected_m1("TESTER", &session.a_pub, &server_key)
        );
    }

    /// The server's reply is checked, so a peer that does not know the verifier
    /// is rejected even if it accepted our proof.
    #[test]
    fn a_forged_server_reply_is_rejected() {
        let salt = fixed_salt();
        let server = Server::new("TESTER", "hunter2", salt, &[9u8; 19]);
        let session =
            Client::respond("TESTER", "hunter2", &salt, &server.b_le(), &[4u8; 19]).unwrap();
        assert!(!session.verify_server(&[0u8; 20]));
    }

    /// Credentials are upper-cased before hashing, so case must not matter.
    #[test]
    fn credentials_are_case_insensitive() {
        let salt = fixed_salt();
        let a = derive_x("tester", "hunter2", &salt);
        let b = derive_x("TESTER", "HUNTER2", &salt);
        let c = derive_x("TeStEr", "Hunter2", &salt);
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    /// A different salt must give a different verifier, or the salt is not
    /// doing its job.
    #[test]
    fn the_salt_changes_the_verifier() {
        let a = verifier("TESTER", "hunter2", &fixed_salt());
        let b = verifier("TESTER", "hunter2", &[0u8; 32]);
        assert_ne!(a, b);
    }

    /// `S` with leading zeros must skip an even number of bytes, or the two
    /// halves swap. This is the once-in-256 failure that only shows up in
    /// production.
    #[test]
    fn interleave_skips_leading_zeros_in_pairs() {
        let mut s = [0u8; KEY_LEN];
        s[30] = 0xAB;
        s[31] = 0xCD;
        let key = interleave(&s);

        // Only the two significant bytes survive the skip, so the result must
        // match hashing exactly those.
        let expected_even = sha1(&[&[0xABu8]]);
        let expected_odd = sha1(&[&[0xCDu8]]);
        for i in 0..20 {
            assert_eq!(key[i * 2], expected_even[i]);
            assert_eq!(key[i * 2 + 1], expected_odd[i]);
        }
    }

    #[test]
    fn interleave_is_deterministic_and_full_length() {
        let s = [0x5Au8; KEY_LEN];
        assert_eq!(interleave(&s), interleave(&s));
        assert_eq!(interleave(&s).len(), SESSION_KEY_LEN);
    }

    /// Fixed-width encoding: a value with a zero top byte must still fill its
    /// field, or the hashes over it change.
    #[test]
    fn integers_encode_at_fixed_width() {
        let small = BigUint::from(1u8);
        assert_eq!(to_le(&small, 32).len(), 32);
        assert_eq!(to_le(&small, 32)[0], 1);
        assert!(to_le(&small, 32)[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn a_zero_server_key_is_refused() {
        let salt = fixed_salt();
        let err = Client::respond("TESTER", "hunter2", &salt, &[0u8; KEY_LEN], &[4u8; 19]);
        assert!(matches!(err, Err(Error::InvalidServerKey)));
    }

    /// Different private ephemerals must give different session keys, or the
    /// randomness is not reaching the maths.
    #[test]
    fn the_private_ephemeral_changes_the_session() {
        let salt = fixed_salt();
        let server = Server::new("TESTER", "hunter2", salt, &[9u8; 19]);
        let one = Client::respond("TESTER", "hunter2", &salt, &server.b_le(), &[1u8; 19]).unwrap();
        let two = Client::respond("TESTER", "hunter2", &salt, &server.b_le(), &[2u8; 19]).unwrap();
        assert_ne!(one.key, two.key);
        assert_ne!(one.a_pub, two.a_pub);

        // Both must still be accepted by the server.
        for session in [one, two] {
            let key = server.session_key(&session.a_pub);
            assert_eq!(session.key, key);
            assert_eq!(session.m1, server.expected_m1("TESTER", &session.a_pub, &key));
        }
    }

    /// The generated private ephemeral must actually vary.
    #[test]
    fn random_private_keys_differ() {
        assert_ne!(Client::random_private(), Client::random_private());
    }
}
