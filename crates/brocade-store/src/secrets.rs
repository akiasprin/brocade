//! Sealing the recoverable secrets this database is not allowed to hold in the clear.
//!
//! Everything else in here is either a hash (passwords, tokens) or a credential the fleet itself
//! generated and can regenerate at will (UUIDs, short ids, WireGuard keys). Certificates and
//! externally managed proxies break that: the DNS credential can rewrite every record in a
//! domain, a node's private key *is* that node's identity, and an external proxy credential has
//! to be reproduced verbatim in Xray's config. None can be hashed, so they are encrypted instead.
//!
//! # Why a key from the environment, and why a missing one is an error
//!
//! The key lives in `BROCADE_SECRET_KEY`, beside `DATABASE_URL` in the same env file, which is the
//! one place an operator already treats as secret. Deriving it from something in the database
//! would be theatre: whoever reads the ciphertext reads the key too.
//!
//! Absent, sealing fails loudly rather than storing plaintext. The alternative — falling back to
//! the clear with a warning — produces a database that looks encrypted, is not, and gives nobody a
//! way to tell which rows are which. A control plane that never touches certificates never needs
//! the key, so this costs existing deployments nothing.
//!
//! # Why the context string is bound in
//!
//! `seal` takes a context (`"dns-credential"`, `"cert-key"`) and AES-GCM authenticates it. Without
//! it, a sealed value is portable between columns: anybody able to write the database could move a
//! node's sealed private key into the DNS credential column, or swap two nodes' keys, and every
//! decryption would still succeed. Binding the context makes those rejected rather than silently
//! wrong.
//!
//! # Why the version prefix
//!
//! `v1.` is not decoration. Rotating the key or changing the algorithm has to be able to read what
//! the previous one wrote, and a bare blob leaves nothing to branch on but guessing at lengths.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};

use crate::{Result, StoreError};

/// Where the key is read from. Named here rather than at the call sites so that the error message
/// and the documentation cannot drift apart from the thing they name.
pub const SECRET_KEY_ENV: &str = "BROCADE_SECRET_KEY";

/// Marks both the format and the algorithm. Reading tolerates only what it understands; a value
/// carrying an unknown version is an error rather than a best-effort guess.
const VERSION: &str = "v1";

/// Sealing context for the DNS provider credential.
pub const CTX_DNS_CREDENTIAL: &str = "dns-credential";
/// Sealing context for an ACME account key.
pub const CTX_ACME_ACCOUNT: &str = "acme-account";
/// Sealing context for a node certificate's private key.
pub const CTX_CERT_KEY: &str = "cert-key";

/// Binds one external proxy credential to its project and stable resource id. Unlike the fixed
/// contexts above this deliberately prevents swapping two valid proxy credentials in the same
/// column: the moved ciphertext will fail authentication when read under the destination id.
pub fn external_outbound_context(app_id: &str, outbound_id: &str) -> String {
    format!("external-outbound:{app_id}/{outbound_id}")
}

/// Reads and validates the key. Returns `None` when unset, which callers turn into their own
/// message — "cannot save a credential" and "cannot read one back" want different wording, and
/// this module does not know which it is being asked for.
fn key_material() -> Result<Option<[u8; 32]>> {
    let raw = match std::env::var(SECRET_KEY_ENV) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => return Ok(None),
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(raw.trim())
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(raw.trim()))
        .map_err(|_| StoreError::InvalidData(format!("{SECRET_KEY_ENV} is not base64")))?;
    // Length is checked here rather than left to ring, whose error at this point would say
    // nothing about which environment variable is wrong.
    let key: [u8; 32] = bytes.try_into().map_err(|_| {
        StoreError::InvalidData(format!(
            "{SECRET_KEY_ENV} must decode to exactly 32 bytes \
             (generate one with: openssl rand -base64 32)"
        ))
    })?;
    Ok(Some(key))
}

/// Whether this process can seal and open at all. The console asks before offering to store a
/// credential, so that the refusal arrives on the settings page rather than as a failed save.
pub fn sealing_available() -> bool {
    matches!(key_material(), Ok(Some(_)))
}

fn aead_key(key: [u8; 32]) -> Result<LessSafeKey> {
    let unbound = UnboundKey::new(&AES_256_GCM, &key)
        .map_err(|_| StoreError::InvalidData("cannot build AEAD key".to_owned()))?;
    Ok(LessSafeKey::new(unbound))
}

/// Encrypts `plaintext` under `context`. The result is ASCII and safe to put in a TEXT column.
pub fn seal(context: &str, plaintext: &str) -> Result<String> {
    let Some(key) = key_material()? else {
        return Err(StoreError::InvalidData(format!(
            "{SECRET_KEY_ENV} is not set, so this secret cannot be stored. \
             Generate one with `openssl rand -base64 32` and put it in the console's env file."
        )));
    };
    let key = aead_key(key)?;

    // A fresh random nonce per sealing. AES-GCM repeating a nonce under one key is catastrophic
    // rather than merely weak, and a counter would have to be persisted somewhere that several
    // control-plane processes agree on — which is exactly the coordination this avoids.
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes)?;
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);

    let mut buffer = plaintext.as_bytes().to_vec();
    key.seal_in_place_append_tag(nonce, Aad::from(context.as_bytes()), &mut buffer)
        .map_err(|_| StoreError::InvalidData("sealing failed".to_owned()))?;

    let mut payload = Vec::with_capacity(NONCE_LEN + buffer.len());
    payload.extend_from_slice(&nonce_bytes);
    payload.extend_from_slice(&buffer);
    Ok(format!("{VERSION}.{}", URL_SAFE_NO_PAD.encode(payload)))
}

