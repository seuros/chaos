//! The typed code stays out of network requests. A public lookup identifier
//! retrieves an authenticated certificate pin; only then may the app redeem
//! the same invite as the QR. The three HMAC labels are protocol constants.
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub(super) fn tag(code: &str, purpose: &str, pin: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(code.as_bytes()).expect("HMAC accepts any key size");
    mac.update(b"FreeChaOS pairing v1:");
    mac.update(purpose.as_bytes());
    mac.update(b":");
    mac.update(pin.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(super) fn generate() -> String {
    const SYMBOLS: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
    rand::random::<[u8; 20]>()
        .into_iter()
        .map(|b| char::from(SYMBOLS[usize::from(b & 31)]))
        .collect()
}
