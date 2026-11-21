//! Password hashing: Argon2id, PHC strings (the parameters travel with each hash).

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, Version,
};

use crate::{secret, AuthError};

/// The Argon2id cost. [`Cost::Standard`] is argon2's default (19 MiB, 2 passes, 1 lane: the
/// OWASP minimum, about 50 ms on a small box); [`Cost::Test`] keeps tests fast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cost {
    Standard,
    Test,
}

impl Cost {
    fn hasher(self) -> Argon2<'static> {
        match self {
            Cost::Standard => Argon2::default(),
            Cost::Test => {
                // Fixed, valid parameters (the minimums argon2 accepts); `Params::new` only fails on
                // values out of range, which these are not.
                let params = Params::new(Params::MIN_M_COST.max(8), 1, 1, None)
                    .unwrap_or_else(|_| Params::default());
                Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            }
        }
    }
}

/// Hash `password` with a fresh 128-bit salt.
pub(crate) fn hash(password: &str, cost: Cost) -> Result<String, AuthError> {
    let salt_bytes = secret::random_bytes(16).map_err(|e| AuthError::Io(e.to_string()))?;
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|e| AuthError::Io(e.to_string()))?;
    cost.hasher()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AuthError::Io(format!("hashing the password: {e}")))
}

/// Whether `password` matches the stored PHC string (its own parameters are used).
pub(crate) fn verify(password: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    // The parameters come from the hash; the hasher's own are only defaults.
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_argon2id_and_verify() {
        let h = hash("correct horse", Cost::Test).unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify("correct horse", &h));
        assert!(!verify("wrong horse", &h));
        assert!(!verify("correct horse", "not a hash"));
        // A fresh salt every time.
        assert_ne!(h, hash("correct horse", Cost::Test).unwrap());
    }

    #[test]
    fn standard_cost_is_argon2_default() {
        let h = hash("pw-standard", Cost::Standard).unwrap();
        assert!(h.contains("m=19456,t=2,p=1"), "{h}");
        assert!(verify("pw-standard", &h));
    }
}
