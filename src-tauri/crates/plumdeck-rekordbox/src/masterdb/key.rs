//! The SQLCipher passphrase of rekordbox 6/7's `master.db`.
//!
//! The derivation follows pyrekordbox (MIT, Copyright (c) 2022-2025 Dylan
//! Jones; `pyrekordbox.utils.deobfuscate` and `db6.database.BLOB`): the
//! passphrase is stored base85-encoded (Python's `b85decode` alphabet),
//! XOR-ed with a short repeating key and zlib-compressed. Until the app has a
//! THIRD_PARTY_NOTICES file (W0-17), pyrekordbox's notice is kept here:
//!
//! > MIT License. Copyright (c) 2022-2025, Dylan Jones.
//! > Permission is hereby granted, free of charge, to any person obtaining a
//! > copy of this software and associated documentation files (the
//! > "Software"), to deal in the Software without restriction, including
//! > without limitation the rights to use, copy, modify, merge, publish,
//! > distribute, sublicense, and/or sell copies of the Software, and to permit
//! > persons to whom the Software is furnished to do so, subject to the
//! > following conditions: The above copyright notice and this permission
//! > notice shall be included in all copies or substantial portions of the
//! > Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
//! > EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! > MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN
//! > NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
//! > DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
//! > OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
//! > USE OR OTHER DEALINGS IN THE SOFTWARE.
//!
//! The passphrase is never logged, formatted into errors or returned through
//! `Debug`; [`Passphrase`] keeps it opaque.

use std::fmt;

/// Environment variable that replaces the derived passphrase.
pub const KEY_ENV: &str = "PLUMDECK_REKORDBOX_KEY";

const BLOB: &[u8] = b"PN_Pq^*N>(JYe*u^8;Yg76HuZ<mR13S?=>)b9;DpoTXV(6ItkU`}8*m6tx_I{Solh_N#dfe{v=";
const BLOB_KEY: &[u8] = b"657f48f84c437cc1";
const B85_ALPHABET: &[u8; 85] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";

/// An opaque SQLCipher passphrase. `Debug` never shows the value.
#[derive(Clone, PartialEq, Eq)]
pub struct Passphrase(String);

impl Passphrase {
    /// Wraps an explicit passphrase (for example from settings).
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.contains('\0') {
            return None;
        }
        Some(Self(trimmed.to_owned()))
    }

    /// `PLUMDECK_REKORDBOX_KEY` when set and non-empty, else the derived key.
    pub fn resolve(env: &dyn Fn(&str) -> Option<String>) -> Option<Self> {
        env(KEY_ENV).and_then(Self::new).or_else(Self::derived)
    }

    /// The passphrase every rekordbox 6/7 installation uses.
    pub fn derived() -> Option<Self> {
        deobfuscate(BLOB, BLOB_KEY).and_then(Self::new)
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Passphrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Passphrase(<redacted>)")
    }
}

fn deobfuscate(blob: &[u8], key: &[u8]) -> Option<String> {
    let mut data = b85decode(blob)?;
    if key.is_empty() {
        return None;
    }
    for (i, byte) in data.iter_mut().enumerate() {
        *byte ^= key[i % key.len()];
    }
    let plain = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data, 4096).ok()?;
    String::from_utf8(plain).ok()
}

/// Python's `base64.b85decode`: groups of five characters, the last group
/// padded with `~` and the padding bytes dropped again.
fn b85decode(text: &[u8]) -> Option<Vec<u8>> {
    let mut table = [u8::MAX; 256];
    for (value, &ch) in B85_ALPHABET.iter().enumerate() {
        table[usize::from(ch)] = value as u8;
    }
    let padding = (5 - text.len() % 5) % 5;
    let mut out = Vec::with_capacity(text.len() / 5 * 4 + 4);
    for chunk in text.chunks(5) {
        let mut acc: u64 = 0;
        for i in 0..5 {
            let value = match chunk.get(i) {
                Some(&ch) => table[usize::from(ch)],
                None => 84,
            };
            if value == u8::MAX {
                return None;
            }
            acc = acc * 85 + u64::from(value);
        }
        let word = u32::try_from(acc).ok()?;
        out.extend_from_slice(&word.to_be_bytes());
    }
    out.truncate(out.len().checked_sub(padding)?);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b85_matches_python() {
        // base64.b85encode(b"hello world") == b"Xk~0{Zy<MXa%^M"
        assert_eq!(b85decode(b"Xk~0{Zy<MXa%^M").unwrap(), b"hello world");
        assert_eq!(b85decode(b"").unwrap(), b"");
        assert!(b85decode(b"\"\"\"\"\"").is_none());
        // Overflowing group.
        assert!(b85decode(b"~~~~~").is_none());
    }

    #[test]
    fn derived_key_has_the_documented_shape() {
        let key = Passphrase::derived().expect("derivation works");
        // pyrekordbox rejects explicit keys that do not start like this.
        assert!(key.expose().starts_with("402fd"));
        assert!(key.expose().bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(format!("{key:?}"), "Passphrase(<redacted>)");
    }

    #[test]
    fn env_override_wins_and_blank_is_ignored() {
        let env = |name: &str| (name == KEY_ENV).then(|| "  custom  ".to_owned());
        assert_eq!(Passphrase::resolve(&env).unwrap().expose(), "custom");
        let blank = |name: &str| (name == KEY_ENV).then(|| "   ".to_owned());
        assert_eq!(Passphrase::resolve(&blank), Passphrase::derived());
    }
}