/// Decrypts what `seal` produced under the same context.
pub fn open(context: &str, sealed: &str) -> Result<String> {
    let Some(key) = key_material()? else {
        return Err(StoreError::InvalidData(format!(
            "{SECRET_KEY_ENV} is not set, so stored secrets cannot be read. \
             If this used to work, the key was removed from the environment — restore it; \
             the data is not recoverable without it."
        )));
    };
    let key = aead_key(key)?;

    let body = sealed.strip_prefix(&format!("{VERSION}.")).ok_or_else(|| {
        StoreError::InvalidData("sealed value is not in a format this build understands".to_owned())
    })?;
    let payload = URL_SAFE_NO_PAD
        .decode(body)
        .map_err(|_| StoreError::InvalidData("sealed value is not base64".to_owned()))?;
    if payload.len() <= NONCE_LEN {
        return Err(StoreError::InvalidData(
            "sealed value is truncated".to_owned(),
        ));
    }
    let (nonce_bytes, rest) = payload.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce_bytes)
        .map_err(|_| StoreError::InvalidData("sealed value has no nonce".to_owned()))?;

    let mut buffer = rest.to_vec();
    let plaintext = key
        .open_in_place(nonce, Aad::from(context.as_bytes()), &mut buffer)
        // Deliberately one message for every failure. A wrong key, a wrong context and a tampered
        // ciphertext are indistinguishable to AES-GCM by design, and inventing a distinction here
        // would be a guess presented as a diagnosis.
        .map_err(|_| {
            StoreError::InvalidData(
                "cannot open sealed value: wrong key, wrong context, or the value was altered"
                    .to_owned(),
            )
        })?;
    String::from_utf8(plaintext.to_vec())
        .map_err(|_| StoreError::InvalidData("sealed value is not text".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env var is process-global, so these run under one lock and put it back. Without that
    /// they pass alone and fail together, which is the worst way for a test to be wrong.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_key<T>(key: Option<&str>, body: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var(SECRET_KEY_ENV).ok();
        // SAFETY: the lock above serializes every mutation of this variable in this crate's tests.
        unsafe {
            match key {
                Some(value) => std::env::set_var(SECRET_KEY_ENV, value),
                None => std::env::remove_var(SECRET_KEY_ENV),
            }
        }
        let out = body();
        unsafe {
            match previous {
                Some(value) => std::env::set_var(SECRET_KEY_ENV, value),
                None => std::env::remove_var(SECRET_KEY_ENV),
            }
        }
        out
    }

    const KEY: &str = "8fVQ0k1sJp3xQ2mLr7bN5tW9cZ4yA6dE1hG8jK0lM2o";

    #[test]
    fn sealed_value_round_trips_under_the_same_context() {
        with_key(Some(KEY), || {
            let sealed = seal(CTX_CERT_KEY, "-----BEGIN PRIVATE KEY-----").unwrap();
            assert!(sealed.starts_with("v1."), "缺版本前缀：{sealed}");
            assert_eq!(
                open(CTX_CERT_KEY, &sealed).unwrap(),
                "-----BEGIN PRIVATE KEY-----"
            );
        });
    }

    #[test]
    fn the_plaintext_does_not_appear_in_the_sealed_value() {
        with_key(Some(KEY), || {
            let sealed = seal(CTX_DNS_CREDENTIAL, "cfut_secret_token_value").unwrap();
            assert!(!sealed.contains("cfut_"), "明文漏进密文里了：{sealed}");
        });
    }

    #[test]
    fn sealing_twice_gives_different_ciphertext() {
        with_key(Some(KEY), || {
            // Same key, same plaintext, different nonce. Equal outputs would mean the nonce is not
            // random, which is the failure this is here to catch.
            let a = seal(CTX_CERT_KEY, "same").unwrap();
            let b = seal(CTX_CERT_KEY, "same").unwrap();
            assert_ne!(a, b);
        });
    }

    #[test]
    fn a_value_sealed_for_one_context_does_not_open_under_another() {
        with_key(Some(KEY), || {
            let sealed = seal(CTX_CERT_KEY, "node private key").unwrap();
            assert!(open(CTX_DNS_CREDENTIAL, &sealed).is_err());
        });
    }

    #[test]
    fn a_tampered_value_is_refused_rather_than_returned() {
        with_key(Some(KEY), || {
            let sealed = seal(CTX_CERT_KEY, "node private key").unwrap();
            let mut broken = sealed.clone();
            // Flip one character of the ciphertext body, leaving the format intact.
            let last = broken.pop().unwrap();
            broken.push(if last == 'A' { 'B' } else { 'A' });
            assert!(open(CTX_CERT_KEY, &broken).is_err());
        });
    }

    #[test]
    fn without_a_key_sealing_refuses_instead_of_storing_plaintext() {
        with_key(None, || {
            let error = seal(CTX_CERT_KEY, "secret").unwrap_err().to_string();
            assert!(
                error.contains(SECRET_KEY_ENV),
                "报错要指出是哪个变量：{error}"
            );
            assert!(!sealing_available());
        });
    }

    #[test]
    fn a_key_of_the_wrong_length_is_named_as_such() {
        with_key(Some("c2hvcnQ"), || {
            let error = seal(CTX_CERT_KEY, "secret").unwrap_err().to_string();
            assert!(error.contains("32 bytes"), "报错要说清长度：{error}");
        });
    }
}
