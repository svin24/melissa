//! Random tokens, hashes and constant-time comparison.

use std::io::Read;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};

/// `n` random bytes from the kernel (like Melissa's other secrets: no extra crate).
pub(crate) fn random_bytes(n: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf)
}

/// A 256-bit random token, URL-safe base64 without padding (43 characters).
pub(crate) fn token() -> std::io::Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes(32)?))
}

/// A short random id with a prefix (`s-1a2b3c4d5e6f`), for lists and links; not a secret.
pub(crate) fn short_id(prefix: &str) -> std::io::Result<String> {
    let hex: String = random_bytes(6)?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(format!("{prefix}-{hex}"))
}

/// SHA-256 of a token, hex: what is stored instead of the token itself.
pub(crate) fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Equal without an early exit on the first different byte.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Letters and digits that can't be mistaken for each other (no I, L, O, 0, 1).
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// A setup code: 10 characters from [`CODE_ALPHABET`] (about 50 bits), shown as `ABCDE-FGHJK`.
pub(crate) fn setup_code() -> std::io::Result<String> {
    let mut out = String::new();
    // Rejection sampling keeps every character equally likely.
    let limit = 256 - (256 % CODE_ALPHABET.len());
    while out.len() < 11 {
        for b in random_bytes(32)? {
            if usize::from(b) >= limit {
                continue;
            }
            if out.len() == 5 {
                out.push('-');
            }
            out.push(char::from(
                CODE_ALPHABET[usize::from(b) % CODE_ALPHABET.len()],
            ));
            if out.len() == 11 {
                break;
            }
        }
    }
    Ok(out)
}

/// A code as typed: case, spaces and dashes don't matter.
pub(crate) fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_and_distinct() {
        let (a, b) = (token().unwrap(), token().unwrap());
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert_eq!(hash_token(&a).len(), 64);
    }

    #[test]
    fn setup_codes_use_the_plain_alphabet() {
        let code = setup_code().unwrap();
        assert_eq!(code.len(), 11);
        assert_eq!(&code[5..6], "-");
        assert!(normalize_code(&code)
            .bytes()
            .all(|b| CODE_ALPHABET.contains(&b)));
        assert_eq!(normalize_code(" abcde-fghjk "), "ABCDEFGHJK");
    }

    #[test]
    fn constant_time_eq_compares() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
