//! RC4 (used by PDF security handler revisions 2–4). Encryption and decryption are the same.

use ::rc4::{KeyInit, Rc4, StreamCipher};

/// An empty key leaves the data unchanged, and only the first 256 bytes of a longer key count,
/// as in RC4's key schedule.
pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    if let Ok(mut cipher) = Rc4::new_from_slice(key.get(..256).unwrap_or(key)) {
        cipher.apply_keystream(&mut out);
    }
    out
}
