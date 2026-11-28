//! The recovery key of encrypted backups: 160 random bits written as 32 characters of
//! Crockford's base32 (no I, L, O or U: nothing to confuse when copied by hand) in groups of
//! four, plus one group of check characters, e.g. `7K3M-...-Q2XA`. It is restic's password.
//!
//! The owner sees it once and keeps it (paper, a password manager); a root-only copy (0600)
//! stays on the device so scheduled backups run unattended. Without it, a new device (or this
//! one after a factory reset) can't read the backups.

use std::{fmt, io::Read, path::Path};

use sha2::{Digest, Sha256};

use crate::{fsutil, io, BackupError};

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Random characters (5 bits each: 160 bits).
const SYMBOLS: usize = 32;
const GROUP: usize = 4;
const CHECK: usize = 4;

/// A recovery key in its canonical form. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct RecoveryKey(String);

impl fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryKey(..)")
    }
}

fn check_of(symbols: &[u8]) -> Vec<u8> {
    let digest = Sha256::digest(symbols);
    digest
        .iter()
        .take(CHECK)
        .map(|b| ALPHABET[usize::from(b % 32)])
        .collect()
}

fn grouped(all: &[u8]) -> String {
    all.chunks(GROUP)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

impl RecoveryKey {
    /// A new key from the kernel's random source.
    pub fn generate() -> Result<Self, BackupError> {
        let mut bytes = [0u8; SYMBOLS * 5 / 8];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .map_err(io("reading /dev/urandom"))?;
        Ok(Self::from_entropy(&bytes))
    }

    /// The key for these 20 bytes (tests; [`RecoveryKey::generate`] for real ones).
    pub fn from_entropy(bytes: &[u8; SYMBOLS * 5 / 8]) -> Self {
        let mut symbols = Vec::with_capacity(SYMBOLS + CHECK);
        let (mut acc, mut bits) = (0u32, 0u32);
        for b in bytes {
            acc = (acc << 8) | u32::from(*b);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                symbols.push(ALPHABET[((acc >> bits) & 31) as usize]);
            }
        }
        let check = check_of(&symbols);
        symbols.extend(check);
        Self(grouped(&symbols))
    }

    /// A key as a person typed it: case, spaces and dashes don't matter, O reads as 0 and I/L
    /// as 1. Wrong length or check characters: an error that says which.
    pub fn parse(typed: &str) -> Result<Self, BackupError> {
        let mut symbols = Vec::new();
        for c in typed.chars() {
            let c = c.to_ascii_uppercase();
            let c = match c {
                ' ' | '-' | '\t' | '\n' | '\r' => continue,
                'O' => '0',
                'I' | 'L' => '1',
                c => c,
            };
            if !c.is_ascii() || !ALPHABET.contains(&(c as u8)) {
                return Err(BackupError::Invalid(format!(
                    "`{c}` is not part of a recovery key"
                )));
            }
            symbols.push(c as u8);
        }
        if symbols.len() != SYMBOLS + CHECK {
            return Err(BackupError::Invalid(format!(
                "a recovery key has {} characters (without dashes); this has {}",
                SYMBOLS + CHECK,
                symbols.len()
            )));
        }
        if check_of(&symbols[..SYMBOLS]) != symbols[SYMBOLS..] {
            return Err(BackupError::Invalid(
                "the recovery key has a typo (its last group doesn't match)".into(),
            ));
        }
        Ok(Self(grouped(&symbols)))
    }

    /// The key, to show the owner once or to hand to restic.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Write the device's copy (root 0600).
    pub fn save(&self, path: &Path) -> Result<(), BackupError> {
        fsutil::write_atomic(path, self.0.as_bytes(), 0o600, false)
    }

    /// Read the device's copy; `None` if there is none.
    pub fn load(path: &Path) -> Result<Option<Self>, BackupError> {
        fsutil::read_small(path, 256)?
            .map(|t| Self::parse(t.trim()))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch;

    #[test]
    fn keys_are_grouped_checked_and_forgiving_to_type() {
        let k = RecoveryKey::from_entropy(&[0xA5; 20]);
        let s = k.expose();
        assert_eq!(s.len(), 36 + 8);
        assert_eq!(s.split('-').count(), 9);
        assert!(s.split('-').all(|g| g.len() == 4));
        assert_eq!(format!("{k:?}"), "RecoveryKey(..)");
        // Typed sloppily: lower case, spaces, no dashes.
        let typed = s.replace('-', " ").to_lowercase();
        assert_eq!(RecoveryKey::parse(&typed).unwrap(), k);
        // O for 0 and l for 1.
        let looks = s.replace('0', "O").replace('1', "l");
        assert_eq!(RecoveryKey::parse(&looks).unwrap(), k);
        // A typo is caught by the check group.
        let mut bad: Vec<char> = s.chars().collect();
        bad[0] = if bad[0] == 'A' { 'B' } else { 'A' };
        let bad: String = bad.into_iter().collect();
        assert!(RecoveryKey::parse(&bad)
            .unwrap_err()
            .to_string()
            .contains("typo"));
        assert!(RecoveryKey::parse("ABCD").is_err());
        assert!(RecoveryKey::parse(&s.replace('-', "U")).is_err());
    }

    #[test]
    fn generated_keys_differ_and_round_trip_through_the_file() {
        let (a, b) = (
            RecoveryKey::generate().unwrap(),
            RecoveryKey::generate().unwrap(),
        );
        assert_ne!(a, b);
        let d = scratch("key");
        let p = d.join("key");
        assert_eq!(RecoveryKey::load(&p).unwrap(), None);
        a.save(&p).unwrap();
        assert_eq!(RecoveryKey::load(&p).unwrap(), Some(a));
        use std::os::unix::fs::MetadataExt;
        assert_eq!(p.metadata().unwrap().mode() & 0o777, 0o600);
    }
}
