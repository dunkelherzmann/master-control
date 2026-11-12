//! What a master and a worker share to talk across the network: pairing by a short code, and sealing every message
//! with the key that pairing gives them.
//!
//! Pairing uses SPAKE2, a password-authenticated key exchange: both sides end with the same key only if both used the
//! same code, and someone listening learns nothing that lets them try codes afterwards. Messages are sealed with
//! XChaCha20-Poly1305, each under a fresh random nonce.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use spake2::{Ed25519Group, Identity, Password, Spake2};

pub const NONCE: usize = 24;
pub type SharedKey = [u8; 32];

pub fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes).expect("the system has random bytes");
    bytes
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    text.len().is_multiple_of(2).then(|| (0..text.len()).step_by(2).map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok()).collect::<Option<Vec<u8>>>())?
}

/// One side of a pairing in progress.
pub struct Pairing(Spake2<Ed25519Group>);

impl Pairing {
    /// Starts a pairing with the code both sides were given. Returns the message to send to the other side.
    pub fn start(code: &str) -> (Pairing, Vec<u8>) {
        let (state, message) = Spake2::<Ed25519Group>::start_symmetric(&Password::new(code.trim().as_bytes()), &Identity::new(b"master-control pairing 1"));
        (Pairing(state), message)
    }

    /// The key, given the other side's message. Both sides get the same key only if they used the same code; with
    /// different codes each gets a key the other can't use, which shows at the first sealed message.
    pub fn finish(self, their_message: &[u8]) -> Option<SharedKey> {
        self.0.finish(their_message).ok()?.try_into().ok()
    }
}

/// Seals a message: a fresh nonce, then the ciphertext. `context` is checked on opening but not sent; it ties a
/// message to its purpose (a request, or the answer to one particular request).
pub fn seal(key: &SharedKey, context: &[u8], message: &[u8]) -> Vec<u8> {
    let nonce = random::<NONCE>();
    let sealed = XChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: message, aad: context })
        .expect("sealing in memory doesn't fail");
    [nonce.as_slice(), &sealed].concat()
}

/// Opens a sealed message. None if it wasn't sealed with this key and context, or was changed on the way.
pub fn open(key: &SharedKey, context: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    let (nonce, body) = sealed.split_at_checked(NONCE)?;
    XChaCha20Poly1305::new(Key::from_slice(key)).decrypt(XNonce::from_slice(nonce), Payload { msg: body, aad: context }).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(a: &str, b: &str) -> (SharedKey, SharedKey) {
        let (first, to_second) = Pairing::start(a);
        let (second, to_first) = Pairing::start(b);
        (first.finish(&to_first).unwrap(), second.finish(&to_second).unwrap())
    }

    #[test]
    fn the_same_code_gives_both_sides_the_same_key() {
        let (master, worker) = pair("4821 9037", " 4821 9037\n");
        assert_eq!(master, worker);
        assert_ne!(master, pair("4821 9037", "4821 9037").0, "and a new key every time");
    }

    #[test]
    fn different_codes_give_keys_that_dont_open_each_others_messages() {
        let (master, worker) = pair("48219037", "48219038");
        assert_ne!(master, worker);
        assert_eq!(open(&worker, b"request", &seal(&master, b"request", b"hello")), None);
        assert!(Pairing::start("1").0.finish(b"not a pairing message").is_none());
    }

    #[test]
    fn a_sealed_message_opens_only_whole_with_its_key_and_context() {
        let key = random::<32>();
        let sealed = seal(&key, b"request", b"run this");
        assert_eq!(open(&key, b"request", &sealed).as_deref(), Some(&b"run this"[..]));
        assert_eq!(open(&key, b"answer", &sealed), None, "another purpose");
        assert_eq!(open(&random::<32>(), b"request", &sealed), None, "another key");
        let mut changed = sealed.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert_eq!(open(&key, b"request", &changed), None, "changed on the way");
        assert_eq!(open(&key, b"request", &sealed[..10]), None, "cut short");
        assert_ne!(seal(&key, b"request", b"run this"), sealed, "never the same bytes twice");
        assert!(!sealed.windows(8).any(|window| window == b"run this"));
    }

    #[test]
    fn hex_goes_there_and_back() {
        assert_eq!(from_hex(&to_hex(&[0, 255, 16])), Some(vec![0, 255, 16]));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
    }
}
